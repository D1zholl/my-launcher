import { invoke } from "@tauri-apps/api/core";
import { getDatabase } from "./database";
import type { Instance, InstanceDraft } from "../types";

const INSTANCE_COLUMNS =
  "INSERT INTO instances (id, name, folder_name, minecraft_version, loader_type, loader_version, java_version, java_path, memory_mb, jvm_args, window_width, window_height, fullscreen, avatar_data, modpack_project_id, modpack_version_id, modpack_version_number, repository_url, repository_pack_id, repository_version, repository_channel) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21)";

export async function registerModpackInstance(input: {
  name: string;
  minecraftVersion: string;
  loaderType: string;
  loaderVersion: string | null;
  javaVersion: number;
  javaPath: string;
  memoryMb: number;
  projectId: string;
  versionId: string;
  versionNumber: string;
}): Promise<Instance> {
  const id = crypto.randomUUID();
  const database = await getDatabase();
  await database.execute(
    "INSERT INTO instances (id, name, folder_name, minecraft_version, loader_type, loader_version, java_version, java_path, memory_mb, jvm_args, window_width, window_height, fullscreen, avatar_data, modpack_project_id, modpack_version_id, modpack_version_number) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
    [
      id,
      input.name,
      input.name,
      input.minecraftVersion,
      input.loaderType,
      input.loaderVersion,
      input.javaVersion,
      input.javaPath,
      input.memoryMb,
      "",
      1280,
      720,
      0,
      null,
      input.projectId,
      input.versionId,
      input.versionNumber,
    ],
  );
  return {
    id,
    name: input.name,
    folder_name: input.name,
    minecraft_version: input.minecraftVersion,
    loader_type: input.loaderType,
    loader_version: input.loaderVersion,
    java_version: input.javaVersion,
    java_path: input.javaPath,
    memory_mb: input.memoryMb,
    jvm_args: "",
    window_width: 1280,
    window_height: 720,
    fullscreen: 0,
    avatar_data: null,
    modpack_project_id: input.projectId,
    modpack_version_id: input.versionId,
    modpack_version_number: input.versionNumber,
    created_at: new Date().toISOString(),
  };
}

export async function registerRepositoryInstance(input: {
  name: string;
  minecraftVersion: string;
  loaderType: string;
  loaderVersion: string | null;
  javaVersion: number;
  javaPath: string;
  memoryMb: number | null;
  jvmArgs: string | null;
  windowWidth: number | null;
  windowHeight: number | null;
  fullscreen: boolean | null;
  repositoryUrl: string;
  packId: string;
  version: string;
  channel: string;
}): Promise<Instance> {
  const id = crypto.randomUUID();
  const database = await getDatabase();
  const memoryMb = input.memoryMb ?? 4096;
  const jvmArgs = input.jvmArgs ?? "";
  const windowWidth = input.windowWidth ?? 1280;
  const windowHeight = input.windowHeight ?? 720;
  const fullscreen = input.fullscreen ? 1 : 0;
  await database.execute(
    "INSERT INTO instances (id, name, folder_name, minecraft_version, loader_type, loader_version, java_version, java_path, memory_mb, jvm_args, window_width, window_height, fullscreen, avatar_data, modpack_project_id, modpack_version_id, modpack_version_number, repository_url, repository_pack_id, repository_version, repository_channel) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21)",
    [id, input.name, input.name, input.minecraftVersion, input.loaderType, input.loaderVersion,
      input.javaVersion, input.javaPath, memoryMb, jvmArgs, windowWidth, windowHeight,
      fullscreen, null, null, null, null, input.repositoryUrl, input.packId, input.version, input.channel],
  );
  return {
    id, name: input.name, folder_name: input.name, minecraft_version: input.minecraftVersion,
    loader_type: input.loaderType, loader_version: input.loaderVersion, java_version: input.javaVersion,
    java_path: input.javaPath, memory_mb: memoryMb, jvm_args: jvmArgs, window_width: windowWidth,
    window_height: windowHeight, fullscreen, avatar_data: null, repository_url: input.repositoryUrl,
    repository_pack_id: input.packId, repository_version: input.version, repository_channel: input.channel, created_at: new Date().toISOString(),
  };
}

export async function saveRepositoryVersion(instanceId: string, version: string, channel: string, settings?: {
  memoryMb: number | null;
  jvmArgs: string | null;
  windowWidth: number | null;
  windowHeight: number | null;
  fullscreen: boolean | null;
}): Promise<void> {
  const database = await getDatabase();
  if (!settings) {
    await database.execute("UPDATE instances SET repository_version = $1, repository_channel = $2 WHERE id = $3", [version, channel, instanceId]);
    return;
  }
  await database.execute(
    "UPDATE instances SET repository_version = $1, repository_channel = $2, memory_mb = COALESCE($3, memory_mb), jvm_args = COALESCE($4, jvm_args), window_width = COALESCE($5, window_width), window_height = COALESCE($6, window_height), fullscreen = COALESCE($7, fullscreen) WHERE id = $8",
    [version, channel, settings.memoryMb, settings.jvmArgs, settings.windowWidth, settings.windowHeight, settings.fullscreen === null ? null : Number(settings.fullscreen), instanceId],
  );
}

async function persistAvatar(instanceId: string, avatarData: string | null): Promise<string | null> {
  if (avatarData?.startsWith("data:image/")) {
    return invoke<string>("save_instance_avatar", { instanceId, dataUrl: avatarData });
  }
  return avatarData;
}

async function removeAvatarIfUnused(avatarData: string | null) {
  if (!avatarData || avatarData.startsWith("data:")) return;
  try {
    const database = await getDatabase();
    const references = await database.select<{ count: number }[]>(
      "SELECT COUNT(*) AS count FROM instances WHERE avatar_data = $1",
      [avatarData],
    );
    if (references[0]?.count === 0) {
      await invoke("remove_instance_avatar", { avatarPath: avatarData });
    }
  } catch {
    return;
  }
}

function draftValues(draft: InstanceDraft, avatarData = draft.avatarData) {
  return [
    draft.name,
    draft.name,
    draft.minecraftVersion,
    draft.loaderType,
    draft.loaderVersion,
    draft.javaVersion ?? 21,
    draft.javaPath ?? null,
    draft.memoryMb,
    draft.jvmArgs,
    draft.windowWidth,
    draft.windowHeight,
    draft.fullscreen ? 1 : 0,
    avatarData,
    null,
    null,
    null,
    null,
    null,
    null,
    "stable",
  ];
}

export async function createInstance(draft: InstanceDraft): Promise<Instance> {
  await invoke<string>("create_instance_directory", {
    folderName: draft.name,
    minecraftVersion: draft.minecraftVersion,
    loaderType: draft.loaderType,
    loaderVersion: draft.loaderVersion,
  });
  const id = crypto.randomUUID();
  let avatarData = draft.avatarData;
  try {
    avatarData = await persistAvatar(id, avatarData);
    const database = await getDatabase();
    const result = await database.execute(INSTANCE_COLUMNS, [id, ...draftValues(draft, avatarData)]);
    if (result.rowsAffected !== 1) throw new Error("Не удалось сохранить инстанс в базе данных.");
  } catch (error) {
    try {
      await invoke("delete_instance_directory", { folderName: draft.name });
    } catch (cleanupError) {
      await removeAvatarIfUnused(avatarData);
      throw new Error(`Не удалось сохранить инстанс (${String(error)}) и удалить его папку (${String(cleanupError)}).`, { cause: cleanupError });
    }
    await removeAvatarIfUnused(avatarData);
    throw error;
  }
  return {
    id,
    name: draft.name,
    folder_name: draft.name,
    minecraft_version: draft.minecraftVersion,
    loader_type: draft.loaderType,
    loader_version: draft.loaderVersion,
    java_version: draft.javaVersion ?? 21,
    java_path: draft.javaPath ?? null,
    memory_mb: draft.memoryMb,
    jvm_args: draft.jvmArgs,
    window_width: draft.windowWidth,
    window_height: draft.windowHeight,
    fullscreen: draft.fullscreen ? 1 : 0,
    avatar_data: avatarData,
    created_at: new Date().toISOString(),
  };
}

export async function updateInstance(existing: Instance, draft: InstanceDraft): Promise<Instance> {
  const avatarData = await persistAvatar(existing.id, draft.avatarData);
  try {
    await invoke("update_instance_directory", {
      oldFolderName: existing.folder_name,
      newFolderName: draft.name,
      minecraftVersion: draft.minecraftVersion,
      loaderType: draft.loaderType,
      loaderVersion: draft.loaderVersion,
    });
  } catch (error) {
    await removeAvatarIfUnused(avatarData);
    throw error;
  }
  try {
    const database = await getDatabase();
    const values = draftValues(draft, avatarData);
    const result = await database.execute(
      "UPDATE instances SET name = $1, folder_name = $2, minecraft_version = $3, loader_type = $4, loader_version = $5, java_version = $6, java_path = $7, memory_mb = $8, jvm_args = $9, window_width = $10, window_height = $11, fullscreen = $12, avatar_data = $13 WHERE id = $14",
      [...values.slice(0, 13), existing.id],
    );
    if (result.rowsAffected !== 1) throw new Error("Запись инстанса не найдена в базе данных.");
  } catch (error) {
    try {
      await invoke("update_instance_directory", {
        oldFolderName: draft.name,
        newFolderName: existing.folder_name,
        minecraftVersion: existing.minecraft_version,
        loaderType: existing.loader_type,
        loaderVersion: existing.loader_version,
      });
    } catch (rollbackError) {
      await removeAvatarIfUnused(avatarData);
      throw new Error(`Не удалось сохранить изменения (${String(error)}) и восстановить имя папки (${String(rollbackError)}).`, { cause: rollbackError });
    }
    await removeAvatarIfUnused(avatarData);
    throw error;
  }
  if (existing.avatar_data !== avatarData) await removeAvatarIfUnused(existing.avatar_data ?? null);
  return {
    ...existing,
    name: draft.name,
    folder_name: draft.name,
    minecraft_version: draft.minecraftVersion,
    loader_type: draft.loaderType,
    loader_version: draft.loaderVersion,
    java_version: draft.javaVersion ?? existing.java_version,
    java_path: draft.javaPath ?? existing.java_path,
    memory_mb: draft.memoryMb,
    jvm_args: draft.jvmArgs,
    window_width: draft.windowWidth,
    window_height: draft.windowHeight,
    fullscreen: draft.fullscreen ? 1 : 0,
    avatar_data: avatarData,
  };
}

export async function deleteInstance(instance: Instance): Promise<void> {
  const database = await getDatabase();
  const result = await database.execute("DELETE FROM instances WHERE id = $1", [instance.id]);
  if (result.rowsAffected !== 1) throw new Error("Запись инстанса не найдена в базе данных.");
  try {
    await invoke("delete_instance_directory", { folderName: instance.folder_name });
  } catch (error) {
    try {
      await database.execute(INSTANCE_COLUMNS, [
        instance.id,
        instance.name,
        instance.folder_name,
        instance.minecraft_version,
        instance.loader_type,
        instance.loader_version,
        instance.java_version,
        instance.java_path,
        instance.memory_mb,
        instance.jvm_args,
        instance.window_width,
        instance.window_height,
        instance.fullscreen,
        instance.avatar_data,
        instance.modpack_project_id ?? null,
        instance.modpack_version_id ?? null,
        instance.modpack_version_number ?? null,
        instance.repository_url ?? null,
        instance.repository_pack_id ?? null,
        instance.repository_version ?? null,
        instance.repository_channel ?? "stable",
      ]);
    } catch (restoreError) {
      throw new Error(`Не удалось удалить папку (${String(error)}) и восстановить запись в базе (${String(restoreError)}).`, { cause: restoreError });
    }
    throw error;
  }
  await removeAvatarIfUnused(instance.avatar_data ?? null);
}

export async function duplicateInstance(instance: Instance, name: string): Promise<Instance> {
  await invoke("duplicate_instance_directory", {
    sourceName: instance.folder_name,
    destinationName: name,
  });
  const id = crypto.randomUUID();
  try {
    const database = await getDatabase();
    const result = await database.execute(INSTANCE_COLUMNS, [
        id,
        name,
        name,
        instance.minecraft_version,
        instance.loader_type,
        instance.loader_version,
        instance.java_version,
        instance.java_path,
        instance.memory_mb,
        instance.jvm_args,
        instance.window_width,
        instance.window_height,
        instance.fullscreen,
        instance.avatar_data,
        instance.modpack_project_id ?? null,
        instance.modpack_version_id ?? null,
        instance.modpack_version_number ?? null,
        instance.repository_url ?? null,
        instance.repository_pack_id ?? null,
        instance.repository_version ?? null,
        instance.repository_channel ?? "stable",
      ]);
    if (result.rowsAffected !== 1) throw new Error("Не удалось сохранить копию в базе данных.");
  } catch (error) {
    try {
      await invoke("delete_instance_directory", { folderName: name });
    } catch (cleanupError) {
      throw new Error(`Не удалось сохранить копию (${String(error)}) и удалить её папку (${String(cleanupError)}).`, { cause: cleanupError });
    }
    throw error;
  }
  return { ...instance, id, name, folder_name: name, created_at: new Date().toISOString() };
}

export async function ensureJavaRuntime(
  instance: Instance,
  requiredVersion: number,
): Promise<string> {
  const javaPath = await invoke<string>("install_java_runtime", { version: requiredVersion });
  if (instance.java_version !== requiredVersion || instance.java_path !== javaPath) {
    const database = await getDatabase();
    await database.execute("UPDATE instances SET java_version = $1, java_path = $2 WHERE id = $3", [
      requiredVersion,
      javaPath,
      instance.id,
    ]);
  }
  return javaPath;
}
