import { Blocks, Boxes, ChevronRight, CircleUserRound, LayoutGrid, Server, Settings } from "lucide-react";
import type { DatabaseStatus, View } from "../types";

interface SidebarProps {
  view: View;
  databaseStatus: DatabaseStatus;
  onSelectView: (view: View) => void;
  onOpenAbout: () => void;
}

export function Sidebar({ view, databaseStatus, onSelectView, onOpenAbout }: SidebarProps) {
  return (
    <aside className="sidebar">
      <div className="brand-lockup">
        <div className="brand-mark"><Blocks size={21} strokeWidth={2.1} /></div>
        <div>
          <span className="brand-name">MYLAUNCHER</span>
          <span className="brand-caption">MINECRAFT LIBRARY</span>
        </div>
      </div>

      <div className="nav-label">БИБЛИОТЕКА</div>
      <nav className="main-nav" aria-label="Основная навигация">
        <button className={`nav-item ${view === "instances" || view === "instance-detail" ? "active" : ""}`} onClick={() => onSelectView("instances")}>
          <LayoutGrid size={18} /><span>Инстансы</span><ChevronRight className="nav-chevron" size={15} />
        </button>
        <button className={`nav-item ${view === "accounts" ? "active" : ""}`} onClick={() => onSelectView("accounts")}>
          <CircleUserRound size={18} /><span>Аккаунты</span>
        </button>
        <button className={`nav-item ${view === "servers" ? "active" : ""}`} onClick={() => onSelectView("servers")}>
          <Server size={18} /><span>Серверы</span>
        </button>
        <button className={`nav-item ${view === "modpacks" ? "active" : ""}`} onClick={() => onSelectView("modpacks")}>
          <Boxes size={18} /><span>Сборки Modrinth</span>
        </button>
        <button className={`nav-item ${view === "repository" ? "active" : ""}`} onClick={() => onSelectView("repository")}>
          <Boxes size={18} /><span>Собственные сборки</span>
        </button>
      </nav>

      <div className="sidebar-spacer" />
      <div className="sidebar-status">
        <span className={`status-light ${databaseStatus}`} />
        <span>{databaseStatus === "ready" ? "Локальное хранилище" : databaseStatus === "error" ? "Ошибка хранилища" : "Подключение к базе"}</span>
      </div>
      <button className={`nav-item settings-link ${view === "settings" ? "active" : ""}`} onClick={() => onSelectView("settings")}>
        <Settings size={18} /><span>Настройки</span>
      </button>
      <button className="sidebar-version" type="button" onClick={onOpenAbout} aria-label="Открыть страницу разработчиков" title="Разработчики">
        MYLAUNCHER <span>0.1.0</span>
      </button>
    </aside>
  );
}
