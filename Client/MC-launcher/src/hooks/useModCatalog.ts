import { useEffect, useState } from "react";
import {
  installProject,
  listInstalledCatalogProjects,
  listInstalledMods,
  listInstanceWorlds,
  removeMod,
  searchMods,
  toggleMod,
  updateMod,
} from "../lib/mods";
import { MOD_CATALOG_LOADERS } from "../types";
import type {
  CatalogProjectType,
  InstalledMod,
  Instance,
  ModrinthProject,
  View,
} from "../types";

interface UseModCatalogOptions {
  view: View;
  instance: Instance | undefined;
  reportError: (error: unknown, fallback?: string) => void;
  showNotification: (message: string, kind?: "error" | "success") => void;
}

export function useModCatalog({
  view,
  instance,
  reportError,
  showNotification,
}: UseModCatalogOptions) {
  const [projectType, setProjectType] = useState<CatalogProjectType>("mod");
  const [worlds, setWorlds] = useState<string[]>([]);
  const [worldName, setWorldName] = useState("");
  const [installedProjectIds, setInstalledProjectIds] = useState<string[]>([]);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<ModrinthProject[]>([]);
  const [resultsTotal, setResultsTotal] = useState(0);
  const [page, setPage] = useState(0);
  const [hasSearched, setHasSearched] = useState(false);
  const [selectedVersions, setSelectedVersions] = useState<string[]>([]);
  const [selectedLoaders, setSelectedLoaders] = useState<string[]>([]);
  const [selectedCategory, setSelectedCategory] = useState("");
  const [versionQuery, setVersionQuery] = useState("");
  const [sort, setSort] = useState<"downloads" | "relevance">("downloads");
  const [installedMods, setInstalledMods] = useState<InstalledMod[]>([]);
  const [busy, setBusy] = useState(false);

  const instanceId = instance?.id;

  async function refreshInstalledMods() {
    if (!instance) return;
    setInstalledMods(await listInstalledMods(instance));
  }

  useEffect(() => {
    if ((view !== "mods" && view !== "instance-detail") || !instance) return;
    let active = true;
    listInstalledMods(instance)
      .then((mods) => {
        if (active) setInstalledMods(mods);
      })
      .catch((error: unknown) => {
        if (active) reportError(error);
      });
    return () => {
      active = false;
    };
  }, [view, instanceId, reportError]);

  useEffect(() => {
    if (view !== "mods" || projectType !== "datapack" || !instance) {
      setWorlds([]);
      setWorldName("");
      return;
    }
    let active = true;
    setWorlds([]);
    setWorldName("");
    listInstanceWorlds(instance)
      .then((items) => {
        if (active) setWorlds(items);
      })
      .catch((error: unknown) => {
        if (active) reportError(error);
      });
    return () => {
      active = false;
    };
  }, [view, instanceId, projectType, reportError]);

  useEffect(() => {
    if (view !== "mods" || !instance || projectType === "mod") {
      setInstalledProjectIds([]);
      return;
    }
    if (projectType === "datapack" && !worldName) {
      setInstalledProjectIds([]);
      return;
    }
    let active = true;
    listInstalledCatalogProjects(instance, projectType, worldName)
      .then((projects) => {
        if (active) setInstalledProjectIds(projects);
      })
      .catch((error: unknown) => {
        if (active) reportError(error);
      });
    return () => {
      active = false;
    };
  }, [view, instanceId, projectType, worldName, reportError]);

  async function search(nextPage: number) {
    if (!instance || (projectType === "mod" && instance.loader_type === "vanilla")) return;
    setBusy(true);
    try {
      const response = await searchMods({
        query,
        projectType,
        minecraftVersions: selectedVersions,
        loaders: selectedLoaders,
        category: selectedCategory,
        sort,
        page: nextPage,
      });
      setResults(response.hits);
      setResultsTotal(response.totalHits);
      setPage(nextPage);
      setHasSearched(true);
    } catch (error) {
      reportError(error);
    } finally {
      setBusy(false);
    }
  }

  useEffect(() => {
    if (view !== "mods" || !instance) return;
    void search(0);
    // Intentionally re-search only when the instance or content type changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [view, instanceId, projectType]);

  async function submitSearch(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    await search(0);
  }

  async function install(project: ModrinthProject) {
    if (!instance) return;
    if (projectType === "mod") {
      if (
        instance.loader_type === "vanilla"
        || (selectedVersions.length > 0 && !selectedVersions.includes(instance.minecraft_version))
        || (selectedLoaders.length > 0 && !selectedLoaders.includes(instance.loader_type))
      ) {
        return;
      }
    } else if (selectedVersions.length > 0 && !selectedVersions.includes(instance.minecraft_version)) {
      return;
    }
    if (projectType === "datapack" && !worldName) return;
    setBusy(true);
    try {
      await installProject(instance, project, projectType, worldName);
      if (projectType === "mod") {
        await refreshInstalledMods();
      } else {
        setInstalledProjectIds((current) => [...new Set([...current, project.projectId])]);
      }
      showNotification(`${project.title} установлен`, "success");
    } catch (error) {
      reportError(error);
    } finally {
      setBusy(false);
    }
  }

  async function toggle(mod: InstalledMod) {
    if (!instance) return;
    try {
      await toggleMod(instance, mod);
      await refreshInstalledMods();
    } catch (error) {
      reportError(error);
    }
  }

  async function update(mod: InstalledMod, versionId?: string) {
    if (!instance || !mod.projectId || instance.loader_type === "vanilla") return;
    setBusy(true);
    try {
      await updateMod(instance, mod, versionId);
      await refreshInstalledMods();
    } catch (error) {
      reportError(error);
    } finally {
      setBusy(false);
    }
  }

  async function remove(mod: InstalledMod) {
    if (!instance) return;
    try {
      await removeMod(instance, mod);
      setInstalledMods((current) => current.filter((item) => item.fileName !== mod.fileName));
    } catch (error) {
      reportError(error);
    }
  }

  function selectProjectType(type: CatalogProjectType) {
    setProjectType(type);
    setSelectedLoaders([]);
    setSelectedCategory("");
    setPage(0);
    setResults([]);
    setHasSearched(false);
  }

  function prepareForInstance(target: Instance) {
    setProjectType("mod");
    setQuery("");
    setResults([]);
    setResultsTotal(0);
    setHasSearched(false);
    setPage(0);
    setSelectedVersions([target.minecraft_version]);
    setSelectedLoaders(
      MOD_CATALOG_LOADERS.some((loader) => loader === target.loader_type) ? [target.loader_type] : [],
    );
    setSelectedCategory("");
    setVersionQuery("");
    setSort("downloads");
    setWorldName("");
  }

  return {
    projectType,
    worlds,
    worldName,
    setWorldName,
    installedProjectIds,
    query,
    setQuery,
    results,
    resultsTotal,
    page,
    hasSearched,
    selectedVersions,
    setSelectedVersions,
    selectedLoaders,
    setSelectedLoaders,
    selectedCategory,
    setSelectedCategory,
    versionQuery,
    setVersionQuery,
    sort,
    setSort,
    installedMods,
    busy,
    search,
    submitSearch,
    install,
    toggle,
    update,
    remove,
    selectProjectType,
    prepareForInstance,
  };
}

export type ModCatalogController = ReturnType<typeof useModCatalog>;
