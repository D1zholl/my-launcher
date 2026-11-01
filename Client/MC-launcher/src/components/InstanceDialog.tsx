import { createPortal } from "react-dom";
import { useRef } from "react";
import { Blocks, LoaderCircle, Plus, RefreshCw, Settings, Upload } from "lucide-react";
import { avatarSource, readAvatarFile } from "../lib/common";
import { isLightPalette, themeClassName } from "../lib/palettes";
import type {
  Instance,
  InstanceForm,
  LoaderVersion,
  ModLoaderType,
  Theme,
  ThemePalettes,
} from "../types";

interface InstanceDialogProps {
  editingInstance: Instance | null;
  form: InstanceForm;
  onChange: (patch: Partial<InstanceForm>) => void;
  versions: string[];
  availableLoaderTypes: ModLoaderType[];
  loaderVersions: LoaderVersion[];
  loaderAvailabilityLoading: boolean;
  loaderLoading: boolean;
  versionError: string;
  loaderError: string;
  memoryLimitMb: number;
  busy: boolean;
  theme: Theme;
  uiPalettes: ThemePalettes;
  onSubmit: (event: React.FormEvent<HTMLFormElement>) => void;
  onClose: () => void;
  onError: (message: string) => void;
  onRepair: () => void;
}

export function InstanceDialog({
  editingInstance,
  form,
  onChange,
  versions,
  availableLoaderTypes,
  loaderVersions,
  loaderAvailabilityLoading,
  loaderLoading,
  versionError,
  loaderError,
  memoryLimitMb,
  busy,
  theme,
  uiPalettes,
  onSubmit,
  onClose,
  onError,
  onRepair,
}: InstanceDialogProps) {
  const dialogRef = useRef<HTMLElement>(null);

  return createPortal((
    <div
      className={`dialog-backdrop ${themeClassName(theme, uiPalettes[theme])}`}
      style={{
        "--accent": uiPalettes[theme].accent,
        "--button": uiPalettes[theme].button,
        "--canvas": uiPalettes[theme].canvas,
        "--surface": uiPalettes[theme].surface,
        "--ink": uiPalettes[theme].ink,
        colorScheme: theme === "light" || (theme === "custom" && isLightPalette(uiPalettes.custom)) ? "light" : "dark",
      } as React.CSSProperties}
      role="presentation"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget && !busy) onClose();
      }}
    >
      <section
        ref={dialogRef}
        className="instance-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="create-instance-title"
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            if (!busy) onClose();
            return;
          }
          if (event.key !== "Tab") return;
          const focusable = dialogRef.current?.querySelectorAll<HTMLElement>(
            'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
          );
          if (!focusable?.length) return;
          const first = focusable[0];
          const last = focusable[focusable.length - 1];
          if (event.shiftKey && document.activeElement === first) {
            event.preventDefault();
            last.focus();
          } else if (!event.shiftKey && document.activeElement === last) {
            event.preventDefault();
            first.focus();
          }
        }}
      >
        <div className="dialog-heading">
          <div>
            <span className="eyebrow">{editingInstance ? "ПАРАМЕТРЫ СБОРКИ" : "НОВАЯ СБОРКА"}</span>
            <h2 id="create-instance-title">{editingInstance ? "Изменить инстанс" : "Создать инстанс"}</h2>
          </div>
          <button className="dialog-close" aria-label="Закрыть" onClick={onClose} disabled={busy}>×</button>
        </div>
        <form onSubmit={onSubmit}>
          <label className="form-field">
            <span>Имя инстанса</span>
            <input
              autoFocus
              value={form.name}
              onChange={(event) => onChange({ name: event.target.value })}
              maxLength={64}
              placeholder="Например, Survival"
              required
              pattern={'[^\\/:*?"<>|]+'}
              title={'Имя не должно содержать символы \\ / : * ? " < > |'}
            />
          </label>
          <div className="avatar-field">
            <div className="instance-icon avatar-preview">{form.avatarData ? <img src={avatarSource(form.avatarData) ?? undefined} alt="Предпросмотр аватарки" /> : <Blocks size={21} />}</div>
            <div className="avatar-picker">
              <label className="button-secondary avatar-upload">
                <Upload size={15} /> Выбрать аватар
                <input
                  type="file"
                  accept="image/png,image/jpeg,image/webp"
                  onChange={(event) => {
                    const file = event.currentTarget.files?.[0];
                    event.currentTarget.value = "";
                    if (!file) return;
                    if (!["image/png", "image/jpeg", "image/webp"].includes(file.type) || file.size > 2 * 1024 * 1024) {
                      onError("Выберите PNG, JPEG или WebP размером до 2 МБ.");
                      return;
                    }
                    void readAvatarFile(file)
                      .then((data) => onChange({ avatarData: data }))
                      .catch((error: unknown) => onError(error instanceof Error ? error.message : String(error)));
                  }}
                />
              </label>
              {form.avatarData && <button type="button" className="button-secondary" onClick={() => onChange({ avatarData: null })}>Убрать аватар</button>}
              <span className="avatar-hint">PNG, JPEG или WebP, до 2 МБ</span>
            </div>
          </div>
          <label className="form-field">
            <span>Версия Minecraft</span>
            <select value={form.version} onChange={(event) => onChange({ version: event.target.value })} disabled={versions.length === 0} required>
              <option value="" disabled>{versions.length ? "Выберите версию" : "Загрузка версий..."}</option>
              {versions.map((version) => <option key={version} value={version}>{version}{version === form.version ? " · последняя" : ""}</option>)}
            </select>
          </label>
          <label className="form-field">
            <span>Загрузчик</span>
            <select value={form.loader} onChange={(event) => onChange({ loader: event.target.value })} disabled={loaderAvailabilityLoading}>
              <option value="vanilla">Vanilla</option>
              {availableLoaderTypes.includes("fabric") && <option value="fabric">Fabric</option>}
              {availableLoaderTypes.includes("forge") && <option value="forge">Forge</option>}
              {availableLoaderTypes.includes("neoforge") && <option value="neoforge">NeoForge</option>}
            </select>
          </label>
          {form.loader !== "vanilla" && (
            <label className="form-field">
              <span>Версия загрузчика</span>
              <select value={form.loaderVersion} onChange={(event) => onChange({ loaderVersion: event.target.value })} disabled={loaderLoading || loaderVersions.length === 0} required>
                <option value="" disabled>{loaderLoading ? "Загрузка версий..." : "Выберите версию"}</option>
                {loaderVersions.map((loaderVersion) => <option key={`${loaderVersion.version}-${loaderVersion.channel}`} value={loaderVersion.version}>{loaderVersion.version} · {loaderVersion.channel}</option>)}
              </select>
            </label>
          )}
          <section className="dialog-settings">
            <div className="settings-section-heading"><span>СРЕДА ЗАПУСКА</span></div>
            <div className="runtime-row">
              <span>Java Temurin будет подобрана автоматически для Minecraft {form.version || ""}</span>
              <span>{editingInstance?.java_path ? `Установлена Java ${editingInstance.java_version}` : "Установится при первом запуске"}</span>
            </div>
            <label className="form-field">
              <span>Память: {form.memoryMb} МБ · максимум {memoryLimitMb} МБ</span>
              <input type="range" min="512" max={memoryLimitMb} step="512" value={form.memoryMb} onChange={(event) => onChange({ memoryMb: Number(event.target.value) })} />
            </label>
            <div className="resolution-row">
              <label className="form-field"><span>Ширина окна</span><input type="number" min="640" max="3840" value={form.windowWidth} onChange={(event) => onChange({ windowWidth: Number(event.target.value) })} /></label>
              <label className="form-field"><span>Высота окна</span><input type="number" min="480" max="2160" value={form.windowHeight} onChange={(event) => onChange({ windowHeight: Number(event.target.value) })} /></label>
            </div>
            <label className="fullscreen-toggle"><input type="checkbox" checked={form.fullscreen} onChange={(event) => onChange({ fullscreen: event.target.checked })} /><span>Полноэкранный режим</span></label>
            <label className="form-field"><span>Дополнительные JVM-флаги</span><input value={form.jvmArgs} onChange={(event) => onChange({ jvmArgs: event.target.value })} placeholder="-XX:+UseG1GC" /></label>
            {editingInstance && (
              <div className="repair-game-row">
                <div><strong>Файлы Minecraft</strong><span>Проверить и восстановить игровые файлы. Сохранения и контент останутся на месте.</span></div>
                <button className="button-secondary" type="button" onClick={onRepair} disabled={busy}>
                  {busy ? <LoaderCircle className="spin" size={14} /> : <RefreshCw size={14} />}
                  {busy ? "Проверяем..." : "Проверить и восстановить"}
                </button>
              </div>
            )}
          </section>
          {versionError && <p className="inline-error" role="alert">{versionError}</p>}
          {loaderError && <p className="inline-error" role="alert">{loaderError}</p>}
          <div className="dialog-actions">
            <button type="button" className="button-secondary" onClick={onClose} disabled={busy}>Отмена</button>
            <button
              type="submit"
              className="button-primary"
              disabled={busy || versions.length === 0 || loaderAvailabilityLoading || !form.name.trim() || form.memoryMb > memoryLimitMb || (form.loader !== "vanilla" && (!form.loaderVersion || loaderLoading))}
            >
              {busy ? <LoaderCircle className="spin" size={17} /> : editingInstance ? <Settings size={17} /> : <Plus size={17} />} {editingInstance ? "Сохранить" : "Создать"}
            </button>
          </div>
        </form>
      </section>
    </div>
  ), document.body);
}
