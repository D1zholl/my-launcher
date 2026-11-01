use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

use crate::{
    auth::refresh_minecraft_session, get_memory_limit_mb, instances_root, java::validate_javaw,
    locks::InstanceLock, process::hide_console_window, validate_folder_name,
};

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GameLog {
    folder_name: String,
    stream: String,
    line: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct GameExit {
    folder_name: String,
    success: bool,
    code: Option<i32>,
}

static GAME_PROCESSES: OnceLock<Mutex<HashMap<String, Arc<Mutex<Child>>>>> = OnceLock::new();

fn game_processes() -> &'static Mutex<HashMap<String, Arc<Mutex<Child>>>> {
    GAME_PROCESSES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn metadata_string(metadata: &Value, field: &str) -> Result<String, String> {
    metadata
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("В метаданных Minecraft отсутствует {field}."))
}

pub(crate) fn rules_allow(rules: Option<&Value>, features: &Value) -> bool {
    let Some(rules) = rules.and_then(Value::as_array) else {
        return true;
    };
    let mut allowed = false;
    for rule in rules {
        let os_matches = rule
            .get("os")
            .and_then(|os| os.get("name"))
            .and_then(Value::as_str)
            .is_none_or(|name| name == "windows");
        let features_match =
            rule.get("features")
                .and_then(Value::as_object)
                .is_none_or(|required| {
                    required
                        .iter()
                        .all(|(name, expected)| features.get(name) == Some(expected))
                });
        if os_matches && features_match {
            allowed = rule.get("action").and_then(Value::as_str) == Some("allow");
        }
    }
    allowed
}

pub(crate) fn default_game_features() -> Value {
    serde_json::json!({
        "has_custom_resolution": true,
        "is_demo_user": false,
        "is_quick_play_singleplayer": false,
        "is_quick_play_multiplayer": false,
        "is_quick_play_realms": false,
        "has_quick_plays_support": false,
    })
}

fn argument_values(value: &Value, output: &mut Vec<String>, features: &Value) {
    match value {
        Value::String(argument) => output.push(argument.clone()),
        Value::Array(arguments) => {
            for argument in arguments {
                argument_values(argument, output, features);
            }
        }
        Value::Object(object)
            if rules_allow(object.get("rules"), features)
                && let Some(value) = object.get("value") =>
        {
            argument_values(value, output, features);
        }
        _ => {}
    }
}

fn inherited_arguments(
    base_metadata: &Value,
    profile_metadata: &Value,
    profile_inherits: bool,
    argument_type: &str,
    features: &Value,
) -> Vec<String> {
    let mut arguments = Vec::new();
    if let Some(base_arguments) = base_metadata.pointer(&format!("/arguments/{argument_type}")) {
        argument_values(base_arguments, &mut arguments, features);
    }
    if profile_inherits
        && let Some(profile_arguments) =
            profile_metadata.pointer(&format!("/arguments/{argument_type}"))
    {
        argument_values(profile_arguments, &mut arguments, features);
    }
    arguments
}

fn replace_placeholders(argument: &str, values: &[(&str, &str)]) -> String {
    values
        .iter()
        .fold(argument.to_string(), |result, (key, value)| {
            result.replace(key, value)
        })
}

fn include_client_jar_in_forge_ignore_list(argument: &str, client_jar_name: &str) -> String {
    let Some(ignore_list) = argument.strip_prefix("-DignoreList=") else {
        return argument.to_string();
    };
    if ignore_list
        .split(',')
        .any(|entry| entry.eq_ignore_ascii_case(client_jar_name))
    {
        return argument.to_string();
    }
    format!("-DignoreList={ignore_list},{client_jar_name}")
}

fn uses_mapped_client(loader_type: &str) -> bool {
    matches!(loader_type, "forge" | "neoforge")
}

fn apply_window_arguments(
    arguments: Vec<String>,
    width: &str,
    height: &str,
    fullscreen: bool,
) -> Vec<String> {
    let mut normalized = Vec::with_capacity(arguments.len() + 4);
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--width" | "--height" => {
                index += 1;
                if index < arguments.len() && !arguments[index].starts_with('-') {
                    index += 1;
                }
            }
            argument if argument.starts_with("--width=") || argument.starts_with("--height=") => {
                index += 1;
            }
            "--fullscreen" => index += 1,
            _ => {
                normalized.push(arguments[index].clone());
                index += 1;
            }
        }
    }
    normalized.extend([
        "--width".into(),
        width.into(),
        "--height".into(),
        height.into(),
    ]);
    if fullscreen {
        normalized.push("--fullscreen".into());
    }
    normalized
}

pub(crate) fn maven_library_path(coordinate: &str) -> Option<PathBuf> {
    let (coordinate, extension) = coordinate
        .split_once('@')
        .map_or((coordinate, "jar"), |(coordinate, extension)| {
            (coordinate, extension)
        });
    let parts: Vec<_> = coordinate.split(':').collect();
    if !(3..=4).contains(&parts.len())
        || parts
            .iter()
            .any(|part| part.is_empty() || part.contains(['/', '\\']))
        || extension.contains(['/', '\\'])
    {
        return None;
    }
    let group_path = parts[0].replace('.', "/");
    let classifier = parts
        .get(3)
        .map_or(String::new(), |value| format!("-{value}"));
    let filename = format!("{}-{}{}.{}", parts[1], parts[2], classifier, extension);
    Some(
        PathBuf::from(group_path)
            .join(parts[1])
            .join(parts[2])
            .join(filename),
    )
}

fn emit_lines(
    app: AppHandle,
    folder_name: String,
    stream: &'static str,
    log_file: Arc<Mutex<fs::File>>,
    reader: impl std::io::Read + Send + 'static,
) {
    thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            if let Ok(mut file) = log_file.lock() {
                let _ = writeln!(file, "[{stream}] {line}");
            }
            let _ = app.emit(
                "game-log",
                GameLog {
                    folder_name: folder_name.clone(),
                    stream: stream.into(),
                    line,
                },
            );
        }
    });
}

#[tauri::command]
pub(crate) fn read_game_logs(app: AppHandle, folder_name: String) -> Result<Vec<GameLog>, String> {
    validate_folder_name(&folder_name)?;
    let logs_directory = app
        .path()
        .app_config_dir()
        .map_err(|error| error.to_string())?
        .parent()
        .ok_or("Не удалось определить папку AppData.")?
        .join("MyLauncher")
        .join("logs");
    let prefix = format!("{folder_name}-");
    let log_entries = match fs::read_dir(logs_directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let Some(log_path) = log_entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let file_name = path.file_name()?.to_str()?;
            (file_name.starts_with(&prefix) && file_name.ends_with(".log"))
                .then_some((file_name.to_string(), path))
        })
        .max_by(|left, right| left.0.cmp(&right.0))
        .map(|(_, path)| path)
    else {
        return Ok(Vec::new());
    };
    let mut entries = VecDeque::with_capacity(500);
    for line in BufReader::new(fs::File::open(log_path).map_err(|error| error.to_string())?)
        .lines()
        .map_while(Result::ok)
    {
        if entries.len() == 500 {
            entries.pop_front();
        }
        let (stream, line) = if let Some(line) = line.strip_prefix("[stderr] ") {
            ("stderr", line)
        } else if let Some(line) = line.strip_prefix("[stdout] ") {
            ("stdout", line)
        } else {
            ("stdout", line.as_str())
        };
        entries.push_back(GameLog {
            folder_name: folder_name.clone(),
            stream: stream.into(),
            line: line.into(),
        });
    }
    Ok(entries.into())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn launch_game(
    app: AppHandle,
    folder_name: String,
    java_path: String,
    memory_mb: u32,
    jvm_args: String,
    username: String,
    player_uuid: String,
    account_type: String,
    microsoft_client_id: String,
    width: u32,
    height: u32,
    fullscreen: bool,
) -> Result<u32, String> {
    validate_folder_name(&folder_name)?;
    if username.is_empty()
        || username.len() > 16
        || !["offline", "microsoft"].contains(&account_type.as_str())
        || (account_type == "offline"
            && !username
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_'))
    {
        return Err("Некорректный игровой аккаунт.".into());
    }
    if !(512..=get_memory_limit_mb()).contains(&memory_mb) || width < 640 || height < 480 {
        return Err("Некорректные параметры запуска Minecraft.".into());
    }

    let instance_lock = InstanceLock::acquire(&folder_name)?;
    let canonical_java = validate_javaw(&java_path)?;
    let session = if account_type == "microsoft" {
        Some(refresh_minecraft_session(&player_uuid, &microsoft_client_id).await?)
    } else {
        None
    };
    let access_token = session
        .as_ref()
        .map_or("0", |session| session.access_token.as_str());
    let xuid = session
        .as_ref()
        .map_or("0", |session| session.xuid.as_str());
    let auth_session = format!("token:{access_token}:{player_uuid}");

    let instance = instances_root(&app)?.join(&folder_name);
    let metadata_path = instance.join(".launcher_meta.json");
    let instance_metadata: Value =
        serde_json::from_slice(&fs::read(metadata_path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let loader_type = metadata_string(&instance_metadata, "loader_type")?;
    let loader_installed = instance_metadata
        .get("loader_installed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if loader_type != "vanilla" && !loader_installed {
        return Err("Сначала установите загрузчик выбранного профиля.".into());
    }
    let version = metadata_string(&instance_metadata, "minecraft_version")?;
    if !crate::minecraft::valid_version_component(&version) {
        return Err("Некорректная версия Minecraft в метаданных инстанса.".into());
    }
    let version_dir = instance.join("versions").join(&version);
    if !crate::minecraft::installation_complete(&version_dir, &version) {
        return Err(
            "Установка Minecraft не завершена. Запустите подготовку инстанса ещё раз.".into(),
        );
    }
    let base_version_data: Value = serde_json::from_slice(
        &fs::read(version_dir.join(format!("{version}.json")))
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let (profile_id, version_data) = if loader_type == "vanilla" {
        (version.clone(), base_version_data.clone())
    } else {
        let versions_dir = instance.join("versions");
        let expected_name = loader_type.to_ascii_lowercase();
        let selected_profile_id = instance_metadata
            .get("loader_profile_id")
            .and_then(Value::as_str)
            .filter(|id| {
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
            });
        let mut selected_profile = if let Some(id) = selected_profile_id {
            let profile_path = versions_dir.join(id).join(format!("{id}.json"));
            let profile: Value =
                serde_json::from_slice(&fs::read(profile_path).map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?;
            (profile.get("inheritsFrom").and_then(Value::as_str) == Some(version.as_str()))
                .then(|| (id.to_string(), profile))
        } else {
            None
        };
        if selected_profile.is_none() {
            for entry in fs::read_dir(&versions_dir).map_err(|error| error.to_string())? {
                let entry = entry.map_err(|error| error.to_string())?;
                if !entry
                    .file_type()
                    .map_err(|error| error.to_string())?
                    .is_dir()
                {
                    continue;
                }
                let id = entry.file_name().to_string_lossy().into_owned();
                if !id.to_ascii_lowercase().contains(&expected_name) {
                    continue;
                }
                let profile_path = entry.path().join(format!("{id}.json"));
                let Ok(bytes) = fs::read(profile_path) else {
                    continue;
                };
                let Ok(profile): Result<Value, _> = serde_json::from_slice(&bytes) else {
                    continue;
                };
                if profile.get("inheritsFrom").and_then(Value::as_str) == Some(version.as_str()) {
                    selected_profile = Some((id, profile));
                    break;
                }
            }
        }
        selected_profile.ok_or("Не найден профиль установленного загрузчика.")?
    };
    let client_jar = version_dir.join(format!("{version}.jar"));
    if !client_jar.is_file() {
        return Err("Клиентский JAR Minecraft не установлен.".into());
    }

    let launcher_root = instances_root(&app)?
        .parent()
        .ok_or("Не удалось определить MyLauncher.")?
        .to_path_buf();
    let libraries_root = launcher_root.join("libraries");
    let mut classpath = vec![client_jar.to_string_lossy().into_owned()];
    for metadata in [&base_version_data, &version_data] {
        if let Some(libraries) = metadata.get("libraries").and_then(Value::as_array) {
            for library in libraries {
                if !rules_allow(library.get("rules"), &Value::Null) {
                    continue;
                }
                let library_path = library
                    .pointer("/downloads/artifact/path")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .or_else(|| {
                        library
                            .get("name")
                            .and_then(Value::as_str)
                            .and_then(maven_library_path)
                    });
                if let Some(candidate) = library_path
                    && candidate
                        .components()
                        .all(|component| matches!(component, std::path::Component::Normal(_)))
                {
                    for root in [&libraries_root, &instance.join("libraries")] {
                        let library_path = root.join(&candidate);
                        if library_path.is_file() {
                            let resolved = library_path.to_string_lossy().into_owned();
                            if !classpath.contains(&resolved) {
                                classpath.push(resolved);
                            }
                            break;
                        }
                    }
                }
            }
        }
    }

    let classpath = classpath.join(";");
    let classpath_separator = ";";
    let assets_root = launcher_root.join("assets");
    let asset_index = version_data
        .pointer("/assetIndex/id")
        .or_else(|| base_version_data.pointer("/assetIndex/id"))
        .and_then(Value::as_str)
        .unwrap_or(&version);
    if !crate::minecraft::valid_version_component(asset_index) {
        return Err("Некорректный идентификатор asset index.".into());
    }
    let asset_index_path = assets_root
        .join("indexes")
        .join(format!("{asset_index}.json"));
    let asset_index_metadata = fs::read(asset_index_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let game_assets = if asset_index_metadata
        .as_ref()
        .and_then(|metadata| metadata.get("virtual"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        assets_root.join("virtual").join(asset_index)
    } else if asset_index_metadata
        .as_ref()
        .and_then(|metadata| metadata.get("map_to_resources"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        instance.join("resources")
    } else {
        assets_root.clone()
    };
    let main_class = version_data
        .get("mainClass")
        .and_then(Value::as_str)
        .or_else(|| base_version_data.get("mainClass").and_then(Value::as_str))
        .ok_or("В метаданных отсутствует mainClass.")?;
    let natives_directory = version_dir.join("natives");
    fs::create_dir_all(&natives_directory).map_err(|error| error.to_string())?;
    let mut command = Command::new(canonical_java);
    command.arg(format!("-Xmx{memory_mb}M"));
    command.arg(format!("-Xms{}M", 1024.min(memory_mb)));
    command.arg(format!(
        "-Djava.library.path={}",
        natives_directory.display()
    ));

    let values = [
        ("${auth_player_name}", username.as_str()),
        ("${clientid}", microsoft_client_id.as_str()),
        ("${auth_session}", &auth_session),
        ("${version_name}", profile_id.as_str()),
        (
            "${game_directory}",
            instance.to_str().ok_or("Некорректный путь папки игры.")?,
        ),
        (
            "${assets_root}",
            assets_root.to_str().ok_or("Некорректный путь assets.")?,
        ),
        ("${assets_index_name}", asset_index),
        (
            "${game_assets}",
            game_assets
                .to_str()
                .ok_or("Некорректный путь game assets.")?,
        ),
        ("${auth_uuid}", player_uuid.as_str()),
        ("${auth_access_token}", access_token),
        ("${auth_xuid}", xuid),
        ("${user_properties}", "{}"),
        (
            "${user_type}",
            if account_type == "microsoft" {
                "msa"
            } else {
                "legacy"
            },
        ),
        ("${version_type}", "release"),
        (
            "${natives_directory}",
            natives_directory
                .to_str()
                .ok_or("Некорректный путь natives.")?,
        ),
        ("${launcher_name}", "MyLauncher"),
        ("${launcher_version}", "0.1.0"),
        ("${classpath}", &classpath),
        ("${classpath_separator}", classpath_separator),
        (
            "${library_directory}",
            libraries_root
                .to_str()
                .ok_or("Некорректный путь libraries.")?,
        ),
        ("${resolution_width}", &width.to_string()),
        ("${resolution_height}", &height.to_string()),
    ];

    let launch_features = default_game_features();
    let jvm_arguments = inherited_arguments(
        &base_version_data,
        &version_data,
        profile_id != version,
        "jvm",
        &launch_features,
    );
    let mut index = 0;
    while index < jvm_arguments.len() {
        if ["-cp", "-classpath", "--class-path"].contains(&jvm_arguments[index].as_str()) {
            index += 2;
            continue;
        }
        let mut argument = replace_placeholders(&jvm_arguments[index], &values);
        if uses_mapped_client(&loader_type) {
            argument =
                include_client_jar_in_forge_ignore_list(&argument, &format!("{version}.jar"));
        }
        command.arg(argument);
        index += 1;
    }
    command.args(
        shell_words::split(&jvm_args)
            .map_err(|error| format!("Некорректные JVM-флаги: {error}"))?,
    );
    command.arg("-cp").arg(&classpath).arg(main_class);

    let game_arguments = inherited_arguments(
        &base_version_data,
        &version_data,
        profile_id != version,
        "game",
        &launch_features,
    );
    let mut resolved_game_arguments = if !game_arguments.is_empty() {
        game_arguments
            .iter()
            .map(|argument| replace_placeholders(argument, &values))
            .collect()
    } else if let Some(arguments) = version_data
        .get("minecraftArguments")
        .or_else(|| base_version_data.get("minecraftArguments"))
        .and_then(Value::as_str)
    {
        shell_words::split(arguments)
            .map_err(|error| error.to_string())?
            .iter()
            .map(|argument| replace_placeholders(argument, &values))
            .collect()
    } else {
        [
            "--username",
            username.as_str(),
            "--version",
            profile_id.as_str(),
            "--gameDir",
            instance.to_str().ok_or("Некорректный путь инстанса.")?,
            "--assetsDir",
            assets_root.to_str().ok_or("Некорректный путь assets.")?,
            "--assetIndex",
            asset_index,
            "--uuid",
            player_uuid.as_str(),
            "--accessToken",
            access_token,
            "--userType",
            if account_type == "microsoft" {
                "msa"
            } else {
                "legacy"
            },
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    };
    resolved_game_arguments = apply_window_arguments(
        resolved_game_arguments,
        &width.to_string(),
        &height.to_string(),
        fullscreen,
    );
    command.args(resolved_game_arguments);

    hide_console_window(&mut command);
    command
        .current_dir(&instance)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let launcher_root = app
        .path()
        .app_config_dir()
        .map_err(|error| error.to_string())?
        .parent()
        .ok_or("Не удалось определить AppData.")?
        .join("MyLauncher");
    let logs_directory = launcher_root.join("logs");
    fs::create_dir_all(&logs_directory).map_err(|error| error.to_string())?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let log_path = logs_directory.join(format!("{folder_name}-{timestamp}.log"));
    let log_file = Arc::new(Mutex::new(
        fs::File::create(log_path).map_err(|error| error.to_string())?,
    ));
    let mut child = command
        .spawn()
        .map_err(|error| format!("Не удалось запустить Java: {error}"))?;
    let process_id = child.id();
    if let Some(stdout) = child.stdout.take() {
        emit_lines(
            app.clone(),
            folder_name.clone(),
            "stdout",
            Arc::clone(&log_file),
            stdout,
        );
    }
    if let Some(stderr) = child.stderr.take() {
        emit_lines(
            app.clone(),
            folder_name.clone(),
            "stderr",
            Arc::clone(&log_file),
            stderr,
        );
    }
    let child = Arc::new(Mutex::new(child));
    let mut processes = match game_processes().lock() {
        Ok(processes) => processes,
        Err(_) => {
            let mut unregistered = child
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _ = unregistered.kill();
            let _ = unregistered.wait();
            return Err("Не удалось зарегистрировать процесс Minecraft; запуск отменён.".into());
        }
    };
    if processes.contains_key(&folder_name) {
        drop(processes);
        let mut unregistered = child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = unregistered.kill();
        let _ = unregistered.wait();
        return Err("Для этого инстанса уже запущена игра; второй запуск отменён.".into());
    }
    processes.insert(folder_name.clone(), Arc::clone(&child));
    drop(processes);

    thread::spawn(move || {
        // The lock is held for the whole game session, so the running instance
        // cannot be updated, renamed, duplicated or deleted underneath the game.
        let _instance_lock = instance_lock;
        let status: std::io::Result<std::process::ExitStatus> = loop {
            let polled = match child.lock() {
                Ok(mut guard) => guard.try_wait(),
                Err(_) => Err(std::io::Error::other(
                    "Не удалось проверить состояние процесса Minecraft.",
                )),
            };
            match polled {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => thread::sleep(Duration::from_millis(400)),
                Err(error) => break Err(error),
            }
        };

        if let Ok(mut processes) = game_processes().lock() {
            processes.remove(&folder_name);
        }
        if let Ok(mut file) = log_file.lock() {
            let _ = writeln!(
                file,
                "[exit] {:?}",
                status.as_ref().map(|status| status.code())
            );
        }
        let _ = app.emit(
            "game-exit",
            GameExit {
                folder_name,
                success: status.as_ref().is_ok_and(std::process::ExitStatus::success),
                code: status.ok().and_then(|status| status.code()),
            },
        );
    });
    Ok(process_id)
}

#[tauri::command]
pub(crate) fn stop_game(folder_name: String) -> Result<(), String> {
    validate_folder_name(&folder_name)?;
    let child = {
        let processes = game_processes()
            .lock()
            .map_err(|_| "Не удалось получить список запущенных игр.".to_string())?;
        processes.get(&folder_name).cloned()
    }
    .ok_or_else(|| format!("Инстанс «{folder_name}» сейчас не запущен."))?;

    let mut child = child
        .lock()
        .map_err(|_| "Не удалось остановить процесс Minecraft.".to_string())?;
    match child.try_wait() {
        Ok(Some(_)) => Err("Процесс Minecraft уже завершён.".into()),
        Ok(None) => child
            .kill()
            .map_err(|error| format!("Не удалось остановить Minecraft: {error}")),
        Err(error) => Err(format!("Не удалось проверить процесс Minecraft: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_window_arguments, argument_values, include_client_jar_in_forge_ignore_list,
        inherited_arguments, maven_library_path, uses_mapped_client,
    };

    #[test]
    fn replaces_duplicate_loader_window_arguments() {
        let arguments = [
            "--width",
            "640",
            "--height",
            "480",
            "--width=800",
            "--height=600",
            "--fullscreen",
            "--username",
            "player",
        ]
        .map(str::to_string)
        .to_vec();

        assert_eq!(
            apply_window_arguments(arguments, "1280", "720", false),
            ["--username", "player", "--width", "1280", "--height", "720"]
        );
    }

    #[test]
    fn loader_arguments_extend_base_minecraft_arguments() {
        let base = serde_json::json!({
            "arguments": {
                "jvm": ["-Dbase=true"],
                "game": ["--username", "${auth_player_name}"]
            }
        });
        let profile = serde_json::json!({
            "arguments": {
                "jvm": ["-Dloader=true"],
                "game": ["--loader", "fabric"]
            }
        });
        let features = serde_json::json!({
            "has_custom_resolution": true,
            "is_demo_user": false
        });

        assert_eq!(
            inherited_arguments(&base, &profile, true, "jvm", &features),
            ["-Dbase=true", "-Dloader=true"]
        );
        assert_eq!(
            inherited_arguments(&base, &profile, true, "game", &features),
            ["--username", "${auth_player_name}", "--loader", "fabric"]
        );
        assert_eq!(
            inherited_arguments(&base, &profile, false, "game", &features),
            ["--username", "${auth_player_name}"]
        );
    }

    #[test]
    fn ignores_the_base_client_jar_when_launching_forge() {
        let argument = "-DignoreList=bootstraplauncher,1.19-forge-41.1.0.jar";
        assert_eq!(
            include_client_jar_in_forge_ignore_list(argument, "1.19.jar"),
            "-DignoreList=bootstraplauncher,1.19-forge-41.1.0.jar,1.19.jar"
        );
        assert_eq!(
            include_client_jar_in_forge_ignore_list(
                "-DignoreList=bootstraplauncher,1.19.jar",
                "1.19.jar"
            ),
            "-DignoreList=bootstraplauncher,1.19.jar"
        );
        assert_eq!(
            include_client_jar_in_forge_ignore_list("-Dexample=value", "1.19.jar"),
            "-Dexample=value"
        );
    }

    #[test]
    fn mapped_client_loaders_ignore_the_base_client_jar() {
        assert!(uses_mapped_client("forge"));
        assert!(uses_mapped_client("neoforge"));
        assert!(!uses_mapped_client("fabric"));
        assert!(!uses_mapped_client("vanilla"));
    }

    #[test]
    fn excludes_unavailable_quick_play_options() {
        let metadata = serde_json::json!([
            {
                "rules": [{
                    "action": "allow",
                    "features": {"is_quick_play_singleplayer": true}
                }],
                "value": "--quickPlaySingleplayer"
            },
            {
                "rules": [{
                    "action": "allow",
                    "features": {"is_quick_play_multiplayer": true}
                }],
                "value": ["--quickPlayMultiplayer", "server.example"]
            },
            {
                "rules": [{
                    "action": "allow",
                    "features": {"has_custom_resolution": true}
                }],
                "value": ["--width", "1280"]
            }
        ]);
        let features = serde_json::json!({"has_custom_resolution": true});
        let mut arguments = Vec::new();

        argument_values(&metadata, &mut arguments, &features);

        assert_eq!(arguments, ["--width", "1280"]);
    }

    #[test]
    fn converts_maven_coordinates_to_relative_library_paths() {
        assert_eq!(
            maven_library_path("org.ow2.asm:asm:9.6").unwrap(),
            std::path::PathBuf::from("org/ow2/asm/asm/9.6/asm-9.6.jar")
        );
        assert_eq!(
            maven_library_path("org.lwjgl:lwjgl:3.3.3:natives-windows@jar").unwrap(),
            std::path::PathBuf::from("org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-windows.jar")
        );
        assert!(maven_library_path("../../bad:jar:1.0").is_none());
    }
}
