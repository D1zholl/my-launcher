import hashlib
import json
import os
import re
import shutil
import sqlite3
import tempfile
import time
from pathlib import Path
from posixpath import normpath

from server import DATABASE, OBJECTS, ROOT


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def backup_directory_path(backup_directory=None):
    return Path(backup_directory or os.environ.get("REPO_BACKUP_DIR", Path(__file__).parent / "backups")).resolve()


def list_backups(backup_directory=None):
    directory = backup_directory_path(backup_directory)
    if not directory.is_dir():
        return []
    snapshots = []
    for path in directory.iterdir():
        if not path.is_dir() or not re.fullmatch(r"\d{8}-\d{6}(?:-\d{9})?", path.name):
            continue
        manifest_path = path / "manifest.json"
        try:
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            files = manifest.get("files", [])
            byte_count = sum(int(item.get("size", 0)) for item in files if isinstance(item, dict))
            snapshots.append({"id": path.name, "createdAt": manifest.get("createdAt", path.name),
                "files": len(files), "bytes": byte_count})
        except (OSError, ValueError, TypeError, json.JSONDecodeError):
            snapshots.append({"id": path.name, "createdAt": path.name, "files": 0, "bytes": 0, "valid": False})
    return sorted(snapshots, key=lambda item: item["id"], reverse=True)


def verify_backup(snapshot, backup_directory=None):
    directory = backup_directory_path(backup_directory)
    snapshot = Path(snapshot).resolve(strict=True)
    if snapshot.parent != directory or not re.fullmatch(r"\d{8}-\d{6}(?:-\d{9})?", snapshot.name):
        raise ValueError("Недопустимый идентификатор резервной копии")
    manifest_path = snapshot / "manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if not isinstance(manifest, dict) or not isinstance(manifest.get("files"), list):
        raise ValueError("Некорректный manifest резервной копии")
    checked = 0
    total_bytes = 0
    seen = set()
    for item in manifest["files"]:
        if not isinstance(item, dict) or not isinstance(item.get("path"), str):
            raise ValueError("Некорректная запись файла резервной копии")
        relative = item["path"]
        if normpath(relative) != relative or relative.startswith(("/", "\\")) or "\\" in relative or ".." in relative.split("/"):
            raise ValueError("Резервная копия содержит небезопасный путь")
        if relative in seen:
            raise ValueError("Резервная копия содержит повторяющийся путь")
        seen.add(relative)
        candidate = snapshot / Path(*relative.split("/"))
        if candidate.is_symlink():
            raise ValueError("Резервная копия содержит символическую ссылку")
        path = candidate.resolve(strict=True)
        if not path.is_relative_to(snapshot) or not path.is_file():
            raise ValueError("Файл резервной копии отсутствует или выходит за каталог snapshot")
        size = int(item.get("size", -1))
        expected_hash = item.get("sha256")
        if size < 0 or not re.fullmatch(r"[a-f0-9]{64}", str(expected_hash)):
            raise ValueError("Некорректный размер или SHA-256 в manifest резервной копии")
        if path.stat().st_size != size or sha256_file(path) != expected_hash:
            raise ValueError(f"Проверка целостности не пройдена: {relative}")
        checked += 1
        total_bytes += size
    actual_files = set()
    for path in snapshot.rglob("*"):
        if path.is_symlink():
            raise ValueError("Резервная копия содержит символическую ссылку")
        if path.is_file() and path.name != "manifest.json":
            actual_files.add(path.relative_to(snapshot).as_posix())
    if actual_files != seen:
        raise ValueError("Manifest резервной копии не соответствует фактическому составу файлов")
    database = snapshot / "repository.sqlite3"
    objects = snapshot / "objects" / "sha256"
    if "repository.sqlite3" not in seen or not database.is_file() or not objects.is_dir():
        raise ValueError("В резервной копии отсутствуют база данных или blob-хранилище")
    connection = sqlite3.connect(database)
    try:
        integrity = connection.execute("PRAGMA integrity_check").fetchone()[0]
    finally:
        connection.close()
    if integrity != "ok":
        raise sqlite3.DatabaseError(f"Проверка SQLite не пройдена: {integrity}")
    return {"valid": True, "createdAt": manifest.get("createdAt", snapshot.name), "checkedFiles": checked,
        "bytes": total_bytes, "sqliteIntegrity": integrity}


def restore_backup(snapshot, database=DATABASE, objects=OBJECTS, data_root=ROOT, backup_directory=None):
    snapshot = Path(snapshot).resolve(strict=True)
    verification = verify_backup(snapshot, backup_directory)
    database = Path(database)
    objects = Path(objects)
    data_root = Path(data_root)
    data_root.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=".restore-staging-", dir=data_root))
    staged_database = staging / "repository.sqlite3"
    staged_objects = staging / "objects" / "sha256"
    source_database = snapshot / "repository.sqlite3"
    source_objects = snapshot / "objects" / "sha256"
    safety_backup = None
    installed_database = False
    installed_objects = False
    moved_database = False
    moved_objects = False
    moved_secret = False
    secret = data_root / "download-visitor-secret"
    staged_secret = staging / "download-visitor-secret"
    previous_database = staging / "previous-repository.sqlite3"
    previous_objects = staging / "previous-objects"
    previous_secret = staging / "previous-download-visitor-secret"
    sidecars = (Path(f"{database}-wal"), Path(f"{database}-shm"))
    previous_sidecars = tuple(staging / path.name for path in sidecars)
    moved_sidecars = []
    try:
        shutil.copy2(source_database, staged_database)
        shutil.copytree(source_objects, staged_objects)
        if (snapshot / "download-visitor-secret").is_file():
            shutil.copy2(snapshot / "download-visitor-secret", staged_secret)
        if database.is_file() and objects.is_dir():
            safety_backup = create_backup(database, objects, data_root, backup_directory)
        else:
            raise FileNotFoundError("Текущая база и хранилище должны существовать для безопасного восстановления")
        if database.exists():
            os.replace(database, previous_database)
            moved_database = True
        for sidecar, previous_sidecar in zip(sidecars, previous_sidecars):
            if sidecar.exists():
                os.replace(sidecar, previous_sidecar)
                moved_sidecars.append((sidecar, previous_sidecar))
        if objects.exists():
            os.replace(objects, previous_objects)
            moved_objects = True
        os.replace(staged_database, database)
        installed_database = True
        os.replace(staged_objects, objects)
        installed_objects = True
        if secret.exists():
            os.replace(secret, previous_secret)
            moved_secret = True
        if staged_secret.exists():
            os.replace(staged_secret, secret)
        return {"restoredAt": int(time.time()), "safetyBackup": str(safety_backup), **verification}
    except BaseException:
        if installed_database:
            database.unlink(missing_ok=True)
        if installed_objects:
            shutil.rmtree(objects, ignore_errors=True)
        if moved_objects and previous_objects.exists():
            os.replace(previous_objects, objects)
        if moved_database and previous_database.exists():
            os.replace(previous_database, database)
        for sidecar, previous_sidecar in reversed(moved_sidecars):
            if previous_sidecar.exists():
                os.replace(previous_sidecar, sidecar)
        if moved_secret:
            secret.unlink(missing_ok=True)
            os.replace(previous_secret, secret)
        raise
    finally:
        shutil.rmtree(staging, ignore_errors=True)


def create_backup(database=DATABASE, objects=OBJECTS, data_root=ROOT, backup_directory=None, retention=None):
    backup_directory = backup_directory_path(backup_directory)
    if retention is None:
        retention = int(os.environ.get("REPO_BACKUP_RETENTION", "14"))
    retention = max(1, int(retention))
    database = Path(database)
    objects = Path(objects)
    data_root = Path(data_root)
    if not database.is_file() or not objects.is_dir():
        raise FileNotFoundError("База данных и каталог объектов должны существовать перед backup")

    backup_directory.mkdir(parents=True, exist_ok=True)
    timestamp = time.strftime("%Y%m%d-%H%M%S") + f"-{time.time_ns() % 1_000_000_000:09d}"
    snapshot = backup_directory / timestamp
    staging = Path(tempfile.mkdtemp(prefix=f".{timestamp}.tmp-", dir=backup_directory))
    published = False
    try:
        destination_database = staging / "repository.sqlite3"
        source = sqlite3.connect(database)
        destination = sqlite3.connect(destination_database)
        try:
            source.backup(destination)
            integrity = destination.execute("PRAGMA integrity_check").fetchone()[0]
            if integrity != "ok":
                raise sqlite3.DatabaseError(f"SQLite integrity_check failed: {integrity}")
        finally:
            destination.close()
            source.close()

        shutil.copytree(objects, staging / "objects" / "sha256")
        visitor_secret = data_root / "download-visitor-secret"
        if visitor_secret.is_file():
            shutil.copy2(visitor_secret, staging / visitor_secret.name)

        files = []
        for path in sorted(staging.rglob("*")):
            if path.is_file():
                files.append({
                    "path": path.relative_to(staging).as_posix(),
                    "size": path.stat().st_size,
                    "sha256": sha256_file(path),
                })
        manifest = {"createdAt": timestamp, "files": files}
        (staging / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        staging.rename(snapshot)
        published = True

        backups = sorted(
            (path for path in backup_directory.iterdir() if path.is_dir() and re.fullmatch(r"\d{8}-\d{6}(?:-\d{9})?", path.name)),
            key=lambda path: path.name,
            reverse=True,
        )
        for old_backup in backups[retention:]:
            try:
                shutil.rmtree(old_backup)
            except OSError as error:
                print(f"Не удалось удалить старую копию {old_backup.name}: {error}", flush=True)
        return snapshot
    except BaseException:
        if not published:
            shutil.rmtree(staging, ignore_errors=True)
        raise


def main():
    snapshot = create_backup()
    print(f"Backup created: {snapshot}")


if __name__ == "__main__":
    main()