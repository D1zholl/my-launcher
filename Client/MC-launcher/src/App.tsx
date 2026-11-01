import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

import { Sidebar } from "./components/Sidebar";
import { Topbar } from "./components/Topbar";
import { Toast } from "./components/Toast";
import { InstanceDialog } from "./components/InstanceDialog";
import { ActionDialog } from "./components/ActionDialog";
import type { ActionDialogOptions, ActionDialogRequest } from "./components/ActionDialog";
import { InstancesView } from "./views/InstancesView";
import { AccountsView } from "./views/AccountsView";
import { ModsView } from "./views/ModsView";
import { SettingsView } from "./views/SettingsView";
import { InstanceDetailView } from "./views/InstanceDetailView";
import { ServersView } from "./views/ServersView";
import { AboutView } from "./views/AboutView";
import { ModpacksView } from "./views/ModpacksView";
import { RepositoryView } from "./views/RepositoryView";
import { useMinecraftVersions } from "./hooks/useMinecraftVersions";
import { useLoaderOptions } from "./hooks/useLoaderOptions";
import { useModCatalog } from "./hooks/useModCatalog";
import { useNotifications } from "./hooks/useNotifications";
import { clearActiveAccount, initializeDatabase, saveSetting } from "./lib/database";
import {
  completeMicrosoftAccount,
  createOfflineAccount,
  deleteAccount as deleteAccountRecord,
  requestMicrosoftDeviceCode,
  saveMicrosoftClientId as persistMicrosoftClientId,
  selectAccount as persistActiveAccount,
} from "./lib/accounts";
import {
  createInstance,
  deleteInstance as deleteInstanceRecord,
  duplicateInstance as duplicateInstanceRecord,
  ensureJavaRuntime,
  registerModpackInstance,
  registerRepositoryInstance,
  saveRepositoryVersion,
  updateInstance,
} from "./lib/instances";
import { cancelModpackInstall, installModpack } from "./lib/modpacks";
import { installRepositoryPack, listRepositoryVersions, updateRepositoryPack } from "./lib/repository";
import { openExternalUrl } from "./lib/external";
import { clampMemory, EMPTY_INSTANCE_FORM } from "./lib/instanceForm";
import { DEFAULT_UI_PALETTES, isLightPalette, themeClassName } from "./lib/palettes";
import { errorMessage } from "./lib/common";
import type {
  Account,
  DatabaseStatus,
  DeviceCodeInfo,
  DownloadProgress,
  GameExit,
  GameLog,
  InstallStage,
  Instance,
  InstanceDetailTab,
  InstanceDraft,
  InstanceForm,
  PaletteColor,
  ServerInstallRequest,
  ServerSummary,
  Theme,
  ThemePalettes,
  ModpackInstallTask,
  ModrinthProject,
  ModpackVersion,
  RepositoryPack,
  RepositoryPackVersion,
  PendingRepositoryUpdate,
  View,
} from "./types";

function App() {
  const [view, setView] = useState<View>("instances");
  const [aboutReturnView, setAboutReturnView] = useState<View>("instances");
  const [theme, setTheme] = useState<Theme>("light");
  const [uiPalettes, setUiPalettes] = useState<ThemePalettes>(DEFAULT_UI_PALETTES);
  const [databaseStatus, setDatabaseStatus] = useState<DatabaseStatus>("loading");
  const [databaseError, setDatabaseError] = useState("");
  const [instances, setInstances] = useState<Instance[]>([]);
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [activeAccountId, setActiveAccountId] = useState("");
  const [microsoftClientId, setMicrosoftClientId] = useState("");
  const [deviceCode, setDeviceCode] = useState<DeviceCodeInfo | null>(null);
  const [microsoftBusy, setMicrosoftBusy] = useState(false);
  const [offlineUsername, setOfflineUsername] = useState("");
  const [gameLogs, setGameLogs] = useState<GameLog[]>([]);
  const [runningInstanceId, setRunningInstanceId] = useState<string | null>(null);
  const [modsInstanceId, setModsInstanceId] = useState("");
  const [detailInstanceId, setDetailInstanceId] = useState("");
  const [detailTab, setDetailTab] = useState<InstanceDetailTab>("content");
  const [catalogReturnView, setCatalogReturnView] = useState<"instances" | "instance-detail">("instances");
  const [instanceForm, setInstanceForm] = useState<InstanceForm>(EMPTY_INSTANCE_FORM);
  const [createDialogOpen, setCreateDialogOpen] = useState(false);
  const [editingInstance, setEditingInstance] = useState<Instance | null>(null);
  const [actionDialog, setActionDialog] = useState<ActionDialogRequest | null>(null);
  const [memoryLimitMb, setMemoryLimitMb] = useState(8192);
  const [busy, setBusy] = useState(false);
  const [busyInstanceIds, setBusyInstanceIds] = useState<Set<string>>(() => new Set());
  const [installedInstanceIds, setInstalledInstanceIds] = useState<Set<string>>(() => new Set());
  const [downloadingInstanceId, setDownloadingInstanceId] = useState<string | null>(null);
  const [downloadProgress, setDownloadProgress] = useState<DownloadProgress | null>(null);
  const [installStage, setInstallStage] = useState<InstallStage | null>(null);
  const [taskTrayOpen, setTaskTrayOpen] = useState(false);
  const [serverInstallTask, setServerInstallTask] = useState<ServerInstallRequest | null>(null);
  const [modpackInstallTask, setModpackInstallTask] = useState<ModpackInstallTask | null>(null);
  const notifiedRepositoryUpdates = useRef(new Map<string, string>());

  const { notification, showNotification, reportError, dismissNotification } = useNotifications();
  const { versions, latest, versionError } = useMinecraftVersions();
  const {
    availableLoaderTypes,
    availabilityFor,
    loaderAvailabilityLoading,
    loaderVersions,
    loaderVersionsFor,
    loaderLoading,
    loaderError,
  } = useLoaderOptions(instanceForm.version, instanceForm.loader);
  const modsInstance = instances.find((instance) => instance.id === modsInstanceId);
  const detailInstance = instances.find((instance) => instance.id === detailInstanceId);
  const catalog = useModCatalog({ view, instance: modsInstance, reportError, showNotification });
  const activeBusyInstanceIds = new Set(busyInstanceIds);
  if (catalog.busy && modsInstanceId) activeBusyInstanceIds.add(modsInstanceId);
  const installationCheckKey = JSON.stringify(instances.map(({ id, folder_name, minecraft_version }) => [id, folder_name, minecraft_version]));
  const repositoryUpdateCheckKey = JSON.stringify(instances
    .filter((instance) => instance.repository_url && instance.repository_pack_id)
    .map((instance) => [instance.id, instance.repository_url, instance.repository_pack_id, instance.repository_version, instance.repository_channel]));

  function updateForm(patch: Partial<InstanceForm>) {
    setInstanceForm((current) => ({ ...current, ...patch }));
  }

  function markInstanceBusy(instanceId: string, value: boolean) {
    setBusyInstanceIds((current) => {
      const next = new Set(current);
      if (value) next.add(instanceId);
      else next.delete(instanceId);
      return next;
    });
  }

  function requestAction(options: ActionDialogOptions): Promise<string | null> {
    return new Promise((resolve) => {
      setActionDialog({
        ...options,
        onConfirm: resolve,
        onCancel: () => resolve(null),
      });
    });
  }

  async function installServerInBackground(request: ServerInstallRequest): Promise<ServerSummary | null> {
    if (serverInstallTask) {
      reportError(new Error("Дождитесь завершения текущей установки."));
      return null;
    }
    setServerInstallTask(request);
    setTaskTrayOpen(true);
    try {
      const created = await invoke<ServerSummary>("install_server", {
        name: request.name,
        minecraftVersion: request.minecraftVersion,
        core: request.core,
        loaderVersion: request.loaderVersion,
        memoryMb: request.memoryMb,
        eulaAccepted: request.eulaAccepted,
      });
      showNotification(`Сервер ${created.name} установлен.`, "success");
      return created;
    } catch (error) {
      if (errorMessage(error) === "Установка сервера отменена.") showNotification("Установка сервера отменена.", "success");
      else reportError(error);
      return null;
    } finally {
      setServerInstallTask(null);
    }
  }

  async function cancelServerInstallation() {
    if (!serverInstallTask) return;
    try {
      await invoke("cancel_server_install", { name: serverInstallTask.name });
    } catch (error) {
      reportError(error);
    }
  }

  async function cancelGameInstallation() {
    const instance = instances.find((item) => item.id === downloadingInstanceId);
    if (!instance) return;
    try {
      await invoke("cancel_instance_install", { folderName: instance.folder_name });
    } catch (error) {
      reportError(error);
    }
  }

  async function installModpackInstance(input: { name: string; project: ModrinthProject; version: ModpackVersion; memoryMb: number }) {
    if (modpackInstallTask || databaseStatus !== "ready") {
      throw new Error("Дождитесь готовности хранилища и завершения текущей задачи.");
    }
    const task = { folderName: input.name, projectTitle: input.project.title };
    setModpackInstallTask(task);
    setTaskTrayOpen(true);
    setDownloadProgress(null);
    try {
      const installed = await installModpack({
        name: input.name,
        projectId: input.project.projectId,
        versionId: input.version.versionId,
      });
      let instance: Instance;
      try {
        instance = await registerModpackInstance({
          name: input.name,
          minecraftVersion: installed.minecraftVersion,
          loaderType: installed.loaderType,
          loaderVersion: installed.loaderVersion,
          javaVersion: installed.javaVersion,
          javaPath: installed.javaPath,
          memoryMb: clampMemory(input.memoryMb, memoryLimitMb),
          projectId: installed.modpackProjectId,
          versionId: installed.modpackVersionId,
          versionNumber: installed.modpackVersionNumber,
        });
      } catch (error) {
        await invoke("delete_instance_directory", { folderName: input.name }).catch(() => undefined);
        throw error;
      }
      setInstances((current) => [instance, ...current]);
      setInstalledInstanceIds((current) => new Set(current).add(instance.id));
      setView("instances");
      showNotification(`Сборка ${input.project.title} установлена.`, "success");
    } finally {
      setModpackInstallTask(null);
      setDownloadProgress(null);
    }
  }

  async function cancelModpackInstallation() {
    if (!modpackInstallTask) return;
    try {
      await cancelModpackInstall(modpackInstallTask.folderName);
    } catch (error) {
      reportError(error);
    }
  }

  async function installRepositoryPackInstance(input: { name: string; repository: string; pack: RepositoryPack; version: RepositoryPackVersion }) {
    if (modpackInstallTask || databaseStatus !== "ready") {
      throw new Error("Дождитесь готовности хранилища и завершения текущей задачи.");
    }
    setModpackInstallTask({ folderName: input.name, projectTitle: input.pack.name });
    setTaskTrayOpen(true);
    setDownloadProgress(null);
    try {
      const installed = await installRepositoryPack({
        name: input.name,
        repository: input.repository,
        packId: input.pack.id,
        version: input.version.version,
        channel: input.version.channel,
      });
      let instance: Instance;
      try {
        instance = await registerRepositoryInstance({
          name: input.name,
          minecraftVersion: installed.minecraftVersion,
          loaderType: installed.loaderType,
          loaderVersion: installed.loaderVersion,
          javaVersion: installed.javaVersion,
          javaPath: installed.javaPath,
          memoryMb: clampMemory(installed.memoryMb ?? 4096, memoryLimitMb),
          jvmArgs: installed.jvmArgs,
          windowWidth: installed.windowWidth,
          windowHeight: installed.windowHeight,
          fullscreen: installed.fullscreen,
          repositoryUrl: installed.repositoryUrl,
          packId: installed.packId,
          version: installed.version,
          channel: installed.channel,
        });
      } catch (error) {
        await invoke("delete_instance_directory", { folderName: input.name }).catch(() => undefined);
        throw error;
      }
      setInstances((current) => [instance, ...current]);
      setInstalledInstanceIds((current) => new Set(current).add(instance.id));
      setView("instances");
      showNotification(`Сборка ${input.pack.name} установлена.`, "success");
    } finally {
      setModpackInstallTask(null);
      setDownloadProgress(null);
    }
  }

  async function updateRepositoryInstance(instance: Instance) {
    if (!instance.repository_url || !instance.repository_pack_id || modpackInstallTask || busyInstanceIds.has(instance.id) || runningInstanceId === instance.id) return;
    setModpackInstallTask({ folderName: instance.folder_name, projectTitle: instance.name });
    markInstanceBusy(instance.id, true);
    setTaskTrayOpen(true);
    setDownloadProgress(null);
    try {
      const channel = instance.repository_channel ?? "stable";
      const versions = await listRepositoryVersions(instance.repository_url, instance.repository_pack_id, channel);
      const latestVersion = versions[0]?.version;
      if (!latestVersion) throw new Error(`Опубликованная версия канала ${channel} не найдена.`);
      if (latestVersion === instance.repository_version) {
        showNotification(`Установлена последняя версия канала ${channel}.`, "success");
        return;
      }
      const confirmation = await requestAction({
        title: "Обновить сборку?",
        message: "Обновление может заменить изменённые файлы и удалить файлы, которых больше нет в новой версии.",
        confirmLabel: "Обновить",
        danger: true,
      });
      if (confirmation === null) return;
      const updated = await updateRepositoryPack({
        name: instance.folder_name,
        repository: instance.repository_url,
        packId: instance.repository_pack_id,
        version: latestVersion,
        channel,
      });
      const updatedMemoryMb = updated.memoryMb === null
        ? null
        : clampMemory(updated.memoryMb, memoryLimitMb);
      try {
        await saveRepositoryVersion(instance.id, updated.version, updated.channel, {
          ...updated,
          memoryMb: updatedMemoryMb,
        });
      } catch (databaseError) {
        if (updated.transactionId) {
          try {
            await invoke("rollback_repository_update", { transactionId: updated.transactionId });
          } catch (rollbackError) {
            throw new Error(`Не удалось сохранить версию в базе (${String(databaseError)}) и восстановить файлы (${String(rollbackError)}).`, { cause: rollbackError });
          }
        }
        throw databaseError;
      }
      if (updated.transactionId) {
        await invoke("finalize_repository_update", { transactionId: updated.transactionId }).catch(() => undefined);
      }
      setInstances((current) => current.map((item) => item.id === instance.id ? {
        ...item,
        repository_version: updated.version,
        repository_channel: updated.channel,
        memory_mb: updatedMemoryMb ?? item.memory_mb,
        jvm_args: updated.jvmArgs ?? item.jvm_args,
        window_width: updated.windowWidth ?? item.window_width,
        window_height: updated.windowHeight ?? item.window_height,
        fullscreen: updated.fullscreen === null ? item.fullscreen : Number(updated.fullscreen),
      } : item));
      showNotification(`Сборка ${instance.name} обновлена до ${updated.version}.`, "success");
    } catch (error) {
      reportError(error);
    } finally {
      markInstanceBusy(instance.id, false);
      setModpackInstallTask(null);
      setDownloadProgress(null);
    }
  }

  useEffect(() => {
    let active = true;
    initializeDatabase()
      .then(async (state) => {
        try {
          const pendingUpdates = await invoke<PendingRepositoryUpdate[]>("list_pending_repository_updates");
          for (const update of pendingUpdates) {
            const instance = state.instances.find((item) => item.folder_name === update.instanceFolder);
            const command = instance?.repository_version === update.version
              ? "finalize_repository_update"
              : "rollback_repository_update";
            await invoke(command, { transactionId: update.transactionId }).catch(reportError);
          }
        } catch (error) {
          reportError(error);
        }
        if (!active) return;
        setAccounts(state.accounts);
        setActiveAccountId(state.activeAccountId);
        setMicrosoftClientId(state.microsoftClientId);
        setInstances(state.instances);
        setTheme(state.theme);
        setUiPalettes(state.uiPalettes);
        setDatabaseStatus("ready");
      })
      .catch((error: unknown) => {
        if (!active) return;
        setDatabaseError(errorMessage(error, "Не удалось открыть базу данных."));
        setDatabaseStatus("error");
      });
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    invoke<number>("get_memory_limit_mb")
      .then((limit) => {
        setMemoryLimitMb(limit);
        setInstanceForm((current) => ({ ...current, memoryMb: clampMemory(current.memoryMb, limit) }));
      })
      .catch(() => undefined);
  }, []);

  useEffect(() => {
    let active = true;
    let stopLogListener: (() => void) | undefined;
    let stopExitListener: (() => void) | undefined;
    Promise.all([
      listen<GameLog>("game-log", ({ payload }) => {
        if (active) setGameLogs((current) => [...current.slice(-499), payload]);
      }),
      listen<GameExit>("game-exit", ({ payload }) => {
        if (!active) return;
        setRunningInstanceId(null);
        setGameLogs((current) => [...current.slice(-499), {
          folderName: payload.folderName,
          stream: payload.success ? "stdout" : "stderr",
          line: `Процесс завершился с кодом ${payload.code ?? "unknown"}.`,
        }]);
      }),
    ])
      .then(([stopLogs, stopExit]) => {
        if (active) {
          stopLogListener = stopLogs;
          stopExitListener = stopExit;
        } else {
          stopLogs();
          stopExit();
        }
      })
      .catch(reportError);
    return () => {
      active = false;
      stopLogListener?.();
      stopExitListener?.();
    };
  }, [reportError]);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    listen<DownloadProgress>("download-progress", ({ payload }) => {
      if (active) setDownloadProgress(payload);
    })
      .then((stopListening) => {
        if (active) unlisten = stopListening;
        else stopListening();
      })
      .catch(reportError);
    return () => {
      active = false;
      unlisten?.();
    };
  }, [reportError]);

  useEffect(() => {
    let active = true;
    invoke<string[]>("list_installed_instance_ids", {
      instances: instances.map((instance) => ({
        id: instance.id,
        folderName: instance.folder_name,
        version: instance.minecraft_version,
      })),
    }).then((ids) => {
      if (!active) return;
      setInstalledInstanceIds(new Set(ids));
    }).catch(() => {
      if (active) setInstalledInstanceIds(new Set());
    });
    return () => {
      active = false;
    };
  }, [installationCheckKey]);

  useEffect(() => {
    if (databaseStatus !== "ready") return;
    let active = true;
    let checking = false;
    const checkUpdates = async () => {
      if (checking) return;
      checking = true;
      const updates = await Promise.all(instances
        .filter((instance) => instance.repository_url && instance.repository_pack_id)
        .map(async (instance) => {
          try {
            const versions = await listRepositoryVersions(
              instance.repository_url!,
              instance.repository_pack_id!,
              instance.repository_channel ?? "stable",
            );
            const version = versions[0]?.version;
            return version && version !== instance.repository_version ? { instance, version } : null;
          } catch {
            return null;
          }
        }));
      checking = false;
      if (!active) return;
      const newUpdates = updates.filter((item): item is NonNullable<typeof item> => {
        if (!item || notifiedRepositoryUpdates.current.get(item.instance.id) === item.version) return false;
        notifiedRepositoryUpdates.current.set(item.instance.id, item.version);
        return true;
      });
      if (newUpdates.length) {
        showNotification(
          `Доступно обновление сборки: ${newUpdates.map(({ instance, version }) => `${instance.name} ${version}`).join(", ")}.`,
          "success",
        );
      }
    };
    void checkUpdates();
    const interval = window.setInterval(() => { void checkUpdates(); }, 30 * 60 * 1000);
    return () => {
      active = false;
      window.clearInterval(interval);
    };
  }, [databaseStatus, repositoryUpdateCheckKey, showNotification]);

  useEffect(() => {
    if (!detailInstance) return;
    let active = true;
    invoke<GameLog[]>("read_game_logs", { folderName: detailInstance.folder_name })
      .then((entries) => { if (active) setGameLogs(entries); })
      .catch(reportError);
    return () => { active = false; };
  }, [detailInstance?.folder_name, reportError]);

  useEffect(() => {
    if (!latest) return;
    setInstanceForm((current) => (current.version ? current : { ...current, version: latest }));
  }, [latest]);

  useEffect(() => {
    if (loaderAvailabilityLoading || availabilityFor !== instanceForm.version) return;
    setInstanceForm((current) => {
      if (current.loader === "vanilla" || availableLoaderTypes.includes(current.loader as (typeof availableLoaderTypes)[number])) {
        return current;
      }
      return { ...current, loader: "vanilla", loaderVersion: "" };
    });
  }, [availableLoaderTypes, availabilityFor, loaderAvailabilityLoading, instanceForm.version]);

  const loaderKey = `${instanceForm.version}|${instanceForm.loader}`;

  useEffect(() => {
    setInstanceForm((current) => {
      if (current.loader === "vanilla") {
        return current.loaderVersion ? { ...current, loaderVersion: "" } : current;
      }
      if (loaderVersionsFor !== loaderKey || loaderLoading || loaderVersions.length === 0) return current;
      if (loaderVersions.some((item) => item.version === current.loaderVersion)) return current;
      return { ...current, loaderVersion: loaderVersions[0].version };
    });
  }, [loaderKey, loaderLoading, loaderVersions, loaderVersionsFor]);

  function openCreateDialog() {
    setEditingInstance(null);
    setInstanceForm({ ...EMPTY_INSTANCE_FORM, version: instanceForm.version || latest });
    setCreateDialogOpen(true);
  }

  function editInstance(instance: Instance) {
    setEditingInstance(instance);
    setInstanceForm({
      name: instance.name,
      avatarData: instance.avatar_data,
      version: instance.minecraft_version,
      loader: instance.loader_type,
      loaderVersion: instance.loader_version ?? "",
      memoryMb: instance.memory_mb,
      jvmArgs: instance.jvm_args,
      windowWidth: instance.window_width,
      windowHeight: instance.window_height,
      fullscreen: instance.fullscreen === 1,
    });
    setCreateDialogOpen(true);
  }

  function closeDialog() {
    setCreateDialogOpen(false);
    setEditingInstance(null);
  }

  async function saveInstance(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const name = instanceForm.name.trim();
    const editingBusy = editingInstance ? busyInstanceIds.has(editingInstance.id) : false;
    if (!name || !instanceForm.version || busy || editingBusy || instanceForm.memoryMb > memoryLimitMb
      || (instanceForm.loader !== "vanilla" && !instanceForm.loaderVersion)) {
      return;
    }
    if (editingInstance) markInstanceBusy(editingInstance.id, true);
    else setBusy(true);
    const draft: InstanceDraft = {
      name,
      minecraftVersion: instanceForm.version,
      loaderType: instanceForm.loader,
      loaderVersion: instanceForm.loader === "vanilla" ? null : instanceForm.loaderVersion,
      memoryMb: instanceForm.memoryMb,
      jvmArgs: instanceForm.jvmArgs,
      windowWidth: instanceForm.windowWidth,
      windowHeight: instanceForm.windowHeight,
      fullscreen: instanceForm.fullscreen,
      avatarData: instanceForm.avatarData,
      javaVersion: editingInstance?.java_version,
      javaPath: editingInstance?.java_path,
    };
    try {
      if (editingInstance) {
        const updated = await updateInstance(editingInstance, draft);
        setInstances((current) => current.map((item) => (item.id === updated.id ? updated : item)));
      } else {
        const created = await createInstance(draft);
        setInstances((current) => [created, ...current]);
      }
      closeDialog();
    } catch (error) {
      reportError(error);
    } finally {
      if (editingInstance) markInstanceBusy(editingInstance.id, false);
      else setBusy(false);
    }
  }

  async function removeInstance(instance: Instance): Promise<boolean> {
    if (busyInstanceIds.has(instance.id) || runningInstanceId === instance.id) return false;
    const confirmation = await requestAction({
      title: "Удалить инстанс?",
      message: `Инстанс «${instance.name}» и все его файлы будут удалены без возможности восстановления.`,
      confirmLabel: "Удалить",
      danger: true,
    });
    if (confirmation === null) return false;
    markInstanceBusy(instance.id, true);
    try {
      await deleteInstanceRecord(instance);
      setInstances((current) => current.filter((item) => item.id !== instance.id));
      return true;
    } catch (error) {
      reportError(error);
      return false;
    } finally {
      markInstanceBusy(instance.id, false);
    }
  }

  async function duplicateInstance(instance: Instance) {
    if (busyInstanceIds.has(instance.id) || runningInstanceId === instance.id) return;
    const name = (await requestAction({
      title: "Дублировать инстанс",
      message: "Выберите имя для копии.",
      confirmLabel: "Дублировать",
      inputLabel: "Имя копии",
      initialValue: `${instance.name} copy`,
    }))?.trim();
    if (!name) return;
    markInstanceBusy(instance.id, true);
    try {
      const copy = await duplicateInstanceRecord(instance, name);
      setInstances((current) => [copy, ...current]);
    } catch (error) {
      reportError(error);
    } finally {
      markInstanceBusy(instance.id, false);
    }
  }

  async function openInstanceDirectory(instance: Instance) {
    try {
      await invoke("open_instance_directory", { folderName: instance.folder_name });
    } catch (error) {
      reportError(error);
    }
  }

  function openCatalog(instance: Instance) {
    setCatalogReturnView("instances");
    catalog.prepareForInstance(instance);
    setModsInstanceId(instance.id);
    setView("mods");
  }

  function openInstanceDetails(instance: Instance) {
    setDetailInstanceId(instance.id);
    setModsInstanceId(instance.id);
    catalog.prepareForInstance(instance);
    setDetailTab("content");
    setView("instance-detail");
  }

  function openInstanceCatalog(instance: Instance) {
    setCatalogReturnView("instance-detail");
    catalog.prepareForInstance(instance);
    setModsInstanceId(instance.id);
    setView("mods");
  }

  async function addOfflineAccount(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const username = offlineUsername.trim();
    if (!/^[A-Za-z0-9_]{1,16}$/.test(username)) return;
    try {
      const account = await createOfflineAccount(username);
      setAccounts((current) => [account, ...current]);
      setActiveAccountId(account.id);
      setOfflineUsername("");
    } catch (error) {
      reportError(error);
    }
  }

  async function saveMicrosoftClientId(value: string) {
    const clientId = value.trim();
    setMicrosoftClientId(clientId);
    try {
      await persistMicrosoftClientId(clientId);
    } catch (error) {
      reportError(error);
    }
  }

  async function beginMicrosoftLogin() {
    setMicrosoftBusy(true);
    try {
      await persistMicrosoftClientId(microsoftClientId.trim());
      const code = await requestMicrosoftDeviceCode(microsoftClientId.trim());
      setDeviceCode(code);
      await openExternalUrl(code.verificationUri);
    } catch (error) {
      reportError(error);
    } finally {
      setMicrosoftBusy(false);
    }
  }

  async function completeMicrosoftLogin() {
    if (!deviceCode) return;
    setMicrosoftBusy(true);
    try {
      const account = await completeMicrosoftAccount(deviceCode.flowId);
      setAccounts((current) => [account, ...current.filter((item) => item.id !== account.id)]);
      setActiveAccountId(account.id);
      setDeviceCode(null);
      showNotification("Аккаунт Microsoft добавлен.", "success");
    } catch (error) {
      reportError(error);
    } finally {
      setMicrosoftBusy(false);
    }
  }

  async function selectAccount(account: Account) {
    try {
      await persistActiveAccount(account.id);
      setActiveAccountId(account.id);
    } catch (error) {
      reportError(error);
    }
  }

  async function deleteAccount(account: Account) {
    const confirmation = await requestAction({
      title: "Удалить аккаунт?",
      message: `Учётная запись «${account.username}» будет удалена из лаунчера.`,
      confirmLabel: "Удалить",
      danger: true,
    });
    if (confirmation === null) return;
    try {
      await deleteAccountRecord(account);
      const remaining = accounts.filter((item) => item.id !== account.id);
      setAccounts(remaining);
      if (activeAccountId === account.id) {
        const nextAccount = remaining[0];
        if (nextAccount) {
          await selectAccount(nextAccount);
        } else {
          await clearActiveAccount();
          setActiveAccountId("");
        }
      }
    } catch (error) {
      reportError(error);
    }
  }

  async function installInstance(instance: Instance) {
    if (busyInstanceIds.has(instance.id) || runningInstanceId === instance.id) return;
    const account = accounts.find((item) => item.id === activeAccountId);
    if (!account) {
      reportError(new Error("Сначала добавьте и выберите аккаунт."));
      return;
    }
    markInstanceBusy(instance.id, true);
    setDownloadingInstanceId(instance.id);
    setInstallStage("minecraft");
    setTaskTrayOpen(true);
    setDownloadProgress(null);
    let cancellationRegistered = false;
    try {
      await invoke("begin_instance_install", { folderName: instance.folder_name });
      cancellationRegistered = true;
      const minecraftInstalled = await invoke<boolean>("is_minecraft_installed", {
        folderName: instance.folder_name,
        version: instance.minecraft_version,
      });
      if (!minecraftInstalled) {
        await invoke("install_minecraft_version", {
          folderName: instance.folder_name,
          version: instance.minecraft_version,
        });
      }
      setInstalledInstanceIds((current) => new Set(current).add(instance.id));
      setDownloadProgress(null);

      setInstallStage("java");
      const requiredJavaVersion = await invoke<number>("get_required_java_version", {
        folderName: instance.folder_name,
        version: instance.minecraft_version,
      });
      const javaPath = await ensureJavaRuntime(instance, requiredJavaVersion);
      await invoke("check_instance_install_cancellation", { folderName: instance.folder_name });
      setInstances((current) => current.map((item) => (item.id === instance.id
        ? { ...item, java_version: requiredJavaVersion, java_path: javaPath }
        : item)));

      if (instance.loader_type !== "vanilla") {
        await invoke("check_instance_install_cancellation", { folderName: instance.folder_name });
        const loaderInstalled = await invoke<boolean>("is_loader_installed", { folderName: instance.folder_name });
        if (!loaderInstalled) {
          setInstallStage("loader");
          await invoke("install_loader", {
            folderName: instance.folder_name,
            minecraftVersion: instance.minecraft_version,
            loaderType: instance.loader_type,
            loaderVersion: instance.loader_version,
            javaPath,
          });
        }
      }

      await invoke("check_instance_install_cancellation", { folderName: instance.folder_name });
      setGameLogs([]);
      setInstallStage("launch");
      setRunningInstanceId(instance.id);
      await invoke("launch_game", {
        folderName: instance.folder_name,
        javaPath,
        memoryMb: instance.memory_mb,
        jvmArgs: instance.jvm_args,
        username: account.username,
        playerUuid: account.uuid.replace(/-/g, ""),
        accountType: account.account_type,
        microsoftClientId: microsoftClientId.trim(),
        width: instance.window_width,
        height: instance.window_height,
        fullscreen: instance.fullscreen === 1,
      });
    } catch (error) {
      setRunningInstanceId(null);
      if (errorMessage(error) === "Установка Minecraft отменена.") showNotification("Установка Minecraft отменена.", "success");
      else reportError(error);
    } finally {
      if (cancellationRegistered) {
        await invoke("finish_instance_install", { folderName: instance.folder_name }).catch(reportError);
      }
      markInstanceBusy(instance.id, false);
      setDownloadingInstanceId(null);
      setDownloadProgress(null);
      setInstallStage(null);
      setTaskTrayOpen(false);
    }
  }

  async function repairInstanceFiles(instance: Instance) {
    if (busyInstanceIds.has(instance.id) || runningInstanceId === instance.id) return;
    markInstanceBusy(instance.id, true);
    setDownloadingInstanceId(instance.id);
    setInstallStage("minecraft");
    setTaskTrayOpen(true);
    setDownloadProgress(null);
    let cancellationRegistered = false;
    try {
      await invoke("begin_instance_install", { folderName: instance.folder_name });
      cancellationRegistered = true;
      await invoke("repair_minecraft_version", {
        folderName: instance.folder_name,
        version: instance.minecraft_version,
      });
      setInstalledInstanceIds((current) => new Set(current).add(instance.id));
      showNotification("Файлы Minecraft проверены и восстановлены. Папки модов и сохранений сохранены.", "success");
    } catch (error) {
      if (errorMessage(error) === "Установка Minecraft отменена.") showNotification("Проверка Minecraft отменена.", "success");
      else reportError(error);
    } finally {
      if (cancellationRegistered) {
        await invoke("finish_instance_install", { folderName: instance.folder_name }).catch(reportError);
      }
      markInstanceBusy(instance.id, false);
      setDownloadingInstanceId(null);
      setDownloadProgress(null);
      setInstallStage(null);
      setTaskTrayOpen(false);
    }
  }

  async function stopGame(instance: Instance) {
    const confirmation = await requestAction({
      title: "Остановить игру?",
      message: "Minecraft будет завершён принудительно. Сначала сохраните мир и выйдите из игры обычным способом.",
      confirmLabel: "Остановить",
      danger: true,
    });
    if (confirmation === null) return;
    try {
      await invoke("stop_game", { folderName: instance.folder_name });
    } catch (error) {
      reportError(error);
    }
  }

  async function changeTheme(nextTheme: Theme) {
    setTheme(nextTheme);
    try {
      await saveSetting("theme", nextTheme);
    } catch (error) {
      setDatabaseError(errorMessage(error, "Не удалось сохранить тему."));
      setDatabaseStatus("error");
    }
  }

  async function changePaletteColor(color: PaletteColor, value: string) {
    if (!/^#[0-9a-fA-F]{6}$/.test(value)) return;
    const nextPalettes: ThemePalettes = {
      ...uiPalettes,
      custom: { ...uiPalettes[theme], [color]: value },
    };
    setUiPalettes(nextPalettes);
    setTheme("custom");
    try {
      await Promise.all([
        saveSetting("ui_palette", JSON.stringify(nextPalettes)),
        saveSetting("theme", "custom"),
      ]);
    } catch (error) {
      setDatabaseError(errorMessage(error, "Не удалось сохранить палитру."));
      setDatabaseStatus("error");
    }
  }

  async function resetPalette() {
    const nextPalettes: ThemePalettes = {
      ...uiPalettes,
      [theme]: DEFAULT_UI_PALETTES[theme],
    };
    setUiPalettes(nextPalettes);
    try {
      await saveSetting("ui_palette", JSON.stringify(nextPalettes));
      showNotification("Палитра интерфейса сброшена.", "success");
    } catch (error) {
      setDatabaseError(errorMessage(error, "Не удалось сохранить палитру."));
      setDatabaseStatus("error");
    }
  }

  async function openLauncherDirectory() {
    try {
      await invoke("open_launcher_directory");
    } catch (error) {
      reportError(error);
    }
  }

  const currentViewTitle = view === "instance-detail"
    ? detailInstance?.name ?? "Инстанс"
    : {
        instances: "Инстансы",
        mods: "Моды",
        modpacks: "Сборки",
        repository: "Собственные сборки",
        accounts: "Аккаунты",
        settings: "Настройки",
        servers: "Серверы",
        about: "Разработчики",
      }[view];
  const activeAccount = accounts.find((account) => account.id === activeAccountId);
  const installingInstance = instances.find((instance) => instance.id === downloadingInstanceId);
  const installStageLabel = {
    minecraft: "Скачивание Minecraft",
    java: "Подготовка Java",
    loader: "Установка загрузчика",
    launch: "Запуск игры",
  }[installStage ?? "minecraft"];
  const activeTaskCount = Number(Boolean(downloadingInstanceId)) + Number(Boolean(serverInstallTask)) + Number(Boolean(modpackInstallTask));

  return (
    <div
      className={`launcher-shell ${themeClassName(theme, uiPalettes[theme])}`}
      style={{
        "--accent": uiPalettes[theme].accent,
        "--button": uiPalettes[theme].button,
        "--canvas": uiPalettes[theme].canvas,
        "--surface": uiPalettes[theme].surface,
        "--ink": uiPalettes[theme].ink,
        colorScheme: theme === "light" || (theme === "custom" && isLightPalette(uiPalettes.custom)) ? "light" : "dark",
      } as React.CSSProperties}
    >
      <Sidebar
        view={view}
        databaseStatus={databaseStatus}
        onSelectView={setView}
        onOpenAbout={() => {
          setAboutReturnView(view);
          setView("about");
        }}
      />

      <main className="workspace">
        <Topbar
          currentViewTitle={currentViewTitle}
          downloadingInstanceId={downloadingInstanceId}
          installingInstanceName={installingInstance?.name ?? null}
          installStageLabel={installStageLabel}
          downloadProgress={downloadProgress}
          serverInstallTask={serverInstallTask}
          onCancelServerInstall={() => void cancelServerInstallation()}
          onCancelGameInstall={() => void cancelGameInstallation()}
          modpackInstallTask={modpackInstallTask}
          onCancelModpackInstall={() => void cancelModpackInstallation()}
          activeTaskCount={activeTaskCount}
          taskTrayOpen={taskTrayOpen}
          onToggleTaskTray={() => setTaskTrayOpen((open) => !open)}
          activeAccountName={activeAccount?.username ?? null}
        />

        <div className="page-content" key={view}>
          {view === "instances" && (
            <InstancesView
              instances={instances}
              accountCount={accounts.length}
              databaseStatus={databaseStatus}
              databaseError={databaseError}
              hasActiveAccount={Boolean(activeAccount)}
              busy={busy}
              busyInstanceIds={activeBusyInstanceIds}
              downloadingInstanceId={downloadingInstanceId}
              runningInstanceId={runningInstanceId}
              installedInstanceIds={installedInstanceIds}
              gameLogs={gameLogs}
              onCreate={openCreateDialog}
              onOpenDirectory={(instance) => void openInstanceDirectory(instance)}
              onOpenInstance={openInstanceDetails}
              onOpenCatalog={openCatalog}
              onEdit={editInstance}
              onDuplicate={(instance) => void duplicateInstance(instance)}
              onDelete={(instance) => void removeInstance(instance)}
              onLaunch={(instance) => void installInstance(instance)}
              onStop={(instance) => void stopGame(instance)}
              onUpdateRepository={(instance) => void updateRepositoryInstance(instance)}
            />
          )}

          {view === "instance-detail" && detailInstance && (
            <InstanceDetailView
              instance={detailInstance}
              tab={detailTab}
              installedMods={catalog.installedMods}
              gameLogs={gameLogs}
              isRunning={runningInstanceId === detailInstance.id}
              busy={activeBusyInstanceIds.has(detailInstance.id)}
              hasActiveAccount={Boolean(activeAccount)}
              onChangeTab={setDetailTab}
              onBack={() => setView("instances")}
              onOpenDirectory={() => void openInstanceDirectory(detailInstance)}
              onOpenCatalog={() => openInstanceCatalog(detailInstance)}
              onEdit={() => editInstance(detailInstance)}
              onDelete={() => void removeInstance(detailInstance).then((deleted) => {
                if (deleted) setView("instances");
              })}
              onLaunch={() => void installInstance(detailInstance)}
              onStop={() => void stopGame(detailInstance)}
              onToggleMod={(mod) => void catalog.toggle(mod)}
              onUpdateMod={(mod, versionId) => void catalog.update(mod, versionId)}
              onRemoveMod={(mod) => void catalog.remove(mod)}
            />
          )}

          {view === "accounts" && (
            <AccountsView
              accounts={accounts}
              activeAccountId={activeAccountId}
              microsoftClientId={microsoftClientId}
              offlineUsername={offlineUsername}
              deviceCode={deviceCode}
              microsoftBusy={microsoftBusy}
              onMicrosoftClientIdChange={setMicrosoftClientId}
              onSaveMicrosoftClientId={(value) => void saveMicrosoftClientId(value)}
              onBeginMicrosoftLogin={() => void beginMicrosoftLogin()}
              onCompleteMicrosoftLogin={() => void completeMicrosoftLogin()}
              onOpenVerification={() => {
                if (deviceCode) void openExternalUrl(deviceCode.verificationUri).catch(reportError);
              }}
              onOfflineUsernameChange={setOfflineUsername}
              onAddOfflineAccount={(event) => void addOfflineAccount(event)}
              onSelectAccount={(account) => void selectAccount(account)}
              onDeleteAccount={(account) => void deleteAccount(account)}
            />
          )}

          {view === "mods" && modsInstance && (
            <ModsView
              instance={modsInstance}
              versions={versions}
              catalog={catalog}
              backLabel={catalogReturnView === "instance-detail" ? "Назад к инстансу" : "Назад к инстансам"}
              onBack={() => setView(catalogReturnView)}
            />
          )}

          {view === "modpacks" && <ModpacksView versions={versions} memoryLimitMb={memoryLimitMb} theme={theme} uiPalettes={uiPalettes} reportError={reportError} onInstall={installModpackInstance} />}

          {view === "repository" && <RepositoryView reportError={reportError} onInstall={installRepositoryPackInstance} />}

          {view === "settings" && (
            <SettingsView
              theme={theme}
              uiPalettes={uiPalettes}
              databaseStatus={databaseStatus}
              onChangeTheme={(nextTheme) => void changeTheme(nextTheme)}
              onChangePaletteColor={(color, value) => void changePaletteColor(color, value)}
              onResetPalette={() => void resetPalette()}
              onOpenLauncherDirectory={() => void openLauncherDirectory()}
            />
          )}

          {view === "servers" && (
            <ServersView
              versions={versions}
              latest={latest}
              memoryLimitMb={memoryLimitMb}
              theme={theme}
              uiPalettes={uiPalettes}
              reportError={reportError}
              showNotification={showNotification}
              serverInstallTask={serverInstallTask}
              onInstallServer={installServerInBackground}
            />
          )}

          {view === "about" && <AboutView onBack={() => setView(aboutReturnView)} />}
        </div>
      </main>

      {createDialogOpen && (
        <InstanceDialog
          editingInstance={editingInstance}
          form={instanceForm}
          onChange={updateForm}
          versions={versions}
          availableLoaderTypes={availableLoaderTypes}
          loaderVersions={loaderVersions}
          loaderAvailabilityLoading={loaderAvailabilityLoading}
          loaderLoading={loaderLoading}
          versionError={versionError}
          loaderError={loaderError}
          memoryLimitMb={memoryLimitMb}
          busy={busy || Boolean(editingInstance && busyInstanceIds.has(editingInstance.id))}
          theme={theme}
          uiPalettes={uiPalettes}
          onSubmit={(event) => void saveInstance(event)}
          onClose={closeDialog}
          onError={showNotification}
          onRepair={() => {
            if (editingInstance) void repairInstanceFiles(editingInstance);
          }}
        />
      )}

      <ActionDialog request={actionDialog} onClose={() => setActionDialog(null)} />
      {notification && <Toast notification={notification} onDismiss={() => dismissNotification(notification.id)} />}
    </div>
  );
}

export default App;
