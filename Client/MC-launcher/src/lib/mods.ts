import { invoke } from "@tauri-apps/api/core";
import type {
  CatalogProjectType,
  InstalledMod,
  Instance,
  ModSearchPage,
  ModVersionOption,
  ModrinthProject,
} from "../types";

export async function searchMods(input: {
  query: string;
  projectType: CatalogProjectType;
  minecraftVersions: string[];
  loaders: readonly string[];
  category: string;
  sort: "downloads" | "relevance";
  page: number;
}): Promise<ModSearchPage> {
  return invoke<ModSearchPage>("search_mods", {
    query: input.query.trim(),
    projectType: input.projectType,
    minecraftVersions: input.minecraftVersions,
    loaders: input.projectType === "mod" ? input.loaders : [],
    category: input.projectType === "mod" ? input.category : "",
    index: input.sort,
    offset: input.page * 20,
  });
}

export async function listInstalledMods(instance: Instance): Promise<InstalledMod[]> {
  return invoke<InstalledMod[]>("list_installed_mods", { folderName: instance.folder_name });
}

export async function listModVersions(instance: Instance, mod: InstalledMod): Promise<ModVersionOption[]> {
  if (!mod.projectId) return [];
  return invoke<ModVersionOption[]>("list_mod_versions", {
    projectId: mod.projectId,
    minecraftVersion: instance.minecraft_version,
    loader: instance.loader_type,
  });
}

export async function listInstanceWorlds(instance: Instance): Promise<string[]> {
  return invoke<string[]>("list_instance_worlds", { folderName: instance.folder_name });
}

export async function listInstalledCatalogProjects(
  instance: Instance,
  projectType: CatalogProjectType,
  worldName: string,
): Promise<string[]> {
  return invoke<string[]>("list_installed_catalog_projects", {
    folderName: instance.folder_name,
    projectType,
    worldName: projectType === "datapack" ? worldName : null,
  });
}

export async function installProject(
  instance: Instance,
  project: ModrinthProject,
  projectType: CatalogProjectType,
  worldName: string,
): Promise<void> {
  if (projectType === "mod") {
    await invoke("install_mod", {
      folderName: instance.folder_name,
      projectId: project.projectId,
      minecraftVersion: instance.minecraft_version,
      loader: instance.loader_type,
    });
    return;
  }
  await invoke("install_catalog_project", {
    folderName: instance.folder_name,
    projectId: project.projectId,
    minecraftVersion: instance.minecraft_version,
    projectType,
    worldName: projectType === "datapack" ? worldName : null,
  });
}

export async function toggleMod(instance: Instance, mod: InstalledMod): Promise<void> {
  await invoke("toggle_mod", {
    folderName: instance.folder_name,
    fileName: mod.fileName,
    enabled: !mod.enabled,
  });
}

export async function updateMod(instance: Instance, mod: InstalledMod, versionId?: string): Promise<void> {
  await invoke("update_mod", {
    folderName: instance.folder_name,
    fileName: mod.fileName,
    minecraftVersion: instance.minecraft_version,
    loader: instance.loader_type,
    versionId: versionId ?? null,
  });
}

export async function removeMod(instance: Instance, mod: InstalledMod): Promise<void> {
  await invoke("remove_mod", { folderName: instance.folder_name, fileName: mod.fileName });
}
