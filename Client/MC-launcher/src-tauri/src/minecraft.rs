use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Cursor, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex, OnceLock,
    },
};

use futures::{StreamExt, TryStreamExt};
use quick_xml::{Reader, events::Event};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;
use zip::ZipArchive;

use crate::{
    instances_root,
    launcher::{default_game_features, maven_library_path, rules_allow},
    process::hide_console_window,
    validate_folder_name,
};

const VERSION_MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const INSTALLER_PROCESSOR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);
static DOWNLOAD_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();
static INSTANCE_INSTALL_CANCELLATIONS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    OnceLock::new();

fn instance_install_cancellations() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    INSTANCE_INSTALL_CANCELLATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn instance_install_cancellation(folder_name: &str) -> Result<Option<Arc<AtomicBool>>, String> {
    instance_install_cancellations()
        .lock()
        .map(|active| active.get(folder_name).cloned())
        .map_err(|_| "Не удалось получить состояние установки Minecraft.".to_string())
}

fn check_instance_install(cancellation: Option<&AtomicBool>) -> Result<(), String> {
    if cancellation.is_some_and(|token| token.load(Ordering::Acquire)) {
        Err("Установка Minecraft отменена.".into())
    } else {
        Ok(())
    }
}

async fn wait_for_instance_install_cancel(cancellation: &AtomicBool) {
    while !cancellation.load(Ordering::Acquire) {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[tauri::command]
pub(crate) fn begin_instance_install(folder_name: String) -> Result<(), String> {
    validate_folder_name(&folder_name)?;
    let mut active = instance_install_cancellations()
        .lock()
        .map_err(|_| "Не удалось зарегистрировать установку Minecraft.".to_string())?;
    if active.contains_key(&folder_name) {
        return Err("Установка Minecraft для этого инстанса уже выполняется.".into());
    }
    active.insert(folder_name, Arc::new(AtomicBool::new(false)));
    Ok(())
}

#[tauri::command]
pub(crate) fn cancel_instance_install(folder_name: String) -> Result<(), String> {
    validate_folder_name(&folder_name)?;
    let active = instance_install_cancellations()
        .lock()
        .map_err(|_| "Не удалось получить состояние установки Minecraft.".to_string())?;
    let cancellation = active
        .get(&folder_name)
        .ok_or("Установка Minecraft не выполняется.")?;
    cancellation.store(true, Ordering::Release);
    Ok(())
}

#[tauri::command]
pub(crate) fn check_instance_install_cancellation(folder_name: String) -> Result<(), String> {
    validate_folder_name(&folder_name)?;
    check_instance_install(instance_install_cancellation(&folder_name)?.as_deref())
}

#[tauri::command]
pub(crate) fn finish_instance_install(folder_name: String) -> Result<(), String> {
    validate_folder_name(&folder_name)?;
    instance_install_cancellations()
        .lock()
        .map_err(|_| "Не удалось завершить установку Minecraft.".to_string())?
        .remove(&folder_name);
    Ok(())
}

fn download_lock(path: &Path) -> Result<Arc<tokio::sync::Mutex<()>>, String> {
    DOWNLOAD_LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| "Не удалось заблокировать общий файл загрузки.".to_string())
        .map(|mut locks| {
            Arc::clone(
                locks
                    .entry(path.to_path_buf())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        })
}

#[derive(Deserialize)]
struct VersionManifest {
    latest: ManifestLatest,
    versions: Vec<ManifestVersion>,
}

#[derive(Deserialize)]
struct ManifestLatest {
    release: String,
}

#[derive(Deserialize)]
struct ManifestVersion {
    id: String,
    #[serde(rename = "type")]
    version_type: String,
    url: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MinecraftVersions {
    versions: Vec<String>,
    latest: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionDetails {
    downloads: VersionDownloads,
    libraries: Vec<Library>,
    asset_index: AssetIndex,
}

#[derive(Deserialize)]
struct VersionDownloads {
    client: Artifact,
}

#[derive(Deserialize)]
struct Artifact {
    url: String,
    sha1: Option<String>,
    path: Option<String>,
    size: Option<u64>,
}

#[derive(Deserialize)]
struct Library {
    downloads: Option<LibraryDownloads>,
    natives: Option<std::collections::HashMap<String, String>>,
    extract: Option<NativeExtract>,
}

#[derive(Deserialize)]
struct LibraryDownloads {
    artifact: Option<Artifact>,
    classifiers: Option<std::collections::HashMap<String, Artifact>>,
}

#[derive(Deserialize)]
struct NativeExtract {
    exclude: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssetIndex {
    id: String,
    url: String,
    sha1: Option<String>,
}

#[derive(Deserialize)]
struct AssetObjects {
    objects: std::collections::HashMap<String, AssetObject>,
    #[serde(default, rename = "virtual")]
    virtual_assets: bool,
    #[serde(default)]
    map_to_resources: bool,
}

#[derive(Deserialize)]
struct AssetObject {
    hash: String,
    size: u64,
}

struct DownloadTask {
    url: String,
    path: PathBuf,
    sha1: Option<String>,
    size: Option<u64>,
    label: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    completed: usize,
    total: usize,
    current_file: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LoaderVersion {
    version: String,
    channel: String,
}

fn version_numbers(version: &str) -> Vec<u64> {
    version
        .split(|character: char| !character.is_ascii_digit())
        .filter_map(|part| part.parse().ok())
        .collect()
}

fn channel_priority(channel: &str) -> u8 {
    match channel {
        "recommended" => 0,
        "stable" => 1,
        "latest" => 2,
        "release" => 3,
        _ => 4,
    }
}

fn sort_loader_versions(mut versions: Vec<LoaderVersion>) -> Vec<LoaderVersion> {
    versions.sort_by(|left, right| {
        channel_priority(&left.channel)
            .cmp(&channel_priority(&right.channel))
            .then_with(|| version_numbers(&right.version).cmp(&version_numbers(&left.version)))
            .then_with(|| right.version.cmp(&left.version))
    });
    let mut seen = HashSet::new();
    versions.retain(|version| seen.insert(version.version.clone()));
    versions
}

fn neoforge_minecraft_prefix(minecraft_version: &str) -> Result<String, String> {
    let invalid = || "Некорректная версия Minecraft для NeoForge.".to_string();
    if let Some(version) = minecraft_version.strip_prefix("1.") {
        let components: Vec<_> = version.split('.').collect();
        if !(1..=2).contains(&components.len())
            || components.iter().any(|component| {
                component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit())
            })
        {
            return Err(invalid());
        }
        let minor = components[0];
        let patch = components.get(1).copied().unwrap_or("0");
        Ok(format!("{minor}.{patch}."))
    } else {
        let components: Vec<_> = minecraft_version.split('.').collect();
        if !(2..=3).contains(&components.len())
            || components.iter().any(|component| {
                component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit())
            })
        {
            return Err(invalid());
        }
        Ok(format!("{minecraft_version}."))
    }
}

fn neoforge_version_matches_minecraft(
    loader_version: &str,
    minecraft_version: &str,
) -> Result<bool, String> {
    let maven_prefix = format!("{minecraft_version}-");
    let version_prefix = neoforge_minecraft_prefix(minecraft_version)?;
    Ok(loader_version.starts_with(&maven_prefix) || loader_version.starts_with(&version_prefix))
}

pub(crate) fn neoforge_is_available(minecraft_version: &str) -> bool {
    if let Some(version) = minecraft_version.strip_prefix("1.") {
        let components: Vec<_> = version.split('.').collect();
        if components.len() != 2
            || components
                .iter()
                .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return false;
        }
        let (Ok(minor), Ok(patch)) = (components[0].parse::<u32>(), components[1].parse::<u32>())
        else {
            return false;
        };
        minor > 20 || (minor == 20 && patch >= 1)
    } else {
        let components: Vec<_> = minecraft_version.split('.').collect();
        components.len() >= 2
            && components.iter().all(|part| {
                !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
            })
    }
}

fn stable_loader_version(version: &str) -> bool {
    let version = version.to_ascii_lowercase();
    !["alpha", "beta", "rc", "snapshot", "pre"]
        .iter()
        .any(|marker| version.contains(marker))
}

fn maven_metadata_versions(xml: &str) -> Result<Vec<String>, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut in_versions = false;
    let mut in_version = false;
    let mut versions = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) if element.name().as_ref() == b"versions" => {
                in_versions = true;
            }
            Ok(Event::End(element)) if element.name().as_ref() == b"versions" => {
                in_versions = false;
            }
            Ok(Event::Start(element)) if in_versions && element.name().as_ref() == b"version" => {
                in_version = true;
            }
            Ok(Event::End(element)) if element.name().as_ref() == b"version" => {
                in_version = false;
            }
            Ok(Event::Text(text)) if in_versions && in_version => {
                versions.push(
                    text.decode()
                        .map_err(|error| error.to_string())?
                        .into_owned(),
                );
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(format!("Некорректный Maven metadata XML: {error}")),
        }
    }
    Ok(versions)
}

async fn fetch_maven_versions(client: &reqwest::Client, url: &str) -> Result<Vec<String>, String> {
    trusted_url(url)?;
    let xml = client
        .get(url)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .text()
        .await
        .map_err(|error| error.to_string())?;
    maven_metadata_versions(&xml)
}

fn forge_full_version(minecraft_version: &str, loader_version: &str) -> String {
    let prefix = format!("{minecraft_version}-");
    if loader_version.starts_with(&prefix) {
        loader_version.to_string()
    } else {
        format!("{prefix}{loader_version}")
    }
}

fn legacy_forge_supported(minecraft_version: &str) -> bool {
    let Some(rest) = minecraft_version.strip_prefix("1.") else {
        return false;
    };
    rest.split('.')
        .next()
        .and_then(|minor| minor.parse::<u32>().ok())
        .is_some_and(|minor| minor <= 12)
}

async fn download_optional_artifact(
    client: &reqwest::Client,
    url: &str,
) -> Result<Option<Vec<u8>>, String> {
    trusted_url(url)?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let bytes = response
        .error_for_status()
        .map_err(|error| error.to_string())?
        .bytes()
        .await
        .map_err(|error| error.to_string())?;
    if bytes.len() > 200_000_000 {
        return Err("Архив загрузчика превышает допустимый размер.".into());
    }
    Ok(Some(bytes.to_vec()))
}

fn write_legacy_forge_profile(
    instance: &Path,
    libraries_root: &Path,
    minecraft_version: &str,
    loader_version: &str,
    artifact_url: &str,
    extension: &str,
    artifact_bytes: &[u8],
) -> Result<(String, serde_json::Value), String> {
    ZipArchive::new(Cursor::new(artifact_bytes))
        .map_err(|_| "Forge universal-файл не является корректным JAR/ZIP архивом.".to_string())?;
    let full_version = forge_full_version(minecraft_version, loader_version);
    let artifact_path = format!(
        "net/minecraftforge/forge/{full_version}/forge-{full_version}-universal.{extension}"
    );
    let output = safe_destination(libraries_root, &artifact_path)?;
    let parent = output
        .parent()
        .ok_or("Некорректный путь Forge universal.")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    fs::write(output, artifact_bytes).map_err(|error| error.to_string())?;

    let profile_id = format!("{minecraft_version}-forge-{loader_version}");
    let base_metadata_path = instance
        .join("versions")
        .join(minecraft_version)
        .join(format!("{minecraft_version}.json"));
    let base_metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(base_metadata_path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let mut game_arguments = base_metadata
        .get("minecraftArguments")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !game_arguments.is_empty() {
        game_arguments.push(' ');
    }
    game_arguments.push_str("--tweakClass cpw.mods.fml.common.launcher.FMLTweaker");
    let profile = serde_json::json!({
        "id": profile_id,
        "inheritsFrom": minecraft_version,
        "mainClass": "net.minecraft.launchwrapper.Launch",
        "minecraftArguments": game_arguments,
        "libraries": [
            { "name": "net.minecraft:launchwrapper:1.12" },
            {
                "name": format!("net.minecraftforge:forge:{full_version}:universal"),
                "downloads": { "artifact": {
                    "path": artifact_path,
                    "url": artifact_url
                }}
            }
        ]
    });
    write_loader_profile(instance, profile, minecraft_version)
}

fn trusted_url(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|error| error.to_string())?;
    if parsed.scheme() != "https" || !parsed.host_str().is_some_and(is_trusted_host) {
        return Err("Ссылка загрузки не принадлежит доверенному HTTPS-домену Mojang.".into());
    }
    Ok(())
}

fn official_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("MyLauncher/0.1.0")
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(600))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.url().scheme() == "https"
                && attempt.url().host_str().is_some_and(is_trusted_host)
            {
                attempt.follow()
            } else {
                attempt.error("Redirect blocked: destination is not allowlisted")
            }
        }))
        .build()
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn get_minecraft_versions() -> Result<MinecraftVersions, String> {
    let manifest: VersionManifest = official_client()?
        .get(VERSION_MANIFEST_URL)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    Ok(MinecraftVersions {
        versions: manifest
            .versions
            .into_iter()
            .filter(|version| version.version_type == "release")
            .map(|version| version.id)
            .collect(),
        latest: manifest.latest.release,
    })
}

pub(crate) fn is_trusted_host(host: &str) -> bool {
    matches!(
        host,
        "piston-meta.mojang.com"
            | "piston-data.mojang.com"
            | "launcher.mojang.com"
            | "libraries.minecraft.net"
            | "resources.download.minecraft.net"
            | "repo.maven.apache.org"
            | "repo1.maven.org"
            | "meta.fabricmc.net"
            | "maven.fabricmc.net"
            | "files.minecraftforge.net"
            | "maven.minecraftforge.net"
            | "maven.neoforged.net"
            | "api.papermc.io"
            | "fill.papermc.io"
            | "fill-data.papermc.io"
            | "api.purpurmc.org"
            | "api.adoptium.net"
            | "github.com"
            | "raw.githubusercontent.com"
            | "gitlab.com"
            | "release-assets.githubusercontent.com"
            | "objects.githubusercontent.com"
            | "api.modrinth.com"
            | "cdn.modrinth.com"
    )
}

#[tauri::command]
pub(crate) async fn get_loader_versions(
    loader_type: String,
    minecraft_version: String,
) -> Result<Vec<LoaderVersion>, String> {
    if !valid_version_component(&minecraft_version) {
        return Err("Некорректная версия Minecraft.".into());
    }
    let client = official_client()?;

    match loader_type.as_str() {
        "fabric" => {
            let url = format!("https://meta.fabricmc.net/v2/versions/loader/{minecraft_version}");
            trusted_url(&url)?;
            #[derive(Deserialize)]
            struct FabricEntry {
                loader: FabricLoader,
            }
            #[derive(Deserialize)]
            struct FabricLoader {
                version: String,
                stable: bool,
            }
            let response = client
                .get(url)
                .send()
                .await
                .map_err(|error| error.to_string())?;
            if response.status() == reqwest::StatusCode::BAD_REQUEST {
                return Ok(Vec::new());
            }
            let entries: Vec<FabricEntry> = response
                .error_for_status()
                .map_err(|error| error.to_string())?
                .json()
                .await
                .map_err(|error| error.to_string())?;
            Ok(sort_loader_versions(
                entries
                    .into_iter()
                    .map(|entry| LoaderVersion {
                        version: entry.loader.version,
                        channel: if entry.loader.stable {
                            "stable"
                        } else {
                            "preview"
                        }
                        .into(),
                    })
                    .collect(),
            ))
        }
        "forge" => {
            let url =
                "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json";
            trusted_url(url)?;
            let response: serde_json::Value = client
                .get(url)
                .send()
                .await
                .map_err(|error| error.to_string())?
                .error_for_status()
                .map_err(|error| error.to_string())?
                .json()
                .await
                .map_err(|error| error.to_string())?;
            let prefix = format!("{minecraft_version}-");
            let promotions = response
                .get("promos")
                .and_then(serde_json::Value::as_object)
                .ok_or("Некорректный Forge promotions API.")?;
            let mut versions = ["recommended", "latest"]
                .into_iter()
                .filter_map(|channel| {
                    promotions
                        .get(&format!("{minecraft_version}-{channel}"))
                        .and_then(serde_json::Value::as_str)
                        .filter(|version| stable_loader_version(version))
                        .map(|version| LoaderVersion {
                            version: version.to_string(),
                            channel: channel.to_string(),
                        })
                })
                .collect::<Vec<_>>();
            if versions.is_empty() {
                let metadata = fetch_maven_versions(
                    &client,
                    "https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml",
                )
                .await?;
                versions.extend(
                    metadata
                        .into_iter()
                        .filter(|version| version.starts_with(&prefix))
                        .filter(|version| stable_loader_version(version))
                        .map(|version| LoaderVersion {
                            version: version[prefix.len()..].to_string(),
                            channel: "release".into(),
                        }),
                );
            }
            Ok(sort_loader_versions(versions))
        }
        "neoforge" => {
            if !neoforge_is_available(&minecraft_version) {
                return Err("NeoForge недоступен для выбранной версии Minecraft.".into());
            }
            let versions = fetch_maven_versions(
                &client,
                "https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml",
            )
            .await?;
            let minecraft_prefix = neoforge_minecraft_prefix(&minecraft_version)?;
            Ok(sort_loader_versions(
                versions
                    .iter()
                    .filter(|version| {
                        (version.starts_with(&minecraft_prefix)
                            || version.starts_with(&format!("{minecraft_version}-")))
                            && stable_loader_version(version)
                    })
                    .map(|version| LoaderVersion {
                        version: version.to_string(),
                        channel: "release".into(),
                    })
                    .collect(),
            ))
        }
        _ => Err("Неизвестный тип загрузчика.".into()),
    }
}

fn safe_destination(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative_path = Path::new(relative);
    if relative_path.as_os_str().is_empty()
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("Некорректный относительный путь файла Minecraft.".into());
    }
    Ok(root.join(relative_path))
}

fn legacy_asset_destinations(
    assets_root: &Path,
    instance_root: &Path,
    index_id: &str,
    logical_name: &str,
    virtual_assets: bool,
    map_to_resources: bool,
) -> Result<Vec<PathBuf>, String> {
    let mut destinations = Vec::new();
    if virtual_assets {
        destinations.push(safe_destination(
            &assets_root.join("virtual").join(index_id),
            logical_name,
        )?);
    }
    if map_to_resources {
        destinations.push(safe_destination(
            &instance_root.join("resources"),
            logical_name,
        )?);
    }
    Ok(destinations)
}

fn materialize_legacy_assets(
    assets_root: &Path,
    instance_root: &Path,
    index_id: &str,
    objects: &AssetObjects,
) -> Result<(), String> {
    if !objects.virtual_assets && !objects.map_to_resources {
        return Ok(());
    }
    for (logical_name, object) in &objects.objects {
        if object.hash.len() != 40 || !object.hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Индекс Mojang содержит некорректный SHA-1 ресурса.".into());
        }
        let prefix = &object.hash[..2];
        let source = safe_destination(assets_root, &format!("objects/{prefix}/{}", object.hash))?;
        if !fs::symlink_metadata(&source).is_ok_and(|metadata| metadata.file_type().is_file())
            || !file_matches_sha1(&source, &object.hash)
        {
            return Err(format!(
                "Файл ресурса {logical_name} не найден или повреждён."
            ));
        }
        for destination in legacy_asset_destinations(
            assets_root,
            instance_root,
            index_id,
            logical_name,
            objects.virtual_assets,
            objects.map_to_resources,
        )? {
            let parent = destination
                .parent()
                .ok_or("Некорректный путь legacy-ресурса.")?;
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            if let Ok(metadata) = fs::symlink_metadata(&destination) {
                if !metadata.file_type().is_file() {
                    return Err("Путь legacy-ресурса занят не обычным файлом.".into());
                }
                if file_matches_sha1(&destination, &object.hash) {
                    continue;
                }
                fs::remove_file(&destination).map_err(|error| error.to_string())?;
            }
            fs::copy(&source, &destination).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

pub(crate) fn valid_version_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
}

const INSTALL_MARKER_FILE: &str = ".installed";

fn install_marker_path(version_dir: &Path) -> PathBuf {
    version_dir.join(INSTALL_MARKER_FILE)
}

pub(crate) fn installation_complete(version_dir: &Path, version: &str) -> bool {
    version_dir.join(format!("{version}.jar")).is_file()
        && fs::symlink_metadata(install_marker_path(version_dir))
            .is_ok_and(|metadata| metadata.file_type().is_file())
}

fn file_matches_sha1(path: &Path, expected: &str) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut hasher = Sha1::new();
    if std::io::copy(&mut file, &mut hasher).is_err() {
        return false;
    }
    let digest = hasher.finalize();
    let actual = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    actual.eq_ignore_ascii_case(expected)
}

fn extract_natives(
    archives: &[PathBuf],
    destination: &Path,
    excludes: &[String],
) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|error| error.to_string())?;
    for archive_path in archives {
        let archive_file = fs::File::open(archive_path).map_err(|error| error.to_string())?;
        let mut archive = ZipArchive::new(archive_file).map_err(|error| error.to_string())?;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
            let Some(relative_path) = entry.enclosed_name() else {
                return Err("Архив native-библиотеки содержит небезопасный путь.".into());
            };
            let relative_string = relative_path.to_string_lossy().replace('\\', "/");
            if relative_string.starts_with("META-INF/")
                || excludes
                    .iter()
                    .any(|prefix| relative_string.starts_with(prefix))
            {
                continue;
            }
            if entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                return Err("Архив native-библиотеки содержит символическую ссылку.".into());
            }
            let output = destination.join(relative_path);
            if entry.is_dir() {
                fs::create_dir_all(output).map_err(|error| error.to_string())?;
            } else {
                let parent = output.parent().ok_or("Некорректный путь native-файла.")?;
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                let mut file = fs::File::create(output).map_err(|error| error.to_string())?;
                std::io::copy(&mut entry, &mut file).map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

async fn download_file(
    client: &reqwest::Client,
    task: &DownloadTask,
    cancellation: Option<&AtomicBool>,
) -> Result<(), String> {
    trusted_url(&task.url)?;
    check_instance_install(cancellation)?;
    let path_lock = download_lock(&task.path)?;
    let _path_guard = path_lock.lock().await;
    if task.path.is_file() {
        let size_matches = task.size.is_none_or(|size| {
            fs::metadata(&task.path).is_ok_and(|metadata| metadata.len() == size)
        });
        let hash_matches = if let Some(expected_sha1) = task.sha1.as_deref() {
            let path = task.path.clone();
            let expected_sha1 = expected_sha1.to_string();
            tokio::task::spawn_blocking(move || file_matches_sha1(&path, &expected_sha1))
                .await
                .unwrap_or(false)
        } else {
            true
        };
        if size_matches && hash_matches {
            return Ok(());
        }
    }

    let parent = task
        .path
        .parent()
        .ok_or("Не удалось определить папку загрузки.")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let file_name = task
        .path
        .file_name()
        .ok_or("Некорректное имя файла загрузки.")?
        .to_string_lossy();
    let temporary_path = parent.join(format!("{file_name}.download-tmp"));

    let mut last_error = None;
    for attempt in 0..3 {
        check_instance_install(cancellation)?;
        let response_result = if let Some(cancellation) = cancellation {
            match futures::future::select(
                Box::pin(client.get(&task.url).send()),
                Box::pin(wait_for_instance_install_cancel(cancellation)),
            )
            .await
            {
                futures::future::Either::Left((result, _)) => result,
                futures::future::Either::Right(((), _)) => {
                    return Err("Установка Minecraft отменена.".into());
                }
            }
        } else {
            client.get(&task.url).send().await
        };
        let response = match response_result {
            Ok(response) => response,
            Err(error)
                if attempt < 2 && (error.is_connect() || error.is_timeout() || error.is_body()) =>
            {
                last_error = Some(format!(
                    "Не удалось скачать {} с Mojang: {error:#}",
                    task.label
                ));
                tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
                continue;
            }
            Err(error) => {
                return Err(format!(
                    "Не удалось скачать {} с Mojang: {error:#}",
                    task.label
                ));
            }
        };
        let mut response = response
            .error_for_status()
            .map_err(|error| format!("Сервер отклонил загрузку {}: {error:#}", task.label))?;
        let mut file = tokio::fs::File::create(&temporary_path)
            .await
            .map_err(|error| format!("Не удалось создать файл {}: {error}", task.label))?;
        let mut hasher = Sha1::new();
        let mut total = 0_u64;
        let mut stream_error = None;

        loop {
            let chunk_result = if let Some(cancellation) = cancellation {
                match futures::future::select(
                    Box::pin(response.chunk()),
                    Box::pin(wait_for_instance_install_cancel(cancellation)),
                )
                .await
                {
                    futures::future::Either::Left((result, _)) => result,
                    futures::future::Either::Right(((), _)) => {
                        drop(file);
                        let _ = tokio::fs::remove_file(&temporary_path).await;
                        return Err("Установка Minecraft отменена.".into());
                    }
                }
            } else {
                response.chunk().await
            };
            match chunk_result {
                Ok(Some(chunk)) => {
                    total = total.saturating_add(chunk.len() as u64);
                    if task.size.is_some_and(|size| total > size) {
                        stream_error = Some(format!(
                            "Размер файла {} превышает метаданные Mojang.",
                            task.label
                        ));
                        break;
                    }
                    hasher.update(&chunk);
                    if let Err(error) = file.write_all(&chunk).await {
                        stream_error = Some(format!(
                            "Не удалось записать {} на диск: {error}",
                            task.label
                        ));
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    stream_error = Some(format!(
                        "Соединение оборвалось при загрузке {}: {error:#}",
                        task.label
                    ));
                    break;
                }
            }
        }
        if stream_error.is_none()
            && let Err(error) = file.flush().await
        {
            stream_error = Some(format!("Не удалось сохранить {}: {error}", task.label));
        }
        drop(file);

        if let Some(error) = stream_error {
            let _ = tokio::fs::remove_file(&temporary_path).await;
            if attempt < 2 {
                last_error = Some(error);
                tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
                continue;
            }
            return Err(error);
        }
        if task.size.is_some_and(|size| total != size) {
            let _ = tokio::fs::remove_file(&temporary_path).await;
            return Err(format!(
                "Размер файла {} не совпал с метаданными Mojang.",
                task.label
            ));
        }
        if let Some(expected) = task.sha1.as_deref() {
            let digest = hasher.finalize();
            let actual = digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            if !actual.eq_ignore_ascii_case(expected) {
                let _ = tokio::fs::remove_file(&temporary_path).await;
                return Err(format!("Не совпала SHA-1-проверка файла {}.", task.label));
            }
        }

        if task.path.exists() {
            fs::remove_file(&task.path).map_err(|error| error.to_string())?;
        }
        fs::rename(&temporary_path, &task.path).map_err(|error| error.to_string())?;
        return Ok(());
    }

    Err(last_error
        .unwrap_or_else(|| format!("Не удалось загрузить {} после трёх попыток.", task.label)))
}

#[tauri::command]
pub(crate) async fn is_minecraft_installed(
    app: AppHandle,
    folder_name: String,
    version: String,
) -> Result<bool, String> {
    validate_folder_name(&folder_name)?;
    if !valid_version_component(&version) {
        return Err("Некорректная версия Minecraft.".into());
    }
    let instance = instances_root(&app)?.join(folder_name);
    let version_dir = safe_destination(&instance, &format!("versions/{version}"))?;
    Ok(installation_complete(&version_dir, &version))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstanceInstallCheck {
    id: String,
    folder_name: String,
    version: String,
}

#[tauri::command]
pub(crate) fn list_installed_instance_ids(
    app: AppHandle,
    instances: Vec<InstanceInstallCheck>,
) -> Result<Vec<String>, String> {
    if instances.len() > 512 {
        return Err("Слишком много инстансов для проверки.".into());
    }
    let root = instances_root(&app)?;
    instances
        .into_iter()
        .filter_map(|instance| {
            if instance.id.is_empty() || instance.id.len() > 64 {
                return Some(Err("Некорректный идентификатор инстанса.".into()));
            }
            let result = (|| {
                validate_folder_name(&instance.folder_name)?;
                if !valid_version_component(&instance.version) {
                    return Err("Некорректная версия Minecraft.".to_string());
                }
                let directory = root.join(&instance.folder_name);
                let version_dir = safe_destination(
                    &directory,
                    &format!("versions/{}", instance.version),
                )?;
                Ok(installation_complete(&version_dir, &instance.version)
                    .then_some(instance.id))
            })();
            Some(result)
        })
        .collect::<Result<Vec<_>, String>>()
        .map(|instances| instances.into_iter().flatten().collect())
}

fn infer_java_version(version: &str) -> Option<u8> {
    let mut components = version.split('.');
    let major = components.next()?.parse::<u32>().ok()?;
    let minor = components.next().and_then(|part| {
        part.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u32>()
            .ok()
    })?;
    let patch = components
        .next()
        .and_then(|part| {
            part.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<u32>()
                .ok()
        })
        .unwrap_or(0);
    if major == 1 {
        Some(match minor {
            0..=16 => 8,
            17 => 16,
            18..=19 => 17,
            20 if patch < 5 => 17,
            _ => 21,
        })
    } else if major >= 25 {
        Some(25)
    } else {
        Some(21)
    }
}

pub(crate) fn required_java_version(metadata: &serde_json::Value, version: &str) -> Option<u8> {
    metadata
        .pointer("/javaVersion/majorVersion")
        .and_then(serde_json::Value::as_u64)
        .and_then(|version| u8::try_from(version).ok())
        .or_else(|| infer_java_version(version))
}

#[tauri::command]
pub(crate) fn get_required_java_version(
    app: AppHandle,
    folder_name: String,
    version: String,
) -> Result<u8, String> {
    validate_folder_name(&folder_name)?;
    if !valid_version_component(&version) {
        return Err("Некорректная версия Minecraft.".into());
    }
    let version_metadata_path = instances_root(&app)?
        .join(folder_name)
        .join("versions")
        .join(&version)
        .join(format!("{version}.json"));
    let metadata: serde_json::Value = serde_json::from_slice(
        &fs::read(version_metadata_path).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let java_version = required_java_version(&metadata, &version).unwrap_or(21);
    if java_version < 8 {
        return Err(format!(
            "Для Minecraft {version} определена неподдерживаемая версия Java {java_version}."
        ));
    }
    Ok(java_version)
}

#[tauri::command]
pub(crate) async fn install_minecraft_version(
    app: AppHandle,
    folder_name: String,
    version: String,
) -> Result<(), String> {
    validate_folder_name(&folder_name)?;
    if !valid_version_component(&version) {
        return Err("Некорректная версия Minecraft.".into());
    }
    let _lock = crate::locks::InstanceLock::acquire(&folder_name)?;
    let cancellation = instance_install_cancellation(&folder_name)?;
    check_instance_install(cancellation.as_deref())?;

    let client = official_client()?;
    let manifest: VersionManifest = client
        .get(VERSION_MANIFEST_URL)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    check_instance_install(cancellation.as_deref())?;
    let manifest_version = manifest
        .versions
        .into_iter()
        .find(|entry| entry.id == version)
        .ok_or_else(|| format!("Версия Minecraft {version} отсутствует в манифесте Mojang."))?;
    trusted_url(&manifest_version.url)?;

    let details_bytes = client
        .get(&manifest_version.url)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .bytes()
        .await
        .map_err(|error| error.to_string())?;
    let details: VersionDetails =
        serde_json::from_slice(&details_bytes).map_err(|error| error.to_string())?;
    check_instance_install(cancellation.as_deref())?;

    let launcher_root = instances_root(&app)?
        .parent()
        .ok_or("Не удалось определить папку MyLauncher.")?
        .to_path_buf();
    let instance = instances_root(&app)?.join(&folder_name);
    if !instance.is_dir() {
        return Err("Папка инстанса не найдена. Сначала создайте инстанс.".into());
    }
    let version_dir = safe_destination(&instance, &format!("versions/{version}"))?;
    fs::create_dir_all(&version_dir).map_err(|error| error.to_string())?;
    fs::write(version_dir.join(format!("{version}.json")), &details_bytes)
        .map_err(|error| error.to_string())?;
    let libraries_dir = launcher_root.join("libraries");
    let assets_dir = launcher_root.join("assets");
    let mut tasks = Vec::new();
    let mut native_jars = Vec::new();
    let mut native_excludes = Vec::new();

    let client_jar = version_dir.join(format!("{version}.jar"));
    tasks.push(DownloadTask {
        url: details.downloads.client.url,
        path: client_jar,
        sha1: details.downloads.client.sha1,
        size: details.downloads.client.size,
        label: format!("Minecraft {version}"),
    });

    for library in details.libraries {
        if let Some(excludes) = library.extract.and_then(|extract| extract.exclude) {
            native_excludes.extend(excludes);
        }
        if let Some(downloads) = library.downloads {
            if let Some(artifact) = downloads.artifact
                && let Some(relative_path) = artifact.path.as_deref()
            {
                tasks.push(DownloadTask {
                    url: artifact.url,
                    path: safe_destination(&libraries_dir, relative_path)?,
                    sha1: artifact.sha1,
                    size: artifact.size,
                    label: relative_path.to_string(),
                });
            }
            if let (Some(natives), Some(classifiers)) = (library.natives, downloads.classifiers)
                && let Some(classifier_name) = natives.get("windows")
            {
                let classifier_name = classifier_name.replace("${arch}", "64");
                if let Some(artifact) = classifiers.get(&classifier_name)
                    && let Some(relative_path) = artifact.path.as_deref()
                {
                    let path = safe_destination(&libraries_dir, relative_path)?;
                    native_jars.push(path.clone());
                    tasks.push(DownloadTask {
                        url: artifact.url.clone(),
                        path,
                        sha1: artifact.sha1.clone(),
                        size: artifact.size,
                        label: relative_path.to_string(),
                    });
                }
            }
        }
    }

    let asset_index_dir = assets_dir.join("indexes");
    let asset_index_path = safe_destination(
        &asset_index_dir,
        &format!("{}.json", details.asset_index.id),
    )?;
    tasks.push(DownloadTask {
        url: details.asset_index.url.clone(),
        path: asset_index_path.clone(),
        sha1: details.asset_index.sha1.clone(),
        size: None,
        label: format!("Индекс assets {}", details.asset_index.id),
    });

    let mut asset_objects: Option<AssetObjects> = None;
    if asset_index_path.is_file()
        && let Ok(bytes) = fs::read(&asset_index_path)
    {
        asset_objects = serde_json::from_slice(&bytes).ok();
    }
    if asset_objects.is_none() {
        trusted_url(&details.asset_index.url)?;
        let downloaded = client
            .get(&details.asset_index.url)
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?
            .bytes()
            .await
            .map_err(|error| error.to_string())?;
        check_instance_install(cancellation.as_deref())?;
        asset_objects =
            Some(serde_json::from_slice(&downloaded).map_err(|error| error.to_string())?);
        fs::create_dir_all(&asset_index_dir).map_err(|error| error.to_string())?;
        fs::write(&asset_index_path, downloaded).map_err(|error| error.to_string())?;
    }
    let assets = asset_objects.ok_or("Не удалось прочитать индекс ресурсов.")?;
    for object in assets.objects.values() {
        if object.hash.len() != 40 || !object.hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Индекс Mojang содержит некорректный SHA-1 ресурса.".into());
        }
        let prefix = object.hash[..2].to_string();
        let relative_path = format!("objects/{prefix}/{}", object.hash);
        tasks.push(DownloadTask {
            url: format!(
                "https://resources.download.minecraft.net/{prefix}/{}",
                object.hash
            ),
            path: safe_destination(&assets_dir, &relative_path)?,
            sha1: Some(object.hash.clone()),
            size: Some(object.size),
            label: format!("asset {}/{}", prefix, &relative_path[10..]),
        });
        let _ = object.size;
    }

    let mut seen_paths = HashSet::new();
    tasks.retain(|task| seen_paths.insert(task.path.clone()));

    let total = tasks.len();
    let completed = Arc::new(AtomicUsize::new(0));
    let app_handle = app.clone();
    let client = Arc::new(client);
    let result = futures::stream::iter(tasks)
        .map(|task| {
            let client = Arc::clone(&client);
            let completed = Arc::clone(&completed);
            let app = app_handle.clone();
            let cancellation = cancellation.clone();
            async move {
                let label = task.label.clone();
                download_file(&client, &task, cancellation.as_deref()).await?;
                let count = completed.fetch_add(1, Ordering::Relaxed) + 1;
                let _ = app.emit(
                    "download-progress",
                    DownloadProgress {
                        completed: count,
                        total,
                        current_file: label,
                    },
                );
                Ok::<(), String>(())
            }
        })
        .buffer_unordered(8)
        .try_collect::<Vec<_>>()
        .await;

    result?;
    check_instance_install(cancellation.as_deref())?;
    materialize_legacy_assets(&assets_dir, &instance, &details.asset_index.id, &assets)?;
    extract_natives(&native_jars, &version_dir.join("natives"), &native_excludes)?;
    fs::write(install_marker_path(&version_dir), format!("{version}\n"))
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn repair_minecraft_version(
    app: AppHandle,
    folder_name: String,
    version: String,
) -> Result<(), String> {
    install_minecraft_version(app, folder_name, version).await
}

#[cfg(test)]
mod tests {
    use super::{
        AssetIndex, AssetObjects, LoaderVersion, extract_loader_archive, forge_full_version,
        infer_java_version, install_marker_path, installation_complete, is_trusted_host,
        materialize_legacy_assets, maven_metadata_versions, neoforge_is_available,
        neoforge_minecraft_prefix, neoforge_version_matches_minecraft, processor_applies_to_side,
        processor_argument_value, processor_data_value, processor_library_profile,
        processor_output_paths, profile_library_task, replace_processor_variables,
        required_java_version, safe_destination, sort_loader_versions, stable_loader_version,
        valid_version_component, write_legacy_forge_profile, write_loader_profile,
    };
    use sha1::{Digest, Sha1};
    use std::{
        collections::HashMap,
        io::{Cursor, Write},
        path::{Path, PathBuf},
    };
    use zip::{ZipWriter, write::SimpleFileOptions};

    #[test]
    fn reads_required_java_major_version_from_mojang_metadata() {
        let metadata =
            serde_json::json!({"javaVersion":{"component":"java-runtime-gamma","majorVersion":25}});
        assert_eq!(required_java_version(&metadata, "26.3"), Some(25));
        assert_eq!(
            required_java_version(&serde_json::json!({}), "26.3"),
            Some(25)
        );
        assert_eq!(infer_java_version("1.16.5"), Some(8));
        assert_eq!(infer_java_version("1.17.1"), Some(16));
        assert_eq!(infer_java_version("1.20.4"), Some(17));
        assert_eq!(infer_java_version("1.20.5"), Some(21));
    }

    #[test]
    fn rejects_paths_that_escape_download_root() {
        let root = std::path::Path::new("C:/MyLauncher/libraries");
        assert!(safe_destination(root, "../outside.jar").is_err());
        assert!(safe_destination(root, "C:/outside.jar").is_err());
        assert!(safe_destination(root, "org/example/library.jar").is_ok());
    }

    #[test]
    fn materializes_legacy_language_assets_to_virtual_and_resources_paths() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::current_dir().unwrap().join(format!("mylauncher-assets-test-{nonce}"));
        let assets_root = root.join("assets");
        let instance_root = root.join("instance");
        let contents = b"language data";
        let hash = Sha1::digest(contents)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let object_path = assets_root.join("objects").join(&hash[..2]).join(&hash);
        std::fs::create_dir_all(object_path.parent().unwrap()).unwrap();
        std::fs::write(&object_path, contents).unwrap();
        let index: AssetIndex = serde_json::from_value(serde_json::json!({
            "id": "legacy",
            "url": "https://example.invalid/index.json",
            "sha1": null
        }))
        .unwrap();
        let objects: AssetObjects = serde_json::from_value(serde_json::json!({
            "objects": {
                "lang/ru_ru.json": {
                    "hash": hash,
                    "size": contents.len()
                }
            },
            "virtual": true,
            "map_to_resources": true
        }))
        .unwrap();

        materialize_legacy_assets(&assets_root, &instance_root, &index.id, &objects).unwrap();

        assert_eq!(
            std::fs::read(assets_root.join("virtual/legacy/lang/ru_ru.json")).unwrap(),
            contents
        );
        assert_eq!(
            std::fs::read(instance_root.join("resources/lang/ru_ru.json")).unwrap(),
            contents
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_installation_requires_a_jar_and_a_finished_marker() {
        let root = std::env::current_dir().unwrap().join(format!(
            "mylauncher-install-marker-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let version_dir = root.join("1.20.1");
        std::fs::create_dir_all(&version_dir).unwrap();

        assert!(!installation_complete(&version_dir, "1.20.1"));
        std::fs::write(version_dir.join("1.20.1.jar"), b"jar").unwrap();
        assert!(!installation_complete(&version_dir, "1.20.1"));
        std::fs::write(install_marker_path(&version_dir), b"1.20.1\n").unwrap();
        assert!(installation_complete(&version_dir, "1.20.1"));
        std::fs::remove_file(version_dir.join("1.20.1.jar")).unwrap();
        assert!(!installation_complete(&version_dir, "1.20.1"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolves_forge_processor_data_and_placeholders() {
        assert!(
            processor_data_value(
                "[net.minecraft:client:1.21.1:mappings@tsrg]",
                Path::new("C:/Launcher/libraries"),
                Path::new("C:/Instance/.processor")
            )
            .unwrap()
            .replace('\\', "/")
            .ends_with("net/minecraft/client/1.21.1/client-1.21.1-mappings.tsrg")
        );
        assert!(
            processor_data_value(
                "/data/client.lzma",
                Path::new("C:/Launcher/libraries"),
                Path::new("C:/Instance/.processor")
            )
            .unwrap()
            .ends_with("data/client.lzma")
        );
        assert!(
            processor_argument_value(
                "[de.oceanlabs.mcp:mcp_config:1.20.1-20230612.114412@zip]",
                Path::new("C:/Launcher/libraries"),
                Path::new("C:/Instance/.processor")
            )
            .unwrap()
            .replace('\\', "/")
            .ends_with(
                "de/oceanlabs/mcp/mcp_config/1.20.1-20230612.114412/mcp_config-1.20.1-20230612.114412.zip"
            )
        );

        let variables = HashMap::from([
            ("SIDE".to_string(), "client".to_string()),
            (
                "MOJMAPS".to_string(),
                "C:/Launcher/libraries/maps.tsrg".to_string(),
            ),
        ]);
        assert_eq!(
            replace_processor_variables("--side {SIDE} --output {MOJMAPS}", &variables),
            "--side client --output C:/Launcher/libraries/maps.tsrg"
        );
        assert!(processor_applies_to_side(
            &serde_json::json!({"sides":["client"]}),
            "client"
        ));
        assert!(!processor_applies_to_side(
            &serde_json::json!({"sides":["server"]}),
            "client"
        ));
    }

    #[test]
    fn identifies_generated_processor_outputs_and_dependencies() {
        let installer = serde_json::json!({
            "data": {
                "PATCHED": {"client": "[net.minecraftforge:forge:1.21.1-52.1.20:client]"}
            },
            "libraries": [{
                "name": "net.minecraftforge:binarypatcher:1.2.0",
                "url": "https://maven.minecraftforge.net/"
            }],
            "processors": [{
                "jar": "net.minecraftforge:binarypatcher:1.2.0",
                "classpath": ["net.minecraftforge:installertools:1.4.0"],
                "sides": ["client"],
                "outputs": {"{PATCHED}": "abc123"}
            }]
        });
        let library_root = Path::new("C:/Launcher/libraries");
        let output_paths = processor_output_paths(&installer, "client", library_root).unwrap();
        assert!(output_paths.contains(&PathBuf::from(
            "C:/Launcher/libraries/net/minecraftforge/forge/1.21.1-52.1.20/forge-1.21.1-52.1.20-client.jar"
        )));

        let processor_profile =
            processor_library_profile(&serde_json::json!({"libraries": []}), &installer, "client")
                .unwrap();
        let libraries = processor_profile["libraries"].as_array().unwrap();
        assert_eq!(libraries.len(), 2);
        assert_eq!(libraries[0]["url"], "https://maven.minecraftforge.net/");
        assert_eq!(
            libraries[1]["name"],
            "net.minecraftforge:installertools:1.4.0"
        );
    }

    #[test]
    fn allows_only_expected_mojang_and_maven_hosts() {
        assert!(is_trusted_host("piston-data.mojang.com"));
        assert!(is_trusted_host("repo.maven.apache.org"));
        assert!(is_trusted_host("fill-data.papermc.io"));
        assert!(is_trusted_host("api.purpurmc.org"));
        assert!(!is_trusted_host("mojang.com.attacker.invalid"));
        assert!(!is_trusted_host("example.org"));
    }

    #[test]
    fn extracts_loader_profile_and_maven_files_from_installer_zip() {
        let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
        archive
            .start_file("version.json", SimpleFileOptions::default())
            .unwrap();
        archive
            .write_all(br#"{"id":"forge-1.21.1-52.0.1","libraries":[],"mainClass":"example.Main"}"#)
            .unwrap();
        archive
            .start_file("install_profile.json", SimpleFileOptions::default())
            .unwrap();
        archive
            .write_all(br#"{"minecraft":"1.21.1","libraries":[{"name":"net.minecraftforge:forge:1.21.1-52.0.1:universal","url":"maven"}]}"#)
            .unwrap();
        archive
            .start_file(
                "maven/com/example/library/1.0/library-1.0.jar",
                SimpleFileOptions::default(),
            )
            .unwrap();
        archive.write_all(b"library bytes").unwrap();
        let archive_bytes = archive.finish().unwrap().into_inner();

        let temp = std::env::current_dir().unwrap().join(format!(
            "mylauncher-loader-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let libraries = temp.join("libraries");
        let instance = temp.join("instance");
        let (profile_id, profile, installer_profile) =
            extract_loader_archive(&archive_bytes, &libraries, &instance, "1.21.1").unwrap();

        assert_eq!(profile_id, "forge-1.21.1-52.0.1");
        assert_eq!(profile["inheritsFrom"], "1.21.1");
        let installer_profile = installer_profile.unwrap();
        assert_eq!(
            installer_profile["libraries"][0]["name"],
            "net.minecraftforge:forge:1.21.1-52.0.1:universal"
        );
        let installer_library =
            profile_library_task(&installer_profile["libraries"][0], &libraries)
                .unwrap()
                .unwrap();
        assert_eq!(
            installer_library.url,
            "https://maven.minecraftforge.net/net/minecraftforge/forge/1.21.1-52.0.1/forge-1.21.1-52.0.1-universal.jar"
        );
        assert!(
            instance
                .join("versions/forge-1.21.1-52.0.1/forge-1.21.1-52.0.1.json")
                .is_file()
        );
        assert_eq!(
            std::fs::read(libraries.join("com/example/library/1.0/library-1.0.jar")).unwrap(),
            b"library bytes"
        );
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn extracts_version_profile_from_install_profile_fallback() {
        let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
        archive
            .start_file("install_profile.json", SimpleFileOptions::default())
            .unwrap();
        archive
            .write_all(br#"{"versionInfo":{"id":"neoforge-1.21.1-21.1.1","inheritsFrom":"1.21.1","libraries":[]}}"#)
            .unwrap();
        let archive_bytes = archive.finish().unwrap().into_inner();
        let temp = std::env::current_dir().unwrap().join(format!(
            "mylauncher-install-profile-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let (profile_id, profile, _) = extract_loader_archive(
            &archive_bytes,
            &temp.join("libraries"),
            &temp.join("instance"),
            "1.21.1",
        )
        .unwrap();

        assert_eq!(profile_id, "neoforge-1.21.1-21.1.1");
        assert_eq!(profile["inheritsFrom"], "1.21.1");
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn prefers_matching_install_profile_version_info_over_mismatched_version_json() {
        let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
        archive
            .start_file("version.json", SimpleFileOptions::default())
            .unwrap();
        archive
            .write_all(br#"{"id":"forge-1.21.1-52.0.1","inheritsFrom":"1.20.1","libraries":[]}"#)
            .unwrap();
        archive
            .start_file("install_profile.json", SimpleFileOptions::default())
            .unwrap();
        archive
            .write_all(br#"{"versionInfo":{"id":"forge-1.21.1-52.0.1","inheritsFrom":"1.21.1","libraries":[]}}"#)
            .unwrap();
        let archive_bytes = archive.finish().unwrap().into_inner();
        let temp = std::env::current_dir().unwrap().join(format!(
            "mylauncher-profile-candidate-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let (profile_id, profile, _) = extract_loader_archive(
            &archive_bytes,
            &temp.join("libraries"),
            &temp.join("instance"),
            "1.21.1",
        )
        .unwrap();

        assert_eq!(profile_id, "forge-1.21.1-52.0.1");
        assert_eq!(profile["inheritsFrom"], "1.21.1");
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn preserves_processors_for_execution_after_profile_extraction() {
        let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
        archive
            .start_file("version.json", SimpleFileOptions::default())
            .unwrap();
        archive
            .write_all(br#"{"id":"forge-1.21.1-52.0.1","inheritsFrom":"1.21.1","libraries":[]}"#)
            .unwrap();
        archive
            .start_file("install_profile.json", SimpleFileOptions::default())
            .unwrap();
        archive
            .write_all(br#"{"minecraft":"1.21.1","processors":[{"jar":"example:processor:1"}]}"#)
            .unwrap();
        let archive_bytes = archive.finish().unwrap().into_inner();
        let temp = std::env::current_dir().unwrap().join(format!(
            "mylauncher-processor-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).unwrap();

        let (profile_id, _, installer_profile) =
            extract_loader_archive(&archive_bytes, &temp.join("libraries"), &temp, "1.21.1")
                .unwrap();

        assert_eq!(profile_id, "forge-1.21.1-52.0.1");
        assert_eq!(
            installer_profile.unwrap()["processors"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(
            temp.join("versions/forge-1.21.1-52.0.1/forge-1.21.1-52.0.1.json")
                .is_file()
        );
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn writes_legacy_forge_universal_profile_without_replacing_vanilla() {
        let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
        archive
            .start_file("META-INF/MANIFEST.MF", SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"Manifest-Version: 1.0\n").unwrap();
        let universal = archive.finish().unwrap().into_inner();
        let temp = std::env::current_dir().unwrap().join(format!(
            "mylauncher-legacy-forge-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let instance = temp.join("instance");
        let base_dir = instance.join("versions/1.12.2");
        std::fs::create_dir_all(&base_dir).unwrap();
        std::fs::write(
            base_dir.join("1.12.2.json"),
            br#"{"id":"1.12.2","minecraftArguments":"--username ${auth_player_name}"}"#,
        )
        .unwrap();

        let (profile_id, profile) = write_legacy_forge_profile(
            &instance,
            &temp.join("libraries"),
            "1.12.2",
            "14.23.5.2860",
            "https://maven.minecraftforge.net/net/minecraftforge/forge/1.12.2-14.23.5.2860/forge-1.12.2-14.23.5.2860-universal.jar",
            "jar",
            &universal,
        )
        .unwrap();

        assert_eq!(profile_id, "1.12.2-forge-14.23.5.2860");
        assert_eq!(profile["inheritsFrom"], "1.12.2");
        assert!(instance.join("versions/1.12.2/1.12.2.json").is_file());
        assert!(
            instance
                .join("versions/1.12.2-forge-14.23.5.2860/1.12.2-forge-14.23.5.2860.json")
                .is_file()
        );
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn rejects_loader_profile_with_mismatched_inherits_from() {
        let instance = std::env::current_dir().unwrap().join(format!(
            "mylauncher-profile-parent-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let profile = serde_json::json!({
            "id": "forge-1.21.1-52.0.1",
            "inheritsFrom": "1.20.1"
        });

        assert!(write_loader_profile(&instance, profile, "1.21.1").is_err());
        assert!(!instance.exists());
    }

    #[test]
    fn accepts_only_safe_loader_version_url_segments() {
        assert!(valid_version_component("1.21.1"));
        assert!(valid_version_component("21.1.200-beta"));
        assert!(!valid_version_component("../profile"));
        assert!(!valid_version_component(".."));
        assert!(!valid_version_component("1.21.1?other"));
    }

    #[test]
    fn maps_neoforge_versions_to_exact_minecraft_series() {
        assert_eq!(neoforge_minecraft_prefix("1.21").unwrap(), "21.0.");
        assert_eq!(neoforge_minecraft_prefix("1.21.1").unwrap(), "21.1.");
        assert_eq!(neoforge_minecraft_prefix("1.20.4").unwrap(), "20.4.");
        assert_eq!(neoforge_minecraft_prefix("26.1").unwrap(), "26.1.");
        assert_eq!(neoforge_minecraft_prefix("26.1.2").unwrap(), "26.1.2.");
        assert_eq!(neoforge_minecraft_prefix("26.3").unwrap(), "26.3.");
        assert_eq!(neoforge_minecraft_prefix("21.1.1").unwrap(), "21.1.1.");
        assert!(neoforge_minecraft_prefix("26.x").is_err());
        assert!(neoforge_version_matches_minecraft("21.1.209", "1.21.1").unwrap());
        assert!(neoforge_version_matches_minecraft("1.20.4-20.4.237", "1.20.4").unwrap());
        assert!(!neoforge_version_matches_minecraft("21.4.1", "1.21.1").unwrap());
        assert!(!neoforge_is_available("1.20.0"));
        assert!(neoforge_is_available("1.20.1"));
        assert!(neoforge_is_available("1.21.1"));
    }

    #[test]
    fn parses_maven_versions_and_filters_prereleases() {
        let metadata = r#"<metadata><versioning><versions>
            <version>1.20.1-47.2.0</version>
            <version>1.20.1-47.3.0-beta</version>
            <version>21.1.180</version>
        </versions></versioning></metadata>"#;
        let versions = maven_metadata_versions(metadata).unwrap();
        assert_eq!(
            versions,
            ["1.20.1-47.2.0", "1.20.1-47.3.0-beta", "21.1.180"]
        );
        assert!(stable_loader_version(&versions[0]));
        assert!(!stable_loader_version(&versions[1]));
        assert_eq!(forge_full_version("1.20.1", "47.2.0"), "1.20.1-47.2.0");
        assert_eq!(
            forge_full_version("1.20.1", "1.20.1-47.2.0"),
            "1.20.1-47.2.0"
        );
    }

    #[test]
    fn sorts_loader_versions_newest_first_and_prefers_recommended() {
        let versions = sort_loader_versions(vec![
            LoaderVersion {
                version: "21.1.9".into(),
                channel: "release".into(),
            },
            LoaderVersion {
                version: "21.1.265".into(),
                channel: "release".into(),
            },
            LoaderVersion {
                version: "52.1.20".into(),
                channel: "latest".into(),
            },
            LoaderVersion {
                version: "52.1.20".into(),
                channel: "recommended".into(),
            },
            LoaderVersion {
                version: "0.20.2".into(),
                channel: "preview".into(),
            },
            LoaderVersion {
                version: "0.19.5".into(),
                channel: "stable".into(),
            },
        ]);

        assert_eq!(versions[0].version, "52.1.20");
        assert_eq!(versions[0].channel, "recommended");
        assert_eq!(versions[1].version, "0.19.5");
        assert_eq!(versions[2].version, "21.1.265");
        assert_eq!(versions[3].version, "21.1.9");
        assert_eq!(versions[4].version, "0.20.2");
        assert_eq!(versions.len(), 5);
    }

    #[test]
    fn builds_fabric_maven_download_from_profile_coordinates() {
        let library = serde_json::json!({
            "name": "net.fabricmc:intermediary:1.21.1",
            "url": "https://maven.fabricmc.net/"
        });
        let task = profile_library_task(&library, Path::new("C:/MyLauncher/libraries"))
            .unwrap()
            .unwrap();

        assert_eq!(
            task.url,
            "https://maven.fabricmc.net/net/fabricmc/intermediary/1.21.1/intermediary-1.21.1.jar"
        );
        assert_eq!(
            task.path,
            PathBuf::from(
                "C:/MyLauncher/libraries/net/fabricmc/intermediary/1.21.1/intermediary-1.21.1.jar"
            )
        );
    }

    #[test]
    fn builds_forge_maven_url_when_repository_url_is_empty() {
        for repository in ["", "maven"] {
            let library = serde_json::json!({
                "name": "net.minecraftforge:forge:1.21.1-52.0.1:universal",
                "url": repository
            });
            let task = profile_library_task(&library, Path::new("C:/MyLauncher/libraries"))
                .unwrap()
                .unwrap();

            assert_eq!(
                task.url,
                "https://maven.minecraftforge.net/net/minecraftforge/forge/1.21.1-52.0.1/forge-1.21.1-52.0.1-universal.jar"
            );
        }
    }

    #[test]
    fn builds_forge_mcp_config_zip_download_from_install_profile() {
        let library = serde_json::json!({
            "name": "de.oceanlabs.mcp:mcp_config:1.20.1-20230612.114412@zip",
            "url": "https://maven.minecraftforge.net/"
        });
        let task = profile_library_task(&library, Path::new("C:/MyLauncher/libraries"))
            .unwrap()
            .unwrap();

        assert_eq!(
            task.url,
            "https://maven.minecraftforge.net/de/oceanlabs/mcp/mcp_config/1.20.1-20230612.114412/mcp_config-1.20.1-20230612.114412.zip"
        );
        assert_eq!(
            task.path,
            PathBuf::from(
                "C:/MyLauncher/libraries/de/oceanlabs/mcp/mcp_config/1.20.1-20230612.114412/mcp_config-1.20.1-20230612.114412.zip"
            )
        );
    }

    #[test]
    fn skips_forge_client_artifacts_without_direct_download_urls() {
        let library = serde_json::json!({
            "name": "net.minecraftforge:forge:1.21-51.0.33:client",
            "downloads": {"artifact": {
                "path": "net/minecraftforge/forge/1.21-51.0.33/forge-1.21-51.0.33-client.jar",
                "url": "",
                "sha1": "7d55e3f6f9d91d8e5ee66d374d17d147f4b3d130"
            }}
        });

        assert!(
            profile_library_task(&library, Path::new("C:/MyLauncher/libraries"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn resolves_relative_forge_artifact_url_against_official_maven() {
        let library = serde_json::json!({
            "name": "net.minecraftforge:forge:1.21.1-52.0.1:universal",
            "url": "maven",
            "downloads": { "artifact": {
                "path": "net/minecraftforge/forge/1.21.1-52.0.1/forge-1.21.1-52.0.1-universal.jar",
                "url": "forge-1.21.1-52.0.1-universal.jar"
            }}
        });
        let task = profile_library_task(&library, Path::new("C:/MyLauncher/libraries"))
            .unwrap()
            .unwrap();

        assert_eq!(
            task.url,
            "https://maven.minecraftforge.net/net/minecraftforge/forge/1.21.1-52.0.1/forge-1.21.1-52.0.1-universal.jar"
        );
    }
}

fn write_loader_profile(
    instance: &Path,
    profile: serde_json::Value,
    minecraft_version: &str,
) -> Result<(String, serde_json::Value), String> {
    let profile_id = profile
        .get("id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| valid_version_component(id))
        .ok_or("В профиле загрузчика отсутствует корректный id.")?
        .to_string();
    match profile
        .get("inheritsFrom")
        .and_then(serde_json::Value::as_str)
    {
        Some(parent) if parent == minecraft_version => {}
        _ => {
            return Err("Профиль загрузчика не соответствует выбранной версии Minecraft.".into());
        }
    }

    let version_dir = instance.join("versions").join(&profile_id);
    fs::create_dir_all(&version_dir).map_err(|error| error.to_string())?;
    let profile_path = version_dir.join(format!("{profile_id}.json"));
    fs::write(
        profile_path,
        serde_json::to_vec_pretty(&profile).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok((profile_id, profile))
}

fn extract_loader_archive(
    archive_bytes: &[u8],
    libraries_root: &Path,
    instance: &Path,
    minecraft_version: &str,
) -> Result<(String, serde_json::Value, Option<serde_json::Value>), String> {
    let mut archive = ZipArchive::new(Cursor::new(archive_bytes))
        .map_err(|error| format!("Не удалось открыть installer JAR как ZIP: {error}"))?;
    let mut version_profile = None;
    let mut installer_profile = None;
    let mut total_uncompressed = 0_u64;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        total_uncompressed = total_uncompressed.saturating_add(entry.size());
        if total_uncompressed > 1_500_000_000 {
            return Err("Распакованный installer превышает допустимый размер.".into());
        }
        let relative_path = entry
            .enclosed_name()
            .ok_or("Installer содержит небезопасный путь.")?;

        if relative_path == Path::new("version.json")
            || relative_path == Path::new("install_profile.json")
        {
            if entry.size() > 10_000_000 {
                return Err("Метаданные installer превышают допустимый размер.".into());
            }
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            entry
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            let json: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            if relative_path == Path::new("version.json") {
                version_profile = Some(json);
            } else {
                installer_profile = Some(json);
            }
            continue;
        }

        let Ok(maven_path) = relative_path.strip_prefix("maven") else {
            continue;
        };
        if maven_path.as_os_str().is_empty() {
            continue;
        }
        if !maven_path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        {
            return Err("Installer содержит небезопасный путь библиотеки.".into());
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("Installer содержит символическую ссылку.".into());
        }

        let output = safe_destination(libraries_root, &maven_path.to_string_lossy())?;
        if entry.is_dir() {
            fs::create_dir_all(output).map_err(|error| error.to_string())?;
        } else {
            let parent = output
                .parent()
                .ok_or("Некорректный путь библиотеки installer.")?;
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            let mut file = fs::File::create(output).map_err(|error| error.to_string())?;
            std::io::copy(&mut entry, &mut file).map_err(|error| error.to_string())?;
        }
    }

    let installer_minecraft = installer_profile
        .as_ref()
        .and_then(|installer| installer.get("minecraft"))
        .and_then(serde_json::Value::as_str);
    let installer_matches = installer_minecraft == Some(minecraft_version);
    let mut profile_candidates = Vec::new();
    if let Some(profile) = version_profile {
        profile_candidates.push(profile);
    }
    if let Some(profile) = installer_profile.as_ref().and_then(|installer| {
        installer
            .get("versionInfo")
            .or_else(|| installer.get("version"))
            .filter(|version| version.is_object())
            .cloned()
    }) {
        profile_candidates.push(profile);
    }
    let matching_profile = profile_candidates.iter().position(|profile| {
        profile
            .get("inheritsFrom")
            .and_then(serde_json::Value::as_str)
            == Some(minecraft_version)
    });
    let selected_profile = matching_profile.or_else(|| {
        if installer_matches {
            (!profile_candidates.is_empty()).then_some(0)
        } else {
            None
        }
    });
    let Some(mut profile) = selected_profile.map(|index| profile_candidates.remove(index)) else {
        return Err("Профиль загрузчика не соответствует выбранной версии Minecraft.".into());
    };
    if installer_matches {
        profile["inheritsFrom"] = serde_json::Value::String(minecraft_version.to_string());
    }
    let (profile_id, profile) = write_loader_profile(instance, profile, minecraft_version)?;
    Ok((profile_id, profile, installer_profile))
}

fn library_repository_base(library: &serde_json::Value) -> Result<reqwest::Url, String> {
    let coordinate = library
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let fallback_base = match coordinate.split(':').next().unwrap_or_default() {
        "net.minecraftforge" => "https://maven.minecraftforge.net/",
        "net.neoforged" => "https://maven.neoforged.net/releases/",
        "net.fabricmc" => "https://maven.fabricmc.net/",
        _ => "https://libraries.minecraft.net/",
    };
    let configured_base = library
        .get("url")
        .and_then(serde_json::Value::as_str)
        .filter(|url| !url.trim().is_empty() && url.contains("://"))
        .unwrap_or(fallback_base);
    let base = reqwest::Url::parse(configured_base).map_err(|error| error.to_string())?;
    if base.scheme() != "https" || !base.host_str().is_some_and(is_trusted_host) {
        return Err("Библиотека загрузчика указана на недоверенном домене.".into());
    }
    Ok(base)
}

fn profile_library_task(
    library: &serde_json::Value,
    libraries_root: &Path,
) -> Result<Option<DownloadTask>, String> {
    let artifact = library.pointer("/downloads/artifact");
    let (relative_path, url, sha1, size) = if let Some(artifact) = artifact {
        let (Some(path), Some(raw_url)) = (
            artifact.get("path").and_then(serde_json::Value::as_str),
            artifact.get("url").and_then(serde_json::Value::as_str),
        ) else {
            return Ok(None);
        };
        if raw_url.trim().is_empty() {
            return Ok(None);
        }
        let url = match reqwest::Url::parse(raw_url) {
            Ok(url) => {
                if url.scheme() != "https" || !url.host_str().is_some_and(is_trusted_host) {
                    return Err("Библиотека загрузчика указана на недоверенном домене.".into());
                }
                url.to_string()
            }
            Err(_) => library_repository_base(library)?
                .join(path)
                .map_err(|error| error.to_string())?
                .to_string(),
        };
        (
            path.to_string(),
            url,
            artifact
                .get("sha1")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            artifact.get("size").and_then(serde_json::Value::as_u64),
        )
    } else {
        let Some(coordinate) = library.get("name").and_then(serde_json::Value::as_str) else {
            return Ok(None);
        };
        let Some(relative_path) = maven_library_path(coordinate) else {
            return Ok(None);
        };
        let relative_path = relative_path
            .to_str()
            .ok_or("Путь библиотеки содержит неподдерживаемые символы.")?
            .replace('\\', "/");
        let url = library_repository_base(library)?
            .join(&relative_path)
            .map_err(|error| error.to_string())?
            .to_string();
        (relative_path, url, None, None)
    };

    let path = safe_destination(libraries_root, &relative_path)?;
    Ok(Some(DownloadTask {
        url,
        path,
        sha1,
        size,
        label: relative_path.to_string(),
    }))
}

fn processor_data_value(
    value: &str,
    libraries_root: &Path,
    processor_root: &Path,
) -> Result<String, String> {
    if let Some(coordinate) = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        let relative = maven_library_path(coordinate)
            .ok_or("Некорректная Maven-координата в installer processor.")?;
        return Ok(
            safe_destination(libraries_root, &relative.to_string_lossy())?
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(path) = value.strip_prefix("/data/") {
        return Ok(safe_destination(processor_root, &format!("data/{path}"))?
            .to_string_lossy()
            .into_owned());
    }
    Ok(value.trim_matches('\'').trim_matches('"').to_string())
}

fn processor_argument_value(
    value: &str,
    libraries_root: &Path,
    processor_root: &Path,
) -> Result<String, String> {
    if value.starts_with('[') || value.starts_with("/data/") {
        processor_data_value(value, libraries_root, processor_root)
    } else {
        Ok(value.to_string())
    }
}

fn processor_applies_to_side(processor: &serde_json::Value, side: &str) -> bool {
    processor
        .get("sides")
        .and_then(serde_json::Value::as_array)
        .is_none_or(|sides| {
            sides
                .iter()
                .filter_map(serde_json::Value::as_str)
                .any(|processor_side| processor_side == side)
        })
}

fn replace_processor_variables(value: &str, variables: &HashMap<String, String>) -> String {
    variables
        .iter()
        .fold(value.to_string(), |result, (name, value)| {
            result.replace(&format!("{{{name}}}"), value)
        })
}

async fn download_profile_libraries(
    app: &AppHandle,
    client: &reqwest::Client,
    profile: &serde_json::Value,
    libraries_root: &Path,
    generated_outputs: &HashSet<PathBuf>,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<(), String> {
    let libraries = profile
        .get("libraries")
        .and_then(serde_json::Value::as_array)
        .ok_or("В профиле загрузчика отсутствует список библиотек.")?;
    let features = default_game_features();
    let tasks = libraries
        .iter()
        .map(|library| {
            if !rules_allow(library.get("rules"), &features) {
                return Ok(None);
            }
            Ok(profile_library_task(library, libraries_root)?
                .filter(|task| !generated_outputs.contains(&task.path)))
        })
        .collect::<Result<Vec<_>, String>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let total = tasks.len();
    let completed = Arc::new(AtomicUsize::new(0));
    let client = Arc::new(client.clone());
    futures::stream::iter(tasks)
        .map(|task| {
            let client = Arc::clone(&client);
            let completed = Arc::clone(&completed);
            let app = app.clone();
            let cancellation = cancellation.clone();
            async move {
                let label = task.label.clone();
                download_file(&client, &task, cancellation.as_deref()).await?;
                let count = completed.fetch_add(1, Ordering::Relaxed) + 1;
                let _ = app.emit(
                    "download-progress",
                    DownloadProgress {
                        completed: count,
                        total,
                        current_file: label,
                    },
                );
                Ok::<(), String>(())
            }
        })
        .buffer_unordered(8)
        .try_collect::<Vec<_>>()
        .await?;
    Ok(())
}

fn processor_output_paths(
    installer_profile: &serde_json::Value,
    side: &str,
    libraries_root: &Path,
) -> Result<HashSet<PathBuf>, String> {
    let data = installer_profile
        .get("data")
        .and_then(serde_json::Value::as_object);
    let Some(processors) = installer_profile
        .get("processors")
        .and_then(serde_json::Value::as_array)
    else {
        return Ok(HashSet::new());
    };
    let mut paths = HashSet::new();
    for processor in processors
        .iter()
        .filter(|processor| processor_applies_to_side(processor, side))
    {
        let Some(outputs) = processor
            .get("outputs")
            .and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        for output in outputs.keys() {
            let name = output.trim_matches(['{', '}']);
            let Some(value) = data
                .and_then(|data| data.get(name))
                .and_then(|definition| definition.get(side))
                .and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            paths.insert(PathBuf::from(processor_data_value(
                value,
                libraries_root,
                libraries_root,
            )?));
        }
    }
    Ok(paths)
}

fn processor_library_profile(
    profile: &serde_json::Value,
    installer_profile: &serde_json::Value,
    side: &str,
) -> Result<serde_json::Value, String> {
    let mut definitions = HashMap::new();
    for library in profile
        .get("libraries")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .chain(
            installer_profile
                .get("libraries")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten(),
        )
    {
        if let Some(name) = library.get("name").and_then(serde_json::Value::as_str) {
            definitions.insert(name.to_string(), library.clone());
        }
    }

    let processors = installer_profile
        .get("processors")
        .and_then(serde_json::Value::as_array)
        .ok_or("В installer отсутствует список processors.")?;
    let mut coordinates = HashSet::new();
    let mut libraries = Vec::new();
    for processor in processors
        .iter()
        .filter(|processor| processor_applies_to_side(processor, side))
    {
        for coordinate in std::iter::once(processor.get("jar")).chain(
            processor
                .get("classpath")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .map(Some),
        ) {
            let Some(coordinate) = coordinate.and_then(serde_json::Value::as_str) else {
                return Err("Некорректная библиотека в installer processor.".into());
            };
            if !coordinates.insert(coordinate.to_string()) {
                continue;
            }
            let mut library = definitions
                .get(coordinate)
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"name": coordinate}));
            if let Some(object) = library.as_object_mut() {
                object.insert("name".into(), serde_json::Value::String(coordinate.into()));
                object.remove("rules");
            }
            libraries.push(library);
        }
    }
    Ok(serde_json::json!({"libraries": libraries}))
}

fn processor_library_path(coordinate: &str, libraries_root: &Path) -> Result<PathBuf, String> {
    let relative_path = maven_library_path(coordinate)
        .ok_or("Некорректная Maven-координата в installer processor.")?;
    safe_destination(libraries_root, &relative_path.to_string_lossy())
}

fn processor_main_class(jar_path: &Path) -> Result<String, String> {
    let file = fs::File::open(jar_path).map_err(|error| error.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|error| error.to_string())?;
    let mut manifest = String::new();
    archive
        .by_name("META-INF/MANIFEST.MF")
        .map_err(|error| format!("У processor JAR отсутствует manifest: {error}"))?
        .read_to_string(&mut manifest)
        .map_err(|error| error.to_string())?;
    manifest
        .lines()
        .find_map(|line| line.strip_prefix("Main-Class:").map(str::trim))
        .filter(|main_class| !main_class.is_empty())
        .map(str::to_string)
        .ok_or("В manifest processor JAR отсутствует Main-Class.".into())
}

fn output_hash_matches(path: &Path, expected: &str) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut hasher = Sha1::new();
    if std::io::copy(&mut file, &mut hasher).is_err() {
        return false;
    }
    let actual = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    actual.eq_ignore_ascii_case(expected.trim_matches('\''))
}

#[allow(clippy::too_many_arguments)]
fn processor_variables(
    installer_bytes: &[u8],
    installer_profile: &serde_json::Value,
    side: &str,
    launcher_root: &Path,
    instance: &Path,
    libraries_root: &Path,
    processor_root: &Path,
    installer_path: &Path,
    minecraft_version: &str,
) -> Result<HashMap<String, String>, String> {
    let mut variables = HashMap::from([
        ("ROOT".into(), launcher_root.to_string_lossy().into_owned()),
        (
            "MINECRAFT_JAR".into(),
            instance
                .join("versions")
                .join(minecraft_version)
                .join(format!("{minecraft_version}.jar"))
                .to_string_lossy()
                .into_owned(),
        ),
        ("MINECRAFT_VERSION".into(), minecraft_version.to_string()),
        (
            "INSTALLER".into(),
            installer_path.to_string_lossy().into_owned(),
        ),
        (
            "LIBRARY_DIR".into(),
            libraries_root.to_string_lossy().into_owned(),
        ),
        ("SIDE".into(), side.to_string()),
    ]);
    let Some(data) = installer_profile
        .get("data")
        .and_then(serde_json::Value::as_object)
    else {
        return Ok(variables);
    };
    let mut archive = ZipArchive::new(Cursor::new(installer_bytes))
        .map_err(|error| format!("Не удалось прочитать данные installer: {error}"))?;
    for (name, definition) in data {
        let Some(value) = definition.get(side).and_then(serde_json::Value::as_str) else {
            continue;
        };
        let resolved = processor_data_value(value, libraries_root, processor_root)?;
        if let Some(archive_path) = value.strip_prefix('/') {
            let safe_path = safe_destination(processor_root, archive_path)?;
            let mut entry = archive
                .by_name(archive_path)
                .map_err(|error| format!("В installer отсутствует {archive_path}: {error}"))?;
            if entry.size() > 1_500_000_000 {
                return Err("Файл данных processor превышает допустимый размер.".into());
            }
            let parent = safe_path
                .parent()
                .ok_or("Некорректный путь данных processor.")?;
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            let mut output = fs::File::create(&safe_path).map_err(|error| error.to_string())?;
            std::io::copy(&mut entry, &mut output).map_err(|error| error.to_string())?;
        }
        variables.insert(name.clone(), resolved);
    }
    Ok(variables)
}

fn read_processor_output(
    mut reader: impl Read,
    stream: &'static str,
    log_file: Arc<Mutex<fs::File>>,
) -> Vec<u8> {
    const MAX_ERROR_OUTPUT_BYTES: usize = 8 * 1024;

    if let Ok(mut file) = log_file.lock() {
        let _ = writeln!(file, "\n[{stream}]");
    }
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        if let Ok(mut file) = log_file.lock() {
            let _ = file.write_all(&buffer[..count]);
        }
        output.extend_from_slice(&buffer[..count]);
        if output.len() > MAX_ERROR_OUTPUT_BYTES {
            let excess = output.len() - MAX_ERROR_OUTPUT_BYTES;
            output.drain(..excess);
        }
    }
    output
}

#[allow(clippy::too_many_arguments)]
fn run_install_processors(
    java_path: &Path,
    folder_name: &str,
    installer_bytes: &[u8],
    installer_profile: &serde_json::Value,
    launcher_root: &Path,
    instance: &Path,
    libraries_root: &Path,
    minecraft_version: &str,
    processor_root: &Path,
    cancellation: Option<&AtomicBool>,
) -> Result<(), String> {
    let java = if java_path
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("javaw.exe"))
    {
        java_path.with_file_name("java.exe")
    } else {
        java_path.to_path_buf()
    };
    if !java.is_file() {
        return Err("Для выполнения Forge processors не найден java.exe.".into());
    }
    let installer_path = processor_root.join("installer.jar");
    fs::create_dir_all(processor_root).map_err(|error| error.to_string())?;
    fs::write(&installer_path, installer_bytes).map_err(|error| error.to_string())?;
    let variables = processor_variables(
        installer_bytes,
        installer_profile,
        "client",
        launcher_root,
        instance,
        libraries_root,
        processor_root,
        &installer_path,
        minecraft_version,
    )?;
    let processors = installer_profile
        .get("processors")
        .and_then(serde_json::Value::as_array)
        .ok_or("В installer отсутствует список processors.")?;
    let logs_directory = launcher_root.join("logs");
    fs::create_dir_all(&logs_directory).map_err(|error| error.to_string())?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let log_path = logs_directory.join(format!("{folder_name}-{timestamp}-install.log"));
    let log_file = Arc::new(Mutex::new(
        fs::File::create(&log_path).map_err(|error| error.to_string())?,
    ));
    for processor in processors
        .iter()
        .filter(|processor| processor_applies_to_side(processor, "client"))
    {
        check_instance_install(cancellation)?;
        let jar_coordinate = processor
            .get("jar")
            .and_then(serde_json::Value::as_str)
            .ok_or("В processor отсутствует координата jar.")?;
        let processor_jar = processor_library_path(jar_coordinate, libraries_root)?;
        if !processor_jar.is_file() {
            return Err(format!("Не найдена библиотека processor {jar_coordinate}."));
        }
        let mut classpath = vec![processor_jar.clone()];
        if let Some(entries) = processor
            .get("classpath")
            .and_then(serde_json::Value::as_array)
        {
            for entry in entries {
                let coordinate = entry
                    .as_str()
                    .ok_or("Некорректная запись classpath в processor.")?;
                let path = processor_library_path(coordinate, libraries_root)?;
                if !path.is_file() {
                    return Err(format!("Не найдена библиотека processor {coordinate}."));
                }
                classpath.push(path);
            }
        }
        let mut arguments = Vec::new();
        for argument in processor
            .get("args")
            .and_then(serde_json::Value::as_array)
            .ok_or("В processor отсутствует список args.")?
        {
            let argument = argument
                .as_str()
                .ok_or("Некорректный аргумент installer processor.")?;
            let argument = replace_processor_variables(argument, &variables);
            let argument = processor_argument_value(&argument, libraries_root, processor_root)?;
            if argument.contains(['{', '}']) {
                return Err(format!(
                    "Для Forge processor не удалось разрешить аргумент {argument}."
                ));
            }
            arguments.push(argument);
        }
        let outputs = processor
            .get("outputs")
            .and_then(serde_json::Value::as_object);
        if let Some(outputs) = outputs {
            let mut already_complete = !outputs.is_empty();
            for (output, expected_hash) in outputs {
                let output = output.trim_matches(['{', '}']);
                let Some(path) = variables.get(output) else {
                    return Err(format!("Неизвестный output Forge processor: {output}."));
                };
                let expected_hash = expected_hash
                    .as_str()
                    .map(|value| replace_processor_variables(value, &variables))
                    .ok_or("Некорректная SHA-1-проверка processor output.")?;
                already_complete &= output_hash_matches(Path::new(path), &expected_hash);
                if let Some(parent) = Path::new(path).parent() {
                    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
            }
            if already_complete {
                continue;
            }
        }
        let main_class = processor_main_class(&processor_jar)?;
        let classpath = classpath
            .iter()
            .map(|path| path.to_string_lossy())
            .collect::<Vec<_>>()
            .join(if cfg!(windows) { ";" } else { ":" });
        let mut processor_command = Command::new(&java);
        hide_console_window(&mut processor_command);
        processor_command
            .arg("-cp")
            .arg(classpath)
            .arg(&main_class)
            .args(&arguments)
            .current_dir(launcher_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = processor_command.spawn().map_err(|error| {
            if let Ok(mut file) = log_file.lock() {
                let _ = writeln!(file, "\n[process] launch_error={error}");
            }
            format!(
                "Не удалось запустить Forge processor: {error}. Лог установки: {}",
                log_path.display()
            )
        })?;
        let process_id = child.id();
        if let Ok(mut file) = log_file.lock() {
            let _ = writeln!(file, "\n[process] pid={process_id} main_class={main_class}");
        }
        let stdout = child
            .stdout
            .take()
            .ok_or("Не удалось прочитать вывод Forge processor.")?;
        let stderr = child
            .stderr
            .take()
            .ok_or("Не удалось прочитать вывод Forge processor.")?;
        let stdout_log = Arc::clone(&log_file);
        let stdout_reader =
            std::thread::spawn(move || read_processor_output(stdout, "stdout", stdout_log));
        let stderr_log = Arc::clone(&log_file);
        let stderr_reader =
            std::thread::spawn(move || read_processor_output(stderr, "stderr", stderr_log));
        let started_at = std::time::Instant::now();
        let status = loop {
            if cancellation.is_some_and(|token| token.load(Ordering::Acquire)) {
                let _ = child.kill();
                let exit_status = child.wait().ok();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                if let Ok(mut file) = log_file.lock() {
                    let _ = writeln!(
                        file,
                        "\n[process] pid={process_id} cancelled=true exit_status={exit_status:?}"
                    );
                }
                return Err("Установка Minecraft отменена.".into());
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(error) => {
                    let _ = child.kill();
                    let exit_status = child.wait().ok();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    if let Ok(mut file) = log_file.lock() {
                        let _ = writeln!(
                            file,
                            "\n[process] pid={process_id} wait_error={error} exit_status={exit_status:?}"
                        );
                    }
                    return Err(format!(
                        "Не удалось проверить Forge processor: {error}. Лог установки: {}",
                        log_path.display()
                    ));
                }
            }
            if started_at.elapsed() >= INSTALLER_PROCESSOR_TIMEOUT {
                let _ = child.kill();
                let exit_status = child.wait().ok();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                if let Ok(mut file) = log_file.lock() {
                    let _ = writeln!(
                        file,
                        "\n[process] pid={process_id} timed_out=true exit_status={exit_status:?}"
                    );
                }
                return Err(format!(
                    "Forge processor превысил лимит времени в 10 минут. Лог установки: {}",
                    log_path.display()
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        let stdout =
            String::from_utf8_lossy(&stdout_reader.join().unwrap_or_default()).into_owned();
        let stderr =
            String::from_utf8_lossy(&stderr_reader.join().unwrap_or_default()).into_owned();
        if let Ok(mut file) = log_file.lock() {
            let _ = writeln!(file, "\n[process] pid={process_id} exit_status={status}");
        }
        if !status.success() {
            return Err(format!(
                "Forge processor завершился с кодом {}: {}\n{}\nЛог установки: {}",
                status,
                stderr.trim(),
                stdout.trim(),
                log_path.display()
            ));
        }
        if let Some(outputs) = outputs {
            for (output, expected_hash) in outputs {
                let output = output.trim_matches(['{', '}']);
                let path = variables
                    .get(output)
                    .ok_or("Неизвестный output Forge processor.")?;
                let expected_hash = expected_hash
                    .as_str()
                    .map(|value| replace_processor_variables(value, &variables))
                    .ok_or("Некорректная SHA-1-проверка processor output.")?;
                if !output_hash_matches(Path::new(path), &expected_hash) {
                    return Err(format!(
                        "SHA-1 output Forge processor {output} не совпал с installer metadata."
                    ));
                }
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn install_loader(
    app: AppHandle,
    folder_name: String,
    minecraft_version: String,
    loader_type: String,
    loader_version: String,
    java_path: String,
) -> Result<(), String> {
    validate_folder_name(&folder_name)?;
    if !["fabric", "forge", "neoforge"].contains(&loader_type.as_str())
        || !valid_version_component(&minecraft_version)
        || !valid_version_component(&loader_version)
    {
        return Err("Некорректный тип или версия загрузчика.".into());
    }
    if loader_type == "neoforge" {
        if !neoforge_is_available(&minecraft_version) {
            return Err("NeoForge недоступен для выбранной версии Minecraft.".into());
        }
        if !neoforge_version_matches_minecraft(&loader_version, &minecraft_version)? {
            return Err("Выбранная версия NeoForge не поддерживает эту версию Minecraft.".into());
        }
    }
    let _lock = crate::locks::InstanceLock::acquire(&folder_name)?;
    let cancellation = instance_install_cancellation(&folder_name)?;
    check_instance_install(cancellation.as_deref())?;
    let canonical_java = crate::java::validate_javaw(&java_path)?;
    let instance = instances_root(&app)?.join(&folder_name);
    let base_version_dir = instance.join("versions").join(&minecraft_version);
    if !installation_complete(&base_version_dir, &minecraft_version) {
        return Err("Сначала установите базовую версию Minecraft.".into());
    }

    let client = official_client()?;
    let launcher_root = instances_root(&app)?
        .parent()
        .ok_or("Не удалось определить MyLauncher.")?
        .to_path_buf();
    let libraries_root = launcher_root.join("libraries");
    fs::create_dir_all(&libraries_root).map_err(|error| error.to_string())?;
    let mut processor_installer_bytes = None;
    let (profile_id, profile, installer_profile) = if loader_type == "fabric" {
        let profile_url = format!(
            "https://meta.fabricmc.net/v2/versions/loader/{minecraft_version}/{loader_version}/profile/json"
        );
        trusted_url(&profile_url)?;
        let profile: serde_json::Value = client
            .get(profile_url)
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?
            .json()
            .await
            .map_err(|error| error.to_string())?;
        let (profile_id, profile) = write_loader_profile(&instance, profile, &minecraft_version)?;
        (profile_id, profile, None)
    } else {
        let forge_version = forge_full_version(&minecraft_version, &loader_version);
        let installer_url = if loader_type == "forge" {
            format!(
                "https://maven.minecraftforge.net/net/minecraftforge/forge/{forge_version}/forge-{forge_version}-installer.jar"
            )
        } else {
            format!(
                "https://maven.neoforged.net/releases/net/neoforged/neoforge/{loader_version}/neoforge-{loader_version}-installer.jar"
            )
        };
        if let Some(installer_bytes) = download_optional_artifact(&client, &installer_url).await? {
            check_instance_install(cancellation.as_deref())?;
            let extracted = extract_loader_archive(
                &installer_bytes,
                &libraries_root,
                &instance,
                &minecraft_version,
            )?;
            processor_installer_bytes = Some(installer_bytes);
            extracted
        } else if loader_type == "forge" && legacy_forge_supported(&minecraft_version) {
            let mut installed = None;
            for extension in ["jar", "zip"] {
                let artifact_url = format!(
                    "https://maven.minecraftforge.net/net/minecraftforge/forge/{forge_version}/forge-{forge_version}-universal.{extension}"
                );
                if let Some(bytes) = download_optional_artifact(&client, &artifact_url).await? {
                    check_instance_install(cancellation.as_deref())?;
                    installed = Some(write_legacy_forge_profile(
                        &instance,
                        &libraries_root,
                        &minecraft_version,
                        &loader_version,
                        &artifact_url,
                        extension,
                        &bytes,
                    )?);
                    break;
                }
            }
            let (profile_id, profile) = installed
                .ok_or("Не найдены installer.jar или universal-архив этой версии Forge.")?;
            (profile_id, profile, None)
        } else {
            return Err(format!(
                "Установщик {loader_type} для версии {loader_version} не найден."
            ));
        }
    };
    check_instance_install(cancellation.as_deref())?;
    let processor_outputs = installer_profile
        .as_ref()
        .map(|installer| processor_output_paths(installer, "client", &libraries_root))
        .transpose()?
        .unwrap_or_default();
    let processor_libraries = installer_profile
        .as_ref()
        .filter(|installer| {
            installer
                .get("processors")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|processors| !processors.is_empty())
        })
        .map(|installer| processor_library_profile(&profile, installer, "client"))
        .transpose()?;
    download_profile_libraries(
        &app,
        &client,
        &profile,
        &libraries_root,
        &processor_outputs,
        cancellation.clone(),
    )
    .await?;
    if let Some(installer_libraries) = installer_profile
        .as_ref()
        .and_then(|installer| installer.get("libraries"))
    {
        let installer_profile = serde_json::json!({"libraries": installer_libraries});
        download_profile_libraries(
            &app,
            &client,
            &installer_profile,
            &libraries_root,
            &processor_outputs,
            cancellation.clone(),
        )
        .await?;
    }
    if let Some(processor_libraries) = processor_libraries {
        download_profile_libraries(
            &app,
            &client,
            &processor_libraries,
            &libraries_root,
            &processor_outputs,
            cancellation.clone(),
        )
        .await?;
    }
    if let Some(installer_profile) = installer_profile.as_ref()
        && installer_profile
            .get("processors")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|processors| !processors.is_empty())
    {
        let installer_bytes = processor_installer_bytes
            .as_deref()
            .ok_or("Installer processors отсутствуют в загруженном архиве.")?;
        let processor_root = instance.join(format!(".loader-processors-{}", uuid::Uuid::new_v4()));
        let processor_result = run_install_processors(
            &canonical_java,
            &folder_name,
            installer_bytes,
            installer_profile,
            &launcher_root,
            &instance,
            &libraries_root,
            &minecraft_version,
            &processor_root,
            cancellation.as_deref(),
        );
        let _ = fs::remove_dir_all(&processor_root);
        processor_result?;
    }

    check_instance_install(cancellation.as_deref())?;
    let metadata_path = instance.join(".launcher_meta.json");
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(&metadata_path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    metadata["loader_type"] = serde_json::Value::String(loader_type);
    metadata["loader_version"] = serde_json::Value::String(loader_version);
    metadata["loader_profile_id"] = serde_json::Value::String(profile_id);
    metadata["loader_installed"] = serde_json::Value::Bool(true);
    let contents = serde_json::to_vec_pretty(&metadata).map_err(|error| error.to_string())?;
    crate::fsutil::write_file_with_backup(&metadata_path, &contents)
}

#[tauri::command]
pub(crate) async fn is_loader_installed(
    app: AppHandle,
    folder_name: String,
) -> Result<bool, String> {
    validate_folder_name(&folder_name)?;
    let instance = instances_root(&app)?.join(folder_name);
    let Ok(contents) = fs::read(instance.join(".launcher_meta.json")) else {
        return Ok(false);
    };
    let Ok(metadata) = serde_json::from_slice::<serde_json::Value>(&contents) else {
        return Ok(false);
    };
    if metadata
        .get("loader_installed")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Ok(false);
    }

    // A marker alone is not enough: the loader profile and the base Minecraft
    // version must still be present, otherwise launch would fail anyway.
    let Some(version) = metadata
        .get("minecraft_version")
        .and_then(serde_json::Value::as_str)
        .filter(|version| valid_version_component(version))
    else {
        return Ok(false);
    };
    if !installation_complete(&instance.join("versions").join(version), version) {
        return Ok(false);
    }
    let Some(profile_id) = metadata
        .get("loader_profile_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
        })
    else {
        return Ok(false);
    };
    let profile_path = instance
        .join("versions")
        .join(profile_id)
        .join(format!("{profile_id}.json"));
    let Ok(profile_contents) = fs::read(profile_path) else {
        return Ok(false);
    };
    let Ok(profile) = serde_json::from_slice::<serde_json::Value>(&profile_contents) else {
        return Ok(false);
    };
    Ok(profile
        .get("inheritsFrom")
        .and_then(serde_json::Value::as_str)
        == Some(version))
}
