import {
  Blocks,
  Boxes,
  CircleUserRound,
  Copy,
  Database as DatabaseIcon,
  Download,
  ExternalLink,
  FolderOpen,
  Gamepad2,
  LayoutGrid,
  LoaderCircle,
  PackageSearch,
  Plus,
  RefreshCw,
  Settings,
  Square,
  Trash2,
} from "lucide-react";
import type { DatabaseStatus, GameLog, Instance } from "../types";
import { avatarSource } from "../lib/common";

interface InstancesViewProps {
  instances: Instance[];
  accountCount: number;
  databaseStatus: DatabaseStatus;
  databaseError: string;
  hasActiveAccount: boolean;
  busy: boolean;
  busyInstanceIds: Set<string>;
  downloadingInstanceId: string | null;
  runningInstanceId: string | null;
  installedInstanceIds: Set<string>;
  gameLogs: GameLog[];
  onCreate: () => void;
  onOpenDirectory: (instance: Instance) => void;
  onOpenInstance: (instance: Instance) => void;
  onOpenCatalog: (instance: Instance) => void;
  onEdit: (instance: Instance) => void;
  onDuplicate: (instance: Instance) => void;
  onDelete: (instance: Instance) => void;
  onLaunch: (instance: Instance) => void;
  onStop: (instance: Instance) => void;
  onUpdateRepository: (instance: Instance) => void;
}

export function InstancesView({
  instances,
  accountCount,
  databaseStatus,
  databaseError,
  hasActiveAccount,
  busy,
  busyInstanceIds,
  downloadingInstanceId,
  runningInstanceId,
  installedInstanceIds,
  gameLogs,
  onCreate,
  onOpenDirectory,
  onOpenInstance,
  onOpenCatalog,
  onEdit,
  onDuplicate,
  onDelete,
  onLaunch,
  onStop,
  onUpdateRepository,
}: InstancesViewProps) {
  return (
    <>
      <section className="page-heading">
        <div>
          <div className="eyebrow"><Gamepad2 size={15} /> ИГРОВАЯ БИБЛИОТЕКА</div>
          <h1>Инстансы</h1>
          <p className="page-subtitle">Ваши игровые профили и сборки Minecraft.</p>
        </div>
        <button className="button-primary" onClick={onCreate} disabled={busy}>
          <Plus size={18} /> Создать инстанс
        </button>
      </section>

      <section className="summary-strip" aria-label="Сводка библиотеки">
        <div className="summary-item"><span className="summary-icon green"><LayoutGrid size={17} /></span><div><strong>{instances.length}</strong><span>Инстансов</span></div></div>
        <div className="summary-divider" />
        <div className="summary-item"><span className="summary-icon orange"><CircleUserRound size={17} /></span><div><strong>{accountCount}</strong><span>Аккаунтов</span></div></div>
        <div className="summary-divider" />
        <div className="summary-item"><span className="summary-icon blue"><DatabaseIcon size={17} /></span><div><strong>{databaseStatus === "ready" ? "Готово" : databaseStatus === "error" ? "Ошибка" : "..."}</strong><span>Локальная база</span></div></div>
      </section>

      {databaseStatus === "error" ? (
        <section className="error-panel" role="alert">
          <DatabaseIcon size={22} />
          <div><strong>Не удалось подключить локальную базу</strong><p>{databaseError}</p></div>
        </section>
      ) : instances.length === 0 && databaseStatus === "ready" ? (
        <section className="empty-state">
          <div className="empty-art" aria-hidden="true">
            <div className="art-grid" />
            <div className="art-cube"><Blocks size={48} strokeWidth={1.35} /></div>
            <span className="art-spark spark-one" /><span className="art-spark spark-two" />
          </div>
          <div className="empty-copy">
            <span className="empty-kicker">БИБЛИОТЕКА ПУСТА</span>
            <h2>Здесь начнётся ваше приключение</h2>
            <p>Создайте отдельные игровые профили для разных версий и сборок.</p>
          </div>
          <div className="empty-meta"><span><span className="meta-dot" />Хранилище готово</span><span>Данные остаются на этом устройстве</span></div>
        </section>
      ) : (
        <section className="instance-list" aria-label="Список инстансов">
          {instances.map((instance) => {
            const isRunning = runningInstanceId === instance.id;
            const isBusy = busyInstanceIds.has(instance.id);
            const isDownloading = downloadingInstanceId === instance.id;
            return (
              <article className="instance-row" key={instance.id}>
                <div className="instance-icon">{instance.avatar_data ? <img src={avatarSource(instance.avatar_data) ?? undefined} alt={`Аватар ${instance.name}`} /> : <Blocks size={21} />}</div>
                <div className="instance-details">
                  <strong>{instance.name}</strong>
                  <span>Minecraft {instance.minecraft_version} <i /> {instance.loader_type === "vanilla" ? "Vanilla" : `${instance.loader_type}${instance.loader_version ? ` ${instance.loader_version}` : ""}`}{instance.repository_pack_id ? <> <i /> <span className="instance-pack-source"><Boxes size={11} /> Репозиторий {instance.repository_version}</span></> : instance.modpack_project_id && <> <i /> <span className="instance-pack-source"><Boxes size={11} /> Modrinth {instance.modpack_version_number}</span></>}</span>
                </div>
                <button className="icon-button" aria-label={`Открыть страницу инстанса ${instance.name}`} title="Страница инстанса" onClick={() => onOpenInstance(instance)}><ExternalLink size={16} /></button>
                <button className="icon-button" aria-label={`Открыть папку ${instance.name}`} title="Открыть папку" onClick={() => onOpenDirectory(instance)}><FolderOpen size={17} /></button>
                {instance.repository_pack_id && instance.repository_url && <button className="icon-button" aria-label={`Обновить сборку ${instance.name}`} title="Проверить обновления сборки" onClick={() => onUpdateRepository(instance)} disabled={isBusy || isRunning}><RefreshCw size={16} /></button>}
                <button className="icon-button" aria-label={`Каталог контента ${instance.name}`} title="Каталог контента" onClick={() => onOpenCatalog(instance)}><PackageSearch size={17} /></button>
                <button className="icon-button" aria-label={`Изменить ${instance.name}`} title="Изменить инстанс" onClick={() => onEdit(instance)} disabled={isBusy || isRunning}><Settings size={16} /></button>
                <button className="icon-button" aria-label={`Дублировать ${instance.name}`} title="Дублировать" onClick={() => onDuplicate(instance)} disabled={isBusy || isRunning}><Copy size={16} /></button>
                <button className="icon-button danger-button" aria-label={`Удалить ${instance.name}`} title={isRunning ? "Сначала остановите игру" : "Удалить инстанс"} onClick={() => onDelete(instance)} disabled={isBusy || isRunning}><Trash2 size={16} /></button>
                {isRunning ? (
                  <button className="button-play stop-play" onClick={() => onStop(instance)} disabled={isBusy} title="Остановить Minecraft">
                    <Square size={13} /> ОСТАНОВИТЬ
                  </button>
                ) : (
                  <button
                    className="button-play"
                    onClick={() => onLaunch(instance)}
                    disabled={isBusy}
                    title={isDownloading ? "Подготовка Minecraft и Java" : !hasActiveAccount ? "Выберите аккаунт" : "Подготовить и запустить Minecraft"}
                  >
                    {isDownloading ? <LoaderCircle size={15} className="spin" /> : installedInstanceIds.has(instance.id) ? <Gamepad2 size={15} /> : <Download size={15} />}
                    {isDownloading ? "ПОДГОТОВКА" : "ИГРАТЬ"}
                  </button>
                )}
              </article>
            );
          })}
        </section>
      )}
      {gameLogs.length > 0 && (
        <section className="game-log-panel" aria-label="Логи игры">
          <div className="download-status-heading"><span>Логи игры</span><strong>{runningInstanceId ? "ЗАПУЩЕНА" : "ЗАВЕРШЕНА"}</strong></div>
          <pre>{gameLogs.map((entry, index) => <span className={entry.stream === "stderr" ? "log-error" : ""} key={`${index}-${entry.line}`}>[{entry.stream}] {entry.line}{"\n"}</span>)}</pre>
        </section>
      )}
    </>
  );
}
