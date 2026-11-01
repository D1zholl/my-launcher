# MyLauncher Build Repository

## Варианты администрирования

1. **Веб-панель на сервере (рекомендуется).** Панель уже входит в этот комплект и раздаётся сервером на `/`. Не требует установки на рабочие станции, все операции и аудит остаются рядом с SQLite и файлами. Обновляется вместе с сервером.
2. **Desktop-приложение Tauri 2 + React + TypeScript.** Удобно для офлайн-работы и интеграции с локальными файлами, но требует отдельной сборки, раздачи обновлений, установки и управления токенами на каждом компьютере администратора. Для одного Windows-сервера это лишняя эксплуатационная поверхность. При необходимости его можно добавить позже поверх тех же `/api/admin` endpoints.

## Архитектура

- `server.py` использует Python 3.11+ и стандартную библиотеку (`http.server`, `sqlite3`, `hashlib`, `scrypt`). Дополнительные Python-пакеты не нужны.
- Установщик по умолчанию задаёт `dataDir: "data"`; путь разрешается относительно `server_config.json`, поэтому база и файлы хранятся в `MC-launcher-server/data` и переносятся вместе с папкой сервера. Абсолютный `REPO_DATA_DIR` или `dataDir` по-прежнему поддерживается для отдельного хранилища.
- Production HTTP-сервер ограничен 64 worker-потоками и backlog 128; при перегрузке возвращает `503` с `Retry-After`, а не создаёт неограниченные потоки. Неактивное соединение закрывается после 30 секунд. Один `dataDir` защищён межпроцессным OS-lock-файлом `server.lock`; второй сервер завершится с PID владельца.
- SQLite хранит сборки, версии-манифесты, сведения о blob-файлах и журнал административных действий. Файлы располагаются в `data/objects/sha256/<первые 2 символа>/<sha256>`; повторная загрузка тех же байтов занимает место только один раз. GC запускается в фоне после старта и далее раз в сутки; ручной GC сначала показывает кандидатов и удаляет только файлы старше 72 часов через восстанавливаемые `.gc` tombstones. При старте незавершённые tombstones восстанавливаются или дочищаются по состоянию SQLite; старые upload-файлы удаляются.
- Ответы официальных API Minecraft/Fabric/Forge/NeoForge сохраняются в `data/metadata-cache`; учитываются `Cache-Control`, `ETag` и `Last-Modified`, а при временной ошибке до суток используется последний корректный ответ. Запросы ограничены таймаутом и тремя попытками. При первом запуске серверу нужен исходящий HTTPS-доступ к источникам.
- Панель из соседней папки `MC-launcher-adminpanel` обслуживается тем же процессом. Для единого происхождения страницы нет CORS и отдельной веб-службы.
- Адрес прослушивания задаётся `REPO_HOST` или `host` в `server_config.json`; код без настройки использует `127.0.0.1`, но текущий пример конфигурации в репозитории задаёт `0.0.0.0`. Перед запуском за reverse proxy явно установите `host` в `127.0.0.1` и проверьте, что порт 8787 не доступен извне. Caddy принимает интернет-трафик на 80/443, выдаёт и автоматически продлевает TLS-сертификаты, затем проксирует запросы на localhost.
- Версии релизов обязаны соответствовать SemVer. Опубликованный манифест неизменяем, пока версия опубликована. Unpublish возвращает её в черновик: после этого версию можно редактировать или удалить.
- Stable и beta доступны через клиент. Beta-only сборки видны в каталоге `?channel=beta`; бета-версии публичны, не являются приватными.

### Формат версии 1

Эталонная JSON Schema: [`manifest.schema.json`](manifest.schema.json). Пример:

```json
{
  "manifestVersion": 1,
  "packId": "adventure-pack",
  "version": "1.2.0",
  "name": "Adventure Pack",
  "description": "Пример пользовательской сборки",
  "authors": ["Team"],
  "categories": ["adventure", "fabric"],
  "iconUrl": null,
  "minecraft": { "version": "1.21.1", "loader": "fabric", "loaderVersion": "0.16.9" },
  "java": { "major": 25 },
  "files": [
    { "path": "mods/example.jar", "hash": "sha256:<64 lowercase hex>", "size": 123456, "url": "/api/files/sha256:<64 lowercase hex>", "required": true }
  ],
  "settings": { "memoryMb": 4096, "jvmArgs": "", "windowWidth": 1280, "windowHeight": 720, "fullscreen": false }
}
```

Лаунчер сам устанавливает Minecraft, loader и требуемую Java из Temurin; major берётся из официальных метаданных выбранной игры. Поддерживаются Java 8, 11, 16, 17, 21 и 25. Манифест не содержит клиент игры и библиотеки. Файлы JAR исполняют код внутри игры: загружайте только доверенный контент, даже если его хэш и размер проверены.

## API

Публичные GET endpoints:

- `/api/health` — readiness; возвращает `503`, если недоступна БД, хранилище не writable, версия схемы неизвестна или свободного места меньше `minimumFreeBytes` (по умолчанию 100 MiB)
- `/api/live` — liveness процесса, без проверки зависимостей; оба health-маршрута поддерживают `HEAD`
- `/api/packs` — stable-сборки, отсортированные по SemVer
- `/api/packs?channel=beta` — beta-сборки
- `/api/packs/{packId}` и `/api/packs/{packId}/versions`
- `/api/packs/{packId}/versions?channel=beta` — версии beta-канала
- `/api/packs/{packId}/versions/{version}/manifest`
- `/api/packs/{packId}/versions/{version}/manifest?channel=beta`
- `/api/files/sha256:{hex}` — файл с ETag, `HEAD` и одиночным byte `Range`

Админские endpoints используют защищённую cookie-сессию (`repo-session`, `Path=/api/admin`); Bearer-токен поддерживается для API-клиентов. Изменяющие запросы с cookie требуют CSRF-токен:

- `POST /api/admin/login` `{ "password": "..." }`
- `GET /api/admin/dashboard` — сводки релизов, статистика, последние 50 действий; для постраничной загрузки релизов используйте `?page=1&perPage=100` (максимум 200), ответ содержит `releasePagination` и агрегированный `releaseSummary`; без параметров сохраняется полный список для совместимости
- `GET /api/admin/stats?days=30&page=1&perPage=50` — дневные агрегаты по сборке, версии и каналу без потери строк; ответ содержит `total/pages`
- `GET /api/admin/audit?page=1&perPage=50` — журнал; поддерживает фильтры `action`, `actor`, `result=success|failure`, `from=YYYY-MM-DD`, `to=YYYY-MM-DD`, `search` и CSV export
- `GET /api/admin/releases/{releaseId}` — полный манифест выбранного релиза
- `GET /api/admin/options/minecraft` — официальный Mojang manifest: релизы, снапшоты и legacy-типы с датой публикации и URL JSON версии
- `GET /api/admin/options/java?gameVersion=...` — required Java major из метаданных выбранной версии игры
- `GET /api/admin/options/fabric?resource=...` — официальный Fabric Meta ресурс (`game`, `loader`, `installer`, `yarn`, `intermediary`, `compatible-loader`, `loader-details`, client/server profile, Yarn/Intermediary выбранной игры); ZIP-профиль и server JAR возвращаются официальным URL без проксирования binary body
- `GET /api/admin/options/loaders?gameVersion=...&loader=...` — совместимые версии и унифицированные `items` с stable/latest/recommended, official profile/metadata/installer URL и датой, если она опубликована; поле `versions` оставлено для совместимости старой панели
- `GET /api/admin/options/loaders?gameVersion=...&loader=...&refresh=1` — принудительная проверка источников с условными HTTP-запросами; при обычном refresh-кэше используются валидаторы upstream
- `POST /api/admin/files?name=...` — raw body, лимит по умолчанию 8 GiB (можно уменьшить через `REPO_MAX_UPLOAD_BYTES`); перед чтением тела резервируется место с остаточным запасом `minimumFreeBytes`, при нехватке возвращается `507`; ответ содержит hash/size/duplicate/url
- `POST /api/admin/packs`, `PUT /api/admin/packs/{packId}`
- `POST /api/admin/releases`, `PUT` для манифеста черновика и `PATCH /api/admin/releases/{releaseId}` для частичных изменений
- `PATCH /api/admin/releases/{releaseId}` с `{ "channel": "beta" }` меняет канал только у неопубликованного draft; требуется `If-Match` из ETag ответа `GET`, успешный ответ содержит новый ETag
- `POST /api/admin/releases/{releaseId}/clone` — новый draft с копией манифеста, файлами и настройками без копирования blob
- `POST /api/admin/releases/{releaseId}/archive` и `/restore`
- `POST /api/admin/releases/{releaseId}/validate` — сохраняет результат проверки, timestamp и hash манифеста; ответ содержит `ready`, `validatedAt` и `validationHash`
- `POST /api/admin/releases/{releaseId}/publish` требует успешную проверку не старше 15 минут и совпадающий hash текущего манифеста; `/unpublish` снимает публикацию
- `POST /api/admin/gc?dryRun=true` — список кандидатов и суммарный размер; `POST /api/admin/gc` запускает фоновую очистку, `GET /api/admin/gc` возвращает её статус и отчёт
- `GET /api/admin/system` и `POST /api/admin/system/diagnose` — runtime, SQLite/WAL, свободное место, blob storage, metadata cache, активные задачи и последние ошибки
- `GET /api/admin/backups`, `POST /api/admin/backups` — история backup и запуск фонового snapshot; `POST /api/admin/backups/{id}/verify` проверяет SHA-256 всех файлов и `PRAGMA integrity_check`
- `POST /api/admin/backups/{id}/restore` — проверяет snapshot, планирует restore, штатно останавливает процесс; при следующем запуске под data lock создаёт safety-backup текущих данных, восстанавливает проверенную копию и отзывает все сессии из неё
- `POST /api/admin/system/sessions/revoke` завершает все административные сессии; `POST /api/admin/system/password` меняет пароль в конфигурации и отзывает сессии, но отключён, когда пароль задан через `REPO_ADMIN_PASSWORD_HASH`
- `DELETE /api/admin/releases/{releaseId}` (только версия, которая никогда не публиковалась)
- `DELETE /api/admin/packs/{packId}` (только если версии удалены)

Манифесты ограничены 64 MiB, что покрывает лимит в 100 000 файлов. Черновики могут быть неполными: `PUT` проверяет структуру и безопасность данных, а полный набор ошибок возвращает endpoint `validate` и блокирует публикацию. Серверный валидатор согласован со схемой по поддерживаемым Java и дополнительным полям. Результаты readiness хранятся в добавочной таблице `release_readiness`; при запуске таблица создаётся идемпотентно без пересоздания существующих таблиц или изменения записей. Dashboard читает сохранённые результаты readiness и не валидирует все манифесты при каждом запросе. Скачивания агрегируются как уникальные пары visitor/blob за UTC-день (не как установки); события ставятся в ограниченную очередь и записываются пакетно фоновым потоком, не задерживая выдачу blob. Для опубликованных релизов клиент передаёт pack/version/channel, которые сервер проверяет по манифесту. Учитываются и изображения. HMAC-ключ хранится в `data/download-visitor-secret` и не меняется при смене пароля администратора. Ограничение перебора: 8 неудачных паролей блокируют IP на 15 минут; после истечения блокировки счетчик начинается заново. Сессия живёт 8 часов и хранится в SQLite, поэтому переживает перезапуск процесса. Все admin операции и неудачные попытки входа попадают в таблицу `audit`. Проверка readiness пытается получить актуальные официальные метаданные и использует кэш не старше суток при сбое upstream; если совместимость подтвердить нельзя, проверка и публикация завершаются ошибкой. Для NeoForge 1.20.1 используется отдельный официальный `net/neoforged/forge` каталог с точным Minecraft component в Maven version coordinate; modern NeoForge фильтруется по точному `net.minecraft` requirement из Prism metadata, которая не подменяет Maven список. Сервер публикует installer/profile URL для дальнейшей установки, но сам installer JAR не устанавливает.

## Windows 11: установка и запуск

1. Создайте DNS A-запись домена на публичный IPv4. На роутере пробросьте TCP 80 и 443 на ПК сервера. Не открывайте порт 8787 наружу.
2. Установите Python 3.12 x64 с python.org, включите Python Launcher. Caddy скачайте с официальной страницы `caddyserver.com/download` (стандартная сборка достаточна). Для службы установите NSSM с официального сайта проекта.
3. Скопируйте папки `MC-launcher-server` и `MC-launcher-adminpanel` в постоянный путь, например `C:\MyLauncher`. Установщик должен сохранить соседнее расположение каталогов.
4. Для службы создайте отдельного локального пользователя Windows без прав администратора. Дайте ему Modify на каталог данных и Read/Execute на файлы приложения. Храните только salted scrypt-хэш `REPO_ADMIN_PASSWORD_HASH`, не исходный пароль.
5. При первой настройке откройте PowerShell в `MC-launcher-server` и выполните:

```powershell
py -3.12 .\start_local.py
```

6. До первого запуска откройте `server_config.json` и задайте `host` как `127.0.0.1`, если доступ будет идти через reverse proxy или только с этого компьютера. Значение `0.0.0.0` прослушивает все сетевые интерфейсы; не оставляйте его без осознанной необходимости и сетевых ограничений. Helper попросит ввести пароль от 8 символов и повторить его, сохранит salted scrypt-хэш, адрес, порт и каталог данных в `server_config.json`, затем запустит сервер. Этот файл хранит только хэш, не пароль. При необходимости измените в нём `port` и `dataDir`. Проверьте `http://127.0.0.1:8787/api/health`: ожидается JSON `{"ok":true,"manifestVersion":1}`. Откройте `http://127.0.0.1:8787/` и войдите заданным паролем. После первоначальной настройки и при последующих запусках используйте напрямую:

```powershell
py -3.12 .\server.py
```

  Для постоянной работы настройте NSSM: Application = `C:\Program Files\Python312\python.exe`, Arguments = `C:\MyLauncher\MC-launcher-server\server.py`, Startup directory = `C:\MyLauncher\MC-launcher-server`. Переменные `REPO_HOST`, `REPO_PORT`, `REPO_DATA_DIR` и `REPO_CONFIG_FILE` можно задать в Environment; хэш пароля берётся из конфигурационного файла. Укажите service account, Automatic и stdout/stderr с ротацией, затем запустите `nssm start MyLauncherRepository`.
7. Создайте `C:\MyLauncher\Caddyfile`:

```caddyfile
packs.example.com {
    encode zstd gzip
  header {
    Strict-Transport-Security "max-age=31536000; includeSubDomains"
    Referrer-Policy "no-referrer"
    X-Content-Type-Options "nosniff"
    X-Frame-Options "DENY"
  }
    reverse_proxy 127.0.0.1:8787
}
```

Запустите Caddy один раз из консоли `caddy run --config C:\MyLauncher\Caddyfile`; Caddy автоматически получит и продлит сертификат Let's Encrypt, если DNS и входящие порты доступны. После проверки зарегистрируйте Caddy службой через `nssm install MyLauncherCaddy` (Application: путь к `caddy.exe`, Arguments: `run --config C:\MyLauncher\Caddyfile`, Startup directory: каталог Caddy) либо используйте официальный Windows service wrapper Caddy. Разрешите входящие TCP 80/443 в Windows Firewall. Убедитесь, что домен открывается по HTTPS и имеет актуальный сертификат.
8. В лаунчере откройте «Собственные сборки» и задайте `https://packs.example.com`. Не используйте HTTP: клиент специально принимает только HTTPS.

## Резервное копирование и обновления

- Перед обновлением или изменениями хранилища выполните `python .\backup.py` из каталога сервера. Скрипт использует SQLite online backup API, проверяет копию через `PRAGMA integrity_check`, копирует content-addressed objects и секрет статистики во временную папку, записывает `manifest.json` с размерами и SHA-256, затем атомарно публикует snapshot. По умолчанию хранятся последние 14 копий; настройте число через `REPO_BACKUP_RETENTION`. Для production задайте `REPO_BACKUP_DIR` на отдельный диск/NAS и настройте ежедневный запуск через Task Scheduler; регулярно переносите копию за пределы сервера.
- Restore из админ-панели всегда выполняется offline: процесс завершится, supervisor/Task Scheduler должен перезапустить его; восстановление произойдёт при старте под single-instance lock. Не запускайте второй процесс вручную во время перезапуска. При ошибке исходные файлы откатываются, а причина остаётся на странице «Система» после следующего запуска.
- Проверяйте восстановление на отдельной копии: остановите службу, восстановите `repository.sqlite3` и каталог `objects`, затем запустите тестовый экземпляр с отдельным портом/путём данных.
- Для обновления остановите серверную службу, сделайте backup, замените код `server.py` и панель на проверенную версию, затем запустите службу и проверьте `/api/health` и вход в `/`. Схема создаётся идемпотентно при старте; не удаляйте `data` при обновлении.
- Логи: NSSM stdout/stderr (`C:\MyLauncher\logs`), журналы Windows Services и таблица `audit`; SQLite можно читать локальной утилитой только после создания резервной копии либо открывать read-only.
- Настройте очистку старых backup-каталогов с retention, но не удаляйте отдельные blobs вручную: immutable версии могут ссылаться на них. Удаление недостижимых blobs/GC намеренно не входит в первый релиз.

## Безопасность и границы

- Внешний HTTPS завершается в Caddy. Убедитесь, что эффективное значение `REPO_HOST`/`host` равно `127.0.0.1`, если backend должен быть доступен только через локальный reverse proxy; конфигурация с `0.0.0.0` слушает все интерфейсы. Не публикуйте напрямую 8787.
- Пароль администратора хранится только как salted scrypt-хэш в конфигурации. Cookie/Bearer-сессии случайные, живут 8 часов и хранятся в SQLite, поэтому переживают перезапуск; session-cookie отправляется только на `/api/admin`.
- Upload разрешает известные игровые и конфигурационные типы, включая `.bak`, `.backup`, `.json5`, `.snbt`, `.md` и `.defaults`; размер одного файла ограничен 8 GiB. Архивы никогда не распаковываются на сервере.
- Загрузка потоковая, ограничена размером и сохраняется только под SHA-256. Путь из манифеста POSIX-формата, абсолютные пути, `..`, обратная косая черта, управляющие символы, повторяющиеся пути и служебные файлы launcher запрещаются на клиенте.
- Сервер проверяет структуру, хэш и размер до публикации; лаунчер повторно проверяет размер и SHA-256 до установки. TLS обязателен, редиректы клиента отключены.
- Не запускайте службу от администратора. Ограничьте ACL каталога данных и службы, регулярно обновляйте Windows/Python/Caddy, ротируйте пароль при подозрении на утечку.
- Хэши подтверждают целостность, не безопасность содержимого. Моды JAR являются исполняемым кодом; доверяйте авторам и проверяйте загрузки антивирусом на административной машине. Один admin пароль и beta-канал не обеспечивают multi-user RBAC или приватные приглашения.

## Ограничения первой версии

Встроенный `http.server` рассчитан на небольшой личный репозиторий с умеренным числом одновременных загрузок. Это не CDN, не отказоустойчивый сервис и не многопользовательская CMS. Для большой аудитории оставьте API/SQLite, но перенесите blob delivery за Caddy либо на объектное хранилище/CDN; админ API при этом держите за дополнительным VPN/allowlist.