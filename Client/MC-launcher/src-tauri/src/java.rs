use std::{
    collections::HashMap,
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, OnceLock},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};
use tokio::io::AsyncWriteExt;
use zip::ZipArchive;

use crate::{minecraft::is_trusted_host, process::hide_console_window};

static JAVA_INSTALL_LOCKS: OnceLock<Mutex<HashMap<u8, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

fn java_install_locks() -> &'static Mutex<HashMap<u8, Arc<tokio::sync::Mutex<()>>>> {
    JAVA_INSTALL_LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Deserialize)]
struct TemurinAsset {
    binary: TemurinBinary,
}

#[derive(Deserialize)]
struct TemurinBinary {
    package: TemurinPackage,
}

#[derive(Deserialize)]
struct TemurinPackage {
    link: String,
    checksum: String,
}

fn launcher_root(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map_err(|error| error.to_string())?
        .parent()
        .ok_or_else(|| "Не удалось определить папку AppData.".to_string())
        .map(|path| path.join("MyLauncher"))
}

fn valid_archive_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn find_javaw(root: &Path, depth: usize) -> Option<PathBuf> {
    if depth > 2 {
        return None;
    }
    let entries = fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("javaw.exe"))
        {
            return Some(path);
        }
        if path.is_dir()
            && let Some(found) = find_javaw(&path, depth + 1)
        {
            return Some(found);
        }
    }
    None
}

fn parse_java_major(output: &str) -> Option<u8> {
    output
        .split(|character: char| !character.is_ascii_digit() && character != '.')
        .find_map(|token| {
            let mut components = token.split('.');
            let first = components.next()?.parse::<u8>().ok()?;
            if first == 1 {
                components.next()?.parse::<u8>().ok()
            } else {
                Some(first)
            }
        })
}

pub(crate) fn java_major_version(javaw_path: &Path) -> Result<u8, String> {
    let java_path = javaw_path.with_file_name("java.exe");
    let mut command = Command::new(java_path);
    hide_console_window(&mut command);
    let output = command
        .arg("-version")
        .output()
        .map_err(|error| format!("Не удалось проверить Java: {error}"))?;
    let version_output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        return Err("Java runtime не прошёл проверку версии.".into());
    }
    parse_java_major(&version_output)
        .ok_or_else(|| "Не удалось определить major-версию Java.".into())
}

pub(crate) fn validate_javaw(path: &str) -> Result<PathBuf, String> {
    let canonical = PathBuf::from(path)
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if !canonical.is_file()
        || !canonical
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("javaw.exe"))
    {
        return Err("Выберите существующий файл javaw.exe.".into());
    }
    let mut command = Command::new(canonical.with_file_name("java.exe"));
    hide_console_window(&mut command);
    let status = command
        .arg("-version")
        .output()
        .map_err(|error| format!("Не удалось проверить Java перед запуском: {error}"))?;
    if !status.status.success() {
        return Err("Java runtime не прошёл проверку версии.".into());
    }
    Ok(canonical)
}

async fn download_java_archive(
    client: &reqwest::Client,
    url: &str,
    destination: &Path,
) -> Result<String, String> {
    let mut last_error = None;
    for attempt in 0..3 {
        let response = match client.get(url).send().await {
            Ok(response) => response,
            Err(error)
                if attempt < 2 && (error.is_connect() || error.is_timeout() || error.is_body()) =>
            {
                last_error = Some(error.to_string());
                tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
                continue;
            }
            Err(error) => return Err(error.to_string()),
        };
        let mut response = response
            .error_for_status()
            .map_err(|error| error.to_string())?;
        if response.content_length().is_some_and(|size| size > 512_000_000) {
            return Err("Архив JRE превышает допустимый размер.".into());
        }
        let mut archive_file = tokio::fs::File::create(destination)
            .await
            .map_err(|error| format!("Не удалось создать файл архива JRE: {error}"))?;
        let mut hasher = Sha256::new();
        let mut total_downloaded = 0_u64;
        let mut retry_error = None;
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    total_downloaded = total_downloaded.saturating_add(chunk.len() as u64);
                    if total_downloaded > 512_000_000 {
                        drop(archive_file);
                        let _ = tokio::fs::remove_file(destination).await;
                        return Err("Архив JRE превышает допустимый размер.".into());
                    }
                    hasher.update(&chunk);
                    if let Err(error) = archive_file.write_all(&chunk).await {
                        drop(archive_file);
                        let _ = tokio::fs::remove_file(destination).await;
                        return Err(format!("Не удалось записать архив JRE на диск: {error}"));
                    }
                }
                Ok(None) => break,
                Err(error)
                    if attempt < 2 && (error.is_connect() || error.is_timeout() || error.is_body()) =>
                {
                    retry_error = Some(error.to_string());
                    break;
                }
                Err(error) => {
                    drop(archive_file);
                    let _ = tokio::fs::remove_file(destination).await;
                    return Err(format!("Соединение оборвалось при загрузке JRE: {error:#}"));
                }
            }
        }
        if let Some(error) = retry_error {
            drop(archive_file);
            let _ = tokio::fs::remove_file(destination).await;
            last_error = Some(error);
            tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
            continue;
        }
        if let Err(error) = archive_file.flush().await {
            drop(archive_file);
            let _ = tokio::fs::remove_file(destination).await;
            return Err(format!("Не удалось сохранить архив JRE: {error}"));
        }
        drop(archive_file);
        return Ok(hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect());
    }
    Err(format!(
        "Не удалось загрузить архив JRE после трёх попыток: {}",
        last_error.unwrap_or_else(|| "неизвестная ошибка сети".into())
    ))
}

#[tauri::command]
pub(crate) async fn install_java_runtime(app: AppHandle, version: u8) -> Result<String, String> {
    if version < 8 {
        return Err("Для установки требуется Java 8 или новее.".into());
    }

    let version_lock = java_install_locks()
        .lock()
        .map_err(|_| "Не удалось получить блокировку установки Java.".to_string())?
        .entry(version)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    let _version_guard = version_lock.lock().await;

    let java_root = launcher_root(&app)?.join("java");
    let destination = java_root.join(version.to_string());
    if let Some(javaw) = find_javaw(&destination, 0) {
        if java_major_version(&javaw).ok() == Some(version) {
            return Ok(javaw.to_string_lossy().into_owned());
        }
        fs::remove_dir_all(&destination).map_err(|error| error.to_string())?;
    }
    fs::create_dir_all(&java_root).map_err(|error| error.to_string())?;

    let client = reqwest::Client::builder()
        .user_agent("MyLauncher/0.1.0")
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(900))
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
        .map_err(|error| error.to_string())?;
    let metadata_url = format!(
        "https://api.adoptium.net/v3/assets/latest/{version}/hotspot?architecture=x64&image_type=jre&os=windows&vendor=eclipse"
    );
    let assets: Vec<TemurinAsset> = client
        .get(metadata_url)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    let package = assets
        .into_iter()
        .next()
        .ok_or_else(|| format!("Temurin JRE для Java {version} не найден."))?
        .binary
        .package;
    let package_url = reqwest::Url::parse(&package.link).map_err(|error| error.to_string())?;
    if package_url.scheme() != "https" || !package_url.host_str().is_some_and(is_trusted_host) {
        return Err("Ссылка JRE не принадлежит доверенному HTTPS-домену.".into());
    }

    let archive_path = java_root.join(format!("{version}-temurin.zip"));
    let _ = fs::remove_file(&archive_path);
    let actual_checksum = download_java_archive(&client, &package.link, &archive_path).await?;
    if !actual_checksum.eq_ignore_ascii_case(&package.checksum) {
        let _ = fs::remove_file(&archive_path);
        return Err("SHA-256 архива Temurin не совпал с метаданными.".into());
    }

    let staging = java_root.join(format!("{version}-staging"));
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|error| error.to_string())?;
    }
    fs::create_dir_all(&staging).map_err(|error| error.to_string())?;
    let extraction = (|| {
        let archive_file = fs::File::open(&archive_path).map_err(|error| error.to_string())?;
        let mut archive = ZipArchive::new(archive_file).map_err(|error| error.to_string())?;
        let mut total_uncompressed = 0_u64;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
            let Some(relative_path) = entry.enclosed_name() else {
                return Err("Архив JRE содержит небезопасный путь.".into());
            };
            if !valid_archive_path(&relative_path) {
                return Err("Архив JRE содержит небезопасный путь.".into());
            }
            total_uncompressed = total_uncompressed.saturating_add(entry.size());
            if total_uncompressed > 1_500_000_000 {
                return Err("Распакованный архив JRE превышает допустимый размер.".into());
            }
            let output = staging.join(relative_path);
            if entry.is_dir() {
                fs::create_dir_all(output).map_err(|error| error.to_string())?;
            } else {
                let parent = output.parent().ok_or("Некорректная структура JRE.")?;
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                let mut file = fs::File::create(output).map_err(|error| error.to_string())?;
                std::io::copy(&mut entry, &mut file).map_err(|error| error.to_string())?;
            }
        }
        Ok::<(), String>(())
    })();
    let _ = fs::remove_file(&archive_path);
    if let Err(error) = extraction {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    let staged_javaw = find_javaw(&staging, 0)
        .ok_or_else(|| "В архиве Temurin не найден javaw.exe.".to_string())?;
    if java_major_version(&staged_javaw)? != version {
        let _ = fs::remove_dir_all(&staging);
        return Err(format!("Temurin вернул runtime не для Java {version}."));
    }
    let relative_javaw = staged_javaw
        .strip_prefix(&staging)
        .map_err(|error| error.to_string())?;
    if destination.exists() {
        fs::remove_dir_all(&destination).map_err(|error| error.to_string())?;
    }
    fs::rename(&staging, &destination).map_err(|error| error.to_string())?;
    let javaw_path = destination.join(relative_javaw);
    let _ = package.link;
    Ok(javaw_path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::{find_javaw, parse_java_major, valid_archive_path};

    #[test]
    fn parses_legacy_and_modern_java_version_output() {
        assert_eq!(parse_java_major("java version \"1.8.0_432\""), Some(8));
        assert_eq!(parse_java_major("openjdk version \"21.0.3\""), Some(21));
        assert_eq!(parse_java_major("openjdk 25 2025-03-18"), Some(25));
    }

    #[test]
    fn accepts_only_relative_archive_paths() {
        assert!(valid_archive_path(std::path::Path::new(
            "jdk/bin/javaw.exe"
        )));
        assert!(!valid_archive_path(std::path::Path::new("../outside.exe")));
        assert!(!valid_archive_path(std::path::Path::new("C:/outside.exe")));
    }

    #[test]
    fn searches_java_runtime_archive_layout() {
        let temp = std::env::current_dir()
            .unwrap()
            .join(format!("mylauncher-java-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        let javaw = temp.join("jdk-21/bin/javaw.exe");
        std::fs::create_dir_all(javaw.parent().unwrap()).unwrap();
        std::fs::write(&javaw, []).unwrap();
        assert_eq!(find_javaw(&temp, 0), Some(javaw));
        std::fs::remove_dir_all(temp).unwrap();
    }
}
