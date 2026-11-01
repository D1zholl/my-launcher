use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Write},
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha1::{Digest, Sha1};
use sha2::{Sha256, Sha512};
use futures::{StreamExt, TryStreamExt};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;
use zip::ZipArchive;

const MAX_PACK_BYTES: u64 = 1_000_000_000;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_REPOSITORY_FILE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_TOTAL_FILE_BYTES: u64 = 20_000_000_000;
const MAX_TOTAL_ARCHIVE_BYTES: u64 = 20_000_000_000;
const MAX_ARCHIVE_ENTRY_BYTES: u64 = 1_000_000_000;
const MAX_INDEX_BYTES: u64 = 16_000_000;
const MAX_ARCHIVE_ENTRIES: usize = 100_000;
const MAX_REPOSITORY_JSON_BYTES: usize = 64 * 1024 * 1024;

fn repository_file_size_supported(size: u64) -> bool {
    size <= MAX_REPOSITORY_FILE_BYTES
}

#[derive(Deserialize)]
struct ApiProject {
    #[serde(alias = "project_id")]
    id: String,
    slug: String,
    title: String,
    description: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    categories: Vec<String>,
    icon_url: Option<String>,
    downloads: u64,
    project_type: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ModpackProject {
    project_id: String,
    slug: String,
    title: String,
    description: String,
    author: String,
    categories: Vec<String>,
    icon_url: Option<String>,
    downloads: u64,
}

#[derive(Deserialize)]
struct SearchResponse {
    hits: Vec<ApiProject>,
    total_hits: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModpackSearchResults {
    hits: Vec<ModpackProject>,
    total_hits: usize,
}

#[derive(Deserialize)]
struct ApiVersion {
    id: String,
    project_id: String,
    version_number: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    game_versions: Vec<String>,
    #[serde(default)]
    loaders: Vec<String>,
    #[serde(default)]
    date_published: String,
    files: Vec<ApiVersionFile>,
}

#[derive(Deserialize)]
struct ApiVersionFile {
    url: String,
    filename: String,
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    hashes: HashMap<String, String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ModpackVersion {
    version_id: String,
    version_number: String,
    name: Option<String>,
    game_versions: Vec<String>,
    loaders: Vec<String>,
    file_name: String,
    date_published: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModpackDetails {
    project: ModpackProject,
    versions: Vec<ModpackVersion>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModpackInstallResult {
    minecraft_version: String,
    loader_type: String,
    loader_version: Option<String>,
    java_version: u8,
    java_path: String,
    modpack_project_id: String,
    modpack_version_id: String,
    modpack_version_number: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RepositoryPackSummary {
    id: String,
    name: String,
    description: String,
    authors: Vec<String>,
    categories: Vec<String>,
    icon_url: Option<String>,
    latest_version: String,
    channel: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RepositoryVersionSummary {
    version: String,
    channel: String,
    updated_at: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepositoryPackList {
    packs: Vec<RepositoryPackSummary>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepositoryVersions {
    versions: Vec<RepositoryVersionSummary>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepositoryManifest {
    manifest_version: u32,
    pack_id: String,
    version: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    authors: Vec<String>,
    #[serde(default)]
    categories: Vec<String>,
    minecraft: RepositoryMinecraft,
    java: RepositoryJava,
    files: Vec<RepositoryFile>,
    #[serde(default)]
    settings: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepositoryMinecraft {
    version: String,
    loader: String,
    loader_version: Option<String>,
}

#[derive(Deserialize)]
struct RepositoryJava {
    major: u8,
}

#[derive(Deserialize, Serialize, Clone)]
struct RepositoryFile {
    path: String,
    hash: String,
    size: u64,
    url: String,
    required: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RepositoryInstallResult {
    minecraft_version: String,
    loader_type: String,
    loader_version: Option<String>,
    java_version: u8,
    java_path: String,
    pack_id: String,
    version: String,
    channel: String,
    name: String,
    repository_url: String,
    memory_mb: Option<u32>,
    jvm_args: Option<String>,
    window_width: Option<u32>,
    window_height: Option<u32>,
    fullscreen: Option<bool>,
    transaction_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MrpackIndex {
    format_version: u32,
    game: String,
    version_id: String,
    #[serde(default)]
    name: String,
    files: Vec<MrpackFile>,
    dependencies: HashMap<String, String>,
}

#[derive(Deserialize)]
struct MrpackFile {
    path: String,
    hashes: HashMap<String, String>,
    #[serde(default)]
    env: HashMap<String, String>,
    downloads: Vec<String>,
    #[serde(rename = "fileSize")]
    file_size: u64,
}

fn client_file_supported(file: &MrpackFile) -> bool {
    file.env
        .get("client")
        .is_none_or(|value| value != "unsupported")
}

fn validate_pack_file(file: &MrpackFile) -> Result<(), String> {
    safe_pack_path(&file.path)?;
    if file.file_size > MAX_FILE_BYTES {
        return Err("Файл сборки превышает допустимый размер.".into());
    }
    if file.env
        .values()
        .any(|value| !["required", "optional", "unsupported"].contains(&value.as_str()))
    {
        return Err("Манифест содержит некорректное окружение файла.".into());
    }
    if !client_file_supported(file) {
        return Ok(());
    }
    let valid_digest = |algorithm: &str, length: usize| {
        file.hashes.get(algorithm).is_some_and(|digest| {
            digest.len() == length && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    };
    if !valid_digest("sha1", 40) || !valid_digest("sha512", 128) {
        return Err("В манифесте файла должны быть корректные SHA-1 и SHA-512.".into());
    }
    if file.downloads.is_empty() {
        return Err("Для файла сборки не указан URL загрузки.".into());
    }
    for url in &file.downloads {
        safe_modrinth_url(url)?;
    }
    Ok(())
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    completed: usize,
    total: usize,
    current_file: String,
}

static CANCELLATIONS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();

fn cancellations() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    CANCELLATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

struct CancellationRegistration {
    folder_name: String,
}

impl Drop for CancellationRegistration {
    fn drop(&mut self) {
        if let Ok(mut active) = cancellations().lock() {
            active.remove(&self.folder_name);
        }
    }
}

struct TemporaryDirectory {
    path: PathBuf,
    keep: bool,
}

#[derive(Deserialize, Serialize)]
struct UpdateRecoveryJournal {
    instance_folder: String,
    old_files: Vec<String>,
    staged_files: Vec<StagedRecoveryFile>,
}

#[derive(Deserialize, Serialize)]
struct StagedRecoveryFile {
    path: String,
    temporary_name: String,
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

impl TemporaryDirectory {
    fn new(path: PathBuf) -> Self {
        Self { path, keep: false }
    }
}

pub(crate) fn cleanup_repository_staging(root: &Path) -> std::io::Result<()> {
    if !root.is_dir() {
        return Ok(());
    }
    let download_cache = root.join(".repository-download-cache");
    if download_cache.is_dir() {
        let cutoff = std::time::SystemTime::now() - Duration::from_secs(30 * 24 * 60 * 60);
        for entry in fs::read_dir(&download_cache)? {
            let entry = entry?;
            if entry.metadata()?.modified().is_ok_and(|modified| modified < cutoff) {
                fs::remove_file(entry.path())?;
            }
        }
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_staging_instance = name.strip_prefix(".repo-stage-")
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok());
        if !file_type.is_dir() || is_staging_instance {
            if file_type.is_dir() && is_staging_instance {
                fs::remove_dir_all(entry.path())?;
            }
            continue;
        }
        let is_staging_transaction = name.strip_prefix(".repo-")
            .and_then(|value| value.strip_suffix(".tmp"))
            .is_some_and(has_uuid_suffix);
        if is_staging_transaction {
            if entry.path().join("commit-complete").is_file() {
                continue;
            }
            if let Err(error) = recover_repository_update(root, &entry.path()) {
                eprintln!("Не удалось восстановить обновление репозитория {}: {error}", entry.path().display());
                continue;
            }
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

fn has_uuid_suffix(value: &str) -> bool {
    let uuid_start = value.len().saturating_sub(36);
    value.len() > 36
        && value.as_bytes().get(uuid_start - 1) == Some(&b'-')
        && value.get(uuid_start..).is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
}

fn recover_repository_update(root: &Path, staging: &Path) -> Result<(), String> {
    if staging.join("commit-complete").is_file() {
        return Ok(());
    }
    let journal_path = staging.join("commit-journal.json");
    if !journal_path.is_file() {
        return Ok(());
    }
    let journal: UpdateRecoveryJournal = serde_json::from_slice(
        &fs::read(&journal_path).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    crate::validate_folder_name(&journal.instance_folder)?;
    let instance_root = root.join(&journal.instance_folder);
    let backup_root = staging.join("backup/files");
    let mut restored = HashSet::new();
    for relative in &journal.old_files {
        let safe_relative = safe_repository_path(relative)?;
        let backup = backup_root.join(&safe_relative);
        if !backup.exists() {
            continue;
        }
        let destination = safe_pack_destination(&instance_root, relative)?;
        if destination.exists() {
            fs::remove_file(&destination).map_err(|error| error.to_string())?;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::rename(&backup, &destination).map_err(|error| error.to_string())?;
        restored.insert(relative.to_ascii_lowercase());
    }
    for staged_file in &journal.staged_files {
        let source_name = Path::new(&staged_file.temporary_name)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Некорректное имя staging-файла")?;
        if source_name != staged_file.temporary_name {
            return Err("Некорректное имя staging-файла".into());
        }
        if staging.join(source_name).exists() || restored.contains(&staged_file.path.to_ascii_lowercase()) {
            continue;
        }
        let destination = safe_pack_destination(&instance_root, &staged_file.path)?;
        if destination.is_file() {
            fs::remove_file(destination).map_err(|error| error.to_string())?;
        }
    }
    let metadata_backup = staging.join("backup/launcher-meta.json");
    if metadata_backup.is_file() {
        let metadata_path = instance_root.join(".launcher_meta.json");
        if metadata_path.exists() {
            fs::remove_file(&metadata_path).map_err(|error| error.to_string())?;
        }
        fs::rename(metadata_backup, metadata_path).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn repository_update_staging(root: &Path, transaction_id: &str) -> Result<PathBuf, String> {
    let id = transaction_id
        .strip_prefix(".repo-update-")
        .and_then(|value| value.strip_suffix(".tmp"))
        .ok_or("Некорректный идентификатор обновления")?;
    uuid::Uuid::parse_str(id).map_err(|_| "Некорректный идентификатор обновления")?;
    let staging = root.join(transaction_id);
    if !staging.is_dir() {
        return Err("Журнал обновления не найден.".into());
    }
    Ok(staging)
}

#[tauri::command]
pub(crate) fn rollback_repository_update(
    app: AppHandle,
    transaction_id: String,
) -> Result<(), String> {
    let root = crate::instances_root(&app)?;
    let staging = repository_update_staging(&root, &transaction_id)?;
    let commit_marker = staging.join("commit-complete");
    if commit_marker.exists() {
        fs::remove_file(commit_marker).map_err(|error| error.to_string())?;
    }
    recover_repository_update(&root, &staging)?;
    fs::remove_dir_all(staging).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn finalize_repository_update(
    app: AppHandle,
    transaction_id: String,
) -> Result<(), String> {
    let root = crate::instances_root(&app)?;
    let staging = repository_update_staging(&root, &transaction_id)?;
    fs::remove_dir_all(staging).map_err(|error| error.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PendingRepositoryUpdate {
    transaction_id: String,
    instance_folder: String,
    version: String,
}

fn pending_update_instance_root(root: &Path, instance_folder: &str) -> Result<PathBuf, String> {
    crate::validate_folder_name(instance_folder)?;
    Ok(root.join(instance_folder))
}

#[tauri::command]
pub(crate) fn list_pending_repository_updates(
    app: AppHandle,
) -> Result<Vec<PendingRepositoryUpdate>, String> {
    let root = crate::instances_root(&app)?;
    let mut pending = Vec::new();
    for entry in fs::read_dir(&root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let transaction_id = entry.file_name().to_string_lossy().into_owned();
        if !transaction_id.starts_with(".repo-update-") || !transaction_id.ends_with(".tmp") {
            continue;
        }
        let staging = repository_update_staging(&root, &transaction_id)?;
        if !staging.join("commit-complete").is_file() {
            continue;
        }
        let journal: UpdateRecoveryJournal = serde_json::from_slice(
            &fs::read(staging.join("commit-journal.json")).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let instance_root = pending_update_instance_root(&root, &journal.instance_folder)?;
        let metadata: serde_json::Value = serde_json::from_slice(
            &fs::read(instance_root.join(".launcher_meta.json"))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let version = metadata
            .get("repository_version")
            .and_then(serde_json::Value::as_str)
            .ok_or("В метаданных обновления отсутствует версия сборки")?;
        pending.push(PendingRepositoryUpdate {
            transaction_id,
            instance_folder: journal.instance_folder,
            version: version.to_string(),
        });
    }
    Ok(pending)
}

fn modrinth_client() -> Result<reqwest::Client, String> {
    crate::modrinth::modrinth_client()
}

fn is_pack_source_host(host: &str) -> bool {
    matches!(
        host,
        "cdn.modrinth.com" | "github.com" | "raw.githubusercontent.com" | "gitlab.com"
    )
}

fn is_pack_redirect_host(host: &str) -> bool {
    is_pack_source_host(host)
        || matches!(
            host,
            "release-assets.githubusercontent.com"
                | "objects.githubusercontent.com"
                | "github-releases.githubusercontent.com"
        )
}

fn modpack_download_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("MyLauncher/0.1.0 (local desktop launcher)")
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(600))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 8
                && attempt.url().scheme() == "https"
                && attempt.url().host_str().is_some_and(is_pack_redirect_host)
            {
                attempt.follow()
            } else {
                attempt.error("Redirect blocked: destination is not allowlisted")
            }
        }))
        .build()
        .map_err(|error| error.to_string())
}

async fn wait_for_cancel(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn cancellable<T, E, F>(future: F, cancel: &AtomicBool) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    if cancel.load(Ordering::Relaxed) {
        return Err("Установка сборки отменена.".into());
    }
    match futures::future::select(Box::pin(future), Box::pin(wait_for_cancel(cancel))).await {
        futures::future::Either::Left((Ok(value), _)) => Ok(value),
        futures::future::Either::Left((Err(error), _)) => Err(error.to_string()),
        futures::future::Either::Right(((), _)) => Err("Установка сборки отменена.".into()),
    }
}

fn is_retryable_repository_status(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::REQUEST_TIMEOUT
            | reqwest::StatusCode::TOO_MANY_REQUESTS
            | reqwest::StatusCode::INTERNAL_SERVER_ERROR
            | reqwest::StatusCode::BAD_GATEWAY
            | reqwest::StatusCode::SERVICE_UNAVAILABLE
            | reqwest::StatusCode::GATEWAY_TIMEOUT
    )
}

async fn wait_for_repository_retry(
    attempt: usize,
    retry_after: Option<&str>,
    cancel: Option<&AtomicBool>,
) -> Result<(), String> {
    let seconds = retry_after
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| seconds.min(30))
        .unwrap_or_else(|| 1_u64.checked_shl(attempt as u32).unwrap_or(30).min(30));
    if cancel.is_some_and(|token| token.load(Ordering::Relaxed)) {
        return Err("Установка сборки отменена.".into());
    }
    let delay = tokio::time::sleep(Duration::from_secs(seconds));
    if let Some(cancel) = cancel {
        tokio::select! {
            _ = delay => Ok(()),
            _ = wait_for_cancel(cancel) => Err("Установка сборки отменена.".into()),
        }
    } else {
        delay.await;
        Ok(())
    }
}

async fn repository_get(
    client: &reqwest::Client,
    url: reqwest::Url,
    cancel: Option<&AtomicBool>,
) -> Result<reqwest::Response, String> {
    for attempt in 0..3 {
        let result = if let Some(cancel) = cancel {
            cancellable(client.get(url.clone()).send(), cancel).await
        } else {
            client.get(url.clone()).send().await.map_err(|error| error.to_string())
        };
        match result {
            Ok(response) if is_retryable_repository_status(response.status()) && attempt < 2 => {
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                drop(response);
                wait_for_repository_retry(attempt, retry_after.as_deref(), cancel).await?;
            }
            Ok(response) => {
                return response.error_for_status().map_err(|error| error.to_string());
            }
            Err(_error) if attempt < 2 => {
                wait_for_repository_retry(attempt, None, cancel).await?;
            }
            Err(error) => return Err(error),
        }
    }
    Err("Не удалось получить ответ от репозитория после нескольких попыток.".into())
}

fn checked_project_id(project_id: &str) -> Result<(), String> {
    if project_id.is_empty()
        || project_id.len() > 64
        || !project_id.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err("Некорректный идентификатор проекта Modrinth.".into());
    }
    Ok(())
}

fn map_project(project: ApiProject) -> Result<ModpackProject, String> {
    if project.project_type != "modpack" {
        return Err("Проект Modrinth не является сборкой.".into());
    }
    Ok(ModpackProject {
        project_id: project.id,
        slug: project.slug,
        title: project.title,
        description: project.description,
        author: project.author,
        categories: project.categories,
        icon_url: project.icon_url,
        downloads: project.downloads,
    })
}

fn safe_pack_path(relative: &str) -> Result<PathBuf, String> {
    if relative.is_empty()
        || relative.starts_with('/')
        || relative.contains(['\\', ':'])
        || relative.chars().any(char::is_control)
    {
        return Err("Пакет содержит небезопасный путь.".into());
    }
    let mut path = PathBuf::new();
    for (index, component) in relative.split('/').enumerate() {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.ends_with([' ', '.'])
        {
            return Err("Пакет содержит небезопасный путь.".into());
        }
        if index == 0
            && (component.eq_ignore_ascii_case("versions")
                || component.eq_ignore_ascii_case("assets")
                || component.eq_ignore_ascii_case("libraries")
                || [
                    ".launcher_meta.json",
                    ".launcher_meta.json.tmp",
                    ".launcher_meta.json.backup",
                ]
                .iter()
                .any(|reserved| component.eq_ignore_ascii_case(reserved)))
        {
            return Err("Пакет не может изменять служебные файлы лаунчера.".into());
        }
        let stem = component
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit()
                && stem.as_bytes()[3] != b'0')
        {
            return Err("Пакет содержит имя файла, недопустимое в Windows.".into());
        }
        path.push(component);
    }
    Ok(path)
}

fn safe_pack_destination(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = safe_pack_path(relative)?;
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    let components = relative.components().collect::<Vec<_>>();
    let mut current = canonical_root.clone();
    for (index, component) in components.iter().enumerate() {
        let std::path::Component::Normal(name) = component else {
            return Err("Пакет содержит небезопасный путь.".into());
        };
        current.push(name);
        let is_leaf = index + 1 == components.len();
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("Запись по пути пакета является символической ссылкой.".into());
            }
            Ok(metadata) if !is_leaf && !metadata.is_dir() => {
                return Err("Путь пакета пересекает файл вместо каталога.".into());
            }
            Ok(metadata) if is_leaf && !metadata.is_file() => {
                return Err("Целевой путь пакета не является обычным файлом.".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        if !is_leaf {
            if let Ok(canonical) = current.canonicalize() {
                if !canonical.starts_with(&canonical_root) {
                    return Err("Путь пакета выходит за пределы инстанса.".into());
                }
                current = canonical;
            } else if current.exists() {
                return Err("Не удалось проверить путь пакета.".into());
            }
        }
    }
    Ok(current)
}

fn prepare_pack_destination(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let destination = safe_pack_destination(root, relative)?;
    let parent = destination
        .parent()
        .ok_or("Некорректная папка назначения файла сборки.")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    Ok(destination)
}

fn safe_modrinth_url(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|error| error.to_string())?;
    if parsed.scheme() != "https" || !parsed.host_str().is_some_and(is_pack_source_host) {
        return Err("Ссылка файла не принадлежит доверенному HTTPS-домену.".into());
    }
    Ok(())
}

fn verified_file_hashes_match(
    actual_sha1: &str,
    actual_sha512: &str,
    expected: &HashMap<String, String>,
) -> bool {
    let sha1_matches = expected
        .get("sha1")
        .is_none_or(|value| value.eq_ignore_ascii_case(actual_sha1));
    let sha512_matches = expected
        .get("sha512")
        .is_none_or(|value| value.eq_ignore_ascii_case(actual_sha512));
    expected.contains_key("sha1")
        && expected.contains_key("sha512")
        && sha1_matches
        && sha512_matches
}

async fn download_and_verify(
    client: &reqwest::Client,
    urls: &[String],
    destination: &Path,
    expected_size: Option<u64>,
    expected_hashes: &HashMap<String, String>,
    max_bytes: u64,
    cancel: &AtomicBool,
) -> Result<u64, String> {
    if expected_size.is_some_and(|size| size > max_bytes) {
        return Err("Файл Modrinth превышает допустимый размер.".into());
    }
    if !expected_hashes.contains_key("sha1") || !expected_hashes.contains_key("sha512") {
        return Err("В метаданных Modrinth отсутствует поддерживаемая контрольная сумма.".into());
    }

    let mut last_error = "Не удалось скачать файл Modrinth.".to_string();
    for url in urls {
        safe_modrinth_url(url)?;
        for _attempt in 0..2 {
            if cancel.load(Ordering::Relaxed) {
                return Err("Установка сборки отменена.".into());
            }
            let _ = fs::remove_file(destination);
            let mut response = match cancellable(client.get(url).send(), cancel).await {
                Ok(response) => match response.error_for_status() {
                    Ok(response) => response,
                    Err(error) => {
                        last_error = error.to_string();
                        continue;
                    }
                },
                Err(error) => {
                    if error == "Установка сборки отменена." {
                        return Err(error);
                    }
                    last_error = error.to_string();
                    continue;
                }
            };
            if response
                .content_length()
                .is_some_and(|length| length > max_bytes)
            {
                last_error = "Файл Modrinth превышает допустимый размер.".into();
                continue;
            }
            let mut file = tokio::fs::File::create(destination)
                .await
                .map_err(|error| error.to_string())?;
            let mut sha1 = Sha1::new();
            let mut sha512 = Sha512::new();
            let mut total = 0_u64;
            let mut failed = None;
            loop {
                if cancel.load(Ordering::Relaxed) {
                    failed = Some("Установка сборки отменена.".to_string());
                    break;
                }
                match cancellable(response.chunk(), cancel).await {
                    Ok(Some(chunk)) => {
                        total = total.saturating_add(chunk.len() as u64);
                        if total > max_bytes || expected_size.is_some_and(|size| total > size) {
                            failed = Some("Размер файла Modrinth превышает ожидаемый.".into());
                            break;
                        }
                        sha1.update(&chunk);
                        sha512.update(&chunk);
                        if let Err(error) = file.write_all(&chunk).await {
                            failed = Some(error.to_string());
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        if error == "Установка сборки отменена." {
                            failed = Some(error);
                            break;
                        }
                        failed = Some(error.to_string());
                        break;
                    }
                }
            }
            if failed.is_none() {
                if let Err(error) = file.flush().await {
                    failed = Some(error.to_string());
                }
            }
            drop(file);
            if let Some(error) = failed {
                let _ = fs::remove_file(destination);
                if error == "Установка сборки отменена." {
                    return Err(error);
                }
                last_error = error;
                continue;
            }
            let actual_sha1 = format!("{:x}", sha1.finalize());
            let actual_sha512 = format!("{:x}", sha512.finalize());
            if expected_size.is_some_and(|size| size != total)
                || !verified_file_hashes_match(&actual_sha1, &actual_sha512, expected_hashes)
            {
                let _ = fs::remove_file(destination);
                last_error = "Контрольная сумма или размер файла Modrinth не совпали.".into();
                continue;
            }
            return Ok(total);
        }
    }
    let _ = fs::remove_file(destination);
    Err(last_error)
}

fn emit_progress(app: &AppHandle, completed: usize, total: usize, current_file: impl Into<String>) {
    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            completed,
            total,
            current_file: current_file.into(),
        },
    );
}

fn pack_dependencies(index: &MrpackIndex) -> Result<(String, String, Option<String>), String> {
    if index.format_version != 1 || index.game != "minecraft" {
        return Err("Формат этой Modrinth-сборки не поддерживается.".into());
    }
    if index.version_id.trim().is_empty()
        || index.version_id.len() > 128
        || index.version_id.chars().any(char::is_control)
    {
        return Err("В манифесте указана некорректная версия сборки.".into());
    }
    let minecraft_version = index
        .dependencies
        .get("minecraft")
        .filter(|version| valid_component(version))
        .cloned()
        .ok_or("В манифесте сборки не указана корректная версия Minecraft.")?;
    if index.dependencies.keys().any(|dependency| {
        ![
            "minecraft",
            "fabric-loader",
            "forge",
            "neoforge",
            "quilt-loader",
        ]
        .contains(&dependency.as_str())
    }) {
        return Err("Сборка использует неизвестную зависимость или загрузчик.".into());
    }
    let loaders = [
        ("fabric", "fabric-loader"),
        ("forge", "forge"),
        ("neoforge", "neoforge"),
    ]
    .into_iter()
    .filter_map(|(loader, dependency)| {
        index
            .dependencies
            .get(dependency)
            .map(|version| (loader, version.clone()))
    })
    .collect::<Vec<_>>();
    if index.dependencies.contains_key("quilt-loader") {
        return Err("Загрузчик Quilt пока не поддерживается. Выберите сборку на Fabric, Forge или NeoForge.".into());
    }
    if loaders.len() > 1 {
        return Err("Манифест сборки содержит несколько загрузчиков.".into());
    }
    let (loader_type, loader_version) = loaders.into_iter().next().map_or_else(
        || ("vanilla".to_string(), None),
        |(loader, version)| (loader.to_string(), Some(version)),
    );
    if loader_version
        .as_deref()
        .is_some_and(|version| !valid_component(version))
    {
        return Err("В манифесте указана некорректная версия загрузчика.".into());
    }
    Ok((minecraft_version, loader_type, loader_version))
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-_+".contains(&byte))
}

fn read_pack_index(archive: &mut ZipArchive<fs::File>) -> Result<MrpackIndex, String> {
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err("Архив сборки содержит слишком много записей.".into());
    }
    let entry = archive
        .by_name("modrinth.index.json")
        .map_err(|_| "В архиве отсутствует modrinth.index.json.")?;
    if entry.size() > MAX_INDEX_BYTES {
        return Err("Манифест сборки превышает допустимый размер.".into());
    }
    let mut contents = Vec::with_capacity(entry.size() as usize);
    entry
        .take(MAX_INDEX_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|error| error.to_string())?;
    if contents.len() as u64 > MAX_INDEX_BYTES {
        return Err("Манифест сборки превышает допустимый размер.".into());
    }
    serde_json::from_slice(&contents)
        .map_err(|error| format!("Некорректный манифест сборки: {error}"))
}

fn validate_archive_entries(archive: &mut ZipArchive<fs::File>) -> Result<(), String> {
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err("Архив сборки содержит слишком много записей.".into());
    }
    let cancel = AtomicBool::new(false);
    let mut total_size = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        let entry_size = copy_limited(
            &mut entry,
            &mut std::io::sink(),
            MAX_ARCHIVE_ENTRY_BYTES,
            &cancel,
        )?;
        if entry_size != entry.size() {
            return Err("Размер ZIP entry не совпадает с заявленным размером.".into());
        }
        total_size = total_size
            .checked_add(entry_size)
            .filter(|size| *size <= MAX_TOTAL_ARCHIVE_BYTES)
            .ok_or("Распакованный размер сборки превышает допустимый предел.")?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("Символические ссылки в архиве сборки запрещены.".into());
        }
        let entry_name = entry.name().trim_end_matches('/');
        if !entry_name.is_empty() {
            safe_pack_path(entry_name)?;
        }
    }
    Ok(())
}

fn copy_limited<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    max_bytes: u64,
    cancel: &AtomicBool,
) -> Result<u64, String> {
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Установка сборки отменена.".into());
        }
        let read = input.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            return Ok(total);
        }
        total = total
            .checked_add(read as u64)
            .filter(|size| *size <= max_bytes)
            .ok_or("Распакованный файл превышает допустимый размер.")?;
        output
            .write_all(&buffer[..read])
            .map_err(|error| error.to_string())?;
    }
}

fn apply_overrides(
    archive_path: &Path,
    instance_root: &Path,
    prefix: &str,
    cancel: &AtomicBool,
    total_override_bytes: &mut u64,
) -> Result<(), String> {
    let file = fs::File::open(archive_path).map_err(|error| error.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|error| error.to_string())?;
    for index in 0..archive.len() {
        if cancel.load(Ordering::Relaxed) {
            return Err("Установка сборки отменена.".into());
        }
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        let entry_name = entry.name().to_string();
        let Some(relative) = entry_name.strip_prefix(prefix) else {
            continue;
        };
        let relative = relative.trim_end_matches('/');
        if relative.is_empty() || entry.is_dir() {
            continue;
        }
        if entry.size() > MAX_FILE_BYTES {
            return Err("Файл overrides превышает допустимый размер.".into());
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("Символические ссылки в overrides запрещены.".into());
        }
        let target = safe_pack_destination(instance_root, relative)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let mut output = fs::File::create(target).map_err(|error| error.to_string())?;
        let copied = copy_limited(&mut entry, &mut output, MAX_FILE_BYTES, cancel)?;
        if copied != entry.size() {
            return Err("Размер файла overrides не совпал с ZIP-архивом.".into());
        }
        *total_override_bytes = total_override_bytes
            .checked_add(copied)
            .filter(|size| *size <= MAX_TOTAL_ARCHIVE_BYTES)
            .ok_or("Общий распакованный размер overrides превышает допустимый предел.")?;
        output.flush().map_err(|error| error.to_string())?;
    }
    Ok(())
}

async fn install_modpack_inner(
    app: AppHandle,
    folder_name: String,
    project_id: String,
    version_id: String,
    cancel: Arc<AtomicBool>,
    instance_created: Arc<AtomicBool>,
) -> Result<ModpackInstallResult, String> {
    let api_client = modrinth_client()?;
    let download_client = modpack_download_client()?;
    let project: ApiProject = cancellable(
        async {
            let response = api_client
                .get(format!("https://api.modrinth.com/v2/project/{project_id}"))
                .send()
                .await
                .map_err(|error| error.to_string())?
                .error_for_status()
                .map_err(|error| error.to_string())?;
            crate::modrinth::bounded_json(response).await
        },
        &cancel,
    )
    .await?;
    if project.project_type != "modpack" {
        return Err("Проект Modrinth не является сборкой.".into());
    }
    let versions: Vec<ApiVersion> = cancellable(
        async {
            let response = api_client
                .get(format!(
                    "https://api.modrinth.com/v2/project/{project_id}/version"
                ))
                .send()
                .await
                .map_err(|error| error.to_string())?
                .error_for_status()
                .map_err(|error| error.to_string())?;
            crate::modrinth::bounded_json(response).await
        },
        &cancel,
    )
    .await?;
    let version = versions
        .into_iter()
        .find(|version| version.id == version_id && version.project_id == project_id)
        .ok_or("Выбранная версия сборки не найдена у этого проекта.")?;
    let pack_file = version
        .files
        .iter()
        .find(|file| file.primary && file.filename.to_ascii_lowercase().ends_with(".mrpack"))
        .or_else(|| {
            version
                .files
                .iter()
                .find(|file| file.filename.to_ascii_lowercase().ends_with(".mrpack"))
        })
        .ok_or("В выбранной версии нет файла .mrpack.")?;
    safe_modrinth_url(&pack_file.url)?;

    let root = crate::instances_root(&app)?;
    let staging = root.join(format!(
        ".mrpack-{}-{}.tmp",
        folder_name,
        uuid::Uuid::new_v4()
    ));
    fs::create_dir(&staging).map_err(|error| error.to_string())?;
    let _staging_guard = TemporaryDirectory::new(staging.clone());
    let archive_path = staging.join("pack.mrpack");
    emit_progress(&app, 0, 1, "Загрузка .mrpack");
    download_and_verify(
        &download_client,
        std::slice::from_ref(&pack_file.url),
        &archive_path,
        pack_file.size,
        &pack_file.hashes,
        MAX_PACK_BYTES,
        &cancel,
    )
    .await?;
    if cancel.load(Ordering::Relaxed) {
        return Err("Установка сборки отменена.".into());
    }
    emit_progress(&app, 1, 1, "Проверка .mrpack");

    let mut archive =
        ZipArchive::new(fs::File::open(&archive_path).map_err(|error| error.to_string())?)
            .map_err(|error| format!("Не удалось открыть ZIP-архив сборки: {error}"))?;
    validate_archive_entries(&mut archive)?;
    let index = read_pack_index(&mut archive)?;
    let (minecraft_version, loader_type, loader_version) = pack_dependencies(&index)?;
    for file in &index.files {
        validate_pack_file(file)?;
    }
    let unique_paths = index
        .files
        .iter()
        .map(|file| safe_pack_path(&file.path).map(|path| path.to_string_lossy().to_lowercase()))
        .collect::<Result<HashSet<_>, _>>()?;
    if unique_paths.len() != index.files.len() {
        return Err("Манифест сборки содержит повторяющиеся пути.".into());
    }
    if index.files.len() > MAX_ARCHIVE_ENTRIES {
        return Err("Сборка содержит слишком много файлов.".into());
    }
    let total_bytes = index.files.iter().try_fold(0_u64, |total, file| {
        total
            .checked_add(file.file_size)
            .filter(|size| *size <= MAX_TOTAL_FILE_BYTES)
            .ok_or_else(|| "Общий размер сборки превышает допустимый предел.".to_string())
    })?;
    let _ = total_bytes;

    crate::create_instance_directory(
        app.clone(),
        folder_name.clone(),
        minecraft_version.clone(),
        loader_type.clone(),
        loader_version.clone(),
    )?;
    instance_created.store(true, Ordering::Relaxed);
    if cancel.load(Ordering::Relaxed) {
        return Err("Установка сборки отменена.".into());
    }

    crate::minecraft::install_minecraft_version(
        app.clone(),
        folder_name.clone(),
        minecraft_version.clone(),
    )
    .await?;
    if cancel.load(Ordering::Relaxed) {
        return Err("Установка сборки отменена.".into());
    }
    let java_version = crate::minecraft::get_required_java_version(
        app.clone(),
        folder_name.clone(),
        minecraft_version.clone(),
    )?;
    let java_path = crate::java::install_java_runtime(app.clone(), java_version).await?;
    if cancel.load(Ordering::Relaxed) {
        return Err("Установка сборки отменена.".into());
    }
    if loader_type != "vanilla" {
        crate::minecraft::install_loader(
            app.clone(),
            folder_name.clone(),
            minecraft_version.clone(),
            loader_type.clone(),
            loader_version
                .clone()
                .ok_or("В манифесте отсутствует версия загрузчика.")?,
            java_path.clone(),
        )
        .await?;
        if cancel.load(Ordering::Relaxed) {
            return Err("Установка сборки отменена.".into());
        }
    }

    let instance_root = root.join(&folder_name);
    let total_downloads = index
        .files
        .iter()
        .filter(|file| client_file_supported(file))
        .count();
    let downloads_dir = staging.join("downloads");
    fs::create_dir(&downloads_dir).map_err(|error| error.to_string())?;
    let mut completed = 0;
    for (file_index, pack_file) in index.files.iter().enumerate() {
        if !client_file_supported(pack_file) {
            continue;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err("Установка сборки отменена.".into());
        }
        let target = safe_pack_destination(&instance_root, &pack_file.path)?;
        let temporary = downloads_dir.join(format!("{file_index}.part"));
        download_and_verify(
            &download_client,
            &pack_file.downloads,
            &temporary,
            Some(pack_file.file_size),
            &pack_file.hashes,
            MAX_FILE_BYTES,
            &cancel,
        )
        .await
        .map_err(|error| format!("{}: {error}", pack_file.path))?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        if target.exists() {
            fs::remove_file(&target).map_err(|error| error.to_string())?;
        }
        fs::rename(&temporary, &target).map_err(|error| error.to_string())?;
        completed += 1;
        emit_progress(&app, completed, total_downloads.max(1), &pack_file.path);
    }

    let mut total_override_bytes = 0;
    apply_overrides(
        &archive_path,
        &instance_root,
        "overrides/",
        &cancel,
        &mut total_override_bytes,
    )?;
    apply_overrides(
        &archive_path,
        &instance_root,
        "client-overrides/",
        &cancel,
        &mut total_override_bytes,
    )?;
    if cancel.load(Ordering::Relaxed) {
        return Err("Установка сборки отменена.".into());
    }

    let metadata_path = instance_root.join(".launcher_meta.json");
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(&metadata_path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    metadata["modpack_project_id"] = serde_json::Value::String(project_id.clone());
    metadata["modpack_version_id"] = serde_json::Value::String(version_id.clone());
    metadata["modpack_version_number"] = serde_json::Value::String(version.version_number.clone());
    metadata["modpack_title"] = serde_json::Value::String(project.title);
    metadata["modpack_name"] = serde_json::Value::String(index.name);
    crate::fsutil::write_file_with_backup(
        &metadata_path,
        &serde_json::to_vec_pretty(&metadata).map_err(|error| error.to_string())?,
    )?;
    if cancel.load(Ordering::Relaxed) {
        return Err("Установка сборки отменена.".into());
    }
    emit_progress(
        &app,
        total_downloads.max(1),
        total_downloads.max(1),
        "Сборка установлена",
    );
    Ok(ModpackInstallResult {
        minecraft_version,
        loader_type,
        loader_version,
        java_version,
        java_path,
        modpack_project_id: project_id,
        modpack_version_id: version_id,
        modpack_version_number: version.version_number,
    })
}

#[tauri::command]
pub(crate) async fn search_modpacks(
    query: String,
    minecraft_versions: Vec<String>,
    loaders: Vec<String>,
    index: String,
    offset: usize,
) -> Result<ModpackSearchResults, String> {
    if query.chars().count() > 128
        || minecraft_versions.len() > 20
        || loaders.len() > 4
        || loaders
            .iter()
            .any(|loader| !["fabric", "forge", "neoforge", "quilt"].contains(&loader.as_str()))
        || !["downloads", "relevance"].contains(&index.as_str())
        || offset > 1_000_000
    {
        return Err("Параметры поиска сборок некорректны.".into());
    }
    let valid_version = |version: &str| {
        !version.is_empty()
            && version.len() <= 32
            && version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
    };
    if minecraft_versions
        .iter()
        .any(|version| !valid_version(version))
    {
        return Err("Выбрана некорректная версия Minecraft.".into());
    }
    let mut facets = vec![vec!["project_type:modpack".to_string()]];
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
    let facets = serde_json::to_string(&facets).map_err(|error| error.to_string())?;
    let offset = offset.to_string();
    let response = modrinth_client()?
        .get("https://api.modrinth.com/v2/search")
        .query(&[
            ("query", query.trim()),
            ("facets", facets.as_str()),
            ("index", index.as_str()),
            ("limit", "20"),
            ("offset", offset.as_str()),
        ])
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    let response: SearchResponse = crate::modrinth::bounded_json(response).await?;
    let hits = response
        .hits
        .into_iter()
        .filter_map(|project| map_project(project).ok())
        .collect();
    Ok(ModpackSearchResults {
        hits,
        total_hits: response.total_hits,
    })
}

#[tauri::command]
pub(crate) async fn get_modpack_versions(project_id: String) -> Result<ModpackDetails, String> {
    checked_project_id(&project_id)?;
    let client = modrinth_client()?;
    let project_url = format!("https://api.modrinth.com/v2/project/{project_id}");
    let versions_url = format!("https://api.modrinth.com/v2/project/{project_id}/version");
    let (project, versions) = futures::try_join!(
        async {
            client
                .get(project_url)
                .send()
                .await
                .map_err(|error| error.to_string())?
                .error_for_status()
                .map_err(|error| error.to_string())?
                .json::<ApiProject>()
                .await
                .map_err(|error| error.to_string())
        },
        async {
            client
                .get(versions_url)
                .send()
                .await
                .map_err(|error| error.to_string())?
                .error_for_status()
                .map_err(|error| error.to_string())?
                .json::<Vec<ApiVersion>>()
                .await
                .map_err(|error| error.to_string())
        }
    )?;
    let versions = versions
        .into_iter()
        .filter_map(|version| {
            let file = version
                .files
                .iter()
                .find(|file| {
                    file.primary && file.filename.to_ascii_lowercase().ends_with(".mrpack")
                })
                .or_else(|| {
                    version
                        .files
                        .iter()
                        .find(|file| file.filename.to_ascii_lowercase().ends_with(".mrpack"))
                })?;
            Some(ModpackVersion {
                version_id: version.id,
                version_number: version.version_number,
                name: version.name,
                game_versions: version.game_versions,
                loaders: version.loaders,
                file_name: file.filename.clone(),
                date_published: version.date_published,
            })
        })
        .collect();
    Ok(ModpackDetails {
        project: map_project(project)?,
        versions,
    })
}

#[tauri::command]
pub(crate) async fn install_modpack(
    app: AppHandle,
    folder_name: String,
    project_id: String,
    version_id: String,
) -> Result<ModpackInstallResult, String> {
    crate::validate_folder_name(&folder_name)?;
    let _instance_lock = crate::locks::InstanceLock::acquire(&folder_name)?;
    checked_project_id(&project_id)?;
    if version_id.is_empty()
        || version_id.len() > 64
        || !version_id.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err("Некорректный идентификатор версии Modrinth.".into());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut active = cancellations()
            .lock()
            .map_err(|_| "Не удалось проверить задачи установки.".to_string())?;
        if active.contains_key(&folder_name) {
            return Err("Для этого имени уже выполняется установка сборки.".into());
        }
        active.insert(folder_name.clone(), Arc::clone(&cancel));
    }
    let _registration = CancellationRegistration {
        folder_name: folder_name.clone(),
    };
    let instance_created = Arc::new(AtomicBool::new(false));
    let result = install_modpack_inner(
        app.clone(),
        folder_name.clone(),
        project_id,
        version_id,
        Arc::clone(&cancel),
        Arc::clone(&instance_created),
    )
    .await;
    if result.is_err() && instance_created.load(Ordering::Relaxed) {
        let _ = crate::delete_instance_directory(app, folder_name);
    }
    result
}

#[tauri::command]
pub(crate) fn cancel_modpack_install(folder_name: String) -> Result<(), String> {
    crate::validate_folder_name(&folder_name)?;
    let active = cancellations()
        .lock()
        .map_err(|_| "Не удалось получить состояние установки сборки.".to_string())?;
    let cancel = active
        .get(&folder_name)
        .ok_or("Установка сборки не найдена.")?;
    cancel.store(true, Ordering::Relaxed);
    Ok(())
}

fn is_local_repository_host(host: &str) -> bool {
    let domain = host.strip_suffix('.').unwrap_or(host);
    if domain.eq_ignore_ascii_case("localhost") || domain.to_ascii_lowercase().ends_with(".local") {
        return true;
    }
    let address = host.trim_start_matches('[').trim_end_matches(']');
    address.parse::<IpAddr>().is_ok_and(|address| match address {
        IpAddr::V4(address) => address.is_private() || address.is_loopback() || address.is_link_local(),
        IpAddr::V6(address) => address.is_loopback() || address.is_unique_local() || address.is_unicast_link_local(),
    })
}

fn repository_url(value: &str) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(value.trim()).map_err(|error| error.to_string())?;
    let local_host = url.host_str().is_some_and(is_local_repository_host);
    if (url.scheme() != "https" && !(url.scheme() == "http" && local_host))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Адрес репозитория должен быть HTTPS URL (или HTTP адресом локальной сети) без учётных данных и query.".into());
    }
    if !url.path().ends_with('/') { url.set_path(&format!("{}/", url.path())); }
    Ok(url)
}

fn repository_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("MyLauncher/0.1.0")
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(600))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())
}

    fn repository_api_client() -> Result<reqwest::Client, String> {
        reqwest::Client::builder()
        .user_agent("MyLauncher/0.1.0")
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())
    }

async fn repository_json<T: DeserializeOwned>(
    mut response: reqwest::Response,
    context: &str,
) -> Result<T, String> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_REPOSITORY_JSON_BYTES as u64)
    {
        return Err(format!("{context}: ответ превышает допустимый размер."));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("{context}: {error}"))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_REPOSITORY_JSON_BYTES {
            return Err(format!("{context}: ответ превышает допустимый размер."));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|error| format!("{context}: {error}"))
}

fn repository_manifest_url(
    base: &reqwest::Url,
    pack_id: &str,
    version: &str,
    channel: &str,
) -> Result<reqwest::Url, String> {
    if !matches!(channel, "stable" | "beta") {
        return Err("Неизвестный канал репозитория.".into());
    }
    let mut url = base
        .join(&format!("api/packs/{pack_id}/versions/{version}/manifest"))
        .map_err(|error| error.to_string())?;
    url.query_pairs_mut().append_pair("channel", channel);
    Ok(url)
}

fn checked_repository_pack_id(value: &str) -> Result<(), String> {
    if value.len() < 3
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
    {
        return Err("Некорректный идентификатор сборки.".into());
    }
    Ok(())
}

fn safe_repository_path(value: &str) -> Result<PathBuf, String> {
    let path = safe_pack_path(value)?;
    if !value.chars().all(|character| character.is_ascii_alphanumeric() || " _./+@()-§".contains(character)) {
        return Err("Путь содержит символы, не поддерживаемые форматом манифеста.".into());
    }
    let extension = Path::new(value).extension().and_then(|value| value.to_str()).unwrap_or_default().to_ascii_lowercase();
    let filename = Path::new(value).file_name().and_then(|value| value.to_str()).unwrap_or_default();
    if filename != ".defaults" && ![
        "jar", "zip", "json", "json5", "toml", "bak", "backup", "cfg", "conf", "properties", "txt", "md", "png", "jpg", "jpeg", "webp",
        "mcmeta", "dat", "nbt", "snbt", "mcfunction", "vsh", "fsh", "glsl", "lang", "yml", "yaml", "xml", "ini",
    ].contains(&extension.as_str()) {
        return Err("Тип файла не разрешён политикой репозитория.".into());
    }
    Ok(path)
}

#[tauri::command]
pub(crate) async fn list_repository_packs(
    repository: String,
    channel: String,
) -> Result<Vec<RepositoryPackSummary>, String> {
    if !matches!(channel.as_str(), "stable" | "beta") {
        return Err("Неизвестный канал репозитория.".into());
    }
    let base = repository_url(&repository)?;
    let mut url = base.join("api/packs").map_err(|error| error.to_string())?;
    url.query_pairs_mut().append_pair("channel", &channel);
    let response: RepositoryPackList = repository_json(repository_get(&repository_api_client()?, url, None)
        .await
        .map_err(|error| format!("Ошибка каталога репозитория: {error}"))?,
        "Не удалось прочитать каталог репозитория",
    )
    .await?;
    Ok(response.packs)
}

#[tauri::command]
pub(crate) async fn list_repository_versions(
    repository: String,
    pack_id: String,
    channel: String,
) -> Result<Vec<RepositoryVersionSummary>, String> {
    checked_repository_pack_id(&pack_id)?;
    let base = repository_url(&repository)?;
    if !matches!(channel.as_str(), "stable" | "beta") {
        return Err("Неизвестный канал репозитория.".into());
    }
    let mut url = base.join(&format!("api/packs/{pack_id}/versions")).map_err(|error| error.to_string())?;
    url.query_pairs_mut().append_pair("channel", &channel);
    let response: RepositoryVersions = repository_json(repository_get(&repository_api_client()?, url, None)
        .await
        .map_err(|error| format!("Ошибка списка версий: {error}"))?, "Не удалось прочитать версии сборки")
        .await?;
    Ok(response.versions)
}

async fn download_repository_file(
    client: &reqwest::Client,
    base: &reqwest::Url,
    file: &RepositoryFile,
    pack_id: &str,
    version: &str,
    channel: &str,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    safe_repository_path(&file.path)?;
    if !repository_file_size_supported(file.size)
        || !file.hash.starts_with("sha256:")
        || file.hash.len() != 71
        || !file.hash.is_ascii()
    {
        return Err(format!("Некорректные данные файла {}.", file.path));
    }
    let expected_hash = &file.hash[7..];
    if !expected_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        || file.url != format!("/api/files/{}", file.hash)
    {
        return Err(format!("Некорректная ссылка или SHA-256 файла {}.", file.path));
    }
    let mut url = base.join(file.url.trim_start_matches('/')).map_err(|error| error.to_string())?;
    if url.scheme() != base.scheme() || url.host_str() != base.host_str() || url.port() != base.port() {
        return Err("Файл сборки ссылается на другой домен.".into());
    }
    url.query_pairs_mut()
        .append_pair("packId", pack_id)
        .append_pair("version", version)
        .append_pair("channel", channel);
    if cancel.load(Ordering::Relaxed) {
        return Err("Установка сборки отменена.".into());
    }
    for attempt in 0..3 {
        if cancel.load(Ordering::Relaxed) { return Err("Установка сборки отменена.".into()); }
        let mut size = fs::metadata(destination).map(|metadata| metadata.len()).unwrap_or(0);
        if size > file.size { fs::remove_file(destination).map_err(|error| error.to_string())?; size = 0; }
        let mut digest = Sha256::new();
        if size > 0 {
            let mut input = fs::File::open(destination).map_err(|error| error.to_string())?;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let count = input.read(&mut buffer).map_err(|error| error.to_string())?;
                if count == 0 { break; }
                digest.update(&buffer[..count]);
            }
            if size == file.size {
                if format!("{:x}", digest.finalize()) == expected_hash { return Ok(()); }
                fs::remove_file(destination).map_err(|error| error.to_string())?;
                size = 0;
                digest = Sha256::new();
            }
        }
        let mut request = client.get(url.clone());
        if size > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={size}-"))
                .header(reqwest::header::IF_RANGE, format!("\"sha256-{expected_hash}\""));
        }
        let response = match cancellable(request.send(), cancel).await {
            Ok(response) => response,
            Err(_error) if !cancel.load(Ordering::Relaxed) && attempt < 2 => {
                wait_for_repository_retry(attempt, None, Some(cancel)).await?;
                continue;
            }
            Err(error) => return Err(error),
        };
        if is_retryable_repository_status(response.status()) && attempt < 2 {
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            wait_for_repository_retry(attempt, retry_after.as_deref(), Some(cancel)).await?;
            continue;
        }
        let mut response = if size > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
            let expected_prefix = format!("bytes {size}-");
            if !response.headers().get(reqwest::header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok()).is_some_and(|value| value.starts_with(&expected_prefix)) {
                return Err(format!("Некорректный Content-Range: {}.", file.path));
            }
            response
        } else {
            size = 0;
            digest = Sha256::new();
            response
        };
        response.error_for_status_ref().map_err(|error| error.to_string())?;
        if response.content_length().is_some_and(|length| length != file.size - size) {
            return Err(format!("Размер ответа не совпал: {}.", file.path));
        }
        let mut output = tokio::fs::OpenOptions::new().create(true).write(true)
            .append(size > 0).truncate(size == 0).open(destination).await
            .map_err(|error| error.to_string())?;
        let mut retry = false;
        loop {
            if cancel.load(Ordering::Relaxed) { return Err("Установка сборки отменена.".into()); }
            let chunk = match cancellable(response.chunk(), cancel).await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(_error) if !cancel.load(Ordering::Relaxed) && attempt < 2 => { retry = true; break; }
                Err(error) => return Err(error),
            };
            size = size.checked_add(chunk.len() as u64).filter(|size| *size <= file.size)
                .ok_or_else(|| format!("Скачанный файл превышает размер манифеста: {}.", file.path))?;
            digest.update(&chunk);
            output.write_all(&chunk).await.map_err(|error| error.to_string())?;
        }
        output.flush().await.map_err(|error| error.to_string())?;
        if retry {
            drop(output);
            wait_for_repository_retry(attempt, None, Some(cancel)).await?;
            continue;
        }
        if size != file.size || format!("{:x}", digest.finalize()) != expected_hash {
            let _ = fs::remove_file(destination);
            return Err(format!("Хэш или размер файла не совпал: {}.", file.path));
        }
        return Ok(());
    }
    Err(format!("Не удалось докачать файл после нескольких попыток: {}.", file.path))
}

fn repository_download_cache(root: &Path, file: &RepositoryFile) -> Result<PathBuf, String> {
    let digest = file.hash.strip_prefix("sha256:").ok_or("Некорректный SHA-256 файла")?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Некорректный SHA-256 файла".into());
    }
    let directory = root.join(".repository-download-cache");
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let path_key = format!("{:x}", Sha256::digest(file.path.as_bytes()));
    Ok(directory.join(format!("{digest}-{path_key}.part")))
}

fn stage_cached_download(cache_path: &Path, destination: &Path) -> Result<(), String> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::copy(cache_path, destination)
        .map_err(|error| error.to_string())
        .map(|_| ())
}

fn local_file_matches(path: &Path, file: &RepositoryFile) -> Result<bool, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != file.size {
        return Ok(false);
    }
    let mut input = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 { break; }
        digest.update(&buffer[..count]);
    }
    Ok(format!("sha256:{:x}", digest.finalize()) == file.hash)
}

fn preserve_existing_optional_files(
    files: &[RepositoryFile],
    old_files: &HashMap<String, &RepositoryFile>,
    instance_root: &Path,
    installed_indices: &mut HashSet<usize>,
) -> Result<(), String> {
    for (index, file) in files.iter().enumerate() {
        let path_key = file.path.to_ascii_lowercase();
        if file.required || installed_indices.contains(&index) || !old_files.contains_key(&path_key) {
            continue;
        }
        if safe_pack_destination(instance_root, &file.path)?.is_file() {
            installed_indices.insert(index);
        }
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn update_repository_pack(
    app: AppHandle,
    folder_name: String,
    repository: String,
    pack_id: String,
    version: String,
    channel: String,
) -> Result<RepositoryInstallResult, String> {
    crate::validate_folder_name(&folder_name)?;
    let _instance_lock = crate::locks::InstanceLock::acquire(&folder_name)?;
    checked_repository_pack_id(&pack_id)?;
    if !valid_component(&version) { return Err("Некорректная версия сборки.".into()); }
    let base = repository_url(&repository)?;
    let root = crate::instances_root(&app)?;
    let instance_root = root.join(&folder_name);
    let metadata_path = instance_root.join(".launcher_meta.json");
    let mut metadata: serde_json::Value = serde_json::from_slice(&fs::read(&metadata_path).map_err(|_| "Инстанс не содержит метаданные репозитория.".to_string())?)
        .map_err(|error| error.to_string())?;
    let stored_repository = metadata.get("repository_url").and_then(serde_json::Value::as_str)
        .and_then(|value| repository_url(value).ok());
    if metadata.get("repository_pack_id").and_then(serde_json::Value::as_str) != Some(pack_id.as_str())
        || stored_repository.as_ref() != Some(&base)
    {
        return Err("Этот инстанс не принадлежит указанному репозиторию и сборке.".into());
    }
    let old_files: Vec<RepositoryFile> = serde_json::from_value(metadata.get("repository_manifest").cloned().unwrap_or_default())
        .map_err(|error| format!("Не удалось прочитать состав установленной сборки: {error}"))?;
    let manifest_url = repository_manifest_url(&base, &pack_id, &version, &channel)?;
    let api_client = repository_api_client()?;
    let client = repository_client()?;
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut active = cancellations().lock().map_err(|_| "Не удалось проверить задачи установки.".to_string())?;
        if active.contains_key(&folder_name) { return Err("Для этого инстанса уже выполняется обновление сборки.".into()); }
        active.insert(folder_name.clone(), Arc::clone(&cancel));
    }
    let _registration = CancellationRegistration { folder_name: folder_name.clone() };
    let result = async {
        let response = repository_get(&api_client, manifest_url, Some(&cancel)).await
            .map_err(|error| format!("Версия сборки не найдена: {error}"))?;
        let manifest: RepositoryManifest = repository_json(response, "Некорректный манифест сборки").await?;
        if manifest.manifest_version != 1 || manifest.pack_id != pack_id || manifest.version != version {
            return Err("Версия или формат манифеста не совпадают с запросом.".into());
        }
        if !valid_component(&manifest.minecraft.version)
            || manifest.java.major < 8
            || manifest.files.len() > MAX_ARCHIVE_ENTRIES
        {
            return Err("Манифест содержит неподдерживаемые параметры Minecraft, Java или список файлов.".into());
        }
        crate::validate_loader_selection(&manifest.minecraft.loader, manifest.minecraft.loader_version.as_deref())?;
        let old_minecraft = metadata.get("minecraft_version").and_then(serde_json::Value::as_str);
        let old_loader = metadata.get("loader_type").and_then(serde_json::Value::as_str);
        let old_loader_version = metadata.get("loader_version").and_then(serde_json::Value::as_str);
        if old_minecraft != Some(manifest.minecraft.version.as_str())
            || old_loader != Some(manifest.minecraft.loader.as_str())
            || old_loader_version != manifest.minecraft.loader_version.as_deref()
        {
            return Err("Версия Minecraft или загрузчика изменилась. Установите эту версию как новый инстанс, чтобы сохранить текущую установку.".into());
        }
        let required_java = crate::minecraft::get_required_java_version(app.clone(), folder_name.clone(), manifest.minecraft.version.clone())?;
        if manifest.java.major != required_java {
            return Err(format!("Манифест требует Java {}, но Minecraft {} требует Java {}.", manifest.java.major, manifest.minecraft.version, required_java));
        }
        if manifest.files.len() > MAX_ARCHIVE_ENTRIES { return Err("Сборка содержит слишком много файлов.".into()); }
        manifest.files.iter().try_fold(0_u64, |total, file| {
            total.checked_add(file.size).filter(|size| *size <= MAX_TOTAL_FILE_BYTES)
                .ok_or_else(|| "Общий размер сборки превышает допустимый предел.".to_string())
        })?;
        let mut old_by_path = HashMap::new();
        for file in &old_files { old_by_path.insert(file.path.to_ascii_lowercase(), file); }
        let mut new_paths = HashSet::new();
        for file in &manifest.files {
            safe_repository_path(&file.path)?;
            if !new_paths.insert(file.path.to_ascii_lowercase()) { return Err("Манифест содержит повторяющиеся пути.".into()); }
            let target = safe_pack_destination(&instance_root, &file.path)?;
            if target.exists() && !old_by_path.contains_key(&file.path.to_ascii_lowercase()) {
                return Err(format!("Путь {} занят пользовательским файлом. Переместите его перед обновлением.", file.path));
            }
        }
        let transaction_id = format!(".repo-update-{}.tmp", uuid::Uuid::new_v4());
        let staging = root.join(&transaction_id);
        fs::create_dir(&staging).map_err(|error| error.to_string())?;
        let mut staging_guard = TemporaryDirectory::new(staging.clone());
        let mut installed_files = Vec::new();
        let mut staged_files = HashMap::<String, PathBuf>::new();
        let mut verified_existing = HashSet::new();
        let mut installed_indices = HashSet::new();
        let mut download_indices = Vec::new();
        let total_files = manifest.files.len().max(1);
        for (index, file) in manifest.files.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) { return Err("Установка сборки отменена.".into()); }
            let target = safe_pack_destination(&instance_root, &file.path)?;
            if old_by_path.get(&file.path.to_ascii_lowercase()).is_some_and(|old| old.hash == file.hash && old.size == file.size)
                && local_file_matches(&target, file)?
            {
                verified_existing.insert(file.path.to_ascii_lowercase());
                installed_indices.insert(index);
                emit_progress(&app, index + 1, total_files, format!("Уже проверен: {}", file.path));
                continue;
            }
            download_indices.push(index);
        }
        let completed_files = Arc::new(std::sync::atomic::AtomicUsize::new(installed_indices.len()));
        let downloaded = futures::stream::iter(download_indices)
            .map(|index| {
                let file = manifest.files[index].clone();
                let download_pack_id = pack_id.clone();
                let download_version = version.clone();
                let download_channel = channel.clone();
                let client = client.clone();
                let base = base.clone();
                let staging = staging.clone();
                let root = root.clone();
                let cancel = Arc::clone(&cancel);
                let app = app.clone();
                let completed_files = Arc::clone(&completed_files);
                async move {
                    if cancel.load(Ordering::Relaxed) { return Err("Установка сборки отменена.".into()); }
                    let cache_path = repository_download_cache(&root, &file)?;
                    let temporary = staging.join(format!("{index}.part"));
                    if let Err(error) = download_repository_file(&client, &base, &file, &download_pack_id, &download_version, &download_channel, &cache_path, &cancel).await {
                        if !file.required && error != "Установка сборки отменена." {
                            let count = completed_files.fetch_add(1, Ordering::Relaxed) + 1;
                            emit_progress(&app, count, total_files, format!("Пропущен optional: {}", file.path));
                            return Ok(None);
                        }
                        return Err(format!("{}: {error}", file.path));
                    }
                    stage_cached_download(&cache_path, &temporary)?;
                    let count = completed_files.fetch_add(1, Ordering::Relaxed) + 1;
                    emit_progress(&app, count, total_files, &file.path);
                    Ok(Some((index, temporary)))
                }
            })
            .buffer_unordered(8)
            .try_collect::<Vec<_>>()
            .await?;
        for (index, temporary) in downloaded.into_iter().flatten() {
            installed_indices.insert(index);
            staged_files.insert(manifest.files[index].path.to_ascii_lowercase(), temporary);
        }
        preserve_existing_optional_files(
            &manifest.files,
            &old_by_path,
            &instance_root,
            &mut installed_indices,
        )?;
        installed_files.extend(manifest.files.iter().enumerate()
            .filter(|(index, _)| installed_indices.contains(index))
            .map(|(_, file)| file));
        if cancel.load(Ordering::Relaxed) { return Err("Установка сборки отменена.".into()); }
        let backup = staging.join("backup");
        fs::create_dir(&backup).map_err(|error| error.to_string())?;
        let recovery_journal = UpdateRecoveryJournal {
            instance_folder: folder_name.clone(),
            old_files: old_files.iter().map(|file| file.path.clone()).collect(),
            staged_files: installed_files.iter().filter_map(|file| {
                staged_files.get(&file.path.to_ascii_lowercase()).and_then(|path| {
                    path.file_name().and_then(|name| name.to_str()).map(|temporary_name| StagedRecoveryFile {
                        path: file.path.clone(),
                        temporary_name: temporary_name.to_string(),
                    })
                })
            }).collect(),
        };
        let journal_path = staging.join("commit-journal.json");
        let mut journal_file = fs::File::create(&journal_path).map_err(|error| error.to_string())?;
        journal_file.write_all(&serde_json::to_vec(&recovery_journal).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        journal_file.sync_all().map_err(|error| error.to_string())?;
        let mut backed_up = Vec::<(PathBuf, PathBuf)>::new();
        let mut installed_paths = Vec::<PathBuf>::new();
        let old_metadata_backup = backup.join("launcher-meta.json");
        let mut metadata_backed_up = false;
        let commit_result: Result<(), String> = (|| {
            for old in &old_files {
                let keep = installed_files.iter().any(|file| file.path.eq_ignore_ascii_case(&old.path));
                if keep { continue; }
                let source = safe_pack_destination(&instance_root, &old.path)?;
                if !source.exists() { continue; }
                let backup_path = backup.join("files").join(safe_repository_path(&old.path)?);
                if let Some(parent) = backup_path.parent() { fs::create_dir_all(parent).map_err(|error| error.to_string())?; }
                fs::rename(&source, &backup_path).map_err(|error| error.to_string())?;
                backed_up.push((source, backup_path));
            }
            for file in &installed_files {
                if verified_existing.contains(&file.path.to_ascii_lowercase()) { continue; }
                let target = safe_pack_destination(&instance_root, &file.path)?;
                if target.exists() {
                    let backup_path = backup.join("files").join(safe_repository_path(&file.path)?);
                    if let Some(parent) = backup_path.parent() { fs::create_dir_all(parent).map_err(|error| error.to_string())?; }
                    fs::rename(&target, &backup_path).map_err(|error| error.to_string())?;
                    backed_up.push((target.clone(), backup_path));
                }
                let source = staged_files.remove(&file.path.to_ascii_lowercase())
                    .ok_or_else(|| format!("Не найден временный файл {}.", file.path))?;
                let target_parent = target.parent().ok_or("Некорректный путь сборки")?;
                fs::create_dir_all(target_parent).map_err(|error| error.to_string())?;
                fs::rename(&source, &target).map_err(|error| error.to_string())?;
                installed_paths.push(target);
            }
            metadata["repository_version"] = serde_json::Value::String(version.clone());
            metadata["repository_channel"] = serde_json::Value::String(channel.clone());
            metadata["repository_manifest"] = serde_json::to_value(&installed_files).map_err(|error| error.to_string())?;
            let metadata_temp = staging.join("launcher-meta.new.json");
            fs::write(&metadata_temp, serde_json::to_vec_pretty(&metadata).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
            fs::rename(&metadata_path, &old_metadata_backup).map_err(|error| error.to_string())?;
            metadata_backed_up = true;
            fs::rename(&metadata_temp, &metadata_path).map_err(|error| error.to_string())?;
            fs::File::create(staging.join("commit-complete"))
                .and_then(|file| file.sync_all())
                .map_err(|error| error.to_string())?;
            Ok(())
        })();
        if let Err(error) = commit_result {
            for path in installed_paths.iter().rev() { let _ = fs::remove_file(path); }
            for (original, backup_path) in backed_up.iter().rev() {
                if let Some(parent) = original.parent() { let _ = fs::create_dir_all(parent); }
                let _ = fs::rename(backup_path, original);
            }
            if metadata_backed_up { let _ = fs::rename(&old_metadata_backup, &metadata_path); }
            return Err(format!("Обновление отменено, исходные файлы восстановлены: {error}"));
        }
        staging_guard.keep = true;
        emit_progress(&app, manifest.files.len().max(1), manifest.files.len().max(1), "Сборка обновлена");
        Ok(RepositoryInstallResult {
            minecraft_version: manifest.minecraft.version,
            loader_type: manifest.minecraft.loader,
            loader_version: manifest.minecraft.loader_version,
            java_version: manifest.java.major,
            java_path: metadata.get("java_path").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(),
            pack_id,
            version,
            channel,
            name: manifest.name,
            repository_url: base.as_str().to_string(),
            memory_mb: manifest.settings.get("memoryMb").and_then(serde_json::Value::as_u64).map(|value| value.clamp(512, 65536) as u32),
            jvm_args: manifest.settings.get("jvmArgs").and_then(serde_json::Value::as_str).map(|value| value.chars().take(2000).collect()),
            window_width: manifest.settings.get("windowWidth").and_then(serde_json::Value::as_u64).map(|value| value.clamp(640, 7680) as u32),
            window_height: manifest.settings.get("windowHeight").and_then(serde_json::Value::as_u64).map(|value| value.clamp(480, 4320) as u32),
            fullscreen: manifest.settings.get("fullscreen").and_then(serde_json::Value::as_bool),
            transaction_id: Some(transaction_id),
        })
    }.await;
    result
}

async fn install_repository_pack_inner(
    app: AppHandle,
    folder_name: String,
    staging_folder_name: String,
    repository: String,
    pack_id: String,
    version: String,
    channel: String,
    cancel: Arc<AtomicBool>,
    instance_created: Arc<AtomicBool>,
) -> Result<RepositoryInstallResult, String> {
    let base = repository_url(&repository)?;
    let manifest_url = repository_manifest_url(&base, &pack_id, &version, &channel)?;
    let api_client = repository_api_client()?;
    let client = repository_client()?;
    let response = repository_get(&api_client, manifest_url, Some(&cancel)).await
        .map_err(|error| format!("Версия сборки не найдена: {error}"))?;
    let manifest: RepositoryManifest = repository_json(response, "Некорректный манифест сборки").await?;
    if manifest.manifest_version != 1 || manifest.pack_id != pack_id || manifest.version != version {
        return Err("Версия или формат манифеста не совпадают с запросом.".into());
    }
    if !valid_component(&manifest.minecraft.version)
        || manifest.java.major < 8
        || manifest.files.len() > MAX_ARCHIVE_ENTRIES
    {
        return Err("Манифест содержит неподдерживаемые параметры Minecraft, Java или список файлов.".into());
    }
    crate::validate_loader_selection(&manifest.minecraft.loader, manifest.minecraft.loader_version.as_deref())?;
    let mut seen_paths = HashSet::new();
    let total_size = manifest.files.iter().try_fold(0_u64, |total, file| {
        safe_repository_path(&file.path)?;
        if !seen_paths.insert(file.path.to_ascii_lowercase()) {
            return Err("Манифест содержит повторяющиеся пути файлов.".to_string());
        }
        total.checked_add(file.size).filter(|size| *size <= MAX_TOTAL_FILE_BYTES)
            .ok_or_else(|| "Общий размер сборки превышает допустимый предел.".to_string())
    })?;
    let _ = total_size;
    let root = crate::instances_root(&app)?;
    let staging = root.join(format!(".repo-{}-{}.tmp", folder_name, uuid::Uuid::new_v4()));
    fs::create_dir(&staging).map_err(|error| error.to_string())?;
    let _staging_guard = TemporaryDirectory::new(staging.clone());
    let instance_destination = root.join(&folder_name);
    if instance_destination.exists() {
        return Err("Папка с таким именем уже существует.".into());
    }
    crate::create_instance_directory(
        app.clone(), staging_folder_name.clone(), manifest.minecraft.version.clone(),
        manifest.minecraft.loader.clone(), manifest.minecraft.loader_version.clone(),
    )?;
    instance_created.store(true, Ordering::Relaxed);
    if cancel.load(Ordering::Relaxed) { return Err("Установка сборки отменена.".into()); }
    crate::minecraft::install_minecraft_version(app.clone(), staging_folder_name.clone(), manifest.minecraft.version.clone()).await?;
    let java_version = crate::minecraft::get_required_java_version(app.clone(), staging_folder_name.clone(), manifest.minecraft.version.clone())?;
    if manifest.java.major != java_version {
        return Err(format!("Манифест указывает Java {}, но Minecraft {} требует Java {}.", manifest.java.major, manifest.minecraft.version, java_version));
    }
    let java_path = crate::java::install_java_runtime(app.clone(), java_version).await?;
    if cancel.load(Ordering::Relaxed) { return Err("Установка сборки отменена.".into()); }
    if manifest.minecraft.loader != "vanilla" {
        crate::minecraft::install_loader(app.clone(), staging_folder_name.clone(), manifest.minecraft.version.clone(),
            manifest.minecraft.loader.clone(), manifest.minecraft.loader_version.clone().ok_or("В манифесте отсутствует версия загрузчика.")?, java_path.clone()).await?;
    }
    let instance_root = root.join(&staging_folder_name);
    let total = manifest.files.len().max(1);
    let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let downloaded = futures::stream::iter(manifest.files.iter().cloned().enumerate())
        .map(|(index, file)| {
            let download_pack_id = pack_id.clone();
            let download_version = version.clone();
            let download_channel = channel.clone();
            let client = client.clone();
            let base = base.clone();
            let staging = staging.clone();
            let root = root.clone();
            let cancel = Arc::clone(&cancel);
            let app = app.clone();
            let completed = Arc::clone(&completed);
            async move {
                if cancel.load(Ordering::Relaxed) { return Err("Установка сборки отменена.".into()); }
                let cache_path = repository_download_cache(&root, &file)?;
                let temporary = staging.join(format!("{index}.part"));
                if let Err(error) = download_repository_file(&client, &base, &file, &download_pack_id, &download_version, &download_channel, &cache_path, &cancel).await {
                    if !file.required && error != "Установка сборки отменена." {
                        let count = completed.fetch_add(1, Ordering::Relaxed) + 1;
                        emit_progress(&app, count, total, format!("Пропущен optional: {}", file.path));
                        return Ok(None);
                    }
                    return Err(format!("{}: {error}", file.path));
                }
                stage_cached_download(&cache_path, &temporary)?;
                let count = completed.fetch_add(1, Ordering::Relaxed) + 1;
                emit_progress(&app, count, total, &file.path);
                Ok(Some((index, temporary)))
            }
        })
        .buffer_unordered(8)
        .try_collect::<Vec<_>>()
        .await?;
    let mut downloaded: Vec<_> = downloaded.into_iter().flatten().collect();
    downloaded.sort_by_key(|(index, _)| *index);
    for (index, temporary) in downloaded {
        if cancel.load(Ordering::Relaxed) { return Err("Установка сборки отменена.".into()); }
        let file = &manifest.files[index];
        let target = prepare_pack_destination(&instance_root, &file.path)?;
        if target.exists() { fs::remove_file(&target).map_err(|error| error.to_string())?; }
        fs::rename(&temporary, &target).map_err(|error| error.to_string())?;
    }
    let metadata_path = instance_root.join(".launcher_meta.json");
    let mut metadata: serde_json::Value = serde_json::from_slice(&fs::read(&metadata_path).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    metadata["name"] = serde_json::Value::String(folder_name.clone());
    metadata["repository_pack_name"] = serde_json::Value::String(manifest.name.clone());
    metadata["repository_description"] = serde_json::Value::String(manifest.description.clone());
    metadata["repository_authors"] = serde_json::to_value(&manifest.authors).map_err(|error| error.to_string())?;
    metadata["repository_categories"] = serde_json::to_value(&manifest.categories).map_err(|error| error.to_string())?;
    metadata["repository_url"] = serde_json::Value::String(base.as_str().to_string());
    metadata["repository_pack_id"] = serde_json::Value::String(pack_id.clone());
    metadata["repository_version"] = serde_json::Value::String(version.clone());
    metadata["repository_channel"] = serde_json::Value::String(channel.clone());
    metadata["repository_manifest"] = serde_json::to_value(&manifest.files).map_err(|error| error.to_string())?;
    crate::fsutil::write_file_with_backup(&metadata_path, &serde_json::to_vec_pretty(&metadata).map_err(|error| error.to_string())?)?;
    fs::rename(&instance_root, &instance_destination).map_err(|error| error.to_string())?;
    let settings = &manifest.settings;
    Ok(RepositoryInstallResult {
        minecraft_version: manifest.minecraft.version,
        loader_type: manifest.minecraft.loader,
        loader_version: manifest.minecraft.loader_version,
        java_version,
        java_path,
        pack_id,
        version,
        channel,
        name: manifest.name,
        repository_url: base.as_str().to_string(),
        memory_mb: settings.get("memoryMb").and_then(serde_json::Value::as_u64).map(|value| value.clamp(512, 65536) as u32),
        jvm_args: settings.get("jvmArgs").and_then(serde_json::Value::as_str).map(|value| value.chars().take(2000).collect()),
        window_width: settings.get("windowWidth").and_then(serde_json::Value::as_u64).map(|value| value.clamp(640, 7680) as u32),
        window_height: settings.get("windowHeight").and_then(serde_json::Value::as_u64).map(|value| value.clamp(480, 4320) as u32),
        fullscreen: settings.get("fullscreen").and_then(serde_json::Value::as_bool),
        transaction_id: None,
    })
}

#[tauri::command]
pub(crate) async fn install_repository_pack(
    app: AppHandle,
    folder_name: String,
    repository: String,
    pack_id: String,
    version: String,
    channel: String,
) -> Result<RepositoryInstallResult, String> {
    crate::validate_folder_name(&folder_name)?;
    let _instance_lock = crate::locks::InstanceLock::acquire(&folder_name)?;
    checked_repository_pack_id(&pack_id)?;
    if !valid_component(&version) { return Err("Некорректная версия сборки.".into()); }
    repository_url(&repository)?;
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut active = cancellations().lock().map_err(|_| "Не удалось проверить задачи установки.".to_string())?;
        if active.contains_key(&folder_name) { return Err("Для этого имени уже выполняется установка сборки.".into()); }
        active.insert(folder_name.clone(), Arc::clone(&cancel));
    }
    let _registration = CancellationRegistration { folder_name: folder_name.clone() };
    let staging_folder_name = format!(".repo-stage-{}", uuid::Uuid::new_v4());
    crate::validate_folder_name(&staging_folder_name)?;
    let instance_created = Arc::new(AtomicBool::new(false));
    let result = install_repository_pack_inner(app.clone(), folder_name, staging_folder_name.clone(), repository, pack_id, version, channel,
        Arc::clone(&cancel), Arc::clone(&instance_created)).await;
    if result.is_err() && instance_created.load(Ordering::Relaxed) {
        let _ = crate::delete_instance_directory(app, staging_folder_name);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{
        MrpackFile, MrpackIndex, SearchResponse, client_file_supported, copy_limited,
        checked_repository_pack_id, is_retryable_repository_status, map_project, pack_dependencies, repository_file_size_supported, repository_url,
        preserve_existing_optional_files, safe_pack_path, safe_repository_path, validate_pack_file,
        pending_update_instance_root, verified_file_hashes_match,
    };
    use std::{collections::HashMap, path::PathBuf};

    #[test]
    fn pack_paths_reject_traversal_absolute_and_windows_paths() {
        for path in [
            "../outside.jar",
            "mods/../../outside.jar",
            "/absolute/file.jar",
            "C:/outside.jar",
            "mods\\outside.jar",
            "//server/share/file.jar",
            "mods/CON.txt",
            "versions/1.20.1/1.20.1.json",
            "assets/indexes/index.json",
            "libraries/com/example/library.jar",
            ".launcher_meta.json",
            ".launcher_meta.json.tmp",
            ".launcher_meta.json.backup",
        ] {
            assert!(safe_pack_path(path).is_err(), "accepted {path}");
        }
        assert_eq!(
            safe_pack_path("mods/example.jar").unwrap(),
            PathBuf::from("mods/example.jar")
        );
    }

    #[test]
    fn interrupted_repository_update_restores_files_and_metadata() {
        use super::{StagedRecoveryFile, UpdateRecoveryJournal, cleanup_repository_staging};
        use std::fs;

        let root = std::env::current_dir().unwrap().join(format!("mylauncher-recovery-{}", uuid::Uuid::new_v4()));
        let instance = root.join("instance");
        let staging = root.join(format!(".repo-update-{}.tmp", uuid::Uuid::new_v4()));
        let file_path = instance.join("config/settings.toml");
        let metadata_path = instance.join(".launcher_meta.json");
        let backup_file = staging.join("backup/files/config/settings.toml");
        let backup_metadata = staging.join("backup/launcher-meta.json");
        fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        fs::create_dir_all(backup_file.parent().unwrap()).unwrap();
        fs::create_dir_all(backup_metadata.parent().unwrap()).unwrap();
        fs::write(&file_path, "new file").unwrap();
        fs::write(&metadata_path, "new metadata").unwrap();
        fs::write(&backup_file, "old file").unwrap();
        fs::write(&backup_metadata, "old metadata").unwrap();
        let journal = UpdateRecoveryJournal {
            instance_folder: "instance".into(),
            old_files: vec!["config/settings.toml".into()],
            staged_files: vec![StagedRecoveryFile {
                path: "config/settings.toml".into(),
                temporary_name: "0.part".into(),
            }],
        };
        fs::write(staging.join("commit-journal.json"), serde_json::to_vec(&journal).unwrap()).unwrap();

        cleanup_repository_staging(&root).unwrap();

        assert_eq!(fs::read_to_string(file_path).unwrap(), "old file");
        assert_eq!(fs::read_to_string(metadata_path).unwrap(), "old metadata");
        assert!(!staging.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_optional_update_keeps_an_existing_file_installed() {
        use super::RepositoryFile;
        use std::{collections::{HashMap, HashSet}, fs};

        let root = std::env::current_dir().unwrap().join(format!(".mylauncher-optional-update-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("mods")).unwrap();
        fs::write(root.join("mods/optional.jar"), b"previous version").unwrap();
        let old_file = RepositoryFile {
            path: "mods/optional.jar".into(),
            hash: "sha256:old".into(),
            size: 16,
            url: "https://example.invalid/old.jar".into(),
            required: false,
        };
        let old_files = HashMap::from([(old_file.path.to_ascii_lowercase(), &old_file)]);
        let files = [RepositoryFile {
            path: "mods/optional.jar".into(),
            hash: "sha256:new".into(),
            size: 12,
            url: "https://example.invalid/new.jar".into(),
            required: false,
        }];
        let mut installed_indices = HashSet::new();

        preserve_existing_optional_files(&files, &old_files, &root, &mut installed_indices).unwrap();

        assert!(installed_indices.contains(&0));
        assert_eq!(fs::read(root.join("mods/optional.jar")).unwrap(), b"previous version");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn safe_pack_destination_does_not_create_directories_during_validation() {
        use super::safe_pack_destination;
        use std::fs;

        let root = std::env::current_dir().unwrap().join(format!(".mylauncher-path-check-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();

        let destination = safe_pack_destination(&root, "config/nested/options.txt").unwrap();

        assert!(destination.ends_with(PathBuf::from("config/nested/options.txt")));
        assert!(!root.join("config").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepare_pack_destination_creates_nested_parent_directories() {
        use super::prepare_pack_destination;
        use std::fs;

        let root = std::env::current_dir().unwrap().join(format!(".mylauncher-pack-destination-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();

        let destination = prepare_pack_destination(&root, "config/nested/options.txt").unwrap();

        assert!(root.join("config/nested").is_dir());
        assert!(destination.ends_with(PathBuf::from("config/nested/options.txt")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pending_update_rejects_untrusted_instance_folder_names() {
        let root = PathBuf::from("instances");
        assert!(pending_update_instance_root(&root, "../outside").is_err());
        assert_eq!(
            pending_update_instance_root(&root, "instance-1").unwrap(),
            root.join("instance-1")
        );
    }

    #[test]
    fn repository_addresses_support_subpaths_and_local_http() {
        assert!(repository_url("https://packs.example.com").is_ok());
        assert!(repository_url("https://packs.example.com/admin").is_ok());
        assert!(repository_url("http://localhost:8787/repository/").is_ok());
        assert!(repository_url("http://192.168.1.20:8787/repository/").is_ok());
        assert!(repository_url("http://10.0.0.5:8787").is_ok());
        assert!(repository_url("http://172.31.0.8:8787").is_ok());
        assert!(repository_url("http://[fd12:3456::1]:8787").is_ok());
        assert!(repository_url("http://packs.local:8787").is_ok());
        for value in [
            "http://packs.example.com",
            "http://8.8.8.8",
            "https://user:pass@packs.example.com",
            "https://packs.example.com/?token=x",
        ] {
            assert!(repository_url(value).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn repository_retries_only_temporary_http_statuses() {
        for status in [
            reqwest::StatusCode::REQUEST_TIMEOUT,
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            reqwest::StatusCode::BAD_GATEWAY,
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            reqwest::StatusCode::GATEWAY_TIMEOUT,
        ] {
            assert!(is_retryable_repository_status(status), "status {status}");
        }
        for status in [
            reqwest::StatusCode::BAD_REQUEST,
            reqwest::StatusCode::UNAUTHORIZED,
            reqwest::StatusCode::NOT_FOUND,
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
        ] {
            assert!(!is_retryable_repository_status(status), "status {status}");
        }
    }

    #[test]
    fn repository_get_retries_503_and_returns_the_next_success() {
        use super::repository_get;
        use std::{
            io::{Read, Write},
            net::TcpListener,
            thread,
            time::{Duration, Instant},
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for attempt in 0..2 {
                let deadline = Instant::now() + Duration::from_secs(5);
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("expected retry request: {error}"),
                    }
                };
                let mut request = [0; 1024];
                stream.read(&mut request).unwrap();
                let response = if attempt == 0 {
                    "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                };
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let response = runtime.block_on(async {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap();
            repository_get(
                &client,
                reqwest::Url::parse(&format!("http://{address}/api/packs")).unwrap(),
                None,
            )
            .await
        });

        assert_eq!(response.unwrap().status(), reqwest::StatusCode::OK);
        server.join().unwrap();
    }

    #[test]
    fn repository_accepts_server_supported_file_sizes() {
        assert!(repository_file_size_supported(2 * 1024 * 1024 * 1024 + 1));
        assert!(repository_file_size_supported(8 * 1024 * 1024 * 1024));
        assert!(!repository_file_size_supported(8 * 1024 * 1024 * 1024 + 1));
    }

    #[test]
    fn repository_rejects_executable_paths_and_invalid_pack_ids() {
        assert!(safe_repository_path("mods/example.jar").is_ok());
        assert!(safe_repository_path("config/settings.toml").is_ok());
        assert!(safe_repository_path("config/.defaults").is_ok());
        assert!(safe_repository_path("config/overrides.json5").is_ok());
        assert!(safe_repository_path("config/overrides.snbt").is_ok());
        assert!(safe_repository_path("config/README.md").is_ok());
        assert!(safe_repository_path("config/options.toml.bak").is_ok());
        assert!(safe_repository_path("config/.axiom.json.backup").is_ok());
        assert!(safe_repository_path("resourcepacks/LowOnFire v26.2§8.zip").is_ok());
        assert!(safe_repository_path("mods/loader.exe").is_err());
        assert!(safe_repository_path("../outside.jar").is_err());
        assert!(checked_repository_pack_id("adventure-pack").is_ok());
        assert!(checked_repository_pack_id("../outside").is_err());
    }

    #[test]
    fn mrpack_dependencies_select_supported_loaders() {
        let index: MrpackIndex = serde_json::from_value(serde_json::json!({
            "formatVersion": 1,
            "game": "minecraft",
            "versionId": "15.0.0-beta.2",
            "name": "Pack",
            "files": [],
            "dependencies": {
                "minecraft": "1.20.1",
                "fabric-loader": "0.15.11"
            }
        }))
        .unwrap();
        assert_eq!(
            pack_dependencies(&index).unwrap(),
            ("1.20.1".into(), "fabric".into(), Some("0.15.11".into()))
        );

        let forge_index = MrpackIndex {
            format_version: 1,
            game: "minecraft".into(),
            version_id: "forge-pack-1".into(),
            name: "Forge pack".into(),
            files: Vec::new(),
            dependencies: HashMap::from([
                ("minecraft".into(), "1.20.1".into()),
                ("forge".into(), "47.2.0".into()),
            ]),
        };
        assert_eq!(
            pack_dependencies(&forge_index).unwrap(),
            ("1.20.1".into(), "forge".into(), Some("47.2.0".into()))
        );
    }

    #[test]
    fn parses_standard_modrinth_index_and_skips_client_unsupported_files() {
        let index: MrpackIndex = serde_json::from_value(serde_json::json!({
            "formatVersion": 1,
            "game": "minecraft",
            "versionId": "pack-1",
            "name": "Pack",
            "files": [{
                "path": "mods/server-only.jar",
                "hashes": {"sha1": "abc"},
                "env": {"client": "unsupported"},
                "downloads": ["https://cdn.modrinth.com/test.jar"],
                "fileSize": 42
            }],
            "dependencies": {"minecraft": "1.20.1"}
        }))
        .unwrap();
        assert_eq!(index.format_version, 1);
        assert!(!client_file_supported(&index.files[0]));
    }

    #[test]
    fn decodes_modrinth_search_hit_project_id_field() {
        let response: SearchResponse = serde_json::from_value(serde_json::json!({
            "hits": [{
                "project_id": "AANobbMI",
                "project_type": "modpack",
                "slug": "example-pack",
                "title": "Example Pack",
                "description": "A test pack",
                "author": "author",
                "categories": ["technology"],
                "icon_url": null,
                "downloads": 1234
            }],
            "total_hits": 1
        }))
        .unwrap();
        let project = map_project(response.hits.into_iter().next().unwrap()).unwrap();
        assert_eq!(project.project_id, "AANobbMI");
        assert_eq!(project.title, "Example Pack");
    }

    #[test]
    fn validates_file_paths_hashes_and_download_hosts_before_install() {
        let valid_file = MrpackFile {
            path: "mods/example.jar".into(),
            hashes: HashMap::from([
                ("sha1".into(), "a".repeat(40)),
                ("sha512".into(), "b".repeat(128)),
            ]),
            env: HashMap::new(),
            downloads: vec!["https://cdn.modrinth.com/test.jar".into()],
            file_size: 42,
        };
        assert!(validate_pack_file(&valid_file).is_ok());

        let mut invalid_file = MrpackFile {
            path: "mods/example.jar".into(),
            hashes: valid_file.hashes.clone(),
            env: HashMap::new(),
            downloads: vec!["https://example.com/file.jar".into()],
            file_size: 42,
        };
        assert!(validate_pack_file(&invalid_file).is_err());
        invalid_file.downloads = valid_file.downloads.clone();
        invalid_file.hashes.remove("sha512");
        assert!(validate_pack_file(&invalid_file).is_err());
    }

    #[test]
    fn rejects_unsupported_mrpack_versions_and_quilt() {
        let mut dependencies = HashMap::from([("minecraft".into(), "1.20.1".into())]);
        dependencies.insert("quilt-loader".into(), "0.24.0".into());
        let index = MrpackIndex {
            format_version: 1,
            game: "minecraft".into(),
            version_id: "pack-1".into(),
            name: "Pack".into(),
            files: Vec::new(),
            dependencies,
        };
        assert!(pack_dependencies(&index).unwrap_err().contains("Quilt"));

        let unknown_index = MrpackIndex {
            format_version: 1,
            game: "minecraft".into(),
            version_id: "unknown-pack-1".into(),
            name: "Unknown loader pack".into(),
            files: Vec::new(),
            dependencies: HashMap::from([
                ("minecraft".into(), "1.20.1".into()),
                ("unknown-loader".into(), "1.0.0".into()),
            ]),
        };
        assert!(pack_dependencies(&unknown_index).is_err());
    }

    #[test]
    fn validates_manifest_hashes_for_available_algorithms() {
        let expected = HashMap::from([
            ("sha1".into(), "abc".into()),
            ("sha512".into(), "def".into()),
        ]);
        assert!(verified_file_hashes_match("abc", "def", &expected));
        assert!(!verified_file_hashes_match("bad", "def", &expected));
        assert!(!verified_file_hashes_match(
            "abc",
            "def",
            &HashMap::from([("sha1".into(), "abc".into())]),
        ));
    }
}
