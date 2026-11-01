import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { Boxes, Check, ChevronLeft, ChevronRight, Download, LoaderCircle, Search, X } from "lucide-react";
import { getModpackVersions, searchModpacks } from "../lib/modpacks";
import { isLightPalette, themeClassName } from "../lib/palettes";
import type { ModpackDetails, ModpackVersion, ModrinthProject, Theme, ThemePalettes } from "../types";

interface ModpacksViewProps {
  versions: string[];
  memoryLimitMb: number;
  theme: Theme;
  uiPalettes: ThemePalettes;
  reportError: (error: unknown) => void;
  onInstall: (input: { name: string; project: ModrinthProject; version: ModpackVersion; memoryMb: number }) => Promise<void>;
}

const SUPPORTED_LOADERS = ["fabric", "forge", "neoforge"] as const;

function defaultInstanceName(title: string): string {
  return title.replace(/[\\/:*?"<>|]/g, "-").replace(/[. ]+$/g, "").slice(0, 64).trim() || "Modrinth Pack";
}

export function ModpacksView({ versions, memoryLimitMb, theme, uiPalettes, reportError, onInstall }: ModpacksViewProps) {
  const [query, setQuery] = useState("");
  const [selectedVersion, setSelectedVersion] = useState("");
  const [selectedLoader, setSelectedLoader] = useState("");
  const [sort, setSort] = useState<"downloads" | "relevance">("downloads");
  const [page, setPage] = useState(0);
  const [results, setResults] = useState<ModrinthProject[]>([]);
  const [total, setTotal] = useState(0);
  const [searched, setSearched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [details, setDetails] = useState<ModpackDetails | null>(null);
  const [detailsBusy, setDetailsBusy] = useState(false);
  const [versionId, setVersionId] = useState("");
  const [instanceName, setInstanceName] = useState("");
  const [memoryMb, setMemoryMb] = useState(() => Math.min(4096, memoryLimitMb));
  const [installing, setInstalling] = useState(false);

  async function search(nextPage: number) {
    setBusy(true);
    try {
      const response = await searchModpacks({
        query,
        minecraftVersions: selectedVersion ? [selectedVersion] : [],
        loaders: selectedLoader ? [selectedLoader] : [],
        sort,
        page: nextPage,
      });
      setResults(response.hits);
      setTotal(response.totalHits);
      setPage(nextPage);
      setSearched(true);
    } catch (error) {
      reportError(error);
    } finally {
      setBusy(false);
    }
  }

  useEffect(() => {
    void search(0);
  }, []);

  async function openDetails(project: ModrinthProject) {
    setDetailsBusy(true);
    setDetails(null);
    setVersionId("");
    setInstanceName(defaultInstanceName(project.title));
    try {
      const loaded = await getModpackVersions(project.projectId);
      setDetails(loaded);
      const compatible = loaded.versions.find((version) =>
        (!selectedVersion || version.gameVersions.includes(selectedVersion))
        && (!selectedLoader || version.loaders.includes(selectedLoader)),
      );
      setVersionId((compatible ?? loaded.versions[0])?.versionId ?? "");
    } catch (error) {
      reportError(error);
    } finally {
      setDetailsBusy(false);
    }
  }

  async function submitInstall(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!details || !versionId || !instanceName.trim() || installing) return;
    const version = details.versions.find((item) => item.versionId === versionId);
    if (!version) return;
    setInstalling(true);
    try {
      await onInstall({ name: instanceName.trim(), project: details.project, version, memoryMb });
      setDetails(null);
    } catch (error) {
      reportError(error);
    } finally {
      setInstalling(false);
    }
  }

  const selectedVersionDetails = details?.versions.find((version) => version.versionId === versionId);
  const compatibleVersions = details?.versions.filter((version) =>
    (!selectedVersion || version.gameVersions.includes(selectedVersion))
    && (!selectedLoader || version.loaders.includes(selectedLoader)),
  ) ?? [];

  return (
    <>
      <section className="page-heading">
        <div><div className="eyebrow"><Boxes size={15} /> MODRINTH</div><h1>Сборки</h1><p className="page-subtitle">Найдите сборку и установите её отдельным игровым инстансом.</p></div>
      </section>
      <form className="modpack-toolbar" onSubmit={(event) => { event.preventDefault(); void search(0); }}>
        <label className="mod-search-field"><Search size={17} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Название или описание сборки" aria-label="Поиск сборок" /></label>
        <label className="modpack-filter"><span>Minecraft</span><select value={selectedVersion} onChange={(event) => setSelectedVersion(event.target.value)}><option value="">Все версии</option>{versions.map((version) => <option key={version} value={version}>{version}</option>)}</select></label>
        <label className="modpack-filter"><span>Загрузчик</span><select value={selectedLoader} onChange={(event) => setSelectedLoader(event.target.value)}><option value="">Любой</option>{SUPPORTED_LOADERS.map((loader) => <option key={loader} value={loader}>{loader === "neoforge" ? "NeoForge" : loader[0].toUpperCase() + loader.slice(1)}</option>)}</select></label>
        <label className="modpack-filter"><span>Порядок</span><select value={sort} onChange={(event) => setSort(event.target.value as "downloads" | "relevance")}><option value="downloads">По загрузкам</option><option value="relevance">По релевантности</option></select></label>
        <button className="button-primary" type="submit" disabled={busy}>{busy ? <LoaderCircle className="spin" size={16} /> : <Search size={16} />} Найти</button>
      </form>
      <div className="mod-results-heading">
        <div><h2>Сборки</h2><span>{total.toLocaleString("ru-RU")} найдено</span></div>
        <nav className="mod-pagination" aria-label="Страницы каталога">
          <button className="icon-button" aria-label="Предыдущая страница" title="Предыдущая страница" disabled={busy || page === 0} onClick={() => void search(page - 1)}><ChevronLeft size={16} /></button>
          <span>{page + 1} / {Math.max(1, Math.ceil(total / 20))}</span>
          <button className="icon-button" aria-label="Следующая страница" title="Следующая страница" disabled={busy || (page + 1) * 20 >= total} onClick={() => void search(page + 1)}><ChevronRight size={16} /></button>
        </nav>
      </div>
      {busy ? (
        <div className="mod-catalog-loading"><LoaderCircle className="spin" size={20} /> Загружаем сборки Modrinth...</div>
      ) : results.length ? (
        <section className="modpack-results" aria-label="Результаты поиска сборок">
          {results.map((project) => (
            <article className="mod-catalog-card modpack-card" key={project.projectId}>
              {project.iconUrl ? <img className="mod-project-icon" src={project.iconUrl} alt="" /> : <div className="mod-file-icon"><Boxes size={18} /></div>}
              <div className="mod-copy"><div className="mod-card-title"><strong>{project.title}</strong><span>от {project.author || "автора Modrinth"}</span></div><span className="mod-card-description">{project.description}</span><div className="mod-tags">{project.categories.slice(0, 4).map((category) => <span className="mod-tag" key={category}>{category}</span>)}</div></div>
              <div className="mod-card-actions"><span className="mod-downloads"><Download size={13} /> {project.downloads.toLocaleString("ru-RU")}</span><button className="button-secondary" type="button" onClick={() => void openDetails(project)} disabled={detailsBusy}>{detailsBusy ? <LoaderCircle className="spin" size={15} /> : <Download size={15} />} Версии</button></div>
            </article>
          ))}
        </section>
      ) : searched ? (
        <section className="section-empty mod-catalog-empty"><div className="section-empty-icon"><Boxes size={24} /></div><h2>Сборки не найдены</h2><p>Измените запрос или фильтры.</p></section>
      ) : null}
      {(details || detailsBusy) && createPortal((
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
          onMouseDown={(event) => { if (event.target === event.currentTarget && !installing && !detailsBusy) setDetails(null); }}
        >
          <section className="instance-dialog modpack-dialog" role="dialog" aria-modal="true" aria-labelledby="modpack-dialog-title">
            <div className="dialog-heading"><div><span className="eyebrow">MODRINTH PACK</span><h2 id="modpack-dialog-title">{details?.project.title ?? "Загрузка версий"}</h2></div><button className="dialog-close" type="button" aria-label="Закрыть" disabled={installing || detailsBusy} onClick={() => setDetails(null)}><X size={17} /></button></div>
            {detailsBusy ? <div className="mod-catalog-loading"><LoaderCircle className="spin" size={20} /> Получаем версии...</div> : details && (
              <form onSubmit={(event) => void submitInstall(event)}>
                <label className="form-field"><span>Версия сборки</span><select value={versionId} onChange={(event) => setVersionId(event.target.value)} required><option value="" disabled>Выберите версию</option>{details.versions.map((version) => <option key={version.versionId} value={version.versionId}>{version.name || version.versionNumber} · Minecraft {version.gameVersions.join(", ")} · {version.loaders.join(", ") || "Vanilla"}</option>)}</select></label>
                {selectedVersionDetails && compatibleVersions.length === 0 && <p className="inline-error">Версия не соответствует выбранным фильтрам каталога.</p>}
                {selectedVersionDetails?.loaders.some((loader) => !SUPPORTED_LOADERS.includes(loader as (typeof SUPPORTED_LOADERS)[number])) && <p className="inline-error">Эта версия использует неподдерживаемый загрузчик. Доступны Fabric, Forge и NeoForge.</p>}
                <label className="form-field"><span>Имя инстанса</span><input value={instanceName} onChange={(event) => setInstanceName(event.target.value)} maxLength={64} required pattern={'[^\\/:*?"<>|]+'} /></label>
                <label className="form-field"><span>Память: {memoryMb.toLocaleString("ru-RU")} МБ · максимум {memoryLimitMb.toLocaleString("ru-RU")} МБ</span><input type="range" min="512" max={memoryLimitMb} step="512" value={memoryMb} onChange={(event) => setMemoryMb(Number(event.target.value))} /></label>
                {selectedVersionDetails && <p className="modpack-version-info">Файл: {selectedVersionDetails.fileName}</p>}
                <div className="dialog-actions"><button className="button-secondary" type="button" onClick={() => setDetails(null)} disabled={installing}>Отмена</button><button className="button-primary" type="submit" disabled={installing || !versionId || compatibleVersions.length === 0 || !instanceName.trim() || Boolean(selectedVersionDetails?.loaders.some((loader) => !SUPPORTED_LOADERS.includes(loader as (typeof SUPPORTED_LOADERS)[number])))}>{installing ? <LoaderCircle className="spin" size={16} /> : <Check size={16} />} Установить</button></div>
              </form>
            )}
          </section>
        </div>
      ), document.body)}
    </>
  );
}
