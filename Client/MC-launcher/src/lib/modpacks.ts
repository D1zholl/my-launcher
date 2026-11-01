import { invoke } from "@tauri-apps/api/core";
import type { ModpackDetails, ModpackInstallResult, ModpackSearchPage } from "../types";

export async function searchModpacks(input: {
  query: string;
  minecraftVersions: string[];
  loaders: string[];
  sort: "downloads" | "relevance";
  page: number;
}): Promise<ModpackSearchPage> {
  return invoke<ModpackSearchPage>("search_modpacks", {
    query: input.query.trim(),
    minecraftVersions: input.minecraftVersions,
    loaders: input.loaders,
    index: input.sort,
    offset: input.page * 20,
  });
}

export async function getModpackVersions(projectId: string): Promise<ModpackDetails> {
  return invoke<ModpackDetails>("get_modpack_versions", { projectId });
}

export async function installModpack(input: {
  name: string;
  projectId: string;
  versionId: string;
}): Promise<ModpackInstallResult> {
  return invoke<ModpackInstallResult>("install_modpack", {
    folderName: input.name,
    projectId: input.projectId,
    versionId: input.versionId,
  });
}

export async function cancelModpackInstall(folderName: string): Promise<void> {
  await invoke("cancel_modpack_install", { folderName });
}
