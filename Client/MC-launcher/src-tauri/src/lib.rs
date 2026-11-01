use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use sqlx::{Column, Row, TypeInfo, ValueRef, sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow}};
use std::{fs, path::Path, process::Command};
use tauri::{AppHandle, Manager};
use tokio::sync::OnceCell;

mod auth;
mod fsutil;
mod java;
mod launcher;
mod locks;
mod minecraft;
mod modrinth;
mod modpack;
mod process;
mod server;

use locks::InstanceLock;

static DATABASE_POOL: OnceCell<SqlitePool> = OnceCell::const_new();
const APPROVED_DATABASE_SELECTS: &[&str] = &[
    "PRAGMA user_version",
    "SELECT id, avatar_data FROM instances WHERE avatar_data LIKE 'data:image/%;base64,%'",
    "SELECT value FROM settings WHERE key = 'accent_palette'",
    "SELECT * FROM instances ORDER BY created_at DESC",
    "SELECT * FROM accounts ORDER BY created_at DESC",
    "SELECT value FROM settings WHERE key = 'active_account_id'",
    "SELECT value FROM settings WHERE key = 'theme'",
    "SELECT value FROM settings WHERE key = 'ui_palette'",
    "SELECT value FROM settings WHERE key = 'microsoft_client_id'",
    "SELECT COUNT(*) AS count FROM instances WHERE avatar_data = $1",
];
const APPROVED_DATABASE_MUTATIONS: &[&str] = &[
    "INSERT INTO accounts (id, username, uuid, account_type) VALUES ($1, $2, $3, $4)",
    "INSERT INTO accounts (id, username, uuid, account_type) VALUES ($1, $2, $3, $4) ON CONFLICT(id) DO UPDATE SET username = excluded.username, uuid = excluded.uuid, account_type = excluded.account_type",
    "DELETE FROM accounts WHERE id = $1",
    "INSERT INTO settings (key, value) VALUES ($1, $2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    "DELETE FROM settings WHERE key = 'active_account_id'",
    "UPDATE instances SET avatar_data = $1 WHERE id = $2",
    "PRAGMA user_version = 2",
    "INSERT INTO settings (key, value) VALUES ('theme', 'light') ON CONFLICT(key) DO NOTHING",
    "INSERT INTO settings (key, value) VALUES ('ui_palette', $1) ON CONFLICT(key) DO NOTHING",
    "INSERT INTO settings (key, value) VALUES ('microsoft_client_id', '') ON CONFLICT(key) DO NOTHING",
    "INSERT INTO instances (id, name, folder_name, minecraft_version, loader_type, loader_version, java_version, java_path, memory_mb, jvm_args, window_width, window_height, fullscreen, avatar_data, modpack_project_id, modpack_version_id, modpack_version_number, repository_url, repository_pack_id, repository_version, repository_channel) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21)",
    "INSERT INTO instances (id, name, folder_name, minecraft_version, loader_type, loader_version, java_version, java_path, memory_mb, jvm_args, window_width, window_height, fullscreen, avatar_data, modpack_project_id, modpack_version_id, modpack_version_number) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
    "UPDATE instances SET repository_version = $1, repository_channel = $2 WHERE id = $3",
    "UPDATE instances SET repository_version = $1, repository_channel = $2, memory_mb = COALESCE($3, memory_mb), jvm_args = COALESCE($4, jvm_args), window_width = COALESCE($5, window_width), window_height = COALESCE($6, window_height), fullscreen = COALESCE($7, fullscreen) WHERE id = $8",
    "UPDATE instances SET name = $1, folder_name = $2, minecraft_version = $3, loader_type = $4, loader_version = $5, java_version = $6, java_path = $7, memory_mb = $8, jvm_args = $9, window_width = $10, window_height = $11, fullscreen = $12, avatar_data = $13 WHERE id = $14",
    "DELETE FROM instances WHERE id = $1",
    "UPDATE instances SET java_version = $1, java_path = $2 WHERE id = $3",
];

async fn database_pool(app: &AppHandle) -> Result<&'static SqlitePool, String> {
    let path = launcher_root(app)?.join("launcher.db");
    DATABASE_POOL
        .get_or_try_init(|| async move {
            let options = SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true);
            SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(options)
                .await
        })
        .await
        .map_err(|error| error.to_string())
}

fn sqlite_row_to_json(row: &SqliteRow) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let mut values = serde_json::Map::new();
    for column in row.columns() {
        let index = column.ordinal();
        let value_type = row
            .try_get_raw(index)
            .map_err(|error| error.to_string())?
            .type_info()
            .name()
            .to_string();
        let value = match value_type.as_str() {
            "NULL" => Ok(serde_json::Value::Null),
            "INTEGER" => row
                .try_get::<Option<i64>, _>(index)
                .map(|value| value.map_or(serde_json::Value::Null, |value| serde_json::json!(value))),
            "REAL" => row
                .try_get::<Option<f64>, _>(index)
                .map(|value| value.map_or(serde_json::Value::Null, |value| serde_json::json!(value))),
            "BLOB" => row
                .try_get::<Option<Vec<u8>>, _>(index)
                .map(|value| value.map_or(serde_json::Value::Null, |value| serde_json::json!(STANDARD.encode(value)))),
            _ => row
                .try_get::<Option<String>, _>(index)
                .map(|value| value.map_or(serde_json::Value::Null, serde_json::Value::String)),
        }
        .map_err(|error| error.to_string())?;
        values.insert(column.name().to_string(), value);
    }
    Ok(values)
}

#[tauri::command]
async fn database_select(
    app: AppHandle,
    query: String,
    bind_values: Vec<serde_json::Value>,
) -> Result<Vec<serde_json::Map<String, serde_json::Value>>, String> {
    if !APPROVED_DATABASE_SELECTS.contains(&query.as_str()) {
        return Err("Этот запрос к базе данных не разрешён.".into());
    }
    let mut statement = sqlx::query(&query);
    for value in bind_values {
        statement = match value {
            serde_json::Value::Null => statement.bind(Option::<String>::None),
            serde_json::Value::Bool(value) => statement.bind(i64::from(value)),
            serde_json::Value::Number(value) => {
                if let Some(value) = value.as_i64() {
                    statement.bind(value)
                } else if let Some(value) = value.as_f64() {
                    statement.bind(value)
                } else {
                    return Err("Некорректное числовое значение SQL.".into());
                }
            }
            serde_json::Value::String(value) => statement.bind(value),
            _ => return Err("Поддерживаются только scalar SQL значения.".into()),
        };
    }
    statement
        .fetch_all(database_pool(&app).await?)
        .await
        .map_err(|error| error.to_string())?
        .iter()
        .map(sqlite_row_to_json)
        .collect()
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DatabaseExecuteResult {
    rows_affected: u64,
    last_insert_id: i64,
}

#[tauri::command]
async fn database_execute(
    app: AppHandle,
    query: String,
    bind_values: Vec<serde_json::Value>,
) -> Result<DatabaseExecuteResult, String> {
    if !APPROVED_DATABASE_MUTATIONS.contains(&query.as_str()) {
        return Err("Этот запрос к базе данных не разрешён.".into());
    }
    let mut statement = sqlx::query(&query);
    for value in bind_values {
        statement = match value {
            serde_json::Value::Null => statement.bind(Option::<String>::None),
            serde_json::Value::Bool(value) => statement.bind(i64::from(value)),
            serde_json::Value::Number(value) => {
                if let Some(value) = value.as_i64() {
                    statement.bind(value)
                } else if let Some(value) = value.as_f64() {
                    statement.bind(value)
                } else {
                    return Err("Некорректное числовое значение SQL.".into());
                }
            }
            serde_json::Value::String(value) => statement.bind(value),
            _ => return Err("Поддерживаются только scalar SQL значения.".into()),
        };
    }
    let result = statement
        .execute(database_pool(&app).await?)
        .await
        .map_err(|error| error.to_string())?;
    Ok(DatabaseExecuteResult {
        rows_affected: result.rows_affected(),
        last_insert_id: result.last_insert_rowid(),
    })
}

#[tauri::command]
fn get_memory_limit_mb() -> u32 {
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    ((system.total_memory() as f64 * 0.8 / 1024.0 / 1024.0).floor() as u32).max(512)
}

async fn migrate_database_schema_at(path: &Path) -> Result<(), String> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|error| error.to_string())?;
    let result = async {
        let mut transaction = pool.begin().await.map_err(|error| error.to_string())?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS instances (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL, folder_name TEXT NOT NULL UNIQUE, minecraft_version TEXT NOT NULL, loader_type TEXT NOT NULL DEFAULT 'vanilla', loader_version TEXT, window_width INTEGER NOT NULL DEFAULT 1280, window_height INTEGER NOT NULL DEFAULT 720, fullscreen INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP)",
        )
        .execute(&mut *transaction)
        .await
        .map_err(|error| error.to_string())?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS accounts (id TEXT PRIMARY KEY NOT NULL, username TEXT NOT NULL, uuid TEXT NOT NULL, account_type TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP)",
        )
        .execute(&mut *transaction)
        .await
        .map_err(|error| error.to_string())?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL)",
        )
        .execute(&mut *transaction)
        .await
        .map_err(|error| error.to_string())?;

        let version_row = sqlx::query("PRAGMA user_version")
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?;
        let schema_version: i64 = version_row
            .try_get("user_version")
            .map_err(|error| error.to_string())?;
        if schema_version < 1 {
            let columns = sqlx::query("PRAGMA table_info(instances)")
                .fetch_all(&mut *transaction)
                .await
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|column| column.try_get::<String, _>("name"))
                .collect::<Result<std::collections::HashSet<_>, _>>()
                .map_err(|error| error.to_string())?;
            for (name, definition) in [
                ("java_version", "INTEGER NOT NULL DEFAULT 21"),
                ("java_path", "TEXT"),
                ("memory_mb", "INTEGER NOT NULL DEFAULT 4096"),
                ("jvm_args", "TEXT NOT NULL DEFAULT ''"),
                ("window_width", "INTEGER NOT NULL DEFAULT 1280"),
                ("window_height", "INTEGER NOT NULL DEFAULT 720"),
                ("fullscreen", "INTEGER NOT NULL DEFAULT 0"),
                ("avatar_data", "TEXT"),
                ("modpack_project_id", "TEXT"),
                ("modpack_version_id", "TEXT"),
                ("modpack_version_number", "TEXT"),
                ("repository_url", "TEXT"),
                ("repository_pack_id", "TEXT"),
                ("repository_version", "TEXT"),
                ("repository_channel", "TEXT NOT NULL DEFAULT 'stable'"),
            ] {
                if !columns.contains(name) {
                    sqlx::query(&format!("ALTER TABLE instances ADD COLUMN {name} {definition}"))
                        .execute(&mut *transaction)
                        .await
                        .map_err(|error| error.to_string())?;
                }
            }
            sqlx::query("PRAGMA user_version = 1")
                .execute(&mut *transaction)
                .await
                .map_err(|error| error.to_string())?;
        }
        transaction.commit().await.map_err(|error| error.to_string())
    }
    .await;
    pool.close().await;
    result
}

#[tauri::command]
async fn migrate_database_schema(app: AppHandle) -> Result<(), String> {
    let directory = launcher_root(&app)?;
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    migrate_database_schema_at(&directory.join("launcher.db")).await
}

fn launcher_root(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let config_dir = app
        .path()
        .app_config_dir()
        .map_err(|error| error.to_string())?;
    Ok(config_dir
        .parent()
        .ok_or("Не удалось определить AppData.")?
        .join("MyLauncher"))
}

pub(crate) fn open_in_explorer(path: &Path) -> Result<(), String> {
    Command::new("explorer.exe")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Не удалось открыть папку в Проводнике: {error}"))
}

#[tauri::command]
fn open_launcher_directory(app: AppHandle) -> Result<(), String> {
    let root = launcher_root(&app)?;
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    open_in_explorer(&root)
}

#[tauri::command]
fn save_instance_avatar(app: AppHandle, instance_id: String, data_url: String) -> Result<String, String> {
    if instance_id.is_empty()
        || instance_id.len() > 64
        || !instance_id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        || data_url.len() > 1_000_000
    {
        return Err("Некорректный аватар инстанса.".into());
    }
    let (header, encoded) = data_url
        .split_once(',')
        .ok_or("Некорректный формат аватара.")?;
    let extension = match header {
        "data:image/webp;base64" => "webp",
        "data:image/png;base64" => "png",
        "data:image/jpeg;base64" => "jpg",
        _ => return Err("Поддерживаются только PNG, JPEG и WebP аватары.".into()),
    };
    let bytes = STANDARD.decode(encoded).map_err(|error| error.to_string())?;
    if bytes.len() > 512_000 {
        return Err("Аватар превышает допустимый размер.".into());
    }
    let valid_image = match extension {
        "webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        _ => false,
    };
    if !valid_image {
        return Err("Содержимое аватара не соответствует типу изображения.".into());
    }
    let digest = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let avatar_directory = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?
        .join("avatars");
    fs::create_dir_all(&avatar_directory).map_err(|error| error.to_string())?;
    let avatar_path = avatar_directory.join(format!("{instance_id}-{digest}.{extension}"));
    if !avatar_path.is_file() {
        let temporary_path = avatar_directory.join(format!(".{instance_id}-{}.tmp", uuid::Uuid::new_v4()));
        fs::write(&temporary_path, bytes).map_err(|error| error.to_string())?;
        if let Err(error) = fs::rename(&temporary_path, &avatar_path) {
            let _ = fs::remove_file(&temporary_path);
            if !avatar_path.is_file() {
                return Err(error.to_string());
            }
        }
    }
    Ok(avatar_path.to_string_lossy().into_owned())
}

#[tauri::command]
fn remove_instance_avatar(app: AppHandle, avatar_path: String) -> Result<(), String> {
    let avatar_directory = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?
        .join("avatars");
    if !Path::new(&avatar_path).exists() {
        return Ok(());
    }
    let canonical_directory = avatar_directory.canonicalize().map_err(|error| error.to_string())?;
    let canonical_file = Path::new(&avatar_path).canonicalize().map_err(|error| error.to_string())?;
    if canonical_file.parent() != Some(canonical_directory.as_path())
        || !["webp", "png", "jpg"].contains(&canonical_file.extension().and_then(|value| value.to_str()).unwrap_or_default())
    {
        return Err("Некорректный путь файла аватара.".into());
    }
    fs::remove_file(canonical_file).map_err(|error| error.to_string())
}

pub(crate) fn instances_root(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let root = app
        .path()
        .app_config_dir()
        .map_err(|error| error.to_string())?
        .parent()
        .ok_or_else(|| "Не удалось определить папку AppData.".to_string())?
        .join("MyLauncher")
        .join("instances");
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    Ok(root)
}

#[tauri::command]
fn open_instance_directory(app: AppHandle, folder_name: String) -> Result<(), String> {
    validate_folder_name(&folder_name)?;
    let root = instances_root(&app)?;
    let instance = root.join(folder_name);
    if !fs::symlink_metadata(&instance).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        return Err("Папка инстанса не найдена.".into());
    }
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    let canonical_instance = instance.canonicalize().map_err(|error| error.to_string())?;
    if canonical_instance.parent() != Some(canonical_root.as_path()) {
        return Err("Путь инстанса выходит за пределы хранилища.".into());
    }
    open_in_explorer(&canonical_instance)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct InstanceFileEntry {
    name: String,
    is_directory: bool,
    size_bytes: u64,
    modified_unix_seconds: Option<u64>,
}

#[tauri::command]
fn list_instance_files(
    app: AppHandle,
    folder_name: String,
) -> Result<Vec<InstanceFileEntry>, String> {
    validate_folder_name(&folder_name)?;
    let root = instances_root(&app)?;
    let instance = root.join(folder_name);
    if !fs::symlink_metadata(&instance).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        return Err("Папка инстанса не найдена.".into());
    }
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    let canonical_instance = instance.canonicalize().map_err(|error| error.to_string())?;
    if canonical_instance.parent() != Some(canonical_root.as_path()) {
        return Err("Путь инстанса выходит за пределы хранилища.".into());
    }

    let mut files = fs::read_dir(canonical_instance)
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if !file_type.is_file() && !file_type.is_dir() {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            let modified_unix_seconds = metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs());
            Some(InstanceFileEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_directory: file_type.is_dir(),
                size_bytes: if file_type.is_file() {
                    metadata.len()
                } else {
                    0
                },
                modified_unix_seconds,
            })
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| {
        right
            .is_directory
            .cmp(&left.is_directory)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    Ok(files)
}

pub(crate) fn validate_folder_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || name.encode_utf16().count() > 64
        || trimmed != name
        || name == "."
        || name == ".."
        || name.ends_with('.')
        || name.ends_with(' ')
        || name
            .chars()
            .any(|character| character.is_control() || "\\/:*?\"<>|".contains(character))
        || name.split('.').next().is_some_and(|stem| {
            let uppercase_stem = stem.to_ascii_uppercase();
            matches!(uppercase_stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (stem.len() == 4
                    && (uppercase_stem.starts_with("COM") || uppercase_stem.starts_with("LPT"))
                    && stem.as_bytes()[3].is_ascii_digit()
                    && stem.as_bytes()[3] != b'0')
        })
    {
        return Err("Недопустимое имя папки инстанса для Windows.".into());
    }
    Ok(())
}

fn validate_loader_selection(
    loader_type: &str,
    loader_version: Option<&str>,
) -> Result<(), String> {
    match loader_type {
        "vanilla" if loader_version.is_none() => Ok(()),
        "fabric" | "forge" | "neoforge" => {
            let version = loader_version.ok_or("Выберите версию загрузчика.")?;
            if version.is_empty() || version.len() > 128 || version.contains(['/', '\\']) {
                return Err("Некорректная версия загрузчика.".into());
            }
            Ok(())
        }
        _ => Err("Неизвестный тип или версия загрузчика.".into()),
    }
}

fn write_metadata(directory: &Path, metadata: &serde_json::Value) -> Result<(), String> {
    let metadata_path = directory.join(".launcher_meta.json");
    let contents = serde_json::to_vec_pretty(metadata).map_err(|error| error.to_string())?;
    fsutil::write_file_with_backup(&metadata_path, &contents)
}

#[tauri::command]
fn offline_account_uuid(username: String) -> Result<String, String> {
    if username.is_empty()
        || username.len() > 16
        || !username
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err("Некорректный никнейм offline-аккаунта.".into());
    }

    // Matches Java's `UUID.nameUUIDFromBytes(("OfflinePlayer:" + name).getBytes())`.
    use md5::{Digest, Md5};
    let digest = Md5::digest(format!("OfflinePlayer:{username}").as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest);
    bytes[6] = (bytes[6] & 0x0f) | 0x30;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(uuid::Uuid::from_bytes(bytes).to_string())
}

fn updated_instance_metadata(
    directory: &Path,
    name: &str,
    minecraft_version: &str,
    loader_type: &str,
    loader_version: Option<&str>,
) -> Result<serde_json::Value, String> {
    let metadata_path = directory.join(".launcher_meta.json");
    let mut metadata = if metadata_path.is_file() {
        serde_json::from_slice(&fs::read(&metadata_path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?
    } else {
        serde_json::json!({})
    };
    let same_loader = metadata
        .get("loader_type")
        .and_then(serde_json::Value::as_str)
        == Some(loader_type)
        && metadata
            .get("loader_version")
            .and_then(serde_json::Value::as_str)
            == loader_version
        && metadata
            .get("minecraft_version")
            .and_then(serde_json::Value::as_str)
            == Some(minecraft_version);
    metadata["name"] = serde_json::Value::String(name.to_string());
    metadata["minecraft_version"] = serde_json::Value::String(minecraft_version.to_string());
    metadata["loader_type"] = serde_json::Value::String(loader_type.to_string());
    metadata["loader_version"] = loader_version.map_or(serde_json::Value::Null, |value| {
        serde_json::Value::String(value.to_string())
    });
    if !same_loader && let Some(object) = metadata.as_object_mut() {
        object.remove("loader_installed");
        object.remove("loader_profile_id");
    }
    Ok(metadata)
}

#[tauri::command]
fn create_instance_directory(
    app: AppHandle,
    folder_name: String,
    minecraft_version: String,
    loader_type: String,
    loader_version: Option<String>,
) -> Result<String, String> {
    validate_folder_name(&folder_name)?;
    validate_loader_selection(&loader_type, loader_version.as_deref())?;
    if minecraft_version.is_empty() || minecraft_version.len() > 32 {
        return Err("Некорректная версия Minecraft.".into());
    }

    let directory = instances_root(&app)?.join(&folder_name);
    fs::create_dir(&directory).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            "Папка с таким именем уже существует.".to_string()
        } else {
            error.to_string()
        }
    })?;

    let result = (|| {
        for child in ["versions", "mods", "config", "logs"] {
            fs::create_dir(directory.join(child)).map_err(|error| error.to_string())?;
        }
        let metadata = serde_json::json!({
            "name": folder_name,
            "minecraft_version": minecraft_version,
            "loader_type": loader_type,
            "loader_version": loader_version,
        });
        write_metadata(&directory, &metadata)?;
        Ok(directory.to_string_lossy().into_owned())
    })();

    if result.is_err() {
        let _ = fs::remove_dir_all(&directory);
    }
    result
}

#[tauri::command]
fn update_instance_directory(
    app: AppHandle,
    old_folder_name: String,
    new_folder_name: String,
    minecraft_version: String,
    loader_type: String,
    loader_version: Option<String>,
) -> Result<String, String> {
    validate_folder_name(&old_folder_name)?;
    validate_folder_name(&new_folder_name)?;
    validate_loader_selection(&loader_type, loader_version.as_deref())?;
    if minecraft_version.is_empty() || minecraft_version.len() > 32 {
        return Err("Некорректная версия Minecraft.".into());
    }

    let root = instances_root(&app)?;
    let source = root.join(&old_folder_name);
    let destination = root.join(&new_folder_name);
    let _locks = InstanceLock::acquire_all(&[&old_folder_name, &new_folder_name])?;
    if !fs::symlink_metadata(&source).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        return Err("Папка инстанса не найдена.".into());
    }
    if source != destination && destination.exists() {
        return Err("Папка с таким именем уже существует.".into());
    }

    let metadata = updated_instance_metadata(
        &source,
        &new_folder_name,
        &minecraft_version,
        &loader_type,
        loader_version.as_deref(),
    )?;
    if source != destination {
        fs::rename(&source, &destination)
            .map_err(|error| format!("Не удалось переименовать папку инстанса: {error}"))?;
    }
    if let Err(error) = write_metadata(&destination, &metadata) {
        if source != destination {
            let _ = fs::rename(&destination, &source);
        }
        return Err(error);
    }
    Ok(destination.to_string_lossy().into_owned())
}

#[tauri::command]
fn delete_instance_directory(app: AppHandle, folder_name: String) -> Result<(), String> {
    validate_folder_name(&folder_name)?;
    let _lock = InstanceLock::acquire(&folder_name)?;
    let root = instances_root(&app)?;
    let directory = root.join(folder_name);
    if !directory.exists() {
        return Ok(());
    }
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    let canonical_directory = directory
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if canonical_directory.parent() != Some(canonical_root.as_path()) {
        return Err("Путь инстанса выходит за пределы хранилища.".into());
    }
    fs::remove_dir_all(canonical_directory).map_err(|error| error.to_string())
}

fn copy_instance_contents(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir(destination).map_err(|error| error.to_string())?;
    for entry in fs::read_dir(source).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_symlink() {
            return Err("Копирование инстансов с символическими ссылками запрещено.".into());
        }
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_instance_contents(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), target).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
fn duplicate_instance_directory(
    app: AppHandle,
    source_name: String,
    destination_name: String,
) -> Result<(), String> {
    validate_folder_name(&source_name)?;
    validate_folder_name(&destination_name)?;
    if source_name == destination_name {
        return Err("Имя копии должно отличаться от имени исходного инстанса.".into());
    }
    let _locks = InstanceLock::acquire_all(&[&source_name, &destination_name])?;
    let root = instances_root(&app)?;
    let source = root.join(source_name);
    let destination = root.join(&destination_name);
    if !fs::symlink_metadata(&source).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        return Err("Исходная папка инстанса не найдена.".into());
    }
    copy_instance_contents(&source, &destination)
        .and_then(|()| {
            let metadata_path = destination.join(".launcher_meta.json");
            let mut metadata: serde_json::Value = serde_json::from_slice(
                &fs::read(&metadata_path).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            metadata["name"] = serde_json::Value::String(destination_name);
            write_metadata(&destination, &metadata)
        })
        .inspect_err(|_| {
            let _ = fs::remove_dir_all(&destination);
        })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let launcher_dir = config_dir
                .parent()
                .ok_or("Не удалось определить папку AppData.")?
                .join("MyLauncher");
            for child in [
                "instances",
                "java",
                "assets",
                "libraries",
                "logs",
                "servers",
            ] {
                fs::create_dir_all(launcher_dir.join(child))?;
            }
            modpack::cleanup_repository_staging(&launcher_dir.join("instances"))?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            database_select,
            database_execute,
            migrate_database_schema,
            create_instance_directory,
            update_instance_directory,
            delete_instance_directory,
            duplicate_instance_directory,
            open_instance_directory,
            server::open_server_directory,
            list_instance_files,
            minecraft::get_loader_versions,
            minecraft::get_minecraft_versions,
            save_instance_avatar,
            remove_instance_avatar,
            server::get_server_install_options,
            minecraft::install_loader,
            minecraft::install_minecraft_version,
            minecraft::repair_minecraft_version,
            minecraft::is_minecraft_installed,
            minecraft::get_required_java_version,
            minecraft::is_loader_installed,
            minecraft::list_installed_instance_ids,
            minecraft::begin_instance_install,
            minecraft::cancel_instance_install,
            minecraft::check_instance_install_cancellation,
            minecraft::finish_instance_install,
            java::install_java_runtime,
            launcher::launch_game,
            launcher::stop_game,
            launcher::read_game_logs,
            get_memory_limit_mb,
            offline_account_uuid,
            open_launcher_directory,
            modrinth::search_mods,
            modpack::search_modpacks,
            modpack::get_modpack_versions,
            modpack::install_modpack,
            modpack::cancel_modpack_install,
            modpack::list_repository_packs,
            modpack::list_repository_versions,
            modpack::install_repository_pack,
            modpack::update_repository_pack,
            modpack::rollback_repository_update,
            modpack::finalize_repository_update,
            modpack::list_pending_repository_updates,
            modrinth::list_instance_worlds,
            modrinth::list_installed_catalog_projects,
            modrinth::install_catalog_project,
            modrinth::list_installed_mods,
            modrinth::list_mod_versions,
            modrinth::install_mod,
            modrinth::update_mod,
            modrinth::toggle_mod,
            modrinth::remove_mod,
            modrinth::list_server_worlds,
            modrinth::search_server_content,
            modrinth::install_server_content,
            modrinth::list_server_optional_dependencies,
            modrinth::list_server_content,
            modrinth::update_server_content,
            modrinth::toggle_server_content,
            modrinth::remove_server_content,
            server::list_servers,
            server::install_server,
            server::cancel_server_install,
            server::delete_server,
            server::update_server_memory,
            server::update_server_controls,
            server::start_server,
            server::stop_server,
            server::restart_server,
            server::send_server_command,
            server::read_server_log,
            auth::request_microsoft_device_code,
            auth::complete_microsoft_login,
            auth::remove_microsoft_tokens
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{
        APPROVED_DATABASE_MUTATIONS, APPROVED_DATABASE_SELECTS, SqliteConnectOptions,
        SqlitePoolOptions, migrate_database_schema_at, offline_account_uuid, sqlite_row_to_json,
        updated_instance_metadata, validate_folder_name,
    };
    use sqlx::Row;
    use std::fs;

    #[test]
    fn accepts_valid_windows_folder_names() {
        assert!(validate_folder_name("Survival Pack").is_ok());
        assert!(validate_folder_name("Моя сборка").is_ok());
    }

    #[test]
    fn offline_uuid_matches_java_name_uuid_from_bytes() {
        assert_eq!(
            offline_account_uuid("Player".into()).unwrap(),
            "a01e3843-e521-3998-958a-f459800e4d11"
        );
        assert!(offline_account_uuid("bad name".into()).is_err());
    }

    #[test]
    fn rejects_invalid_windows_folder_names() {
        for name in ["", "..", "bad/name", "trailing.", "CON", "com1.txt"] {
            assert!(validate_folder_name(name).is_err(), "accepted {name:?}");
        }
        assert!(validate_folder_name(&"a".repeat(65)).is_err());
    }

    #[test]
    fn profile_edit_preserves_installed_loader_until_changed() {
        let root = std::env::current_dir()
            .unwrap()
            .join(format!("mylauncher-meta-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join(".launcher_meta.json"),
            r#"{"minecraft_version":"1.21.1","loader_type":"fabric","loader_version":"0.16.2","loader_installed":true}"#,
        )
        .unwrap();
        let preserved =
            updated_instance_metadata(&root, "Pack Two", "1.21.1", "fabric", Some("0.16.2"))
                .unwrap();
        assert_eq!(preserved["loader_installed"], true);
        let reset =
            updated_instance_metadata(&root, "Pack Two", "1.21.1", "fabric", Some("0.16.3"))
                .unwrap();
        assert!(reset.get("loader_installed").is_none());
        fs::write(
            root.join(".launcher_meta.json"),
            r#"{"minecraft_version":"1.21.1","loader_type":"fabric","loader_version":"0.16.2","loader_installed":true}"#,
        )
        .unwrap();
        let game_changed =
            updated_instance_metadata(&root, "Pack Two", "1.21.2", "fabric", Some("0.16.2"))
                .unwrap();
        assert!(game_changed.get("loader_installed").is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn database_query_allowlist_rejects_arbitrary_sql() {
        assert!(APPROVED_DATABASE_SELECTS.contains(&"SELECT * FROM instances ORDER BY created_at DESC"));
        assert!(APPROVED_DATABASE_MUTATIONS.contains(&"DELETE FROM accounts WHERE id = $1"));
        assert!(!APPROVED_DATABASE_SELECTS.contains(&"SELECT 1"));
        assert!(!APPROVED_DATABASE_MUTATIONS.contains(&"DROP TABLE instances"));
        assert!(!APPROVED_DATABASE_MUTATIONS.contains(&"DELETE FROM accounts WHERE id = $1; DROP TABLE instances"));
    }

    #[tokio::test]
    async fn database_rows_serialize_scalar_and_null_values() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let row = sqlx::query("SELECT 7 AS count, 'ready' AS status, NULL AS avatar")
            .fetch_one(&pool)
            .await
            .unwrap();
        let values = sqlite_row_to_json(&row).unwrap();

        assert_eq!(values["count"], 7);
        assert_eq!(values["status"], "ready");
        assert!(values["avatar"].is_null());
        pool.close().await;
    }

    #[tokio::test]
    async fn database_schema_migration_commits_columns_and_version_together() {
        let path = std::env::current_dir()
            .unwrap()
            .join(format!("mylauncher-migration-test-{}.db", uuid::Uuid::new_v4()));
        let legacy_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE instances (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL, folder_name TEXT NOT NULL UNIQUE, minecraft_version TEXT NOT NULL, loader_type TEXT NOT NULL DEFAULT 'vanilla', loader_version TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP)",
        )
        .execute(&legacy_pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO instances (id, name, folder_name, minecraft_version) VALUES ('old', 'Old', 'Old', '1.20.1')")
            .execute(&legacy_pool)
            .await
            .unwrap();
        legacy_pool.close().await;

        migrate_database_schema_at(&path).await.unwrap();

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(false),
            )
            .await
            .unwrap();
        let version: i64 = sqlx::query("PRAGMA user_version")
            .fetch_one(&pool)
            .await
            .unwrap()
            .try_get("user_version")
            .unwrap();
        let columns = sqlx::query("PRAGMA table_info(instances)")
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|column| column.try_get::<String, _>("name").unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(version, 1);
        for column in [
            "java_version",
            "avatar_data",
            "repository_channel",
            "window_width",
            "window_height",
            "fullscreen",
        ] {
            assert!(columns.contains(column));
        }
        let old_instance = sqlx::query("SELECT window_width, window_height, fullscreen FROM instances WHERE id = 'old'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(old_instance.try_get::<i64, _>("window_width").unwrap(), 1280);
        assert_eq!(old_instance.try_get::<i64, _>("window_height").unwrap(), 720);
        assert_eq!(old_instance.try_get::<i64, _>("fullscreen").unwrap(), 0);
        pool.close().await;
        fs::remove_file(path).unwrap();
    }
}
