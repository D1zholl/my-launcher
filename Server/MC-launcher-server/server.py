import hashlib
import hmac
import csv
import io
from collections import deque
from datetime import date
from http.cookies import SimpleCookie
import ipaddress
import json
import logging
import mimetypes
import os
import re
import secrets
import shutil
import sqlite3
import threading
import time
import traceback
import urllib.parse
import urllib.request
import urllib.error
import xml.etree.ElementTree as ET
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from queue import Empty, Full, Queue
from make_password import password_hash, valid_password


CONFIG_PATH = Path(os.environ.get("REPO_CONFIG_FILE", Path(__file__).with_name("server_config.json")))


def load_configuration(path):
    try:
        configuration = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return {}
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"Не удалось прочитать конфигурацию {path}: {error}") from error
    if not isinstance(configuration, dict):
        raise SystemExit(f"Конфигурация {path} должна быть JSON-объектом")
    return configuration


class DataDirectoryLock:
    def __init__(self, data_directory):
        self.path = Path(data_directory) / "server.lock"
        self.handle = None

    def acquire(self):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        try:
            handle = self.path.open("a+b")
        except OSError as error:
            raise RuntimeError(f"Не удалось открыть lock-файл каталога данных {self.path}: {error}") from error
        try:
            if os.name == "nt":
                import msvcrt

                handle.seek(0, os.SEEK_END)
                if handle.tell() == 0:
                    handle.write(b"\0")
                    handle.flush()
                handle.seek(0)
                msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl

                fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as error:
            handle.seek(1)
            owner_pid = handle.read(64).decode("ascii", errors="ignore").strip("\0\r\n ")
            handle.close()
            owner = f" (PID {owner_pid})" if owner_pid.isdigit() else ""
            raise RuntimeError(f"Каталог данных уже используется другим сервером{owner}: {self.path}") from error
        handle.seek(1)
        handle.truncate(1)
        handle.write(f"{os.getpid()}\n".encode("ascii"))
        handle.flush()
        self.handle = handle

    def release(self):
        if self.handle is None:
            return
        try:
            if os.name == "nt":
                import msvcrt

                self.handle.seek(0)
                msvcrt.locking(self.handle.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                import fcntl

                fcntl.flock(self.handle.fileno(), fcntl.LOCK_UN)
        finally:
            self.handle.close()
            self.handle = None


CONFIG = load_configuration(CONFIG_PATH)


def resolve_data_directory(config_path, configuration, override=None):
    configured_directory = override if override is not None else os.environ.get(
        "REPO_DATA_DIR", configuration.get("dataDir", "data")
    )
    data_directory = Path(configured_directory)
    if not data_directory.is_absolute():
        data_directory = Path(config_path).parent / data_directory
    return data_directory.resolve()


ROOT = resolve_data_directory(CONFIG_PATH, CONFIG)
OBJECTS = ROOT / "objects" / "sha256"
DATABASE = ROOT / "repository.sqlite3"
BACKUP_DIR = Path(os.environ.get("REPO_BACKUP_DIR", Path(__file__).parent / "backups")).resolve()
APP_VERSION = os.environ.get("REPO_APP_VERSION", "development")
SERVER_STARTED_AT = time.monotonic()
WEB_ROOT = Path(__file__).resolve().parent.parent / "MC-launcher-adminpanel"
STATIC_ASSET_EXTENSIONS = frozenset((".html", ".js", ".css", ".svg", ".ttf", ".otf", ".woff", ".woff2"))
HOST = os.environ.get("REPO_HOST", CONFIG.get("host", "127.0.0.1"))
PORT = int(os.environ.get("REPO_PORT", str(CONFIG.get("port", "8787"))))
MAX_FILE_BYTES = 8 * 1024 * 1024 * 1024
MAX_UPLOAD = min(int(os.environ.get("REPO_MAX_UPLOAD_BYTES", str(MAX_FILE_BYTES))), MAX_FILE_BYTES)
MAX_MANIFEST_BYTES = 64 * 1024 * 1024
ALLOWED_FILE_EXTENSIONS = frozenset((".jar", ".zip", ".json", ".json5", ".toml", ".bak", ".backup", ".cfg", ".conf", ".properties", ".txt", ".md", ".png", ".jpg", ".jpeg", ".webp", ".mcmeta", ".dat", ".nbt", ".snbt", ".mcfunction", ".vsh", ".fsh", ".glsl", ".lang", ".yml", ".yaml", ".xml", ".ini"))
ALLOWED_SPECIAL_FILENAMES = frozenset((".defaults",))
MANIFEST_FIELDS = frozenset(("manifestVersion", "packId", "version", "name", "description", "authors", "categories", "iconUrl", "minecraft", "java", "files", "settings"))
MINECRAFT_FIELDS = frozenset(("version", "loader", "loaderVersion"))
JAVA_FIELDS = frozenset(("major",))
FILE_FIELDS = frozenset(("path", "hash", "size", "url", "required"))
SUPPORTED_JAVA_MAJORS = frozenset((8, 11, 16, 17, 21, 25))
PASSWORD_HASH = os.environ.get("REPO_ADMIN_PASSWORD_HASH", CONFIG.get("adminPasswordHash", ""))
DOWNLOAD_VISITOR_SECRET = secrets.token_bytes(32)
SESSION_SECONDS = 8 * 60 * 60
MAX_ADMIN_SESSIONS = 1000
metadata_cache = {}
metadata_cache_headers = {}
metadata_flights = {}
metadata_flights_lock = threading.Lock()
metadata_logger = logging.getLogger("launcher.metadata")
static_asset_hashes = {}
static_asset_hashes_lock = threading.Lock()
dashboard_attention_cache = {"key": None, "expires_at": 0, "values": {}}
dashboard_attention_lock = threading.Lock()
audit_prune_counter = 0
GC_GRACE_SECONDS = 72 * 60 * 60
gc_execution_lock = threading.Lock()
maintenance_execution_lock = threading.Lock()
gc_status_lock = threading.Lock()
gc_status = {"status": "idle", "result": None, "startedAt": None, "completedAt": None}
backup_execution_lock = threading.Lock()
backup_status_lock = threading.Lock()
backup_status = {"status": "idle", "backup": None, "error": None, "startedAt": None, "completedAt": None}
system_error_lock = threading.Lock()
system_errors = deque(maxlen=50)
GC_SCHEDULE_SECONDS = 24 * 60 * 60
SESSION_CLEANUP_SECONDS = 60 * 60
STALE_UPLOAD_SECONDS = 24 * 60 * 60
RELEASE_READINESS_SECONDS = 15 * 60
download_event_queue = Queue(maxsize=50000)
upload_reservation_lock = threading.Lock()
reserved_upload_bytes = 0


def record_system_error(source, error):
    entry = {"at": int(time.time()), "source": str(source)[:80], "message": str(error)[:1000]}
    with system_error_lock:
        system_errors.appendleft(entry)
    print(f"Системная ошибка ({entry['source']}): {entry['message']}", flush=True)


def write_json_atomic(path, payload):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{secrets.token_hex(8)}.tmp")
    try:
        temporary.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


@contextmanager
def db():
    connection = sqlite3.connect(DATABASE, timeout=30)
    connection.row_factory = sqlite3.Row
    connection.execute("PRAGMA foreign_keys=ON")
    try:
        yield connection
        connection.commit()
    except BaseException:
        connection.rollback()
        raise
    finally:
        connection.close()


def ensure_download_visitor_secret():
    global DOWNLOAD_VISITOR_SECRET
    configured_secret = CONFIG.get("downloadVisitorSecret")
    if configured_secret is not None:
        if not isinstance(configured_secret, str) or len(configured_secret.encode("utf-8")) < 32:
            raise SystemExit("downloadVisitorSecret в конфигурации должен содержать не менее 32 байт")
        DOWNLOAD_VISITOR_SECRET = configured_secret.encode("utf-8")
        return

    secret_path = ROOT / "download-visitor-secret"
    try:
        secret = secret_path.read_bytes()
    except FileNotFoundError:
        secret = secrets.token_bytes(32)
        try:
            with secret_path.open("xb") as secret_file:
                secret_file.write(secret)
        except FileExistsError:
            secret = secret_path.read_bytes()
    if len(secret) < 32:
        raise SystemExit(f"Секрет статистики {secret_path} повреждён: требуется не менее 32 байт")
    DOWNLOAD_VISITOR_SECRET = secret


def initialize():
    ROOT.mkdir(parents=True, exist_ok=True)
    OBJECTS.mkdir(parents=True, exist_ok=True)
    ensure_download_visitor_secret()
    with db() as connection:
        connection.executescript("""
            PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS packs (
                id TEXT PRIMARY KEY, title TEXT NOT NULL, description TEXT NOT NULL DEFAULT '',
                authors TEXT NOT NULL DEFAULT '[]', categories TEXT NOT NULL DEFAULT '[]',
                icon_url TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS releases (
                id INTEGER PRIMARY KEY AUTOINCREMENT, pack_id TEXT NOT NULL REFERENCES packs(id),
                version TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'draft', channel TEXT NOT NULL DEFAULT 'stable',
                manifest TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, published_at INTEGER,
                UNIQUE(pack_id, version)
            );
            CREATE TABLE IF NOT EXISTS release_readiness (
                release_id INTEGER PRIMARY KEY REFERENCES releases(id) ON DELETE CASCADE,
                validation_json TEXT NOT NULL, validated_at INTEGER NOT NULL,
                validation_hash TEXT NOT NULL, ready INTEGER NOT NULL CHECK(ready IN (0,1))
            );
            CREATE TABLE IF NOT EXISTS blobs (
                hash TEXT PRIMARY KEY, size INTEGER NOT NULL, download_count INTEGER NOT NULL DEFAULT 0,
                unique_download_count INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS blob_downloads (
                hash TEXT NOT NULL REFERENCES blobs(hash) ON DELETE CASCADE,
                visitor TEXT NOT NULL, PRIMARY KEY(hash, visitor)
            );
            CREATE TABLE IF NOT EXISTS blob_downloads_daily (
                download_date TEXT NOT NULL, hash TEXT NOT NULL REFERENCES blobs(hash) ON DELETE CASCADE,
                visitor TEXT NOT NULL, pack_id TEXT NOT NULL DEFAULT '', version TEXT NOT NULL DEFAULT '',
                channel TEXT NOT NULL DEFAULT '',
                PRIMARY KEY(download_date, hash, visitor, pack_id, version, channel)
            );
            CREATE INDEX IF NOT EXISTS blob_downloads_daily_date_idx ON blob_downloads_daily(download_date);
            CREATE TABLE IF NOT EXISTS audit (
                id INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL, actor TEXT NOT NULL,
                action TEXT NOT NULL, detail TEXT NOT NULL, remote TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS admin_sessions (
                token_hash TEXT PRIMARY KEY, csrf_token TEXT NOT NULL, expires_at INTEGER NOT NULL, created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS admin_sessions_expiry_idx ON admin_sessions(expires_at);
            CREATE TABLE IF NOT EXISTS admin_login_limits (
                remote TEXT PRIMARY KEY, attempts INTEGER NOT NULL, blocked_until INTEGER NOT NULL, updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS releases_public_idx ON releases(pack_id, status, channel, updated_at);
        """)
        blob_columns = {row["name"] for row in connection.execute("PRAGMA table_info(blobs)").fetchall()}
        if "download_count" not in blob_columns:
            connection.execute("ALTER TABLE blobs ADD COLUMN download_count INTEGER NOT NULL DEFAULT 0")
        if "unique_download_count" not in blob_columns:
            connection.execute("ALTER TABLE blobs ADD COLUMN unique_download_count INTEGER NOT NULL DEFAULT 0")
        release_columns = {row["name"] for row in connection.execute("PRAGMA table_info(releases)").fetchall()}
        if "published_at" not in release_columns:
            connection.execute("ALTER TABLE releases ADD COLUMN published_at INTEGER")
        connection.execute("""
            UPDATE releases SET published_at=COALESCE(
                (SELECT MIN(at) FROM audit WHERE action='release_publish' AND detail=(releases.pack_id || '@' || releases.version)),
                CASE WHEN status='published' THEN updated_at END
            ) WHERE published_at IS NULL
        """)
        connection.execute("UPDATE releases SET published_at=NULL WHERE status='draft' AND published_at IS NOT NULL")
        connection.execute("PRAGMA user_version=1")
    recover_gc_tombstones()


def cleanup_stale_uploads():
    cutoff = time.time() - STALE_UPLOAD_SECONDS
    removed = 0
    for temporary in ROOT.glob("upload-*.tmp"):
        try:
            if temporary.stat().st_mtime < cutoff:
                temporary.unlink(missing_ok=True)
                removed += 1
        except OSError as error:
            print(f"Не удалось удалить временный файл {temporary.name}: {error}")
    return removed


def readiness_report():
    checks = {"database": False, "objectsWritable": False, "schemaVersion": None, "freeBytes": 0}
    try:
        with db() as connection:
            connection.execute("SELECT 1").fetchone()
            checks["schemaVersion"] = connection.execute("PRAGMA user_version").fetchone()[0]
            checks["database"] = checks["schemaVersion"] == 1
    except sqlite3.Error:
        pass
    try:
        checks["objectsWritable"] = OBJECTS.is_dir() and os.access(OBJECTS, os.W_OK)
        checks["freeBytes"] = shutil.disk_usage(ROOT).free
    except OSError:
        pass
    minimum_free = int(CONFIG.get("minimumFreeBytes", 100 * 1024 * 1024))
    checks["diskSpace"] = checks["freeBytes"] >= minimum_free
    ok = checks["database"] and checks["objectsWritable"] and checks["diskSpace"]
    return ok, checks


def prune_audit(connection, now):
    connection.execute(
        "DELETE FROM audit WHERE at < ? OR id NOT IN (SELECT id FROM audit ORDER BY id DESC LIMIT 10000)",
        (now - 180 * 24 * 60 * 60,),
    )


def recover_gc_tombstones():
    restored = 0
    removed = 0
    for tombstone in OBJECTS.glob("*/*.gc"):
        digest = tombstone.stem
        if not re.fullmatch(r"[a-f0-9]{64}", digest):
            continue
        target = tombstone.with_name(digest)
        try:
            with db() as connection:
                blob_exists = connection.execute("SELECT 1 FROM blobs WHERE hash=?", (digest,)).fetchone() is not None
            if blob_exists and not target.exists():
                tombstone.replace(target)
                restored += 1
            else:
                tombstone.unlink(missing_ok=True)
                removed += 1
        except (OSError, sqlite3.Error) as error:
            print(f"Не удалось восстановить GC tombstone {tombstone.name}: {error}", flush=True)
    return {"restored": restored, "removed": removed}


def collect_unreferenced_blobs(grace_seconds=30 * 24 * 60 * 60, dry_run=False):
    now = int(time.time())
    warnings = []
    candidates = []
    moved = []
    deleted = 0
    try:
        with db() as connection:
            if not dry_run:
                connection.execute("BEGIN IMMEDIATE")
            referenced = {row["icon_url"].split(":", 1)[1] for row in connection.execute("SELECT icon_url FROM packs WHERE icon_url IS NOT NULL") if re.fullmatch(r"/api/files/sha256:[a-f0-9]{64}", row["icon_url"])}
            releases = connection.execute("SELECT manifest FROM releases").fetchall()
            for release in releases:
                try:
                    manifest = json.loads(release["manifest"])
                except (json.JSONDecodeError, TypeError) as error:
                    warnings.append(f"Некорректный JSON манифеста: {error}")
                    continue
                if not isinstance(manifest, dict) or not isinstance(manifest.get("files", []), list):
                    warnings.append("Некорректная структура манифеста")
                    continue
                icon_url = manifest.get("iconUrl")
                if isinstance(icon_url, str) and re.fullmatch(r"/api/files/sha256:[a-f0-9]{64}", icon_url):
                    referenced.add(icon_url.split(":", 1)[1])
                for item in manifest.get("files", []):
                    if not isinstance(item, dict):
                        warnings.append("Некорректная запись файла в манифесте")
                        continue
                    digest = item.get("hash")
                    if isinstance(digest, str) and re.fullmatch(r"sha256:[a-f0-9]{64}", digest):
                        referenced.add(digest[7:])
                    url = item.get("url")
                    if isinstance(url, str) and re.fullmatch(r"/api/files/sha256:[a-f0-9]{64}", url):
                        referenced.add(url.rsplit(":", 1)[1])
            cutoff = now - grace_seconds
            candidates = [] if warnings else [dict(row) for row in connection.execute("SELECT hash,size FROM blobs WHERE created_at < ?", (cutoff,)) if row["hash"] not in referenced]
            if not dry_run and not warnings:
                for item in candidates:
                    digest = item["hash"]
                    target = OBJECTS / digest[:2] / digest
                    tombstone = target.with_name(f"{digest}.gc")
                    if tombstone.exists():
                        if target.exists():
                            raise OSError(f"Одновременно существуют blob и GC tombstone: {digest}")
                    elif target.exists():
                        target.replace(tombstone)
                    moved.append((target, tombstone))
                for item in candidates:
                    deleted += connection.execute("DELETE FROM blobs WHERE hash=?", (item["hash"],)).rowcount
                connection.execute("DELETE FROM blob_downloads_daily WHERE download_date < date('now', '-400 days')")
                prune_audit(connection, now)
    except Exception:
        for target, tombstone in reversed(moved):
            try:
                if tombstone.exists() and not target.exists():
                    tombstone.replace(target)
            except OSError as error:
                print(f"Не удалось откатить GC для {target.name}: {error}", flush=True)
        raise
    if not dry_run:
        for target, tombstone in moved:
            try:
                tombstone.unlink(missing_ok=True)
            except OSError as error:
                warnings.append(f"Не удалось удалить GC tombstone {tombstone.name}: {error}")
    for warning in warnings:
        print(f"GC остановлен: {warning}")
    return {"deleted": deleted, "candidates": candidates, "bytes": sum(item["size"] for item in candidates), "warnings": warnings}


def execute_gc_job(actor, remote):
    global gc_status
    try:
        result = collect_unreferenced_blobs(grace_seconds=GC_GRACE_SECONDS)
        audit(actor, "blob_gc", json.dumps(result, ensure_ascii=False), remote)
        with gc_status_lock:
            gc_status = {"status": "completed", "result": result, "startedAt": gc_status.get("startedAt"), "completedAt": int(time.time())}
    except Exception as error:
        record_system_error("gc", error)
        with gc_status_lock:
            gc_status = {"status": "failed", "error": str(error), "startedAt": gc_status.get("startedAt"), "completedAt": int(time.time())}
    finally:
        gc_execution_lock.release()
        maintenance_execution_lock.release()


def start_gc_job(actor, remote):
    global gc_status
    if not maintenance_execution_lock.acquire(blocking=False):
        return False
    if (ROOT / "pending-restore.json").exists():
        maintenance_execution_lock.release()
        return False
    if not gc_execution_lock.acquire(blocking=False):
        maintenance_execution_lock.release()
        return False
    with gc_status_lock:
        gc_status = {"status": "running", "result": None, "startedAt": int(time.time()), "completedAt": None}
    worker = threading.Thread(target=execute_gc_job, args=(actor, remote), daemon=True)
    worker.start()
    return True


def execute_backup_job(actor, remote):
    global backup_status
    try:
        from backup import create_backup

        snapshot = create_backup(DATABASE, OBJECTS, ROOT, BACKUP_DIR)
        result = {"id": snapshot.name, "createdAt": snapshot.name}
        audit(actor, "backup_create", json.dumps(result, ensure_ascii=False), remote)
        with backup_status_lock:
            backup_status = {"status": "completed", "backup": result, "error": None,
                "startedAt": backup_status["startedAt"], "completedAt": int(time.time())}
    except Exception as error:
        record_system_error("backup", error)
        with backup_status_lock:
            backup_status = {"status": "failed", "backup": None, "error": str(error)[:1000],
                "startedAt": backup_status["startedAt"], "completedAt": int(time.time())}
    finally:
        backup_execution_lock.release()
        maintenance_execution_lock.release()


def start_backup_job(actor, remote):
    global backup_status
    if not maintenance_execution_lock.acquire(blocking=False):
        return False
    if (ROOT / "pending-restore.json").exists():
        maintenance_execution_lock.release()
        return False
    if not backup_execution_lock.acquire(blocking=False):
        maintenance_execution_lock.release()
        return False
    with backup_status_lock:
        backup_status = {"status": "running", "backup": None, "error": None,
            "startedAt": int(time.time()), "completedAt": None}
    threading.Thread(target=execute_backup_job, args=(actor, remote), daemon=True).start()
    return True


def diagnose_system():
    checks = {}
    try:
        with db() as connection:
            checks["databaseIntegrity"] = connection.execute("PRAGMA integrity_check").fetchone()[0]
            checks["foreignKeys"] = len(connection.execute("PRAGMA foreign_key_check").fetchall()) == 0
            connection.execute("SELECT 1").fetchone()
    except sqlite3.Error as error:
        checks["databaseIntegrity"] = str(error)
        checks["foreignKeys"] = False
    checks["databaseReadable"] = DATABASE.is_file() and os.access(DATABASE, os.R_OK)
    checks["storageAvailable"] = OBJECTS.is_dir() and os.access(OBJECTS, os.R_OK | os.W_OK)
    try:
        checks["diskFreeBytes"] = shutil.disk_usage(ROOT).free
    except OSError as error:
        checks["diskFreeBytes"] = 0
        record_system_error("diagnostics_disk", error)
    checks["backupDirectoryAvailable"] = BACKUP_DIR.is_dir() and os.access(BACKUP_DIR, os.R_OK)
    ok = checks.get("databaseIntegrity") == "ok" and checks.get("foreignKeys") is True \
        and checks.get("databaseReadable") and checks.get("storageAvailable") and checks.get("diskFreeBytes", 0) > 0
    return {"at": int(time.time()), "ok": bool(ok), "checks": checks}


def read_restore_state():
    path = ROOT / "restore-status.json"
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
        return payload if isinstance(payload, dict) else None
    except (OSError, json.JSONDecodeError):
        return None


def apply_pending_restore():
    pending_path = ROOT / "pending-restore.json"
    if not pending_path.is_file():
        return None
    try:
        pending = json.loads(pending_path.read_text(encoding="utf-8"))
        backup_id = pending.get("backupId") if isinstance(pending, dict) else None
        if not isinstance(backup_id, str) or not re.fullmatch(r"\d{8}-\d{6}(?:-\d{9})?", backup_id):
            raise ValueError("Некорректный запрос восстановления")
        from backup import restore_backup

        result = restore_backup(BACKUP_DIR / backup_id, DATABASE, OBJECTS, ROOT, BACKUP_DIR)
        with db() as connection:
            revoked_sessions = connection.execute("DELETE FROM admin_sessions").rowcount
        safe_backup = Path(result["safetyBackup"]).name if result.get("safetyBackup") else None
        restore_state = {"status": "completed", "backupId": backup_id, "restoredAt": result["restoredAt"],
            "safetyBackup": safe_backup, "verifiedFiles": result["checkedFiles"], "revokedSessions": revoked_sessions}
        write_json_atomic(ROOT / "restore-status.json", restore_state)
        pending_path.unlink(missing_ok=True)
        return restore_state
    except Exception as error:
        record_system_error("restore", error)
        write_json_atomic(ROOT / "restore-status.json", {"status": "failed", "backupId": locals().get("backup_id"),
            "failedAt": int(time.time()), "error": str(error)[:1000]})
        pending_path.unlink(missing_ok=True)
        return None


def system_status_snapshot():
    from backup import list_backups

    with db() as connection:
        storage = connection.execute("SELECT COUNT(*) AS files, COALESCE(SUM(size),0) AS bytes FROM blobs").fetchone()
        sessions = connection.execute("SELECT COUNT(*) FROM admin_sessions WHERE expires_at>=?", (int(time.time()),)).fetchone()[0]
    try:
        disk_free = shutil.disk_usage(ROOT).free
    except OSError as error:
        disk_free = 0
        record_system_error("disk", error)
    database_size = DATABASE.stat().st_size if DATABASE.is_file() else 0
    backup_history = list_backups(BACKUP_DIR)
    cached_at = [int(value[0]) for value in metadata_cache.values() if isinstance(value, tuple) and value]
    metadata_freshest = max(cached_at, default=0)
    with gc_status_lock:
        raw_gc = dict(gc_status)
    gc_result = raw_gc.get("result") if isinstance(raw_gc.get("result"), dict) else {}
    gc = {key: raw_gc.get(key) for key in ("status", "startedAt", "completedAt", "error") if raw_gc.get(key) is not None}
    if gc_result:
        gc["result"] = {"deleted": gc_result.get("deleted", 0), "candidates": len(gc_result.get("candidates", [])),
            "bytes": gc_result.get("bytes", 0), "warnings": len(gc_result.get("warnings", []))}
    with backup_status_lock:
        backup = dict(backup_status)
    with system_error_lock:
        errors = list(system_errors)
    tasks = []
    if gc.get("status") == "running":
        tasks.append({"type": "gc", "status": "running", "startedAt": gc.get("startedAt")})
    if backup.get("status") == "running":
        tasks.append({"type": "backup", "status": "running", "startedAt": backup.get("startedAt")})
    if (ROOT / "pending-restore.json").is_file():
        tasks.append({"type": "restore", "status": "scheduled"})
    ready, health_checks = readiness_report()
    return {"status": "ready" if ready else "degraded", "healthChecks": health_checks,
        "uptimeSeconds": max(0, int(time.monotonic() - SERVER_STARTED_AT)),
        "version": APP_VERSION, "diskFreeBytes": disk_free, "databaseBytes": database_size,
        "walBytes": Path(f"{DATABASE}-wal").stat().st_size if Path(f"{DATABASE}-wal").is_file() else 0,
        "storage": {"files": storage["files"], "bytes": storage["bytes"]},
        "lastGc": gc, "lastBackup": backup_history[0] if backup_history else None,
        "backupStatus": backup, "metadata": {"state": "cached" if cached_at else "empty",
            "cacheEntries": len(metadata_cache), "freshestAt": metadata_freshest or None},
        "activeSessions": sessions, "activeTasks": tasks, "recentErrors": errors,
        "restore": read_restore_state()}


def cleanup_expired_sessions(now=None):
    current_time = int(time.time()) if now is None else int(now)
    with db() as connection:
        sessions = connection.execute("DELETE FROM admin_sessions WHERE expires_at < ?", (current_time,)).rowcount
        login_limits = connection.execute(
            "DELETE FROM admin_login_limits WHERE updated_at < ?",
            (current_time - 30 * 24 * 60 * 60,),
        ).rowcount
    return {"sessions": sessions, "loginLimits": login_limits}


def scheduled_gc(stop_event):
    since_gc = 0
    since_cleanup = 0
    interval = min(GC_SCHEDULE_SECONDS, SESSION_CLEANUP_SECONDS)
    while not stop_event.wait(interval):
        since_gc += interval
        since_cleanup += interval
        if since_cleanup >= SESSION_CLEANUP_SECONDS:
            try:
                cleanup_expired_sessions()
            except Exception:
                print("Не удалось очистить истёкшие административные данные", flush=True)
                traceback.print_exc()
                record_system_error("session_cleanup", "Не удалось очистить истёкшие административные данные")
            since_cleanup = 0
        if since_gc >= GC_SCHEDULE_SECONDS:
            start_gc_job("scheduler", "localhost")
            since_gc = 0


def audit(actor, action, detail, remote):
    global audit_prune_counter
    now = int(time.time())
    try:
        with db() as connection:
            connection.execute("INSERT INTO audit(at, actor, action, detail, remote) VALUES(?,?,?,?,?)",
                               (now, actor, action, detail[:2000], remote[:128]))
            audit_prune_counter += 1
            if audit_prune_counter >= 100:
                prune_audit(connection, now)
                audit_prune_counter = 0
    except (sqlite3.Error, OSError):
        traceback.print_exc()
        return False
    return True


def clean_pack_id(value):
    if not isinstance(value, str) or not re.fullmatch(r"[a-z0-9][a-z0-9-]{1,62}[a-z0-9]", value):
        raise ValueError("id сборки: только 3-64 символа a-z, 0-9 и дефис")
    return value


def semver_sort_key(value):
    match = re.fullmatch(
        r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
        r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?",
        value,
    )
    if not match:
        return (0, value)
    major, minor, patch, prerelease = match.groups()
    if prerelease is None:
        prerelease_key = (1, ())
    else:
        identifiers = []
        for identifier in prerelease.split("."):
            if identifier.isdigit():
                if len(identifier) > 1 and identifier.startswith("0"):
                    return (0, value)
                identifiers.append((0, int(identifier)))
            else:
                identifiers.append((1, identifier))
        prerelease_key = (0, tuple(identifiers))
    return (1, int(major), int(minor), int(patch), prerelease_key, value)


def ordered_releases(rows):
    valid_rows = (row for row in rows if semver_sort_key(row["version"])[0] == 1)
    return sorted(valid_rows, key=lambda row: semver_sort_key(row["version"]), reverse=True)


def allowed_repository_file(value):
    filename = Path(value).name.lower()
    return filename in ALLOWED_SPECIAL_FILENAMES or Path(filename).suffix.lower() in ALLOWED_FILE_EXTENSIONS


def safe_relative_path(value):
    if not isinstance(value, str) or not value or len(value) > 240 or "\\" in value or not re.fullmatch(r"[A-Za-z0-9 _./+@()§-]+", value):
        raise ValueError("путь файла должен быть относительным POSIX-путём")
    parts = value.split("/")
    if any(part in ("", ".", "..") or any(ord(char) < 32 for char in part) for part in parts):
        raise ValueError("путь файла содержит недопустимый сегмент")
    if re.match(r"^[a-zA-Z]:", value) or value.startswith("/"):
        raise ValueError("абсолютные пути запрещены")
    reserved = {"CON", "PRN", "AUX", "NUL", *(f"COM{number}" for number in range(1, 10)), *(f"LPT{number}" for number in range(1, 10))}
    for index, part in enumerate(parts):
        if any(char in part for char in '<>:"|?*') or part.endswith((".", " ")):
            raise ValueError("путь содержит имя, недопустимое в Windows")
        if part.split(".", 1)[0].upper() in reserved:
            raise ValueError("путь содержит зарезервированное имя Windows")
        if index == 0 and part.casefold() in {"versions", "assets", "libraries", ".launcher_meta.json", ".launcher_meta.json.tmp", ".launcher_meta.json.backup"}:
            raise ValueError("сборка не может изменять служебные файлы лаунчера")
    if not allowed_repository_file(value):
        raise ValueError("тип файла не разрешён политикой репозитория")
    return value


def password_matches(candidate):
    try:
        algorithm, n, r, p, salt_hex, digest_hex = PASSWORD_HASH.split("$", 5)
        if algorithm != "scrypt" or int(n) != 16384 or int(r) != 8 or int(p) != 1:
            return False
        expected = bytes.fromhex(digest_hex)
        actual = hashlib.scrypt(candidate.encode("utf-8"), salt=bytes.fromhex(salt_hex), n=16384, r=8, p=1, dklen=len(expected))
        return hmac.compare_digest(actual, expected)
    except (ValueError, TypeError):
        return False


def sha256_file(path):
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
            size += len(chunk)
    return digest.hexdigest(), size


def image_content_type(path):
    with path.open("rb") as source:
        signature = source.read(12)
    if signature.startswith(b"\x89PNG\r\n\x1a\n"):
        return "image/png"
    if signature.startswith(b"\xff\xd8\xff"):
        return "image/jpeg"
    if signature[:4] == b"RIFF" and signature[8:12] == b"WEBP":
        return "image/webp"
    return None


def parse_byte_range(value, size):
    if value is None:
        return None
    match = re.fullmatch(r"bytes=(\d*)-(\d*)", value.strip())
    if not match or size <= 0 or not any(match.groups()):
        return False
    start_text, end_text = match.groups()
    if not start_text:
        suffix = int(end_text)
        if suffix <= 0:
            return False
        return (max(0, size - suffix), size - 1)
    start = int(start_text)
    end = int(end_text) if end_text else size - 1
    if start >= size or end < start:
        return False
    return (start, min(end, size - 1))


def if_none_match_matches(value, etag):
    if not value:
        return False
    return any(tag.strip() == "*" or tag.strip().removeprefix("W/") == etag for tag in value.split(","))


def validate_metadata_payload(url, value):
    if isinstance(value, str):
        root = ET.fromstring(value)
        if root.tag.rsplit("}", 1)[-1] != "metadata":
            raise ValueError("XML источника не является Maven Metadata")
        version_elements = [element for element in root.iter() if element.tag.rsplit("}", 1)[-1] == "version"]
        if not version_elements or any(not element.text for element in version_elements):
            raise ValueError("Maven Metadata не содержит опубликованных версий")
        return
    if not isinstance(value, (dict, list)):
        raise ValueError("Источник вернул неожиданный тип метаданных")
    if "piston-meta.mojang.com/mc/game/version_manifest_v2.json" in url:
        if not isinstance(value, dict) or not isinstance(value.get("versions"), list) or not isinstance(value.get("latest"), dict):
            raise ValueError("Mojang manifest не содержит versions/latest")
    elif "files.minecraftforge.net" in url and "promotions" in url:
        if not isinstance(value, dict) or not isinstance(value.get("promos"), dict):
            raise ValueError("Forge Promotions не содержит promos")
    elif "meta.fabricmc.net/v2/versions/" in url and "/profile/" not in url:
        if not isinstance(value, list):
            raise ValueError("Fabric Meta не вернул список версий")
    elif "maven.neoforged.net/api/maven/versions/" in url:
        versions = value.get("versions") if isinstance(value, dict) else value
        if not isinstance(versions, list) or any(not isinstance(version, str) for version in versions):
            raise ValueError("NeoForge Maven API не вернул список версий")
    elif "meta.prismlauncher.org/v1/" in url and url.endswith("/index.json"):
        if not isinstance(value, dict) or not isinstance(value.get("versions"), (list, dict)):
            raise ValueError("Prism index не содержит versions")


def cached_metadata(url, ttl=1800, offline=False, force=False):
    cache_key = hashlib.sha256(url.encode("utf-8")).hexdigest()
    cache_path = ROOT / "metadata-cache" / f"{cache_key}.json"
    now = time.time()
    cached = metadata_cache.get(url)
    validators = metadata_cache_headers.get(url, {})
    if cached is None:
        try:
            envelope = json.loads(cache_path.read_text(encoding="utf-8"))
            if envelope.get("url") == url and type(envelope.get("fetchedAt")) is int and "payload" in envelope:
                validate_metadata_payload(url, envelope["payload"])
                cached = (envelope["fetchedAt"], envelope["payload"])
                validators = {key: envelope[key] for key in ("etag", "lastModified", "expiresAt") if envelope.get(key) is not None}
                metadata_cache[url] = cached
                metadata_cache_headers[url] = validators
        except FileNotFoundError:
            pass
        except (OSError, json.JSONDecodeError, AttributeError, KeyError, ValueError, ET.ParseError) as error:
            metadata_logger.warning("Ignoring invalid metadata cache file for %s: %s", url, error)

    def cache_is_fresh(entry, headers):
        expires_at = headers.get("expiresAt")
        return now < expires_at if isinstance(expires_at, (int, float)) else bool(entry and now - entry[0] < ttl)

    if cached and not force and cache_is_fresh(cached, validators):
        return cached[1]
    if offline:
        if cached:
            return cached[1]
        raise ValueError(f"Нет кэшированных официальных метаданных: {url}")

    with metadata_flights_lock:
        flight = metadata_flights.get(url)
        if flight is None:
            flight = {"event": threading.Event(), "result": None, "error": None}
            metadata_flights[url] = flight
            owner = True
        else:
            owner = False
    if not owner:
        flight["event"].wait()
        if flight["error"] is not None:
            raise flight["error"]
        return flight["result"]

    try:
        cached = metadata_cache.get(url, cached)
        validators = metadata_cache_headers.get(url, validators)
        if cached and not force and cache_is_fresh(cached, validators):
            flight["result"] = cached[1]
            return cached[1]

        request_headers = {"User-Agent": "MyLauncherRepository/1", "Accept": "application/json, application/xml, text/xml"}
        if validators.get("etag"):
            request_headers["If-None-Match"] = validators["etag"]
        if validators.get("lastModified"):
            request_headers["If-Modified-Since"] = validators["lastModified"]

        def persist(entry, cache_headers):
            cache_path.parent.mkdir(parents=True, exist_ok=True)
            envelope = {"url": url, "fetchedAt": entry[0], "payload": entry[1], **cache_headers}
            temporary_path = cache_path.with_name(f"{cache_path.stem}-{secrets.token_hex(8)}.tmp")
            try:
                temporary_path.write_text(json.dumps(envelope, ensure_ascii=False), encoding="utf-8")
                temporary_path.replace(cache_path)
            finally:
                temporary_path.unlink(missing_ok=True)

        for attempt in range(3):
            try:
                request = urllib.request.Request(url, headers=request_headers)
                with urllib.request.urlopen(request, timeout=20) as response:
                    body = response.read(16 * 1024 * 1024 + 1)
                    response_headers = getattr(response, "headers", {}) or {}
                if len(body) > 16 * 1024 * 1024:
                    raise ValueError("Метаданные источника превышают допустимый размер")
                text = body.decode("utf-8-sig")
                if text.lstrip().startswith("<"):
                    ET.fromstring(text)
                    value = text
                else:
                    value = json.loads(text)
                validate_metadata_payload(url, value)
                fetched_at = int(time.time())
                control = response_headers.get("Cache-Control", "")
                max_age = re.search(r"(?:^|,)\s*max-age=(\d+)", control, re.IGNORECASE)
                expires_in = int(max_age.group(1)) if max_age else ttl
                if re.search(r"(?:^|,)\s*(?:no-cache|no-store)(?:\s|,|$)", control, re.IGNORECASE):
                    expires_in = 0
                age = response_headers.get("Age", "0")
                if age.isdigit():
                    expires_in = max(0, expires_in - int(age))
                new_validators = {
                    key: value for key, value in (
                        ("etag", response_headers.get("ETag")),
                        ("lastModified", response_headers.get("Last-Modified")),
                        ("expiresAt", fetched_at + expires_in),
                    ) if value is not None
                }
                entry = (fetched_at, value)
                metadata_cache[url] = entry
                metadata_cache_headers[url] = new_validators
                if "no-store" not in control.lower():
                    persist(entry, new_validators)
                flight["result"] = value
                return value
            except urllib.error.HTTPError as error:
                if error.code == 304 and cached:
                    fetched_at = int(time.time())
                    new_validators = dict(validators)
                    if error.headers:
                        new_validators.update({
                            key: value for key, value in (
                                ("etag", error.headers.get("ETag")),
                                ("lastModified", error.headers.get("Last-Modified")),
                            ) if value is not None
                        })
                    control = error.headers.get("Cache-Control", "") if error.headers else ""
                    max_age = re.search(r"(?:^|,)\s*max-age=(\d+)", control, re.IGNORECASE)
                    expires_in = int(max_age.group(1)) if max_age else ttl
                    if re.search(r"(?:^|,)\s*(?:no-cache|no-store)(?:\s|,|$)", control, re.IGNORECASE):
                        expires_in = 0
                    age = error.headers.get("Age", "0") if error.headers else "0"
                    if age.isdigit():
                        expires_in = max(0, expires_in - int(age))
                    new_validators["expiresAt"] = fetched_at + expires_in
                    entry = (fetched_at, cached[1])
                    metadata_cache[url] = entry
                    metadata_cache_headers[url] = new_validators
                    persist(entry, new_validators)
                    flight["result"] = entry[1]
                    return entry[1]
                retryable = error.code == 429 or 500 <= error.code <= 599
                if retryable and attempt < 2:
                    retry_after = error.headers.get("Retry-After", "") if error.headers else ""
                    delay = min(float(retry_after), 2.0) if retry_after.isdigit() else 0.25 * (2 ** attempt)
                    time.sleep(delay)
                    continue
                if (retryable or error.code in (403, 404)) and cached and time.time() - cached[0] <= 24 * 60 * 60:
                    metadata_logger.warning("Using stale metadata cache for %s after HTTP %s", url, error.code)
                    flight["result"] = cached[1]
                    return cached[1]
                metadata_logger.error("Metadata request failed for %s with HTTP %s", url, error.code)
                raise
            except (urllib.error.URLError, TimeoutError, OSError) as error:
                if isinstance(error, urllib.error.URLError) and isinstance(error.reason, urllib.error.HTTPError):
                    raise error.reason
                if attempt < 2:
                    time.sleep(0.25 * (2 ** attempt))
                    continue
                if cached and time.time() - cached[0] <= 24 * 60 * 60:
                    metadata_logger.warning("Using stale metadata cache for %s after %s", url, error)
                    flight["result"] = cached[1]
                    return cached[1]
                metadata_logger.error("Metadata request failed for %s: %s", url, error)
                raise urllib.error.URLError(f"{url}: {error}") from error
            except (ValueError, UnicodeDecodeError, ET.ParseError, json.JSONDecodeError) as error:
                if cached:
                    metadata_logger.warning("Ignoring invalid metadata from %s: %s", url, error)
                    flight["result"] = cached[1]
                    return cached[1]
                metadata_logger.error("Invalid metadata from %s: %s", url, error)
                raise
    except BaseException as error:
        flight["error"] = error
        raise
    finally:
        with metadata_flights_lock:
            metadata_flights.pop(url, None)
            flight["event"].set()


def minecraft_manifest(offline=False, force=False):
    return cached_metadata("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json", 3600, offline, force=force)


def minecraft_options(force=False):
    manifest = minecraft_manifest(force=force)
    versions = [
        {"id": item["id"], "releaseTime": item.get("releaseTime"), "type": item["type"], "url": item.get("url")}
        for item in manifest.get("versions", [])
        if item.get("type") in ("release", "snapshot", "old_beta", "old_alpha")
        and re.fullmatch(r"[A-Za-z0-9._+-]{1,64}", item.get("id", ""))
    ]
    return {"latestRelease": manifest.get("latest", {}).get("release"), "latestSnapshot": manifest.get("latest", {}).get("snapshot"), "versions": versions}


def selected_minecraft_version(game_version, offline=False):
    if not isinstance(game_version, str) or not re.fullmatch(r"[A-Za-z0-9._+-]{1,64}", game_version):
        raise ValueError("Некорректная версия Minecraft")
    item = next((entry for entry in minecraft_manifest(offline).get("versions", []) if entry.get("id") == game_version), None)
    if not item:
        raise ValueError("Версия Minecraft отсутствует в официальном manifest")
    return item


def java_major_for_version(game_version, offline=False):
    item = selected_minecraft_version(game_version, offline)
    url = item.get("url", "")
    parsed = urllib.parse.urlsplit(url)
    if parsed.scheme != "https" or parsed.hostname not in ("piston-meta.mojang.com", "launchermeta.mojang.com"):
        raise ValueError("Официальный manifest содержит недопустимый URL метаданных")
    metadata = cached_metadata(url, 3600, offline)
    major = metadata.get("javaVersion", {}).get("majorVersion")
    if type(major) is not int or not 8 <= major <= 255:
        raise ValueError("Официальные метаданные содержат некорректную версию Java")
    return {"major": major, "component": metadata.get("javaVersion", {}).get("component")}


def loader_version_sort_key(version):
    match = re.match(r"^(\d+(?:\.\d+)*)(.*)$", version)
    if not match:
        return ((), 0, ((1, version.casefold()),))
    numeric = tuple(int(part) for part in match.group(1).split("."))
    suffix = tuple((0, int(part)) if part.isdigit() else (1, part.casefold()) for part in re.findall(r"\d+|[A-Za-z]+", match.group(2)))
    return numeric, int(not suffix), suffix


def fabric_metadata(resource, game_version="", loader_version="", installer_version="", offline=False, force=False):
    base = "https://meta.fabricmc.net/v2/versions"
    urls = {
        "game": f"{base}/game",
        "loader": f"{base}/loader",
        "installer": f"{base}/installer",
        "yarn": f"{base}/yarn",
        "intermediary": f"{base}/intermediary",
    }
    if resource not in urls:
        if resource not in ("compatible-loader", "loader-details", "client-profile", "client-profile-zip", "server-profile", "server-jar", "yarn-for-game", "intermediary-for-game"):
            raise ValueError("Неизвестный ресурс Fabric Meta")
        if not isinstance(game_version, str) or not re.fullmatch(r"[A-Za-z0-9._+-]{1,64}", game_version):
            raise ValueError("Укажите корректную версию Minecraft для Fabric Meta")
        safe_game = urllib.parse.quote(game_version, safe="")
        if resource == "compatible-loader":
            url = f"{base}/loader/{safe_game}"
        elif resource == "yarn-for-game":
            url = f"{base}/yarn/{safe_game}"
        elif resource == "intermediary-for-game":
            url = f"{base}/intermediary/{safe_game}"
        else:
            if not isinstance(loader_version, str) or not re.fullmatch(r"[A-Za-z0-9._+-]{1,128}", loader_version):
                raise ValueError("Укажите корректную версию Fabric Loader")
            safe_loader = urllib.parse.quote(loader_version, safe="")
            root = f"{base}/loader/{safe_game}/{safe_loader}"
            if resource == "loader-details":
                url = root
            elif resource == "client-profile":
                url = f"{root}/profile/json"
            elif resource == "client-profile-zip":
                return {"url": f"{root}/profile/zip", "source": "official"}
            elif resource == "server-profile":
                url = f"{root}/server/json"
            else:
                if not isinstance(installer_version, str) or not re.fullmatch(r"[A-Za-z0-9._+-]{1,128}", installer_version):
                    raise ValueError("Укажите корректную версию Fabric Installer")
                safe_installer = urllib.parse.quote(installer_version, safe="")
                return {"url": f"{root}/{safe_installer}/server/jar", "source": "official"}
    else:
        url = urls[resource]
    return {"url": url, "data": cached_metadata(url, offline=offline, force=force), "source": "official"}


def maven_version_list(payload):
    if isinstance(payload, str):
        root = ET.fromstring(payload)
        return [element.text.strip() for element in root.iter() if element.tag.rsplit("}", 1)[-1] == "version" and element.text]
    if isinstance(payload, list):
        versions = payload
    elif isinstance(payload, dict):
        versions = payload.get("versions")
    else:
        versions = None
    if not isinstance(versions, list) or any(not isinstance(version, str) for version in versions):
        raise ValueError("Maven API вернул некорректный список версий")
    return versions


def loader_record(loader, game_version, version, *, stable=None, recommended=False, latest=False, published_at=None, profile_url=None, metadata_url=None, installer_url=None):
    return {
        "loader": loader,
        "minecraftVersion": game_version,
        "loaderVersion": version,
        "releaseType": "stable" if stable is True else "preview" if stable is False else None,
        "stable": stable,
        "recommended": recommended,
        "latest": latest,
        "source": "official",
        "installerUrl": installer_url,
        "profileUrl": profile_url,
        "metadataUrl": metadata_url,
        "publishedAt": published_at,
        "checksums": None,
    }


def prism_compatible_versions(loader, game_version, offline=False, force=False):
    component = {"fabric": "net.fabricmc.fabric-loader", "forge": "net.minecraftforge", "neoforge": "net.neoforged"}[loader]
    index_url = f"https://meta.prismlauncher.org/v1/{component}/index.json"
    index = cached_metadata(index_url, offline=offline, force=force)
    records = index.get("versions") if isinstance(index, dict) else None
    if isinstance(records, dict):
        records = [dict(value, version=key) if isinstance(value, dict) else {"version": key} for key, value in records.items()]
    if not isinstance(records, list):
        raise ValueError("Prism metadata вернул некорректный индекс версий")
    compatible = {}
    for record in records:
        if not isinstance(record, dict) or not isinstance(record.get("version"), str):
            continue
        requirements = record.get("requires", [])
        if not isinstance(requirements, list):
            continue
        if any(
            isinstance(requirement, dict)
            and requirement.get("uid") == "net.minecraft"
            and requirement.get("equals") == game_version
            for requirement in requirements
        ):
            compatible[record["version"]] = record.get("released") or record.get("releaseTime")
    return compatible


def loader_options(game_version, loader, offline=False, force=False):
    selected_minecraft_version(game_version, offline)
    if loader == "vanilla":
        return {"versions": [], "items": [], "source": "official"}
    if loader not in ("fabric", "forge", "neoforge"):
        raise ValueError("Неизвестный загрузчик")

    items = []
    if loader == "fabric":
        safe_game = urllib.parse.quote(game_version, safe="")
        payload = cached_metadata(f"https://meta.fabricmc.net/v2/versions/loader/{safe_game}", offline=offline, force=force)
        if not isinstance(payload, list):
            raise ValueError("Fabric Meta вернул некорректный список версий")
        for entry in payload:
            if not isinstance(entry, dict):
                raise ValueError("Fabric Meta содержит некорректную запись версии")
            loader_info = entry.get("loader")
            if not isinstance(loader_info, dict):
                raise ValueError("В ответе Fabric Meta отсутствует обязательное поле loader")
            version = loader_info.get("version")
            stable = loader_info.get("stable")
            if not isinstance(version, str) or type(stable) is not bool:
                raise ValueError("В ответе Fabric Meta некорректны версия или stable")
            safe_loader = urllib.parse.quote(version, safe="")
            root = f"https://meta.fabricmc.net/v2/versions/loader/{safe_game}/{safe_loader}"
            items.append(loader_record(
                loader, game_version, version, stable=stable,
                profile_url=f"{root}/profile/json", metadata_url=root,
            ))
        items = list({item["loaderVersion"]: item for item in reversed(items)}.values())
        items.sort(key=lambda item: loader_version_sort_key(item["loaderVersion"]), reverse=True)
        if items:
            items[0]["latest"] = True

    elif loader == "forge":
        try:
            promotions = cached_metadata(
                "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json", offline=offline, force=force
            )
        except (urllib.error.URLError, TimeoutError):
            promotions = cached_metadata(
                "https://files.minecraftforge.net/net/minecraftforge/forge/promotions.json",
                offline=offline,
                force=force,
            )
        if not isinstance(promotions, dict) or not isinstance(promotions.get("promos", {}), dict):
            raise ValueError("Forge Promotions вернул некорректные данные")
        promos = promotions.get("promos", {})
        prefix = f"{game_version}-"
        published = maven_version_list(cached_metadata(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml", offline=offline, force=force
        ))
        available = {version for version in published if version.startswith(prefix)}
        recommended_version = promos.get(f"{game_version}-recommended")
        latest_version = promos.get(f"{game_version}-latest")
        if recommended_version:
            recommended_version = f"{game_version}-{recommended_version}"
        if latest_version:
            latest_version = f"{game_version}-{latest_version}"
        ordered = sorted(available, key=lambda version: loader_version_sort_key(version[len(prefix):]), reverse=True)
        for version in ordered:
            suffix = version[len(prefix):]
            stable = not bool(re.search(r"(?i)(?:^|[.-])(alpha|beta|rc|snapshot|pre|preview)(?:[.-]|\d|$)", suffix))
            artifact = urllib.parse.quote(version, safe="")
            artifact_root = f"https://maven.minecraftforge.net/net/minecraftforge/forge/{artifact}/forge-{artifact}"
            items.append(loader_record(
                loader, game_version, suffix, stable=stable,
                recommended=version == recommended_version, latest=version == latest_version,
                installer_url=f"{artifact_root}-installer.jar",
                metadata_url=f"https://files.minecraftforge.net/net/minecraftforge/forge/{artifact}/meta.json",
            ))
        if items and not any(item["latest"] for item in items):
            items[0]["latest"] = True

    else:
        historical = game_version == "1.20.1"
        artifact = "forge" if historical else "neoforge"
        encoded_path = urllib.parse.quote(f"net/neoforged/{artifact}", safe="")
        json_url = f"https://maven.neoforged.net/api/maven/versions/releases/{encoded_path}"
        xml_url = f"https://maven.neoforged.net/releases/net/neoforged/{artifact}/maven-metadata.xml"
        try:
            published = maven_version_list(cached_metadata(json_url, offline=offline, force=force))
        except (urllib.error.URLError, TimeoutError, ValueError, json.JSONDecodeError):
            published = maven_version_list(cached_metadata(xml_url, offline=offline, force=force))
        available = set(published)
        if historical:
            coordinate_pattern = re.compile(rf"{re.escape(game_version)}-\d+(?:\.\d+)+(?:[-+][A-Za-z0-9._+-]+)?\Z")
            compatible = sorted((version for version in available if coordinate_pattern.fullmatch(version)), key=loader_version_sort_key, reverse=True)
            prism_versions = {}
        else:
            prism_versions = prism_compatible_versions(loader, game_version, offline=offline, force=force)
            compatible = sorted(available.intersection(prism_versions), key=loader_version_sort_key, reverse=True)
        for version in compatible:
            safe_version = urllib.parse.quote(version, safe="")
            artifact_root = f"https://maven.neoforged.net/releases/net/neoforged/{artifact}/{safe_version}/{artifact}-{safe_version}"
            installer_url = f"{artifact_root}-installer.jar"
            items.append(loader_record(
                loader, game_version, version, stable=not bool(re.search(r"(?i)(?:^|[.-])(alpha|beta|rc|snapshot|pre|preview)(?:[.-]|\d|$)", version)),
                latest=not items, published_at=prism_versions.get(version),
                installer_url=installer_url,
                metadata_url=f"{artifact_root}.pom",
                profile_url=installer_url if historical else f"https://meta.prismlauncher.org/v1/net.neoforged/{safe_version}.json",
            ))

    return {"versions": [item["loaderVersion"] for item in items], "items": items, "source": "official"}


def validate_official_metadata(manifest, offline=False):
    errors = []
    minecraft = manifest.get("minecraft", {})
    game_version = minecraft.get("version") if isinstance(minecraft, dict) else ""
    java = manifest.get("java", {})
    try:
        expected_java = java_major_for_version(game_version, offline)["major"]
        if not isinstance(java, dict) or java.get("major") != expected_java:
            errors.append(f"Для Minecraft {game_version} официальные метаданные требуют Java {expected_java}")
    except (urllib.error.URLError, TimeoutError, ValueError, ET.ParseError, json.JSONDecodeError) as error:
        errors.append(f"Официальная совместимость Java не подтверждена: {error}")
    if isinstance(minecraft, dict) and minecraft.get("loader") != "vanilla":
        try:
            compatible = loader_options(game_version, minecraft["loader"], offline)["versions"]
            if minecraft.get("loaderVersion") not in compatible:
                errors.append(f"Версия {minecraft.get('loaderVersion')} недоступна для {minecraft.get('loader')} и Minecraft {game_version}")
        except (urllib.error.URLError, TimeoutError, ValueError, ET.ParseError, json.JSONDecodeError) as error:
            errors.append(f"Официальная совместимость загрузчика не подтверждена: {error}")
    return errors


def validate_manifest(manifest, pack_id=None, version=None):
    errors = []
    if not isinstance(manifest, dict):
        return ["manifest должен быть JSON-объектом"]
    required = ("manifestVersion", "packId", "version", "name", "description", "authors", "categories", "minecraft", "java", "files", "settings")
    for key in required:
        if key not in manifest:
            errors.append(f"нет обязательного поля: {key}")
    if set(manifest) - MANIFEST_FIELDS:
        errors.append("manifest содержит неизвестные поля")
    if not isinstance(manifest.get("name"), str) or not manifest.get("name", "").strip() or len(manifest.get("name", "")) > 120:
        errors.append("name должен содержать 1-120 символов")
    if not isinstance(manifest.get("description"), str) or len(manifest.get("description", "")) > 4000:
        errors.append("description должен быть строкой до 4000 символов")
    for key in ("authors", "categories"):
        item_limit = 120
        if not isinstance(manifest.get(key), list) or any(not isinstance(item, str) or len(item) > item_limit for item in manifest.get(key, [])):
            errors.append(f"{key} должен быть массивом коротких строк")
    icon_url = manifest.get("iconUrl")
    if icon_url is not None and (not isinstance(icon_url, str) or not re.fullmatch(r"/api/files/sha256:[a-f0-9]{64}", icon_url)):
        errors.append("iconUrl должен указывать на загруженный файл репозитория или быть null")
    if type(manifest.get("manifestVersion")) is not int or manifest.get("manifestVersion") != 1:
        errors.append("поддерживается manifestVersion=1")
    try:
        clean_pack_id(manifest.get("packId"))
    except ValueError as error:
        errors.append(str(error))
    if pack_id and manifest.get("packId") != pack_id:
        errors.append("packId не совпадает с родительской сборкой")
    if version and manifest.get("version") != version:
        errors.append("version не совпадает с версией релиза")
    minecraft = manifest.get("minecraft")
    if isinstance(minecraft, dict) and set(minecraft) - MINECRAFT_FIELDS:
        errors.append("minecraft содержит неизвестные параметры")
    if not isinstance(minecraft, dict) or not isinstance(minecraft.get("version"), str) or not re.fullmatch(r"[A-Za-z0-9._+-]{1,64}", minecraft.get("version", "")):
        errors.append("minecraft.version задан некорректно")
    if not isinstance(minecraft, dict) or minecraft.get("loader") not in ("vanilla", "fabric", "forge", "neoforge"):
        errors.append("minecraft.loader: vanilla, fabric, forge или neoforge")
    elif "loaderVersion" not in minecraft:
        errors.append("minecraft.loaderVersion является обязательным полем")
    elif minecraft["loader"] == "vanilla":
        if minecraft.get("loaderVersion") not in (None, ""):
            errors.append("для vanilla loaderVersion должен быть пустым")
    elif not isinstance(minecraft.get("loaderVersion"), str) or not re.fullmatch(r"[A-Za-z0-9._+-]{1,128}", minecraft["loaderVersion"]):
        errors.append("укажите корректный minecraft.loaderVersion")
    java = manifest.get("java")
    if isinstance(java, dict) and set(java) - JAVA_FIELDS:
        errors.append("java содержит неизвестные параметры")
    if not isinstance(java, dict) or type(java.get("major")) is not int or java["major"] not in SUPPORTED_JAVA_MAJORS:
        errors.append("java.major должен быть одним из поддерживаемых значений: 8, 11, 16, 17, 21, 25")
    files = manifest.get("files")
    if not isinstance(files, list):
        errors.append("files должен быть массивом")
        files = []
    if len(files) > 100000:
        errors.append("files содержит больше 100000 записей")
    total_size = 0
    seen_paths = set()
    for index, item in enumerate(files):
        prefix = f"files[{index}]"
        if not isinstance(item, dict):
            errors.append(f"{prefix}: ожидается объект")
            continue
        if set(item) - FILE_FIELDS:
            errors.append(f"{prefix}: содержит неизвестные поля")
        try:
            path = safe_relative_path(item.get("path"))
            if path.casefold() in seen_paths:
                errors.append(f"{prefix}: повторяется путь {path}")
            seen_paths.add(path.casefold())
        except ValueError as error:
            errors.append(f"{prefix}: {error}")
        digest = item.get("hash", "")
        if not isinstance(digest, str) or not re.fullmatch(r"sha256:[a-f0-9]{64}", digest):
            errors.append(f"{prefix}: hash должен быть sha256:<64 hex>")
        if type(item.get("size")) is not int or item["size"] < 0:
            errors.append(f"{prefix}: size должен быть неотрицательным целым числом")
        elif item["size"] > MAX_FILE_BYTES:
            errors.append(f"{prefix}: файл превышает лимит {MAX_FILE_BYTES // (1024 ** 3)} GiB")
        else:
            total_size += item["size"]
        if not isinstance(item.get("required"), bool):
            errors.append(f"{prefix}: required должен быть boolean")
        if item.get("url") != f"/api/files/{digest}":
            errors.append(f"{prefix}: url должен ссылаться на content-addressed файл")
    if total_size > 20_000_000_000:
        errors.append("общий размер сборки превышает 20 GB")
    if not isinstance(manifest.get("settings"), dict):
        errors.append("settings должен быть JSON-объектом")
    elif set(manifest["settings"]) - {"memoryMb", "jvmArgs", "windowWidth", "windowHeight", "fullscreen"}:
        errors.append("settings содержит неизвестные параметры")
    else:
        settings = manifest["settings"]
        for key, low, high in (("memoryMb", 512, 65536), ("windowWidth", 640, 7680), ("windowHeight", 480, 4320)):
            if key in settings and (type(settings[key]) is not int or not low <= settings[key] <= high):
                errors.append(f"settings.{key} вне допустимого диапазона")
        if "jvmArgs" in settings and (not isinstance(settings["jvmArgs"], str) or len(settings["jvmArgs"]) > 2000):
            errors.append("settings.jvmArgs должен быть строкой до 2000 символов")
        if "fullscreen" in settings and not isinstance(settings["fullscreen"], bool):
            errors.append("settings.fullscreen должен быть boolean")
    return errors


def validate_draft_manifest(manifest, pack_id, version):
    errors = []
    if not isinstance(manifest, dict):
        return ["manifest должен быть JSON-объектом"]
    if set(manifest) - MANIFEST_FIELDS:
        errors.append("manifest содержит неизвестные поля")
    if "manifestVersion" in manifest and (type(manifest["manifestVersion"]) is not int or manifest["manifestVersion"] != 1):
        errors.append("поддерживается manifestVersion=1")
    if "packId" in manifest:
        try:
            clean_pack_id(manifest["packId"])
        except ValueError as error:
            errors.append(str(error))
        if manifest["packId"] != pack_id:
            errors.append("packId не совпадает с родительской сборкой")
    if "version" in manifest:
        draft_version = manifest["version"]
        if not isinstance(draft_version, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]{0,63}", draft_version):
            errors.append("version задана некорректно")
        elif draft_version != version:
            errors.append("version не совпадает с версией релиза")
    for key, maximum in (("name", 120), ("description", 4000)):
        if key in manifest and (not isinstance(manifest[key], str) or len(manifest[key]) > maximum):
            errors.append(f"{key} должен быть строкой до {maximum} символов")
    for key in ("authors", "categories"):
        if key in manifest and (not isinstance(manifest[key], list) or any(not isinstance(item, str) or len(item) > 120 for item in manifest[key])):
            errors.append(f"{key} должен быть массивом коротких строк")
    icon_url = manifest.get("iconUrl")
    if "iconUrl" in manifest and icon_url is not None and (not isinstance(icon_url, str) or not re.fullmatch(r"/api/files/sha256:[a-f0-9]{64}", icon_url)):
        errors.append("iconUrl должен указывать на загруженный файл репозитория или быть null")
    minecraft = manifest.get("minecraft")
    if "minecraft" in manifest and not isinstance(minecraft, dict):
        errors.append("minecraft должен быть JSON-объектом")
    elif isinstance(minecraft, dict):
        if set(minecraft) - MINECRAFT_FIELDS:
            errors.append("minecraft содержит неизвестные параметры")
        game_version = minecraft.get("version")
        if game_version not in (None, "") and (not isinstance(game_version, str) or not re.fullmatch(r"[A-Za-z0-9._+-]{1,64}", game_version)):
            errors.append("minecraft.version задан некорректно")
        loader = minecraft.get("loader")
        if loader is not None and loader not in ("vanilla", "fabric", "forge", "neoforge"):
            errors.append("minecraft.loader: vanilla, fabric, forge или neoforge")
        loader_version = minecraft.get("loaderVersion")
        if loader_version not in (None, "") and (not isinstance(loader_version, str) or not re.fullmatch(r"[A-Za-z0-9._+-]{1,128}", loader_version)):
            errors.append("укажите корректный minecraft.loaderVersion")
    java = manifest.get("java")
    if "java" in manifest and not isinstance(java, dict):
        errors.append("java должен быть JSON-объектом")
    elif isinstance(java, dict):
        if set(java) - JAVA_FIELDS:
            errors.append("java содержит неизвестные параметры")
        if "major" in java and (type(java["major"]) is not int or java["major"] not in SUPPORTED_JAVA_MAJORS):
            errors.append("java.major должен быть одним из поддерживаемых значений: 8, 11, 16, 17, 21, 25")
    files = manifest.get("files", [])
    if not isinstance(files, list) or len(files) > 100000:
        errors.append("files должен быть массивом не более чем из 100000 записей")
        files = []
    total_size = 0
    seen_paths = set()
    for index, item in enumerate(files):
        prefix = f"files[{index}]"
        if not isinstance(item, dict):
            errors.append(f"{prefix}: ожидается объект")
            continue
        if set(item) - FILE_FIELDS:
            errors.append(f"{prefix}: содержит неизвестные поля")
        if "path" in item:
            try:
                path = safe_relative_path(item["path"])
                if path.casefold() in seen_paths:
                    errors.append(f"{prefix}: повторяется путь {path}")
                seen_paths.add(path.casefold())
            except ValueError as error:
                errors.append(f"{prefix}: {error}")
        digest = item.get("hash")
        if digest is not None and (not isinstance(digest, str) or not re.fullmatch(r"sha256:[a-f0-9]{64}", digest)):
            errors.append(f"{prefix}: hash должен быть sha256:<64 hex>")
        size = item.get("size")
        if size is not None:
            if type(size) is not int or not 0 <= size <= MAX_FILE_BYTES:
                errors.append(f"{prefix}: size должен быть целым числом от 0 до {MAX_FILE_BYTES // (1024 ** 3)} GiB")
            else:
                total_size += size
        if "required" in item and not isinstance(item["required"], bool):
            errors.append(f"{prefix}: required должен быть boolean")
        if "url" in item and (not isinstance(item["url"], str) or not re.fullmatch(r"/api/files/sha256:[a-f0-9]{64}", item["url"])):
            errors.append(f"{prefix}: url должен ссылаться на content-addressed файл")
        if digest and item.get("url") and item["url"] != f"/api/files/{digest}":
            errors.append(f"{prefix}: url не совпадает с hash")
    if total_size > 20_000_000_000:
        errors.append("общий размер сборки превышает 20 GB")
    settings = manifest.get("settings")
    if "settings" in manifest and not isinstance(settings, dict):
        errors.append("settings должен быть JSON-объектом")
    elif isinstance(settings, dict):
        if set(settings) - {"memoryMb", "jvmArgs", "windowWidth", "windowHeight", "fullscreen"}:
            errors.append("settings содержит неизвестные параметры")
        for key, low, high in (("memoryMb", 512, 65536), ("windowWidth", 640, 7680), ("windowHeight", 480, 4320)):
            if key in settings and (type(settings[key]) is not int or not low <= settings[key] <= high):
                errors.append(f"settings.{key} вне допустимого диапазона")
        if "jvmArgs" in settings and (not isinstance(settings["jvmArgs"], str) or len(settings["jvmArgs"]) > 2000):
            errors.append("settings.jvmArgs должен быть строкой до 2000 символов")
        if "fullscreen" in settings and not isinstance(settings["fullscreen"], bool):
            errors.append("settings.fullscreen должен быть boolean")
    return errors


def public_pack(row):
    return {"id": row["id"], "name": row["title"], "description": row["description"],
            "authors": json.loads(row["authors"]), "categories": json.loads(row["categories"]),
            "iconUrl": row["icon_url"]}


def public_release(row):
    return {"version": row["version"], "channel": row["channel"], "updatedAt": row["updated_at"]}


def release_needs_attention(row):
    validated_at = row["validated_at"]
    ready = row["ready"]
    return not ready or validated_at is None or int(time.time()) - validated_at > RELEASE_READINESS_SECONDS


def cached_release_attention(rows):
    cache_key = tuple((row["id"], row["ready"], row["validated_at"]) for row in rows)
    now = time.time()
    with dashboard_attention_lock:
        if dashboard_attention_cache["key"] == cache_key and dashboard_attention_cache["expires_at"] > now:
            return dashboard_attention_cache["values"]
        values = {row["id"]: release_needs_attention(row) for row in rows}
        dashboard_attention_cache.update(key=cache_key, expires_at=now + 5, values=values)
        return values


def static_asset_hash(path, metadata=None):
    metadata = metadata or path.stat()
    key = str(path.resolve())
    signature = (metadata.st_mtime_ns, metadata.st_size)
    with static_asset_hashes_lock:
        cached = static_asset_hashes.get(key)
        if cached and cached[0] == signature:
            return cached[1]
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        static_asset_hashes[key] = (signature, digest)
        return digest


def release_etag(manifest_text, channel):
    value = f"{channel}\0{manifest_text}".encode("utf-8")
    return f'"{hashlib.sha256(value).hexdigest()}"'


def manifest_hash(manifest_text):
    return hashlib.sha256(manifest_text.encode("utf-8")).hexdigest()


def hash_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def download_context(connection, digest, query_string):
    try:
        query = urllib.parse.parse_qs(query_string, max_num_fields=16)
    except ValueError:
        return "", "", ""
    pack_id = query.get("packId", [""])[0][:64]
    version = query.get("version", [""])[0][:64]
    channel = query.get("channel", [""])[0][:8]
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{1,62}[a-z0-9]", pack_id) or semver_sort_key(version)[0] != 1 or channel not in ("stable", "beta"):
        return "", "", ""
    release = connection.execute("SELECT manifest FROM releases WHERE pack_id=? AND version=? AND channel=? AND status='published'", (pack_id, version, channel)).fetchone()
    if not release:
        return "", "", ""
    try:
        manifest = json.loads(release[0])
    except (json.JSONDecodeError, TypeError):
        return "", "", ""
    digest_ref = f"sha256:{digest}"
    matches = any(isinstance(item, dict) and (item.get("hash") == digest_ref or item.get("url") == f"/api/files/{digest_ref}") for item in manifest.get("files", [])) if isinstance(manifest, dict) else False
    return (pack_id, version, channel) if matches else ("", "", "")


def enqueue_download_event(event):
    try:
        download_event_queue.put_nowait(event)
    except Full:
        print("Очередь аналитики скачиваний переполнена; событие отброшено", flush=True)


def process_download_events(events):
    if not events:
        return
    with db() as connection:
        for event in events:
            digest = event["hash"]
            visitor = event["visitor"]
            inserted = connection.execute("INSERT OR IGNORE INTO blob_downloads(hash,visitor) VALUES(?,?)", (digest, visitor)).rowcount
            if inserted:
                connection.execute("UPDATE blobs SET unique_download_count=unique_download_count+1 WHERE hash=?", (digest,))
            context = download_context(connection, digest, event["query"])
            connection.execute("INSERT OR IGNORE INTO blob_downloads_daily(download_date,hash,visitor,pack_id,version,channel) VALUES(?,?,?,?,?,?)",
                (event["date"], digest, visitor, *context))


def take_download_batch(first=None, maximum=1000):
    events = [] if first is None else [first]
    while len(events) < maximum:
        try:
            events.append(download_event_queue.get_nowait())
        except Empty:
            break
    return events


def flush_download_events():
    events = take_download_batch()
    if events:
        process_download_events(events)
        for _ in events:
            download_event_queue.task_done()
    return len(events)


def download_analytics_worker(stop_event):
    while not stop_event.is_set() or not download_event_queue.empty():
        try:
            first = download_event_queue.get(timeout=0.25)
        except Empty:
            continue
        events = take_download_batch(first)
        try:
            process_download_events(events)
        except Exception as error:
            print(f"Не удалось записать аналитику скачиваний: {error}", flush=True)
            record_system_error("download_analytics", error)
        finally:
            for _ in events:
                download_event_queue.task_done()


def reserve_upload_space(length):
    global reserved_upload_bytes
    reserve = max(0, int(CONFIG.get("minimumFreeBytes", 100 * 1024 * 1024)))
    with upload_reservation_lock:
        try:
            free = shutil.disk_usage(ROOT).free
        except OSError:
            return False, 0, reserve
        if free - reserved_upload_bytes < length + reserve:
            return False, free, reserve
        reserved_upload_bytes += length
        return True, free, reserve


def release_upload_space(length):
    global reserved_upload_bytes
    with upload_reservation_lock:
        reserved_upload_bytes = max(0, reserved_upload_bytes - length)


def validate_release_artifacts(connection, manifest):
    errors = []
    signatures = {}
    icon_url = manifest.get("iconUrl")
    if icon_url:
        icon_digest = icon_url.split(":", 1)[1]
        icon_blob = connection.execute("SELECT size FROM blobs WHERE hash=?", (icon_digest,)).fetchone()
        icon_path = OBJECTS / icon_digest[:2] / icon_digest
        try:
            if not icon_blob or not icon_path.is_file():
                errors.append("Файл иконки не загружен")
            else:
                signature = file_signature(icon_path)
                if signature["size"] != icon_blob["size"] or hash_file(icon_path) != icon_digest:
                    errors.append("Размер или SHA-256 файла иконки не совпадает")
                else:
                    signatures[icon_digest] = signature
        except OSError as error:
            errors.append(f"Не удалось проверить файл иконки: {error}")
    for item in manifest["files"]:
        digest = item["hash"].removeprefix("sha256:")
        blob = connection.execute("SELECT size FROM blobs WHERE hash=?", (digest,)).fetchone()
        target = OBJECTS / digest[:2] / digest
        try:
            if not blob or not target.is_file():
                errors.append(f"Не загружен файл {item['path']} ({digest})")
            else:
                signature = file_signature(target)
                if blob["size"] != item["size"] or signature["size"] != item["size"] or hash_file(target) != digest:
                    errors.append(f"Размер или SHA-256 файла не совпадает: {item['path']}")
                else:
                    signatures[digest] = signature
        except OSError as error:
            errors.append(f"Не удалось проверить файл {item['path']}: {error}")
    return errors, signatures


def file_signature(path):
    metadata = path.stat()
    return {"size": metadata.st_size, "mtimeNs": metadata.st_mtime_ns,
        "ctimeNs": metadata.st_ctime_ns, "inode": metadata.st_ino}


def verify_release_artifact_signatures(connection, manifest, signatures):
    errors = []
    references = []
    icon_url = manifest.get("iconUrl")
    if icon_url:
        references.append((icon_url.split(":", 1)[1], None, "Файл иконки"))
    references.extend((item["hash"].removeprefix("sha256:"), item["size"], item["path"]) for item in manifest["files"])
    for digest, expected_size, label in references:
        expected = signatures.get(digest) if isinstance(signatures, dict) else None
        blob = connection.execute("SELECT size FROM blobs WHERE hash=?", (digest,)).fetchone()
        target = OBJECTS / digest[:2] / digest
        if not expected or not blob or not target.is_file():
            errors.append(f"Артефакт {label} изменился после проверки; запустите проверку повторно")
            continue
        try:
            current = file_signature(target)
        except OSError as error:
            errors.append(f"Не удалось повторно проверить {label}: {error}")
            continue
        if current != expected or blob["size"] != expected["size"] or (expected_size is not None and current["size"] != expected_size):
            errors.append(f"Артефакт {label} изменился после проверки; запустите проверку повторно")
    return errors


class LimitedThreadingHTTPServer(ThreadingHTTPServer):
    request_queue_size = 128
    max_workers = 64
    daemon_threads = False
    block_on_close = True

    def __init__(self, server_address, request_handler_class):
        super().__init__(server_address, request_handler_class)
        self.worker_slots = threading.BoundedSemaphore(self.max_workers)

    def process_request(self, request, client_address):
        if not self.worker_slots.acquire(blocking=False):
            try:
                request_id = secrets.token_hex(12)
                body = json.dumps({"error": {"code": "SERVICE_BUSY", "message": "Сервер перегружен", "fields": [], "requestId": request_id}}, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
                request.settimeout(1)
                request.sendall(
                    b"HTTP/1.1 503 Service Unavailable\r\n"
                    b"Content-Type: application/json; charset=utf-8\r\n"
                    b"Retry-After: 1\r\nConnection: close\r\n"
                    + f"X-Request-ID: {request_id}\r\nContent-Length: {len(body)}\r\n\r\n".encode("ascii")
                    + body
                )
            except OSError:
                pass
            finally:
                self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except Exception:
            self.worker_slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.worker_slots.release()


class Handler(BaseHTTPRequestHandler):
    server_version = "MyLauncherRepository/1"
    protocol_version = "HTTP/1.1"
    timeout = 30

    def handle_one_request(self):
        self.request_id = secrets.token_hex(12)
        self.response_started = False
        try:
            super().handle_one_request()
        except (BrokenPipeError, ConnectionResetError) as error:
            self.close_connection = True
            print(f"Клиент закрыл соединение; requestId={self.request_id}: {error}", flush=True)
        except Exception as error:
            self.close_connection = True
            print(f"Необработанная ошибка HTTP-запроса; requestId={self.request_id}", flush=True)
            traceback.print_exc()
            record_system_error("http_request", f"requestId={self.request_id}: {error}")
            if not self.response_started:
                try:
                    self.send_json(500, {"error": "Внутренняя ошибка сервера"})
                except (BrokenPipeError, ConnectionResetError, OSError):
                    pass

    def send_response(self, code, message=None):
        super().send_response(code, message)
        self.response_started = True
        self.send_header("X-Request-ID", self.request_id)

    def log_message(self, fmt, *args):
        print(f"{self.log_date_time_string()} {self.remote_address()} {fmt % args}")

    def stream_audit_csv(self, where, parameters, result_column, maximum_id):
        self.send_response(200)
        self.send_header("Content-Type", "text/csv; charset=utf-8")
        self.send_header("Content-Disposition", 'attachment; filename="audit.csv"')
        self.send_header("Cache-Control", "no-store")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        output = io.StringIO(newline="")
        writer = csv.writer(output)
        writer.writerow(("at", "actor", "action", "result", "detail", "remote"))
        chunk = output.getvalue().encode("utf-8-sig")
        self.wfile.write(f"{len(chunk):X}\r\n".encode("ascii") + chunk + b"\r\n")
        output.seek(0)
        output.truncate(0)
        last_id = maximum_id + 1 if maximum_id is not None else 0
        while last_id > 0:
            condition = f"{where} AND id < ?" if where else "WHERE id < ?"
            with db() as connection:
                rows = connection.execute(
                    f"SELECT id, at, actor, action, detail, remote, {result_column} FROM audit {condition} ORDER BY id DESC LIMIT 500",
                    (*parameters, last_id),
                ).fetchall()
            if not rows:
                break
            writer.writerows((row["at"], row["actor"], row["action"], row["result"], row["detail"], row["remote"]) for row in rows)
            chunk = output.getvalue().encode("utf-8")
            if chunk:
                self.wfile.write(f"{len(chunk):X}\r\n".encode("ascii") + chunk + b"\r\n")
            output.seek(0)
            output.truncate(0)
            last_id = rows[-1]["id"]
        self.wfile.write(b"0\r\n\r\n")
        self.wfile.flush()

    def remote_address(self):
        peer = self.client_address[0]
        forwarded = self.headers.get("X-Forwarded-For", "").rsplit(",", 1)[-1].strip()
        if peer in ("127.0.0.1", "::1") and forwarded:
            try:
                return str(ipaddress.ip_address(forwarded))
            except ValueError:
                pass
        return peer

    def send_json(self, status, payload, cache_control="no-store", etag=None):
        if isinstance(payload, dict) and isinstance(payload.get("error"), str):
            message = payload["error"]
            fields = []
            for item in payload.get("validationErrors", []):
                text = str(item)
                indexed = re.match(r"files\[(\d+)\]", text)
                required = re.match(r"нет обязательного поля: ([A-Za-z0-9_.]+)", text)
                dotted = re.match(r"([A-Za-z][A-Za-z0-9_.]*)\b", text)
                field = f"files.{indexed.group(1)}" if indexed else required.group(1) if required else dotted.group(1) if dotted and "." in dotted.group(1) else "manifest"
                fields.append({"path": field, "code": "INVALID", "message": text})
            code = "VALIDATION_FAILED" if fields else {400: "BAD_REQUEST", 401: "UNAUTHENTICATED", 403: "FORBIDDEN", 404: "NOT_FOUND", 409: "CONFLICT", 413: "PAYLOAD_TOO_LARGE", 415: "UNSUPPORTED_MEDIA_TYPE", 422: "VALIDATION_FAILED", 429: "RATE_LIMITED", 502: "UPSTREAM_ERROR", 507: "INSUFFICIENT_STORAGE"}.get(status, "REQUEST_FAILED")
            payload = {**payload, "error": {"code": code, "message": message, "fields": fields, "requestId": self.request_id}}
        request_id = payload.get("error", {}).get("requestId") if isinstance(payload, dict) and isinstance(payload.get("error"), dict) else None
        body = json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        if cache_control != "no-store" and etag is None:
            etag = f'"{hashlib.sha256(body).hexdigest()}"'
        if etag and if_none_match_matches(self.headers.get("If-None-Match"), etag):
            self.send_response(304)
            self.send_header("Cache-Control", cache_control)
            self.send_header("ETag", etag)
            self.send_header("X-Content-Type-Options", "nosniff")
            self.end_headers()
            return
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", cache_control)
        if self.close_connection:
            self.send_header("Connection", "close")
        if request_id:
            self.send_header("X-Request-ID", request_id)
        if etag:
            self.send_header("ETag", etag)
        self.send_header("X-Content-Type-Options", "nosniff")
        self.end_headers()
        if not getattr(self, "head_only", False):
            self.wfile.write(body)

    def read_json(self, limit=MAX_MANIFEST_BYTES):
        length = int(self.headers.get("Content-Length", "0"))
        if length <= 0 or length > limit:
            raise ValueError("Некорректный размер JSON-запроса")
        value = json.loads(self.rfile.read(length))
        if not isinstance(value, dict):
            raise ValueError("JSON-запрос должен быть объектом")
        return value

    def send_blob(self, digest, head_only=False):
        target = OBJECTS / digest[:2] / digest
        if not target.is_file():
            if head_only:
                self.send_response(404)
                self.send_header("Content-Length", "0")
                self.end_headers()
            else:
                self.send_json(404, {"error": "Файл не найден"})
            return
        metadata = target.stat()
        size = metadata.st_size
        etag = f'"sha256-{digest}"'
        modified = time.strftime("%a, %d %b %Y %H:%M:%S GMT", time.gmtime(metadata.st_mtime))
        if if_none_match_matches(self.headers.get("If-None-Match"), etag) or (
            not self.headers.get("If-None-Match")
            and self.headers.get("If-Modified-Since") == modified
        ):
            self.send_response(304)
            self.send_header("ETag", etag)
            self.send_header("Last-Modified", modified)
            self.send_header("Cache-Control", "public, max-age=31536000, immutable")
            self.end_headers()
            return
        byte_range = parse_byte_range(self.headers.get("Range"), size)
        if self.headers.get("If-Range") and self.headers["If-Range"] != etag and self.headers["If-Range"] != modified:
            byte_range = None
        if byte_range is False:
            self.send_response(416)
            self.send_header("Content-Range", f"bytes */{size}")
            self.send_header("Content-Length", "0")
            self.send_header("Accept-Ranges", "bytes")
            self.end_headers()
            return
        start, end = byte_range if byte_range is not None else (0, size - 1)
        response_size = max(0, end - start + 1)
        content_type = image_content_type(target) or "application/octet-stream"
        if not head_only:
            visitor = hmac.new(DOWNLOAD_VISITOR_SECRET, self.remote_address().encode("utf-8"), hashlib.sha256).hexdigest()
            event = {"hash": digest, "visitor": visitor, "query": urllib.parse.urlsplit(self.path).query,
                "date": time.strftime("%Y-%m-%d", time.gmtime())}
            try:
                enqueue_download_event(event)
            except Exception as error:
                print(f"Не удалось поставить событие скачивания в очередь: {error}", flush=True)
        self.send_response(206 if byte_range is not None else 200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(response_size))
        self.send_header("ETag", etag)
        self.send_header("Last-Modified", modified)
        self.send_header("Cache-Control", "public, max-age=31536000, immutable")
        self.send_header("Accept-Ranges", "bytes")
        if byte_range is not None:
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.end_headers()
        if head_only:
            return
        with target.open("rb") as source:
            source.seek(start)
            remaining = response_size
            while remaining:
                chunk = source.read(min(1024 * 1024, remaining))
                if not chunk:
                    break
                self.wfile.write(chunk)
                remaining -= len(chunk)

    def download_context(self, connection, digest):
        return download_context(connection, digest, urllib.parse.urlsplit(self.path).query)

    def require_admin(self):
        authorization = self.headers.get("Authorization", "")
        bearer = authorization.removeprefix("Bearer ") if authorization.startswith("Bearer ") else ""
        cookie = SimpleCookie()
        try:
            cookie.load(self.headers.get("Cookie", ""))
        except Exception:
            cookie = SimpleCookie()
        cookie_token = cookie.get("repo-session").value if cookie.get("repo-session") else ""
        token = cookie_token or bearer
        session = None
        if token:
            with db() as connection:
                session = connection.execute("SELECT csrf_token, expires_at FROM admin_sessions WHERE token_hash=?", (hashlib.sha256(token.encode("utf-8")).hexdigest(),)).fetchone()
        expiry = session["expires_at"] if session else None
        if not token or not expiry or expiry < time.time():
            if self.command not in ("GET", "HEAD", "OPTIONS"):
                self.close_connection = True
            self.send_json(401, {"error": "Требуется вход администратора"})
            return False
        if cookie_token and self.command not in ("GET", "HEAD", "OPTIONS"):
            supplied = self.headers.get("X-CSRF-Token", "")
            if not session or not hmac.compare_digest(supplied, session["csrf_token"]):
                self.close_connection = True
                self.send_json(403, {"error": "Некорректный CSRF-токен"})
                return False
        self.actor = "admin"
        self.admin_token = token
        return True

    def do_GET(self):
        path = urllib.parse.urlsplit(self.path).path
        static_name = path[1:] if path.count("/") == 1 else ""
        if path == "/" or path == "/index.html" or (static_name and Path(static_name).suffix.lower() in STATIC_ASSET_EXTENSIONS):
            file_path = WEB_ROOT / ("index.html" if path == "/" else static_name)
            if file_path.resolve().parent != WEB_ROOT.resolve():
                self.send_json(404, {"error": "Маршрут не найден"})
                return
            if not file_path.is_file():
                self.send_json(404, {"error": "Админ-панель не установлена"})
                return
            body = file_path.read_bytes()
            if file_path.name == "index.html":
                for asset in WEB_ROOT.iterdir():
                    if asset.is_file() and asset.suffix in (".js", ".css"):
                        version = static_asset_hash(asset, asset.stat())[:16]
                        original = f'"/{asset.name}"'.encode("utf-8")
                        versioned = f'"/{asset.name}?v={version}"'.encode("utf-8")
                        body = body.replace(original, versioned)
            content_type = mimetypes.guess_type(file_path.name)[0] or "application/octet-stream"
            metadata = file_path.stat()
            digest = static_asset_hash(file_path, metadata) if file_path.suffix in (".js", ".css") else hashlib.sha256(body).hexdigest()
            etag = f'"sha256-{digest}"'
            modified = time.strftime("%a, %d %b %Y %H:%M:%S GMT", time.gmtime(metadata.st_mtime))
            cache_control = "no-cache" if file_path.name == "index.html" else (
                "public, max-age=31536000, immutable" if urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query).get("v") else "no-cache"
            )
            if if_none_match_matches(self.headers.get("If-None-Match"), etag) or (
                not self.headers.get("If-None-Match") and self.headers.get("If-Modified-Since") == modified
            ):
                self.send_response(304)
                self.send_header("ETag", etag)
                self.send_header("Last-Modified", modified)
                self.send_header("Cache-Control", cache_control)
                self.end_headers()
                return
            self.send_response(200)
            self.send_header("Content-Type", f"{content_type}; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", cache_control)
            self.send_header("ETag", etag)
            self.send_header("Last-Modified", modified)
            self.send_header("Content-Security-Policy", "default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.end_headers()
            if not getattr(self, "head_only", False):
                self.wfile.write(body)
            return
        if path == "/api/admin/session" and self.require_admin():
            with db() as connection:
                session = connection.execute("SELECT csrf_token FROM admin_sessions WHERE token_hash=?", (hashlib.sha256(self.admin_token.encode("utf-8")).hexdigest(),)).fetchone()
            if not session:
                self.send_json(401, {"error": "Сессия истекла"})
                return
            self.send_json(200, {"csrfToken": session["csrf_token"]})
            return
        if path == "/api/admin/audit":
            if not self.require_admin():
                return
            query = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
            try:
                page = max(1, int(query.get("page", ["1"])[0]))
                per_page = min(200, max(1, int(query.get("perPage", ["50"])[0])))
            except ValueError:
                self.send_json(400, {"error": "page и perPage должны быть целыми числами"})
                return
            action = query.get("action", [""])[0].strip()[:80]
            search = query.get("search", [""])[0].strip()[:200]
            actor = query.get("actor", [""])[0].strip()[:128]
            result = query.get("result", [""])[0].strip().lower()
            date_from = query.get("from", [""])[0].strip()
            date_to = query.get("to", [""])[0].strip()
            try:
                if date_from:
                    if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", date_from):
                        raise ValueError("invalid from date format")
                    date.fromisoformat(date_from)
                if date_to:
                    if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", date_to):
                        raise ValueError("invalid to date format")
                    date.fromisoformat(date_to)
            except ValueError:
                self.send_json(400, {"error": "Даты журнала должны иметь формат YYYY-MM-DD"})
                return
            if result not in ("", "success", "failure"):
                self.send_json(400, {"error": "result должен быть success или failure"})
                return
            conditions = []
            parameters = []
            if action:
                conditions.append("action=?")
                parameters.append(action)
            if search:
                conditions.append("(actor LIKE ? OR action LIKE ? OR detail LIKE ? OR remote LIKE ?)")
                parameters.extend([f"%{search}%"] * 4)
            if actor:
                conditions.append("actor=?")
                parameters.append(actor)
            if date_from:
                conditions.append("date(at,'unixepoch')>=?")
                parameters.append(date_from)
            if date_to:
                conditions.append("date(at,'unixepoch')<=?")
                parameters.append(date_to)
            failure_filter = "(action LIKE '%failed%' OR action LIKE '%failure%' OR action LIKE '%denied%')"
            if result == "failure":
                conditions.append(failure_filter)
            elif result == "success":
                conditions.append(f"NOT {failure_filter}")
            where = f"WHERE {' AND '.join(conditions)}" if conditions else ""
            with db() as connection:
                total = connection.execute(f"SELECT COUNT(*) FROM audit {where}", parameters).fetchone()[0]
                result_column = f"CASE WHEN {failure_filter} THEN 'failure' ELSE 'success' END AS result"
                export_csv = query.get("export", [""])[0].lower() == "csv"
                if export_csv:
                    maximum_id = connection.execute("SELECT MAX(id) FROM audit").fetchone()[0]
                else:
                    logs = connection.execute(f"SELECT at, actor, action, detail, remote, {result_column} FROM audit {where} ORDER BY id DESC LIMIT ? OFFSET ?",
                        (*parameters, per_page, (page - 1) * per_page)).fetchall()
            if export_csv:
                self.stream_audit_csv(where, parameters, result_column, maximum_id)
                return
            self.send_json(200, {"audit": [dict(row) for row in logs], "page": page, "perPage": per_page,
                "total": total, "pages": max(1, (total + per_page - 1) // per_page)})
            return
        if path == "/api/admin/storage":
            if not self.require_admin():
                return
            with db() as connection:
                blobs = connection.execute("SELECT COUNT(*) AS count, COALESCE(SUM(size),0) AS bytes FROM blobs").fetchone()
                downloads = connection.execute("SELECT COALESCE(SUM(unique_download_count),0) AS count FROM blobs").fetchone()["count"]
            try:
                free_bytes = shutil.disk_usage(ROOT).free
            except OSError:
                free_bytes = 0
            self.send_json(200, {"files": blobs["count"], "bytes": blobs["bytes"], "downloads": downloads,
                "freeBytes": free_bytes, "minimumFreeBytes": int(CONFIG.get("minimumFreeBytes", 100 * 1024 * 1024)),
                "maxUploadBytes": MAX_UPLOAD})
            return
        if path == "/api/admin/system":
            if not self.require_admin():
                return
            try:
                self.send_json(200, system_status_snapshot())
            except (OSError, sqlite3.Error) as error:
                record_system_error("system_status", error)
                self.send_json(503, {"error": "Не удалось собрать состояние системы"})
            return
        if path == "/api/admin/backups":
            if not self.require_admin():
                return
            try:
                from backup import list_backups

                self.send_json(200, {"backups": list_backups(BACKUP_DIR)})
            except OSError as error:
                record_system_error("backup_history", error)
                self.send_json(500, {"error": "Не удалось прочитать историю резервных копий"})
            return
        if path == "/api/admin/stats":
            if not self.require_admin():
                return
            query = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
            try:
                days = min(365, max(1, int(query.get("days", ["30"])[0])))
                page = max(1, int(query.get("page", ["1"])[0]))
                per_page = min(100, max(1, int(query.get("perPage", ["50"])[0])))
            except ValueError:
                self.send_json(400, {"error": "days, page и perPage должны быть целыми числами с допустимыми значениями"})
                return
            since = time.strftime("%Y-%m-%d", time.gmtime(time.time() - days * 24 * 60 * 60))
            with db() as connection:
                total = connection.execute("""
                    SELECT COUNT(*) FROM (
                        SELECT 1 FROM blob_downloads_daily WHERE download_date>=?
                        GROUP BY download_date,pack_id,version,channel
                    )
                """, (since,)).fetchone()[0]
                pages = max(1, (total + per_page - 1) // per_page)
                page = min(page, pages)
                rows = connection.execute("""
                    SELECT download_date,pack_id,version,channel,COUNT(*) AS file_visitors,COUNT(DISTINCT hash) AS files
                    FROM blob_downloads_daily WHERE download_date>=?
                    GROUP BY download_date,pack_id,version,channel
                    ORDER BY download_date DESC,file_visitors DESC,pack_id,version,channel
                    LIMIT ? OFFSET ?
                """, (since, per_page, (page - 1) * per_page)).fetchall()
                daily = connection.execute("""
                    SELECT download_date,COUNT(*) AS file_visitors,COUNT(DISTINCT hash) AS files
                    FROM blob_downloads_daily WHERE download_date>=?
                    GROUP BY download_date ORDER BY download_date
                """, (since,)).fetchall()
                top_packs = connection.execute("""
                    SELECT pack_id,COUNT(*) AS file_visitors,COUNT(DISTINCT hash) AS files
                    FROM blob_downloads_daily WHERE download_date>=? AND pack_id!=''
                    GROUP BY pack_id ORDER BY file_visitors DESC,pack_id LIMIT 10
                """, (since,)).fetchall()
            self.send_json(200, {"days": days, "metric": "unique_blob_visitors_per_day", "page": page,
                "perPage": per_page, "pages": pages, "total": total, "stats": [dict(row) for row in rows],
                "daily": [dict(row) for row in daily], "topPacks": [dict(row) for row in top_packs]})
            return
        if path == "/api/admin/gc":
            if not self.require_admin():
                return
            with gc_status_lock:
                self.send_json(200, dict(gc_status))
            return
        if path == "/api/admin/dashboard" and self.require_admin():
            query = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
            paginate = "page" in query or "perPage" in query
            try:
                page = max(1, int(query.get("page", ["1"])[0]))
                per_page = min(200, max(1, int(query.get("perPage", ["100"])[0])))
            except ValueError:
                self.send_json(400, {"error": "page и perPage должны быть целыми числами"})
                return
            now = int(time.time())
            with db() as connection:
                packs = connection.execute("SELECT * FROM packs ORDER BY updated_at DESC").fetchall()
                release_total = connection.execute("SELECT COUNT(*) FROM releases").fetchone()[0]
                release_pages = max(1, (release_total + per_page - 1) // per_page)
                page = min(page, release_pages)
                release_summary = connection.execute("""
                    SELECT COUNT(*) AS total,
                        COALESCE(SUM(status='draft'),0) AS drafts,
                        COALESCE(SUM(status='published'),0) AS published,
                        COALESCE(SUM(channel='beta'),0) AS beta
                    FROM releases
                """).fetchone()
                needs_attention = connection.execute("""
                    SELECT COUNT(*) FROM releases AS r
                    LEFT JOIN release_readiness AS rr ON rr.release_id=r.id
                    WHERE COALESCE(rr.ready,0)=0 OR rr.validated_at IS NULL OR rr.validated_at < ?
                """, (now - RELEASE_READINESS_SECONDS,)).fetchone()[0]
                release_query = """
                    SELECT r.id,r.pack_id,r.version,r.status,r.channel,r.published_at,r.updated_at,
                        CASE WHEN json_valid(r.manifest) THEN json_extract(r.manifest,'$.minecraft.version') ELSE '' END AS game_version,
                        CASE WHEN json_valid(r.manifest) THEN json_extract(r.manifest,'$.minecraft.loader') ELSE '' END AS loader,
                        rr.ready,rr.validated_at,rr.validation_json
                    FROM releases AS r LEFT JOIN release_readiness AS rr ON rr.release_id=r.id
                    ORDER BY r.updated_at DESC
                """
                releases = connection.execute(
                    release_query + (" LIMIT ? OFFSET ?" if paginate else ""),
                    (per_page, (page - 1) * per_page) if paginate else (),
                ).fetchall()
                blobs = connection.execute("SELECT COUNT(*) AS count, COALESCE(SUM(size),0) AS bytes FROM blobs").fetchone()
                logs = connection.execute("SELECT at, actor, action, detail, remote FROM audit ORDER BY id DESC LIMIT 50").fetchall()
                downloads = connection.execute("SELECT COALESCE(SUM(unique_download_count),0) AS count FROM blobs").fetchone()["count"]
            attention = cached_release_attention(releases)
            release_items = []
            for row in releases:
                validation = {}
                try:
                    validation = json.loads(row["validation_json"] or "{}")
                except (TypeError, json.JSONDecodeError):
                    pass
                reasons = validation.get("errors", []) if isinstance(validation, dict) else []
                if attention[row["id"]] and not reasons:
                    if row["validated_at"] is None:
                        reasons = ["Проверка готовности не выполнялась"]
                    elif int(time.time()) - row["validated_at"] > RELEASE_READINESS_SECONDS:
                        reasons = ["Результат проверки устарел"]
                    else:
                        reasons = ["Готовность версии не подтверждена"]
                release_items.append({"id": row["id"], "packId": row["pack_id"], "version": row["version"],
                    "status": row["status"], "channel": row["channel"], "immutable": row["published_at"] is not None,
                    "needsAttention": attention[row["id"]], "attentionReasons": reasons,
                    "ready": bool(row["ready"]), "validatedAt": row["validated_at"],
                    "gameVersion": row["game_version"] or "", "loader": row["loader"] or "",
                    "updatedAt": row["updated_at"]})
            try:
                free_bytes = shutil.disk_usage(ROOT).free
            except OSError:
                free_bytes = 0
            self.send_json(200, {"packs": [public_pack(pack) for pack in packs],
                "releases": release_items,
                "releaseSummary": {**dict(release_summary), "needsAttention": needs_attention},
                "releasePagination": {"page": page, "perPage": per_page if paginate else release_total,
                    "pages": release_pages, "total": release_total},
                "storage": {"files": blobs["count"], "bytes": blobs["bytes"], "downloads": downloads,
                    "freeBytes": free_bytes, "minimumFreeBytes": int(CONFIG.get("minimumFreeBytes", 100 * 1024 * 1024)),
                    "maxUploadBytes": MAX_UPLOAD}, "audit": [dict(row) for row in logs]})
            return
        if path == "/api/admin/dashboard":
            return
        if path == "/api/admin/session":
            return
        release_match = re.fullmatch(r"/api/admin/releases/(\d+)", path)
        if release_match:
            if not self.require_admin():
                return
            with db() as connection:
                release = connection.execute("SELECT * FROM releases WHERE id=?", (int(release_match.group(1)),)).fetchone()
            if not release:
                self.send_json(404, {"error": "Релиз не найден"})
                return
            etag = release_etag(release["manifest"], release["channel"])
            self.send_json(200, {"id": release["id"], "packId": release["pack_id"], "version": release["version"],
                "status": release["status"], "channel": release["channel"], "immutable": release["published_at"] is not None,
                "manifest": json.loads(release["manifest"]), "etag": etag}, etag=etag)
            return
        if path.startswith("/api/admin/options/"):
            if not self.require_admin():
                return
            query = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
            try:
                if path == "/api/admin/options/minecraft":
                    result = minecraft_options(force=query.get("refresh", [""])[0] == "1")
                elif path == "/api/admin/options/java":
                    result = java_major_for_version(query.get("gameVersion", [""])[0])
                elif path == "/api/admin/options/fabric":
                    result = fabric_metadata(
                        query.get("resource", [""])[0],
                        game_version=query.get("gameVersion", [""])[0],
                        loader_version=query.get("loaderVersion", [""])[0],
                        installer_version=query.get("installerVersion", [""])[0],
                        force=query.get("refresh", [""])[0] == "1",
                    )
                elif path == "/api/admin/options/loaders":
                    result = loader_options(
                        query.get("gameVersion", [""])[0],
                        query.get("loader", [""])[0],
                        force=query.get("refresh", [""])[0] == "1",
                    )
                else:
                    self.send_json(404, {"error": "Маршрут не найден"})
                    return
                self.send_json(200, result)
            except (urllib.error.URLError, TimeoutError, ValueError, ET.ParseError, json.JSONDecodeError) as error:
                self.send_json(502, {"error": f"Не удалось загрузить официальные метаданные: {error}"})
            return
        if path == "/api/live":
            self.send_json(200, {"ok": True})
            return
        if path == "/api/health":
            ready, checks = readiness_report()
            self.send_json(200 if ready else 503, {"ok": ready, "manifestVersion": 1, "checks": checks})
            return
        if path == "/api/packs":
            channel = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query).get("channel", ["stable"])[0]
            if channel not in ("stable", "beta"):
                self.send_json(400, {"error": "Неизвестный канал"})
                return
            with db() as connection:
                rows = connection.execute("""
                    SELECT DISTINCT p.* FROM packs p JOIN releases r ON r.pack_id=p.id
                    WHERE r.status='published' AND r.channel=?
                    ORDER BY p.title COLLATE NOCASE
                """, (channel,)).fetchall()
                latest_by_pack = {}
                for row in ordered_releases(connection.execute("SELECT pack_id, version, channel FROM releases WHERE status='published' AND channel=?", (channel,)).fetchall()):
                    latest_by_pack.setdefault(row["pack_id"], row)
                packs = [row for row in rows if row["id"] in latest_by_pack]
            self.send_json(200, {"packs": [{**public_pack(row), "latestVersion": latest_by_pack[row["id"]]["version"], "channel": channel} for row in packs]}, cache_control="public, max-age=45")
            return
        if path.startswith("/api/files/"):
            digest = path.removeprefix("/api/files/").removeprefix("sha256:")
            if not re.fullmatch(r"[a-f0-9]{64}", digest):
                self.send_json(400, {"error": "Некорректный SHA-256"})
                return
            self.send_blob(digest)
            return
        match = re.fullmatch(r"/api/packs/([a-z0-9-]+)(?:/versions(?:/([^/]+)/manifest)?)?", path)
        if match:
            pack_id, version = match.groups()
            with db() as connection:
                pack = connection.execute("SELECT * FROM packs WHERE id=?", (pack_id,)).fetchone()
                if not pack:
                    self.send_json(404, {"error": "Сборка не найдена"})
                    return
                if version is None and "/versions" not in path:
                    channel = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query).get("channel", ["stable"])[0]
                    if channel not in ("stable", "beta"):
                        self.send_json(400, {"error": "Неизвестный канал"})
                        return
                    rows = ordered_releases(connection.execute("SELECT version, channel, updated_at FROM releases WHERE pack_id=? AND status='published' AND channel=?", (pack_id, channel)).fetchall())
                    self.send_json(200, {"pack": public_pack(pack), "versions": [public_release(row) for row in rows]}, cache_control="public, max-age=45")
                    return
                if version is None:
                    channel = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query).get("channel", ["stable"])[0]
                    if channel not in ("stable", "beta"):
                        self.send_json(400, {"error": "Неизвестный канал"})
                        return
                    rows = ordered_releases(connection.execute("SELECT version, channel, updated_at FROM releases WHERE pack_id=? AND status='published' AND channel=?", (pack_id, channel)).fetchall())
                    self.send_json(200, {"versions": [public_release(row) for row in rows]}, cache_control="public, max-age=45")
                    return
                release = connection.execute("SELECT manifest, channel FROM releases WHERE pack_id=? AND version=? AND status='published'", (pack_id, version)).fetchone()
            requested_channel = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query).get("channel", ["stable"])[0]
            if not release or release["channel"] != requested_channel:
                self.send_json(404, {"error": "Опубликованная версия не найдена"})
                return
            etag = f'"{hashlib.sha256(release["manifest"].encode("utf-8")).hexdigest()}"'
            self.send_json(200, json.loads(release["manifest"]), cache_control="public, max-age=31536000, immutable", etag=etag)
            return
        if path == "/admin" or path.startswith("/admin/"):
            self.send_response(302)
            self.send_header("Location", "/")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        self.send_json(404, {"error": "Маршрут не найден"})

    def do_HEAD(self):
        path = urllib.parse.urlsplit(self.path).path
        if path in ("/api/live", "/api/health"):
            self.head_only = True
            try:
                self.do_GET()
            finally:
                del self.head_only
            return
        static_name = path[1:] if path.count("/") == 1 else ""
        if path in ("/", "/index.html") or (static_name and Path(static_name).suffix.lower() in STATIC_ASSET_EXTENSIONS):
            self.head_only = True
            try:
                self.do_GET()
            finally:
                del self.head_only
            return
        if path == "/api/packs" or re.fullmatch(r"/api/packs/[a-z0-9-]+(?:/versions(?:/[^/]+/manifest)?)?", path):
            self.head_only = True
            try:
                self.do_GET()
            finally:
                del self.head_only
            return
        if path.startswith("/api/files/"):
            digest = path.removeprefix("/api/files/").removeprefix("sha256:")
            if not re.fullmatch(r"[a-f0-9]{64}", digest):
                self.send_response(400)
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            self.send_blob(digest, head_only=True)
            return
        self.send_response(405)
        self.send_header("Allow", "GET, HEAD")
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_POST(self):
        global gc_status
        path = urllib.parse.urlsplit(self.path).path
        if path == "/api/admin/login":
            remote = self.remote_address()
            now = time.time()
            with db() as connection:
                limit = connection.execute("SELECT attempts, blocked_until FROM admin_login_limits WHERE remote=?", (remote,)).fetchone()
            attempts, blocked_until = (limit["attempts"], limit["blocked_until"]) if limit else (0, 0)
            if blocked_until and blocked_until <= now:
                attempts, blocked_until = 0, 0
            if blocked_until > now:
                self.send_json(429, {"error": "Слишком много попыток. Попробуйте позже."})
                return
            try:
                data = self.read_json(4096)
            except Exception:
                self.send_json(400, {"error": "Некорректный запрос"})
                return
            candidate = str(data.get("password", "")) if isinstance(data, dict) else ""
            if not PASSWORD_HASH or not password_matches(candidate):
                attempts += 1
                blocked_until = int(now + 900) if attempts >= 8 else 0
                with db() as connection:
                    connection.execute("INSERT INTO admin_login_limits(remote,attempts,blocked_until,updated_at) VALUES(?,?,?,?) ON CONFLICT(remote) DO UPDATE SET attempts=excluded.attempts,blocked_until=excluded.blocked_until,updated_at=excluded.updated_at", (remote, attempts, blocked_until, int(now)))
                audit("anonymous", "login_failed", "Неверный пароль", remote)
                self.send_json(401, {"error": "Неверный пароль"})
                return
            with db() as connection:
                connection.execute("DELETE FROM admin_login_limits WHERE remote=?", (remote,))
            token = secrets.token_urlsafe(32)
            csrf_token = secrets.token_urlsafe(32)
            with db() as connection:
                session_count = connection.execute("SELECT COUNT(*) FROM admin_sessions").fetchone()[0]
                if session_count >= MAX_ADMIN_SESSIONS:
                    expired = connection.execute("SELECT token_hash FROM admin_sessions ORDER BY created_at ASC LIMIT ?", (session_count - MAX_ADMIN_SESSIONS + 1,)).fetchall()
                    connection.executemany("DELETE FROM admin_sessions WHERE token_hash=?", ((row["token_hash"],) for row in expired))
                connection.execute("INSERT INTO admin_sessions(token_hash,csrf_token,expires_at,created_at) VALUES(?,?,?,?)", (hashlib.sha256(token.encode("utf-8")).hexdigest(), csrf_token, int(now + SESSION_SECONDS), int(now)))
            audit("admin", "login", "Успешный вход", remote)
            secure = CONFIG.get("secureAdminCookie", False) or self.headers.get("X-Forwarded-Proto", "").lower() == "https"
            cookie_flags = f"repo-session={token}; HttpOnly; SameSite=Strict; Path=/api/admin; Max-Age={SESSION_SECONDS}"
            self.send_response(200)
            self.send_header("Set-Cookie", cookie_flags + ("; Secure" if secure else ""))
            body = json.dumps({"csrfToken": csrf_token, "expiresIn": SESSION_SECONDS}, separators=(",", ":")).encode("utf-8")
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.end_headers()
            self.wfile.write(body)
            return
        if not path.startswith("/api/admin/") or not self.require_admin():
            if not path.startswith("/api/admin/"):
                self.send_json(404, {"error": "Маршрут не найден"})
            return
        try:
            if path == "/api/admin/logout":
                token = getattr(self, "admin_token", "")
                if token:
                    with db() as connection:
                        connection.execute("DELETE FROM admin_sessions WHERE token_hash=?", (hashlib.sha256(token.encode("utf-8")).hexdigest(),))
                audit(self.actor, "logout", "Сессия завершена", self.remote_address())
                body = json.dumps({"loggedOut": True}, separators=(",", ":")).encode("utf-8")
                self.send_response(200)
                self.send_header("Set-Cookie", "repo-session=; HttpOnly; SameSite=Strict; Path=/api/admin; Max-Age=0")
                self.send_header("Content-Type", "application/json; charset=utf-8")
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Cache-Control", "no-store")
                self.send_header("X-Content-Type-Options", "nosniff")
                self.end_headers()
                self.wfile.write(body)
            elif path == "/api/admin/files":
                self.upload_file()
            elif path == "/api/admin/packs":
                self.create_pack()
            elif path == "/api/admin/releases":
                self.create_release()
            elif re.fullmatch(r"/api/admin/releases/\d+/clone", path):
                self.clone_release(int(path.split("/")[-2]))
            elif re.fullmatch(r"/api/admin/releases/\d+/archive", path):
                self.archive_release(int(path.split("/")[-2]), True)
            elif re.fullmatch(r"/api/admin/releases/\d+/restore", path):
                self.archive_release(int(path.split("/")[-2]), False)
            elif path.startswith("/api/admin/releases/") and path.endswith("/publish"):
                self.publish_release(path.split("/")[-2], True)
            elif path.startswith("/api/admin/releases/") and path.endswith("/unpublish"):
                self.publish_release(path.split("/")[-2], False)
            elif re.fullmatch(r"/api/admin/releases/\d+/validate", path):
                self.read_json(4096)
                release_id = int(path.split("/")[-2])
                with db() as connection:
                    release = connection.execute("SELECT * FROM releases WHERE id=?", (release_id,)).fetchone()
                if not release:
                    self.send_json(404, {"error": "Релиз не найден"})
                else:
                    manifest = json.loads(release["manifest"])
                    errors = validate_manifest(manifest, release["pack_id"], release["version"])
                    artifact_signatures = {}
                    if not errors:
                        errors.extend(validate_official_metadata(manifest))
                    if not errors:
                        with db() as connection:
                            artifact_errors, artifact_signatures = validate_release_artifacts(connection, manifest)
                            errors.extend(artifact_errors)
                    validation_payload = {"errors": errors, "artifacts": artifact_signatures}
                    validated_at = int(time.time())
                    validation_hash = manifest_hash(release["manifest"])
                    with db() as connection:
                        saved = connection.execute(
                            "INSERT INTO release_readiness(release_id,validation_json,validated_at,validation_hash,ready) "
                            "SELECT ?,?,?,?,? WHERE EXISTS(SELECT 1 FROM releases WHERE id=? AND manifest=? AND status='draft') "
                            "ON CONFLICT(release_id) DO UPDATE SET validation_json=excluded.validation_json,validated_at=excluded.validated_at,validation_hash=excluded.validation_hash,ready=excluded.ready",
                            (release_id, json.dumps(validation_payload, ensure_ascii=False), validated_at, validation_hash, int(not errors), release_id, release["manifest"]),
                        ).rowcount
                    if not saved:
                        self.send_json(409, {"error": "Версия изменилась во время проверки. Повторите проверку."})
                    else:
                        self.send_json(200, {"valid": not errors, "validationErrors": errors,
                            "validatedAt": validated_at, "validationHash": validation_hash, "ready": not errors})
            elif path == "/api/admin/gc":
                query = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
                if query.get("dryRun", [""])[0].lower() == "true":
                    self.send_json(200, collect_unreferenced_blobs(grace_seconds=GC_GRACE_SECONDS, dry_run=True))
                    return
                if not start_gc_job(self.actor, self.remote_address()):
                    self.send_json(409, {"error": "Очистка хранилища уже выполняется"})
                    return
                self.send_json(202, {"status": "running"})
            elif path == "/api/admin/backups":
                if not start_backup_job(self.actor, self.remote_address()):
                    self.send_json(409, {"error": "Другая задача обслуживания уже выполняется"})
                    return
                self.send_json(202, {"status": "running"})
            elif re.fullmatch(r"/api/admin/backups/\d{8}-\d{6}(?:-\d{9})?/verify", path):
                from backup import verify_backup

                backup_id = path.split("/")[-2]
                result = verify_backup(BACKUP_DIR / backup_id, BACKUP_DIR)
                audit(self.actor, "backup_verify", json.dumps({"backupId": backup_id, **result}, ensure_ascii=False), self.remote_address())
                self.send_json(200, result)
            elif re.fullmatch(r"/api/admin/backups/\d{8}-\d{6}(?:-\d{9})?/restore", path):
                backup_id = path.split("/")[-2]
                if not maintenance_execution_lock.acquire(blocking=False):
                    self.send_json(409, {"error": "Восстановление недоступно: выполняется другая задача или уже ожидает перезапуск"})
                    return
                try:
                    from backup import verify_backup

                    if (ROOT / "pending-restore.json").exists():
                        self.send_json(409, {"error": "Восстановление уже запланировано"})
                        return
                    verify_backup(BACKUP_DIR / backup_id, BACKUP_DIR)
                    write_json_atomic(ROOT / "pending-restore.json", {"backupId": backup_id,
                        "requestedAt": int(time.time()), "requestedBy": self.actor})
                    write_json_atomic(ROOT / "restore-status.json", {"status": "scheduled", "backupId": backup_id,
                        "requestedAt": int(time.time())})
                    audit(self.actor, "backup_restore_scheduled", backup_id, self.remote_address())
                    self.send_json(202, {"status": "scheduled", "backupId": backup_id, "serverWillStop": True})
                    shutdown = threading.Timer(1.5, self.server.shutdown)
                    shutdown.daemon = True
                    shutdown.start()
                finally:
                    maintenance_execution_lock.release()
            elif path == "/api/admin/system/diagnose":
                result = diagnose_system()
                audit(self.actor, "system_diagnose", json.dumps(result, ensure_ascii=False), self.remote_address())
                self.send_json(200, result)
            elif path == "/api/admin/system/sessions/revoke":
                with db() as connection:
                    revoked = connection.execute("DELETE FROM admin_sessions").rowcount
                audit(self.actor, "sessions_revoke", f"Завершено сессий: {revoked}", self.remote_address())
                self.send_json(200, {"revoked": revoked})
            elif path == "/api/admin/system/password":
                self.change_admin_password()
            else:
                self.send_json(404, {"error": "Маршрут не найден"})
        except (ValueError, json.JSONDecodeError) as error:
            self.send_json(400, {"error": str(error)})
        except sqlite3.IntegrityError as error:
            self.send_json(409, {"error": f"Конфликт данных: {error}"})

    def change_admin_password(self):
        global CONFIG, PASSWORD_HASH
        if os.environ.get("REPO_ADMIN_PASSWORD_HASH"):
            self.send_json(409, {"error": "Пароль задан через REPO_ADMIN_PASSWORD_HASH и должен меняться в настройках окружения"})
            return
        try:
            data = self.read_json(4096)
        except (ValueError, json.JSONDecodeError) as error:
            self.send_json(400, {"error": str(error)})
            return
        current_password = str(data.get("currentPassword", ""))
        new_password = str(data.get("newPassword", ""))
        confirmation = str(data.get("confirmPassword", ""))
        if not password_matches(current_password):
            self.send_json(403, {"error": "Текущий пароль неверен"})
            return
        if not valid_password(new_password, confirmation):
            self.send_json(400, {"error": "Новый пароль должен совпадать с подтверждением и содержать не менее 8 символов"})
            return
        encoded = password_hash(new_password)
        updated_configuration = {**CONFIG, "adminPasswordHash": encoded}
        try:
            write_json_atomic(CONFIG_PATH, updated_configuration)
        except OSError as error:
            record_system_error("password_change", error)
            self.send_json(500, {"error": "Не удалось сохранить новый пароль"})
            return
        CONFIG = updated_configuration
        PASSWORD_HASH = encoded
        with db() as connection:
            revoked = connection.execute("DELETE FROM admin_sessions").rowcount
        audit(self.actor, "password_change", f"Сменён пароль; завершено сессий: {revoked}", self.remote_address())
        self.send_json(200, {"changed": True, "revokedSessions": revoked})

    def upload_file(self):
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            self.send_json(400, {"error": "Некорректный Content-Length"})
            return
        if length <= 0 or length > MAX_UPLOAD:
            self.send_json(413, {"error": f"Файл превышает лимит {MAX_UPLOAD} байт"})
            return
        reserved, free_bytes, reserve_bytes = reserve_upload_space(length)
        if not reserved:
            self.close_connection = True
            self.send_json(507, {"error": "Недостаточно свободного места для загрузки файла",
                "freeBytes": free_bytes, "requiredBytes": length + reserve_bytes})
            return
        file_name = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query).get("name", ["upload.bin"])[0]
        file_name = Path(file_name.replace("\\", "/")).name[:180] or "upload.bin"
        if not allowed_repository_file(file_name):
            release_upload_space(length)
            self.send_json(415, {"error": "Тип файла не разрешён. Поддерживаются файлы игровых модов, ресурсов и конфигураций."})
            return
        temporary = ROOT / f"upload-{secrets.token_hex(16)}.tmp"
        digest = hashlib.sha256()
        remaining = length
        try:
            with temporary.open("xb") as output:
                while remaining:
                    chunk = self.rfile.read(min(1024 * 1024, remaining))
                    if not chunk:
                        raise ValueError("Загрузка оборвалась")
                    digest.update(chunk)
                    output.write(chunk)
                    remaining -= len(chunk)
            hex_digest = digest.hexdigest()
            target = OBJECTS / hex_digest[:2] / hex_digest
            target.parent.mkdir(parents=True, exist_ok=True)
            duplicate = False
            if target.is_file():
                try:
                    duplicate = sha256_file(target) == (hex_digest, length)
                except OSError:
                    duplicate = False
            if duplicate:
                temporary.unlink()
            else:
                temporary.replace(target)
            with db() as connection:
                connection.execute("INSERT INTO blobs(hash,size,created_at) VALUES(?,?,?) ON CONFLICT(hash) DO UPDATE SET size=excluded.size", (hex_digest, length, int(time.time())))
            audit(self.actor, "file_upload", json.dumps({"name": file_name, "hash": hex_digest, "size": length, "duplicate": duplicate}), self.remote_address())
            self.send_json(200, {"name": file_name, "hash": f"sha256:{hex_digest}", "size": length, "url": f"/api/files/sha256:{hex_digest}", "duplicate": duplicate})
        finally:
            try:
                temporary.unlink(missing_ok=True)
            finally:
                release_upload_space(length)

    def create_pack(self):
        data = self.read_json()
        pack_id = clean_pack_id(data.get("id"))
        title = str(data.get("name", "")).strip()
        if not title or len(title) > 120:
            raise ValueError("Название сборки: 1-120 символов")
        authors = data.get("authors", [])
        categories = data.get("categories", [])
        icon_url = data.get("iconUrl")
        if not isinstance(authors, list) or not isinstance(categories, list) or any(not isinstance(item, str) for item in authors + categories):
            raise ValueError("Авторы и категории должны быть списками строк")
        if icon_url is not None and not re.fullmatch(r"/api/files/sha256:[a-f0-9]{64}", icon_url):
            raise ValueError("Иконка должна быть загружена в репозиторий")
        now = int(time.time())
        with db() as connection:
            if icon_url and not connection.execute("SELECT 1 FROM blobs WHERE hash=?", (icon_url.split(":", 1)[1],)).fetchone():
                raise ValueError("Файл иконки сначала нужно загрузить")
            existing = connection.execute(
                "SELECT title,description,authors,categories,icon_url FROM packs WHERE id=?", (pack_id,)
            ).fetchone()
            if existing:
                requested = (title, str(data.get("description", ""))[:4000], json.dumps(authors), json.dumps(categories), icon_url)
                current = tuple(existing[key] for key in ("title", "description", "authors", "categories", "icon_url"))
                if current == requested:
                    self.send_json(200, {"id": pack_id, "existing": True})
                else:
                    self.send_json(409, {"error": "Сборка с таким идентификатором уже существует с другими данными"})
                return
            connection.execute("INSERT INTO packs(id,title,description,authors,categories,icon_url,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?)",
                               (pack_id, title, str(data.get("description", ""))[:4000], json.dumps(authors), json.dumps(categories), icon_url, now, now))
        audit(self.actor, "pack_create", pack_id, self.remote_address())
        self.send_json(201, {"id": pack_id})

    def create_release(self):
        data = self.read_json()
        pack_id = clean_pack_id(data.get("packId"))
        version = str(data.get("version", "")).strip()
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]{0,63}", version) or semver_sort_key(version)[0] != 1:
            raise ValueError("Версия должна соответствовать SemVer: MAJOR.MINOR.PATCH[-PRERELEASE][+BUILD]")
        if data.get("channel", "stable") not in ("stable", "beta"):
            raise ValueError("Канал версии должен быть stable или beta")
        with db() as connection:
            pack = connection.execute("SELECT * FROM packs WHERE id=?", (pack_id,)).fetchone()
            if not pack:
                raise ValueError("Сначала создайте сборку")
            manifest = data.get("manifest")
            if not isinstance(manifest, dict):
                manifest = {"manifestVersion": 1, "packId": pack_id, "version": version,
                            "name": pack["title"], "description": pack["description"],
                            "authors": json.loads(pack["authors"]), "categories": json.loads(pack["categories"]),
                            "iconUrl": pack["icon_url"], "minecraft": {"version": "", "loader": "vanilla", "loaderVersion": None},
                            "java": {"major": 21}, "files": [], "settings": {}}
            validation_errors = validate_manifest(manifest, pack_id, version)
            now = int(time.time())
            connection.execute("INSERT INTO releases(pack_id,version,status,channel,manifest,created_at,updated_at) VALUES(?,?,'draft',?,?,?,?)",
                               (pack_id, version, data.get("channel", "stable"), json.dumps(manifest, ensure_ascii=False), now, now))
            release_id = connection.execute("SELECT last_insert_rowid()").fetchone()[0]
        audit(self.actor, "release_create", f"{pack_id}@{version}", self.remote_address())
        self.send_json(201, {"id": release_id, "packId": pack_id, "version": version, "status": "draft", "manifest": manifest, "validationErrors": validation_errors})

    def clone_release(self, release_id):
        data = self.read_json(4096)
        version = str(data.get("version", "")).strip()
        channel = data.get("channel")
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]{0,63}", version) or semver_sort_key(version)[0] != 1:
            raise ValueError("Версия должна соответствовать SemVer: MAJOR.MINOR.PATCH[-PRERELEASE][+BUILD]")
        if channel not in ("stable", "beta"):
            raise ValueError("Канал версии должен быть stable или beta")
        now = int(time.time())
        with db() as connection:
            source = connection.execute("SELECT * FROM releases WHERE id=?", (release_id,)).fetchone()
            if not source:
                self.send_json(404, {"error": "Релиз не найден"})
                return
            manifest = json.loads(source["manifest"])
            if not isinstance(manifest, dict):
                raise ValueError("Манифест исходной версии некорректен")
            manifest["version"] = version
            connection.execute(
                "INSERT INTO releases(pack_id,version,status,channel,manifest,created_at,updated_at) VALUES(?,?,'draft',?,?,?,?)",
                (source["pack_id"], version, channel, json.dumps(manifest, ensure_ascii=False), now, now),
            )
            cloned_id = connection.execute("SELECT last_insert_rowid()").fetchone()[0]
        audit(self.actor, "release_clone", f"{source['pack_id']}@{source['version']} -> {version} ({channel})", self.remote_address())
        self.send_json(201, {"id": cloned_id, "packId": source["pack_id"], "version": version,
            "status": "draft", "channel": channel, "immutable": False, "manifest": manifest})

    def archive_release(self, release_id, archive):
        now = int(time.time())
        with db() as connection:
            release = connection.execute("SELECT * FROM releases WHERE id=?", (release_id,)).fetchone()
            if not release:
                self.send_json(404, {"error": "Релиз не найден"})
                return
            if archive and release["status"] == "archived":
                self.send_json(409, {"error": "Версия уже архивирована"})
                return
            if not archive and release["status"] != "archived":
                self.send_json(409, {"error": "Восстановить можно только архивированную версию"})
                return
            status = "archived" if archive else "draft"
            connection.execute("UPDATE releases SET status=?,updated_at=?,published_at=CASE WHEN ? THEN published_at ELSE NULL END WHERE id=? AND status=?",
                (status, now, int(archive), release_id, release["status"]))
        action = "release_archive" if archive else "release_restore"
        audit(self.actor, action, f"{release['pack_id']}@{release['version']}", self.remote_address())
        self.send_json(200, {"id": release_id, "status": status})

    def publish_release(self, release_id, publish):
        if not release_id.isdigit():
            raise ValueError("Некорректный id релиза")
        with db() as connection:
            release = connection.execute("SELECT * FROM releases WHERE id=?", (int(release_id),)).fetchone()
            if not release:
                self.send_json(404, {"error": "Релиз не найден"})
                return
            if publish and release["status"] != "draft":
                self.send_json(409, {"error": "Публиковать можно только draft"})
                return
            if not publish and release["status"] != "published":
                self.send_json(409, {"error": "Снять с публикации можно только опубликованную версию"})
                return
            if publish:
                readiness = connection.execute("SELECT * FROM release_readiness WHERE release_id=?", (int(release_id),)).fetchone()
                current_hash = manifest_hash(release["manifest"])
                if not readiness or readiness["validation_hash"] != current_hash:
                    self.send_json(409, {"error": "Актуальная проверка готовности отсутствует. Запустите проверку версии."})
                    return
                if int(time.time()) - readiness["validated_at"] > RELEASE_READINESS_SECONDS:
                    self.send_json(409, {"error": "Проверка готовности устарела. Запустите её повторно перед публикацией."})
                    return
                if not readiness["ready"]:
                    validation = json.loads(readiness["validation_json"])
                    validation_errors = validation.get("errors", []) if isinstance(validation, dict) else validation
                    self.send_json(422, {"error": "Версия не готова к публикации", "validationErrors": validation_errors})
                    return
                validation = json.loads(readiness["validation_json"])
                if not isinstance(validation, dict) or not isinstance(validation.get("artifacts"), dict):
                    self.send_json(409, {"error": "Проверка готовности устарела. Запустите её повторно перед публикацией."})
                    return
            manifest = json.loads(release["manifest"])
            errors = validate_manifest(manifest, release["pack_id"], release["version"])
            if publish and not errors:
                errors.extend(verify_release_artifact_signatures(connection, manifest, validation["artifacts"]))
            if publish and errors:
                self.send_json(422, {"error": "Публикация не прошла проверку", "validationErrors": errors})
                return
            status = "published" if publish else "draft"
            now = int(time.time())
            updated = connection.execute(
                "UPDATE releases SET status=?,updated_at=?,published_at=CASE WHEN ? THEN COALESCE(published_at, ?) ELSE NULL END WHERE id=? AND manifest=? AND status=?",
                (status, now, int(publish), now, int(release_id), release["manifest"], release["status"]),
            ).rowcount
            if not updated:
                self.send_json(409, {"error": "Релиз изменился во время проверки. Повторите публикацию."})
                return
        audit(self.actor, "release_publish" if publish else "release_unpublish", f"{release['pack_id']}@{release['version']}", self.remote_address())
        self.send_json(200, {"id": int(release_id), "status": status})

    def do_PUT(self):
        path = urllib.parse.urlsplit(self.path).path
        if not path.startswith("/api/admin/") or not self.require_admin():
            if not path.startswith("/api/admin/"):
                self.send_json(404, {"error": "Маршрут не найден"})
            return
        match = re.fullmatch(r"/api/admin/releases/(\d+)", path)
        pack_match = re.fullmatch(r"/api/admin/packs/([a-z0-9-]+)", path)
        if pack_match:
            try:
                data = self.read_json()
                title = str(data.get("name", "")).strip()
                description = str(data.get("description", ""))
                authors = data.get("authors", [])
                categories = data.get("categories", [])
                icon_url = data.get("iconUrl")
                if not title or len(title) > 120 or len(description) > 4000:
                    raise ValueError("Название или описание имеют недопустимую длину")
                if not isinstance(authors, list) or not isinstance(categories, list) or any(not isinstance(item, str) for item in authors + categories):
                    raise ValueError("Авторы и категории должны быть списками строк")
                if icon_url is not None and not re.fullmatch(r"/api/files/sha256:[a-f0-9]{64}", icon_url):
                    raise ValueError("Иконка должна быть загружена в репозиторий")
                with db() as connection:
                    if icon_url and not connection.execute("SELECT 1 FROM blobs WHERE hash=?", (icon_url.split(":", 1)[1],)).fetchone():
                        raise ValueError("Файл иконки сначала нужно загрузить")
                    now = int(time.time())
                    updated = connection.execute("UPDATE packs SET title=?,description=?,authors=?,categories=?,icon_url=?,updated_at=? WHERE id=?",
                        (title, description, json.dumps(authors), json.dumps(categories), icon_url, now, pack_match.group(1))).rowcount
                    drafts = connection.execute("SELECT id,manifest FROM releases WHERE pack_id=? AND status='draft' AND published_at IS NULL", (pack_match.group(1),)).fetchall()
                    for draft in drafts:
                        manifest = json.loads(draft["manifest"])
                        if not isinstance(manifest, dict):
                            continue
                        manifest.update({"name": title, "description": description, "authors": authors, "categories": categories, "iconUrl": icon_url})
                        connection.execute("UPDATE releases SET manifest=?,updated_at=? WHERE id=? AND published_at IS NULL",
                            (json.dumps(manifest, ensure_ascii=False), now, draft["id"]))
                        connection.execute("DELETE FROM release_readiness WHERE release_id=?", (draft["id"],))
                if not updated:
                    self.send_json(404, {"error": "Сборка не найдена"})
                    return
                audit(self.actor, "pack_update", pack_match.group(1), self.remote_address())
                self.send_json(200, {"id": pack_match.group(1), "updated": True})
            except (ValueError, json.JSONDecodeError) as error:
                self.send_json(400, {"error": str(error)})
            return
        if not match:
            self.send_json(404, {"error": "Маршрут не найден"})
            return
        try:
            manifest = self.read_json()
            with db() as connection:
                release = connection.execute("SELECT * FROM releases WHERE id=?", (int(match.group(1)),)).fetchone()
                if not release:
                    self.send_json(404, {"error": "Релиз не найден"})
                    return
                if release["published_at"] is not None:
                    self.send_json(409, {"error": "Опубликованную версию нельзя редактировать. Создайте новую версию."})
                    return
                if release["status"] != "draft":
                    self.send_json(409, {"error": "Редактировать можно только черновик"})
                    return
                if_match = self.headers.get("If-Match")
                if if_match and not if_none_match_matches(if_match, release_etag(release["manifest"], release["channel"])):
                    self.send_json(409, {"error": "Манифест изменился. Обновите версию перед сохранением."})
                    return
                validation_errors = validate_draft_manifest(manifest, release["pack_id"], release["version"])
                if validation_errors:
                    self.send_json(422, {"error": "Черновик содержит структурные ошибки", "validationErrors": validation_errors})
                    return
                serialized = json.dumps(manifest, ensure_ascii=False)
                updated = connection.execute("UPDATE releases SET manifest=?,updated_at=? WHERE id=? AND manifest=? AND status='draft' AND published_at IS NULL",
                    (serialized, int(time.time()), int(match.group(1)), release["manifest"])).rowcount
                if not updated:
                    self.send_json(409, {"error": "Версия изменилась во время сохранения. Обновите её и повторите."})
                    return
                connection.execute("DELETE FROM release_readiness WHERE release_id=?", (int(match.group(1)),))
            audit(self.actor, "release_update", f"{release['pack_id']}@{release['version']}", self.remote_address())
            etag = release_etag(serialized, release["channel"])
            self.send_json(200, {"id": int(match.group(1)), "status": "draft", "validationErrors": validate_manifest(manifest, release["pack_id"], release["version"]), "etag": etag}, etag=etag)
        except (ValueError, json.JSONDecodeError) as error:
            self.send_json(400, {"error": str(error)})

    def do_PATCH(self):
        path = urllib.parse.urlsplit(self.path).path
        if not path.startswith("/api/admin/") or not self.require_admin():
            if not path.startswith("/api/admin/"):
                self.send_json(404, {"error": "Маршрут не найден"})
            return
        match = re.fullmatch(r"/api/admin/releases/(\d+)", path)
        if not match:
            self.send_json(404, {"error": "Маршрут не найден"})
            return
        try:
            changes = self.read_json()
            if not isinstance(changes, dict):
                raise ValueError("PATCH должен содержать JSON-объект")
            release_id = int(match.group(1))
            if "channel" in changes:
                if set(changes) != {"channel"}:
                    raise ValueError("Канал версии нужно изменять отдельным запросом")
                channel = changes["channel"]
                if channel not in ("stable", "beta"):
                    raise ValueError("Канал версии должен быть stable или beta")
                with db() as connection:
                    release = connection.execute("SELECT * FROM releases WHERE id=?", (release_id,)).fetchone()
                    if not release:
                        self.send_json(404, {"error": "Релиз не найден"})
                        return
                    if release["published_at"] is not None or release["status"] != "draft":
                        self.send_json(409, {"error": "Изменять можно только draft, который никогда не публиковался"})
                        return
                    if_match = self.headers.get("If-Match")
                    if not if_match:
                        self.send_json(428, {"error": "Для изменения канала требуется If-Match с текущим ETag"})
                        return
                    current_etag = release_etag(release["manifest"], release["channel"])
                    if not if_none_match_matches(if_match, current_etag):
                        self.send_json(409, {"error": "Версия изменилась. Обновите её перед сохранением."})
                        return
                    now = int(time.time())
                    updated = connection.execute(
                        "UPDATE releases SET channel=?,updated_at=? WHERE id=? AND manifest=? AND channel=? AND status='draft' AND published_at IS NULL",
                        (channel, now, release_id, release["manifest"], release["channel"]),
                    ).rowcount
                    if not updated:
                        self.send_json(409, {"error": "Версия изменилась во время сохранения. Обновите её и повторите."})
                        return
                    etag = release_etag(release["manifest"], channel)
                audit(self.actor, "release_update", f"{release['pack_id']}@{release['version']} channel={channel}", self.remote_address())
                self.send_json(200, {"id": release_id, "status": "draft", "channel": channel, "etag": etag}, etag=etag)
                return
            with db() as connection:
                release = connection.execute("SELECT * FROM releases WHERE id=?", (release_id,)).fetchone()
                if not release:
                    self.send_json(404, {"error": "Релиз не найден"})
                    return
                if release["published_at"] is not None or release["status"] != "draft":
                    self.send_json(409, {"error": "Изменять можно только draft, который никогда не публиковался"})
                    return
                if_match = self.headers.get("If-Match")
                if if_match and not if_none_match_matches(if_match, release_etag(release["manifest"], release["channel"])):
                    self.send_json(409, {"error": "Манифест изменился. Обновите версию перед сохранением."})
                    return
                manifest = json.loads(release["manifest"])
                for key, value in changes.items():
                    if value is None:
                        manifest.pop(key, None)
                    else:
                        manifest[key] = value
                validation_errors = validate_draft_manifest(manifest, release["pack_id"], release["version"])
                if validation_errors:
                    self.send_json(422, {"error": "Черновик содержит структурные ошибки", "validationErrors": validation_errors})
                    return
                serialized = json.dumps(manifest, ensure_ascii=False)
                updated = connection.execute("UPDATE releases SET manifest=?,updated_at=? WHERE id=? AND manifest=? AND status='draft' AND published_at IS NULL",
                    (serialized, int(time.time()), release_id, release["manifest"])).rowcount
                if not updated:
                    self.send_json(409, {"error": "Версия изменилась во время сохранения. Обновите её и повторите."})
                    return
                connection.execute("DELETE FROM release_readiness WHERE release_id=?", (release_id,))
            audit(self.actor, "release_update", f"{release['pack_id']}@{release['version']}", self.remote_address())
            etag = release_etag(serialized, release["channel"])
            self.send_json(200, {"id": release_id, "status": "draft", "etag": etag,
                "validationErrors": validate_manifest(manifest, release["pack_id"], release["version"])}, etag=etag)
        except (ValueError, json.JSONDecodeError) as error:
            self.send_json(400, {"error": str(error)})

    def do_DELETE(self):
        path = urllib.parse.urlsplit(self.path).path
        if not path.startswith("/api/admin/") or not self.require_admin():
            if not path.startswith("/api/admin/"):
                self.send_json(404, {"error": "Маршрут не найден"})
            return
        release_match = re.fullmatch(r"/api/admin/releases/(\d+)", path)
        pack_match = re.fullmatch(r"/api/admin/packs/([a-z0-9-]+)", path)
        with db() as connection:
            if release_match:
                release = connection.execute("SELECT * FROM releases WHERE id=?", (int(release_match.group(1)),)).fetchone()
                if not release:
                    self.send_json(404, {"error": "Релиз не найден"})
                    return
                if release["published_at"] is not None:
                    self.send_json(409, {"error": "Опубликованную версию нельзя удалить"})
                    return
                connection.execute("DELETE FROM releases WHERE id=?", (release["id"],))
                detail = json.dumps({"release": f"{release['pack_id']}@{release['version']}", "deletedReleases": 1, "deletedBlobs": 0}, ensure_ascii=False)
            elif pack_match:
                pack = connection.execute("SELECT * FROM packs WHERE id=?", (pack_match.group(1),)).fetchone()
                if not pack:
                    self.send_json(404, {"error": "Сборка не найдена"})
                    return
                releases = connection.execute("SELECT COUNT(*) AS count, SUM(CASE WHEN published_at IS NOT NULL THEN 1 ELSE 0 END) AS published FROM releases WHERE pack_id=?", (pack["id"],)).fetchone()
                if releases["published"]:
                    self.send_json(409, {"error": "Сборка содержит опубликованные версии и не может быть удалена"})
                    return
                if releases["count"]:
                    self.send_json(409, {"error": "Сначала удалите черновики сборки"})
                    return
                connection.execute("DELETE FROM packs WHERE id=?", (pack["id"],))
                detail = json.dumps({"packId": pack["id"], "deletedReleases": 0, "deletedBlobs": 0}, ensure_ascii=False)
            else:
                self.send_json(404, {"error": "Маршрут не найден"})
                return
        audit(self.actor, "delete", detail, self.remote_address())
        self.send_json(200, {"deleted": True})


def main():
    if not PASSWORD_HASH:
        raise SystemExit("Установите REPO_ADMIN_PASSWORD_HASH в переменных окружения перед запуском")
    data_lock = DataDirectoryLock(ROOT)
    try:
        data_lock.acquire()
    except RuntimeError as error:
        raise SystemExit(str(error)) from error
    server = None
    scheduler_stop = threading.Event()
    analytics_stop = threading.Event()
    scheduler = None
    analytics = None
    try:
        restore_state = apply_pending_restore()
        initialize()
        if restore_state and restore_state.get("status") == "completed":
            audit("system", "backup_restore", json.dumps(restore_state, ensure_ascii=False), "localhost")
        cleanup_expired_sessions()
        removed_uploads = cleanup_stale_uploads()
        if removed_uploads:
            print(f"Удалено устаревших временных загрузок: {removed_uploads}", flush=True)
        server = LimitedThreadingHTTPServer((HOST, PORT), Handler)
        scheduler = threading.Thread(target=scheduled_gc, args=(scheduler_stop,), daemon=True)
        analytics = threading.Thread(target=download_analytics_worker, args=(analytics_stop,), daemon=True)
        scheduler.start()
        analytics.start()
        print(f"MyLauncher repository listening on http://{HOST}:{PORT}; data={ROOT}", flush=True)
        start_gc_job("startup", "localhost")
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass
    finally:
        scheduler_stop.set()
        if scheduler is not None and scheduler.ident is not None:
            scheduler.join()
        if server is not None:
            server.server_close()
        maintenance_execution_lock.acquire()
        maintenance_execution_lock.release()
        analytics_stop.set()
        if analytics is not None:
            analytics.join()
        data_lock.release()


if __name__ == "__main__":
    main()