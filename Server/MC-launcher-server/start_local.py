import getpass
import json
import os
import runpy
from pathlib import Path

from make_password import password_hash, valid_password


password = getpass.getpass("Пароль администратора (минимум 8 символов): ")
confirmation = getpass.getpass("Повторите пароль: ")
if not valid_password(password, confirmation):
    raise SystemExit("Пароли должны совпадать и содержать не менее 8 символов.")

config_path = Path(os.environ.get("REPO_CONFIG_FILE", Path(__file__).with_name("server_config.json"))).resolve()
try:
    configuration = json.loads(config_path.read_text(encoding="utf-8"))
except FileNotFoundError:
    configuration = {}
if not isinstance(configuration, dict):
    raise SystemExit(f"Конфигурация {config_path} должна быть JSON-объектом")
configuration["adminPasswordHash"] = password_hash(password)
configuration.setdefault("host", os.environ.get("REPO_HOST", "127.0.0.1"))
configuration.setdefault("port", int(os.environ.get("REPO_PORT", "8787")))
configuration.setdefault("dataDir", os.environ.get("REPO_DATA_DIR", "data"))
config_path.parent.mkdir(parents=True, exist_ok=True)
config_path.write_text(json.dumps(configuration, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
os.environ["REPO_CONFIG_FILE"] = str(config_path)
os.environ["REPO_ADMIN_PASSWORD_HASH"] = configuration["adminPasswordHash"]
os.environ.setdefault("REPO_HOST", str(configuration["host"]))
os.environ.setdefault("REPO_PORT", str(configuration["port"]))
os.environ.setdefault("REPO_DATA_DIR", str(configuration["dataDir"]))
runpy.run_module("server", run_name="__main__")