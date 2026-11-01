import {
  Blocks,
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  Download,
  LoaderCircle,
  PackageSearch,
  Power,
  RefreshCw,
  Search,
  Trash2,
} from "lucide-react";
import type { ModCatalogController } from "../hooks/useModCatalog";
import { MOD_CATALOG_CATEGORIES, MOD_CATALOG_LOADERS, MOD_CATALOG_PROJECT_TYPES } from "../types";
import type { Instance } from "../types";

interface ModsViewProps {
  instance: Instance;
  versions: string[];
  catalog: ModCatalogController;
  backLabel: string;
  onBack: () => void;
}

export function ModsView({ instance, versions, catalog, backLabel, onBack }: ModsViewProps) {
  return (
    <>
      <section className="page-heading">
        <div><div className="eyebrow"><PackageSearch size={15} /> MODRINTH</div><h1>Каталог контента</h1><p className="page-subtitle">Для {instance.name} · Minecraft {instance.minecraft_version} · {instance.loader_type}</p></div>
        <button className="button-secondary" onClick={onBack}>{backLabel}</button>
      </section>
      <nav className="catalog-type-tabs" aria-label="Тип контента">
        {MOD_CATALOG_PROJECT_TYPES.map((type) => <button type="button" className={catalog.projectType === type.value ? "active" : ""} key={type.value} onClick={() => catalog.selectProjectType(type.value)}>{type.label}</button>)}
      </nav>
      {catalog.projectType === "mod" && instance.loader_type === "vanilla" ? (
        <section className="error-panel"><PackageSearch size={20} /><div><strong>Для модов нужен загрузчик</strong><p>Измените инстанс и выберите Fabric, Forge или NeoForge.</p></div></section>
      ) : (
        <div className="mod-catalog-layout">
          <aside className="mod-filter-sidebar" aria-label="Фильтры каталога">
            {catalog.projectType === "mod" && <>
              <section className="mod-filter-section">
                <h2>Версия игры</h2>
                <label className="mod-version-search"><Search size={14} /><input value={catalog.versionQuery} onChange={(event) => catalog.setVersionQuery(event.target.value)} placeholder="Поиск версии..." /></label>
                <div className="mod-filter-options mod-version-options">
                  <label className="mod-filter-option"><input type="checkbox" checked={catalog.selectedVersions.length === 0} onChange={() => catalog.setSelectedVersions([])} /><span>Все версии</span></label>
                  {versions.filter((version) => version.toLowerCase().includes(catalog.versionQuery.toLowerCase())).map((version) => <label className="mod-filter-option" key={version}><input type="checkbox" checked={catalog.selectedVersions.includes(version)} onChange={() => catalog.setSelectedVersions((current) => current.includes(version) ? current.filter((item) => item !== version) : [...current, version])} /><span>{version}</span></label>)}
                </div>
              </section>
              <section className="mod-filter-section">
                <h2>Загрузчик</h2>
                <div className="mod-filter-options">
                  <label className="mod-filter-option"><input type="checkbox" checked={catalog.selectedLoaders.length === 0} onChange={() => catalog.setSelectedLoaders([])} /><span>Все загрузчики</span></label>
                  {MOD_CATALOG_LOADERS.map((loader) => <label className="mod-filter-option" key={loader}><input type="checkbox" checked={catalog.selectedLoaders.includes(loader)} onChange={() => catalog.setSelectedLoaders((current) => current.includes(loader) ? current.filter((item) => item !== loader) : [...current, loader])} /><span>{loader === "neoforge" ? "NeoForge" : loader[0].toUpperCase() + loader.slice(1)}</span></label>)}
                </div>
              </section>
              <section className="mod-filter-section">
                <label className="mod-category-label" htmlFor="mod-category">Категория</label>
                <select id="mod-category" value={catalog.selectedCategory} onChange={(event) => catalog.setSelectedCategory(event.target.value)}>
                  <option value="">Все категории</option>
                  {MOD_CATALOG_CATEGORIES.map((category) => <option key={category} value={category}>{category[0].toUpperCase() + category.slice(1)}</option>)}
                </select>
              </section>
            </>}
          </aside>
          <section className="mod-catalog-main" aria-label="Каталог проектов">
            {catalog.projectType === "datapack" && (
              <label className="catalog-world-select">
                <span>Мир для датапака</span>
                <select value={catalog.worldName} onChange={(event) => catalog.setWorldName(event.target.value)} disabled={catalog.worlds.length === 0}>
                  <option value="">{catalog.worlds.length ? "Выберите мир" : "В инстансе нет сохранений"}</option>
                  {catalog.worlds.map((world) => <option key={world} value={world}>{world}</option>)}
                </select>
              </label>
            )}
            <form className="mod-catalog-toolbar" onSubmit={(event) => void catalog.submitSearch(event)}>
              <label className="mod-search-field"><Search size={17} /><input aria-label={`Поиск: ${MOD_CATALOG_PROJECT_TYPES.find((type) => type.value === catalog.projectType)?.label}`} value={catalog.query} onChange={(event) => catalog.setQuery(event.target.value)} placeholder={`Поиск: ${MOD_CATALOG_PROJECT_TYPES.find((type) => type.value === catalog.projectType)?.label.toLowerCase()}...`} /></label>
              <label className="mod-sort-control"><span>Сортировать</span><select value={catalog.sort} onChange={(event) => catalog.setSort(event.target.value as "downloads" | "relevance")} aria-label="Сортировка каталога"><option value="downloads">По загрузкам</option><option value="relevance">По релевантности</option></select></label>
              <button className="button-primary" type="submit" disabled={catalog.busy}>{catalog.busy ? <LoaderCircle className="spin" size={16} /> : <Search size={16} />} Найти</button>
            </form>
            <div className="mod-results-heading">
              <div><h2>Проекты</h2><span>{catalog.resultsTotal.toLocaleString("ru-RU")} найдено</span></div>
              <nav className="mod-pagination" aria-label="Страницы каталога">
                <button className="icon-button" aria-label="Предыдущая страница" title="Предыдущая страница" disabled={catalog.busy || catalog.page === 0} onClick={() => void catalog.search(catalog.page - 1)}><ChevronLeft size={16} /></button>
                <span>{catalog.page + 1} / {Math.max(1, Math.ceil(catalog.resultsTotal / 20))}</span>
                <button className="icon-button" aria-label="Следующая страница" title="Следующая страница" disabled={catalog.busy || (catalog.page + 1) * 20 >= catalog.resultsTotal} onClick={() => void catalog.search(catalog.page + 1)}><ChevronRight size={16} /></button>
              </nav>
            </div>
            {catalog.results.length > 0 ? (
              <div className="mod-catalog-results">
                {catalog.results.map((project) => {
                  const compatibleWithInstance = (catalog.selectedVersions.length === 0 || catalog.selectedVersions.includes(instance.minecraft_version)) && (catalog.projectType !== "mod" || (instance.loader_type !== "vanilla" && project.categories.includes(instance.loader_type) && (catalog.selectedLoaders.length === 0 || catalog.selectedLoaders.includes(instance.loader_type))));
                  const installed = catalog.projectType === "mod" ? catalog.installedMods.some((mod) => mod.projectId === project.projectId) : catalog.installedProjectIds.includes(project.projectId);
                  const canInstall = compatibleWithInstance && (catalog.projectType !== "datapack" || Boolean(catalog.worldName));
                  return (
                    <article className="mod-catalog-card" key={project.projectId}>
                      {project.iconUrl ? <img className="mod-project-icon" src={project.iconUrl} alt="" /> : <div className="mod-file-icon"><PackageSearch size={18} /></div>}
                      <div className="mod-copy"><div className="mod-card-title"><strong>{project.title}</strong><span>от {project.author || "автора Modrinth"}</span></div><span className="mod-card-description">{project.description}</span><div className="mod-tags">{project.categories.slice(0, 4).map((category) => <span className="mod-tag" key={category}>{category}</span>)}</div></div>
                      <div className="mod-card-actions">
                        <span className="mod-downloads"><Download size={13} /> {project.downloads.toLocaleString("ru-RU")}</span>
                        <button className="button-secondary" aria-label={installed ? `${project.title} установлен` : `Установить ${project.title}`} title={installed ? "Уже установлен в этом инстансе" : canInstall ? "Установить в этот инстанс" : catalog.projectType === "datapack" && !catalog.worldName ? "Сначала выберите мир" : "Выбранные фильтры не подходят для этого инстанса"} disabled={catalog.busy || installed || !canInstall} onClick={() => void catalog.install(project)}>{installed ? <CheckCircle2 size={15} /> : <Download size={15} />} {installed ? "Установлен" : "Установить"}</button>
                      </div>
                    </article>
                  );
                })}
              </div>
            ) : catalog.hasSearched && !catalog.busy ? (
              <section className="section-empty mod-catalog-empty"><div className="section-empty-icon"><PackageSearch size={24} /></div><h2>Ничего не найдено</h2><p>Измените запрос или фильтры каталога.</p></section>
            ) : catalog.busy ? (
              <div className="mod-catalog-loading"><LoaderCircle className="spin" size={20} /> Загружаем проекты Modrinth...</div>
            ) : null}
          </section>
        </div>
      )}
      {catalog.projectType === "mod" && catalog.installedMods.length > 0 && (
        <section className="mods-section">
          <div className="mods-section-heading"><h2>Установленные</h2><span>{catalog.installedMods.length}</span></div>
          {catalog.installedMods.map((mod) => (
            <article className="mod-row" key={mod.fileName}>
              <div className="mod-file-icon"><Blocks size={17} /></div>
              <div className="mod-copy"><strong>{mod.fileName.replace(/\.jar(\.disabled)?$/i, "")}</strong><span>{mod.enabled ? "Включён" : "Выключен"}</span></div>
              {mod.projectId && <button className="icon-button" aria-label="Обновить мод" title="Обновить из Modrinth" disabled={catalog.busy} onClick={() => void catalog.update(mod)}><RefreshCw size={16} /></button>}
              <button className="icon-button" aria-label={mod.enabled ? "Выключить мод" : "Включить мод"} title={mod.enabled ? "Выключить" : "Включить"} onClick={() => void catalog.toggle(mod)}><Power size={16} /></button>
              <button className="icon-button danger-button" aria-label="Удалить мод" title="Удалить" onClick={() => void catalog.remove(mod)}><Trash2 size={16} /></button>
            </article>
          ))}
        </section>
      )}
    </>
  );
}
