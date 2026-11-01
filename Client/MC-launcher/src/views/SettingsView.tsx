import { Database as DatabaseIcon, FolderOpen, Moon, Palette, RotateCcw, Settings, Sun } from "lucide-react";
import type { DatabaseStatus, PaletteColor, Theme, ThemePalettes } from "../types";

interface SettingsViewProps {
  theme: Theme;
  uiPalettes: ThemePalettes;
  databaseStatus: DatabaseStatus;
  onChangeTheme: (theme: Theme) => void;
  onChangePaletteColor: (color: PaletteColor, value: string) => void;
  onResetPalette: () => void;
  onOpenLauncherDirectory: () => void;
}

export function SettingsView({
  theme,
  uiPalettes,
  databaseStatus,
  onChangeTheme,
  onChangePaletteColor,
  onResetPalette,
  onOpenLauncherDirectory,
}: SettingsViewProps) {
  return (
    <>
      <section className="page-heading"><div><div className="eyebrow"><Settings size={15} /> ПРИЛОЖЕНИЕ</div><h1>Настройки</h1><p className="page-subtitle">Параметры лаунчера и локального хранилища.</p></div></section>
      <section className="settings-section">
        <div className="settings-section-heading appearance-settings-heading">
          <div><span>ВНЕШНИЙ ВИД</span><p>Настройте оформление приложения.</p></div>
          <button
            type="button"
            className="button-secondary palette-reset-button"
            onClick={onResetPalette}
            disabled={databaseStatus !== "ready"}
            title="Сбросить цвета текущей темы к исходным"
          >
            <RotateCcw size={14} /> Сбросить палитру
          </button>
        </div>
        <div className="setting-row">
          <div className="setting-description"><strong>Тема</strong><span>Сохраняется на этом устройстве</span></div>
          <div className="segmented-control" role="group" aria-label="Тема приложения">
            <button className={theme === "light" ? "selected" : ""} onClick={() => onChangeTheme("light")}><Sun size={16} /> Светлая</button>
            <button className={theme === "dark" ? "selected" : ""} onClick={() => onChangeTheme("dark")}><Moon size={16} /> Тёмная</button>
            <button className={theme === "custom" ? "selected" : ""} onClick={() => onChangeTheme("custom")}><Palette size={16} /> Своя</button>
          </div>
        </div>
        <div className="palette-color-grid" aria-label="Цвета элементов интерфейса">
          {([
            ["accent", "Акцент интерфейса"],
            ["button", "Кнопки"],
            ["canvas", "Фон приложения"],
            ["surface", "Панели и поверхности"],
            ["ink", "Основной текст"],
          ] as const).map(([color, label]) => (
            <label className="palette-color-field" key={color}>
              <span>{label}</span>
              <input
                type="color"
                value={uiPalettes[theme][color]}
                aria-label={label}
                disabled={databaseStatus !== "ready"}
                onChange={(event) => onChangePaletteColor(color, event.currentTarget.value)}
              />
            </label>
          ))}
        </div>
      </section>
      <section className="settings-section storage-section">
        <div className="settings-section-heading"><span>ХРАНИЛИЩЕ</span><p>Локальные файлы приложения.</p></div>
        <div className="setting-row">
          <div className="setting-description"><strong>Папка лаунчера</strong><span>%APPDATA%\MyLauncher</span></div>
          <button className="storage-badge storage-open" onClick={onOpenLauncherDirectory} title="Открыть папку лаунчера в проводнике"><FolderOpen size={16} /> Открыть</button>
        </div>
        <div className="setting-row">
          <div className="setting-description"><strong>База данных</strong><span>{databaseStatus === "ready" ? "launcher.db · подключена" : databaseStatus === "error" ? "Не удалось подключить" : "Подключение..."}</span></div>
          <div className={`storage-badge ${databaseStatus}`}><DatabaseIcon size={16} />{databaseStatus === "ready" ? "SQLite" : databaseStatus === "error" ? "Ошибка" : "Ожидание"}</div>
        </div>
      </section>
      <footer className="settings-footer">MyLauncher <span>Версия 0.1.0</span></footer>
    </>
  );
}
