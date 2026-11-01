import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Activity, Check, CheckCircle2, ChevronLeft, ChevronRight, CircleAlert, Clock3, Cpu, Download, Filter, FolderOpen, Gauge, LoaderCircle, Pencil, Play, Plus, RotateCw, Save, Search, Send, Server, Square, Terminal, Trash2, Users, X } from "lucide-react";
import { openExternalUrl } from "../lib/external";
import { ActionDialog } from "../components/ActionDialog";
import type { ActionDialogOptions, ActionDialogRequest } from "../components/ActionDialog";
import { isLightPalette, themeClassName } from "../lib/palettes";
import { MOD_CATALOG_CATEGORIES } from "../types";
import type { InstalledMod, LoaderVersion, ModrinthProject, ServerInstallOptions, ServerInstallRequest, ServerInstalledContent, ServerLog, ServerOptionalDependency, ServerQuickCommand, ServerStatus, ServerStatusEvent, ServerSummary, Theme, ThemePalettes } from "../types";

type ServerContentType = "mod" | "plugin" | "datapack";

const SERVER_CONTENT_LABELS: Record<ServerContentType, string> = {
  mod: "Моды",
  plugin: "Плагины",
  datapack: "Датапаки",
};

function isServerVersionSupported(version: string) {
  const parts = version.split(".").map(Number);
  const current = [parts[0], parts[1], parts[2] ?? 0];
  if (current.some((part) => !Number.isInteger(part))) return false;
  const minimum = [1, 5, 2];
  for (let index = 0; index < minimum.length; index += 1) {
    if (current[index] !== minimum[index]) return current[index] > minimum[index];
  }
  return true;
}

function tpsCommandForCore(core: string): string | null {
  if (core === "paper" || core === "purpur") return "tps";
  if (core === "forge") return "forge tps";
  if (core === "neoforge") return "neoforge tps";
  return null;
}

const CORE_LABELS: Record<string, string> = {
  vanilla: "Vanilla",
  paper: "Paper",
  purpur: "Purpur",
  fabric: "Fabric",
  forge: "Forge",
  neoforge: "NeoForge",
};

const STATUS_LABELS: Record<ServerStatus, string> = {
  stopped: "Остановлен",
  starting: "Запускается",
  running: "Работает",
  stopping: "Останавливается",
  crashed: "Аварийно завершён",
};

interface ServersViewProps {
  versions: string[];
  latest: string;
  memoryLimitMb: number;
  theme: Theme;
  uiPalettes: ThemePalettes;
  reportError: (error: unknown) => void;
  showNotification: (message: string, kind?: "error" | "success") => void;
  serverInstallTask: ServerInstallRequest | null;
  onInstallServer: (request: ServerInstallRequest) => Promise<ServerSummary | null>;
}

export function ServersView({ versions, latest, memoryLimitMb, theme, uiPalettes, reportError, showNotification, serverInstallTask, onInstallServer }: ServersViewProps) {
  const [servers, setServers] = useState<ServerSummary[]>([]);
  const [selectedName, setSelectedName] = useState("");
  const [logs, setLogs] = useState<ServerLog[]>([]);
  const [players, setPlayers] = useState<string[]>([]);
  const [tps, setTps] = useState<{ values: number[]; label: string } | null>(null);
  const [playerLimit, setPlayerLimit] = useState(20);
  const [command, setCommand] = useState("");
  const [showCreate, setShowCreate] = useState(false);
  const [busy, setBusy] = useState(false);
  const [name, setName] = useState("");
  const [version, setVersion] = useState(latest);
  const [core, setCore] = useState("");
  const [loaderVersion, setLoaderVersion] = useState("");
  const [availableCores, setAvailableCores] = useState<string[]>([]);
  const [loaderVersionsByCore, setLoaderVersionsByCore] = useState<Record<string, LoaderVersion[]>>({});
  const [serverOptionsLoading, setServerOptionsLoading] = useState(false);
  const [memoryMb, setMemoryMb] = useState(Math.min(4096, memoryLimitMb));
  const [memoryDraft, setMemoryDraft] = useState(512);
  const [savingMemory, setSavingMemory] = useState(false);
  const [quickCommandsDraft, setQuickCommandsDraft] = useState<ServerQuickCommand[]>([]);
  const [processorCountDraft, setProcessorCountDraft] = useState<number | null>(null);
  const [editingControls, setEditingControls] = useState(false);
  const [savingControls, setSavingControls] = useState(false);
  const [contentType, setContentType] = useState<ServerContentType>("plugin");
  const [showContentCatalog, setShowContentCatalog] = useState(false);
  const [contentQuery, setContentQuery] = useState("");
  const [contentCategory, setContentCategory] = useState("");
  const [contentSort, setContentSort] = useState<"downloads" | "relevance">("downloads");
  const [contentResults, setContentResults] = useState<ModrinthProject[]>([]);
  const [contentTotal, setContentTotal] = useState(0);
  const [contentPage, setContentPage] = useState(0);
  const [contentSearched, setContentSearched] = useState(false);
  const [contentBusy, setContentBusy] = useState(false);
  const [installedContent, setInstalledContent] = useState<ServerInstalledContent[]>([]);
  const [serverWorlds, setServerWorlds] = useState<string[]>([]);
  const [contentWorld, setContentWorld] = useState("");
  const [pendingContentInstall, setPendingContentInstall] = useState<ModrinthProject | null>(null);
  const [optionalDependencies, setOptionalDependencies] = useState<ServerOptionalDependency[]>([]);
  const [selectedOptionalVersions, setSelectedOptionalVersions] = useState<string[]>([]);
  const [acceptedEula, setAcceptedEula] = useState(false);
  const [actionDialog, setActionDialog] = useState<ActionDialogRequest | null>(null);
  const consoleRef = useRef<HTMLDivElement>(null);
  const followLogsRef = useRef(true);
  const selected = servers.find((server) => server.name === selectedName) ?? null;
  const isRunning = selected?.status === "running" || selected?.status === "starting" || selected?.status === "stopping";
  const loaderVersions = loaderVersionsByCore[core] ?? [];
  const serverVersions = versions.filter(isServerVersionSupported);

  function requestAction(options: ActionDialogOptions): Promise<string | null> {
    return new Promise((resolve) => {
      setActionDialog({ ...options, onConfirm: resolve, onCancel: () => resolve(null) });
    });
  }
  const logicalProcessorCount = typeof navigator === "undefined" ? 1 : Math.max(1, navigator.hardwareConcurrency || 1);

  async function refreshServers() {
    try {
      const current = await invoke<ServerSummary[]>("list_servers");
      setServers(current);
      setSelectedName((previous) => current.some((server) => server.name === previous) ? previous : current[0]?.name ?? "");
    } catch (error) {
      reportError(error);
    }
  }

  function trackPlayers(line: string, core: string) {
    const joined = line.match(/([A-Za-z0-9_]{1,16}) joined the game/);
    const left = line.match(/([A-Za-z0-9_]{1,16}) left the game/);
    const list = line.match(/There are (\d+) of a max of (\d+) players online(?::\s*(.*))?/i);
    const paperTpsLine = line.match(/TPS from last 1m, 5m, 15m:\s*([\d.]+),\s*([\d.]+),\s*([\d.]+)/i);
    const forgeTpsLine = line.match(/Mean TPS:\s*([\d.]+)/i);
    const coreTpsLine = line.match(/\bTPS\b[^\d]*(\d+(?:\.\d+)?)/i);
    if (joined) setPlayers((current) => current.includes(joined[1]) ? current : [...current, joined[1]]);
    if (left) setPlayers((current) => current.filter((player) => player !== left[1]));
    if (list) {
      setPlayerLimit(Number(list[2]));
      setPlayers(list[3]?.trim() ? list[3].split(/,\s*/) : []);
    }
    if (paperTpsLine) {
      setTps({ values: paperTpsLine.slice(1).map(Number), label: "за 1 / 5 / 15 мин" });
    } else if (core === "forge" && forgeTpsLine) {
      setTps({ values: [Number(forgeTpsLine[1])], label: "средний TPS" });
    } else if (core === "neoforge" && coreTpsLine) {
      setTps({ values: [Number(coreTpsLine[1])], label: "текущее измерение" });
    }
  }

  async function requestTps() {
    if (!selected || selected.status !== "running") return;
    const command = tpsCommandForCore(selected.core);
    if (!command) return;
    try {
      await invoke("send_server_command", { name: selected.name, command });
    } catch (error) {
      reportError(error);
    }
  }

  useEffect(() => {
    let active = true;
    let stopLogs: (() => void) | undefined;
    let stopStatus: (() => void) | undefined;
    let stopExit: (() => void) | undefined;
    void refreshServers();
    const timer = window.setInterval(() => { void refreshServers(); }, 10_000);
    Promise.all([
      listen<ServerLog>("server-log", ({ payload }) => {
        if (active && payload.serverName === selectedName) {
          setLogs((current) => [...current.slice(-1999), payload]);
          trackPlayers(payload.line, selected?.core ?? "");
        }
      }),
      listen<ServerStatusEvent>("server-status", ({ payload }) => {
        if (active) setServers((current) => current.map((server) => server.name === payload.name ? { ...server, status: payload.status } : server));
      }),
      listen<{ serverName: string }>("server-exit", () => { if (active) void refreshServers(); }),
    ]).then(([logsUnlisten, statusUnlisten, exitUnlisten]) => {
      if (active) {
        stopLogs = logsUnlisten;
        stopStatus = statusUnlisten;
        stopExit = exitUnlisten;
      } else {
        logsUnlisten();
        statusUnlisten();
        exitUnlisten();
      }
    }).catch(reportError);
    return () => {
      active = false;
      window.clearInterval(timer);
      stopLogs?.();
      stopStatus?.();
      stopExit?.();
    };
  }, [reportError, selectedName, selected?.core]);

  useEffect(() => {
    setLogs([]);
    setPlayers([]);
    setPlayerLimit(20);
    setTps(null);
    setMemoryDraft(selected?.memoryMb ?? Math.min(4096, memoryLimitMb));
    followLogsRef.current = true;
  }, [selectedName]);

  useEffect(() => {
    if (!selected) return;
    let active = true;
    invoke<ServerLog[]>("read_server_log", { name: selected.name })
      .then((entries) => {
        if (!active) return;
        setLogs(entries);
        for (const entry of entries) trackPlayers(entry.line, selected.core);
      })
      .catch(reportError);
    return () => { active = false; };
  }, [selected?.name, selected?.core, reportError]);

  useEffect(() => {
    setQuickCommandsDraft(selected?.quickCommands ?? []);
    setProcessorCountDraft(selected?.activeProcessorCount ?? null);
    setEditingControls(false);
  }, [selectedName]);

  useEffect(() => {
    setContentType(selected?.core === "paper" || selected?.core === "purpur"
      ? "plugin"
      : selected?.core === "vanilla"
        ? "datapack"
        : "mod");
    setShowContentCatalog(false);
    setContentQuery("");
    setContentCategory("");
    setContentSort("downloads");
    setContentResults([]);
    setContentPage(0);
    setContentSearched(false);
    setContentWorld("");
    setInstalledContent([]);
    if (!selected) {
      setServerWorlds([]);
      return;
    }
    void invoke<string[]>("list_server_worlds", { serverName: selected.name })
      .then(setServerWorlds)
      .catch(reportError);
  }, [selectedName]);

  useEffect(() => {
    if (!showContentCatalog && !pendingContentInstall) return;
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (pendingContentInstall) cancelOptionalDependencies();
      else setShowContentCatalog(false);
    };
    document.addEventListener("keydown", closeOnEscape);
    return () => document.removeEventListener("keydown", closeOnEscape);
  }, [showContentCatalog, pendingContentInstall]);

  useEffect(() => {
    if (!selected || (contentType === "datapack" && !contentWorld)) {
      setInstalledContent([]);
      return;
    }
    let active = true;
    invoke<ServerInstalledContent[]>("list_server_content", {
      serverName: selected.name,
      projectType: contentType,
      worldName: contentType === "datapack" ? contentWorld : null,
    })
      .then((items) => { if (active) setInstalledContent(items); })
      .catch((error: unknown) => { if (active) reportError(error); });
    return () => { active = false; };
  }, [selectedName, contentType, contentWorld, selected?.name, reportError]);

  useEffect(() => {
    if (!selected || selected.status !== "running" || !tpsCommandForCore(selected.core)) setTps(null);
  }, [selected?.name, selected?.status, selected?.core]);

  useEffect(() => {
    if (!followLogsRef.current) return;
    const consoleElement = consoleRef.current;
    if (consoleElement) consoleElement.scrollTop = consoleElement.scrollHeight;
  }, [logs]);

  useEffect(() => {
    if (latest && !version) setVersion(latest);
  }, [latest, version]);

  useEffect(() => {
    let active = true;
    if (!version) {
      setServerOptionsLoading(false);
      setAvailableCores([]);
      setLoaderVersionsByCore({});
      setLoaderVersion("");
      return () => { active = false; };
    }
    setServerOptionsLoading(true);
    invoke<ServerInstallOptions>("get_server_install_options", { minecraftVersion: version })
      .then((options) => {
        if (!active) return;
        setAvailableCores(options.cores);
        setLoaderVersionsByCore(options.loaderVersions);
        setCore((current) => options.cores.includes(current) ? current : options.cores[0] ?? "");
        if (options.warnings.length) reportError(options.warnings.join("\n"));
      })
      .catch((error: unknown) => {
        if (active) {
          setAvailableCores([]);
          setLoaderVersionsByCore({});
          setLoaderVersion("");
          setCore("");
          reportError(error);
        }
      })
      .finally(() => { if (active) setServerOptionsLoading(false); });
    return () => { active = false; };
  }, [version, reportError]);

  useEffect(() => {
    const versions = loaderVersionsByCore[core] ?? [];
    setLoaderVersion((current) => versions.some((item) => item.version === current) ? current : versions[0]?.version ?? "");
  }, [core, loaderVersionsByCore]);

  async function installServer(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy || serverInstallTask || serverOptionsLoading || !availableCores.includes(core) || !name.trim() || !isServerVersionSupported(version) || memoryMb < 512 || memoryMb > memoryLimitMb) return;
    if (["fabric", "forge", "neoforge"].includes(core) && !loaderVersions.some((item) => item.version === loaderVersion)) return;
    setBusy(true);
    try {
      const created = await onInstallServer({
        name: name.trim(),
        minecraftVersion: version,
        core,
        loaderVersion: loaderVersion.trim() || null,
        memoryMb,
        eulaAccepted: acceptedEula,
      });
      if (!created) return;
      setServers((current) => [...current.filter((server) => server.name !== created.name), created]);
      setSelectedName(created.name);
      setName("");
      setLoaderVersion("");
      setAcceptedEula(false);
      setShowCreate(false);
    } finally {
      setBusy(false);
    }
  }

  async function start() {
    if (!selected || busy) return;
    setBusy(true);
    setLogs([]);
    followLogsRef.current = true;
    setServers((current) => current.map((server) => server.name === selected.name ? { ...server, status: "starting" } : server));
    try {
      const updated = await invoke<ServerSummary>("start_server", { name: selected.name });
      setServers((current) => current.map((server) => server.name === updated.name ? updated : server));
    } catch (error) {
      setServers((current) => current.map((server) => server.name === selected.name ? { ...server, status: "stopped" } : server));
      reportError(error);
    } finally {
      setBusy(false);
    }
  }

  async function openServerFolder() {
    if (!selected) return;
    try {
      await invoke("open_server_directory", { name: selected.name });
    } catch (error) {
      reportError(error);
    }
  }

  async function deleteSelectedServer() {
    if (!selected || busy || isRunning) return;
    const confirmation = await requestAction({
      title: "Удалить сервер?",
      message: `Сервер «${selected.name}» и все его файлы будут удалены без возможности восстановления.`,
      confirmLabel: "Удалить",
      danger: true,
    });
    if (confirmation === null) return;
    setBusy(true);
    try {
      await invoke("delete_server", { name: selected.name });
      setServers((current) => current.filter((server) => server.name !== selected.name));
      setSelectedName("");
      setLogs([]);
      showNotification(`Сервер «${selected.name}» удалён.`, "success");
    } catch (error) {
      reportError(error);
    } finally {
      setBusy(false);
    }
  }

  async function saveServerMemory() {
    if (!selected || isRunning || savingMemory || memoryDraft < 512 || memoryDraft > memoryLimitMb) return;
    setSavingMemory(true);
    try {
      const updated = await invoke<ServerSummary>("update_server_memory", {
        name: selected.name,
        memoryMb: memoryDraft,
      });
      setServers((current) => current.map((server) => server.name === updated.name ? updated : server));
      showNotification("Выделенная память сервера обновлена.", "success");
    } catch (error) {
      reportError(error);
    } finally {
      setSavingMemory(false);
    }
  }

  function updateQuickCommand(id: string, patch: Partial<ServerQuickCommand>) {
    setQuickCommandsDraft((current) => current.map((item) => item.id === id ? { ...item, ...patch } : item));
  }

  function addQuickCommand() {
    if (quickCommandsDraft.length >= 16) return;
    const id = globalThis.crypto?.randomUUID?.() ?? `quick-${Date.now()}`;
    setQuickCommandsDraft((current) => [...current, { id, label: "Новая команда", command: "list" }]);
  }

  async function saveServerControls() {
    if (!selected || isRunning || savingControls) return;
    setSavingControls(true);
    try {
      const updated = await invoke<ServerSummary>("update_server_controls", {
        name: selected.name,
        quickCommands: quickCommandsDraft,
        activeProcessorCount: processorCountDraft,
      });
      setServers((current) => current.map((server) => server.name === updated.name ? updated : server));
      setQuickCommandsDraft(updated.quickCommands);
      setProcessorCountDraft(updated.activeProcessorCount);
      setEditingControls(false);
      showNotification("Быстрые команды и настройки CPU сохранены.", "success");
    } catch (error) {
      reportError(error);
    } finally {
      setSavingControls(false);
    }
  }

  function cancelServerControls() {
    setQuickCommandsDraft(selected?.quickCommands ?? []);
    setProcessorCountDraft(selected?.activeProcessorCount ?? null);
    setEditingControls(false);
  }

  async function stop(force = false) {
    if (!selected || busy) return;
    setBusy(true);
    try {
      await invoke("stop_server", { name: selected.name, force });
      await refreshServers();
    } catch (error) {
      reportError(error);
    } finally {
      setBusy(false);
    }
  }

  async function restart() {
    if (!selected || busy) return;
    setBusy(true);
    setLogs([]);
    followLogsRef.current = true;
    try {
      const updated = await invoke<ServerSummary>("restart_server", { name: selected.name });
      setServers((current) => current.map((server) => server.name === updated.name ? updated : server));
    } catch (error) {
      reportError(error);
    } finally {
      setBusy(false);
    }
  }

  async function send(commandText = command) {
    if (!selected || !commandText.trim() || selected.status !== "running") return;
    try {
      await invoke("send_server_command", { name: selected.name, command: commandText.trim() });
      setCommand("");
    } catch (error) {
      reportError(error);
    }
  }

  async function searchContent(nextPage = 0) {
    if (!selected || (contentType === "datapack" && !contentWorld)) return;
    setContentBusy(true);
    try {
      const results = await invoke<{ hits: ModrinthProject[]; totalHits: number }>("search_server_content", {
        serverName: selected.name,
        projectType: contentType,
        query: contentQuery,
        category: contentType === "mod" ? contentCategory : "",
        sort: contentSort,
        offset: nextPage * 20,
      });
      setContentResults(results.hits);
      setContentTotal(results.totalHits);
      setContentPage(nextPage);
      setContentSearched(true);
    } catch (error) {
      reportError(error);
    } finally {
      setContentBusy(false);
    }
  }

  async function refreshServerContent() {
    if (!selected || (contentType === "datapack" && !contentWorld)) return;
    const items = await invoke<ServerInstalledContent[]>("list_server_content", {
      serverName: selected.name,
      projectType: contentType,
      worldName: contentType === "datapack" ? contentWorld : null,
    });
    setInstalledContent(items);
  }

  async function beginInstallServerProject(project: ModrinthProject) {
    if (!selected || (contentType === "datapack" && !contentWorld)) return;
    setContentBusy(true);
    try {
      const dependencies = contentType === "datapack"
        ? []
        : await invoke<ServerOptionalDependency[]>("list_server_optional_dependencies", {
            serverName: selected.name,
            projectId: project.projectId,
            projectType: contentType,
          });
      if (dependencies.length) {
        setPendingContentInstall(project);
        setOptionalDependencies(dependencies);
        setSelectedOptionalVersions([]);
        return;
      }
      await installServerProject(project, []);
    } catch (error) {
      reportError(error);
    } finally {
      setContentBusy(false);
    }
  }

  async function installServerProject(project: ModrinthProject, optionalVersionIds: string[]) {
    if (!selected || (contentType === "datapack" && !contentWorld)) return;
    setContentBusy(true);
    try {
      await invoke<InstalledMod>("install_server_content", {
        serverName: selected.name,
        projectId: project.projectId,
        projectType: contentType,
        worldName: contentType === "datapack" ? contentWorld : null,
        optionalVersionIds,
      });
      await refreshServerContent();
      showNotification(`${project.title} установлен${isRunning ? ". Перезапустите сервер для применения." : "."}`, "success");
      setPendingContentInstall(null);
      setOptionalDependencies([]);
    } catch (error) {
      reportError(error);
    } finally {
      setContentBusy(false);
    }
  }

  async function confirmOptionalDependencies() {
    if (!pendingContentInstall) return;
    await installServerProject(pendingContentInstall, selectedOptionalVersions);
  }

  function cancelOptionalDependencies() {
    setPendingContentInstall(null);
    setOptionalDependencies([]);
    setSelectedOptionalVersions([]);
  }

  async function updateServerProject(item: InstalledMod) {
    if (!selected || !item.projectId) return;
    setContentBusy(true);
    try {
      await invoke<InstalledMod>("update_server_content", {
        serverName: selected.name,
        projectType: contentType,
        worldName: contentType === "datapack" ? contentWorld : null,
        fileName: item.fileName,
      });
      await refreshServerContent();
      showNotification(`Контент обновлён${isRunning ? ". Перезапустите сервер для применения." : "."}`, "success");
    } catch (error) {
      reportError(error);
    } finally {
      setContentBusy(false);
    }
  }

  async function toggleServerProject(item: InstalledMod) {
    if (!selected) return;
    setContentBusy(true);
    try {
      const updated = await invoke<InstalledMod>("toggle_server_content", {
        serverName: selected.name,
        projectType: contentType,
        worldName: contentType === "datapack" ? contentWorld : null,
        fileName: item.fileName,
        enabled: !item.enabled,
      });
      setInstalledContent((current) => current.map((installed) => installed.fileName === item.fileName ? { ...updated, updateAvailable: installed.updateAvailable } : installed));
      showNotification(isRunning ? "Состояние файла изменено. Перезапустите сервер для применения." : "Состояние файла изменено.", "success");
    } catch (error) {
      reportError(error);
    } finally {
      setContentBusy(false);
    }
  }

  async function removeServerProject(item: InstalledMod) {
    if (!selected) return;
    const confirmation = await requestAction({
      title: "Удалить файл?",
      message: `Файл «${item.fileName}» будет удалён с сервера.`,
      confirmLabel: "Удалить",
      danger: true,
    });
    if (confirmation === null) return;
    setContentBusy(true);
    try {
      await invoke("remove_server_content", {
        serverName: selected.name,
        projectType: contentType,
        worldName: contentType === "datapack" ? contentWorld : null,
        fileName: item.fileName,
      });
      setInstalledContent((current) => current.filter((installed) => installed.fileName !== item.fileName));
      showNotification(`Файл удалён${isRunning ? ". Перезапустите сервер для применения." : "."}`, "success");
    } catch (error) {
      reportError(error);
    } finally {
      setContentBusy(false);
    }
  }

  function formatUptime(seconds: number) {
    const hours = Math.floor(seconds / 3600);
    const minutes = Math.floor((seconds % 3600) / 60);
    const remaining = seconds % 60;
    return `${hours}ч ${minutes}м ${remaining}с`;
  }

  return (
    <>
    <section className="servers-view">
      <header className="page-heading">
        <div>
          <div className="eyebrow"><Server size={13} /> УПРАВЛЕНИЕ СЕРВЕРАМИ</div>
          <h1>Серверы</h1>
          <p className="page-subtitle">Установка ядер, управление процессом и серверная консоль.</p>
        </div>
        <button className="button-primary" onClick={() => setShowCreate((current) => !current)}>
          <Server size={16} /> {showCreate ? "Закрыть форму" : "Новый сервер"}
        </button>
      </header>

      {showCreate && (
        <form className="server-create-form" onSubmit={(event) => void installServer(event)}>
          <div className="server-form-heading"><strong>Новая установка</strong><span>Файлы будут храниться в каталоге MyLauncher/servers.</span></div>
          {serverInstallTask && <div className="server-install-status" role="status" aria-live="polite"><div><LoaderCircle className="spin" size={17} /><span><strong>Установка сервера</strong><small>{CORE_LABELS[serverInstallTask.core] ?? serverInstallTask.core} · Minecraft {serverInstallTask.minecraftVersion}. Подготовка Java и серверных файлов.</small></span></div><span className="server-install-progress" role="progressbar" aria-label="Установка сервера"><i /></span></div>}
          <label className="server-field">Название папки<input value={name} onChange={(event) => setName(event.target.value)} maxLength={64} required placeholder="Например, survival" /></label>
          <label className="server-field">Ядро<select value={core} onChange={(event) => setCore(event.target.value)} disabled={serverOptionsLoading || !availableCores.length}><option value="" disabled>{serverOptionsLoading ? "Проверяем доступность..." : availableCores.length ? "Выберите ядро" : "Нет доступных ядер"}</option>{availableCores.map((value) => <option key={value} value={value}>{CORE_LABELS[value] ?? value}</option>)}</select></label>
          <label className="server-field">Minecraft<select value={version} onChange={(event) => setVersion(event.target.value)} required><option value="">Выберите версию</option>{serverVersions.map((item) => <option key={item} value={item}>{item}</option>)}</select></label>
          {(["fabric", "forge", "neoforge"].includes(core)) && <label className="server-field">Доступная версия загрузчика<select value={loaderVersion} onChange={(event) => setLoaderVersion(event.target.value)} disabled={serverOptionsLoading || loaderVersions.length === 0} required><option value="">{serverOptionsLoading ? "Загрузка версий..." : loaderVersions.length ? "Выберите версию" : "Версии не найдены"}</option>{loaderVersions.map((item) => <option key={item.version} value={item.version}>{item.version} · {item.channel}</option>)}</select></label>}
          <label className="server-field">Память, МБ<input type="number" min={512} max={memoryLimitMb} step={256} value={memoryMb} onChange={(event) => setMemoryMb(Number(event.target.value))} required /></label>
          <label className="server-eula"><input type="checkbox" checked={acceptedEula} onChange={(event) => setAcceptedEula(event.target.checked)} /><span>Я принимаю <a href="https://www.minecraft.net/eula" onClick={(event) => { event.preventDefault(); event.stopPropagation(); void openExternalUrl("https://www.minecraft.net/eula").catch(reportError); }}>Minecraft EULA</a></span></label>
          <div className="server-form-actions"><span>Доступный лимит: {memoryLimitMb.toLocaleString("ru-RU")} МБ</span><button className="button-primary" type="submit" disabled={busy || Boolean(serverInstallTask) || serverOptionsLoading || !availableCores.includes(core) || !versions.length || !acceptedEula || (["fabric", "forge", "neoforge"].includes(core) && !loaderVersion)}>{busy || serverInstallTask ? <><LoaderCircle size={15} className="spin" /> Установка</> : "Установить сервер"}</button></div>
        </form>
      )}

      <div className="servers-layout">
        <aside className="server-list" aria-label="Список серверов">
          <div className="server-list-heading"><strong>Установленные</strong><span>{servers.length}</span></div>
          {servers.map((server) => (
            <button key={server.name} className={`server-list-item ${selectedName === server.name ? "active" : ""}`} onClick={() => setSelectedName(server.name)}>
              <span className={`server-state-dot ${server.status}`} />
              <span className="server-list-copy"><strong>{server.name}</strong><small>{CORE_LABELS[server.core] ?? server.core} {server.minecraftVersion}</small></span>
              <span className="server-list-state">{STATUS_LABELS[server.status]}</span>
            </button>
          ))}
          {!servers.length && <div className="server-list-empty"><Server size={20} /><span>Установленных серверов пока нет</span></div>}
        </aside>

        {selected ? (
          <div className="server-main">
            <div className="server-detail-heading">
              <div><div className="eyebrow">{CORE_LABELS[selected.core]} · MINECRAFT {selected.minecraftVersion}</div><h2>{selected.name}</h2></div>
              <div className="server-controls">
                <button className="icon-button server-restart" onClick={() => void openServerFolder()} title="Открыть папку сервера"><FolderOpen size={16} /></button>
                <button className="button-secondary server-control-start" onClick={() => void start()} disabled={busy || isRunning} title="Запустить"><Play size={15} /> Запустить</button>
                <button className="button-secondary server-control-stop" onClick={() => void stop()} disabled={busy || !isRunning} title="Остановить"><Square size={14} /> Стоп</button>
                <button className="icon-button server-restart" onClick={() => void restart()} disabled={busy} title="Перезапустить"><RotateCw size={16} /></button>
                <button className="icon-button server-delete" onClick={() => void deleteSelectedServer()} disabled={busy || isRunning} title="Удалить сервер"><Trash2 size={16} /></button>
              </div>
            </div>
            {selected.status === "crashed" && <div className="server-crash-notice"><CircleAlert size={17} /><span>Сервер завершился с ошибкой. Проверьте последние строки журнала перед перезапуском.</span><button onClick={() => void start()}>Перезапустить</button></div>}
            <div className="server-memory-settings">
              <div><strong>Выделенная память</strong><span>{isRunning ? "Остановите сервер, чтобы применить новое значение." : `От 512 до ${memoryLimitMb.toLocaleString("ru-RU")} МБ. Применяется при следующем запуске.`}</span></div>
              <label><input type="number" min={512} max={memoryLimitMb} step={256} value={memoryDraft} onChange={(event) => setMemoryDraft(Number(event.target.value))} disabled={isRunning || savingMemory} aria-label="Память сервера в мегабайтах" /><span>МБ</span></label>
              <button className="button-secondary" onClick={() => void saveServerMemory()} disabled={isRunning || savingMemory || memoryDraft < 512 || memoryDraft > memoryLimitMb || memoryDraft === selected.memoryMb}>
                {savingMemory ? <LoaderCircle size={14} className="spin" /> : <Save size={14} />} Сохранить
              </button>
            </div>
            <div className="server-metrics">
              <div className="server-metric"><Cpu size={17} /><span>Память</span><strong>{selected.memoryUsedMb === null ? "--" : selected.memoryUsedMb >= 1024 ? `${(selected.memoryUsedMb / 1024).toFixed(1)} ГБ` : `${selected.memoryUsedMb} МБ`} / {selected.memoryMb >= 1024 ? `${(selected.memoryMb / 1024).toFixed(1)} ГБ` : `${selected.memoryMb} МБ`}</strong><small>используется / выделено</small></div>
              <div className="server-metric"><Cpu size={17} /><span>Процессор</span><strong>{selected.cpuUsagePercent === null ? "--" : `${selected.cpuUsagePercent.toFixed(1)}%`}</strong><small>нагрузка процесса Java</small></div>
              <div className="server-metric"><Gauge size={17} /><span className="server-tps-label">TPS<button className="icon-button" type="button" onClick={() => void requestTps()} disabled={!isRunning || !tpsCommandForCore(selected.core)} title="Запросить TPS" aria-label="Запросить TPS"><RotateCw size={12} /></button></span><strong>{tps ? tps.values.map((value) => value.toFixed(1)).join(" / ") : ["paper", "purpur", "forge", "neoforge"].includes(selected.core) ? "Ожидание" : "Недоступно"}</strong><small>{tps?.label ?? (["paper", "purpur", "forge", "neoforge"].includes(selected.core) ? "нажмите для запроса" : "нужен серверный TPS-мод")}</small></div>
              <div className="server-metric"><Users size={17} /><span>Игроки</span><strong>{players.length} / {playerLimit}</strong><small>по консольным событиям</small></div>
              <div className="server-metric"><Clock3 size={17} /><span>Время работы</span><strong>{formatUptime(selected.uptimeSeconds)}</strong><small>{selected.status === "running" ? "с момента запуска" : "сервер не запущен"}</small></div>
              <div className="server-metric"><Activity size={17} /><span>Состояние</span><strong className={`server-metric-status ${selected.status}`}>{STATUS_LABELS[selected.status]}</strong><small>{selected.pid ? `PID ${selected.pid}` : "процесс не создан"}</small></div>
            </div>
            <div className="server-workbench">
              <div className="server-workbench-heading"><div><Terminal size={16} /><strong>Консоль</strong></div><span>{logs.length.toLocaleString("ru-RU")} строк</span></div>
              <div className="server-console" ref={consoleRef} onScroll={(event) => {
                const element = event.currentTarget;
                followLogsRef.current = element.scrollHeight - element.scrollTop - element.clientHeight < 48;
              }} role="log" aria-live="polite">
                {!logs.length && <div className="server-console-empty">Вывод сервера появится здесь после запуска.</div>}
                {logs.map((entry, index) => <div key={`${index}-${entry.line}`} className={`server-log-line ${entry.level}`}><span>{entry.line}</span></div>)}
              </div>
              <form className="server-command-form" onSubmit={(event) => { event.preventDefault(); void send(); }}>
                <span className="server-prompt">&gt;</span><input value={command} onChange={(event) => setCommand(event.target.value)} disabled={selected.status !== "running"} placeholder={selected.status === "running" ? "Введите команду сервера" : "Запустите сервер для отправки команд"} /><button className="icon-button" type="submit" disabled={selected.status !== "running" || !command.trim()} title="Отправить команду"><Send size={16} /></button>
              </form>
              <div className="server-quick-commands">
                <div className="server-quick-heading">
                  <span>Быстрые команды</span>
                  <button className="icon-button" onClick={() => setEditingControls((editing) => !editing)} disabled={isRunning || savingControls} title={editingControls ? "Закрыть редактор" : "Редактировать команды и CPU"}>
                    {editingControls ? <X size={14} /> : <Pencil size={14} />}
                  </button>
                </div>
                {editingControls ? (
                  <div className="server-control-editor">
                    <div className="server-command-editor-list">
                      {quickCommandsDraft.map((item) => (
                        <div className="server-command-editor-row" key={item.id}>
                          <input aria-label="Название быстрой команды" value={item.label} maxLength={32} onChange={(event) => updateQuickCommand(item.id, { label: event.target.value })} placeholder="Кнопка" />
                          <input aria-label="Текст быстрой команды" value={item.command} maxLength={128} onChange={(event) => updateQuickCommand(item.id, { command: event.target.value })} placeholder="Команда сервера" />
                          <button className="icon-button" onClick={() => setQuickCommandsDraft((current) => current.filter((entry) => entry.id !== item.id))} title="Удалить быструю команду"><Trash2 size={14} /></button>
                        </div>
                      ))}
                    </div>
                    <button className="server-add-command" onClick={addQuickCommand} disabled={quickCommandsDraft.length >= 16} type="button"><Plus size={13} /> Добавить команду</button>
                    <label className="server-cpu-setting"><span>CPU-потоки, видимые JVM</span><select value={processorCountDraft ?? ""} onChange={(event) => setProcessorCountDraft(event.target.value ? Number(event.target.value) : null)} disabled={isRunning || savingControls || (selected.javaVersion !== null && selected.javaVersion !== undefined && selected.javaVersion < 10)}><option value="">Авто</option>{Array.from({ length: logicalProcessorCount }, (_, index) => index + 1).map((count) => <option key={count} value={count}>{count}</option>)}</select></label>
                    <p className="server-cpu-note">{selected.javaVersion !== null && selected.javaVersion !== undefined && selected.javaVersion < 10 ? `Java ${selected.javaVersion} не поддерживает ActiveProcessorCount; будет использовано значение JVM по умолчанию.` : "Настройка влияет на число потоков, видимых JVM, но не ограничивает загрузку CPU на уровне Windows."}</p>
                    <div className="server-control-editor-actions">
                      <button className="button-secondary" type="button" onClick={cancelServerControls} disabled={savingControls}>Отмена</button>
                      <button className="button-secondary" type="button" onClick={() => void saveServerControls()} disabled={isRunning || savingControls || quickCommandsDraft.length > 16}>{savingControls ? <LoaderCircle size={13} className="spin" /> : <Check size={13} />} Сохранить</button>
                    </div>
                  </div>
                ) : quickCommandsDraft.length ? (
                  quickCommandsDraft.map((item) => <button key={item.id} onClick={() => void send(item.command)} disabled={selected.status !== "running"} title={item.command}>{item.label}</button>)
                ) : <span className="server-no-quick-commands">Команды не добавлены</span>}
              </div>
            </div>
            <section className="server-players">
              <div className="server-section-heading"><div><Users size={16} /><h3>Игроки онлайн</h3></div><span>{players.length} / {playerLimit}</span></div>
              {players.length ? players.map((player) => <div className="server-player-row" key={player}><span className="server-state-dot running" /><strong>{player}</strong><div><button onClick={() => void send(`kick ${player}`)} disabled={selected.status !== "running"}>Кик</button><button onClick={() => void send(`op ${player}`)} disabled={selected.status !== "running"}>Оператор</button><button onClick={() => void send(`whitelist add ${player}`)} disabled={selected.status !== "running"}>В белый список</button></div></div>) : <p className="server-players-empty">Список обновится после входа игроков или команды <code>list</code>.</p>}
            </section>
            <section className="server-content-section">
              <div className="server-content-heading">
                <div><h3>Контент сервера</h3><span>{isRunning ? "Остановите сервер, чтобы изменять файлы контента." : "Каталог Modrinth"}</span></div>
                <span>{installedContent.length} установлено</span>
              </div>
              <button className="button-secondary server-catalog-open" onClick={() => setShowContentCatalog(true)}>
                <Download size={15} /> Открыть каталог
              </button>
              {installedContent.length > 0 && (
                <div className="server-installed-content">
                  <h4>Установленные {SERVER_CONTENT_LABELS[contentType].toLowerCase()}</h4>
                  {installedContent.map((item) => <article className="server-installed-item" key={item.fileName}>
                    <div><strong>{item.title ?? item.fileName}</strong><span>{item.versionNumber ? `Версия ${item.versionNumber} · ` : ""}{item.enabled ? "Включён" : "Выключен"}{item.projectId ? " · Modrinth" : " · локальный файл"}{item.updateAvailable ? " · доступно обновление" : ""}</span></div>
                    {item.projectId && item.updateAvailable && <button className="icon-button" onClick={() => void updateServerProject(item)} disabled={contentBusy || isRunning} title="Обновить"><RotateCw size={15} /></button>}
                    <button className="icon-button" onClick={() => void toggleServerProject(item)} disabled={contentBusy || isRunning} title={item.enabled ? "Выключить" : "Включить"}>{item.enabled ? <Square size={14} /> : <Play size={14} />}</button>
                    <button className="icon-button danger-button" onClick={() => void removeServerProject(item)} disabled={contentBusy || isRunning} title="Удалить"><Trash2 size={15} /></button>
                  </article>)}
                </div>
              )}
            </section>
            {showContentCatalog && createPortal((
              <div
                className={`server-catalog-backdrop ${themeClassName(theme, uiPalettes[theme])}`}
                style={{
                  "--accent": uiPalettes[theme].accent,
                  "--button": uiPalettes[theme].button,
                  "--canvas": uiPalettes[theme].canvas,
                  "--surface": uiPalettes[theme].surface,
                  "--ink": uiPalettes[theme].ink,
                  colorScheme: theme === "light" || (theme === "custom" && isLightPalette(uiPalettes.custom)) ? "light" : "dark",
                } as React.CSSProperties}
                role="presentation"
                onMouseDown={(event) => { if (event.target === event.currentTarget && !contentBusy) setShowContentCatalog(false); }}
              >
                <section className="server-catalog-dialog" role="dialog" aria-modal="true" aria-labelledby="server-catalog-title">
                  <header className="server-catalog-heading">
                    <div><span className="eyebrow">{selected.name} · MINECRAFT {selected.minecraftVersion}</span><h2 id="server-catalog-title">Каталог контента</h2></div>
                    <button className="icon-button" aria-label="Закрыть каталог" title="Закрыть" onClick={() => setShowContentCatalog(false)} disabled={contentBusy}><X size={17} /></button>
                  </header>
                  <nav className="catalog-type-tabs" aria-label="Тип контента">
                    {(selected.core === "vanilla"
                      ? ["datapack"]
                      : selected.core === "paper" || selected.core === "purpur"
                        ? ["plugin", "datapack"]
                        : ["mod", "datapack"]
                    ).map((type) => <button key={type} type="button" className={contentType === type ? "active" : ""} onClick={() => {
                      setContentType(type as ServerContentType);
                      setContentCategory("");
                      setContentResults([]);
                      setContentSearched(false);
                      setContentPage(0);
                    }}>{SERVER_CONTENT_LABELS[type as ServerContentType]}</button>)}
                  </nav>
                  {contentType === "datapack" && (
                    <label className="catalog-world-select">
                      <span>Мир для датапака</span>
                      <select value={contentWorld} onChange={(event) => setContentWorld(event.target.value)} disabled={!serverWorlds.length}>
                        <option value="">{serverWorlds.length ? "Выберите мир" : "У сервера нет существующих миров"}</option>
                        {serverWorlds.map((world) => <option key={world} value={world}>{world}</option>)}
                      </select>
                    </label>
                  )}
                  <form className="server-content-search" onSubmit={(event) => { event.preventDefault(); void searchContent(0); }}>
                    <label><Search size={16} /><input value={contentQuery} onChange={(event) => setContentQuery(event.target.value)} placeholder={`Поиск: ${SERVER_CONTENT_LABELS[contentType].toLowerCase()} Modrinth`} aria-label="Поиск контента Modrinth" /></label>
                    <button className="button-secondary" type="submit" disabled={contentBusy || (contentType === "datapack" && !contentWorld)}>{contentBusy ? <LoaderCircle size={14} className="spin" /> : <Search size={14} />} Найти</button>
                  </form>
                  <div className="server-catalog-filters">
                    {contentType === "mod" && <label><span><Filter size={13} /> Категория</span><select value={contentCategory} onChange={(event) => setContentCategory(event.target.value)}><option value="">Все категории</option>{MOD_CATALOG_CATEGORIES.map((category) => <option key={category} value={category}>{category[0].toUpperCase() + category.slice(1)}</option>)}</select></label>}
                    <label><span>Сортировка</span><select value={contentSort} onChange={(event) => setContentSort(event.target.value as "downloads" | "relevance")}><option value="downloads">По загрузкам</option><option value="relevance">По релевантности</option></select></label>
                  </div>
                  {contentSearched && <div className="server-content-results-heading"><strong>Результаты</strong><span>{contentTotal.toLocaleString("ru-RU")} найдено</span><div><button className="icon-button" onClick={() => void searchContent(contentPage - 1)} disabled={contentBusy || contentPage === 0} title="Предыдущая страница"><ChevronLeft size={15} /></button><span>{contentPage + 1} / {Math.max(1, Math.ceil(contentTotal / 20))}</span><button className="icon-button" onClick={() => void searchContent(contentPage + 1)} disabled={contentBusy || (contentPage + 1) * 20 >= contentTotal} title="Следующая страница"><ChevronRight size={15} /></button></div></div>}
                  {contentResults.length > 0 && (
                    <div className="server-content-results">
                      {contentResults.map((project) => {
                        const installed = installedContent.some((item) => item.projectId === project.projectId);
                        return <article className="server-content-result" key={project.projectId}>
                          {project.iconUrl ? <img src={project.iconUrl} alt="" /> : <div className="server-content-placeholder"><Server size={16} /></div>}
                          <div><strong>{project.title}</strong><p>{project.description}</p><small><Download size={12} /> {project.downloads.toLocaleString("ru-RU")} загрузок</small></div>
                          <button className="button-secondary" disabled={contentBusy || isRunning || installed} onClick={() => void beginInstallServerProject(project)}>{installed ? <><CheckCircle2 size={14} /> Установлен</> : <><Download size={14} /> Установить</>}</button>
                        </article>;
                      })}
                    </div>
                  )}
                  {contentBusy && contentResults.length === 0 && <div className="server-content-empty"><LoaderCircle size={16} className="spin" /> Загружаем проекты Modrinth...</div>}
                  {contentSearched && !contentBusy && contentResults.length === 0 && <p className="server-content-empty">Совместимый контент не найден.</p>}
                </section>
              </div>
            ), document.body)}
            {pendingContentInstall && (
              <div className="server-optional-backdrop">
                <section className="server-optional-dialog" role="dialog" aria-modal="true" aria-labelledby="server-optional-title">
                  <div className="server-optional-heading"><div><span className="eyebrow">MODRINTH</span><h3 id="server-optional-title">Опциональные зависимости</h3></div><button className="icon-button" onClick={cancelOptionalDependencies} title="Закрыть"><X size={16} /></button></div>
                  <p>{pendingContentInstall.title} может работать с дополнительными компонентами. Выберите, что установить вместе с ним.</p>
                  <div className="server-optional-list">
                    {optionalDependencies.map((dependency) => <label key={dependency.versionId}><input type="checkbox" checked={selectedOptionalVersions.includes(dependency.versionId)} onChange={(event) => setSelectedOptionalVersions((current) => event.target.checked ? [...current, dependency.versionId] : current.filter((id) => id !== dependency.versionId))} /><span><strong>{dependency.title || dependency.projectId}</strong><small>Версия {dependency.versionNumber}</small></span></label>)}
                  </div>
                  <div className="server-optional-actions"><button className="button-secondary" onClick={cancelOptionalDependencies}>Отмена</button><button className="button-secondary" onClick={() => void confirmOptionalDependencies()} disabled={contentBusy}>{contentBusy ? <LoaderCircle size={14} className="spin" /> : <Check size={14} />} Установить</button></div>
                </section>
              </div>
            )}
          </div>
        ) : (
          <div className="server-empty-main"><Server size={27} /><h2>Панель сервера</h2><p>Установите серверное ядро, чтобы открыть консоль и управление процессом.</p><button className="button-primary" onClick={() => setShowCreate(true)}>Создать сервер</button></div>
        )}
      </div>
    </section>
    <ActionDialog request={actionDialog} onClose={() => setActionDialog(null)} />
    </>
  );
}