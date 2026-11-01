export type View = "instances" | "instance-detail" | "mods" | "modpacks" | "repository" | "accounts" | "settings" | "servers" | "about";
export type InstanceDetailTab = "content" | "files" | "worlds" | "logs";
export type Theme = "light" | "dark" | "custom";
export type PaletteColor = "accent" | "button" | "canvas" | "surface" | "ink";

export interface ThemePalette {
  accent: string;
  button: string;
  canvas: string;
  surface: string;
  ink: string;
}

export type ThemePalettes = Record<Theme, ThemePalette>;
export type DatabaseStatus = "loading" | "ready" | "error";

export interface ThemeRow {
  value: string;
}

export interface Instance {
  id: string;
  name: string;
  folder_name: string;
  minecraft_version: string;
  loader_type: string;
  loader_version: string | null;
  java_version: number;
  java_path: string | null;
  memory_mb: number;
  jvm_args: string;
  window_width: number;
  window_height: number;
  fullscreen: number;
  avatar_data: string | null;
  modpack_project_id?: string | null;
  modpack_version_id?: string | null;
  modpack_version_number?: string | null;
  repository_url?: string | null;
  repository_pack_id?: string | null;
  repository_version?: string | null;
  repository_channel?: string | null;
  created_at: string;
}

export interface SchemaColumn {
  name: string;
}

export interface Account {
  id: string;
  username: string;
  uuid: string;
  account_type: string;
}

export interface MinecraftVersions {
  versions: string[];
  latest: string;
}

export interface DownloadProgress {
  completed: number;
  total: number;
  currentFile: string;
}

export type InstallStage = "minecraft" | "java" | "loader" | "launch";

export interface GameLog {
  folderName: string;
  stream: "stdout" | "stderr";
  line: string;
}

export interface GameExit {
  folderName: string;
  success: boolean;
  code: number | null;
}

export type ServerStatus = "stopped" | "starting" | "running" | "stopping" | "crashed";

export interface ServerQuickCommand {
  id: string;
  label: string;
  command: string;
}

export interface ServerSummary {
  name: string;
  minecraftVersion: string;
  core: string;
  loaderVersion: string | null;
  memoryMb: number;
  quickCommands: ServerQuickCommand[];
  activeProcessorCount: number | null;
  javaVersion: number | null;
  installedAt: number;
  status: ServerStatus;
  uptimeSeconds: number;
  pid: number | null;
  memoryUsedMb: number | null;
  cpuUsagePercent: number | null;
}

export interface ServerLog {
  serverName: string;
  line: string;
  level: "info" | "warn" | "error" | "debug" | "command";
}

export interface ServerStatusEvent {
  name: string;
  status: ServerStatus;
}

export interface DeviceCodeInfo {
  flowId: string;
  userCode: string;
  verificationUri: string;
  expiresIn: number;
  interval: number;
}

export interface LoaderVersion {
  version: string;
  channel: string;
}

export interface ServerInstallOptions {
  cores: string[];
  loaderVersions: Record<string, LoaderVersion[]>;
  warnings: string[];
}

export interface ServerInstallRequest {
  name: string;
  minecraftVersion: string;
  core: string;
  loaderVersion: string | null;
  memoryMb: number;
  eulaAccepted: boolean;
}

export const MOD_LOADER_TYPES = ["fabric", "forge", "neoforge"] as const;
export type ModLoaderType = (typeof MOD_LOADER_TYPES)[number];

export const MOD_CATALOG_LOADERS = ["fabric", "forge", "neoforge", "quilt"] as const;
export const MOD_CATALOG_CATEGORIES = [
  "optimization",
  "technology",
  "magic",
  "adventure",
  "decoration",
  "library",
] as const;
export const MOD_CATALOG_PROJECT_TYPES = [
  { value: "mod", label: "Моды" },
  { value: "resourcepack", label: "Текстур-паки" },
  { value: "shader", label: "Шейдеры" },
  { value: "datapack", label: "Датапаки" },
] as const;
export type CatalogProjectType = (typeof MOD_CATALOG_PROJECT_TYPES)[number]["value"];

export interface ModrinthProject {
  projectId: string;
  slug: string;
  title: string;
  description: string;
  author: string;
  categories: string[];
  iconUrl: string | null;
  downloads: number;
}

export interface ModSearchPage {
  hits: ModrinthProject[];
  totalHits: number;
}

export interface ModpackSearchPage {
  hits: ModrinthProject[];
  totalHits: number;
}

export interface ModpackVersion {
  versionId: string;
  versionNumber: string;
  name: string | null;
  gameVersions: string[];
  loaders: string[];
  fileName: string;
  datePublished: string;
}

export interface ModpackDetails {
  project: ModrinthProject;
  versions: ModpackVersion[];
}

export interface ModpackInstallResult {
  minecraftVersion: string;
  loaderType: string;
  loaderVersion: string | null;
  javaVersion: number;
  javaPath: string;
  modpackProjectId: string;
  modpackVersionId: string;
  modpackVersionNumber: string;
}

export interface ModpackInstallTask {
  folderName: string;
  projectTitle: string;
}

export interface RepositoryPack {
  id: string;
  name: string;
  description: string;
  authors: string[];
  categories: string[];
  iconUrl: string | null;
  latestVersion: string;
  channel: string;
}

export interface RepositoryPackVersion {
  version: string;
  channel: string;
  updatedAt: number;
}

export interface RepositoryInstallResult {
  minecraftVersion: string;
  loaderType: string;
  loaderVersion: string | null;
  javaVersion: number;
  javaPath: string;
  packId: string;
  version: string;
  channel: string;
  name: string;
  repositoryUrl: string;
  memoryMb: number | null;
  jvmArgs: string | null;
  windowWidth: number | null;
  windowHeight: number | null;
  fullscreen: boolean | null;
  transactionId: string | null;
}

export interface PendingRepositoryUpdate {
  transactionId: string;
  instanceFolder: string;
  version: string;
}

export interface ToastNotification {
  id: number;
  message: string;
  kind: "error" | "success";
}

export interface InstalledMod {
  fileName: string;
  enabled: boolean;
  projectId: string | null;
  versionId: string | null;
  versionNumber: string | null;
  title: string | null;
  iconUrl: string | null;
}

export interface ServerInstalledContent extends InstalledMod {
  updateAvailable: boolean;
}

export interface ServerOptionalDependency {
  projectId: string;
  versionId: string;
  versionNumber: string;
  title: string;
}

export interface ModVersionOption {
  versionId: string;
  versionNumber: string;
}

export interface DatabaseState {
  accounts: Account[];
  activeAccountId: string;
  microsoftClientId: string;
  instances: Instance[];
  theme: Theme;
  uiPalettes: ThemePalettes;
}

export interface InstanceDraft {
  name: string;
  minecraftVersion: string;
  loaderType: string;
  loaderVersion: string | null;
  memoryMb: number;
  jvmArgs: string;
  windowWidth: number;
  windowHeight: number;
  fullscreen: boolean;
  avatarData: string | null;
  javaVersion?: number;
  javaPath?: string | null;
}

export interface InstanceForm {
  name: string;
  avatarData: string | null;
  version: string;
  loader: string;
  loaderVersion: string;
  memoryMb: number;
  jvmArgs: string;
  windowWidth: number;
  windowHeight: number;
  fullscreen: boolean;
}
