import unittest
from contextlib import contextmanager
from http.client import HTTPConnection
from http.server import ThreadingHTTPServer
import io
import hashlib
import json
import os
import re
import sqlite3
import subprocess
import sys
import tempfile
from threading import Barrier, Event, Thread
import time
import urllib.error
from urllib.request import Request, urlopen
from pathlib import Path
from unittest.mock import patch

import server
from backup import create_backup, list_backups, restore_backup, verify_backup
from make_password import password_hash, valid_password
from server import (
    Handler,
    cached_metadata,
    collect_unreferenced_blobs,
    load_configuration,
    loader_options,
    fabric_metadata,
    metadata_cache,
    password_matches,
    parse_byte_range,
    semver_sort_key,
    safe_relative_path,
    validate_manifest,
    validate_draft_manifest,
    validate_official_metadata,
)


class AdminPanelAssetTests(unittest.TestCase):
    def test_upload_stylesheet_is_served(self):
        web_root = Path(__file__).resolve().parent.parent / "MC-launcher-adminpanel"
        with patch("server.WEB_ROOT", web_root):
            http_server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
            thread = Thread(target=http_server.serve_forever, daemon=True)
            thread.start()
            try:
                for asset_path, selector in (("/upload.css", b".upload-destination"), ("/pack.css", b".pack-icon-preview"), ("/browser.css", b".release-workspace"), ("/theme.css", b"@font-face"), ("/icons.svg", b"icon-moon"), ("/Manrope-VF.ttf", b"\x00\x01\x00\x00"), ("/DMMono-Regular.ttf", b"\x00\x01\x00\x00")):
                    with urlopen(f"http://127.0.0.1:{http_server.server_port}{asset_path}") as response:
                        self.assertEqual(response.status, 200)
                        self.assertIn(selector, response.read()[:8] if asset_path.endswith(".ttf") else response.read())
                with urlopen(f"http://127.0.0.1:{http_server.server_port}/") as response:
                    index = response.read().decode("utf-8")
                    index_etag = response.headers["ETag"]
                    self.assertIn("Last-Modified", response.headers)
                self.assertIn('<option value="config">config</option>', index)
                app_asset = re.search(r'/app\.js\?v=([a-f0-9]{16})', index)
                self.assertIsNotNone(app_asset)
                with urlopen(f"http://127.0.0.1:{http_server.server_port}/app.js?v={app_asset.group(1)}") as response:
                    app_etag = response.headers["ETag"]
                    self.assertEqual(response.headers["Cache-Control"], "public, max-age=31536000, immutable")
                conditional = Request(f"http://127.0.0.1:{http_server.server_port}/", headers={"If-None-Match": index_etag})
                with self.assertRaises(urllib.error.HTTPError) as not_modified:
                    urlopen(conditional)
                self.assertEqual(not_modified.exception.code, 304)
                head = Request(f"http://127.0.0.1:{http_server.server_port}/app.js?v={app_asset.group(1)}", method="HEAD")
                with urlopen(head) as response:
                    self.assertEqual(response.headers["ETag"], app_etag)
            finally:
                http_server.shutdown()
                http_server.server_close()
                thread.join()

    def test_static_asset_hash_is_reused_until_file_signature_changes(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            asset = Path(temporary_directory) / "asset.js"
            asset.write_bytes(b"first version")
            first_hash = server.static_asset_hash(asset, asset.stat())
            with patch.object(Path, "read_bytes", side_effect=AssertionError("cached asset should not be read")):
                self.assertEqual(server.static_asset_hash(asset, asset.stat()), first_hash)
            with asset.open("ab") as output:
                output.write(b" updated")
            self.assertNotEqual(server.static_asset_hash(asset, asset.stat()), first_hash)

    def test_image_blob_endpoint_returns_decodable_content_type(self):
        images = (
            ("a" * 64, b"\x89PNG\r\n\x1a\nPNG data", "image/png"),
            ("b" * 64, b"\xff\xd8\xffJPEG data", "image/jpeg"),
            ("c" * 64, b"RIFF\x04\x00\x00\x00WEBPdata", "image/webp"),
        )
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            objects = root / "objects" / "sha256"
            for digest, image_data, _ in images:
                target = objects / digest[:2] / digest
                target.parent.mkdir(parents=True)
                target.write_bytes(image_data)
            database = root / "repository.sqlite3"
            connection = sqlite3.connect(database)
            try:
                connection.execute("CREATE TABLE blobs (hash TEXT PRIMARY KEY, download_count INTEGER NOT NULL DEFAULT 0)")
                connection.execute("ALTER TABLE blobs ADD COLUMN unique_download_count INTEGER NOT NULL DEFAULT 0")
                connection.execute("CREATE TABLE blob_downloads (hash TEXT NOT NULL, visitor TEXT NOT NULL, PRIMARY KEY(hash, visitor))")
                connection.execute("CREATE TABLE blob_downloads_daily (download_date TEXT NOT NULL, hash TEXT NOT NULL, visitor TEXT NOT NULL, pack_id TEXT NOT NULL DEFAULT '', version TEXT NOT NULL DEFAULT '', channel TEXT NOT NULL DEFAULT '', PRIMARY KEY(download_date, hash, visitor, pack_id, version, channel))")
                connection.executemany("INSERT INTO blobs(hash) VALUES(?)", ((digest,) for digest, _, _ in images))
                connection.commit()
            finally:
                connection.close()

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
                try:
                    yield connection
                    connection.commit()
                finally:
                    connection.close()

            with patch("server.OBJECTS", objects), patch("server.db", open_database):
                http_server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                thread = Thread(target=http_server.serve_forever, daemon=True)
                thread.start()
                try:
                    for digest, _, expected_type in images:
                        with urlopen(f"http://127.0.0.1:{http_server.server_port}/api/files/sha256:{digest}") as response:
                            self.assertEqual(response.headers["Content-Type"], expected_type)
                    self.assertEqual(server.download_event_queue.qsize(), 3)
                    server.flush_download_events()
                    with open_database() as connection:
                        self.assertEqual(connection.execute("SELECT SUM(unique_download_count) FROM blobs").fetchone()[0], 3)
                        self.assertEqual(connection.execute("SELECT COUNT(*) FROM blob_downloads_daily").fetchone()[0], 3)
                finally:
                    http_server.shutdown()
                    http_server.server_close()
                    thread.join()

    def test_blob_download_supports_head_and_ranges(self):
        digest = "d" * 64
        data = b"abcdef"
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            objects = root / "objects" / "sha256"
            target = objects / digest[:2] / digest
            target.parent.mkdir(parents=True)
            target.write_bytes(data)
            database = root / "repository.sqlite3"
            connection = sqlite3.connect(database)
            try:
                connection.execute("CREATE TABLE blobs (hash TEXT PRIMARY KEY, download_count INTEGER NOT NULL DEFAULT 0, unique_download_count INTEGER NOT NULL DEFAULT 0)")
                connection.execute("CREATE TABLE blob_downloads (hash TEXT NOT NULL, visitor TEXT NOT NULL, PRIMARY KEY(hash, visitor))")
                connection.execute("CREATE TABLE blob_downloads_daily (download_date TEXT NOT NULL, hash TEXT NOT NULL, visitor TEXT NOT NULL, pack_id TEXT NOT NULL DEFAULT '', version TEXT NOT NULL DEFAULT '', channel TEXT NOT NULL DEFAULT '', PRIMARY KEY(download_date, hash, visitor, pack_id, version, channel))")
                connection.execute("INSERT INTO blobs(hash) VALUES(?)", (digest,))
                connection.commit()
            finally:
                connection.close()

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
                try:
                    yield connection
                    connection.commit()
                finally:
                    connection.close()

            with patch("server.OBJECTS", objects), patch("server.db", open_database):
                http_server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                thread = Thread(target=http_server.serve_forever, daemon=True)
                thread.start()
                url = f"http://127.0.0.1:{http_server.server_port}/api/files/sha256:{digest}"
                try:
                    with urlopen(Request(url, method="HEAD")) as response:
                        self.assertEqual(response.status, 200)
                        self.assertEqual(response.headers["Content-Length"], str(len(data)))
                        self.assertEqual(response.read(), b"")
                    with urlopen(Request(url, headers={"Range": "bytes=1-3"})) as response:
                        self.assertEqual(response.status, 206)
                        self.assertEqual(response.headers["Content-Range"], f"bytes 1-3/{len(data)}")
                        self.assertEqual(response.read(), b"bcd")
                    with urlopen(url) as response:
                        etag = response.headers["ETag"]
                        self.assertTrue(response.headers["Last-Modified"])
                        self.assertEqual(response.headers["Cache-Control"], "public, max-age=31536000, immutable")
                        self.assertEqual(response.read(), data)
                    with self.assertRaises(urllib.error.HTTPError) as not_modified:
                        urlopen(Request(url, headers={"If-None-Match": etag}))
                    self.assertEqual(not_modified.exception.code, 304)
                    with open_database() as connection:
                        self.assertEqual(connection.execute("SELECT unique_download_count FROM blobs WHERE hash=?", (digest,)).fetchone()[0], 0)
                    self.assertEqual(server.download_event_queue.qsize(), 2)
                    server.flush_download_events()
                    with patch("server.enqueue_download_event", side_effect=sqlite3.OperationalError("analytics unavailable")):
                        with urlopen(url) as response:
                            self.assertEqual(response.read(), data)
                    self.assertEqual(not_modified.exception.headers["ETag"], etag)
                    with open_database() as connection:
                        self.assertEqual(connection.execute("SELECT unique_download_count FROM blobs WHERE hash=?", (digest,)).fetchone()[0], 1)
                finally:
                    http_server.shutdown()
                    http_server.server_close()
                    thread.join()



class ServerCapacityTests(unittest.TestCase):
    def test_worker_limit_returns_retryable_503(self):
        class SingleWorkerServer(server.LimitedThreadingHTTPServer):
            max_workers = 1

        class BlockingHandler(Handler):
            def do_GET(self):
                self.server.request_started.set()
                self.server.release_request.wait(2)
                body = b"ok"
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        http_server = SingleWorkerServer(("127.0.0.1", 0), BlockingHandler)
        http_server.request_started = Event()
        http_server.release_request = Event()
        server_thread = Thread(target=http_server.serve_forever, daemon=True)
        server_thread.start()
        first_result = []

        def first_request():
            connection = HTTPConnection("127.0.0.1", http_server.server_port, timeout=3)
            connection.request("GET", "/")
            response = connection.getresponse()
            first_result.append((response.status, response.read()))
            connection.close()

        request_thread = Thread(target=first_request)
        request_thread.start()
        try:
            self.assertTrue(http_server.request_started.wait(1))
            connection = HTTPConnection("127.0.0.1", http_server.server_port, timeout=2)
            connection.request("GET", "/")
            response = connection.getresponse()
            self.assertEqual(response.status, 503)
            self.assertEqual(response.getheader("Retry-After"), "1")
            busy_payload = json.loads(response.read())
            self.assertEqual(busy_payload["error"]["code"], "SERVICE_BUSY")
            self.assertEqual(busy_payload["error"]["requestId"], response.getheader("X-Request-ID"))
            connection.close()
        finally:
            http_server.release_request.set()
            request_thread.join()
            http_server.shutdown()
            http_server.server_close()
            server_thread.join()
        self.assertEqual(first_result, [(200, b"ok")])
        self.assertEqual(server.LimitedThreadingHTTPServer.request_queue_size, 128)
        self.assertEqual(Handler.timeout, 30)

    def test_request_id_and_unhandled_error_response(self):
        class ErrorHandler(Handler):
            def do_GET(self):
                if self.path == "/api/test-error":
                    raise RuntimeError("private implementation detail")
                self.send_json(200, {"ok": True})

        http_server = ThreadingHTTPServer(("127.0.0.1", 0), ErrorHandler)
        thread = Thread(target=http_server.serve_forever, daemon=True)
        thread.start()
        try:
            with urlopen(f"http://127.0.0.1:{http_server.server_port}/api/test-success") as response:
                self.assertEqual(response.status, 200)
                self.assertRegex(response.headers["X-Request-ID"], r"^[a-f0-9]{24}$")
            with patch("server.traceback.print_exc"):
                with self.assertRaises(urllib.error.HTTPError) as failure:
                    urlopen(f"http://127.0.0.1:{http_server.server_port}/api/test-error")
            error_body = failure.exception.read()
            payload = json.loads(error_body)
            self.assertEqual(payload["error"]["code"], "REQUEST_FAILED")
            self.assertEqual(payload["error"]["message"], "Внутренняя ошибка сервера")
            self.assertEqual(payload["error"]["requestId"], failure.exception.headers["X-Request-ID"])
            self.assertNotIn("private implementation detail", error_body.decode("utf-8"))
        finally:
            http_server.shutdown()
            http_server.server_close()
            thread.join()

    def test_server_close_waits_for_active_request_threads(self):
        class BlockingHandler(Handler):
            def do_GET(self):
                self.server.request_started.set()
                self.server.release_request.wait(2)
                self.send_json(200, {"ok": True})

        http_server = server.LimitedThreadingHTTPServer(("127.0.0.1", 0), BlockingHandler)
        http_server.request_started = Event()
        http_server.release_request = Event()
        server_thread = Thread(target=http_server.serve_forever, daemon=True)
        server_thread.start()
        request_thread = Thread(
            target=lambda: urlopen(f"http://127.0.0.1:{http_server.server_port}/api/slow").read()
        )
        request_thread.start()
        close_started = Event()
        close_thread = None
        try:
            self.assertTrue(http_server.request_started.wait(1))
            http_server.shutdown()

            def close_server():
                close_started.set()
                http_server.server_close()

            close_thread = Thread(target=close_server)
            close_thread.start()
            self.assertTrue(close_started.wait(1))
            self.assertTrue(close_thread.is_alive())
        finally:
            http_server.release_request.set()
            if close_thread is not None:
                close_thread.join(2)
            request_thread.join(2)
            server_thread.join(2)
        self.assertFalse(close_thread.is_alive())


class DataDirectoryPathTests(unittest.TestCase):
    def test_relative_data_directory_is_resolved_next_to_config_file(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            server_directory = Path(temporary_directory) / "moved-server"
            config_path = server_directory / "server_config.json"
            expected = (server_directory / "data").resolve()

            self.assertEqual(server.resolve_data_directory(config_path, {"dataDir": "data"}), expected)

    def test_absolute_data_directory_is_preserved(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            configured_directory = Path(temporary_directory) / "external-data"
            config_path = Path(temporary_directory) / "server" / "server_config.json"

            self.assertEqual(
                server.resolve_data_directory(config_path, {"dataDir": str(configured_directory)}),
                configured_directory.resolve(),
            )


class DataDirectoryLockTests(unittest.TestCase):
    def test_second_process_is_rejected_until_owner_releases_lock(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            lock = server.DataDirectoryLock(temporary_directory)
            lock.acquire()
            try:
                code = (
                    "import sys\nfrom server import DataDirectoryLock\n"
                    "lock = DataDirectoryLock(sys.argv[1])\n"
                    "try:\n    lock.acquire()\n"
                    "except RuntimeError as error:\n    print(error)\n    raise SystemExit(12)\n"
                    "lock.release()\n"
                )
                project_path = str(Path(server.__file__).resolve().parent)
                environment = os.environ.copy()
                environment["PYTHONPATH"] = project_path + os.pathsep + environment.get("PYTHONPATH", "")
                blocked = subprocess.run([sys.executable, "-c", code, temporary_directory],
                    cwd=project_path, env=environment, capture_output=True, text=True, check=False)
                self.assertEqual(blocked.returncode, 12, blocked.stderr)
                self.assertIn("PID", blocked.stdout)
            finally:
                lock.release()
            available = subprocess.run([sys.executable, "-c", code, temporary_directory],
                cwd=project_path, env=environment, capture_output=True, text=True, check=False)
            self.assertEqual(available.returncode, 0, available.stderr)
            self.assertTrue(lock.path.exists())


class AuditFailureTests(unittest.TestCase):
    def test_audit_database_failure_is_reported_without_raising(self):
        with patch("server.db", side_effect=sqlite3.OperationalError("database is locked")):
            self.assertFalse(server.audit("admin", "release_update", "pack@1.0.0", "127.0.0.1"))


class UploadDiskSpaceTests(unittest.TestCase):
    def test_upload_reservations_include_reserve_and_concurrent_uploads(self):
        root = Path(".")
        disk_usage = server.shutil._ntuple_diskusage(total=1000, used=800, free=200)
        with patch("server.ROOT", root), patch("server.CONFIG", {"minimumFreeBytes": 100}), patch(
            "server.shutil.disk_usage", return_value=disk_usage
        ), patch("server.reserved_upload_bytes", 0):
            first = server.reserve_upload_space(50)
            second = server.reserve_upload_space(51)
            self.assertEqual(first, (True, 200, 100))
            self.assertEqual(second, (False, 200, 100))
            self.assertEqual(server.reserved_upload_bytes, 50)
            server.release_upload_space(50)
            self.assertEqual(server.reserved_upload_bytes, 0)
            self.assertEqual(server.reserve_upload_space(101), (False, 200, 100))

    def test_unavailable_disk_usage_fails_closed(self):
        with patch("server.CONFIG", {"minimumFreeBytes": 100}), patch(
            "server.shutil.disk_usage", side_effect=OSError("disk unavailable")
        ), patch("server.reserved_upload_bytes", 0):
            self.assertEqual(server.reserve_upload_space(1), (False, 0, 100))

    def test_upload_returns_507_before_creating_a_temporary_file(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            disk_usage = server.shutil._ntuple_diskusage(total=1000, used=1000, free=0)
            with patch("server.ROOT", root), patch("server.CONFIG", {"minimumFreeBytes": 100}), patch(
                "server.shutil.disk_usage", return_value=disk_usage
            ), patch("server.reserved_upload_bytes", 0), patch.object(Handler, "require_admin", return_value=True):
                http_server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                thread = Thread(target=http_server.serve_forever, daemon=True)
                thread.start()
                try:
                    request = Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/files?name=test.jar",
                        data=b"upload bytes", headers={"Content-Type": "application/octet-stream"}, method="POST")
                    with self.assertRaises(urllib.error.HTTPError) as error:
                        urlopen(request)
                    self.assertEqual(error.exception.code, 507)
                    payload = json.loads(error.exception.read())
                    self.assertEqual(payload["error"]["code"], "INSUFFICIENT_STORAGE")
                    self.assertFalse(list(root.glob("upload-*.tmp")))
                finally:
                    http_server.shutdown()
                    http_server.server_close()
                    thread.join()


class RepositoryValidationTests(unittest.TestCase):
    def test_password_minimum_is_eight_characters(self):
        self.assertTrue(valid_password("12345678", "12345678"))
        self.assertFalse(valid_password("1234567", "1234567"))
        self.assertFalse(valid_password("12345678", "87654321"))

    def setUp(self):
        self.manifest = {
            "manifestVersion": 1,
            "packId": "adventure-pack",
            "version": "1.0.0",
            "name": "Adventure Pack",
            "description": "Pack used by validator tests",
            "authors": ["Team"],
            "categories": ["adventure"],
            "iconUrl": None,
            "minecraft": {"version": "1.21.1", "loader": "fabric", "loaderVersion": "0.16.9"},
            "java": {"major": 21},
            "files": [{
                "path": "mods/example.jar",
                "hash": "sha256:" + "a" * 64,
                "size": 123,
                "url": "/api/files/sha256:" + "a" * 64,
                "required": True,
            }],
            "settings": {"memoryMb": 4096, "fullscreen": False},
        }

    def test_accepts_well_formed_manifest(self):
        self.assertEqual(validate_manifest(self.manifest, "adventure-pack", "1.0.0"), [])

    def test_full_and_draft_validation_match_schema_for_java_and_extra_fields(self):
        invalid_manifests = []
        unsupported_java = json.loads(json.dumps(self.manifest))
        unsupported_java["java"]["major"] = 18
        invalid_manifests.append(unsupported_java)
        unknown_root = json.loads(json.dumps(self.manifest))
        unknown_root["futureField"] = True
        invalid_manifests.append(unknown_root)
        unknown_minecraft = json.loads(json.dumps(self.manifest))
        unknown_minecraft["minecraft"]["futureField"] = True
        invalid_manifests.append(unknown_minecraft)
        unknown_file_field = json.loads(json.dumps(self.manifest))
        unknown_file_field["files"][0]["futureField"] = True
        invalid_manifests.append(unknown_file_field)
        for manifest in invalid_manifests:
            with self.subTest(manifest=manifest):
                full_errors = validate_manifest(manifest, "adventure-pack", "1.0.0")
                draft_errors = validate_draft_manifest(manifest, "adventure-pack", "1.0.0")
                self.assertTrue(full_errors)
                self.assertTrue(draft_errors)

    def test_manifest_version_boolean_is_not_integer_one(self):
        manifest = json.loads(json.dumps(self.manifest))
        manifest["manifestVersion"] = True
        self.assertTrue(any("manifestVersion=1" in error for error in validate_manifest(manifest)))

    def test_full_manifest_requires_loader_version_but_draft_may_omit_it(self):
        manifest = json.loads(json.dumps(self.manifest))
        del manifest["minecraft"]["loaderVersion"]
        self.assertTrue(any("loaderVersion является обязательным" in error for error in validate_manifest(manifest)))
        self.assertEqual(validate_draft_manifest(manifest, "adventure-pack", "1.0.0"), [])

    def test_incomplete_draft_is_allowed_but_unsafe_paths_are_rejected(self):
        draft = {"packId": "adventure-pack", "version": "1.0.0", "minecraft": {"version": "", "loader": "vanilla"}, "files": []}
        self.assertEqual(validate_draft_manifest(draft, "adventure-pack", "1.0.0"), [])
        draft["files"] = [{"path": "../outside.jar"}]
        self.assertTrue(validate_draft_manifest(draft, "adventure-pack", "1.0.0"))

    def test_accepts_files_in_nested_folders(self):
        nested_file = dict(self.manifest["files"][0])
        nested_file["path"] = "mods/example/config/overrides.json"
        self.manifest["files"].append(nested_file)
        self.assertEqual(validate_manifest(self.manifest, "adventure-pack", "1.0.0"), [])

    def test_accepts_reported_config_and_metadata_files(self):
        for path in (
            "config/aero.toml.bak",
            "config/axiom.json.backup",
            "config/collective.json5",
            "config/ftbchunks.snbt",
            "config/README.md",
            "config/.defaults",
            "resourcepacks/LowOnFire v26.2§8.zip",
        ):
            with self.subTest(path=path):
                self.assertEqual(safe_relative_path(path), path)

    def test_accepts_files_up_to_eight_gib_and_rejects_larger_files(self):
        self.manifest["files"][0]["size"] = 1024**3 + 1
        self.assertEqual(validate_manifest(self.manifest), [])
        self.manifest["files"][0]["size"] = 8 * 1024**3
        self.assertEqual(validate_manifest(self.manifest), [])
        self.manifest["files"][0]["size"] += 1
        errors = validate_manifest(self.manifest)
        self.assertTrue(any("лимит 8 GiB" in error for error in errors))

    def test_reserved_game_directories_are_rejected(self):
        for path in ("assets/indexes/test.json", "Libraries/example.jar", "versions/1.21/manifest.json"):
            with self.subTest(path=path), self.assertRaises(ValueError):
                safe_relative_path(path)

    def test_versions_follow_semver_precedence(self):
        versions = ["1.9.9", "2.0.0-rc.2", "1.10.0", "2.0.0", "2.0.0-rc.10"]
        self.assertEqual(sorted(versions, key=semver_sort_key, reverse=True), [
            "2.0.0", "2.0.0-rc.10", "2.0.0-rc.2", "1.10.0", "1.9.9",
        ])
        self.assertEqual(semver_sort_key("1.0.0-01")[0], 0)

    def test_single_byte_range_and_suffix_range(self):
        self.assertEqual(parse_byte_range("bytes=2-5", 10), (2, 5))
        self.assertEqual(parse_byte_range("bytes=-3", 10), (7, 9))
        self.assertEqual(parse_byte_range("bytes=7-", 10), (7, 9))
        self.assertFalse(parse_byte_range("bytes=10-", 10))
        self.assertFalse(parse_byte_range("bytes=0-1,4-5", 10))


    def test_java_25_is_accepted_from_game_metadata(self):
        self.manifest["java"]["major"] = 25
        self.assertEqual(validate_manifest(self.manifest), [])

    def test_publication_requires_metadata_compatible_java_and_loader(self):
        with patch("server.java_major_for_version", return_value={"major": 25}), patch(
            "server.loader_options", return_value={"versions": ["0.16.9"]}
        ):
            self.manifest["java"]["major"] = 25
            self.assertEqual(validate_official_metadata(self.manifest), [])
            self.manifest["java"]["major"] = 21
            self.assertTrue(any("требуют Java 25" in error for error in validate_official_metadata(self.manifest)))
            self.manifest["java"]["major"] = 25
            self.manifest["minecraft"]["loaderVersion"] = "0.16.8"
            self.assertTrue(any("недоступна" in error for error in validate_official_metadata(self.manifest)))

    def test_offline_metadata_check_reports_unverified_compatibility(self):
        with tempfile.TemporaryDirectory() as temporary_directory, patch("server.ROOT", Path(temporary_directory)), patch.dict(metadata_cache, {}, clear=True), patch(
            "server.urllib.request.urlopen"
        ) as upstream:
            errors = validate_official_metadata(self.manifest, offline=True)
            self.assertTrue(any("совместимость Java не подтверждена" in error for error in errors))
            upstream.assert_not_called()

    def test_unavailable_online_metadata_returns_explicit_error(self):
        with patch("server.java_major_for_version", side_effect=urllib.error.URLError("metadata unavailable")):
            errors = validate_official_metadata(self.manifest)
        self.assertTrue(any("совместимость Java не подтверждена" in error for error in errors))

    def test_rejects_duplicate_case_insensitive_paths(self):
        duplicate = dict(self.manifest["files"][0])
        duplicate["path"] = "MODS/example.jar"
        self.manifest["files"].append(duplicate)
        full_errors = validate_manifest(self.manifest)
        draft_errors = validate_draft_manifest(self.manifest, "adventure-pack", "1.0.0")
        self.assertTrue(any("повторяется путь" in error for error in full_errors))
        self.assertTrue(any("повторяется путь" in error for error in draft_errors))

    def test_rejects_invalid_paths_and_executable_extensions(self):
        for value in ("../outside.jar", "mods/CON.jar", "mods/loader.exe", "C:/outside.jar"):
            with self.subTest(path=value), self.assertRaises(ValueError):
                safe_relative_path(value)

    def test_password_hash_uses_salted_scrypt(self):
        password = "a-long-test-password"
        encoded = password_hash(password)
        with patch("server.PASSWORD_HASH", encoded):
            self.assertTrue(password_matches(password))
            self.assertFalse(password_matches("incorrect-password"))

    def test_server_configuration_loads_persisted_admin_hash(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            path = Path(temporary_directory) / "server_config.json"
            path.write_text('{"adminPasswordHash":"scrypt-hash","port":8790}', encoding="utf-8")
            self.assertEqual(load_configuration(path), {"adminPasswordHash": "scrypt-hash", "port": 8790})

    def test_metadata_uses_recent_cache_when_upstream_times_out(self):
        cached = {"versions": ["21.1.209"]}
        with patch.dict(metadata_cache, {"https://metadata.invalid/test": (time.time() - 60, cached)}), patch(
            "server.urllib.request.urlopen", side_effect=urllib.error.URLError("timed out")
        ):
            self.assertEqual(cached_metadata("https://metadata.invalid/test"), cached)

    def test_metadata_refresh_is_single_flight_per_url(self):
        url = "https://metadata.invalid/single-flight-test"
        expected = {"versions": ["21.1.209"]}
        started = Event()
        release_response = Event()
        barrier = Barrier(7)
        calls = []

        def fetch(_request, timeout):
            calls.append(timeout)
            started.set()
            release_response.wait(2)
            return io.BytesIO(json.dumps(expected).encode("utf-8"))

        with tempfile.TemporaryDirectory() as temporary_directory, patch(
            "server.ROOT", Path(temporary_directory)
        ), patch.dict(metadata_cache, {}, clear=True), patch("server.urllib.request.urlopen", side_effect=fetch):
            results = []
            threads = [Thread(target=lambda: (barrier.wait(), results.append(cached_metadata(url)))) for _ in range(6)]
            for thread in threads:
                thread.start()
            barrier.wait()
            self.assertTrue(started.wait(1))
            release_response.set()
            for thread in threads:
                thread.join(2)

        self.assertEqual(len(calls), 1)
        self.assertEqual(results, [expected] * 6)

    def test_metadata_revalidates_etag_and_reuses_payload_on_304(self):
        url = "https://metadata.invalid/conditional-test"
        expected = {"versions": ["21.1.209"]}
        calls = []

        class Response(io.BytesIO):
            def __init__(self, body, headers):
                super().__init__(body)
                self.headers = headers

        def fetch(request, timeout):
            calls.append(request)
            if len(calls) == 1:
                return Response(json.dumps(expected).encode(), {"ETag": '"versions-v1"', "Cache-Control": "max-age=0"})
            self.assertEqual(request.get_header("If-none-match"), '"versions-v1"')
            raise urllib.error.HTTPError(url, 304, "Not Modified", {"Cache-Control": "max-age=0"}, io.BytesIO())

        with tempfile.TemporaryDirectory() as temporary_directory, patch("server.ROOT", Path(temporary_directory)), patch.dict(
            metadata_cache, {}, clear=True
        ), patch.dict(server.metadata_cache_headers, {}, clear=True), patch("server.urllib.request.urlopen", side_effect=fetch):
            self.assertEqual(cached_metadata(url), expected)
            self.assertEqual(cached_metadata(url), expected)
            self.assertEqual(cached_metadata(url), expected)
        self.assertEqual(len(calls), 3)

    def test_metadata_malformed_response_does_not_replace_stale_cache(self):
        url = "https://metadata.invalid/malformed-test"
        cached = {"versions": ["known-good"]}
        with patch.dict(metadata_cache, {url: (int(time.time()) - 3601, cached)}), patch(
            "server.metadata_cache_headers", {url: {}}
        ), patch("server.urllib.request.urlopen", return_value=io.BytesIO(b"{")):
            self.assertEqual(cached_metadata(url), cached)
            self.assertEqual(metadata_cache[url][1], cached)

    def test_metadata_invalid_maven_xml_does_not_replace_stale_cache(self):
        url = "https://maven.invalid/net/example/metadata.xml"
        cached = "<metadata><versioning><versions><version>1.0</version></versions></versioning></metadata>"
        with patch.dict(metadata_cache, {url: (int(time.time()) - 3601, cached)}), patch(
            "server.metadata_cache_headers", {url: {}}
        ), patch("server.urllib.request.urlopen", return_value=io.BytesIO(b"<metadata/>")):
            self.assertEqual(cached_metadata(url), cached)
            self.assertEqual(metadata_cache[url][1], cached)

    def test_metadata_uses_stale_cache_after_retried_http_503(self):
        url = "https://metadata.invalid/upstream-503-test"
        cached = {"versions": ["known-good"]}
        error = urllib.error.HTTPError(url, 503, "Unavailable", {}, io.BytesIO())
        with patch.dict(metadata_cache, {url: (int(time.time()) - 3601, cached)}), patch(
            "server.metadata_cache_headers", {url: {}}
        ), patch("server.time.sleep"), patch("server.urllib.request.urlopen", side_effect=error) as upstream:
            self.assertEqual(cached_metadata(url), cached)
        self.assertEqual(upstream.call_count, 3)

    def test_metadata_uses_stale_cache_after_upstream_404(self):
        url = "https://metadata.invalid/removed-endpoint"
        cached = {"versions": ["known-good"]}
        error = urllib.error.HTTPError(url, 404, "Not Found", {}, io.BytesIO())
        with patch.dict(metadata_cache, {url: (int(time.time()) - 3601, cached)}), patch(
            "server.metadata_cache_headers", {url: {}}
        ), patch("server.urllib.request.urlopen", side_effect=error):
            self.assertEqual(cached_metadata(url), cached)

    def test_metadata_force_refresh_bypasses_fresh_cache(self):
        url = "https://metadata.invalid/forced-refresh-test"
        cached = {"versions": ["old"]}
        fresh = {"versions": ["new"]}
        with patch.dict(metadata_cache, {url: (int(time.time()), cached)}), patch(
            "server.metadata_cache_headers", {url: {"expiresAt": time.time() + 1800}}
        ), patch("server.urllib.request.urlopen", return_value=io.BytesIO(json.dumps(fresh).encode())) as upstream:
            self.assertEqual(cached_metadata(url), cached)
            self.assertEqual(cached_metadata(url, force=True), fresh)
        upstream.assert_called_once()

    def test_metadata_cache_survives_process_memory_reset(self):
        url = "https://metadata.invalid/persistent-cache-test"
        expected = {"versions": ["21.1.209"]}
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            with patch("server.ROOT", root), patch.dict(metadata_cache, {}, clear=True):
                with patch(
                    "server.urllib.request.urlopen",
                    return_value=io.BytesIO(json.dumps(expected).encode("utf-8")),
                ):
                    self.assertEqual(cached_metadata(url), expected)
                metadata_cache.clear()
                with patch("server.urllib.request.urlopen", side_effect=urllib.error.URLError("timed out")):
                    self.assertEqual(cached_metadata(url), expected)

    def test_metadata_ignores_semantically_invalid_disk_cache(self):
        url = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            cache_path = root / "metadata-cache" / f"{hashlib.sha256(url.encode()).hexdigest()}.json"
            cache_path.parent.mkdir()
            cache_path.write_text(json.dumps({"url": url, "fetchedAt": int(time.time()), "payload": {}}), encoding="utf-8")
            with patch("server.ROOT", root), patch.dict(metadata_cache, {}, clear=True), patch.dict(
                server.metadata_cache_headers, {}, clear=True
            ), patch("server.urllib.request.urlopen") as upstream, self.assertRaisesRegex(ValueError, "Нет кэшированных"):
                cached_metadata(url, offline=True)
            upstream.assert_not_called()

    def test_metadata_timeout_without_cache_identifies_upstream_url(self):
        url = "https://metadata.invalid/cold-cache-test"
        with tempfile.TemporaryDirectory() as temporary_directory:
            with patch("server.ROOT", Path(temporary_directory)), patch.dict(metadata_cache, {}, clear=True), patch(
                "server.urllib.request.urlopen", side_effect=urllib.error.URLError("timed out")
            ), self.assertRaises(urllib.error.URLError) as error:
                cached_metadata(url)
        self.assertIn(url, str(error.exception))

    def test_fabric_options_include_stable_and_preview_versions(self):
        entries = [
            {"intermediary": {"version": "1.21.1"}, "loader": {"version": "0.20.1", "stable": True}},
            {"intermediary": {"version": "1.21.1"}, "loader": {"version": "0.20.0", "stable": False}},
            {"intermediary": {"version": "1.21.1"}, "loader": {"version": "0.20.1", "stable": True}},
        ]
        with patch("server.selected_minecraft_version"), patch(
            "server.cached_metadata", return_value=entries
        ):
            result = loader_options("1.21.1", "fabric")
        self.assertEqual(result["versions"], ["0.20.1", "0.20.0"])
        self.assertEqual([item["stable"] for item in result["items"]], [True, False])
        self.assertTrue(result["items"][0]["latest"])
        self.assertEqual(result["items"][0]["profileUrl"], "https://meta.fabricmc.net/v2/versions/loader/1.21.1/0.20.1/profile/json")
        self.assertIsNone(result["items"][0]["installerUrl"])

    def test_fabric_options_include_versions_without_intermediary_mapping(self):
        entries = [
            {"intermediary": {"version": "0.0.0"}, "loader": {"version": "0.20.1", "stable": True}},
            {"intermediary": {"version": "0.0.0"}, "loader": {"version": "0.20.0", "stable": False}},
        ]
        with patch("server.selected_minecraft_version"), patch(
            "server.cached_metadata", return_value=entries
        ) as cached:
            result = loader_options("26.3", "fabric")
        self.assertEqual(result["versions"], ["0.20.1", "0.20.0"])
        cached.assert_called_once_with(
            "https://meta.fabricmc.net/v2/versions/loader/26.3", offline=False, force=False
        )

    def test_loader_options_report_current_minecraft_26_support(self):
        forge_promotions = {"promos": {"26.1-latest": "62.0.9"}}
        forge_metadata = "<metadata><versioning><versions><version>26.1-62.0.9</version></versions></versioning></metadata>"
        with patch("server.selected_minecraft_version"), patch(
            "server.cached_metadata", side_effect=[forge_promotions, forge_metadata]
        ) as cached:
            forge = loader_options("26.1", "forge")
        self.assertEqual(forge["versions"], ["62.0.9"])
        self.assertIn("promotions_slim.json", cached.call_args_list[0].args[0])

        neo_metadata = {"versions": ["26.1.0.19-beta"]}
        neo_prism = {"versions": [{
            "version": "26.1.0.19-beta",
            "requires": [{"uid": "net.minecraft", "equals": "26.1"}],
        }]}
        with patch("server.selected_minecraft_version"), patch(
            "server.cached_metadata", side_effect=[neo_metadata, neo_prism]
        ):
            neoforge = loader_options("26.1", "neoforge")
        self.assertEqual(neoforge["versions"], ["26.1.0.19-beta"])

    def test_fabric_metadata_maps_official_version_and_profile_endpoints(self):
        expected_urls = {
            "game": "https://meta.fabricmc.net/v2/versions/game",
            "loader": "https://meta.fabricmc.net/v2/versions/loader",
            "installer": "https://meta.fabricmc.net/v2/versions/installer",
            "yarn": "https://meta.fabricmc.net/v2/versions/yarn",
            "intermediary": "https://meta.fabricmc.net/v2/versions/intermediary",
            "compatible-loader": "https://meta.fabricmc.net/v2/versions/loader/1.21.1",
            "yarn-for-game": "https://meta.fabricmc.net/v2/versions/yarn/1.21.1",
            "intermediary-for-game": "https://meta.fabricmc.net/v2/versions/intermediary/1.21.1",
            "loader-details": "https://meta.fabricmc.net/v2/versions/loader/1.21.1/0.20.1",
            "client-profile": "https://meta.fabricmc.net/v2/versions/loader/1.21.1/0.20.1/profile/json",
            "server-profile": "https://meta.fabricmc.net/v2/versions/loader/1.21.1/0.20.1/server/json",
        }
        for resource, expected_url in expected_urls.items():
            with self.subTest(resource=resource), patch("server.cached_metadata", return_value={"ok": True}) as cached:
                result = fabric_metadata(resource, game_version="1.21.1", loader_version="0.20.1")
                self.assertEqual(result, {"url": expected_url, "data": {"ok": True}, "source": "official"})
                cached.assert_called_once_with(expected_url, offline=False, force=False)

        self.assertEqual(
            fabric_metadata("client-profile-zip", game_version="1.21.1", loader_version="0.20.1")["url"],
            "https://meta.fabricmc.net/v2/versions/loader/1.21.1/0.20.1/profile/zip",
        )
        self.assertEqual(
            fabric_metadata("server-jar", game_version="1.21.1", loader_version="0.20.1", installer_version="1.0.1")["url"],
            "https://meta.fabricmc.net/v2/versions/loader/1.21.1/0.20.1/1.0.1/server/jar",
        )
        with self.assertRaisesRegex(ValueError, "корректную версию Minecraft"):
            fabric_metadata("compatible-loader", game_version="../invalid")

    def test_forge_options_include_maven_versions_and_dedupe_promotions(self):
        promotions = {"promos": {"1.21.1-recommended": "52.1.20", "1.21.1-latest": "52.1.20"}}
        metadata = """<metadata><versioning><versions>
            <version>1.21.1-52.1.20</version>
            <version>1.21.1-52.1.19</version>
            <version>1.21.1-52.1.18-beta</version>
        </versions></versioning></metadata>"""
        with patch("server.selected_minecraft_version"), patch(
            "server.cached_metadata", side_effect=[promotions, metadata]
        ):
            result = loader_options("1.21.1", "forge")
        self.assertEqual(result["versions"], ["52.1.20", "52.1.19", "52.1.18-beta"])
        self.assertTrue(result["items"][0]["recommended"])
        self.assertTrue(result["items"][0]["latest"])
        self.assertFalse(result["items"][2]["stable"])
        self.assertTrue(result["items"][0]["installerUrl"].endswith("forge-1.21.1-52.1.20-installer.jar"))
        self.assertTrue(result["items"][0]["metadataUrl"].endswith("/1.21.1-52.1.20/meta.json"))

    def test_forge_uses_slim_promotions_endpoint(self):
        promotions = {"promos": {"1.21.1-recommended": "52.1.20"}}
        metadata = """<metadata><versioning><versions>
            <version>1.21.1-52.1.20</version>
        </versions></versioning></metadata>"""
        with patch("server.selected_minecraft_version"), patch(
            "server.cached_metadata", side_effect=[promotions, metadata]
        ) as cached:
            result = loader_options("1.21.1", "forge")
        self.assertEqual(result["versions"], ["52.1.20"])
        self.assertIn("promotions_slim.json", cached.call_args_list[0].args[0])

    def test_forge_falls_back_to_legacy_promotions_endpoint(self):
        promotions = {"promos": {"1.21.1-recommended": "52.1.20"}}
        metadata = """<metadata><versioning><versions>
            <version>1.21.1-52.1.20</version>
        </versions></versioning></metadata>"""
        with patch("server.selected_minecraft_version"), patch(
            "server.cached_metadata", side_effect=[urllib.error.URLError("unavailable"), promotions, metadata]
        ) as cached:
            result = loader_options("1.21.1", "forge")
        self.assertEqual(result["versions"], ["52.1.20"])
        self.assertIn("promotions_slim.json", cached.call_args_list[0].args[0])
        self.assertIn("promotions.json", cached.call_args_list[1].args[0])

    def test_forge_without_promotions_uses_newest_maven_version_as_latest(self):
        metadata = """<metadata><versioning><versions>
            <version>1.21.1-52.1.9</version>
            <version>1.21.1-52.1.10</version>
            <version>1.21.1-52.1.10-beta</version>
        </versions></versioning></metadata>"""
        with patch("server.selected_minecraft_version"), patch(
            "server.cached_metadata", side_effect=[{"promos": {}}, metadata]
        ):
            result = loader_options("1.21.1", "forge")
        self.assertEqual(result["versions"], ["52.1.10", "52.1.10-beta", "52.1.9"])
        self.assertTrue(result["items"][0]["latest"])
        self.assertTrue(result["items"][0]["stable"])
        self.assertFalse(result["items"][1]["stable"])
        self.assertFalse(any(item["recommended"] for item in result["items"]))

    def test_neoforge_options_parse_stable_maven_versions(self):
        maven = {"versions": ["21.1.209", "21.1.208", "21.1.207-beta", "21.0.164"]}
        prism = {"versions": [
            {"version": "21.1.209", "requires": [{"uid": "net.minecraft", "equals": "1.21.1"}]},
            {"version": "21.1.208", "requires": [{"uid": "net.minecraft", "equals": "1.21.1"}]},
            {"version": "21.1.207-beta", "requires": [{"uid": "net.minecraft", "equals": "1.21.1"}]},
            {"version": "21.0.164", "requires": [{"uid": "net.minecraft", "equals": "1.21.0"}]},
        ]}
        with patch("server.selected_minecraft_version"), patch("server.cached_metadata", side_effect=[maven, prism]):
            result = loader_options("1.21.1", "neoforge")
        self.assertEqual(result["versions"], ["21.1.209", "21.1.208", "21.1.207-beta"])
        self.assertTrue(result["items"][0]["latest"])
        self.assertFalse(result["items"][2]["stable"])
        self.assertTrue(result["items"][0]["installerUrl"].endswith("/21.1.209/neoforge-21.1.209-installer.jar"))

    def test_neoforge_uses_historical_artifact_for_minecraft_1_20_1(self):
        maven = {"isSnapshot": False, "versions": ["1.20.1-47.1.10", "1.20.1-47.1.9", "47.1.82", "1.20.10-47.1.11"]}
        with patch("server.selected_minecraft_version"), patch("server.cached_metadata", return_value=maven) as metadata:
            result = loader_options("1.20.1", "neoforge")
        self.assertEqual(result["versions"], ["1.20.1-47.1.10", "1.20.1-47.1.9"])
        self.assertIn("/net%2Fneoforged%2Fforge", metadata.call_args_list[0].args[0])
        self.assertIn("/net/neoforged/forge/1.20.1-47.1.10/forge-1.20.1-47.1.10-installer.jar", result["items"][0]["installerUrl"])
        self.assertEqual(result["items"][0]["profileUrl"], result["items"][0]["installerUrl"])

    def test_minecraft_options_include_manifest_types_and_version_urls(self):
        manifest = {
            "latest": {"release": "1.21.1", "snapshot": "25w14a"},
            "versions": [
                {"id": "1.21.1", "type": "release", "releaseTime": "2024-09-10T00:00:00Z", "url": "https://piston-meta.mojang.com/release.json"},
                {"id": "25w14a", "type": "snapshot", "releaseTime": "2025-04-01T00:00:00Z", "url": "https://piston-meta.mojang.com/snapshot.json"},
                {"id": "b1.7.3", "type": "old_beta", "releaseTime": "2011-07-08T00:00:00Z", "url": "https://piston-meta.mojang.com/beta.json"},
                {"id": "bad", "type": "unknown", "url": "https://piston-meta.mojang.com/unknown.json"},
            ],
        }
        with patch("server.minecraft_manifest", return_value=manifest):
            result = server.minecraft_options()
        self.assertEqual(result["latestRelease"], "1.21.1")
        self.assertEqual(result["latestSnapshot"], "25w14a")
        self.assertEqual([item["type"] for item in result["versions"]], ["release", "snapshot", "old_beta"])
        self.assertEqual(result["versions"][1]["url"], "https://piston-meta.mojang.com/snapshot.json")
        with patch("server.minecraft_manifest", return_value=manifest):
            self.assertEqual(server.selected_minecraft_version("25w14a"), manifest["versions"][1])

    def test_minecraft_options_can_force_manifest_refresh(self):
        manifest = {"versions": [], "latest": {}}
        with patch("server.minecraft_manifest", return_value=manifest) as fetch_manifest:
            self.assertEqual(server.minecraft_options(force=True)["versions"], [])
        fetch_manifest.assert_called_once_with(force=True)

    def test_rejects_out_of_range_instance_settings(self):
        self.manifest["settings"]["memoryMb"] = 128
        errors = validate_manifest(self.manifest)
        self.assertTrue(any("memoryMb" in error for error in errors))


class ReleaseImmutabilityApiTests(unittest.TestCase):
    def test_create_pack_can_be_retried_after_partial_build_creation(self):
        token = "create-pack-retry-test-token"
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            database = root / "repository.sqlite3"

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
                connection.row_factory = sqlite3.Row
                connection.execute("PRAGMA foreign_keys=ON")
                try:
                    yield connection
                    connection.commit()
                finally:
                    connection.close()

            with patch("server.ROOT", root), patch("server.DATABASE", database), patch(
                "server.OBJECTS", root / "objects" / "sha256"
            ), patch("server.db", open_database):
                from server import initialize

                initialize()
                now = int(time.time())
                with open_database() as connection:
                    connection.execute(
                        "INSERT INTO admin_sessions(token_hash,csrf_token,expires_at,created_at) VALUES(?,?,?,?)",
                        (hashlib.sha256(token.encode()).hexdigest(), "test-csrf", int(time.time() + 60), now),
                    )
                http_server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                thread = Thread(target=http_server.serve_forever, daemon=True)
                thread.start()
                try:
                    url = f"http://127.0.0.1:{http_server.server_port}/api/admin/packs"
                    headers = {"Authorization": f"Bearer {token}", "Content-Type": "application/json"}
                    payload = json.dumps({"id": "retry-pack", "name": "Retry Pack", "description": "A test pack", "authors": [], "categories": [], "iconUrl": None}).encode()
                    with urlopen(Request(url, data=payload, headers=headers, method="POST")) as response:
                        self.assertEqual(response.status, 201)
                        created = json.loads(response.read())
                    with urlopen(Request(url, data=payload, headers=headers, method="POST")) as response:
                        self.assertEqual(response.status, 200)
                        retried = json.loads(response.read())
                    self.assertEqual(retried, {"id": created["id"], "existing": True})

                    manifest = {
                        "manifestVersion": 1,
                        "packId": "retry-pack",
                        "version": "1.0.0",
                        "name": "Retry Pack",
                        "description": "A test pack",
                        "authors": [],
                        "categories": [],
                        "iconUrl": None,
                        "minecraft": {"version": "1.21.1", "loader": "vanilla", "loaderVersion": None},
                        "java": {"major": 21},
                        "files": [],
                        "settings": {},
                    }
                    release_payload = json.dumps({"packId": "retry-pack", "version": "1.0.0", "manifest": manifest}).encode()
                    with urlopen(Request(f"{url.rsplit('/', 1)[0]}/releases", data=release_payload, headers=headers, method="POST")) as response:
                        self.assertEqual(response.status, 201)
                        self.assertEqual(json.loads(response.read())["status"], "draft")

                    conflicting_payload = json.dumps({"id": "retry-pack", "name": "Different Pack", "authors": [], "categories": [], "iconUrl": None}).encode()
                    with self.assertRaises(urllib.error.HTTPError) as conflict:
                        urlopen(Request(url, data=conflicting_payload, headers=headers, method="POST"))
                    self.assertEqual(conflict.exception.code, 409)
                finally:
                    http_server.shutdown()
                    http_server.server_close()
                    thread.join()

    def test_clone_copies_manifest_without_copying_blobs(self):
        token = "release-clone-test-token"
        manifest = {
            "manifestVersion": 1,
            "packId": "test-pack",
            "version": "1.2.3-beta.1",
            "name": "Test",
            "iconUrl": "/api/files/sha256:" + "a" * 64,
            "minecraft": {"version": "1.21.1", "loader": "fabric", "loaderVersion": "0.16.0"},
            "java": {"major": 21},
            "files": [{"path": "mods/example.jar", "hash": "sha256:" + "b" * 64, "size": 12, "required": True}],
            "settings": {"memoryMb": 4096, "fullscreen": False},
        }
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            database = root / "repository.sqlite3"

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
                connection.row_factory = sqlite3.Row
                connection.execute("PRAGMA foreign_keys=ON")
                try:
                    yield connection
                    connection.commit()
                finally:
                    connection.close()

            with patch("server.ROOT", root), patch("server.DATABASE", database), patch(
                "server.OBJECTS", root / "objects" / "sha256"
            ), patch("server.db", open_database):
                from server import initialize

                initialize()
                now = int(time.time())
                with open_database() as connection:
                    connection.execute("INSERT INTO packs(id,title,created_at,updated_at) VALUES('test-pack','Test',?,?)", (now, now))
                    connection.execute("INSERT INTO releases(pack_id,version,status,channel,manifest,created_at,updated_at,published_at) VALUES(?,?, 'published','beta',?,?,?,?)",
                        ("test-pack", manifest["version"], json.dumps(manifest), now, now, now))
                    connection.execute("INSERT INTO blobs(hash,size,created_at) VALUES(?,?,?)", ("b" * 64, 12, now))
                    connection.execute("INSERT INTO admin_sessions(token_hash,csrf_token,expires_at,created_at) VALUES(?,?,?,?)",
                        (hashlib.sha256(token.encode()).hexdigest(), "test-csrf", int(time.time() + 60), now))
                http_server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                thread = Thread(target=http_server.serve_forever, daemon=True)
                thread.start()
                try:
                    payload = json.dumps({"version": "1.2.4", "channel": "stable"}).encode()
                    request = Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/releases/1/clone",
                        data=payload, headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"}, method="POST")
                    with urlopen(request) as response:
                        cloned = json.loads(response.read())
                    self.assertEqual(cloned["status"], "draft")
                    self.assertEqual(cloned["channel"], "stable")
                    self.assertEqual(cloned["manifest"]["version"], "1.2.4")
                    self.assertEqual(cloned["manifest"]["files"], manifest["files"])
                    self.assertEqual(cloned["manifest"]["settings"], manifest["settings"])
                    self.assertEqual(cloned["manifest"]["iconUrl"], manifest["iconUrl"])
                    with urlopen(Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/releases/{cloned['id']}", headers={"Authorization": f"Bearer {token}"})) as response:
                        current = json.loads(response.read())
                    invalid_channel = Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/releases/{cloned['id']}",
                        data=json.dumps({"channel": "nightly"}).encode(), headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json", "If-Match": current["etag"]}, method="PATCH")
                    with self.assertRaises(urllib.error.HTTPError) as invalid_channel_error:
                        urlopen(invalid_channel)
                    self.assertEqual(invalid_channel_error.exception.code, 400)
                    missing_match = Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/releases/{cloned['id']}",
                        data=json.dumps({"channel": "beta"}).encode(), headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"}, method="PATCH")
                    with self.assertRaises(urllib.error.HTTPError) as missing_match_error:
                        urlopen(missing_match)
                    self.assertEqual(missing_match_error.exception.code, 428)
                    channel_request = Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/releases/{cloned['id']}",
                        data=json.dumps({"channel": "beta"}).encode(), headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json", "If-Match": current["etag"]}, method="PATCH")
                    with urlopen(channel_request) as response:
                        channel_updated = json.loads(response.read())
                        self.assertEqual(response.headers["ETag"], channel_updated["etag"])
                    self.assertEqual(channel_updated["channel"], "beta")
                    self.assertNotEqual(channel_updated["etag"], current["etag"])
                    stale_channel = Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/releases/{cloned['id']}",
                        data=json.dumps({"channel": "stable"}).encode(), headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json", "If-Match": current["etag"]}, method="PATCH")
                    with self.assertRaises(urllib.error.HTTPError) as channel_conflict:
                        urlopen(stale_channel)
                    self.assertEqual(channel_conflict.exception.code, 409)
                    with urlopen(Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/releases/{cloned['id']}", headers={"Authorization": f"Bearer {token}"})) as response:
                        current = json.loads(response.read())
                    self.assertEqual(current["channel"], "beta")
                    with open_database() as connection:
                        self.assertEqual(connection.execute("SELECT channel FROM releases WHERE id=?", (cloned["id"],)).fetchone()[0], "beta")
                    patch_body = json.dumps({"settings": {"memoryMb": 8192}}).encode()
                    patch_request = Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/releases/{cloned['id']}", data=patch_body,
                        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json", "If-Match": current["etag"]}, method="PATCH")
                    with urlopen(patch_request) as response:
                        patched = json.loads(response.read())
                    self.assertNotEqual(patched["etag"], current["etag"])
                    stale = Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/releases/{cloned['id']}", data=patch_body,
                        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json", "If-Match": current["etag"]}, method="PATCH")
                    with self.assertRaises(urllib.error.HTTPError) as conflict:
                        urlopen(stale)
                    self.assertEqual(conflict.exception.code, 409)
                    published_channel = Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/releases/1",
                        data=json.dumps({"channel": "stable"}).encode(), headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json", "If-Match": '"stale"'}, method="PATCH")
                    with self.assertRaises(urllib.error.HTTPError) as immutable:
                        urlopen(published_channel)
                    self.assertEqual(immutable.exception.code, 409)
                    with open_database() as connection:
                        connection.execute("UPDATE releases SET published_at=? WHERE id=?", (now, cloned["id"]))
                    for action, expected_status in (("archive", "archived"), ("restore", "draft")):
                        transition = Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/releases/{cloned['id']}/{action}",
                            data=b"{}", headers={"Authorization": f"Bearer {token}"}, method="POST")
                        with urlopen(transition) as response:
                            self.assertEqual(json.loads(response.read())["status"], expected_status)
                    with urlopen(Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/releases/{cloned['id']}", headers={"Authorization": f"Bearer {token}"})) as response:
                        self.assertFalse(json.loads(response.read())["immutable"])
                    with open_database() as connection:
                        self.assertEqual(connection.execute("SELECT COUNT(*) FROM blobs").fetchone()[0], 1)
                        self.assertEqual(connection.execute("SELECT COUNT(*) FROM releases").fetchone()[0], 2)
                        source = connection.execute("SELECT manifest,status FROM releases WHERE id=1").fetchone()
                        self.assertEqual(json.loads(source["manifest"]), manifest)
                        self.assertEqual(source["status"], "published")
                finally:
                    http_server.shutdown()
                    http_server.server_close()
                    thread.join()

    def test_legacy_unpublished_release_is_normalized_and_published_versions_stay_protected(self):
        token = "release-immutability-test-token"
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            database = root / "repository.sqlite3"

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
                connection.row_factory = sqlite3.Row
                connection.execute("PRAGMA foreign_keys=ON")
                try:
                    yield connection
                    connection.commit()
                finally:
                    connection.close()

            with patch("server.ROOT", root), patch("server.DATABASE", database), patch(
                "server.OBJECTS", root / "objects" / "sha256"
            ), patch("server.db", open_database):
                from server import initialize

                initialize()
                now = int(time.time())
                with open_database() as connection:
                    connection.execute("INSERT INTO packs(id,title,created_at,updated_at) VALUES('test-pack','Test',?,?)", (now, now))
                    connection.execute("INSERT INTO releases(pack_id,version,status,channel,manifest,created_at,updated_at,published_at) VALUES('test-pack','1.0.0','draft','stable','{}',?,?,?)", (now, now, now - 60))
                    connection.execute("INSERT INTO releases(pack_id,version,status,channel,manifest,created_at,updated_at,published_at) VALUES('test-pack','1.0.1','published','stable','{}',?,?,?)", (now - 100, now - 100, now - 100))
                    connection.execute("INSERT INTO admin_sessions(token_hash,csrf_token,expires_at,created_at) VALUES(?,?,?,?)", (hashlib.sha256(token.encode()).hexdigest(), "test-csrf", int(time.time() + 60), now))
                initialize()
                http_server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                thread = Thread(target=http_server.serve_forever, daemon=True)
                thread.start()
                base_url = f"http://127.0.0.1:{http_server.server_port}"
                headers = {"Authorization": f"Bearer {token}"}
                try:
                    with urlopen(Request(f"{base_url}/api/admin/dashboard", headers=headers)) as response:
                        release = json.loads(response.read())["releases"][0]
                        self.assertEqual(release["status"], "draft")
                        self.assertFalse(release["immutable"])
                        self.assertNotIn("manifest", release)
                    with urlopen(Request(f"{base_url}/api/admin/dashboard?page=2&perPage=1", headers=headers)) as response:
                        dashboard = json.loads(response.read())
                    self.assertEqual(dashboard["releasePagination"], {"page": 2, "perPage": 1, "pages": 2, "total": 2})
                    self.assertEqual(len(dashboard["releases"]), 1)
                    self.assertEqual(dashboard["releaseSummary"]["total"], 2)
                    with urlopen(Request(f"{base_url}/api/admin/releases/1", headers=headers)) as response:
                        self.assertEqual(json.loads(response.read())["manifest"], {})
                    for release_id, action in ((1, "unpublish"), (2, "publish")):
                        request = Request(f"{base_url}/api/admin/releases/{release_id}/{action}", data=b"{}", headers=headers, method="POST")
                        with self.assertRaises(urllib.error.HTTPError) as error:
                            urlopen(request)
                        self.assertEqual(error.exception.code, 409)
                    with urlopen(Request(f"{base_url}/api/admin/releases/1", headers=headers, method="DELETE")) as response:
                        self.assertEqual(response.status, 200)
                    request = Request(f"{base_url}/api/admin/packs/test-pack", headers=headers, method="DELETE")
                    with self.assertRaises(urllib.error.HTTPError) as error:
                        urlopen(request)
                    self.assertEqual(error.exception.code, 409)
                    with open_database() as connection:
                        self.assertIsNone(connection.execute("SELECT 1 FROM releases WHERE id=1").fetchone())
                        self.assertIsNotNone(connection.execute("SELECT 1 FROM releases WHERE id=2").fetchone())
                finally:
                    http_server.shutdown()
                    http_server.server_close()
                    thread.join()

    def test_unpublish_returns_release_to_deletable_draft(self):
        token = "release-unpublish-test-token"
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            database = root / "repository.sqlite3"

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
                connection.row_factory = sqlite3.Row
                connection.execute("PRAGMA foreign_keys=ON")
                try:
                    yield connection
                    connection.commit()
                finally:
                    connection.close()

            with patch("server.ROOT", root), patch("server.DATABASE", database), patch(
                "server.OBJECTS", root / "objects" / "sha256"
            ), patch("server.db", open_database):
                from server import initialize

                initialize()
                now = int(time.time())
                with open_database() as connection:
                    connection.execute("INSERT INTO packs(id,title,created_at,updated_at) VALUES('test-pack','Test',?,?)", (now, now))
                    connection.execute("INSERT INTO releases(pack_id,version,status,channel,manifest,created_at,updated_at,published_at) VALUES('test-pack','1.0.0','published','stable','{}',?,?,?)", (now, now, now))
                    connection.execute("INSERT INTO admin_sessions(token_hash,csrf_token,expires_at,created_at) VALUES(?,?,?,?)", (hashlib.sha256(token.encode()).hexdigest(), "test-csrf", int(time.time() + 60), now))
                http_server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                thread = Thread(target=http_server.serve_forever, daemon=True)
                thread.start()
                base_url = f"http://127.0.0.1:{http_server.server_port}"
                headers = {"Authorization": f"Bearer {token}"}
                try:
                    request = Request(f"{base_url}/api/admin/releases/1/unpublish", data=b"{}", headers=headers, method="POST")
                    with urlopen(request) as response:
                        self.assertEqual(json.loads(response.read())["status"], "draft")
                    with urlopen(Request(f"{base_url}/api/admin/dashboard", headers=headers)) as response:
                        release = json.loads(response.read())["releases"][0]
                    self.assertEqual(release["status"], "draft")
                    self.assertFalse(release["immutable"])
                    with urlopen(Request(f"{base_url}/api/admin/releases/1", headers=headers, method="DELETE")) as response:
                        self.assertEqual(response.status, 200)
                    with urlopen(Request(f"{base_url}/api/admin/packs/test-pack", headers=headers, method="DELETE")) as response:
                        self.assertEqual(response.status, 200)
                finally:
                    http_server.shutdown()
                    http_server.server_close()
                    thread.join()


class UnauthorizedAdminApiTests(unittest.TestCase):
    def test_audit_supports_pagination_filters_and_csv_export(self):
        token = "audit-pagination-test-token"
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            database = root / "repository.sqlite3"

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
                connection.row_factory = sqlite3.Row
                connection.execute("PRAGMA foreign_keys=ON")
                try:
                    yield connection
                    connection.commit()
                finally:
                    connection.close()

            with patch("server.ROOT", root), patch("server.DATABASE", database), patch(
                "server.OBJECTS", root / "objects" / "sha256"
            ), patch("server.db", open_database):
                from server import initialize

                initialize()
                now = int(time.time())
                with open_database() as connection:
                    connection.executemany("INSERT INTO admin_sessions(token_hash,csrf_token,expires_at,created_at) VALUES(?,?,?,?)",
                        ((hashlib.sha256(token.encode()).hexdigest(), "csrf", now + 60, now),))
                    connection.executemany("INSERT INTO audit(at,actor,action,detail,remote) VALUES(?,?,?,?,?)",
                        ((now, "batch", "test_event", f"row-{index}", "127.0.0.1") for index in range(501)))
                    connection.executemany("INSERT INTO audit(at,actor,action,detail,remote) VALUES(?,?,?,?,?)", (
                        (now, "anonymous", "login_failed", "bad credentials", "127.0.0.1"),
                        (now, "admin", "login", "first entry", "127.0.0.1"),
                        (now, "admin", "release_publish", "target release", "127.0.0.1"),
                        (now, "admin", "logout", "last entry", "127.0.0.1"),
                    ))
                http_server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                thread = Thread(target=http_server.serve_forever, daemon=True)
                thread.start()
                base_url = f"http://127.0.0.1:{http_server.server_port}"
                headers = {"Authorization": f"Bearer {token}"}
                try:
                    with urlopen(Request(f"{base_url}/api/admin/audit?page=2&perPage=1", headers=headers)) as response:
                        page = json.loads(response.read())
                    self.assertEqual((page["page"], page["total"], page["pages"]), (2, 505, 505))
                    self.assertEqual(page["audit"][0]["action"], "release_publish")
                    with urlopen(Request(f"{base_url}/api/admin/audit?action=release_publish&search=target", headers=headers)) as response:
                        filtered = json.loads(response.read())
                    self.assertEqual(filtered["total"], 1)
                    today = time.strftime("%Y-%m-%d", time.gmtime(now))
                    with urlopen(Request(f"{base_url}/api/admin/audit?actor=anonymous&result=failure&from={today}&to={today}", headers=headers)) as response:
                        filtered = json.loads(response.read())
                    self.assertEqual(filtered["total"], 1)
                    self.assertEqual(filtered["audit"][0]["result"], "failure")
                    for invalid_filter in ("from=2026-02-31", "from=20261030", "result=unknown"):
                        with self.assertRaises(urllib.error.HTTPError) as invalid:
                            urlopen(Request(f"{base_url}/api/admin/audit?{invalid_filter}", headers=headers))
                        self.assertEqual(invalid.exception.code, 400)
                    request = Request(f"{base_url}/api/admin/audit?export=csv", headers=headers)
                    with urlopen(request) as response:
                        self.assertEqual(response.headers["Content-Type"], "text/csv; charset=utf-8")
                        self.assertEqual(response.headers["Transfer-Encoding"], "chunked")
                        csv_text = response.read().decode("utf-8-sig")
                        self.assertIn("target release", csv_text)
                        self.assertIn("result,detail", csv_text)
                        self.assertEqual(len(csv_text.splitlines()), 506)
                    with self.assertRaises(urllib.error.HTTPError) as bad_page:
                        urlopen(Request(f"{base_url}/api/admin/audit?page=bad", headers=headers))
                    error_payload = json.loads(bad_page.exception.read())
                    self.assertEqual(error_payload["error"]["code"], "BAD_REQUEST")
                    self.assertTrue(error_payload["error"]["requestId"])
                    self.assertEqual(bad_page.exception.headers["X-Request-ID"], error_payload["error"]["requestId"])
                finally:
                    http_server.shutdown()
                    http_server.server_close()
                    thread.join()

    def test_audit_and_storage_return_one_401_on_keep_alive_connection(self):
        http_server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = Thread(target=http_server.serve_forever, daemon=True)
        thread.start()
        try:
            for path in ("/api/admin/audit", "/api/admin/storage"):
                connection = HTTPConnection("127.0.0.1", http_server.server_port)
                try:
                    connection.request("GET", path)
                    response = connection.getresponse()
                    self.assertEqual(response.status, 401)
                    response.read()
                    connection.request("GET", "/api/health", headers={"Connection": "close"})
                    self.assertIn(connection.getresponse().status, (200, 503))
                finally:
                    connection.close()
        finally:
            http_server.shutdown()
            http_server.server_close()
            thread.join()

    def test_health_returns_service_unavailable_when_not_ready(self):
        with patch("server.readiness_report", return_value=(False, {"database": False, "objectsWritable": True, "schemaVersion": None, "freeBytes": 0, "diskSpace": False})):
            http_server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
            thread = Thread(target=http_server.serve_forever, daemon=True)
            thread.start()
            try:
                with self.assertRaises(urllib.error.HTTPError) as response:
                    urlopen(f"http://127.0.0.1:{http_server.server_port}/api/health")
                self.assertEqual(response.exception.code, 503)
                payload = json.loads(response.exception.read())
                self.assertFalse(payload["ok"])
                self.assertFalse(payload["checks"]["database"])
                with urlopen(f"http://127.0.0.1:{http_server.server_port}/api/live") as live:
                    self.assertEqual(json.loads(live.read()), {"ok": True})
                for path, expected_status in (("/api/health", 503), ("/api/live", 200)):
                        connection = HTTPConnection("127.0.0.1", http_server.server_port)
                        connection.request("HEAD", path)
                        head_response = connection.getresponse()
                        self.assertEqual(head_response.status, expected_status)
                        self.assertEqual(head_response.read(), b"")
                        connection.close()
            finally:
                http_server.shutdown()
                http_server.server_close()
                thread.join()


class ReleaseIntegrityApiTests(unittest.TestCase):
    def test_publish_rejects_same_size_corrupted_blob(self):
        token = "release-integrity-test-token"
        data = b"valid bytes"
        digest = hashlib.sha256(data).hexdigest()
        manifest = {
            "manifestVersion": 1,
            "packId": "integrity-pack",
            "version": "1.0.0",
            "name": "Integrity Pack",
            "description": "",
            "authors": [],
            "categories": [],
            "iconUrl": None,
            "minecraft": {"version": "1.21.1", "loader": "vanilla", "loaderVersion": None},
            "java": {"major": 21},
            "files": [{"path": "mods/test.jar", "hash": f"sha256:{digest}", "size": len(data), "url": f"/api/files/sha256:{digest}", "required": True}],
            "settings": {},
        }
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            database = root / "repository.sqlite3"
            objects = root / "objects" / "sha256"

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
                connection.row_factory = sqlite3.Row
                connection.execute("PRAGMA foreign_keys=ON")
                try:
                    yield connection
                    connection.commit()
                finally:
                    connection.close()

            with patch("server.ROOT", root), patch("server.DATABASE", database), patch(
                "server.OBJECTS", objects
            ), patch("server.db", open_database):
                from server import initialize

                initialize()
                now = int(time.time())
                target = objects / digest[:2] / digest
                target.parent.mkdir(parents=True)
                target.write_bytes(b"bad bytes!!")
                with open_database() as connection:
                    connection.execute("INSERT INTO packs(id,title,created_at,updated_at) VALUES('integrity-pack','Integrity',?,?)", (now, now))
                    connection.execute("INSERT INTO blobs(hash,size,created_at) VALUES(?,?,?)", (digest, len(data), now))
                    connection.execute("INSERT INTO releases(pack_id,version,status,channel,manifest,created_at,updated_at) VALUES(?,?,'draft','stable',?,?,?)", ("integrity-pack", "1.0.0", json.dumps(manifest), now, now))
                    connection.execute("INSERT INTO admin_sessions(token_hash,csrf_token,expires_at,created_at) VALUES(?,?,?,?)", (hashlib.sha256(token.encode()).hexdigest(), "test-csrf", int(time.time() + 60), now))
                http_server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                thread = Thread(target=http_server.serve_forever, daemon=True)
                thread.start()
                try:
                    unauthenticated_connection = HTTPConnection("127.0.0.1", http_server.server_port)
                    try:
                        unauthenticated_connection.request("POST", "/api/admin/releases/1/validate", body=b"{}", headers={"Content-Type": "application/json"})
                        unauthorized_response = unauthenticated_connection.getresponse()
                        self.assertEqual(unauthorized_response.status, 401)
                        self.assertEqual(unauthorized_response.getheader("Connection"), "close")
                        unauthorized_response.read()
                        unauthenticated_connection.request("GET", "/api/live")
                        live_response = unauthenticated_connection.getresponse()
                        self.assertEqual(live_response.status, 200)
                        live_response.read()
                    finally:
                        unauthenticated_connection.close()
                    publish_url = f"http://127.0.0.1:{http_server.server_port}/api/admin/releases/1/publish"
                    request = Request(publish_url, data=b"{}", headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"}, method="POST")
                    with self.assertRaises(urllib.error.HTTPError) as missing_readiness:
                        urlopen(request)
                    self.assertEqual(missing_readiness.exception.code, 409)
                    validate_url = f"http://127.0.0.1:{http_server.server_port}/api/admin/releases/1/validate"
                    validate_request = Request(validate_url, data=b"{}", headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"}, method="POST")
                    with patch("server.validate_official_metadata", return_value=[]), urlopen(validate_request) as response:
                        readiness = json.loads(response.read())
                    self.assertFalse(readiness["ready"])
                    self.assertTrue(any("SHA-256" in item for item in readiness["validationErrors"]))
                    with patch("server.validate_official_metadata", return_value=[]):
                        keep_alive_connection = HTTPConnection("127.0.0.1", http_server.server_port)
                        try:
                            headers = {"Authorization": f"Bearer {token}", "Content-Type": "application/json"}
                            keep_alive_connection.request("POST", "/api/admin/releases/1/validate", body=b"{}", headers=headers)
                            validation_response = keep_alive_connection.getresponse()
                            self.assertEqual(validation_response.status, 200)
                            validation_response.read()
                            keep_alive_connection.request("GET", "/api/admin/releases/1", headers={"Authorization": f"Bearer {token}"})
                            detail_response = keep_alive_connection.getresponse()
                            self.assertEqual(detail_response.status, 200)
                            detail_response.read()
                        finally:
                            keep_alive_connection.close()
                    request = Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/releases/1/publish", data=b"{}", headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"}, method="POST")
                    with self.assertRaises(urllib.error.HTTPError) as error:
                        urlopen(request)
                    self.assertEqual(error.exception.code, 422)
                    payload = json.loads(error.exception.read())
                    self.assertTrue(any("SHA-256" in item for item in payload["validationErrors"]))
                    with patch("server.validate_official_metadata", return_value=["Официальная совместимость не подтверждена"]), urlopen(validate_request) as response:
                        readiness = json.loads(response.read())
                    self.assertFalse(readiness["ready"])
                    request = Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/releases/1/publish", data=b"{}", headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"}, method="POST")
                    with self.assertRaises(urllib.error.HTTPError) as unverified:
                        urlopen(request)
                    self.assertEqual(unverified.exception.code, 422)
                    payload = json.loads(unverified.exception.read())
                    self.assertIn("Официальная совместимость не подтверждена", payload["validationErrors"])
                    target.write_bytes(data)
                    with patch("server.validate_official_metadata", return_value=[]), urlopen(validate_request) as response:
                        readiness = json.loads(response.read())
                    self.assertTrue(readiness["ready"])
                    with open_database() as connection:
                        saved_readiness = connection.execute("SELECT validation_hash,ready FROM release_readiness WHERE release_id=1").fetchone()
                        self.assertEqual(saved_readiness["validation_hash"], readiness["validationHash"])
                        self.assertEqual(saved_readiness["ready"], 1)
                        connection.execute("UPDATE release_readiness SET validated_at=? WHERE release_id=1", (int(time.time()) - server.RELEASE_READINESS_SECONDS - 1,))
                    request = Request(publish_url, data=b"{}", headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"}, method="POST")
                    with self.assertRaises(urllib.error.HTTPError) as expired_readiness:
                        urlopen(request)
                    self.assertEqual(expired_readiness.exception.code, 409)
                    with patch("server.validate_official_metadata", return_value=[]), urlopen(validate_request) as response:
                        self.assertTrue(json.loads(response.read())["ready"])
                    target.write_bytes(b"evil bytes!")
                    request = Request(publish_url, data=b"{}", headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"}, method="POST")
                    with patch("server.hash_file", side_effect=AssertionError("publish must not rehash blobs")), self.assertRaises(urllib.error.HTTPError) as changed_artifact:
                        urlopen(request)
                    self.assertEqual(changed_artifact.exception.code, 422)
                    self.assertTrue(any("изменился после проверки" in item for item in json.loads(changed_artifact.exception.read())["validationErrors"]))
                    target.write_bytes(data)
                    with patch("server.validate_official_metadata", return_value=[]), urlopen(validate_request) as response:
                        self.assertTrue(json.loads(response.read())["ready"])
                    with urlopen(Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/dashboard", headers={"Authorization": f"Bearer {token}"})) as response:
                        dashboard_release = json.loads(response.read())["releases"][0]
                    self.assertFalse(dashboard_release["needsAttention"])
                    with urlopen(Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/releases/1", headers={"Authorization": f"Bearer {token}"})) as response:
                        current_release = json.loads(response.read())
                    changed_manifest = current_release["manifest"]
                    changed_manifest["settings"] = {"memoryMb": 8192}
                    update = Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/releases/1", data=json.dumps(changed_manifest).encode(),
                        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json", "If-Match": current_release["etag"]}, method="PUT")
                    with urlopen(update):
                        pass
                    with urlopen(Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/dashboard", headers={"Authorization": f"Bearer {token}"})) as response:
                        dashboard_release = json.loads(response.read())["releases"][0]
                    self.assertTrue(dashboard_release["needsAttention"])
                    request = Request(publish_url, data=b"{}", headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"}, method="POST")
                    with self.assertRaises(urllib.error.HTTPError) as stale_readiness:
                        urlopen(request)
                    self.assertEqual(stale_readiness.exception.code, 409)
                finally:
                    http_server.shutdown()
                    http_server.server_close()
                    thread.join()


class AdminSessionApiTests(unittest.TestCase):
    def test_download_visitor_secret_persists_independently_of_password(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            root.mkdir(exist_ok=True)
            with patch("server.ROOT", root), patch("server.CONFIG", {}), patch("server.PASSWORD_HASH", "first-password-hash"):
                server.ensure_download_visitor_secret()
                first_secret = server.DOWNLOAD_VISITOR_SECRET
                self.assertEqual(len(first_secret), 32)

            with patch("server.ROOT", root), patch("server.CONFIG", {}), patch("server.PASSWORD_HASH", "rotated-password-hash"):
                server.ensure_download_visitor_secret()
                self.assertEqual(server.DOWNLOAD_VISITOR_SECRET, first_secret)

    def test_cookie_session_survives_database_reinitialization_and_requires_csrf(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            database = root / "repository.sqlite3"

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
                connection.row_factory = sqlite3.Row
                connection.execute("PRAGMA foreign_keys=ON")
                try:
                    yield connection
                    connection.commit()
                finally:
                    connection.close()

            with patch("server.ROOT", root), patch("server.DATABASE", database), patch(
                "server.OBJECTS", root / "objects" / "sha256"
            ), patch("server.db", open_database), patch("server.PASSWORD_HASH", "configured"), patch(
                "server.password_matches", return_value=True
            ):
                from server import initialize

                initialize()
                http_server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                thread = Thread(target=http_server.serve_forever, daemon=True)
                thread.start()
                base_url = f"http://127.0.0.1:{http_server.server_port}"
                try:
                    login = Request(f"{base_url}/api/admin/login", data=b'{"password":"test"}', headers={"Content-Type": "application/json"})
                    with urlopen(login) as response:
                        cookie = response.headers["Set-Cookie"]
                        session_cookie = cookie.split(";", 1)[0]
                        csrf_token = json.loads(response.read())["csrfToken"]
                        self.assertIn("HttpOnly", cookie)
                        self.assertIn("SameSite=Strict", cookie)
                        self.assertIn("Path=/api/admin", cookie)

                    initialize()
                    with urlopen(Request(f"{base_url}/api/admin/session", headers={"Cookie": session_cookie})) as response:
                        self.assertEqual(json.loads(response.read())["csrfToken"], csrf_token)

                    mutation = Request(f"{base_url}/api/admin/logout", data=b"{}", headers={"Cookie": session_cookie, "Content-Type": "application/json"}, method="POST")
                    with self.assertRaises(urllib.error.HTTPError) as csrf_error:
                        urlopen(mutation)
                    self.assertEqual(csrf_error.exception.code, 403)

                    mutation.add_header("X-CSRF-Token", csrf_token)
                    with urlopen(mutation) as response:
                        self.assertEqual(json.loads(response.read()), {"loggedOut": True})
                        self.assertIn("Max-Age=0", response.headers["Set-Cookie"])
                        self.assertIn("Path=/api/admin", response.headers["Set-Cookie"])
                finally:
                    http_server.shutdown()
                    http_server.server_close()
                    thread.join()

    def test_login_block_survives_in_memory_counter_reset(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            database = root / "repository.sqlite3"

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
                connection.row_factory = sqlite3.Row
                connection.execute("PRAGMA foreign_keys=ON")
                try:
                    yield connection
                    connection.commit()
                finally:
                    connection.close()

            with patch("server.ROOT", root), patch("server.DATABASE", database), patch(
                "server.OBJECTS", root / "objects" / "sha256"
            ), patch("server.db", open_database), patch("server.PASSWORD_HASH", "configured"), patch(
                "server.password_matches", return_value=False
            ):
                from server import initialize

                initialize()
                http_server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                thread = Thread(target=http_server.serve_forever, daemon=True)
                thread.start()
                url = f"http://127.0.0.1:{http_server.server_port}/api/admin/login"
                try:
                    for _ in range(8):
                        request = Request(url, data=b'{"password":"wrong"}', headers={"Content-Type": "application/json"})
                        with self.assertRaises(urllib.error.HTTPError) as error:
                            urlopen(request)
                        self.assertEqual(error.exception.code, 401)
                    initialize()
                    request = Request(url, data=b'{"password":"wrong"}', headers={"Content-Type": "application/json"})
                    with self.assertRaises(urllib.error.HTTPError) as blocked:
                        urlopen(request)
                    self.assertEqual(blocked.exception.code, 429)
                finally:
                    http_server.shutdown()
                    http_server.server_close()
                    thread.join()


class PublicVersionApiTests(unittest.TestCase):
    def test_catalog_uses_semver_and_exposes_beta_only_packs(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            database = root / "repository.sqlite3"

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
                connection.row_factory = sqlite3.Row
                connection.execute("PRAGMA foreign_keys=ON")
                try:
                    yield connection
                    connection.commit()
                finally:
                    connection.close()

            with patch("server.ROOT", root), patch("server.DATABASE", database), patch(
                "server.OBJECTS", root / "objects" / "sha256"
            ), patch("server.db", open_database):
                from server import initialize

                initialize()
                now = int(time.time())
                with open_database() as connection:
                    for pack_id in ("stable-pack", "beta-only-pack"):
                        connection.execute("INSERT INTO packs(id,title,created_at,updated_at) VALUES(?,?,?,?)", (pack_id, pack_id, now, now))
                    releases = (
                        ("stable-pack", "2.0.0", "stable", 100),
                        ("stable-pack", "1.0.0", "stable", 200),
                        ("beta-only-pack", "0.9.0", "beta", 300),
                        ("beta-only-pack", "0.10.0", "beta", 100),
                    )
                    for pack_id, version, channel, updated_at in releases:
                        connection.execute(
                            "INSERT INTO releases(pack_id,version,status,channel,manifest,created_at,updated_at,published_at) VALUES(?,?,'published',?,?,?, ?,?)",
                            (pack_id, version, channel, json.dumps({"version": version}), now, updated_at, now),
                        )

                http_server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                thread = Thread(target=http_server.serve_forever, daemon=True)
                thread.start()
                base_url = f"http://127.0.0.1:{http_server.server_port}"
                try:
                    with urlopen(f"{base_url}/api/packs") as response:
                        packs_etag = response.headers["ETag"]
                        self.assertEqual(response.headers["Cache-Control"], "public, max-age=45")
                        stable = json.loads(response.read())["packs"]
                    self.assertEqual(stable[0]["latestVersion"], "2.0.0")
                    self.assertEqual({pack["id"] for pack in stable}, {"stable-pack"})
                    with urlopen(f"{base_url}/api/packs?channel=beta") as response:
                        beta = json.loads(response.read())["packs"]
                    self.assertEqual(beta[0]["latestVersion"], "0.10.0")
                    self.assertEqual({pack["id"] for pack in beta}, {"beta-only-pack"})
                    with urlopen(f"{base_url}/api/packs/stable-pack/versions?channel=stable") as response:
                        versions = json.loads(response.read())["versions"]
                    self.assertEqual(versions[0], {"version": "2.0.0", "channel": "stable", "updatedAt": 100})
                    with urlopen(f"{base_url}/api/packs/stable-pack?channel=stable") as response:
                        pack_versions = json.loads(response.read())["versions"]
                    self.assertEqual(pack_versions, versions)
                    with urlopen(f"{base_url}/api/packs/stable-pack/versions/2.0.0/manifest") as response:
                        manifest_etag = response.headers["ETag"]
                        self.assertEqual(response.headers["Cache-Control"], "public, max-age=31536000, immutable")
                        self.assertEqual(json.loads(response.read())["version"], "2.0.0")
                    conditional = Request(f"{base_url}/api/packs", headers={"If-None-Match": packs_etag})
                    with self.assertRaises(urllib.error.HTTPError) as not_modified:
                        urlopen(conditional)
                    self.assertEqual(not_modified.exception.code, 304)
                    manifest_head = Request(f"{base_url}/api/packs/stable-pack/versions/2.0.0/manifest", method="HEAD")
                    with urlopen(manifest_head) as response:
                        self.assertEqual(response.status, 200)
                        self.assertEqual(response.headers["ETag"], manifest_etag)
                    with self.assertRaises(urllib.error.HTTPError) as error:
                        urlopen(f"{base_url}/api/packs/stable-pack/versions/2.0.0")
                    self.assertEqual(error.exception.code, 404)
                finally:
                    http_server.shutdown()
                    http_server.server_close()
                    thread.join()


class DownloadStatisticsTests(unittest.TestCase):
    def test_release_downloads_are_attributed_and_deduplicated_per_day(self):
        token = "download-stats-test-token"
        data = b"resource pack file"
        digest = hashlib.sha256(data).hexdigest()
        manifest = {"files": [{"hash": f"sha256:{digest}", "url": f"/api/files/sha256:{digest}"}],
            "minecraft": {"version": "1.20.1", "loader": "fabric", "loaderVersion": "0.15"}}
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            database = root / "repository.sqlite3"
            objects = root / "objects" / "sha256"
            target = objects / digest[:2] / digest
            target.parent.mkdir(parents=True)
            target.write_bytes(data)

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
                connection.row_factory = sqlite3.Row
                connection.execute("PRAGMA foreign_keys=ON")
                try:
                    yield connection
                    connection.commit()
                finally:
                    connection.close()

            with patch("server.ROOT", root), patch("server.DATABASE", database), patch(
                "server.OBJECTS", objects
            ), patch("server.db", open_database):
                from server import initialize

                initialize()
                now = int(time.time())
                with open_database() as connection:
                    connection.execute("INSERT INTO packs(id,title,created_at,updated_at) VALUES('stats-pack','Stats',?,?)", (now, now))
                    connection.execute("INSERT INTO releases(pack_id,version,status,channel,manifest,created_at,updated_at,published_at) VALUES(?,?,'published','stable',?,?,?,?)",
                        ("stats-pack", "1.0.0", json.dumps(manifest), now, now, now))
                    connection.execute("INSERT INTO blobs(hash,size,created_at) VALUES(?,?,?)", (digest, len(data), now))
                    connection.execute("INSERT INTO admin_sessions(token_hash,csrf_token,expires_at,created_at) VALUES(?,?,?,?)",
                        (hashlib.sha256(token.encode()).hexdigest(), "stats-csrf", now + 60, now))

                http_server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                thread = Thread(target=http_server.serve_forever, daemon=True)
                thread.start()
                base_url = f"http://127.0.0.1:{http_server.server_port}"
                try:
                    valid_url = f"{base_url}/api/files/sha256:{digest}?packId=stats-pack&version=1.0.0&channel=stable"
                    for _ in range(2):
                        with urlopen(valid_url) as response:
                            self.assertEqual(response.read(), data)
                    invalid_context = f"{base_url}/api/files/sha256:{digest}?packId=stats-pack&version=1.0.0&channel=beta"
                    with urlopen(invalid_context) as response:
                        self.assertEqual(response.status, 200)
                    server.flush_download_events()
                    request = Request(f"{base_url}/api/admin/stats?days=30", headers={"Authorization": f"Bearer {token}"})
                    with urlopen(request) as response:
                        result = json.loads(response.read())
                    release_stat = next(item for item in result["stats"] if item["pack_id"] == "stats-pack")
                    self.assertEqual((release_stat["version"], release_stat["channel"], release_stat["files"], release_stat["file_visitors"]),
                        ("1.0.0", "stable", 1, 1))
                    self.assertTrue(any(item["pack_id"] == "" for item in result["stats"]))
                    self.assertEqual(len(result["daily"]), 1)
                    self.assertEqual(result["daily"][0]["file_visitors"], 2)
                    self.assertEqual(result["topPacks"][0]["pack_id"], "stats-pack")
                    dashboard_request = Request(f"{base_url}/api/admin/dashboard", headers={"Authorization": f"Bearer {token}"})
                    with urlopen(dashboard_request) as response:
                        dashboard = json.loads(response.read())
                    dashboard_release = dashboard["releases"][0]
                    self.assertEqual(dashboard_release["attentionReasons"], ["Проверка готовности не выполнялась"])
                    self.assertFalse(dashboard_release["ready"])
                    self.assertIsNone(dashboard_release["validatedAt"])
                    self.assertEqual((dashboard_release["gameVersion"], dashboard_release["loader"]), ("1.20.1", "fabric"))
                    paged = Request(f"{base_url}/api/admin/stats?days=30&page=2&perPage=1", headers={"Authorization": f"Bearer {token}"})
                    with urlopen(paged) as response:
                        page_two = json.loads(response.read())
                    self.assertEqual((page_two["page"], page_two["perPage"], page_two["pages"], page_two["total"]), (2, 1, 2, 2))
                    self.assertEqual(len(page_two["stats"]), 1)
                    last_page = Request(f"{base_url}/api/admin/stats?days=30&page=99&perPage=1", headers={"Authorization": f"Bearer {token}"})
                    with urlopen(last_page) as response:
                        self.assertEqual(json.loads(response.read())["page"], 2)
                    invalid_page = Request(f"{base_url}/api/admin/stats?days=30&page=bad", headers={"Authorization": f"Bearer {token}"})
                    with self.assertRaises(urllib.error.HTTPError) as invalid:
                        urlopen(invalid_page)
                    self.assertEqual(invalid.exception.code, 400)
                finally:
                    http_server.shutdown()
                    http_server.server_close()
                    thread.join()


class BackupTests(unittest.TestCase):
    def test_backup_is_atomic_verified_and_retained(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            data_root = root / "data"
            database = data_root / "repository.sqlite3"
            objects = data_root / "objects" / "sha256"
            object_file = objects / "ab" / ("ab" * 32)
            object_file.parent.mkdir(parents=True)
            object_file.write_bytes(b"object content")
            data_root.mkdir(exist_ok=True)
            (data_root / "download-visitor-secret").write_bytes(b"s" * 32)
            seed_connection = sqlite3.connect(database)
            try:
                connection = seed_connection
                connection.execute("CREATE TABLE sample(value TEXT)")
                connection.execute("INSERT INTO sample VALUES('backup')")
                seed_connection.commit()
            finally:
                seed_connection.close()

            backups = root / "backups"
            backups.mkdir()
            (backups / "20240101-000000").mkdir()
            (backups / "20250101-000000").mkdir()
            snapshot = create_backup(database, objects, data_root, backups, retention=2)

            self.assertTrue(snapshot.is_dir())
            verification = verify_backup(snapshot, backups)
            self.assertTrue(verification["valid"])
            self.assertEqual(verification["sqliteIntegrity"], "ok")
            self.assertEqual(verification["checkedFiles"], 3)
            check_connection = sqlite3.connect(snapshot / "repository.sqlite3")
            try:
                self.assertEqual(check_connection.execute("PRAGMA integrity_check").fetchone()[0], "ok")
            finally:
                check_connection.close()
            self.assertEqual((snapshot / "download-visitor-secret").read_bytes(), b"s" * 32)
            manifest = json.loads((snapshot / "manifest.json").read_text(encoding="utf-8"))
            for item in manifest["files"]:
                path = snapshot / item["path"]
                self.assertEqual(path.stat().st_size, item["size"])
                from backup import sha256_file
                self.assertEqual(sha256_file(path), item["sha256"])
            names = sorted(path.name for path in backups.iterdir() if path.is_dir())
            self.assertEqual(len(names), 2)
            self.assertNotIn("20240101-000000", names)
            self.assertFalse(any(path.name.startswith(".") for path in backups.iterdir()))
            (snapshot / "objects" / "sha256" / "ab" / ("ab" * 32)).write_bytes(b"corrupt")
            with self.assertRaisesRegex(ValueError, "Проверка целостности"):
                verify_backup(snapshot, backups)
            (snapshot / "objects" / "sha256" / "ab" / ("ab" * 32)).write_bytes(b"object content")
            manifest_path = snapshot / "manifest.json"
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            manifest["files"] = [item for item in manifest["files"] if not item["path"].startswith("objects/")]
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "не соответствует фактическому составу"):
                verify_backup(snapshot, backups)

    def test_restore_replaces_data_only_after_safety_backup(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            data_root = root / "data"
            database = data_root / "repository.sqlite3"
            objects = data_root / "objects" / "sha256"
            object_file = objects / "ab" / ("ab" * 32)
            object_file.parent.mkdir(parents=True)
            object_file.write_bytes(b"old blob")
            data_root.mkdir(exist_ok=True)
            secret = data_root / "download-visitor-secret"
            secret.write_bytes(b"o" * 32)
            connection = sqlite3.connect(database)
            connection.execute("CREATE TABLE sample(value TEXT)")
            connection.execute("INSERT INTO sample VALUES('old')")
            connection.commit()
            connection.close()
            backups = root / "backups"
            snapshot = create_backup(database, objects, data_root, backups)

            connection = sqlite3.connect(database)
            connection.execute("INSERT INTO sample VALUES('new')")
            connection.commit()
            connection.close()
            object_file.write_bytes(b"new blob")
            secret.write_bytes(b"n" * 32)

            result = restore_backup(snapshot, database, objects, data_root, backups)

            connection = sqlite3.connect(database)
            try:
                self.assertEqual(connection.execute("SELECT value FROM sample").fetchall(), [("old",)])
            finally:
                connection.close()
            self.assertEqual(object_file.read_bytes(), b"old blob")
            self.assertEqual(secret.read_bytes(), b"o" * 32)
            self.assertTrue(Path(result["safetyBackup"]).is_dir())
            self.assertEqual(len(list_backups(backups)), 2)


class SystemOperationsApiTests(unittest.TestCase):
    def test_system_backup_verify_and_deferred_restore(self):
        token = "system-operations-test-token"
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            database = root / "repository.sqlite3"
            objects = root / "objects" / "sha256"
            backup_directory = root / "backups"

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
                connection.row_factory = sqlite3.Row
                connection.execute("PRAGMA foreign_keys=ON")
                try:
                    yield connection
                    connection.commit()
                finally:
                    connection.close()

            with patch("server.ROOT", root), patch("server.DATABASE", database), patch("server.OBJECTS", objects), \
                patch("server.BACKUP_DIR", backup_directory), patch("server.CONFIG", {"minimumFreeBytes": 0}), \
                patch("server.db", open_database):
                server.initialize()
                object_file = objects / "ab" / ("ab" * 32)
                object_file.parent.mkdir(parents=True)
                object_file.write_bytes(b"backup object")
                with open_database() as connection:
                    connection.execute("CREATE TABLE sample(value TEXT)")
                    connection.execute("INSERT INTO sample VALUES('before')")
                    connection.execute("INSERT INTO admin_sessions(token_hash,csrf_token,expires_at,created_at) VALUES(?,?,?,?)",
                        (hashlib.sha256(token.encode()).hexdigest(), "system-csrf", int(time.time()) + 600, int(time.time())))
                snapshot = create_backup(database, objects, root, backup_directory)
                with open_database() as connection:
                    connection.execute("UPDATE sample SET value='after'")
                object_file.write_bytes(b"changed object")

                class NoopTimer:
                    daemon = False

                    def start(self):
                        return None

                with patch("server.threading.Timer", return_value=NoopTimer()):
                    http_server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                    thread = Thread(target=http_server.serve_forever, daemon=True)
                    thread.start()
                    base_url = f"http://127.0.0.1:{http_server.server_port}"
                    headers = {"Authorization": f"Bearer {token}"}
                    try:
                        with urlopen(Request(f"{base_url}/api/admin/system", headers=headers)) as response:
                            system_status = json.loads(response.read())
                        self.assertEqual(system_status["status"], "ready")
                        self.assertIn("databaseBytes", system_status)
                        self.assertIn("activeTasks", system_status)
                        with urlopen(Request(f"{base_url}/api/admin/backups", headers=headers)) as response:
                            history = json.loads(response.read())["backups"]
                        self.assertEqual(history[0]["id"], snapshot.name)
                        verify = Request(f"{base_url}/api/admin/backups/{snapshot.name}/verify", data=b"{}",
                            headers={**headers, "Content-Type": "application/json"}, method="POST")
                        with urlopen(verify) as response:
                            self.assertTrue(json.loads(response.read())["valid"])
                        diagnose = Request(f"{base_url}/api/admin/system/diagnose", data=b"{}",
                            headers={**headers, "Content-Type": "application/json"}, method="POST")
                        with urlopen(diagnose) as response:
                            self.assertTrue(json.loads(response.read())["ok"])
                        start_backup = Request(f"{base_url}/api/admin/backups", data=b"{}",
                            headers={**headers, "Content-Type": "application/json"}, method="POST")
                        with urlopen(start_backup) as response:
                            self.assertEqual(response.status, 202)
                        for _ in range(200):
                            if not server.backup_execution_lock.locked():
                                break
                            Event().wait(0.01)
                        self.assertFalse(server.backup_execution_lock.locked())
                        with urlopen(Request(f"{base_url}/api/admin/backups", headers=headers)) as response:
                            self.assertGreaterEqual(len(json.loads(response.read())["backups"]), 2)
                        restore = Request(f"{base_url}/api/admin/backups/{snapshot.name}/restore", data=b"{}",
                            headers={**headers, "Content-Type": "application/json"}, method="POST")
                        with urlopen(restore) as response:
                            self.assertEqual(json.loads(response.read())["status"], "scheduled")
                        self.assertTrue((root / "pending-restore.json").is_file())
                        revoke = Request(f"{base_url}/api/admin/system/sessions/revoke", data=b"{}",
                            headers={**headers, "Content-Type": "application/json"}, method="POST")
                        with urlopen(revoke) as response:
                            self.assertEqual(json.loads(response.read())["revoked"], 1)
                    finally:
                        http_server.shutdown()
                        http_server.server_close()
                        thread.join()

                data_lock = server.DataDirectoryLock(root)
                data_lock.acquire()
                try:
                    restore_state = server.apply_pending_restore()
                    server.initialize()
                    with open_database() as connection:
                        self.assertEqual(connection.execute("SELECT value FROM sample").fetchone()[0], "before")
                        self.assertEqual(connection.execute("SELECT COUNT(*) FROM admin_sessions").fetchone()[0], 0)
                    self.assertEqual(object_file.read_bytes(), b"backup object")
                    self.assertEqual(restore_state["status"], "completed")
                    self.assertTrue((backup_directory / restore_state["safetyBackup"]).is_dir())
                finally:
                    data_lock.release()

    def test_password_change_persists_hash_and_revokes_sessions(self):
        token = "password-change-test-token"
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            database = root / "repository.sqlite3"
            config_path = root / "server_config.json"

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
                connection.row_factory = sqlite3.Row
                connection.execute("PRAGMA foreign_keys=ON")
                try:
                    yield connection
                    connection.commit()
                finally:
                    connection.close()

            configuration = {"adminPasswordHash": "old-hash", "port": 8790}
            with patch("server.ROOT", root), patch("server.DATABASE", database), patch("server.OBJECTS", root / "objects" / "sha256"), \
                patch("server.CONFIG_PATH", config_path), patch("server.CONFIG", configuration.copy()), \
                patch("server.PASSWORD_HASH", "old-hash"), patch("server.password_matches", return_value=True), \
                patch("server.db", open_database), patch.dict(os.environ, {"REPO_ADMIN_PASSWORD_HASH": ""}):
                server.initialize()
                with open_database() as connection:
                    connection.execute("INSERT INTO admin_sessions(token_hash,csrf_token,expires_at,created_at) VALUES(?,?,?,?)",
                        (hashlib.sha256(token.encode()).hexdigest(), "password-csrf", int(time.time()) + 600, int(time.time())))
                http_server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                thread = Thread(target=http_server.serve_forever, daemon=True)
                thread.start()
                try:
                    payload = json.dumps({"currentPassword": "old-password", "newPassword": "new-secure-password", "confirmPassword": "new-secure-password"}).encode()
                    request = Request(f"http://127.0.0.1:{http_server.server_port}/api/admin/system/password", data=payload,
                        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"}, method="POST")
                    with patch.dict(os.environ, {"REPO_ADMIN_PASSWORD_HASH": "environment-managed-hash"}):
                        with self.assertRaises(urllib.error.HTTPError) as managed_password:
                            urlopen(request)
                        self.assertEqual(managed_password.exception.code, 409)
                    with urlopen(request) as response:
                        self.assertEqual(json.loads(response.read())["revokedSessions"], 1)
                    saved = json.loads(config_path.read_text(encoding="utf-8"))
                    self.assertNotEqual(saved["adminPasswordHash"], "old-hash")
                    self.assertEqual(saved["port"], 8790)
                    with open_database() as connection:
                        self.assertEqual(connection.execute("SELECT COUNT(*) FROM admin_sessions").fetchone()[0], 0)
                finally:
                    http_server.shutdown()
                    http_server.server_close()
                    thread.join()

    def test_failed_backup_removes_staging_directory(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            database = root / "repository.sqlite3"
            database.write_bytes(b"not a sqlite database")
            objects = root / "objects" / "sha256"
            objects.mkdir(parents=True)
            backups = root / "backups"
            with self.assertRaises(sqlite3.DatabaseError):
                create_backup(database, objects, root, backups)
            self.assertFalse(any(backups.iterdir()))


class BlobMaintenanceTests(unittest.TestCase):
    def test_scheduled_gc_runs_after_configured_interval(self):
        class StopAfterOneRun:
            calls = 0

            def wait(self, seconds):
                self.calls += 1
                self.asserted_interval = seconds
                return self.calls > 1

        stop_event = StopAfterOneRun()
        with patch("server.GC_SCHEDULE_SECONDS", 60), patch("server.start_gc_job") as start_job:
            server.scheduled_gc(stop_event)
        start_job.assert_called_once_with("scheduler", "localhost")
        self.assertEqual(stop_event.asserted_interval, 60)

    def test_expired_admin_data_is_pruned_by_periodic_maintenance(self):
        class StopAfterOneRun:
            calls = 0

            def wait(self, seconds):
                self.calls += 1
                self.asserted_interval = seconds
                return self.calls > 1

        stop_event = StopAfterOneRun()
        with patch("server.SESSION_CLEANUP_SECONDS", 60), patch("server.GC_SCHEDULE_SECONDS", 120), patch(
            "server.cleanup_expired_sessions"
        ) as cleanup:
            server.scheduled_gc(stop_event)
        cleanup.assert_called_once_with()
        self.assertEqual(stop_event.asserted_interval, 60)

    def test_cleanup_removes_only_expired_sessions_and_old_login_limits(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            database = Path(temporary_directory) / "repository.sqlite3"

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
                try:
                    yield connection
                    connection.commit()
                finally:
                    connection.close()

            now = 2_000_000_000
            with open_database() as connection:
                connection.executescript(
                    "CREATE TABLE admin_sessions(token_hash TEXT, expires_at INTEGER);"
                    "CREATE TABLE admin_login_limits(remote TEXT, updated_at INTEGER);"
                )
                connection.executemany("INSERT INTO admin_sessions VALUES(?, ?)", (("expired", now - 1), ("active", now + 1)))
                month = 30 * 24 * 60 * 60
                connection.executemany("INSERT INTO admin_login_limits VALUES(?, ?)", (("old", now - month - 1), ("recent", now - month)))

            with patch("server.db", open_database):
                removed = server.cleanup_expired_sessions(now)

            self.assertEqual(removed, {"sessions": 1, "loginLimits": 1})
            with open_database() as connection:
                self.assertEqual(connection.execute("SELECT token_hash FROM admin_sessions").fetchall(), [("active",)])
                self.assertEqual(connection.execute("SELECT remote FROM admin_login_limits").fetchall(), [("recent",)])

    def test_startup_cleanup_removes_only_stale_uploads(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            stale = root / "upload-stale.tmp"
            fresh = root / "upload-fresh.tmp"
            stale.write_bytes(b"old")
            fresh.write_bytes(b"new")
            old_timestamp = time.time() - 100
            os.utime(stale, (old_timestamp, old_timestamp))
            with patch("server.ROOT", root), patch("server.STALE_UPLOAD_SECONDS", 50):
                removed = server.cleanup_stale_uploads()
            self.assertEqual(removed, 1)
            self.assertFalse(stale.exists())
            self.assertTrue(fresh.exists())

    def test_gc_preserves_blobs_referenced_by_drafts_and_removes_old_orphans(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            database = root / "repository.sqlite3"
            objects = root / "objects" / "sha256"

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
                connection.row_factory = sqlite3.Row
                connection.execute("PRAGMA foreign_keys=ON")
                try:
                    yield connection
                    connection.commit()
                finally:
                    connection.close()

            with patch("server.ROOT", root), patch("server.DATABASE", database), patch(
                "server.OBJECTS", objects
            ), patch("server.db", open_database):
                from server import initialize

                initialize()
                live_hash, orphan_hash = "a" * 64, "b" * 64
                old_time = int(time.time()) - 60 * 24 * 60 * 60
                for digest in (live_hash, orphan_hash):
                    target = objects / digest[:2] / digest
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(b"blob")
                with open_database() as connection:
                    connection.execute("INSERT INTO packs(id,title,created_at,updated_at) VALUES('test-pack','Test',?,?)", (old_time, old_time))
                    connection.executemany("INSERT INTO blobs(hash,size,created_at) VALUES(?,?,?)", ((live_hash, 4, old_time), (orphan_hash, 4, old_time)))
                    manifest = json.dumps({"files": [{"hash": f"sha256:{live_hash}"}]})
                    connection.execute("INSERT INTO releases(pack_id,version,manifest,created_at,updated_at) VALUES('test-pack','1.0.0',?,?,?)", (manifest, old_time, old_time))

                preview = collect_unreferenced_blobs(grace_seconds=0, dry_run=True)
                self.assertEqual(preview["candidates"], [{"hash": orphan_hash, "size": 4}])
                self.assertEqual(preview["bytes"], 4)
                with open_database() as connection:
                    self.assertEqual(connection.execute("SELECT COUNT(*) FROM blobs").fetchone()[0], 2)
                self.assertTrue((objects / orphan_hash[:2] / orphan_hash).is_file())

                collect_unreferenced_blobs(grace_seconds=0)

                with open_database() as connection:
                    hashes = {row[0] for row in connection.execute("SELECT hash FROM blobs")}
                self.assertEqual(hashes, {live_hash})
                self.assertTrue((objects / live_hash[:2] / live_hash).is_file())
                self.assertFalse((objects / orphan_hash[:2] / orphan_hash).exists())

    def test_gc_restores_blob_when_database_transaction_fails(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            database = root / "repository.sqlite3"
            objects = root / "objects" / "sha256"

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
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

            with patch("server.ROOT", root), patch("server.DATABASE", database), patch(
                "server.OBJECTS", objects
            ), patch("server.db", open_database):
                server.initialize()
                digest = "c" * 64
                target = objects / digest[:2] / digest
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(b"orphan")
                with open_database() as connection:
                    connection.execute("INSERT INTO blobs(hash,size,created_at) VALUES(?,?,?)", (digest, 6, int(time.time()) - 100))
                with patch("server.prune_audit", side_effect=sqlite3.OperationalError("simulated transaction failure")):
                    with self.assertRaises(sqlite3.OperationalError):
                        collect_unreferenced_blobs(grace_seconds=0)
                with open_database() as connection:
                    self.assertIsNotNone(connection.execute("SELECT 1 FROM blobs WHERE hash=?", (digest,)).fetchone())
                self.assertEqual(target.read_bytes(), b"orphan")
                self.assertFalse(target.with_name(f"{digest}.gc").exists())

    def test_gc_recovery_restores_referenced_tombstone_and_removes_unreferenced_one(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            database = root / "repository.sqlite3"
            objects = root / "objects" / "sha256"

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
                connection.row_factory = sqlite3.Row
                try:
                    yield connection
                    connection.commit()
                finally:
                    connection.close()

            with patch("server.ROOT", root), patch("server.DATABASE", database), patch(
                "server.OBJECTS", objects
            ), patch("server.db", open_database):
                server.initialize()
                kept_hash, removed_hash = "d" * 64, "e" * 64
                for digest in (kept_hash, removed_hash):
                    path = objects / digest[:2] / f"{digest}.gc"
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(digest.encode())
                with open_database() as connection:
                    connection.execute("INSERT INTO blobs(hash,size,created_at) VALUES(?,?,?)", (kept_hash, 64, int(time.time())))
                result = server.recover_gc_tombstones()
                self.assertEqual(result, {"restored": 1, "removed": 1})
                self.assertEqual((objects / kept_hash[:2] / kept_hash).read_bytes(), kept_hash.encode())
                self.assertFalse((objects / removed_hash[:2] / f"{removed_hash}.gc").exists())

    def test_invalid_manifest_reports_gc_warning_without_deleting_blobs(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            database = root / "repository.sqlite3"
            objects = root / "objects" / "sha256"

            @contextmanager
            def open_database():
                connection = sqlite3.connect(database)
                connection.row_factory = sqlite3.Row
                connection.execute("PRAGMA foreign_keys=ON")
                try:
                    yield connection
                    connection.commit()
                finally:
                    connection.close()

            with patch("server.ROOT", root), patch("server.DATABASE", database), patch(
                "server.OBJECTS", objects
            ), patch("server.db", open_database):
                from server import initialize

                initialize()
                digest = "e" * 64
                target = objects / digest[:2] / digest
                target.parent.mkdir(parents=True)
                target.write_bytes(b"orphan?")
                now = int(time.time()) - 90 * 24 * 60 * 60
                with open_database() as connection:
                    connection.execute("INSERT INTO packs(id,title,created_at,updated_at) VALUES('broken-pack','Broken',?,?)", (now, now))
                    connection.execute("INSERT INTO blobs(hash,size,created_at) VALUES(?,?,?)", (digest, 7, now))
                    connection.execute("INSERT INTO releases(pack_id,version,manifest,created_at,updated_at) VALUES('broken-pack','1.0.0','{broken',?,?)", (now, now))

                result = collect_unreferenced_blobs(grace_seconds=0)

                self.assertEqual(result["deleted"], 0)
                self.assertTrue(result["warnings"])
                with open_database() as connection:
                    self.assertIsNotNone(connection.execute("SELECT 1 FROM blobs WHERE hash=?", (digest,)).fetchone())
                self.assertTrue(target.is_file())


if __name__ == "__main__":
    unittest.main()