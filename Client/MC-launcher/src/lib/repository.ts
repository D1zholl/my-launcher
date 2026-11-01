import { invoke } from "@tauri-apps/api/core";
import type { RepositoryInstallResult, RepositoryPack, RepositoryPackVersion } from "../types";

export function listRepositoryPacks(repository: string, channel: string): Promise<RepositoryPack[]> {
  return invoke("list_repository_packs", { repository, channel });
}

export function listRepositoryVersions(repository: string, packId: string, channel: string): Promise<RepositoryPackVersion[]> {
  return invoke("list_repository_versions", { repository, packId, channel });
}

export function installRepositoryPack(input: {
  name: string;
  repository: string;
  packId: string;
  version: string;
  channel: string;
}): Promise<RepositoryInstallResult> {
  return invoke("install_repository_pack", {
    folderName: input.name,
    repository: input.repository,
    packId: input.packId,
    version: input.version,
    channel: input.channel,
  });
}

export function updateRepositoryPack(input: {
  name: string;
  repository: string;
  packId: string;
  version: string;
  channel: string;
}): Promise<RepositoryInstallResult> {
  return invoke("update_repository_pack", {
    folderName: input.name,
    repository: input.repository,
    packId: input.packId,
    version: input.version,
    channel: input.channel,
  });
}