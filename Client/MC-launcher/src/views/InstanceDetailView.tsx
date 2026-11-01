import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  ArrowLeft,
  AlertTriangle,
  Blocks,
  Check,
  Clock3,
  Download,
  File,
  Folder,
  FolderOpen,
  Gamepad2,
  HardDrive,
  LoaderCircle,
  PackageSearch,
  Power,
  RefreshCw,
  Search,
  Settings2,
  Square,
  Trash2,
  UsersRound,
  ScrollText,
  Globe2,
  X,
} from "lucide-react";
import { listInstanceWorlds, listModVersions } from "../lib/mods";
import { avatarSource } from "../lib/common";
import type { GameLog, Instance, InstanceDetailTab, InstalledMod, ModVersionOption } from "../types";

interface InstanceFileEntry {
  name: string;
  isDirectory: boolean;
  sizeBytes: number;
  modifiedUnixSeconds: number | null;
}

interface InstanceDetailViewProps {
  instance: Instance;
  tab: InstanceDetailTab;
  installedMods: InstalledMod[];
  gameLogs: GameLog[];
  isRunning: boolean;
  busy: boolean;
  hasActiveAccount: boolean;
  onChangeTab: (tab: InstanceDetailTab) => void;
  onBack: () => void;
  onOpenDirectory: () => void;
  onOpenCatalog: () => void;
  onEdit: () => void;
  onDelete: () => void;
  onLaunch: () => void;
  onStop: () => void;
  onToggleMod: (mod: InstalledMod) => void;
  onUpdateMod: (mod: InstalledMod, versionId?: string) => void;
  onRemoveMod: (mod: InstalledMod) => void;
}

const detailTabs: { id: InstanceDetailTab; label: string; icon: typeof Blocks }[] = [
  { id: "content", label: "Контент", icon: Blocks },
  { id: "files", label: "Файлы", icon: Folder },
  { id: "worlds", label: "Миры", icon: Globe2 },
  { id: "logs", label: "Логи", icon: ScrollText },
];

function formatFileSize(size: number): string {
  if (size < 1024) return `${size} Б`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} КБ`;
  if (size < 1024 * 1024 * 1024) return `${(size / (1024 * 1024)).toFixed(1)} МБ`;
  return `${(size / (1024 * 1024 * 1024)).toFixed(1)} ГБ`;
}

function formatModifiedTime(seconds: number | null): string {
  if (seconds === null) return "Дата неизвестна";
  return new Intl.DateTimeFormat("ru-RU", { dateStyle: "medium", timeStyle: "short" })
    .format(new Date(seconds * 1000));
}

export function InstanceDetailView({
  instance,
  tab,
  installedMods,
  gameLogs,
  isRunning,
  busy,
  hasActiveAccount,
  onChangeTab,
  onBack,
  onOpenDirectory,
  onOpenCatalog,
  onEdit,
  onDelete,
  onLaunch,
  onStop,
  onToggleMod,
  onUpdateMod,
  onRemoveMod,
}: InstanceDetailViewProps) {
  const [files, setFiles] = useState<InstanceFileEntry[]>([]);
  const [filesLoading, setFilesLoading] = useState(false);
  const [filesError, setFilesError] = useState("");
  const [worlds, setWorlds] = useState<string[]>([]);
  const [worldsLoading, setWorldsLoading] = useState(false);
  const [worldsError, setWorldsError] = useState("");
  const [contentQuery, setContentQuery] = useState("");
  const [versionsByProject, setVersionsByProject] = useState<Record<string, ModVersionOption[]>>({});
  const [loadingVersionsFor, setLoadingVersionsFor] = useState<string[]>([]);
  const [versionErrors, setVersionErrors] = useState<Record<string, string>>({});
  const [versionPickerMod, setVersionPickerMod] = useState<InstalledMod | null>(null);
  const [versionSearch, setVersionSearch] = useState("");
  const [selectedVersionId, setSelectedVersionId] = useState("");
  const [failedIcons, setFailedIcons] = useState<Set<string>>(() => new Set());

  useEffect(() => {
    if (tab !== "files") return;
    let active = true;
    setFilesLoading(true);
    setFilesError("");
    invoke<InstanceFileEntry[]>("list_instance_files", { folderName: instance.folder_name })
      .then((items) => {
        if (active) setFiles(items);
      })
      .catch((error: unknown) => {
        if (active) setFilesError(error instanceof Error ? error.message : String(error));
      })
      .finally(() => {
        if (active) setFilesLoading(false);
      });
    return () => {
      active = false;
    };
  }, [instance.folder_name, tab]);

  useEffect(() => {
    if (tab !== "worlds") return;
    let active = true;
    setWorldsLoading(true);
    setWorldsError("");
    listInstanceWorlds(instance)
      .then((items) => {
        if (active) setWorlds(items);
      })
      .catch((error: unknown) => {
        if (active) setWorldsError(error instanceof Error ? error.message : String(error));
      })
      .finally(() => {
        if (active) setWorldsLoading(false);
      });
    return () => {
      active = false;
    };
  }, [instance, tab]);

  useEffect(() => {
    if (!versionPickerMod?.projectId) return;
    const versions = versionsByProject[versionPickerMod.projectId];
    if (!versions?.length) return;
    setSelectedVersionId((current) => {
      if (versions.some((version) => version.versionId === current)) return current;
      if (versionPickerMod.versionId && versions.some((version) => version.versionId === versionPickerMod.versionId)) {
        return versionPickerMod.versionId;
      }
      return versions[0].versionId;
    });
  }, [versionPickerMod, versionsByProject]);

  useEffect(() => {
    if (!versionPickerMod) return;
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setVersionPickerMod(null);
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [versionPickerMod]);

  const query = contentQuery.trim().toLowerCase();
  const filteredMods = query
    ? installedMods.filter((mod) => mod.fileName.toLowerCase().includes(query))
    : installedMods;
  const loaderName = instance.loader_type === "vanilla"
    ? "Vanilla"
    : `${instance.loader_type === "neoforge" ? "NeoForge" : instance.loader_type[0].toUpperCase() + instance.loader_type.slice(1)}${instance.loader_version ? ` ${instance.loader_version}` : ""}`;
  const blockedByOtherGame = busy && !isRunning;
  const visibleLogs = gameLogs.filter((log) => log.folderName === instance.folder_name);
  const selectedProjectVersions = versionPickerMod?.projectId
    ? versionsByProject[versionPickerMod.projectId] ?? []
    : [];
  const filteredProjectVersions = selectedProjectVersions.filter((version) =>
    version.versionNumber.toLowerCase().includes(versionSearch.trim().toLowerCase()),
  );

  async function loadModVersions(mod: InstalledMod) {
    if (!mod.projectId || versionsByProject[mod.projectId] || loadingVersionsFor.includes(mod.projectId)) return;
    setLoadingVersionsFor((current) => [...current, mod.projectId!]);
    setVersionErrors((current) => ({ ...current, [mod.projectId!]: "" }));
    try {
      const versions = await listModVersions(instance, mod);
      setVersionsByProject((current) => ({ ...current, [mod.projectId!]: versions }));
    } catch (error) {
      setVersionErrors((current) => ({
        ...current,
        [mod.projectId!]: error instanceof Error ? error.message : String(error),
      }));
    } finally {
      setLoadingVersionsFor((current) => current.filter((projectId) => projectId !== mod.projectId));
    }
  }

  function openVersionPicker(mod: InstalledMod) {
    setVersionPickerMod(mod);
    setVersionSearch("");
    setSelectedVersionId(mod.versionId ?? "");
    void loadModVersions(mod);
  }

  return (
    <>
      <button className="detail-back" type="button" onClick={onBack}><ArrowLeft size={15} /> Все инстансы</button>
      <section className="instance-hero">
        <div className="instance-hero-icon">
          {instance.avatar_data ? <img src={avatarSource(instance.avatar_data) ?? undefined} alt={`Аватар ${instance.name}`} /> : <Blocks size={27} />}
        </div>
        <div className="instance-hero-copy">
          <span className="eyebrow"><Gamepad2 size={14} /> ИГРОВОЙ ПРОФИЛЬ</span>
          <h1>{instance.name}</h1>
          <div className="instance-hero-meta">
            <span><Blocks size={14} /> Minecraft {instance.minecraft_version}</span>
            <i />
            <span>{loaderName}</span>
            <i />
            <span><HardDrive size={14} /> {instance.memory_mb.toLocaleString("ru-RU")} МБ</span>
            {isRunning && <><i /><span className="instance-live"><span /> Игра запущена</span></>}
          </div>
        </div>
        <div className="instance-hero-actions">
          <button className="icon-button" type="button" title="Открыть папку" aria-label="Открыть папку инстанса" onClick={onOpenDirectory}><FolderOpen size={17} /></button>
          <button className="icon-button" type="button" title="Настройки инстанса" aria-label="Настроить инстанс" onClick={onEdit} disabled={busy || isRunning}><Settings2 size={17} /></button>
          <button className="icon-button danger-button" type="button" title="Удалить инстанс" aria-label="Удалить инстанс" onClick={onDelete} disabled={busy || isRunning}><Trash2 size={16} /></button>
          {isRunning ? (
            <button className="button-play stop-play" type="button" onClick={onStop}><Square size={13} /> Остановить</button>
          ) : (
            <button className="button-play" type="button" onClick={onLaunch} disabled={blockedByOtherGame || !hasActiveAccount} title={!hasActiveAccount ? "Сначала выберите аккаунт" : "Подготовить и запустить игру"}>
              <Gamepad2 size={15} /> Играть
            </button>
          )}
        </div>
      </section>

      <nav className="instance-detail-tabs" role="tablist" aria-label="Разделы инстанса">
        {detailTabs.map(({ id, label, icon: Icon }) => (
          <button
            aria-selected={tab === id}
            className={tab === id ? "active" : ""}
            key={id}
            onClick={() => onChangeTab(id)}
            role="tab"
            type="button"
          >
            <Icon size={16} /> {label}
            {id === "content" && installedMods.length > 0 && <span className="detail-tab-count">{installedMods.length}</span>}
            {id === "logs" && isRunning && <span className="detail-live-dot" />}
          </button>
        ))}
      </nav>

      {tab === "content" && (
        <section className="instance-detail-section" role="tabpanel">
          <div className="detail-toolbar">
            <label className="detail-search"><Search size={16} /><input value={contentQuery} onChange={(event) => setContentQuery(event.target.value)} placeholder={`Поиск среди ${installedMods.length} файлов`} /></label>
            <button className="button-secondary" type="button" onClick={onOpenCatalog}><PackageSearch size={15} /> Каталог контента</button>
          </div>
          <div className="detail-list-heading"><strong>Установленные моды</strong><span>{installedMods.length} файлов</span></div>
          {filteredMods.length > 0 ? (
            <div className="detail-content-list">
              {filteredMods.map((mod) => (
                <article className="detail-content-row" key={mod.fileName}>
                  <div className="detail-content-icon">
                    {mod.iconUrl && !failedIcons.has(mod.projectId ?? mod.fileName) ? (
                      <img
                        alt=""
                        key={mod.iconUrl}
                        onError={() => setFailedIcons((current) => new Set(current).add(mod.projectId ?? mod.fileName))}
                        src={mod.iconUrl}
                      />
                    ) : <Blocks size={18} />}
                  </div>
                  <div className="detail-content-name">
                    <strong>{mod.title || mod.fileName.replace(/\.jar(\.disabled)?$/i, "")}</strong>
                    <span>{mod.versionNumber ? `${mod.versionNumber} · ` : ""}{mod.fileName}{mod.projectId ? " · Modrinth" : " · Локальный файл"}</span>
                  </div>
                  <span className={`detail-enabled ${mod.enabled ? "enabled" : "disabled"}`}><span />{mod.enabled ? "Включён" : "Выключен"}</span>
                  {mod.projectId && <button className="icon-button" type="button" title="Выбрать версию" aria-label={`Выбрать версию для ${mod.title || mod.fileName}`} disabled={busy} onClick={() => openVersionPicker(mod)}><RefreshCw size={16} /></button>}
                  <button className="icon-button" type="button" title={mod.enabled ? "Выключить" : "Включить"} aria-label={`${mod.enabled ? "Выключить" : "Включить"} ${mod.fileName}`} disabled={busy} onClick={() => onToggleMod(mod)}>{mod.enabled ? <Power size={16} /> : <Check size={16} />}</button>
                  <button className="icon-button danger-button" type="button" title="Удалить файл" aria-label={`Удалить ${mod.fileName}`} disabled={busy} onClick={() => onRemoveMod(mod)}><Trash2 size={16} /></button>
                </article>
              ))}
            </div>
          ) : (
            <div className="detail-empty"><PackageSearch size={22} /><strong>{installedMods.length ? "Совпадений нет" : "Контент пока не установлен"}</strong><span>{installedMods.length ? "Попробуйте изменить поисковый запрос." : "Найдите моды и пакеты для этого игрового профиля."}</span>{!installedMods.length && <button className="button-primary" type="button" onClick={onOpenCatalog}><Download size={15} /> Найти контент</button>}</div>
          )}
        </section>
      )}

      {tab === "files" && (
        <section className="instance-detail-section" role="tabpanel">
          <div className="detail-section-heading"><div><h2>Файлы инстанса</h2><p>Содержимое папки профиля на этом устройстве.</p></div><button className="button-secondary" type="button" onClick={onOpenDirectory}><FolderOpen size={15} /> Папка в Проводнике</button></div>
          {filesError ? <div className="error-panel" role="alert"><File size={18} /><div><strong>Не удалось прочитать файлы</strong><p>{filesError}</p></div></div> : filesLoading ? <div className="detail-loading"><LoaderCircle className="spin" size={18} /> Читаем папку инстанса...</div> : files.length ? (
            <div className="detail-file-list">
              {files.map((file) => (
                <article className="detail-file-row" key={file.name}>
                  <span className={`detail-file-icon ${file.isDirectory ? "directory" : ""}`}>{file.isDirectory ? <Folder size={17} /> : <File size={17} />}</span>
                  <strong>{file.name}</strong>
                  <span>{file.isDirectory ? "Папка" : formatFileSize(file.sizeBytes)}</span>
                  <span><Clock3 size={13} /> {formatModifiedTime(file.modifiedUnixSeconds)}</span>
                </article>
              ))}
            </div>
          ) : <div className="detail-empty"><Folder size={22} /><strong>Папка инстанса пуста</strong></div>}
        </section>
      )}

      {tab === "worlds" && (
        <section className="instance-detail-section" role="tabpanel">
          <div className="detail-section-heading"><div><h2>Миры</h2><p>Сохранения Minecraft в этом инстансе.</p></div></div>
          {worldsError ? <div className="error-panel" role="alert"><Globe2 size={18} /><div><strong>Не удалось прочитать миры</strong><p>{worldsError}</p></div></div> : worldsLoading ? <div className="detail-loading"><LoaderCircle className="spin" size={18} /> Ищем сохранения...</div> : worlds.length ? (
            <div className="detail-world-list">
              {worlds.map((world, index) => <article className="detail-world-row" key={world}><div className="world-index">{String(index + 1).padStart(2, "0")}</div><div className="detail-world-icon"><Globe2 size={18} /></div><div><strong>{world}</strong><span>Сохранение Minecraft</span></div></article>)}
            </div>
          ) : <div className="detail-empty"><Globe2 size={22} /><strong>Сохранений пока нет</strong><span>Миры появятся здесь после первого запуска игры.</span></div>}
        </section>
      )}

      {tab === "logs" && (
        <section className="instance-detail-section" role="tabpanel">
          <div className="detail-section-heading"><div><h2>Логи запуска</h2><p>{isRunning ? "Вывод работающего процесса Minecraft обновляется в реальном времени." : "Логи доступны в течение текущего сеанса приложения."}</p></div><span className={`detail-log-status ${isRunning ? "running" : ""}`}><span />{isRunning ? "ЗАПУЩЕН" : "ОСТАНОВЛЕН"}</span></div>
          {visibleLogs.length ? <pre className="instance-log-output">{visibleLogs.map((entry, index) => <span className={entry.stream === "stderr" ? "log-error" : ""} key={`${index}-${entry.line}`}>[{entry.stream}] {entry.line}{"\n"}</span>)}</pre> : <div className="detail-empty"><ScrollText size={22} /><strong>Логов пока нет</strong><span>Запустите Minecraft, чтобы увидеть вывод процесса.</span></div>}
        </section>
      )}

      <div className="instance-detail-footer"><UsersRound size={14} /><span>Локальный профиль</span><span className="detail-footer-dot" /><span>Создан {new Intl.DateTimeFormat("ru-RU", { dateStyle: "medium" }).format(new Date(instance.created_at))}</span></div>

      {versionPickerMod && (
        <div className="dialog-backdrop version-picker-backdrop" onMouseDown={(event) => {
          if (event.target === event.currentTarget) setVersionPickerMod(null);
        }}>
          <section className="version-picker" role="dialog" aria-modal="true" aria-labelledby="version-picker-title">
            <header className="version-picker-header">
              <div className="version-picker-icon">
                {versionPickerMod.iconUrl && !failedIcons.has(versionPickerMod.projectId ?? versionPickerMod.fileName)
                  ? <img alt="" src={versionPickerMod.iconUrl} onError={() => setFailedIcons((current) => new Set(current).add(versionPickerMod.projectId ?? versionPickerMod.fileName))} />
                  : <Blocks size={20} />}
              </div>
              <div className="version-picker-heading"><h2 id="version-picker-title">Выбор версии</h2><span>{versionPickerMod.title || versionPickerMod.fileName.replace(/\.jar(\.disabled)?$/i, "")}</span></div>
              <button className="icon-button version-picker-close" type="button" aria-label="Закрыть" title="Закрыть" onClick={() => setVersionPickerMod(null)}><X size={17} /></button>
            </header>

            <div className="version-picker-body">
              <aside className="version-picker-list-panel">
                <label className="version-picker-search"><Search size={15} /><input autoFocus value={versionSearch} onChange={(event) => setVersionSearch(event.target.value)} placeholder="Поиск версии..." /></label>
                <div className="version-picker-list" role="listbox" aria-label="Совместимые версии">
                  {loadingVersionsFor.includes(versionPickerMod.projectId ?? "") ? (
                    <div className="version-picker-state"><LoaderCircle className="spin" size={18} /> Загружаем версии...</div>
                  ) : versionErrors[versionPickerMod.projectId ?? ""] ? (
                    <div className="version-picker-state version-picker-error">{versionErrors[versionPickerMod.projectId ?? ""]}</div>
                  ) : filteredProjectVersions.length ? filteredProjectVersions.map((version) => (
                    <button
                      aria-selected={selectedVersionId === version.versionId}
                      className={`version-option ${selectedVersionId === version.versionId ? "selected" : ""}`}
                      key={version.versionId}
                      onClick={() => setSelectedVersionId(version.versionId)}
                      role="option"
                      type="button"
                    >
                      <span className="version-option-mark">{selectedVersionId === version.versionId ? <Check size={14} /> : null}</span>
                      <span className="version-option-copy"><strong>{version.versionNumber}</strong><span>{version.versionId === versionPickerMod.versionId ? "Установлена" : `Minecraft ${instance.minecraft_version} · ${loaderName}`}</span></span>
                      {version.versionId === versionPickerMod.versionId && <span className="version-current-badge">Текущая</span>}
                    </button>
                  )) : (
                    <div className="version-picker-state">{selectedProjectVersions.length ? "Версии не найдены" : "Нет совместимых версий"}</div>
                  )}
                </div>
              </aside>

              <section className="version-picker-details">
                {selectedProjectVersions.find((version) => version.versionId === selectedVersionId) ? (
                  <>
                    <span className="eyebrow">СОВМЕСТИМАЯ ВЕРСИЯ</span>
                    <h3>{selectedProjectVersions.find((version) => version.versionId === selectedVersionId)?.versionNumber}</h3>
                    <div className="version-picker-compatibility"><Check size={15} /> Minecraft {instance.minecraft_version} <span /> {loaderName}</div>
                    <p>Будет заменён только этот JAR. Остальные моды и файлы инстанса останутся без изменений.</p>
                  </>
                ) : <div className="version-picker-state">Выберите версию из списка.</div>}
              </section>
            </div>

            <footer className="version-picker-footer">
              <div className="version-picker-warning"><AlertTriangle size={17} /><span>Обновление может повлиять на совместимость модов. Перед заменой сохраните резервную копию.</span></div>
              <div className="version-picker-actions">
                <button className="button-secondary" type="button" onClick={() => setVersionPickerMod(null)}>Отмена</button>
                <button
                  className="button-primary"
                  type="button"
                  disabled={busy || !selectedVersionId || selectedVersionId === versionPickerMod.versionId}
                  onClick={() => {
                    onUpdateMod(versionPickerMod, selectedVersionId);
                    setVersionPickerMod(null);
                  }}
                >
                  <RefreshCw size={15} /> Сохранить изменения
                </button>
              </div>
            </footer>
          </section>
        </div>
      )}
    </>
  );
}
