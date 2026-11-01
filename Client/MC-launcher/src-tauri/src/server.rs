use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use md5::Md5;
use sha1::{Digest, Sha1};
use sha2::Sha256;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;

use crate::{get_memory_limit_mb, process::hide_console_window, validate_folder_name};

const SERVER_METADATA: &str = ".launcher-server.json";
const MAX_QUICK_COMMANDS: usize = 16;
const SERVER_INSTALLER_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_COMMAND_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_SERVER_LOG_BYTES: usize = 2 * 1024 * 1024;
const MAX_SERVER_JSON_BYTES: usize = 16 * 1024 * 1024;

fn read_stream_tail(mut reader: impl Read, max_bytes: usize) -> Vec<u8> {
    let mut tail = VecDeque::with_capacity(max_bytes);
    let mut chunk = [0_u8; 8192];
    loop {
        let count = match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        if count >= max_bytes {
            tail.clear();
            tail.extend(&chunk[count - max_bytes..count]);
            continue;
        }
        let overflow = tail.len().saturating_add(count).saturating_sub(max_bytes);
        for _ in 0..overflow {
            tail.pop_front();
        }
        tail.extend(&chunk[..count]);
    }
    tail.into_iter().collect()
}

fn read_file_tail(path: &Path, max_bytes: usize) -> std::io::Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    let file_size = file.metadata()?.len();
    let start = file_size.saturating_sub(max_bytes as u64);
    file.seek(SeekFrom::Start(start))?;
    let mut tail = Vec::with_capacity(max_bytes);
    file.take(max_bytes as u64).read_to_end(&mut tail)?;
    Ok(tail)
}

fn command_output_with_timeout(
    command: &mut Command,
    timeout: Duration,
    cancellation: Option<&AtomicBool>,
) -> std::io::Result<Output> {
    let mut child = command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let stdout_reader = thread::spawn(move || read_stream_tail(BufReader::new(stdout), MAX_COMMAND_OUTPUT_BYTES));
    let stderr_reader = thread::spawn(move || read_stream_tail(BufReader::new(stderr), MAX_COMMAND_OUTPUT_BYTES));
    let started_at = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(error);
            }
            Ok(Some(status)) => break status,
            Ok(None)
                if cancellation.is_some_and(|token| token.load(Ordering::Acquire)) =>
            {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "server installation was cancelled",
                ));
            }
            Ok(None) if started_at.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "server installer exceeded its time limit",
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(100)),
        }
    };
    Ok(Output {
        status,
        stdout: stdout_reader.join().unwrap_or_default(),
        stderr: stderr_reader.join().unwrap_or_default(),
    })
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ServerQuickCommand {
    id: String,
    label: String,
    command: String,
}

fn default_quick_commands() -> Vec<ServerQuickCommand> {
    vec![
        ServerQuickCommand {
            id: "list".into(),
            label: "Игроки".into(),
            command: "list".into(),
        },
        ServerQuickCommand {
            id: "save-all".into(),
            label: "Сохранить мир".into(),
            command: "save-all".into(),
        },
    ]
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerRecord {
    name: String,
    minecraft_version: String,
    core: String,
    loader_version: Option<String>,
    memory_mb: u32,
    #[serde(default = "default_quick_commands")]
    quick_commands: Vec<ServerQuickCommand>,
    #[serde(default)]
    active_processor_count: Option<u16>,
    #[serde(default)]
    java_version: Option<u8>,
    java_path: String,
    jar_file: String,
    installed_at: u64,
}

pub(crate) struct ServerContentInfo {
    pub(crate) root: PathBuf,
    pub(crate) minecraft_version: String,
    pub(crate) core: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ServerSummary {
    name: String,
    minecraft_version: String,
    core: String,
    loader_version: Option<String>,
    memory_mb: u32,
    quick_commands: Vec<ServerQuickCommand>,
    active_processor_count: Option<u16>,
    java_version: Option<u8>,
    installed_at: u64,
    status: String,
    uptime_seconds: u64,
    pid: Option<u32>,
    memory_used_mb: Option<u64>,
    cpu_usage_percent: Option<f32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ServerInstallOptions {
    cores: Vec<String>,
    loader_versions: HashMap<String, Vec<crate::minecraft::LoaderVersion>>,
    warnings: Vec<String>,
}

struct DownloadHashes {
    sha1: String,
    sha256: String,
    md5: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ServerLog {
    server_name: String,
    line: String,
    level: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerExit {
    server_name: String,
    success: bool,
    code: Option<i32>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerStatusEvent {
    name: String,
    status: String,
}

struct ManagedProcess {
    child: Child,
    started_at: u64,
}

static SERVER_PROCESSES: OnceLock<Mutex<HashMap<String, Arc<Mutex<ManagedProcess>>>>> =
    OnceLock::new();
static SERVER_INSTALL_CANCELLATIONS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    OnceLock::new();
static SERVER_STARTING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static SERVER_CONTENT_MUTATIONS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static SERVER_STATES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static SERVER_SYSTEM: OnceLock<Mutex<sysinfo::System>> = OnceLock::new();

fn processes() -> &'static Mutex<HashMap<String, Arc<Mutex<ManagedProcess>>>> {
    SERVER_PROCESSES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn server_install_cancellations() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    SERVER_INSTALL_CANCELLATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

struct ServerInstallRegistration {
    name: String,
    cancellation: Arc<AtomicBool>,
}

impl ServerInstallRegistration {
    fn acquire(name: &str) -> Result<Self, String> {
        let mut active = server_install_cancellations()
            .lock()
            .map_err(|_| "Не удалось зарегистрировать установку сервера.".to_string())?;
        if active.contains_key(name) {
            return Err("Установка сервера с таким именем уже выполняется.".into());
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        active.insert(name.to_string(), Arc::clone(&cancellation));
        Ok(Self { name: name.to_string(), cancellation })
    }
}

impl Drop for ServerInstallRegistration {
    fn drop(&mut self) {
        if let Ok(mut active) = server_install_cancellations().lock()
            && active.get(&self.name).is_some_and(|current| Arc::ptr_eq(current, &self.cancellation))
        {
            active.remove(&self.name);
        }
    }
}

async fn wait_for_install_cancellation(cancellation: &AtomicBool) {
    while !cancellation.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn check_install_cancellation(cancellation: &AtomicBool) -> Result<(), String> {
    if cancellation.load(Ordering::Acquire) {
        Err("Установка сервера отменена.".into())
    } else {
        Ok(())
    }
}

fn starting_servers() -> &'static Mutex<HashSet<String>> {
    SERVER_STARTING.get_or_init(|| Mutex::new(HashSet::new()))
}

fn server_content_mutations() -> &'static Mutex<HashSet<String>> {
    SERVER_CONTENT_MUTATIONS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn terminate_unregistered_process(process: Arc<Mutex<ManagedProcess>>) {
    if let Ok(mut managed) = process.lock() {
        let _ = managed.child.kill();
    }
    thread::spawn(move || {
        let mut managed = process
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = managed.child.wait();
    });
}

struct ServerStartReservation(String);

impl ServerStartReservation {
    fn acquire(name: &str) -> Result<Self, String> {
        let mut starting = starting_servers()
            .lock()
            .map_err(|_| "Не удалось зарезервировать запуск сервера.".to_string())?;
        let already_running = processes()
            .lock()
            .map_err(|_| "Не удалось проверить процессы серверов.".to_string())?
            .contains_key(name);
        let content_mutation = server_content_mutations()
            .lock()
            .map_err(|_| "Не удалось проверить изменения контента сервера.".to_string())?
            .contains(name);
        if already_running || content_mutation || !starting.insert(name.to_string()) {
            return Err("Сервер уже запущен, запускается или изменяется.".into());
        }
        Ok(Self(name.to_string()))
    }
}

pub(crate) struct ServerContentMutationReservation(String);

impl ServerContentMutationReservation {
    pub(crate) fn acquire(name: &str) -> Result<Self, String> {
        let starting = starting_servers()
            .lock()
            .map_err(|_| "Не удалось проверить запуск сервера.".to_string())?;
        if starting.contains(name)
            || processes()
                .lock()
                .map_err(|_| "Не удалось проверить процесс сервера.".to_string())?
                .contains_key(name)
        {
            return Err("Сначала остановите сервер, затем измените его содержимое.".into());
        }
        let mut mutations = server_content_mutations()
            .lock()
            .map_err(|_| "Не удалось зарезервировать изменение контента сервера.".to_string())?;
        if !mutations.insert(name.to_string()) {
            return Err("Для этого сервера уже выполняется изменение контента.".into());
        }
        drop(starting);
        Ok(Self(name.to_string()))
    }
}

impl Drop for ServerContentMutationReservation {
    fn drop(&mut self) {
        if let Ok(mut mutations) = server_content_mutations().lock() {
            mutations.remove(&self.0);
        }
    }
}

impl Drop for ServerStartReservation {
    fn drop(&mut self) {
        if let Ok(mut starting) = starting_servers().lock() {
            starting.remove(&self.0);
        }
    }
}

fn states() -> &'static Mutex<HashMap<String, String>> {
    SERVER_STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn server_system() -> &'static Mutex<sysinfo::System> {
    SERVER_SYSTEM.get_or_init(|| Mutex::new(sysinfo::System::new()))
}

fn refresh_server_metrics() -> Result<(), String> {
    let process_ids = processes()
        .lock()
        .map_err(|_| "Не удалось получить процессы серверов.".to_string())?
        .values()
        .filter_map(|process| process.lock().ok().map(|process| sysinfo::Pid::from_u32(process.child.id())))
        .collect::<Vec<_>>();
    if process_ids.is_empty() {
        return Ok(());
    }
    server_system()
        .lock()
        .map_err(|_| "Не удалось обновить метрики серверов.".to_string())?
        .refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&process_ids),
            true,
            sysinfo::ProcessRefreshKind::nothing()
                .with_memory()
                .with_cpu(),
        );
    Ok(())
}

fn process_metrics(process_id: u32, refresh: bool) -> (Option<u64>, Option<f32>) {
    let process_id = sysinfo::Pid::from_u32(process_id);
    let Ok(mut system) = server_system().lock() else {
        return (None, None);
    };
    if refresh {
        system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[process_id]),
            false,
            sysinfo::ProcessRefreshKind::nothing()
                .with_memory()
                .with_cpu(),
        );
    }
    system.process(process_id).map_or((None, None), |process| {
        (
            Some(process.memory() / 1024 / 1024),
            Some(process.cpu_usage()),
        )
    })
}

fn servers_root(app: &AppHandle) -> Result<PathBuf, String> {
    let root = app
        .path()
        .app_config_dir()
        .map_err(|error| error.to_string())?
        .parent()
        .ok_or_else(|| "Не удалось определить папку AppData.".to_string())?
        .join("MyLauncher")
        .join("servers");
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    Ok(root)
}

fn server_dir(app: &AppHandle, name: &str) -> Result<PathBuf, String> {
    validate_folder_name(name)?;
    let root = servers_root(app)?;
    let directory = root.join(name);
    if directory.exists() {
        let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
        let canonical_directory = directory
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if canonical_directory.parent() != Some(canonical_root.as_path()) {
            return Err("Путь сервера выходит за пределы хранилища.".into());
        }
    }
    Ok(directory)
}

pub(crate) fn server_content_info(
    app: &AppHandle,
    name: &str,
) -> Result<ServerContentInfo, String> {
    let directory = server_dir(app, name)?;
    if !fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        return Err("Сервер не найден.".into());
    }
    let record = read_record(&directory)?;
    Ok(ServerContentInfo {
        root: directory,
        minecraft_version: record.minecraft_version,
        core: record.core,
    })
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn minecraft_version_at_least(version: &str, required: (u16, u16, u16)) -> bool {
    let mut parts = version
        .split('.')
        .filter_map(|part| part.parse::<u16>().ok());
    let current = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    current >= required
}

fn read_record(directory: &Path) -> Result<ServerRecord, String> {
    let contents = fs::read(directory.join(SERVER_METADATA)).map_err(|error| error.to_string())?;
    serde_json::from_slice(&contents).map_err(|error| error.to_string())
}

fn status_for(name: &str) -> String {
    states()
        .lock()
        .ok()
        .and_then(|values| values.get(name).cloned())
        .unwrap_or_else(|| "stopped".into())
}

fn summary_for_with_metrics(
    name: String,
    directory: &Path,
    refresh_metrics: bool,
) -> Result<ServerSummary, String> {
    let record = read_record(directory)?;
    let status = status_for(&record.name);
    let process = processes()
        .lock()
        .map_err(|_| "Не удалось получить состояние процесса сервера.".to_string())?
        .get(&name)
        .cloned();
    let (pid, uptime_seconds) = process.map_or((None, 0), |process| {
        process.lock().map_or((None, 0), |process| {
            (
                Some(process.child.id()),
                now_seconds().saturating_sub(process.started_at),
            )
        })
    });
    let (memory_used_mb, cpu_usage_percent) =
        pid.map_or((None, None), |process_id| process_metrics(process_id, refresh_metrics));
    Ok(ServerSummary {
        name: record.name,
        minecraft_version: record.minecraft_version,
        core: record.core,
        loader_version: record.loader_version,
        memory_mb: record.memory_mb,
        quick_commands: record.quick_commands,
        active_processor_count: record.active_processor_count,
        java_version: record.java_version,
        installed_at: record.installed_at,
        status,
        uptime_seconds,
        pid,
        memory_used_mb,
        cpu_usage_percent,
    })
}

fn summary_for(name: String, directory: &Path) -> Result<ServerSummary, String> {
    summary_for_with_metrics(name, directory, true)
}

#[tauri::command]
pub(crate) fn list_servers(app: AppHandle) -> Result<Vec<ServerSummary>, String> {
    refresh_server_metrics()?;
    let root = servers_root(&app)?;
    let mut servers = fs::read_dir(root)
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            summary_for_with_metrics(name, &entry.path(), false).ok()
        })
        .collect::<Vec<_>>();
    servers.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));
    Ok(servers)
}

#[tauri::command]
pub(crate) fn open_server_directory(app: AppHandle, name: String) -> Result<(), String> {
    let root = servers_root(&app)?;
    let directory = server_dir(&app, &name)?;
    if !fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        return Err("Папка сервера не найдена.".into());
    }
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    let canonical_directory = directory
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if canonical_directory.parent() != Some(canonical_root.as_path()) {
        return Err("Путь сервера выходит за пределы хранилища.".into());
    }
    crate::open_in_explorer(&canonical_directory)
}

#[tauri::command]
pub(crate) fn delete_server(app: AppHandle, name: String) -> Result<(), String> {
    let mut running = processes()
        .lock()
        .map_err(|_| "Не удалось проверить состояние сервера.".to_string())?;
    if running.contains_key(&name) {
        return Err("Сначала остановите сервер, затем удалите его.".into());
    }
    let root = servers_root(&app)?;
    let directory = server_dir(&app, &name)?;
    if !fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        return Err("Папка сервера не найдена.".into());
    }
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    let canonical_directory = directory
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if canonical_directory.parent() != Some(canonical_root.as_path()) {
        return Err("Путь сервера выходит за пределы хранилища.".into());
    }
    fs::remove_dir_all(canonical_directory)
        .map_err(|error| format!("Не удалось удалить папку сервера: {error}"))?;
    if let Ok(mut values) = states().lock() {
        values.remove(&name);
    }
    running.remove(&name);
    Ok(())
}

fn server_jvm_args(memory_mb: u32, processor_argument: Option<String>) -> String {
    let mut arguments = format!(
        "-Xms{}M\n-Xmx{}M\n-XX:+UseG1GC\n-XX:+ParallelRefProcEnabled\n-XX:MaxGCPauseMillis=200\n-XX:+DisableExplicitGC\n-XX:+AlwaysPreTouch\n",
        512.min(memory_mb),
        memory_mb
    );
    if let Some(processor_argument) = processor_argument {
        arguments.push_str(&processor_argument);
        arguments.push('\n');
    }
    arguments
}

fn validate_quick_commands(commands: &[ServerQuickCommand]) -> Result<(), String> {
    if commands.len() > MAX_QUICK_COMMANDS {
        return Err(format!(
            "Можно сохранить не более {MAX_QUICK_COMMANDS} быстрых команд."
        ));
    }
    let mut ids = std::collections::HashSet::new();
    for item in commands {
        if item.id.is_empty()
            || item.id.len() > 64
            || !item
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || !ids.insert(&item.id)
            || item.label.trim().is_empty()
            || item.label.chars().count() > 32
            || item.label.chars().any(char::is_control)
            || item.command.trim().is_empty()
            || item.command.len() > 128
            || item.command.chars().any(char::is_control)
        {
            return Err("Проверьте названия и команды быстрых кнопок.".into());
        }
    }
    Ok(())
}

fn cpu_count_argument(active_processor_count: Option<u16>, java_version: u8) -> Option<String> {
    if java_version >= 10 {
        active_processor_count.map(|count| format!("-XX:ActiveProcessorCount={count}"))
    } else {
        None
    }
}

#[tauri::command]
pub(crate) fn update_server_controls(
    app: AppHandle,
    name: String,
    quick_commands: Vec<ServerQuickCommand>,
    active_processor_count: Option<u16>,
) -> Result<ServerSummary, String> {
    validate_quick_commands(&quick_commands)?;
    let available_processors = std::thread::available_parallelism()
        .map(|count| count.get().min(u16::MAX as usize) as u16)
        .unwrap_or(1);
    if active_processor_count.is_some_and(|count| count == 0 || count > available_processors) {
        return Err(format!(
            "Укажите от 1 до {available_processors} CPU-потоков или выберите «Авто»."
        ));
    }
    if processes()
        .lock()
        .map_err(|_| "Не удалось проверить состояние сервера.".to_string())?
        .contains_key(&name)
    {
        return Err("Остановите сервер, чтобы изменить его настройки запуска.".into());
    }
    let directory = server_dir(&app, &name)?;
    let mut record = read_record(&directory)?;
    record.quick_commands = quick_commands;
    record.active_processor_count = active_processor_count;
    if record.jar_file == "run.bat" {
        fs::write(
            directory.join("user_jvm_args.txt"),
            server_jvm_args(
                record.memory_mb,
                cpu_count_argument(active_processor_count, record.java_version.unwrap_or(21)),
            ),
        )
        .map_err(|error| format!("Не удалось обновить аргументы JVM: {error}"))?;
    }
    fs::write(
        directory.join(SERVER_METADATA),
        serde_json::to_vec_pretty(&record).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("Не удалось сохранить настройки сервера: {error}"))?;
    summary_for(name, &directory)
}

#[tauri::command]
pub(crate) fn update_server_memory(
    app: AppHandle,
    name: String,
    memory_mb: u32,
) -> Result<ServerSummary, String> {
    if memory_mb < 512 || memory_mb > get_memory_limit_mb() {
        return Err(format!(
            "Укажите от 512 до {} МБ оперативной памяти.",
            get_memory_limit_mb()
        ));
    }
    if processes()
        .lock()
        .map_err(|_| "Не удалось проверить состояние сервера.".to_string())?
        .contains_key(&name)
    {
        return Err("Чтобы изменить выделенную память, сначала остановите сервер.".into());
    }
    let directory = server_dir(&app, &name)?;
    let mut record = read_record(&directory)?;
    record.memory_mb = memory_mb;
    if record.jar_file == "run.bat" {
        fs::write(
            directory.join("user_jvm_args.txt"),
            server_jvm_args(
                memory_mb,
                cpu_count_argument(
                    record.active_processor_count,
                    record.java_version.unwrap_or(21),
                ),
            ),
        )
        .map_err(|error| format!("Не удалось обновить аргументы JVM: {error}"))?;
    }
    fs::write(
        directory.join(SERVER_METADATA),
        serde_json::to_vec_pretty(&record).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("Не удалось сохранить настройки сервера: {error}"))?;
    summary_for(name, &directory)
}

async fn download(
    client: &reqwest::Client,
    url: &str,
    destination: &Path,
    cancellation: &AtomicBool,
) -> Result<DownloadHashes, String> {
    check_install_cancellation(cancellation)?;
    let response = match futures::future::select(
        Box::pin(client.get(url).send()),
        Box::pin(wait_for_install_cancellation(cancellation)),
    )
    .await
    {
        futures::future::Either::Left((Ok(response), _)) => response,
        futures::future::Either::Left((Err(error), _)) => {
            return Err(format!("Ошибка загрузки: {error}"));
        }
        futures::future::Either::Right(((), _)) => {
            return Err("Установка сервера отменена.".into());
        }
    };
    let mut response = response
        .error_for_status()
        .map_err(|error| format!("Источник сервера вернул ошибку: {error}"))?;
    let mut file = tokio::fs::File::create(destination)
        .await
        .map_err(|error| format!("Не удалось создать файл сервера: {error}"))?;
    let mut sha1 = Sha1::new();
    let mut sha256 = Sha256::new();
    let mut md5 = Md5::new();
    let mut size = 0_u64;
    loop {
        let chunk = match futures::future::select(
            Box::pin(response.chunk()),
            Box::pin(wait_for_install_cancellation(cancellation)),
        )
        .await
        {
            futures::future::Either::Left((Ok(Some(chunk)), _)) => chunk,
            futures::future::Either::Left((Ok(None), _)) => break,
            futures::future::Either::Left((Err(error), _)) => {
                return Err(format!("Не удалось прочитать загрузку: {error}"));
            }
            futures::future::Either::Right(((), _)) => {
                drop(file);
                let _ = tokio::fs::remove_file(destination).await;
                return Err("Установка сервера отменена.".into());
            }
        };
        size = size.saturating_add(chunk.len() as u64);
        if size > 1_500_000_000 {
            return Err("Файл серверного ядра превышает допустимый размер.".into());
        }
        sha1.update(&chunk);
        sha256.update(&chunk);
        md5.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|error| format!("Не удалось сохранить файл сервера: {error}"))?;
    }
    file.flush()
        .await
        .map_err(|error| format!("Не удалось завершить запись файла сервера: {error}"))?;
    Ok(DownloadHashes {
        sha1: format!("{:x}", sha1.finalize()),
        sha256: format!("{:x}", sha256.finalize()),
        md5: format!("{:x}", md5.finalize()),
    })
}

fn parse_sha1_checksum(contents: &[u8]) -> Result<String, String> {
    let value = String::from_utf8_lossy(contents);
    let checksum = value.split_whitespace().next().unwrap_or_default();
    if checksum.len() != 40 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Источник Maven вернул некорректный SHA-1 checksum.".into());
    }
    Ok(checksum.to_ascii_lowercase())
}

async fn maven_sha1_checksum(client: &reqwest::Client, url: &str) -> Result<Option<String>, String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("Не удалось получить checksum загрузчика: {error}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    response
        .error_for_status_ref()
        .map_err(|error| format!("Источник checksum вернул ошибку: {error}"))?;
    if response.content_length().is_some_and(|length| length > 256) {
        return Err("Ответ checksum превышает допустимый размер.".into());
    }
    let mut contents = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("Не удалось прочитать checksum: {error}"))?
    {
        if contents.len().saturating_add(chunk.len()) > 256 {
            return Err("Ответ checksum превышает допустимый размер.".into());
        }
        contents.extend_from_slice(&chunk);
    }
    parse_sha1_checksum(&contents).map(Some)
}

async fn json(client: &reqwest::Client, url: &str) -> Result<Value, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("Не удалось получить данные сервера: {error}"))?
        .error_for_status()
        .map_err(|error| format!("Источник серверных данных вернул ошибку: {error}"))?;
    bounded_json_response(response).await.map_err(|error| {
        format!("Некорректный ответ источника серверных данных: {error}")
    })
}

async fn bounded_json_response(mut response: reqwest::Response) -> Result<Value, String> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_SERVER_JSON_BYTES as u64)
    {
        return Err("Ответ превышает допустимый размер.".into());
    }
    let mut contents = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| error.to_string())?
    {
        if contents.len().saturating_add(chunk.len()) > MAX_SERVER_JSON_BYTES {
            return Err("Ответ превышает допустимый размер.".into());
        }
        contents.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&contents).map_err(|error| error.to_string())
}

fn string_field<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("В ответе источника отсутствует поле {field}."))
}

fn server_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("MyLauncher/0.1.0")
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(900))
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

async fn server_required_java_version(
    client: &reqwest::Client,
    minecraft_version: &str,
) -> Result<u8, String> {
    let manifest = json(
        client,
        "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json",
    )
    .await?;
    let version_url = manifest
        .get("versions")
        .and_then(Value::as_array)
        .and_then(|versions| {
            versions
                .iter()
                .find(|item| item.get("id").and_then(Value::as_str) == Some(minecraft_version))
        })
        .and_then(|item| item.get("url"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!("Версия Minecraft {minecraft_version} не найдена в официальном манифесте.")
        })?;
    let metadata = json(client, version_url).await?;
    let java_version =
        crate::minecraft::required_java_version(&metadata, minecraft_version).unwrap_or(21);
    if java_version < 8 {
        return Err(format!(
            "Для Minecraft {minecraft_version} определена неподдерживаемая версия Java {java_version}."
        ));
    }
    Ok(java_version)
}

async fn vanilla_server_available(
    client: &reqwest::Client,
    minecraft_version: &str,
) -> Result<bool, String> {
    let manifest = json(
        client,
        "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json",
    )
    .await?;
    let Some(version_url) = manifest
        .get("versions")
        .and_then(Value::as_array)
        .and_then(|versions| {
            versions
                .iter()
                .find(|item| item.get("id").and_then(Value::as_str) == Some(minecraft_version))
        })
        .and_then(|item| item.get("url"))
        .and_then(Value::as_str)
    else {
        return Ok(false);
    };
    let details = json(client, version_url).await?;
    Ok(details.pointer("/downloads/server/url").is_some())
}

fn paper_stable_server_build(builds: &[Value]) -> Option<&Value> {
    builds.iter().find(|build| {
        build.get("channel").and_then(Value::as_str) == Some("STABLE")
            && build
                .pointer("/downloads/server:default/url")
                .and_then(Value::as_str)
                .is_some()
    })
}

async fn paper_server_available(
    client: &reqwest::Client,
    minecraft_version: &str,
) -> Result<bool, String> {
    let response = client
        .get(format!(
            "https://fill.papermc.io/v3/projects/paper/versions/{minecraft_version}/builds"
        ))
        .send()
        .await
        .map_err(|error| format!("Не удалось проверить сборки Paper: {error}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(false);
    }
    let response = response
        .error_for_status()
        .map_err(|error| format!("PaperMC API вернул ошибку: {error}"))?;
    let builds = bounded_json_response(response)
        .await
        .map_err(|error| format!("Некорректный ответ PaperMC API: {error}"))?;
    Ok(builds
        .as_array()
        .is_some_and(|builds| paper_stable_server_build(builds).is_some()))
}

async fn purpur_server_available(
    client: &reqwest::Client,
    minecraft_version: &str,
) -> Result<bool, String> {
    let versions = json(client, "https://api.purpurmc.org/v2/purpur").await?;
    Ok(versions
        .get("versions")
        .and_then(Value::as_array)
        .is_some_and(|versions| {
            versions
                .iter()
                .any(|version| version.as_str() == Some(minecraft_version))
        }))
}

#[tauri::command]
pub(crate) async fn get_server_install_options(
    minecraft_version: String,
) -> Result<ServerInstallOptions, String> {
    if minecraft_version.is_empty()
        || minecraft_version.len() > 64
        || minecraft_version.contains(['/', '\\'])
    {
        return Err("Некорректная версия Minecraft.".into());
    }
    if !minecraft_version_at_least(&minecraft_version, (1, 5, 2)) {
        return Err("Установка серверов поддерживается начиная с Minecraft 1.5.2.".into());
    }
    let client = server_client()?;
    let (vanilla, paper, purpur, fabric, forge, neoforge) = futures::join!(
        vanilla_server_available(&client, &minecraft_version),
        paper_server_available(&client, &minecraft_version),
        purpur_server_available(&client, &minecraft_version),
        crate::minecraft::get_loader_versions("fabric".into(), minecraft_version.clone()),
        crate::minecraft::get_loader_versions("forge".into(), minecraft_version.clone()),
        async {
            if crate::minecraft::neoforge_is_available(&minecraft_version) {
                crate::minecraft::get_loader_versions("neoforge".into(), minecraft_version.clone())
                    .await
            } else {
                Ok(Vec::new())
            }
        },
    );

    let mut warnings = Vec::new();
    let vanilla = match vanilla {
        Ok(available) => available,
        Err(error) => {
            warnings.push(format!("Vanilla: {error}"));
            false
        }
    };
    let paper = match paper {
        Ok(available) => available,
        Err(error) => {
            warnings.push(format!("Paper: {error}"));
            false
        }
    };
    let purpur = match purpur {
        Ok(available) => available,
        Err(error) => {
            warnings.push(format!("Purpur: {error}"));
            false
        }
    };
    let mut loader_versions = HashMap::new();
    for (loader, result) in [("fabric", fabric), ("forge", forge), ("neoforge", neoforge)] {
        match result {
            Ok(versions) if !versions.is_empty() => {
                loader_versions.insert(loader.to_string(), versions);
            }
            Ok(_) => {}
            Err(error) => warnings.push(format!("{}: {error}", loader_label(loader))),
        }
    }

    let mut cores = Vec::new();
    if paper {
        cores.push("paper".into());
    }
    if purpur {
        cores.push("purpur".into());
    }
    for loader in ["fabric", "forge", "neoforge"] {
        if loader_versions.contains_key(loader) {
            cores.push(loader.into());
        }
    }
    if vanilla {
        cores.push("vanilla".into());
    }
    Ok(ServerInstallOptions {
        cores,
        loader_versions,
        warnings,
    })
}

fn loader_label(loader: &str) -> &str {
    match loader {
        "fabric" => "Fabric",
        "forge" => "Forge",
        "neoforge" => "NeoForge",
        _ => loader,
    }
}

#[tauri::command]
pub(crate) async fn install_server(
    app: AppHandle,
    name: String,
    minecraft_version: String,
    core: String,
    loader_version: Option<String>,
    memory_mb: u32,
    eula_accepted: bool,
) -> Result<ServerSummary, String> {
    validate_folder_name(&name)?;
    if minecraft_version.is_empty()
        || minecraft_version.len() > 64
        || minecraft_version.contains(['/', '\\'])
    {
        return Err("Некорректная версия Minecraft.".into());
    }
    if !minecraft_version_at_least(&minecraft_version, (1, 5, 2)) {
        return Err("Установка серверов поддерживается начиная с Minecraft 1.5.2.".into());
    }
    if memory_mb < 512 || memory_mb > get_memory_limit_mb() {
        return Err("Выбранный объём памяти выходит за допустимые пределы.".into());
    }
    if !eula_accepted {
        return Err("Перед установкой необходимо принять Minecraft EULA.".into());
    }
    let _registration = ServerInstallRegistration::acquire(&name)?;
    let cancellation = Arc::clone(&_registration.cancellation);
    check_install_cancellation(&cancellation)?;
    if !matches!(
        core.as_str(),
        "vanilla" | "paper" | "purpur" | "fabric" | "forge" | "neoforge"
    ) {
        return Err("Неизвестный тип серверного ядра.".into());
    }
    if core == "neoforge" && !minecraft_version_at_least(&minecraft_version, (1, 20, 1)) {
        return Err("NeoForge поддерживает Minecraft 1.20.1 и новее.".into());
    }
    let requires_loader = matches!(core.as_str(), "forge" | "neoforge");
    if requires_loader && loader_version.as_deref().is_none_or(str::is_empty) {
        return Err("Укажите версию загрузчика для выбранного ядра.".into());
    }
    let client = server_client()?;
    let java_version = server_required_java_version(&client, &minecraft_version).await?;
    check_install_cancellation(&cancellation)?;
    let java_path = crate::java::install_java_runtime(app.clone(), java_version).await?;
    check_install_cancellation(&cancellation)?;

    let directory = server_dir(&app, &name)?;
    if directory.exists() {
        return Err("Папка сервера уже существует. Выберите другое имя.".into());
    }
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let install_result: Result<ServerSummary, String> = async {
        let (jar_file, resolved_loader) = install_core(
            &client,
            &directory,
            &minecraft_version,
            &core,
            loader_version,
            java_path.clone(),
            &cancellation,
        )
        .await?;
        check_install_cancellation(&cancellation)?;

        fs::write(directory.join("eula.txt"), "eula=true\n").map_err(|error| error.to_string())?;
        if jar_file == "run.bat" {
            fs::write(
                directory.join("user_jvm_args.txt"),
                server_jvm_args(memory_mb, None),
            )
            .map_err(|error| error.to_string())?;
        }
        let mut child_directories = vec!["logs", "backups"];
        match core.as_str() {
            "paper" | "purpur" => child_directories.push("plugins"),
            "fabric" | "forge" | "neoforge" => child_directories.push("mods"),
            _ => {}
        }
        for child in child_directories {
            fs::create_dir_all(directory.join(child)).map_err(|error| error.to_string())?;
        }
        let record = ServerRecord {
            name: name.clone(),
            minecraft_version,
            core,
            loader_version: resolved_loader,
            memory_mb,
            quick_commands: default_quick_commands(),
            active_processor_count: None,
            java_version: Some(java_version),
            java_path,
            jar_file,
            installed_at: now_seconds(),
        };
        let contents = serde_json::to_vec_pretty(&record).map_err(|error| error.to_string())?;
        fs::write(directory.join(SERVER_METADATA), contents).map_err(|error| error.to_string())?;
        fs::write(
            directory.join("server.properties"),
            "motd=A Minecraft Server\nserver-port=25565\nmax-players=20\ndifficulty=easy\ngamemode=survival\nwhite-list=false\npvp=true\nview-distance=10\n",
        )
        .map_err(|error| error.to_string())?;
        summary_for(name, &directory)
    }
    .await;
    if install_result.is_err() {
        let _ = fs::remove_dir_all(&directory);
    }
    install_result
}

async fn install_core(
    client: &reqwest::Client,
    directory: &Path,
    minecraft_version: &str,
    core: &str,
    loader_version: Option<String>,
    java_path: String,
    cancellation: &Arc<AtomicBool>,
) -> Result<(String, Option<String>), String> {
    check_install_cancellation(cancellation)?;
    match core {
        "vanilla" => {
            let manifest = json(
                client,
                "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json",
            )
            .await?;
            let version_url = manifest
                .get("versions")
                .and_then(Value::as_array)
                .and_then(|versions| {
                    versions.iter().find(|item| {
                        item.get("id").and_then(Value::as_str) == Some(minecraft_version)
                    })
                })
                .and_then(|item| item.get("url"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    "Серверная версия недоступна: в манифесте нет этой версии.".to_string()
                })?;
            let version = json(client, version_url).await?;
            let server = version.pointer("/downloads/server").ok_or_else(|| {
                "Серверная версия недоступна для выбранной версии Minecraft.".to_string()
            })?;
            let url = string_field(server, "url")?;
            let expected_sha1 = string_field(server, "sha1")?;
            let actual_hashes = download(client, url, &directory.join("server.jar"), cancellation).await?;
            if actual_hashes.sha1 != expected_sha1 {
                return Err("Проверка SHA-1 серверного файла не пройдена.".into());
            }
            Ok(("server.jar".into(), None))
        }
        "paper" => {
            let builds: Vec<Value> = json(
                client,
                &format!(
                    "https://fill.papermc.io/v3/projects/paper/versions/{minecraft_version}/builds"
                ),
            )
            .await?
            .as_array()
            .cloned()
            .ok_or_else(|| "Некорректный список сборок Paper.".to_string())?;
            let build = paper_stable_server_build(&builds)
                .ok_or_else(|| "Для этой версии не найдено стабильных сборок Paper.".to_string())?;
            let download_info = build
                .pointer("/downloads/server:default")
                .ok_or("В ответе PaperMC отсутствует серверный файл.")?;
            let build_id = build
                .get("id")
                .and_then(Value::as_i64)
                .ok_or("В ответе PaperMC отсутствует номер сборки.")?;
            let jar = format!("paper-{minecraft_version}-{build_id}.jar");
            let url = string_field(download_info, "url")?;
            let expected_sha256 = download_info
                .pointer("/checksums/sha256")
                .and_then(Value::as_str)
                .ok_or("В ответе PaperMC отсутствует SHA-256 серверного файла.")?;
            let hashes = download(client, url, &directory.join(&jar), cancellation).await?;
            if hashes.sha256 != expected_sha256 {
                return Err("Проверка SHA-256 файла Paper не пройдена.".into());
            }
            Ok((jar, None))
        }
        "purpur" => {
            let jar = format!("purpur-{minecraft_version}.jar");
            let metadata: Value = json(
                client,
                &format!("https://api.purpurmc.org/v2/purpur/{minecraft_version}/latest"),
            )
            .await?;
            let expected_md5 = string_field(&metadata, "md5")?;
            let hashes = download(
                client,
                &format!("https://api.purpurmc.org/v2/purpur/{minecraft_version}/latest/download"),
                &directory.join(&jar),
                cancellation,
            )
            .await?;
            if !hashes.md5.eq_ignore_ascii_case(expected_md5) {
                return Err("Проверка MD5 файла Purpur не пройдена.".into());
            }
            Ok((jar, None))
        }
        "fabric" => {
            let loaders = json(
                client,
                &format!("https://meta.fabricmc.net/v2/versions/loader/{minecraft_version}"),
            )
            .await?;
            let loader = loader_version.unwrap_or_else(|| {
                loaders
                    .as_array()
                    .and_then(|items| items.first())
                    .and_then(|item| item.pointer("/loader/version"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            });
            if loader.is_empty() {
                return Err("Для этой версии не найден загрузчик Fabric.".into());
            }
            let installers =
                json(client, "https://meta.fabricmc.net/v2/versions/installer").await?;
            let installer = installers
                .as_array()
                .and_then(|items| items.first())
                .and_then(|item| item.get("version"))
                .and_then(Value::as_str)
                .ok_or_else(|| "Не удалось определить версию установщика Fabric.".to_string())?;
            let jar = "fabric-server-launch.jar";
            let url = format!(
                "https://meta.fabricmc.net/v2/versions/loader/{minecraft_version}/{loader}/{installer}/server/jar"
            );
            download(client, &url, &directory.join(jar), cancellation).await?;
            Ok((jar.into(), Some(loader)))
        }
        "forge" | "neoforge" => {
            let loader = loader_version.ok_or("Укажите версию загрузчика.")?;
            if loader.len() > 64 || loader.contains(['/', '\\']) {
                return Err("Некорректная версия загрузчика.".into());
            }
            let (artifact, artifact_version, installer_name) = if core == "forge" {
                let version = format!("{minecraft_version}-{loader}");
                (
                    "net/minecraftforge/forge",
                    version.clone(),
                    format!("forge-{version}-installer.jar"),
                )
            } else {
                (
                    "net/neoforged/neoforge",
                    loader.clone(),
                    format!("neoforge-{loader}-installer.jar"),
                )
            };
            let base = if core == "forge" {
                "https://maven.minecraftforge.net"
            } else {
                "https://maven.neoforged.net/releases"
            };
            let url = format!("{base}/{artifact}/{artifact_version}/{installer_name}");
            let expected_sha1 = maven_sha1_checksum(client, &format!("{url}.sha1")).await?;
            let installer_path = directory.join(&installer_name);
            let hashes = download(client, &url, &installer_path, cancellation).await?;
            if expected_sha1
                .as_deref()
                .is_some_and(|expected| hashes.sha1 != expected)
            {
                return Err(format!("Проверка SHA-1 установщика {core} не пройдена."));
            }
            let install_dir = directory.to_path_buf();
            let installer_core = core.to_string();
            let installer_java = PathBuf::from(java_path).with_file_name("java.exe");
            let installer_cancellation = Arc::clone(cancellation);
            tokio::task::spawn_blocking(move || {
                let mut command = Command::new(installer_java);
                hide_console_window(&mut command);
                command
                    .arg("-jar")
                    .arg(&installer_path)
                    .arg("--installServer")
                    .arg(&install_dir)
                    .current_dir(&install_dir);
                let output = command_output_with_timeout(
                    &mut command,
                    SERVER_INSTALLER_TIMEOUT,
                    Some(&installer_cancellation),
                )
                    .map_err(|error| {
                        if installer_cancellation.load(Ordering::Acquire) {
                            return "Установка сервера отменена.".to_string();
                        }
                        format!("Не удалось запустить установщик {installer_core}: {error}")
                    })?;
                if !output.status.success() {
                    let log = String::from_utf8_lossy(&output.stdout);
                    let error = String::from_utf8_lossy(&output.stderr);
                    return Err(format!(
                        "Установка {installer_core} завершилась с ошибкой. {} {}",
                        log.chars()
                            .rev()
                            .take(1000)
                            .collect::<String>()
                            .chars()
                            .rev()
                            .collect::<String>(),
                        error
                            .chars()
                            .rev()
                            .take(1000)
                            .collect::<String>()
                            .chars()
                            .rev()
                            .collect::<String>()
                    ));
                }
                Ok::<(), String>(())
            })
            .await
            .map_err(|error| error.to_string())??;
            let launch_file = if directory.join("run.bat").is_file() {
                "run.bat".to_string()
            } else if core == "forge" {
                find_legacy_forge_launch_jar(directory)?.ok_or_else(|| {
                    "Установщик Forge завершился, но не создал запускаемый JAR.".to_string()
                })?
            } else {
                return Err(format!(
                    "Установщик {core} завершился, но run.bat не найден."
                ));
            };
            if core == "forge" && launch_file != "run.bat" {
                ensure_legacy_forge_server_classpath(
                    directory,
                    &directory.join(&launch_file),
                    minecraft_version,
                )?;
            }
            if launch_file == "run.bat" {
                let run_script_path = directory.join("run.bat");
                let run_script = fs::read_to_string(&run_script_path)
                    .map_err(|error| format!("Не удалось прочитать run.bat: {error}"))?;
                let sanitized_script = without_batch_pause_commands(&run_script);
                if sanitized_script != run_script {
                    fs::write(&run_script_path, sanitized_script)
                        .map_err(|error| format!("Не удалось обновить run.bat: {error}"))?;
                }
            }
            Ok((launch_file, Some(loader)))
        }
        _ => Err("Неизвестное серверное ядро.".into()),
    }
}

#[tauri::command]
pub(crate) fn cancel_server_install(name: String) -> Result<(), String> {
    validate_folder_name(&name)?;
    let active = server_install_cancellations()
        .lock()
        .map_err(|_| "Не удалось получить список установок серверов.".to_string())?;
    let cancellation = active
        .get(&name)
        .ok_or("Установка сервера не выполняется.")?;
    cancellation.store(true, Ordering::Release);
    Ok(())
}

fn without_batch_pause_commands(script: &str) -> String {
    script
        .split_inclusive('\n')
        .filter(|line| {
            let command = line.trim_start().trim_start_matches('@').trim_start();
            let command_name = command
                .split(|character: char| character.is_whitespace() || matches!(character, '<' | '>'))
                .next()
                .unwrap_or_default();
            !command_name.eq_ignore_ascii_case("pause")
        })
        .collect()
}

fn jar_manifest(path: &Path) -> Option<String> {
    let Ok(file) = fs::File::open(path) else {
        return None;
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return None;
    };
    let Ok(mut manifest) = archive.by_name("META-INF/MANIFEST.MF") else {
        return None;
    };
    let mut contents = String::new();
    manifest
        .read_to_string(&mut contents)
        .is_ok()
        .then_some(contents)
}

fn jar_has_main_class(path: &Path) -> bool {
    jar_manifest(path).is_some_and(|contents| {
        contents.lines().any(|line| {
            line.strip_prefix("Main-Class:")
                .is_some_and(|main_class| !main_class.trim().is_empty())
        })
    })
}

fn jar_manifest_class_path(path: &Path) -> Option<Vec<String>> {
    let manifest = jar_manifest(path)?;
    let mut class_path = String::new();
    let mut reading_class_path = false;
    for line in manifest.lines() {
        if let Some(value) = line.strip_prefix("Class-Path:") {
            class_path.push_str(value.trim());
            reading_class_path = true;
        } else if reading_class_path && line.starts_with(' ') {
            class_path.push_str(line.trim());
        } else if reading_class_path {
            break;
        }
    }
    (!class_path.is_empty()).then(|| class_path.split_whitespace().map(str::to_string).collect())
}

fn ensure_legacy_forge_server_classpath(
    directory: &Path,
    launch_jar: &Path,
    minecraft_version: &str,
) -> Result<(), String> {
    let Some(class_path) = jar_manifest_class_path(launch_jar) else {
        return Ok(());
    };
    for entry in class_path {
        if entry != "minecraft_server.jar" {
            continue;
        }
        let alias = directory.join(&entry);
        if alias.is_file() {
            continue;
        }
        let versioned_server = directory.join(format!("minecraft_server.{minecraft_version}.jar"));
        if versioned_server.is_file() {
            fs::copy(versioned_server, alias)
                .map_err(|error| format!("Не удалось подготовить JAR сервера Forge: {error}"))?;
        }
    }
    Ok(())
}

fn find_legacy_forge_launch_jar(directory: &Path) -> Result<Option<String>, String> {
    let mut candidates = fs::read_dir(directory)
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let file_name = path.file_name()?.to_str()?.to_string();
            let lower_name = file_name.to_ascii_lowercase();
            let is_jar = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"));
            (is_jar
                && !lower_name.contains("installer")
                && !lower_name.starts_with("minecraft_server"))
            .then_some((file_name, path))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(candidates
        .into_iter()
        .find(|(_, path)| jar_has_main_class(path))
        .map(|(file_name, _)| file_name))
}

fn emit_status(app: &AppHandle, name: &str, status: &str) {
    if let Ok(mut values) = states().lock() {
        values.insert(name.to_string(), status.to_string());
    }
    let _ = app.emit(
        "server-status",
        ServerStatusEvent {
            name: name.into(),
            status: status.into(),
        },
    );
}

fn log_level(line: &str) -> &'static str {
    let upper = line.to_ascii_uppercase();
    if upper.contains("ERROR") || upper.contains("CRASH") || upper.contains("FAILED") {
        "error"
    } else if upper.contains("WARN") {
        "warn"
    } else if upper.contains("DEBUG") {
        "debug"
    } else {
        "info"
    }
}

#[tauri::command]
pub(crate) fn read_server_log(app: AppHandle, name: String) -> Result<Vec<ServerLog>, String> {
    let path = server_dir(&app, &name)?.join("logs/latest.log");
    let bytes = match read_file_tail(&path, MAX_SERVER_LOG_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("Не удалось прочитать журнал сервера: {error}")),
    };
    let contents = String::from_utf8_lossy(&bytes);
    Ok(contents
        .lines()
        .rev()
        .take(2000)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|line| ServerLog {
            server_name: name.clone(),
            line: line.to_string(),
            level: log_level(line).to_string(),
        })
        .collect())
}

fn finish_process(
    app: &AppHandle,
    name: &str,
    process: &Arc<Mutex<ManagedProcess>>,
    exit: ExitStatus,
) {
    let is_current_process = processes().lock().is_ok_and(|mut values| {
        if values
            .get(name)
            .is_some_and(|current| Arc::ptr_eq(current, process))
        {
            values.remove(name);
            true
        } else {
            false
        }
    });
    if is_current_process {
        let next_status = if status_for(name) == "stopping" || exit.success() {
            "stopped"
        } else {
            "crashed"
        };
        emit_status(app, name, next_status);
        let _ = app.emit(
            "server-exit",
            ServerExit {
                server_name: name.to_string(),
                success: exit.success(),
                code: exit.code(),
            },
        );
    }
}

fn stream_server_logs(app: AppHandle, name: String, reader: impl Read + Send + 'static) {
    thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            if line.contains("Done (") && line.contains("For help") {
                emit_status(&app, &name, "running");
            }
            if line.contains("**** CRASH ****") || line.contains("Minecraft has crashed") {
                emit_status(&app, &name, "crashed");
            }
            let _ = app.emit(
                "server-log",
                ServerLog {
                    server_name: name.clone(),
                    level: log_level(&line).into(),
                    line,
                },
            );
        }
    });
}

#[tauri::command]
pub(crate) async fn start_server(app: AppHandle, name: String) -> Result<ServerSummary, String> {
    let _start_reservation = ServerStartReservation::acquire(&name)?;
    let directory = server_dir(&app, &name)?;
    let mut record = read_record(&directory)?;
    if !directory.join("eula.txt").is_file()
        || !fs::read_to_string(directory.join("eula.txt"))
            .unwrap_or_default()
            .lines()
            .any(|line| line.trim().eq_ignore_ascii_case("eula=true"))
    {
        return Err("Для запуска необходимо принять Minecraft EULA в файле eula.txt.".into());
    }
    if !directory.join(&record.jar_file).is_file() {
        return Err("Файл запуска сервера не найден. Переустановите ядро.".into());
    }
    let java_version = match record.java_version {
        Some(version) => version,
        None => server_required_java_version(&server_client()?, &record.minecraft_version).await?,
    };
    let java_path = crate::java::install_java_runtime(app.clone(), java_version).await?;
    if record.java_version != Some(java_version) || record.java_path != java_path {
        record.java_version = Some(java_version);
        record.java_path = java_path;
        fs::write(
            directory.join(SERVER_METADATA),
            serde_json::to_vec_pretty(&record).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    }
    if record.jar_file == "run.bat" {
        fs::write(
            directory.join("user_jvm_args.txt"),
            server_jvm_args(
                record.memory_mb,
                cpu_count_argument(record.active_processor_count, java_version),
            ),
        )
        .map_err(|error| format!("Не удалось обновить аргументы JVM: {error}"))?;
    }
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    let available_mb = (system.available_memory() / 1024 / 1024) as u32;
    if record.memory_mb > available_mb {
        return Err(format!(
            "Недостаточно свободной оперативной памяти: доступно около {available_mb} МБ."
        ));
    }

    let mut command = if record.jar_file == "run.bat" {
        let mut command = Command::new("cmd.exe");
        command.args(["/C", "run.bat", "nogui"]);
        let java_dir = PathBuf::from(&record.java_path)
            .with_file_name("java.exe")
            .parent()
            .ok_or("Некорректный путь к Java.")?
            .to_path_buf();
        let path = std::env::var_os("PATH").unwrap_or_default();
        command.env(
            "PATH",
            format!("{};{}", java_dir.to_string_lossy(), path.to_string_lossy()),
        );
        command
    } else {
        let mut command = Command::new(PathBuf::from(&record.java_path).with_file_name("java.exe"));
        command.args([
            format!("-Xms{}M", 512.min(record.memory_mb)),
            format!("-Xmx{}M", record.memory_mb),
            "-XX:+UseG1GC".into(),
            "-XX:+ParallelRefProcEnabled".into(),
            "-XX:MaxGCPauseMillis=200".into(),
            "-XX:+DisableExplicitGC".into(),
            "-XX:+AlwaysPreTouch".into(),
        ]);
        if let Some(argument) = cpu_count_argument(
            record.active_processor_count,
            record.java_version.unwrap_or(21),
        ) {
            command.arg(argument);
        }
        command.args(["-jar", record.jar_file.as_str(), "nogui"]);
        command
    };
    hide_console_window(&mut command);
    let mut child = command
        .current_dir(&directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!("Не удалось запустить сервер. Проверьте установку Java: {error}")
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Не удалось подключиться к консоли сервера.")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Не удалось подключиться к журналу ошибок сервера.")?;
    let process = Arc::new(Mutex::new(ManagedProcess {
        child,
        started_at: now_seconds(),
    }));
    let registration = match processes().lock() {
        Ok(mut registry) if !registry.contains_key(&name) => {
            registry.insert(name.clone(), process.clone());
            Ok(())
        }
        Ok(_) => Err("Сервер уже запущен; второй процесс остановлен.".to_string()),
        Err(_) => Err("Не удалось сохранить процесс сервера.".to_string()),
    };
    if let Err(error) = registration {
        terminate_unregistered_process(process);
        return Err(error);
    }
    emit_status(&app, &name, "starting");
    stream_server_logs(app.clone(), name.clone(), stdout);
    stream_server_logs(app.clone(), name.clone(), stderr);

    let monitor_app = app.clone();
    let monitor_name = name.clone();
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_millis(500));
            let status = process
                .lock()
                .ok()
                .and_then(|mut process| process.child.try_wait().ok().flatten());
            if let Some(exit) = status {
                finish_process(&monitor_app, &monitor_name, &process, exit);
                break;
            }
        }
    });
    summary_for(name, &directory)
}

#[tauri::command]
pub(crate) fn send_server_command(
    app: AppHandle,
    name: String,
    command: String,
) -> Result<(), String> {
    if command.trim().is_empty() || command.contains(['\n', '\r', '\0']) || command.len() > 2048 {
        return Err("Некорректная команда сервера.".into());
    }
    let process = processes()
        .lock()
        .map_err(|_| "Не удалось получить процесс сервера.".to_string())?
        .get(&name)
        .cloned()
        .ok_or("Сервер не запущен.")?;
    let mut process = process
        .lock()
        .map_err(|_| "Не удалось получить консоль сервера.".to_string())?;
    let stdin = process
        .child
        .stdin
        .as_mut()
        .ok_or("Консоль сервера недоступна.")?;
    writeln!(stdin, "{}", command.trim())
        .map_err(|error| format!("Не удалось отправить команду: {error}"))?;
    let _ = app.emit(
        "server-log",
        ServerLog {
            server_name: name,
            level: "command".into(),
            line: format!("> {}", command.trim()),
        },
    );
    Ok(())
}

#[tauri::command]
pub(crate) async fn stop_server(
    app: AppHandle,
    name: String,
    force: Option<bool>,
) -> Result<(), String> {
    let process = processes()
        .lock()
        .map_err(|_| "Не удалось получить процесс сервера.".to_string())?
        .get(&name)
        .cloned()
        .ok_or("Сервер не запущен.")?;
    emit_status(&app, &name, "stopping");
    {
        let mut managed = process
            .lock()
            .map_err(|_| "Не удалось получить процесс сервера.".to_string())?;
        if force.unwrap_or(false) {
            managed
                .child
                .kill()
                .map_err(|error| format!("Не удалось завершить сервер: {error}"))?;
        } else if let Some(stdin) = managed.child.stdin.as_mut() {
            writeln!(stdin, "save-all\nstop")
                .map_err(|error| format!("Не удалось остановить сервер: {error}"))?;
        }
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let exit = process
            .lock()
            .map_err(|_| "Не удалось проверить остановку сервера.".to_string())?
            .child
            .try_wait()
            .map_err(|error| error.to_string())?;
        if let Some(exit) = exit {
            finish_process(&app, &name, &process, exit);
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            process
                .lock()
                .map_err(|_| "Не удалось завершить сервер.".to_string())?
                .child
                .kill()
                .map_err(|error| format!("Не удалось принудительно завершить сервер: {error}"))?;
            let process_to_wait = Arc::clone(&process);
            let exit = tokio::task::spawn_blocking(move || {
                process_to_wait
                    .lock()
                    .map_err(|_| "Не удалось получить процесс сервера.".to_string())?
                    .child
                    .wait()
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())??;
            finish_process(&app, &name, &process, exit);
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tauri::command]
pub(crate) async fn restart_server(app: AppHandle, name: String) -> Result<ServerSummary, String> {
    if processes()
        .lock()
        .map_err(|_| "Не удалось получить процесс сервера.".to_string())?
        .contains_key(&name)
    {
        stop_server(app.clone(), name.clone(), None).await?;
        if let Ok(mut values) = processes().lock() {
            values.remove(&name);
        }
    }
    start_server(app, name).await
}

#[cfg(test)]
mod tests {
    use super::{
        ServerContentMutationReservation, ServerQuickCommand, ServerRecord, ServerStartReservation,
        cpu_count_argument, default_quick_commands,
        ensure_legacy_forge_server_classpath, find_legacy_forge_launch_jar,
        minecraft_version_at_least, paper_stable_server_build, server_jvm_args,
        parse_sha1_checksum, read_file_tail, read_stream_tail, without_batch_pause_commands,
        validate_quick_commands,
    };
    use std::{
        fs,
        io::Write,
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn write_test_jar(path: &Path, main_class: &str) {
        write_test_jar_with_class_path(path, main_class, None);
    }

    fn write_test_jar_with_class_path(path: &Path, main_class: &str, class_path: Option<&str>) {
        let file = fs::File::create(path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file(
                "META-INF/MANIFEST.MF",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        write!(
            archive,
            "Manifest-Version: 1.0\r\nMain-Class: {main_class}\r\n"
        )
        .unwrap();
        if let Some(class_path) = class_path {
            writeln!(archive, "Class-Path: {class_path}").unwrap();
        }
        writeln!(archive).unwrap();
        archive.finish().unwrap();
    }

    #[test]
    fn prevents_duplicate_server_start_reservations() {
        let name = format!("reservation-test-{}", std::process::id());
        let first = ServerStartReservation::acquire(&name).unwrap();
        assert!(ServerStartReservation::acquire(&name).is_err());
        drop(first);
        assert!(ServerStartReservation::acquire(&name).is_ok());
    }

    #[test]
    fn server_content_mutation_excludes_server_start() {
        let name = format!("content-mutation-test-{}", std::process::id());
        let mutation = ServerContentMutationReservation::acquire(&name).unwrap();
        assert!(ServerStartReservation::acquire(&name).is_err());
        drop(mutation);
        assert!(ServerStartReservation::acquire(&name).is_ok());
    }

    #[test]
    fn resolves_server_java_from_release_metadata() {
        let metadata = serde_json::json!({"javaVersion":{"majorVersion":25}});
        assert_eq!(
            crate::minecraft::required_java_version(&metadata, "26.3"),
            Some(25)
        );
        assert_eq!(
            crate::minecraft::required_java_version(&serde_json::json!({}), "26.3"),
            Some(25)
        );
        assert_eq!(
            crate::minecraft::required_java_version(&serde_json::json!({}), "1.20.1"),
            Some(17)
        );
    }

    #[test]
    fn checks_server_core_version_minimums() {
        assert!(!minecraft_version_at_least("1.5.1", (1, 5, 2)));
        assert!(minecraft_version_at_least("1.5.2", (1, 5, 2)));
        assert!(minecraft_version_at_least("1.12.2", (1, 5, 2)));
        assert!(minecraft_version_at_least("26.3", (1, 5, 2)));
        assert!(!minecraft_version_at_least("1.20.0", (1, 20, 1)));
        assert!(minecraft_version_at_least("1.20.1", (1, 20, 1)));
        assert!(minecraft_version_at_least("1.21.4", (1, 20, 1)));
    }

    #[test]
    fn removes_batch_pause_without_changing_server_launch_command() {
        let script = "@echo off\r\njava @user_jvm_args.txt @libraries/server.txt %*\r\npause\r\n";
        assert_eq!(
            without_batch_pause_commands(script),
            "@echo off\r\njava @user_jvm_args.txt @libraries/server.txt %*\r\n"
        );
        assert_eq!(
            without_batch_pause_commands("@echo pause >nul\r\n@pause>nul\r\n"),
            "@echo pause >nul\r\n"
        );
    }

    #[test]
    fn reads_only_the_requested_log_tail() {
        let path = std::env::current_dir().unwrap().join(format!(".mylauncher-log-tail-{}.tmp", std::process::id()));
        fs::write(&path, b"0123456789").unwrap();
        assert_eq!(read_file_tail(&path, 4).unwrap(), b"6789");
        assert_eq!(read_stream_tail(&b"0123456789"[..], 4), b"6789");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn parses_maven_sha1_checksum_files() {
        assert_eq!(
            parse_sha1_checksum(b"0123456789abcdef0123456789abcdef01234567  installer.jar\n")
                .unwrap(),
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert!(parse_sha1_checksum(b"not-a-checksum").is_err());
    }

    #[test]
    fn finds_legacy_forge_launch_jar_and_skips_installer_and_vanilla_jars() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::current_dir().unwrap().join(format!("my-launcher-forge-jars-{nonce}"));
        fs::create_dir_all(&directory).unwrap();
        write_test_jar(
            &directory.join("minecraft_server.1.12.2.jar"),
            "net.minecraft.server.MinecraftServer",
        );
        write_test_jar(
            &directory.join("forge-installer.jar"),
            "net.minecraftforge.installer.SimpleInstaller",
        );
        write_test_jar(
            &directory.join("forge-1.12.2-14.23.5.2860.jar"),
            "net.minecraftforge.fml.relauncher.ServerLaunchWrapper",
        );

        assert_eq!(
            find_legacy_forge_launch_jar(&directory).unwrap().as_deref(),
            Some("forge-1.12.2-14.23.5.2860.jar")
        );
        ensure_legacy_forge_server_classpath(
            &directory,
            &directory.join("forge-1.12.2-14.23.5.2860.jar"),
            "1.12.2",
        )
        .unwrap();
        assert!(!directory.join("minecraft_server.jar").exists());

        fs::remove_file(directory.join("forge-1.12.2-14.23.5.2860.jar")).unwrap();
        fs::write(directory.join("minecraft_server.1.5.2.jar"), b"server jar").unwrap();
        write_test_jar_with_class_path(
            &directory.join("minecraftforge-universal-1.5.2-7.8.1.738.jar"),
            "net.minecraft.server.MinecraftServer",
            Some("minecraft_server.jar"),
        );
        assert_eq!(
            find_legacy_forge_launch_jar(&directory).unwrap().as_deref(),
            Some("minecraftforge-universal-1.5.2-7.8.1.738.jar")
        );
        ensure_legacy_forge_server_classpath(
            &directory,
            &directory.join("minecraftforge-universal-1.5.2-7.8.1.738.jar"),
            "1.5.2",
        )
        .unwrap();
        assert_eq!(
            fs::read(directory.join("minecraft_server.jar")).unwrap(),
            b"server jar"
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn selects_only_stable_paper_builds_with_server_downloads() {
        let builds = vec![
            serde_json::json!({
                "id": 9,
                "channel": "EXPERIMENTAL",
                "downloads": {"server:default": {"url": "https://example.invalid/experimental.jar"}}
            }),
            serde_json::json!({"id": 8, "channel": "STABLE"}),
            serde_json::json!({
                "id": 7,
                "channel": "STABLE",
                "downloads": {"server:default": {"url": "https://example.invalid/stable.jar"}}
            }),
        ];
        assert_eq!(
            paper_stable_server_build(&builds).and_then(|build| build["id"].as_i64()),
            Some(7)
        );
        assert!(paper_stable_server_build(&[]).is_none());
    }

    #[test]
    fn writes_requested_memory_to_jvm_arguments() {
        let arguments = server_jvm_args(4096, None);
        assert!(arguments.contains("-Xms512M\n"));
        assert!(arguments.contains("-Xmx4096M\n"));
    }

    #[test]
    fn old_server_metadata_receives_safe_control_defaults() {
        let record: ServerRecord = serde_json::from_value(serde_json::json!({
            "name": "existing",
            "minecraftVersion": "1.20.1",
            "core": "paper",
            "loaderVersion": null,
            "memoryMb": 2048,
            "javaPath": "javaw.exe",
            "jarFile": "server.jar",
            "installedAt": 1
        }))
        .unwrap();
        assert_eq!(record.quick_commands, default_quick_commands());
        assert_eq!(record.active_processor_count, None);
    }

    #[test]
    fn validates_quick_command_text_and_cpu_arguments() {
        assert!(validate_quick_commands(&default_quick_commands()).is_ok());
        assert!(
            validate_quick_commands(&[ServerQuickCommand {
                id: "bad".into(),
                label: "Bad".into(),
                command: "say hello\nstop".into(),
            }])
            .is_err()
        );
        assert_eq!(cpu_count_argument(Some(4), 8), None);
        assert_eq!(
            cpu_count_argument(Some(4), 17).as_deref(),
            Some("-XX:ActiveProcessorCount=4")
        );
    }
}
