use std::{
    collections::{HashMap, HashSet},
    future::Future,
    fs,
    path::{Path, PathBuf},
};

use futures::future::join_all;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha1::{Digest, Sha1};
use tauri::AppHandle;
use tokio::io::AsyncWriteExt;

use crate::{instances_root, validate_folder_name};

const MAX_MODRINTH_JSON_BYTES: usize = 8 * 1024 * 1024;

trait BoundedJsonResponse {
    fn bounded_json<T: DeserializeOwned>(self) -> impl Future<Output = Result<T, String>> + Send;
}

impl BoundedJsonResponse for reqwest::Response {
    fn bounded_json<T: DeserializeOwned>(mut self) -> impl Future<Output = Result<T, String>> + Send {
        async move {
            if self
                .content_length()
                .is_some_and(|length| length > MAX_MODRINTH_JSON_BYTES as u64)
            {
                return Err("Ответ Modrinth превышает допустимый размер.".into());
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = self
                .chunk()
                .await
                .map_err(|error| error.to_string())?
            {
                if bytes.len().saturating_add(chunk.len()) > MAX_MODRINTH_JSON_BYTES {
                    return Err("Ответ Modrinth превышает допустимый размер.".into());
                }
                bytes.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())
        }
    }
}

pub(crate) async fn bounded_json<T: DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, String> {
    response.bounded_json().await
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub(crate) struct ModrinthProject {
    #[serde(alias = "id")]
    project_id: String,
    slug: String,
    title: String,
    description: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    categories: Vec<String>,
    icon_url: Option<String>,
    downloads: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModSearchResults {
    hits: Vec<ModrinthProject>,
    total_hits: usize,
}

fn search_facets(
    project_type: &str,
    minecraft_versions: &[String],
    loaders: &[String],
    category: &str,
    environments: &[&str],
) -> Result<String, String> {
    if !["mod", "plugin", "resourcepack", "shader", "datapack"].contains(&project_type) {
        return Err("Выбран неподдерживаемый тип проекта Modrinth.".into());
    }
    let valid_version = |version: &str| {
        !version.is_empty()
            && version.len() <= 32
            && version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
    };
    if minecraft_versions.len() > 20
        || minecraft_versions
            .iter()
            .any(|version| !valid_version(version))
    {
        return Err("Выбрана некорректная версия Minecraft.".into());
    }
    if loaders.len() > 4
        || loaders
            .iter()
            .any(|loader| !["fabric", "forge", "neoforge", "quilt"].contains(&loader.as_str()))
    {
        return Err("Выбран некорректный загрузчик.".into());
    }
    if !category.is_empty()
        && ![
            "optimization",
            "technology",
            "magic",
            "adventure",
            "decoration",
            "library",
        ]
        .contains(&category)
    {
        return Err("Выбрана некорректная категория мода.".into());
    }
    if environments.len() > 10
        || environments.iter().any(|environment| {
            ![
                "client_and_server",
                "client_only",
                "client_only_server_optional",
                "singleplayer_only",
                "server_only",
                "server_only_client_optional",
                "dedicated_server_only",
                "client_or_server",
                "client_or_server_prefers_both",
                "unknown",
            ]
            .contains(environment)
        })
    {
        return Err("Выбрана некорректная среда выполнения мода.".into());
    }

    if project_type != "mod" && (!loaders.is_empty() || !category.is_empty()) {
        return Err("Загрузчик и категория доступны только для модов.".into());
    }

    let mut facets = vec![vec![format!("project_type:{project_type}")]];
    if !minecraft_versions.is_empty() {
        facets.push(
            minecraft_versions
                .iter()
                .map(|version| format!("versions:{version}"))
                .collect(),
        );
    }
    if !loaders.is_empty() {
        facets.push(
            loaders
                .iter()
                .map(|loader| format!("categories:{loader}"))
                .collect(),
        );
    }
    if !category.is_empty() {
        facets.push(vec![format!("categories:{category}")]);
    }
    if !environments.is_empty() {
        facets.push(
            environments
                .iter()
                .map(|environment| format!("environment:{environment}"))
                .collect(),
        );
    }
    serde_json::to_string(&facets).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        ManagedMod, ModVersion, ModrinthProject, content_base_filename, existing_mod_file_matches,
        latest_server_project_version, managed_dependency_matches, search_facets,
        server_content_loaders,
    };
    use sha1::{Digest, Sha1};
    use std::fs;

    use crate::server::ServerContentInfo;

    #[test]
    fn parses_modrinth_snake_case_search_hit() {
        let project: ModrinthProject = serde_json::from_str(
            r#"{"project_id":"AANobbMI","slug":"sodium","title":"Sodium","description":"Fast renderer","icon_url":null,"downloads":42}"#,
        )
        .unwrap();
        let serialized = serde_json::to_value(project).unwrap();
        assert_eq!(serialized["projectId"], "AANobbMI");
        assert_eq!(serialized["iconUrl"], serde_json::Value::Null);
    }

    #[test]
    fn builds_modrinth_facets_with_or_groups_and_and_filters() {
        let facets = search_facets(
            "mod",
            &["1.20.1".into(), "1.20.2".into()],
            &["fabric".into(), "quilt".into()],
            "optimization",
            &[],
        )
        .unwrap();
        let facets: serde_json::Value = serde_json::from_str(&facets).unwrap();
        assert_eq!(facets[0], serde_json::json!(["project_type:mod"]));
        assert_eq!(
            facets[1],
            serde_json::json!(["versions:1.20.1", "versions:1.20.2"])
        );
        assert_eq!(
            facets[2],
            serde_json::json!(["categories:fabric", "categories:quilt"])
        );
        assert_eq!(facets[3], serde_json::json!(["categories:optimization"]));
    }

    #[test]
    fn server_mod_facets_exclude_client_only_projects() {
        let facets = search_facets(
            "mod",
            &["1.21.1".into()],
            &["fabric".into()],
            "",
            &["server_only", "client_and_server"],
        )
        .unwrap();
        let facets: serde_json::Value = serde_json::from_str(&facets).unwrap();
        assert_eq!(
            facets[3],
            serde_json::json!(["environment:server_only", "environment:client_and_server"])
        );
    }

    #[test]
    fn builds_facets_for_resource_packs_shaders_and_data_packs() {
        for project_type in ["resourcepack", "shader", "datapack"] {
            let facets = search_facets(project_type, &["1.21.1".into()], &[], "", &[]).unwrap();
            let facets: serde_json::Value = serde_json::from_str(&facets).unwrap();
            assert_eq!(
                facets[0],
                serde_json::json!([format!("project_type:{project_type}")])
            );
            assert_eq!(facets[1], serde_json::json!(["versions:1.21.1"]));
        }
    }

    #[test]
    fn server_search_facets_cover_plugins_and_datapacks() {
        for project_type in ["plugin", "datapack"] {
            let facets = search_facets(project_type, &["1.21.1".into()], &[], "", &[]).unwrap();
            let facets: serde_json::Value = serde_json::from_str(&facets).unwrap();
            assert_eq!(
                facets[0],
                serde_json::json!([format!("project_type:{project_type}")])
            );
            assert_eq!(facets[1], serde_json::json!(["versions:1.21.1"]));
        }
    }

    #[test]
    fn server_content_rules_match_core_and_forge_compatibility() {
        let server = |core: &str, minecraft_version: &str| ServerContentInfo {
            root: std::path::PathBuf::new(),
            minecraft_version: minecraft_version.into(),
            core: core.into(),
        };
        assert_eq!(
            server_content_loaders(&server("paper", "1.21.1"), "plugin")
                .unwrap()
                .first()
                .unwrap(),
            "paper"
        );
        assert!(server_content_loaders(&server("vanilla", "1.21.1"), "mod").is_err());
        assert!(server_content_loaders(&server("purpur", "1.21.1"), "mod").is_err());
        assert!(
            server_content_loaders(&server("forge", "1.20.2"), "mod")
                .unwrap()
                .iter()
                .all(|loader| loader != "neoforge")
        );
        assert!(
            server_content_loaders(&server("neoforge", "1.20.1"), "mod")
                .unwrap()
                .contains(&"forge".into())
        );
        assert!(
            !server_content_loaders(&server("neoforge", "1.20.2"), "mod")
                .unwrap()
                .contains(&"forge".into())
        );
    }

    #[test]
    fn handles_disabled_server_content_names_safely() {
        assert_eq!(
            content_base_filename("sample.jar.disabled", ".jar").unwrap(),
            ("sample.jar", false)
        );
        assert_eq!(
            content_base_filename("pack.zip", ".zip").unwrap(),
            ("pack.zip", true)
        );
        assert!(content_base_filename("../escape.jar", ".jar").is_err());
    }

    #[test]
    fn selects_only_versions_matching_server_game_and_loader() {
        let versions: Vec<ModVersion> = serde_json::from_value(serde_json::json!([
            {"id":"forge","project_id":"mod","version_number":"1","game_versions":["1.20.1"],"loaders":["forge"],"dependencies":[],"files":[]},
            {"id":"wrong-game","project_id":"mod","version_number":"2","game_versions":["1.20.2"],"loaders":["forge"],"dependencies":[],"files":[]},
            {"id":"wrong-loader","project_id":"mod","version_number":"3","game_versions":["1.20.1"],"loaders":["fabric"],"dependencies":[],"files":[]}
        ]))
        .unwrap();
        assert_eq!(
            latest_server_project_version(&versions, "1.20.1", &["forge".into()])
                .map(|version| version.id.as_str()),
            Some("forge")
        );
        assert!(latest_server_project_version(&versions, "1.20.2", &["fabric".into()]).is_none());
    }

    #[test]
    fn reuses_installed_project_dependencies_and_respects_pinned_versions() {
        let installed = ManagedMod {
            project_id: "AANobbMI".into(),
            version_id: "installed-version".into(),
            version_number: Some("1.0.0".into()),
            icon_url: None,
            dependencies: Vec::new(),
        };
        assert!(managed_dependency_matches(
            &installed,
            "AANobbMI",
            "installed-version",
            false
        ));
        assert!(!managed_dependency_matches(
            &installed,
            "AANobbMI",
            "pinned-version",
            false
        ));
        assert!(managed_dependency_matches(
            &installed,
            "AANobbMI",
            "newer-version",
            true
        ));
        assert!(!managed_dependency_matches(
            &installed,
            "another-project",
            "installed-version",
            true
        ));
    }

    #[test]
    fn only_reuses_existing_files_with_exact_downloaded_contents() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::current_dir().unwrap().join(format!("my-launcher-modrinth-test-{nonce}"));
        fs::create_dir_all(&directory).unwrap();
        let filename = "sodium-neoforge-0.8.13+mc1.21.1.jar";
        fs::write(directory.join(filename), b"verified contents").unwrap();
        let expected_sha1 = Sha1::digest(b"verified contents")
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let different_sha1 = Sha1::digest(b"different version")
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();

        assert!(existing_mod_file_matches(
            &directory,
            filename,
            &expected_sha1
        ));
        assert!(!existing_mod_file_matches(
            &directory,
            filename,
            &different_sha1
        ));

        fs::remove_dir_all(directory).unwrap();
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstalledMod {
    file_name: String,
    enabled: bool,
    project_id: Option<String>,
    version_id: Option<String>,
    version_number: Option<String>,
    title: Option<String>,
    icon_url: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ServerInstalledContent {
    file_name: String,
    enabled: bool,
    project_id: Option<String>,
    version_id: Option<String>,
    version_number: Option<String>,
    title: Option<String>,
    icon_url: Option<String>,
    update_available: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ServerOptionalDependency {
    project_id: String,
    version_id: String,
    version_number: String,
    title: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModVersionOption {
    version_id: String,
    version_number: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstalledProject {
    file_name: String,
    project_id: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct ManagedMod {
    project_id: String,
    version_id: String,
    #[serde(default)]
    version_number: Option<String>,
    #[serde(default)]
    icon_url: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
}

type ModrinthIndex = HashMap<String, ManagedMod>;

fn managed_dependency_matches(
    managed: &ManagedMod,
    project_id: &str,
    version_id: &str,
    allow_any_version: bool,
) -> bool {
    managed.project_id == project_id && (allow_any_version || managed.version_id == version_id)
}

fn installed_dependency_exists(
    mods: &Path,
    index: &ModrinthIndex,
    project_id: &str,
    version_id: &str,
    allow_any_version: bool,
) -> bool {
    index.iter().any(|(filename, managed)| {
        managed_dependency_matches(managed, project_id, version_id, allow_any_version)
            && safe_mod_filename(filename).is_ok()
            && fs::symlink_metadata(mods.join(filename))
                .is_ok_and(|metadata| metadata.file_type().is_file())
    })
}

fn file_sha1(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = Sha1::new();
    std::io::copy(&mut reader, &mut hasher).ok()?;
    Some(
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

fn existing_mod_file_matches(mods: &Path, filename: &str, expected_sha1: &str) -> bool {
    if safe_mod_filename(filename).is_err() {
        return false;
    }
    let path = mods.join(filename);
    if !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_file()) {
        return false;
    }
    file_sha1(&path).is_some_and(|actual| actual.eq_ignore_ascii_case(expected_sha1))
}

#[derive(Deserialize)]
struct SearchResponse {
    hits: Vec<ModrinthProject>,
    total_hits: usize,
}

#[derive(Deserialize, Clone)]
struct ModVersion {
    id: String,
    project_id: String,
    version_number: String,
    #[serde(default)]
    game_versions: Vec<String>,
    #[serde(default)]
    loaders: Vec<String>,
    dependencies: Vec<ModDependency>,
    files: Vec<ModFile>,
}

#[derive(Deserialize, Clone)]
struct ModDependency {
    version_id: Option<String>,
    project_id: Option<String>,
    dependency_type: Option<String>,
}

#[derive(Deserialize, Clone)]
struct ModFile {
    url: String,
    filename: String,
    primary: bool,
    hashes: ModHashes,
}

#[derive(Deserialize, Clone)]
struct ModHashes {
    sha1: String,
}

fn instance_dir(app: &AppHandle, folder_name: &str) -> Result<PathBuf, String> {
    validate_folder_name(folder_name)?;
    let root = instances_root(app)?;
    let instance = root.join(folder_name);
    if !fs::symlink_metadata(&instance).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        return Err("Инстанс не найден.".into());
    }
    let canonical_instance = instance.canonicalize().map_err(|error| error.to_string())?;
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    if canonical_instance.parent() != Some(canonical_root.as_path()) {
        return Err("Путь инстанса выходит за пределы хранилища.".into());
    }
    Ok(canonical_instance)
}

fn ensure_child_directory(parent: &Path, child: &str) -> Result<PathBuf, String> {
    let directory = parent.join(child);
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if !metadata.file_type().is_dir() => {
            return Err(format!("Папка {child} должна быть обычным каталогом."));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&directory).map_err(|error| error.to_string())?;
        }
        Err(error) => return Err(error.to_string()),
    }
    let canonical_parent = parent.canonicalize().map_err(|error| error.to_string())?;
    let canonical_directory = directory
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if canonical_directory.parent() != Some(canonical_parent.as_path()) {
        return Err(format!("Папка {child} выходит за пределы инстанса."));
    }
    Ok(canonical_directory)
}

fn mods_dir(app: &AppHandle, folder_name: &str) -> Result<PathBuf, String> {
    ensure_child_directory(&instance_dir(app, folder_name)?, "mods")
}

#[tauri::command]
pub(crate) fn list_instance_worlds(
    app: AppHandle,
    folder_name: String,
) -> Result<Vec<String>, String> {
    let instance = instance_dir(&app, &folder_name)?;
    let saves = instance.join("saves");
    match fs::symlink_metadata(&saves) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
        Ok(metadata) if !metadata.file_type().is_dir() => {
            return Err("Папка saves должна быть обычным каталогом.".into());
        }
        Ok(_) => {}
    }

    let mut worlds = fs::read_dir(saves)
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if !file_type.is_dir() {
                return None;
            }
            let name = entry.file_name().into_string().ok()?;
            validate_folder_name(&name).ok()?;
            Some(name)
        })
        .collect::<Vec<_>>();
    worlds.sort_by_key(|world| world.to_lowercase());
    Ok(worlds)
}

fn catalog_target_dir(
    app: &AppHandle,
    folder_name: &str,
    project_type: &str,
    world_name: Option<&str>,
) -> Result<PathBuf, String> {
    let instance = instance_dir(app, folder_name)?;
    match project_type {
        "resourcepack" => ensure_child_directory(&instance, "resourcepacks"),
        "shader" => ensure_child_directory(&instance, "shaderpacks"),
        "datapack" => {
            let world_name = world_name.ok_or("Выберите мир для установки датапака.")?;
            validate_folder_name(world_name)?;
            let saves = ensure_child_directory(&instance, "saves")?;
            let world = ensure_child_directory(&saves, world_name)?;
            ensure_child_directory(&world, "datapacks")
        }
        _ => Err("Этот тип проекта нельзя устанавливать как пакет ресурсов.".into()),
    }
}

fn server_content_loaders(
    info: &crate::server::ServerContentInfo,
    project_type: &str,
) -> Result<Vec<String>, String> {
    match (project_type, info.core.as_str()) {
        ("mod", "fabric" | "forge" | "neoforge") => {
            let mut loaders = vec![info.core.clone()];
            if info.core == "neoforge" && info.minecraft_version == "1.20.1" {
                loaders.push("forge".into());
            }
            Ok(loaders)
        }
        ("plugin", "paper") => Ok(vec!["paper".into(), "spigot".into(), "bukkit".into()]),
        ("plugin", "purpur") => Ok(vec!["purpur".into(), "paper".into(), "spigot".into(), "bukkit".into()]),
        ("datapack", _) => Ok(Vec::new()),
        ("mod", "vanilla") => Err("Ванильный сервер не поддерживает моды. Выберите сервер с Forge, NeoForge или Fabric для установки модов.".into()),
        ("plugin", "fabric" | "forge" | "neoforge") => Err("Это ядро поддерживает моды, но не плагины. Выберите Paper или Purpur для установки плагинов.".into()),
        ("plugin", "vanilla") => Err("Ванильный сервер не поддерживает плагины. Выберите Paper или Purpur.".into()),
        ("mod", "paper" | "purpur") => Err("Сервер на Paper поддерживает только плагины. Для установки модов выберите сервер с Forge, NeoForge или Fabric.".into()),
        _ => Err("Для этого сервера не поддерживается выбранный тип контента.".into()),
    }
}

fn server_content_target(
    app: &AppHandle,
    server_name: &str,
    project_type: &str,
    world_name: Option<&str>,
) -> Result<
    (
        crate::server::ServerContentInfo,
        PathBuf,
        String,
        Vec<String>,
    ),
    String,
> {
    let info = crate::server::server_content_info(app, server_name)?;
    let loaders = server_content_loaders(&info, project_type)?;
    let (directory, extension) = match project_type {
        "mod" => (ensure_child_directory(&info.root, "mods")?, ".jar"),
        "plugin" => (ensure_child_directory(&info.root, "plugins")?, ".jar"),
        "datapack" => {
            let world_name = world_name.ok_or("Выберите мир для установки датапака.")?;
            validate_folder_name(world_name)?;
            let world = info.root.join(world_name);
            let world_metadata = world.join("level.dat");
            if !fs::symlink_metadata(&world).is_ok_and(|metadata| metadata.file_type().is_dir())
                || !fs::symlink_metadata(&world_metadata)
                    .is_ok_and(|metadata| metadata.file_type().is_file())
            {
                return Err("Папка выбранного мира не найдена или не содержит level.dat.".into());
            }
            let canonical_root = info
                .root
                .canonicalize()
                .map_err(|error| error.to_string())?;
            let canonical_world = world.canonicalize().map_err(|error| error.to_string())?;
            if canonical_world.parent() != Some(canonical_root.as_path()) {
                return Err("Путь мира выходит за пределы папки сервера.".into());
            }
            (
                ensure_child_directory(&canonical_world, "datapacks")?,
                ".zip",
            )
        }
        _ => return Err("Выбран неподдерживаемый тип контента сервера.".into()),
    };
    Ok((info, directory, extension.into(), loaders))
}

#[tauri::command]
pub(crate) fn list_server_worlds(
    app: AppHandle,
    server_name: String,
) -> Result<Vec<String>, String> {
    let info = crate::server::server_content_info(&app, &server_name)?;
    let mut worlds = fs::read_dir(&info.root)
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            if !entry.file_type().ok()?.is_dir() || !entry.path().join("level.dat").is_file() {
                return None;
            }
            let name = entry.file_name().into_string().ok()?;
            validate_folder_name(&name).ok()?;
            Some(name)
        })
        .collect::<Vec<_>>();
    worlds.sort_by_key(|world| world.to_lowercase());
    Ok(worlds)
}

#[tauri::command]
pub(crate) async fn search_server_content(
    app: AppHandle,
    server_name: String,
    project_type: String,
    query: String,
    category: String,
    sort: String,
    offset: usize,
) -> Result<ModSearchResults, String> {
    if query.chars().count() > 128
        || !["downloads", "relevance"].contains(&sort.as_str())
        || offset > 1_000_000
    {
        return Err("Некорректные параметры поиска контента сервера.".into());
    }
    let info = crate::server::server_content_info(&app, &server_name)?;
    let loaders = server_content_loaders(&info, &project_type)?;
    let minecraft_versions = vec![info.minecraft_version];
    let search_loaders = if project_type == "mod" {
        loaders
    } else {
        Vec::new()
    };
    let search_category = if project_type == "mod" { &category } else { "" };
    let environments = if project_type == "mod" {
        &["server_only", "client_and_server"][..]
    } else {
        &[]
    };
    let facets = search_facets(
        &project_type,
        &minecraft_versions,
        &search_loaders,
        search_category,
        environments,
    )?;
    let offset = offset.to_string();
    let response: SearchResponse = modrinth_client()?
        .get("https://api.modrinth.com/v2/search")
        .query(&[
            ("query", query.as_str()),
            ("facets", facets.as_str()),
            ("index", sort.as_str()),
            ("limit", "20"),
            ("offset", offset.as_str()),
        ])
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .bounded_json()
        .await
        .map_err(|error| error.to_string())?;
    Ok(ModSearchResults {
        hits: response.hits,
        total_hits: response.total_hits,
    })
}

fn validate_server_project_id(project_id: &str) -> Result<(), String> {
    if project_id.is_empty()
        || project_id.len() > 64
        || !project_id.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err("Некорректный идентификатор проекта Modrinth.".into());
    }
    Ok(())
}

fn latest_server_project_version<'a>(
    versions: &'a [ModVersion],
    minecraft_version: &str,
    loaders: &[String],
) -> Option<&'a ModVersion> {
    versions.iter().find(|version| {
        version
            .game_versions
            .iter()
            .any(|game| game == minecraft_version)
            && (loaders.is_empty()
                || loaders
                    .iter()
                    .any(|loader| version.loaders.contains(loader)))
    })
}

fn compatible_modrinth_version(
    version: &ModVersion,
    minecraft_version: &str,
    loaders: &[String],
) -> bool {
    version
        .game_versions
        .iter()
        .any(|game| game == minecraft_version)
        && (loaders.is_empty()
            || loaders
                .iter()
                .any(|loader| version.loaders.contains(loader)))
}

#[tauri::command]
pub(crate) async fn list_server_optional_dependencies(
    app: AppHandle,
    server_name: String,
    project_id: String,
    project_type: String,
) -> Result<Vec<ServerOptionalDependency>, String> {
    validate_server_project_id(&project_id)?;
    let (info, _, extension, loaders) =
        server_content_target(&app, &server_name, &project_type, None)?;
    if extension != ".jar" {
        return Ok(Vec::new());
    }
    let client = modrinth_client()?;
    let mut request = client
        .get(format!(
            "https://api.modrinth.com/v2/project/{project_id}/version"
        ))
        .query(&[("game_versions", format!("[\"{}\"]", info.minecraft_version))]);
    if !loaders.is_empty() {
        request = request.query(&[(
            "loaders",
            serde_json::to_string(&loaders).map_err(|error| error.to_string())?,
        )]);
    }
    let versions: Vec<ModVersion> = request
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .bounded_json()
        .await
        .map_err(|error| error.to_string())?;
    let root = latest_server_project_version(&versions, &info.minecraft_version, &loaders)
        .ok_or("Для сервера не найдена совместимая версия проекта.")?;
    let mut dependencies = Vec::new();
    for dependency in root
        .dependencies
        .iter()
        .filter(|dependency| dependency.dependency_type.as_deref() == Some("optional"))
    {
        let version = if let Some(version_id) = dependency.version_id.as_deref() {
            let candidate: ModVersion = client
                .get(format!("https://api.modrinth.com/v2/version/{version_id}"))
                .send()
                .await
                .map_err(|error| error.to_string())?
                .error_for_status()
                .map_err(|error| error.to_string())?
                .bounded_json()
                .await
                .map_err(|error| error.to_string())?;
            compatible_modrinth_version(&candidate, &info.minecraft_version, &loaders)
                .then_some(candidate)
        } else if let Some(optional_project) = dependency.project_id.as_deref() {
            let mut request = client
                .get(format!(
                    "https://api.modrinth.com/v2/project/{optional_project}/version"
                ))
                .query(&[("game_versions", format!("[\"{}\"]", info.minecraft_version))]);
            if !loaders.is_empty() {
                request = request.query(&[(
                    "loaders",
                    serde_json::to_string(&loaders).map_err(|error| error.to_string())?,
                )]);
            }
            let candidates: Vec<ModVersion> = request
                .send()
                .await
                .map_err(|error| error.to_string())?
                .error_for_status()
                .map_err(|error| error.to_string())?
                .bounded_json()
                .await
                .map_err(|error| error.to_string())?;
            latest_server_project_version(&candidates, &info.minecraft_version, &loaders).cloned()
        } else {
            None
        };
        if let Some(version) = version {
            let title_response = client
                .get(format!(
                    "https://api.modrinth.com/v2/project/{}",
                    version.project_id
                ))
                .send()
                .await;
            let title = if let Ok(response) = title_response {
                if let Ok(response) = response.error_for_status() {
                    response
                        .json::<serde_json::Value>()
                        .await
                        .ok()
                        .and_then(|project| {
                            project
                                .get("title")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .unwrap_or_else(|| version.project_id.clone())
                } else {
                    version.project_id.clone()
                }
            } else {
                version.project_id.clone()
            };
            dependencies.push(ServerOptionalDependency {
                project_id: version.project_id,
                version_id: version.id,
                version_number: version.version_number,
                title,
            });
        }
    }
    dependencies.truncate(16);
    Ok(dependencies)
}

#[tauri::command]
pub(crate) async fn install_server_content(
    app: AppHandle,
    server_name: String,
    project_id: String,
    project_type: String,
    world_name: Option<String>,
    optional_version_ids: Option<Vec<String>>,
) -> Result<InstalledMod, String> {
    let _mutation = crate::server::ServerContentMutationReservation::acquire(&server_name)?;
    validate_server_project_id(&project_id)?;
    let (info, target, extension, loaders) =
        server_content_target(&app, &server_name, &project_type, world_name.as_deref())?;
    let index = read_modrinth_index(&target)?;
    if index
        .values()
        .any(|managed| managed.project_id == project_id)
    {
        return Err(
            "Этот проект уже установлен. Откройте список контента для обновления или удаления."
                .into(),
        );
    }
    if project_type == "mod" || project_type == "plugin" {
        if extension != ".jar" {
            return Err("Для модов и плагинов требуется JAR-файл.".into());
        }
        return install_mod_at(
            target,
            project_id,
            info.minecraft_version,
            loaders,
            optional_version_ids.unwrap_or_default(),
            None,
        )
        .await;
    }
    if project_type != "datapack" || extension != ".zip" {
        return Err("Неподдерживаемый тип контента сервера.".into());
    }
    let client = modrinth_client()?;
    let versions: Vec<ModVersion> = client
        .get(format!(
            "https://api.modrinth.com/v2/project/{project_id}/version"
        ))
        .query(&[("game_versions", format!("[\"{}\"]", info.minecraft_version))])
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .bounded_json()
        .await
        .map_err(|error| error.to_string())?;
    let version = latest_server_project_version(&versions, &info.minecraft_version, &[])
        .ok_or("Для версии игры сервера не найдена совместимая версия датапака.")?;
    let download = download_version_file_with_extension(&client, version, &target, ".zip").await?;
    let destination = target.join(&download.filename);
    if destination.exists() {
        let _ = fs::remove_file(&download.temporary);
        return Err(format!("Файл {} уже существует.", download.filename));
    }
    fs::rename(&download.temporary, &destination).map_err(|error| {
        let _ = fs::remove_file(&download.temporary);
        error.to_string()
    })?;
    let mut index = read_modrinth_index(&target)?;
    index.insert(
        download.filename.clone(),
        ManagedMod {
            project_id: version.project_id.clone(),
            version_id: version.id.clone(),
            version_number: Some(version.version_number.clone()),
            icon_url: None,
            dependencies: Vec::new(),
        },
    );
    if let Err(error) = write_modrinth_index(&target, &index) {
        let _ = fs::remove_file(destination);
        return Err(error);
    }
    Ok(InstalledMod {
        file_name: download.filename,
        enabled: true,
        project_id: Some(version.project_id.clone()),
        version_id: Some(version.id.clone()),
        version_number: Some(version.version_number.clone()),
        title: None,
        icon_url: None,
    })
}

fn content_base_filename<'a>(
    file_name: &'a str,
    extension: &str,
) -> Result<(&'a str, bool), String> {
    if file_name.contains(['/', '\\', ':']) || file_name.starts_with('.') {
        return Err("Некорректное имя файла контента.".into());
    }
    let enabled = !file_name.to_ascii_lowercase().ends_with(".disabled");
    let base = if enabled {
        file_name
    } else {
        file_name
            .strip_suffix(".disabled")
            .ok_or("Некорректное расширение файла.")?
    };
    safe_project_filename(base, extension)?;
    Ok((base, enabled))
}

#[tauri::command]
pub(crate) async fn list_server_content(
    app: AppHandle,
    server_name: String,
    project_type: String,
    world_name: Option<String>,
) -> Result<Vec<ServerInstalledContent>, String> {
    let (info, target, extension, loaders) =
        server_content_target(&app, &server_name, &project_type, world_name.as_deref())?;
    let index = read_modrinth_index(&target)?;
    let mut projects = Vec::new();
    let mut seen_projects = HashSet::new();
    for managed in index.values() {
        if seen_projects.insert(managed.project_id.clone()) && projects.len() < 100 {
            projects.push((managed.project_id.clone(), managed.version_id.clone()));
        }
    }
    let updates = if let Ok(client) = modrinth_client() {
        join_all(projects.into_iter().map(|(project_id, version_id)| {
            let client = client.clone();
            let loaders = loaders.clone();
            let minecraft_version = info.minecraft_version.clone();
            async move {
                let mut request = client
                    .get(format!(
                        "https://api.modrinth.com/v2/project/{project_id}/version"
                    ))
                    .query(&[("game_versions", format!("[\"{minecraft_version}\"]"))]);
                if !loaders.is_empty() {
                    request = request.query(&[(
                        "loaders",
                        serde_json::to_string(&loaders).unwrap_or_default(),
                    )]);
                }
                let versions = match request.send().await {
                    Ok(response) => match response.error_for_status() {
                        Ok(response) => response.bounded_json::<Vec<ModVersion>>().await.ok(),
                        Err(_) => None,
                    },
                    Err(_) => None,
                };
                let available = versions
                    .and_then(|versions| {
                        latest_server_project_version(&versions, &minecraft_version, &loaders)
                            .map(|version| version.id != version_id)
                    })
                    .unwrap_or(false);
                (project_id, available)
            }
        }))
        .await
        .into_iter()
        .collect::<HashMap<_, _>>()
    } else {
        HashMap::new()
    };
    let mut installed = Vec::new();
    for entry in fs::read_dir(&target).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let Ok((base_name, enabled)) = content_base_filename(&file_name, &extension) else {
            continue;
        };
        let managed = index.get(base_name);
        let project_id = managed.map(|item| item.project_id.clone());
        installed.push(ServerInstalledContent {
            file_name,
            enabled,
            update_available: project_id
                .as_ref()
                .and_then(|project| updates.get(project))
                .copied()
                .unwrap_or(false),
            project_id,
            version_id: managed.map(|item| item.version_id.clone()),
            version_number: managed.and_then(|item| item.version_number.clone()),
            title: None,
            icon_url: managed.and_then(|item| item.icon_url.clone()),
        });
    }
    installed.sort_by(|left, right| {
        left.file_name
            .to_lowercase()
            .cmp(&right.file_name.to_lowercase())
    });
    Ok(installed)
}

#[tauri::command]
pub(crate) fn toggle_server_content(
    app: AppHandle,
    server_name: String,
    project_type: String,
    world_name: Option<String>,
    file_name: String,
    enabled: bool,
) -> Result<InstalledMod, String> {
    let _mutation = crate::server::ServerContentMutationReservation::acquire(&server_name)?;
    let (_, target, extension, _) =
        server_content_target(&app, &server_name, &project_type, world_name.as_deref())?;
    let (base_name, current_enabled) = content_base_filename(&file_name, &extension)?;
    if current_enabled == enabled {
        return Err(if enabled {
            "Контент уже включён."
        } else {
            "Контент уже выключен."
        }
        .into());
    }
    let current = target.join(&file_name);
    if !fs::symlink_metadata(&current).is_ok_and(|metadata| metadata.file_type().is_file()) {
        return Err("Файл контента не найден.".into());
    }
    let next_name = if enabled {
        base_name.to_string()
    } else {
        format!("{base_name}.disabled")
    };
    let destination = target.join(&next_name);
    if destination.exists() {
        return Err("Файл с таким именем уже существует.".into());
    }
    fs::rename(current, destination).map_err(|error| error.to_string())?;
    let managed = read_modrinth_index(&target)?.get(base_name).cloned();
    Ok(InstalledMod {
        file_name: next_name,
        enabled,
        project_id: managed.as_ref().map(|item| item.project_id.clone()),
        version_id: managed.as_ref().map(|item| item.version_id.clone()),
        version_number: managed
            .as_ref()
            .and_then(|item| item.version_number.clone()),
        title: None,
        icon_url: managed.and_then(|item| item.icon_url),
    })
}

#[tauri::command]
pub(crate) fn remove_server_content(
    app: AppHandle,
    server_name: String,
    project_type: String,
    world_name: Option<String>,
    file_name: String,
) -> Result<(), String> {
    let _mutation = crate::server::ServerContentMutationReservation::acquire(&server_name)?;
    let (_, target, extension, _) =
        server_content_target(&app, &server_name, &project_type, world_name.as_deref())?;
    let (base_name, _) = content_base_filename(&file_name, &extension)?;
    let path = target.join(&file_name);
    if !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_file()) {
        return Err("Файл контента не найден.".into());
    }
    let mut index = read_modrinth_index(&target)?;
    if let Some(project_id) = index
        .get(base_name)
        .map(|managed| managed.project_id.clone())
    {
        let dependents = index
            .iter()
            .filter(|(filename, managed)| {
                filename.as_str() != base_name && managed.dependencies.contains(&project_id)
            })
            .map(|(filename, _)| filename.clone())
            .collect::<Vec<_>>();
        if !dependents.is_empty() {
            return Err(format!(
                "Нельзя удалить этот проект: от него зависят {}.",
                dependents.join(", ")
            ));
        }
    }
    fs::remove_file(path).map_err(|error| error.to_string())?;
    index.remove(base_name);
    write_modrinth_index(&target, &index)
}

#[tauri::command]
pub(crate) async fn update_server_content(
    app: AppHandle,
    server_name: String,
    project_type: String,
    world_name: Option<String>,
    file_name: String,
) -> Result<InstalledMod, String> {
    let _mutation = crate::server::ServerContentMutationReservation::acquire(&server_name)?;
    let (info, target, extension, loaders) =
        server_content_target(&app, &server_name, &project_type, world_name.as_deref())?;
    let (base_name, current_enabled) = content_base_filename(&file_name, &extension)?;
    let mut index = read_modrinth_index(&target)?;
    let managed = index
        .get(base_name)
        .cloned()
        .ok_or("Этот файл не установлен через Modrinth и не может обновляться автоматически.")?;
    let mut request = modrinth_client()?
        .get(format!(
            "https://api.modrinth.com/v2/project/{}/version",
            managed.project_id
        ))
        .query(&[("game_versions", format!("[\"{}\"]", info.minecraft_version))]);
    if !loaders.is_empty() {
        request = request.query(&[(
            "loaders",
            serde_json::to_string(&loaders).map_err(|error| error.to_string())?,
        )]);
    }
    let versions: Vec<ModVersion> = request
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .bounded_json()
        .await
        .map_err(|error| error.to_string())?;
    let version = latest_server_project_version(&versions, &info.minecraft_version, &loaders)
        .ok_or("Совместимая версия контента не найдена.")?;
    if version.id == managed.version_id {
        return Err("Установлена последняя совместимая версия.".into());
    }
    if project_type == "mod" || project_type == "plugin" {
        for dependency in version
            .dependencies
            .iter()
            .filter(|dependency| dependency.dependency_type.as_deref() == Some("required"))
        {
            let pinned_version = if let Some(version_id) = dependency.version_id.as_deref() {
                let pinned: ModVersion = modrinth_client()?
                    .get(format!("https://api.modrinth.com/v2/version/{version_id}"))
                    .send()
                    .await
                    .map_err(|error| error.to_string())?
                    .error_for_status()
                    .map_err(|error| error.to_string())?
                    .bounded_json()
                    .await
                    .map_err(|error| error.to_string())?;
                if !compatible_modrinth_version(&pinned, &info.minecraft_version, &loaders) {
                    return Err(format!(
                        "Обязательная зависимость {} несовместима с сервером.",
                        pinned.project_id
                    ));
                }
                Some(pinned)
            } else {
                None
            };
            let dependency_project = dependency.project_id.clone().or_else(|| {
                pinned_version
                    .as_ref()
                    .map(|version| version.project_id.clone())
            });
            let Some(dependency_project) = dependency_project else {
                continue;
            };
            let current = index
                .iter()
                .find(|(_, item)| item.project_id == dependency_project)
                .map(|(_, item)| item.version_id.clone());
            if let Some(current_version) = current {
                if pinned_version
                    .as_ref()
                    .is_some_and(|pinned| pinned.id != current_version)
                {
                    return Err(format!(
                        "Для обновления требуется версия {} зависимости {}. Обновите её отдельно и повторите.",
                        pinned_version
                            .as_ref()
                            .map(|item| item.version_number.as_str())
                            .unwrap_or_default(),
                        dependency_project
                    ));
                }
                continue;
            }
            install_mod_at(
                target.clone(),
                dependency_project,
                info.minecraft_version.clone(),
                loaders.clone(),
                Vec::new(),
                pinned_version.map(|version| version.id),
            )
            .await?;
            index = read_modrinth_index(&target)?;
        }
    }
    let download =
        download_version_file_with_extension(&modrinth_client()?, version, &target, &extension)
            .await?;
    let _cleanup_download = TemporaryDownloadGuard(vec![download.temporary.clone()]);
    let new_name = if current_enabled {
        download.filename.clone()
    } else {
        format!("{}.disabled", download.filename)
    };
    let destination = target.join(&new_name);
    let current_path = target.join(&file_name);
    if destination != current_path && destination.exists() {
        return Err(format!("Файл {new_name} уже существует."));
    }
    let backup = target.join(format!("{new_name}.update-backup"));
    if backup.exists() {
        return Err("Найдена резервная копия обновления контента.".into());
    }
    fs::rename(&current_path, &backup).map_err(|error| error.to_string())?;
    if let Err(error) = fs::rename(&download.temporary, &destination) {
        let _ = fs::rename(&backup, &current_path);
        return Err(error.to_string());
    }
    let dependencies = version
        .dependencies
        .iter()
        .filter(|dependency| dependency.dependency_type.as_deref() == Some("required"))
        .filter_map(|dependency| dependency.project_id.clone())
        .collect();
    index.remove(base_name);
    index.insert(
        download.filename,
        ManagedMod {
            project_id: version.project_id.clone(),
            version_id: version.id.clone(),
            version_number: Some(version.version_number.clone()),
            icon_url: managed.icon_url.clone(),
            dependencies,
        },
    );
    if let Err(error) = write_modrinth_index(&target, &index) {
        let _ = fs::remove_file(&destination);
        let _ = fs::rename(&backup, &current_path);
        return Err(error);
    }
    fs::remove_file(backup).map_err(|error| error.to_string())?;
    Ok(InstalledMod {
        file_name: new_name,
        enabled: current_enabled,
        project_id: Some(version.project_id.clone()),
        version_id: Some(version.id.clone()),
        version_number: Some(version.version_number.clone()),
        title: None,
        icon_url: managed.icon_url,
    })
}

pub(crate) fn modrinth_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("MyLauncher/0.1.0 (local desktop launcher)")
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(600))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.url().scheme() == "https"
                && attempt
                    .url()
                    .host_str()
                    .is_some_and(crate::minecraft::is_trusted_host)
            {
                attempt.follow()
            } else {
                attempt.error("Redirect blocked: destination is not allowlisted")
            }
        }))
        .build()
        .map_err(|error| error.to_string())
}

fn safe_project_filename(filename: &str, extension: &str) -> Result<(), String> {
    if filename.is_empty()
        || filename.len() > 240
        || filename
            != std::path::Path::new(filename)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
        || !filename
            .to_ascii_lowercase()
            .ends_with(&extension.to_ascii_lowercase())
        || filename.chars().any(char::is_control)
    {
        return Err(format!(
            "Modrinth вернул некорректное имя файла {extension}."
        ));
    }
    Ok(())
}

fn safe_mod_filename(filename: &str) -> Result<(), String> {
    safe_project_filename(filename, ".jar")
}

fn modrinth_index_path(mods: &Path) -> PathBuf {
    mods.join(".modrinth-index.json")
}

fn read_modrinth_index(mods: &Path) -> Result<ModrinthIndex, String> {
    let path = modrinth_index_path(mods);
    if !path.exists() {
        return Ok(HashMap::new());
    }
    if !fs::symlink_metadata(&path)
        .map_err(|error| error.to_string())?
        .file_type()
        .is_file()
    {
        return Err("Индекс Modrinth должен быть обычным файлом.".into());
    }
    serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
        .map_err(|error| format!("Не удалось прочитать индекс Modrinth: {error}"))
}

fn write_modrinth_index(mods: &Path, index: &ModrinthIndex) -> Result<(), String> {
    let path = modrinth_index_path(mods);
    let contents = serde_json::to_vec_pretty(index).map_err(|error| error.to_string())?;
    crate::fsutil::write_file_with_backup(&path, &contents)
}

fn normalized_mod_filename(file_name: &str) -> Option<&str> {
    file_name
        .strip_suffix(".disabled")
        .or(Some(file_name))
        .filter(|name| name.to_ascii_lowercase().ends_with(".jar"))
}

struct DownloadedFile {
    filename: String,
    temporary: PathBuf,
    size: u64,
}

struct StagedMod {
    download: DownloadedFile,
    project_id: String,
    version_id: String,
    version_number: String,
    dependencies: Vec<String>,
}

struct TemporaryDownloadGuard(Vec<PathBuf>);

impl Drop for TemporaryDownloadGuard {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

fn primary_version_file<'a>(
    version: &'a ModVersion,
    extension: &str,
) -> Result<&'a ModFile, String> {
    let file = version
        .files
        .iter()
        .find(|file| file.primary)
        .or_else(|| version.files.first())
        .ok_or("Modrinth не вернул файл проекта.")?;
    safe_project_filename(&file.filename, extension)?;
    Ok(file)
}

async fn stream_version_download(
    client: &reqwest::Client,
    url: &str,
    destination: &Path,
    max_bytes: u64,
) -> Result<(u64, String), String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes)
    {
        return Err("Файл Modrinth превышает допустимый размер.".into());
    }
    // Remove a stale file or symlink before creating the download target.
    let _ = fs::remove_file(destination);
    let mut file = tokio::fs::File::create(destination)
        .await
        .map_err(|error| format!("Не удалось создать файл загрузки: {error}"))?;
    let mut hasher = Sha1::new();
    let mut total = 0_u64;
    let mut download_result = loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                total = total.saturating_add(chunk.len() as u64);
                if total > max_bytes {
                    break Err("Файл Modrinth превышает допустимый размер.".to_string());
                }
                hasher.update(&chunk);
                if let Err(error) = file.write_all(&chunk).await {
                    break Err(format!("Не удалось записать файл Modrinth: {error}"));
                }
            }
            Ok(None) => break Ok(()),
            Err(error) => break Err(format!("Соединение оборвалось при загрузке файла: {error}")),
        }
    };
    if download_result.is_ok()
        && let Err(error) = file.flush().await
    {
        download_result = Err(format!("Не удалось сохранить файл Modrinth: {error}"));
    }
    drop(file);
    if let Err(error) = download_result {
        let _ = fs::remove_file(destination);
        return Err(error);
    }
    let sha1 = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok((total, sha1))
}

async fn download_version_file(
    client: &reqwest::Client,
    version: &ModVersion,
    destination_dir: &Path,
) -> Result<DownloadedFile, String> {
    download_version_file_with_extension(client, version, destination_dir, ".jar").await
}

async fn download_version_file_with_extension(
    client: &reqwest::Client,
    version: &ModVersion,
    destination_dir: &Path,
    extension: &str,
) -> Result<DownloadedFile, String> {
    let file = primary_version_file(version, extension)?;
    let url = reqwest::Url::parse(&file.url).map_err(|error| error.to_string())?;
    if url.scheme() != "https"
        || !url
            .host_str()
            .is_some_and(crate::minecraft::is_trusted_host)
    {
        return Err("Ссылка загрузки не принадлежит CDN Modrinth.".into());
    }
    let temporary = destination_dir.join(format!("{}.download-tmp", file.filename));
    let (size, sha1) = stream_version_download(client, &file.url, &temporary, 512_000_000).await?;
    if !sha1.eq_ignore_ascii_case(&file.hashes.sha1) {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "SHA-1 файла {} не совпал с Modrinth.",
            file.filename
        ));
    }
    Ok(DownloadedFile {
        filename: file.filename.clone(),
        temporary,
        size,
    })
}

#[tauri::command]
pub(crate) async fn search_mods(
    query: String,
    project_type: String,
    minecraft_versions: Vec<String>,
    loaders: Vec<String>,
    category: String,
    index: String,
    offset: usize,
) -> Result<ModSearchResults, String> {
    if query.chars().count() > 128 {
        return Err("Поисковый запрос слишком длинный.".into());
    }
    if !["downloads", "relevance"].contains(&index.as_str()) {
        return Err("Выбрана неподдерживаемая сортировка каталога.".into());
    }
    if offset > 1_000_000 {
        return Err("Слишком большой номер страницы каталога.".into());
    }
    let facets = search_facets(&project_type, &minecraft_versions, &loaders, &category, &[])?;
    let offset = offset.to_string();
    let response: SearchResponse = modrinth_client()?
        .get("https://api.modrinth.com/v2/search")
        .query(&[
            ("query", query.as_str()),
            ("facets", facets.as_str()),
            ("index", index.as_str()),
            ("limit", "20"),
            ("offset", offset.as_str()),
        ])
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .bounded_json()
        .await
        .map_err(|error| error.to_string())?;
    Ok(ModSearchResults {
        hits: response.hits,
        total_hits: response.total_hits,
    })
}

#[tauri::command]
pub(crate) fn list_installed_catalog_projects(
    app: AppHandle,
    folder_name: String,
    project_type: String,
    world_name: Option<String>,
) -> Result<Vec<String>, String> {
    let target = catalog_target_dir(&app, &folder_name, &project_type, world_name.as_deref())?;
    let index = read_modrinth_index(&target)?;
    let mut projects = index
        .into_iter()
        .filter_map(|(filename, managed)| {
            (safe_project_filename(&filename, ".zip").is_ok()
                && fs::symlink_metadata(target.join(filename))
                    .is_ok_and(|metadata| metadata.file_type().is_file()))
            .then_some(managed.project_id)
        })
        .collect::<Vec<_>>();
    projects.sort();
    projects.dedup();
    Ok(projects)
}

#[tauri::command]
pub(crate) async fn install_catalog_project(
    app: AppHandle,
    folder_name: String,
    project_id: String,
    minecraft_version: String,
    project_type: String,
    world_name: Option<String>,
) -> Result<InstalledProject, String> {
    let _instance_lock = crate::locks::InstanceLock::acquire(&folder_name)?;
    if project_id.is_empty()
        || project_id.len() > 64
        || !project_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Err("Некорректный идентификатор проекта Modrinth.".into());
    }
    if minecraft_version.is_empty()
        || minecraft_version.len() > 32
        || !minecraft_version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
    {
        return Err("Некорректная версия Minecraft.".into());
    }
    let target = catalog_target_dir(&app, &folder_name, &project_type, world_name.as_deref())?;
    let client = modrinth_client()?;
    let versions: Vec<ModVersion> = client
        .get(format!(
            "https://api.modrinth.com/v2/project/{project_id}/version"
        ))
        .query(&[("game_versions", format!("[\"{minecraft_version}\"]"))])
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .bounded_json()
        .await
        .map_err(|error| error.to_string())?;
    let version = versions
        .into_iter()
        .next()
        .ok_or("Для этой версии игры пакет не найден.")?;
    let download = download_version_file_with_extension(&client, &version, &target, ".zip").await?;
    let destination = target.join(&download.filename);
    if destination.exists() {
        let _ = fs::remove_file(&download.temporary);
        return Err(format!("Файл пакета {} уже существует.", download.filename));
    }

    if let Err(error) = fs::rename(&download.temporary, &destination) {
        let _ = fs::remove_file(&download.temporary);
        return Err(error.to_string());
    }
    let filename = download.filename;
    let mut index = read_modrinth_index(&target)?;
    index.insert(
        filename.clone(),
        ManagedMod {
            project_id: version.project_id.clone(),
            version_id: version.id.clone(),
            version_number: Some(version.version_number.clone()),
            icon_url: None,
            dependencies: Vec::new(),
        },
    );
    if let Err(error) = write_modrinth_index(&target, &index) {
        let _ = fs::remove_file(&destination);
        return Err(error);
    }
    Ok(InstalledProject {
        file_name: filename,
        project_id: version.project_id,
    })
}

#[tauri::command]
pub(crate) async fn list_installed_mods(
    app: AppHandle,
    folder_name: String,
) -> Result<Vec<InstalledMod>, String> {
    validate_folder_name(&folder_name)?;
    let _instance_lock = crate::locks::InstanceLock::acquire(&folder_name)?;
    let mods = mods_dir(&app, &folder_name)?;
    let mut index = read_modrinth_index(&mods)?;
    let mut project_ids = index
        .values()
        .filter(|managed| managed.icon_url.is_none())
        .map(|managed| managed.project_id.clone())
        .collect::<Vec<_>>();
    project_ids.sort();
    project_ids.dedup();
    let mut project_details = HashMap::new();
    if let Ok(client) = modrinth_client() {
        for project_ids in project_ids.chunks(100) {
            let Ok(ids) = serde_json::to_string(project_ids) else {
                continue;
            };
            let Ok(response) = client
                .get("https://api.modrinth.com/v2/projects")
                .query(&[("ids", ids)])
                .timeout(std::time::Duration::from_secs(8))
                .send()
                .await
            else {
                continue;
            };
            if let Ok(projects) = response.bounded_json::<Vec<ModrinthProject>>().await {
                project_details.extend(
                    projects
                        .into_iter()
                        .map(|project| (project.project_id.clone(), project)),
                );
            }
        }
    }
    let mut index_changed = false;
    for managed in index.values_mut() {
        if managed.icon_url.is_none()
            && let Some(icon_url) = project_details
                .get(&managed.project_id)
                .and_then(|project| project.icon_url.clone())
        {
            managed.icon_url = Some(icon_url);
            index_changed = true;
        }
    }
    if index_changed {
        write_modrinth_index(&mods, &index)?;
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(mods).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if !file_type.is_file() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let enabled = file_name.to_ascii_lowercase().ends_with(".jar");
        if enabled || file_name.to_ascii_lowercase().ends_with(".jar.disabled") {
            let managed = normalized_mod_filename(&file_name).and_then(|name| index.get(name));
            let project = managed.and_then(|item| project_details.get(&item.project_id));
            files.push(InstalledMod {
                version_id: managed.map(|item| item.version_id.clone()),
                version_number: managed.and_then(|item| item.version_number.clone()),
                title: project.map(|item| item.title.clone()),
                icon_url: managed
                    .and_then(|item| item.icon_url.clone())
                    .or_else(|| project.and_then(|item| item.icon_url.clone())),
                file_name,
                enabled,
                project_id: managed.map(|item| item.project_id.clone()),
            });
        }
    }
    files.sort_by(|left, right| {
        left.file_name
            .to_lowercase()
            .cmp(&right.file_name.to_lowercase())
    });
    Ok(files)
}

#[tauri::command]
pub(crate) async fn list_mod_versions(
    project_id: String,
    minecraft_version: String,
    loader: String,
) -> Result<Vec<ModVersionOption>, String> {
    if project_id.is_empty()
        || project_id.len() > 64
        || !project_id.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err("Некорректный идентификатор проекта Modrinth.".into());
    }
    if minecraft_version.is_empty()
        || minecraft_version.len() > 32
        || !minecraft_version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
        || !["fabric", "forge", "neoforge"].contains(&loader.as_str())
    {
        return Err("Для выбора версии нужны поддерживаемые игра и загрузчик.".into());
    }
    let versions: Vec<ModVersion> = modrinth_client()?
        .get(format!(
            "https://api.modrinth.com/v2/project/{project_id}/version"
        ))
        .query(&[
            ("game_versions", format!("[\"{minecraft_version}\"]")),
            ("loaders", format!("[\"{loader}\"]")),
        ])
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .bounded_json()
        .await
        .map_err(|error| error.to_string())?;
    Ok(versions
        .into_iter()
        .map(|version| ModVersionOption {
            version_id: version.id,
            version_number: version.version_number,
        })
        .collect())
}

async fn install_mod_at(
    mods: PathBuf,
    project_id: String,
    minecraft_version: String,
    loaders: Vec<String>,
    optional_version_ids: Vec<String>,
    pinned_version_id: Option<String>,
) -> Result<InstalledMod, String> {
    if project_id.is_empty()
        || project_id.len() > 64
        || !project_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Err("Некорректный идентификатор проекта Modrinth.".into());
    }
    let client = modrinth_client()?;
    let installed_index = read_modrinth_index(&mods)?;
    if installed_index
        .values()
        .any(|managed| managed.project_id == project_id)
    {
        return Err("Этот проект уже установлен в выбранной папке.".into());
    }
    let versions: Vec<ModVersion> = client
        .get(format!(
            "https://api.modrinth.com/v2/project/{project_id}/version"
        ))
        .query(&[
            ("game_versions", format!("[\"{minecraft_version}\"]")),
            (
                "loaders",
                serde_json::to_string(&loaders).map_err(|error| error.to_string())?,
            ),
        ])
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .bounded_json()
        .await
        .map_err(|error| error.to_string())?;
    if optional_version_ids.len() > 16 {
        return Err("Можно выбрать не более 16 опциональных зависимостей.".into());
    }
    let root_version = if let Some(version_id) = pinned_version_id {
        versions
            .into_iter()
            .find(|version| version.id == version_id)
            .ok_or("Обязательная версия зависимости несовместима с сервером.")?
    } else {
        versions
            .into_iter()
            .next()
            .ok_or("Для этой версии игры мод не найден.")?
    };
    if let Some(incompatible_project) = root_version
        .dependencies
        .iter()
        .filter(|dependency| dependency.dependency_type.as_deref() == Some("incompatible"))
        .filter_map(|dependency| dependency.project_id.as_deref())
        .find(|project| {
            installed_index
                .values()
                .any(|managed| managed.project_id == *project)
        })
    {
        return Err(format!(
            "Установленный проект {incompatible_project} несовместим с выбранным контентом."
        ));
    }
    let mut optional_versions = Vec::new();
    let mut seen_optional = HashSet::new();
    for version_id in optional_version_ids {
        if version_id.is_empty()
            || version_id.len() > 64
            || !version_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
            || !seen_optional.insert(version_id.clone())
        {
            return Err("Некорректная optional dependency version id.".into());
        }
        let version: ModVersion = client
            .get(format!("https://api.modrinth.com/v2/version/{version_id}"))
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?
            .bounded_json()
            .await
            .map_err(|error| error.to_string())?;
        if !compatible_modrinth_version(&version, &minecraft_version, &loaders) {
            return Err("Выбранная опциональная зависимость несовместима с сервером.".into());
        }
        if !root_version.dependencies.iter().any(|dependency| {
            dependency.dependency_type.as_deref() == Some("optional")
                && (dependency.version_id.as_deref() == Some(&version.id)
                    || dependency.project_id.as_deref() == Some(&version.project_id))
        }) {
            return Err("Выбранная версия не является optional dependency этого проекта.".into());
        }
        optional_versions.push(version);
    }
    let mut pending = optional_versions
        .into_iter()
        .map(|version| (version, None))
        .collect::<Vec<_>>();
    pending.push((root_version, None));
    let mut seen_versions = HashSet::new();
    let mut staged_files: Vec<StagedMod> = Vec::new();
    let mut total_size = 0_u64;
    let mut pending_downloads = TemporaryDownloadGuard(Vec::new());

    while let Some((version, allow_any_installed_version)) = pending.pop() {
        if !seen_versions.insert(version.id.clone()) {
            continue;
        }
        if !compatible_modrinth_version(&version, &minecraft_version, &loaders) {
            return Err(format!(
                "Зависимость {} несовместима с версией игры или загрузчиком сервера.",
                version.project_id
            ));
        }
        if allow_any_installed_version.is_some_and(|allow_any_version| {
            installed_dependency_exists(
                &mods,
                &installed_index,
                &version.project_id,
                &version.id,
                allow_any_version,
            )
        }) {
            continue;
        }
        if seen_versions.len() > 48 {
            return Err("Цепочка обязательных зависимостей превышает 48 модов.".into());
        }

        let file = primary_version_file(&version, ".jar")?;
        if allow_any_installed_version.is_some()
            && existing_mod_file_matches(&mods, &file.filename, &file.hashes.sha1)
        {
            continue;
        }
        let download = download_version_file(&client, &version, &mods).await?;
        pending_downloads.0.push(download.temporary.clone());
        total_size = total_size.saturating_add(download.size);
        if total_size > 1_000_000_000 {
            return Err("Общий объём модов превышает 1 ГБ.".into());
        }
        let dependency_ids = version
            .dependencies
            .iter()
            .filter(|dependency| dependency.dependency_type.as_deref() == Some("required"))
            .filter_map(|dependency| dependency.project_id.clone())
            .collect();
        staged_files.push(StagedMod {
            download,
            project_id: version.project_id.clone(),
            version_id: version.id.clone(),
            version_number: version.version_number.clone(),
            dependencies: dependency_ids,
        });

        for dependency in version
            .dependencies
            .into_iter()
            .filter(|dependency| dependency.dependency_type.as_deref() == Some("required"))
        {
            let (dependency_version, allow_any_version) =
                if let Some(version_id) = dependency.version_id {
                    (
                        Some(
                            client
                                .get(format!("https://api.modrinth.com/v2/version/{version_id}"))
                                .send()
                                .await
                                .map_err(|error| error.to_string())?
                                .error_for_status()
                                .map_err(|error| error.to_string())?
                                .json::<ModVersion>()
                                .await
                                .map_err(|error| error.to_string())?,
                        ),
                        false,
                    )
                } else if let Some(project_id) = dependency.project_id {
                    (
                        client
                            .get(format!(
                                "https://api.modrinth.com/v2/project/{project_id}/version"
                            ))
                            .query(&[
                                ("game_versions", format!("[\"{minecraft_version}\"]")),
                                (
                                    "loaders",
                                    serde_json::to_string(&loaders)
                                        .map_err(|error| error.to_string())?,
                                ),
                            ])
                            .send()
                            .await
                            .map_err(|error| error.to_string())?
                            .error_for_status()
                            .map_err(|error| error.to_string())?
                            .json::<Vec<ModVersion>>()
                            .await
                            .map_err(|error| error.to_string())?
                            .into_iter()
                            .next(),
                        true,
                    )
                } else {
                    (None, false)
                };
            pending.push((
                dependency_version
                    .ok_or("Modrinth не вернул обязательную зависимость для текущей версии.")?,
                Some(allow_any_version),
            ));
        }
    }

    let mut filenames = HashSet::new();
    for staged in &staged_files {
        if !filenames.insert(staged.download.filename.to_ascii_lowercase())
            || mods.join(&staged.download.filename).exists()
        {
            return Err(format!(
                "Файл мода {} уже существует.",
                staged.download.filename
            ));
        }
    }
    let mut committed = Vec::new();
    for staged in &staged_files {
        let destination = mods.join(&staged.download.filename);
        if let Err(error) = fs::rename(&staged.download.temporary, &destination) {
            for path in committed {
                let _ = fs::remove_file(path);
            }
            return Err(error.to_string());
        }
        committed.push(destination);
    }
    let root_file = staged_files
        .iter()
        .find(|staged| staged.project_id == project_id)
        .ok_or("Modrinth не вернул файл мода.")?;
    let mut index = read_modrinth_index(&mods)?;
    for staged in &staged_files {
        index.insert(
            staged.download.filename.clone(),
            ManagedMod {
                project_id: staged.project_id.clone(),
                version_id: staged.version_id.clone(),
                version_number: Some(staged.version_number.clone()),
                icon_url: None,
                dependencies: staged.dependencies.clone(),
            },
        );
    }
    if let Err(error) = write_modrinth_index(&mods, &index) {
        for staged in &staged_files {
            let _ = fs::remove_file(mods.join(&staged.download.filename));
        }
        return Err(error);
    }
    Ok(InstalledMod {
        file_name: root_file.download.filename.clone(),
        enabled: true,
        project_id: Some(root_file.project_id.clone()),
        version_id: Some(root_file.version_id.clone()),
        version_number: Some(root_file.version_number.clone()),
        title: None,
        icon_url: None,
    })
}

#[tauri::command]
pub(crate) async fn install_mod(
    app: AppHandle,
    folder_name: String,
    project_id: String,
    minecraft_version: String,
    loader: String,
) -> Result<InstalledMod, String> {
    let _instance_lock = crate::locks::InstanceLock::acquire(&folder_name)?;
    if !["fabric", "forge", "neoforge"].contains(&loader.as_str()) {
        return Err("Для установки мода выберите Fabric, Forge или NeoForge.".into());
    }
    install_mod_at(
        mods_dir(&app, &folder_name)?,
        project_id,
        minecraft_version,
        vec![loader],
        Vec::new(),
        None,
    )
    .await
}

#[tauri::command]
pub(crate) async fn update_mod(
    app: AppHandle,
    folder_name: String,
    file_name: String,
    minecraft_version: String,
    loader: String,
    version_id: Option<String>,
) -> Result<InstalledMod, String> {
    let _instance_lock = crate::locks::InstanceLock::acquire(&folder_name)?;
    if file_name.contains(['/', '\\', ':']) || file_name.starts_with('.') {
        return Err("Некорректное имя файла мода.".into());
    }
    if minecraft_version.is_empty() || !["fabric", "forge", "neoforge"].contains(&loader.as_str()) {
        return Err("Для обновления выберите поддерживаемую версию игры и загрузчик.".into());
    }
    let current_enabled = file_name.to_ascii_lowercase().ends_with(".jar");
    let current_base =
        normalized_mod_filename(&file_name).ok_or("Можно обновлять только файлы модов JAR.")?;
    let mods = mods_dir(&app, &folder_name)?;
    let current_path = mods.join(&file_name);
    if !current_path.is_file() {
        return Err("Файл мода не найден.".into());
    }
    let mut index = read_modrinth_index(&mods)?;
    let managed = index
        .get(current_base)
        .cloned()
        .ok_or("Этот JAR не установлен через Modrinth и не может быть обновлён автоматически.")?;
    let client = modrinth_client()?;
    let versions: Vec<ModVersion> = client
        .get(format!(
            "https://api.modrinth.com/v2/project/{}/version",
            managed.project_id
        ))
        .query(&[
            ("game_versions", format!("[\"{minecraft_version}\"]")),
            ("loaders", format!("[\"{loader}\"]")),
        ])
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .bounded_json()
        .await
        .map_err(|error| error.to_string())?;
    let version = if let Some(version_id) = version_id.filter(|value| !value.is_empty()) {
        if version_id.len() > 64
            || !version_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
        {
            return Err("Некорректный идентификатор версии Modrinth.".into());
        }
        versions
            .into_iter()
            .find(|version| version.id == version_id)
            .ok_or("Выбранная версия несовместима с этим инстансом.")?
    } else {
        versions
            .into_iter()
            .next()
            .ok_or("Совместимая версия мода не найдена.")?
    };
    if version.id == managed.version_id {
        return Err("Установлена последняя совместимая версия этого мода.".into());
    }
    let download = download_version_file(&client, &version, &mods).await?;
    let _cleanup_download = TemporaryDownloadGuard(vec![download.temporary.clone()]);
    let new_filename = download.filename;
    let destination_filename = if current_enabled {
        new_filename.clone()
    } else {
        format!("{new_filename}.disabled")
    };
    let destination = mods.join(&destination_filename);
    if destination != current_path && destination.exists() {
        return Err(format!("Файл {destination_filename} уже существует."));
    }
    let temporary = download.temporary;
    let backup = mods.join(format!("{destination_filename}.update-backup"));
    if backup.exists() {
        return Err("Найден файл резервной копии обновления; проверьте папку mods.".into());
    }
    fs::rename(&current_path, &backup).map_err(|error| error.to_string())?;
    if let Err(error) = fs::rename(&temporary, &destination) {
        let _ = fs::rename(&backup, &current_path);
        return Err(error.to_string());
    }
    index.remove(current_base);
    index.insert(
        new_filename,
        ManagedMod {
            project_id: version.project_id.clone(),
            version_id: version.id.clone(),
            version_number: Some(version.version_number.clone()),
            icon_url: managed.icon_url.clone(),
            dependencies: managed.dependencies.clone(),
        },
    );
    if let Err(error) = write_modrinth_index(&mods, &index) {
        let _ = fs::remove_file(&destination);
        let _ = fs::rename(&backup, &current_path);
        return Err(error);
    }
    fs::remove_file(backup).map_err(|error| error.to_string())?;
    Ok(InstalledMod {
        file_name: destination_filename,
        enabled: current_enabled,
        project_id: Some(managed.project_id),
        version_id: Some(version.id.clone()),
        version_number: Some(version.version_number.clone()),
        title: None,
        icon_url: managed.icon_url,
    })
}

#[tauri::command]
pub(crate) fn toggle_mod(
    app: AppHandle,
    folder_name: String,
    file_name: String,
    enabled: bool,
) -> Result<InstalledMod, String> {
    let _instance_lock = crate::locks::InstanceLock::acquire(&folder_name)?;
    if file_name.contains(['/', '\\', ':']) || file_name.starts_with('.') {
        return Err("Некорректное имя файла мода.".into());
    }
    let mods = mods_dir(&app, &folder_name)?;
    let current = mods.join(&file_name);
    let next_name = if enabled {
        file_name
            .strip_suffix(".jar.disabled")
            .map(|name| format!("{name}.jar"))
            .ok_or("Мод уже включён.")?
    } else {
        if !file_name.to_ascii_lowercase().ends_with(".jar") {
            return Err("Некорректный файл мода.".into());
        }
        format!("{file_name}.disabled")
    };
    safe_mod_filename(if enabled { &next_name } else { &file_name })?;
    let destination = mods.join(&next_name);
    if destination.exists() {
        return Err("Файл с таким именем уже существует.".into());
    }
    fs::rename(current, destination).map_err(|error| error.to_string())?;
    let managed = read_modrinth_index(&mods)?
        .get(normalized_mod_filename(&next_name).unwrap_or_default())
        .cloned();
    let version_id = managed.as_ref().map(|item| item.version_id.clone());
    let version_number = managed
        .as_ref()
        .and_then(|item| item.version_number.clone());
    let project_id = managed.as_ref().map(|item| item.project_id.clone());
    let icon_url = managed.and_then(|item| item.icon_url);
    Ok(InstalledMod {
        file_name: next_name,
        enabled,
        project_id,
        version_id,
        version_number,
        title: None,
        icon_url,
    })
}

#[tauri::command]
pub(crate) fn remove_mod(
    app: AppHandle,
    folder_name: String,
    file_name: String,
) -> Result<(), String> {
    let _instance_lock = crate::locks::InstanceLock::acquire(&folder_name)?;
    if file_name.contains(['/', '\\', ':']) || file_name.starts_with('.') {
        return Err("Некорректное имя файла мода.".into());
    }
    let filename = file_name.to_ascii_lowercase();
    if !filename.ends_with(".jar") && !filename.ends_with(".jar.disabled") {
        return Err("Можно удалять только файлы модов JAR.".into());
    }
    let mods = mods_dir(&app, &folder_name)?;
    let path = mods.join(&file_name);
    if !path.is_file() {
        return Err("Файл мода не найден.".into());
    }
    fs::remove_file(path).map_err(|error| error.to_string())?;
    let mut index = read_modrinth_index(&mods)?;
    if let Some(normalized) = normalized_mod_filename(&file_name) {
        index.remove(normalized);
    }
    write_modrinth_index(&mods, &index)
}
