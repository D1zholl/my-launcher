import { Blocks, Boxes, ChevronDown, ChevronRight, CircleUserRound, LoaderCircle, Server as ServerIcon, X } from "lucide-react";
import type { DownloadProgress, ModpackInstallTask, ServerInstallRequest } from "../types";

interface TopbarProps {
  currentViewTitle: string;
  downloadingInstanceId: string | null;
  installingInstanceName: string | null;
  installStageLabel: string;
  downloadProgress: DownloadProgress | null;
  serverInstallTask: ServerInstallRequest | null;
  onCancelServerInstall: () => void;
  onCancelGameInstall: () => void;
  modpackInstallTask: ModpackInstallTask | null;
  onCancelModpackInstall: () => void;
  activeTaskCount: number;
  taskTrayOpen: boolean;
  onToggleTaskTray: () => void;
  activeAccountName: string | null;
}

export function Topbar({
  currentViewTitle,
  downloadingInstanceId,
  installingInstanceName,
  installStageLabel,
  downloadProgress,
  serverInstallTask,
  onCancelServerInstall,
  onCancelGameInstall,
  modpackInstallTask,
  onCancelModpackInstall,
  activeTaskCount,
  taskTrayOpen,
  onToggleTaskTray,
  activeAccountName,
}: TopbarProps) {
  return (
    <header className="topbar">
      <div className="breadcrumb"><span>Библиотека</span><ChevronRight size={14} /><strong>{currentViewTitle}</strong></div>
      <div className="topbar-right">
        {activeTaskCount > 0 && (
          <div className="task-center">
            <button
              className="task-trigger"
              type="button"
              aria-label="Показать задачи установки"
              aria-expanded={taskTrayOpen}
              title="Активные установки"
              onClick={onToggleTaskTray}
            >
              <LoaderCircle size={16} className="spin" />
              <span>Установка</span>
              <span className="task-count">{activeTaskCount}</span>
              <ChevronDown size={14} className={taskTrayOpen ? "task-chevron open" : "task-chevron"} />
            </button>
            {taskTrayOpen && (
              <section className="task-popover" aria-label="Задачи установки">
                <div className="task-popover-heading"><strong>ЗАДАЧИ</strong><span className="task-count">{activeTaskCount}</span></div>
                <div className="task-section-heading"><strong>Активно</strong><span>{activeTaskCount === 1 ? (downloadingInstanceId ? installStageLabel : modpackInstallTask ? "Установка сборки" : "Установка сервера") : `${activeTaskCount} задачи установки`}</span></div>
                {downloadingInstanceId && (
                  <article className="task-item">
                    <div className="task-item-icon"><Blocks size={18} /></div>
                    <div className="task-item-content">
                      <div className="task-item-title"><strong>{installingInstanceName ?? "Minecraft"}</strong><span>АКТИВНО</span></div>
                      <p>{installStageLabel}</p>
                      <div className="download-track">
                        <span
                          className={downloadProgress ? "" : "download-indeterminate"}
                          style={downloadProgress ? { width: `${downloadProgress.total ? Math.round(downloadProgress.completed / downloadProgress.total * 100) : 0}%` } : undefined}
                        />
                      </div>
                      <div className="task-item-caption">
                        <span>{downloadProgress?.currentFile ?? "Подготовка компонентов игры"}</span>
                        {downloadProgress && <strong>{downloadProgress.completed} / {downloadProgress.total} файлов</strong>}
                      </div>
                      <button className="task-cancel-button" type="button" onClick={onCancelGameInstall}><X size={13} /> Отменить установку</button>
                    </div>
                  </article>
                )}
                {serverInstallTask && (
                  <article className="task-item">
                    <div className="task-item-icon"><ServerIcon size={18} /></div>
                    <div className="task-item-content">
                      <div className="task-item-title"><strong>{serverInstallTask.name}</strong><span>АКТИВНО</span></div>
                      <p>Установка {serverInstallTask.core} · Minecraft {serverInstallTask.minecraftVersion}</p>
                      <div className="download-track"><span className="download-indeterminate" /></div>
                      <div className="task-item-caption"><span>Подготовка Java и серверных файлов</span></div>
                      <button className="task-cancel-button" type="button" onClick={onCancelServerInstall}><X size={13} /> Отменить установку</button>
                    </div>
                  </article>
                )}
                {modpackInstallTask && (
                  <article className="task-item">
                    <div className="task-item-icon"><Boxes size={18} /></div>
                    <div className="task-item-content">
                      <div className="task-item-title"><strong>{modpackInstallTask.projectTitle}</strong><span>АКТИВНО</span></div>
                      <p>Установка сборки · {modpackInstallTask.folderName}</p>
                      <div className="download-track"><span className={downloadProgress ? "" : "download-indeterminate"} style={downloadProgress ? { width: `${downloadProgress.total ? Math.round(downloadProgress.completed / downloadProgress.total * 100) : 0}%` } : undefined} /></div>
                      <div className="task-item-caption"><span>{downloadProgress?.currentFile ?? "Подготовка Minecraft и загрузчика"}</span>{downloadProgress && <strong>{downloadProgress.completed} / {downloadProgress.total} файлов</strong>}</div>
                      <button className="task-cancel-button" type="button" onClick={onCancelModpackInstall}><X size={13} /> Отменить установку</button>
                    </div>
                  </article>
                )}
              </section>
            )}
          </div>
        )}
        <div className="account-chip"><CircleUserRound size={17} /><span>{activeAccountName ?? "Аккаунт не выбран"}</span></div>
      </div>
    </header>
  );
}
