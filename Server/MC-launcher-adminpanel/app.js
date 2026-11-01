const state = { csrf: "", dashboard: null, view: "overview", filter: "all", search: "", busy: false, loaderRequest: 0, minecraftRequest: 0, editingRelease: null, editingFiles: [], activeRelease: null, filePaths: new WeakMap(), folderRoots: new WeakMap(), uploadPaths: new WeakMap(), uploadFiles: [], uploadController: null };
let dashboardRefreshPromise = null;
let dashboardPollTimer = null;
const DASHBOARD_REFRESH_INTERVAL = 15000;
const $ = (selector, root = document) => root.querySelector(selector);
const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];
function setTheme(theme) {
  const dark = theme === "dark";
  document.documentElement.dataset.theme = dark ? "dark" : "light";
  const button = $("#theme-toggle");
  const label = dark ? "Включить светлую тему" : "Включить тёмную тему";
  button.setAttribute("aria-label", label);
  button.setAttribute("title", label);
  button.setAttribute("aria-pressed", String(dark));
  $("use", button).setAttribute("href", dark ? "/icons.svg#icon-sun" : "/icons.svg#icon-moon");
}
try {
  setTheme(localStorage.getItem("mc-launcher-admin-theme") === "dark" ? "dark" : "light");
} catch {
  setTheme("light");
}
const loaderVersionSelect = document.createElement("select");
loaderVersionSelect.id = "loader-version";
loaderVersionSelect.name = "loaderVersion";
loaderVersionSelect.setAttribute("aria-describedby", "loader-hint");
loaderVersionSelect.add(new Option("Выберите версию", ""));
$("#loader-version").replaceWith(loaderVersionSelect);
const minecraftVersionSelect = document.createElement("select");
minecraftVersionSelect.id = "game-version";
minecraftVersionSelect.name = "gameVersion";
minecraftVersionSelect.required = true;
minecraftVersionSelect.setAttribute("aria-describedby", "game-version-hint");
minecraftVersionSelect.add(new Option("Загрузка версий Minecraft…", ""));
const minecraftVersionField = document.createElement("div");
minecraftVersionField.className = "field";
const minecraftVersionLabel = document.createElement("label");
minecraftVersionLabel.htmlFor = "game-version";
minecraftVersionLabel.textContent = $("#game-version").closest(".field").querySelector("span").textContent;
$("#game-version").closest(".field").replaceWith(minecraftVersionField);
const minecraftVersionHint = document.createElement("small");
minecraftVersionHint.id = "game-version-hint";
minecraftVersionHint.textContent = "Список загружается из официального Mojang manifest.";
const minecraftVersionTools = document.createElement("div");
minecraftVersionTools.className = "field-tools";
const refreshMinecraftVersionsButton = document.createElement("button");
refreshMinecraftVersionsButton.className = "button button-quiet";
refreshMinecraftVersionsButton.type = "button";
refreshMinecraftVersionsButton.textContent = "Обновить список";
refreshMinecraftVersionsButton.addEventListener("click", () => loadMinecraftVersions(minecraftVersionSelect.value, true));
minecraftVersionTools.append(minecraftVersionHint, refreshMinecraftVersionsButton);
minecraftVersionField.append(minecraftVersionLabel, minecraftVersionSelect, minecraftVersionTools);
const loaderVersionField = document.createElement("div");
loaderVersionField.className = "field";
loaderVersionField.id = "loader-version-field";
const loaderVersionLabel = document.createElement("label");
loaderVersionLabel.htmlFor = "loader-version";
loaderVersionLabel.textContent = $("#loader-version-field span").textContent;
const loaderVersionHint = $("#loader-hint");
$("#loader-version-field").replaceWith(loaderVersionField);
loaderVersionField.append(loaderVersionLabel, loaderVersionSelect);
const folderInput = document.createElement("input");
folderInput.id = "release-folders";
folderInput.type = "file";
folderInput.multiple = true;
folderInput.setAttribute("webkitdirectory", "");
folderInput.setAttribute("directory", "");
folderInput.hidden = true;
const folderPicker = document.createElement("button");
folderPicker.className = "button button-quiet folder-picker";
folderPicker.type = "button";
folderPicker.innerHTML = `<svg><use href="/icons.svg#icon-folder"></use></svg>Выбрать папку с подпапками`;
const folderTools = document.createElement("div");
folderTools.className = "folder-upload-tools";
folderTools.append(folderPicker, folderInput);
$("#upload-destination").insertAdjacentElement("afterend", folderTools);
const cancelUploadButton = document.createElement("button");
cancelUploadButton.id = "cancel-upload";
cancelUploadButton.className = "button button-quiet";
cancelUploadButton.type = "button";
cancelUploadButton.textContent = "Отменить загрузку";
cancelUploadButton.hidden = true;
$("#create-release-submit").before(cancelUploadButton);

function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>"']/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[character]);
}

function formatApiError(value, fallback) {
  if (typeof value === "string") return value;
  if (value && typeof value === "object") {
    const message = typeof value.message === "string" ? value.message : "";
    const fields = Array.isArray(value.fields) ? value.fields.map((field) => {
      if (typeof field === "string") return field;
      if (!field || typeof field !== "object") return "";
      return field.path && field.message ? `${field.path}: ${field.message}` : field.message || "";
    }).filter(Boolean) : [];
    const details = [message, ...fields].filter(Boolean).join("\n");
    if (details) return details;
    if (typeof value.code === "string") return value.code;
  }
  return fallback;
}

function apiRetryDelay(attempt, retryAfter) {
  if (retryAfter) {
    const seconds = Number(retryAfter);
    if (Number.isFinite(seconds) && seconds >= 0) return Math.min(seconds, 30) * 1000;
    const retryAt = Date.parse(retryAfter);
    if (Number.isFinite(retryAt)) return Math.min(Math.max(0, (retryAt - Date.now()) / 1000), 30) * 1000;
  }
  return Math.min(2 ** attempt, 30) * 1000;
}

async function api(path, options = {}) {
  const headers = new Headers(options.headers || {});
  if (options.body && !(options.body instanceof Blob)) headers.set("Content-Type", "application/json");
  if (options.method && options.method !== "GET" && state.csrf) headers.set("X-CSRF-Token", state.csrf);
  const method = (options.method || "GET").toUpperCase();
  const canRetry = method === "GET" || method === "HEAD";
  let response;
  for (let attempt = 0; ; attempt += 1) {
    try {
      response = await fetch(path, { ...options, headers, credentials: "same-origin" });
    } catch (error) {
      if (!canRetry || attempt >= 2) throw error;
      await new Promise((resolve) => setTimeout(resolve, apiRetryDelay(attempt)));
      continue;
    }
    if (!canRetry || attempt >= 2 || ![408, 429, 500, 502, 503, 504].includes(response.status)) break;
    const retryAfter = response.headers.get("Retry-After");
    await response.body?.cancel().catch(() => undefined);
    await new Promise((resolve) => setTimeout(resolve, apiRetryDelay(attempt, retryAfter)));
  }
  let payload = {};
  if (response.status !== 204) {
    try {
      const parsed = await response.json();
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
        if (response.ok) throw new Error("unexpected response shape");
      } else {
        payload = parsed;
      }
    } catch {
      if (response.ok) {
        const requestId = response.headers.get("X-Request-ID");
        throw new Error(`Сервер вернул пустой, некорректный JSON или неожиданный формат ответа (HTTP ${response.status}).${requestId ? ` ID запроса: ${requestId}.` : ""}`);
      }
    }
  }
  const requestId = response.headers.get("X-Request-ID") ||
    (typeof payload.error === "object" && payload.error ? payload.error.requestId : "");
  if (response.status === 401) {
    state.csrf = "";
    showLogin(`Сессия завершилась. Войдите снова.${requestId ? ` ID запроса: ${requestId}.` : ""}`);
    throw new Error(`Требуется повторный вход.${requestId ? ` ID запроса: ${requestId}.` : ""}`);
  }
  if (!response.ok) {
    const details = Array.isArray(payload.validationErrors) ? `\n${payload.validationErrors.map((item) => formatApiError(item, JSON.stringify(item))).join("\n")}` : "";
    if (response.status === 501 && /\/api\/admin\/releases\/\d+\/validate$/.test(path)) {
      throw new Error("Сервер не поддерживает проверку версии (HTTP 501). Обновите MC-launcher-server и перезапустите его.");
    }
    const errorMessage = formatApiError(payload.error, `Ошибка ${response.status}`);
    const retryAfter = response.headers.get("Retry-After");
    const diagnostics = [`HTTP ${response.status}`, requestId && `ID запроса: ${requestId}`, retryAfter && `Повторите после: ${retryAfter}`]
      .filter(Boolean)
      .join("; ");
    throw new Error(`${errorMessage}${details}\n${diagnostics}`);
  }
  return payload;
}

function showLogin(message = "") {
  $("#app").hidden = true;
  $("#login-view").hidden = false;
  $("#login-error").textContent = message;
  $("#login-password").focus();
}

function showApp() {
  $("#login-view").hidden = true;
  $("#app").hidden = false;
  $("#today-label").textContent = new Intl.DateTimeFormat("ru-RU", { day: "numeric", month: "long", year: "numeric" }).format(new Date());
  refreshDashboard();
  startDashboardPolling();
}

async function bootstrap() {
  try {
    const response = await fetch("/api/admin/session", { credentials: "same-origin" });
    if (!response.ok) {
      showLogin();
      return;
    }
    const session = await response.json();
    state.csrf = session.csrfToken;
    showApp();
  } catch {
    showLogin();
  }
}

async function refreshDashboard({ silent = false } = {}) {
  if (dashboardRefreshPromise) return dashboardRefreshPromise;
  if (!silent) $("#loading-releases").hidden = false;
  dashboardRefreshPromise = (async () => {
    try {
      state.dashboard = await api("/api/admin/dashboard?page=1&perPage=100");
      renderDashboard();
    } catch (error) {
      if (!$("#login-view").hidden) showToast(error.message, "error");
    } finally {
      if (!silent) $("#loading-releases").hidden = true;
    }
  })();
  try {
    await dashboardRefreshPromise;
  } finally {
    dashboardRefreshPromise = null;
  }
}

function startDashboardPolling() {
  if (dashboardPollTimer) return;
  dashboardPollTimer = setInterval(() => {
    if (!document.hidden && state.csrf) refreshDashboard({ silent: true });
  }, DASHBOARD_REFRESH_INTERVAL);
}

document.addEventListener("visibilitychange", () => {
  if (!document.hidden && state.csrf) refreshDashboard({ silent: true });
});

function formatBytes(bytes) {
  if (!bytes) return "0 Б";
  const units = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  return `${(bytes / (1024 ** index)).toLocaleString("ru-RU", { maximumFractionDigits: index ? 1 : 0 })} ${units[index]}`;
}

function dateLabel(timestamp) {
  if (!timestamp) return "—";
  return new Intl.DateTimeFormat("ru-RU", { day: "numeric", month: "short", year: "numeric" }).format(new Date(timestamp * 1000));
}

function statusMarkup(release) {
  const status = release.status === "published" ? ["Опубликована", "status-published"] : release.status === "archived" ? ["В архиве", "status-archived"] : ["Черновик", "status-draft"];
  return `<span class="status-badge ${status[1]}"><i></i>${status[0]}</span>`;
}

function readinessMarkup(release) {
  if (release.status === "published") return `<span class="readiness readiness-good"><svg><use href="/icons.svg#icon-check"></use></svg>Выпущена</span>`;
  if (release.ready && !release.needsAttention) return `<span class="readiness readiness-good"><svg><use href="/icons.svg#icon-check"></use></svg>Готова</span>`;
  return `<span class="readiness readiness-wait"><svg><use href="/icons.svg#icon-alert"></use></svg>Нужна проверка</span>`;
}

function releaseRow(release) {
  const loader = release.loader && release.loader !== "vanilla" ? ` · ${escapeHtml(release.loader)}` : "";
  return `<tr class="release-row" data-release-id="${release.id}" tabindex="0"><td><div class="pack-cell"><span class="pack-icon-preview">${escapeHtml((release.packId || "M").slice(0, 1).toUpperCase())}</span><span class="pack-cell-copy"><strong>${escapeHtml(release.packId)}</strong><small>v${escapeHtml(release.version)} · обновлено ${dateLabel(release.updatedAt)}</small></span></div></td><td><span class="game-version">${escapeHtml(release.gameVersion || "Не указана")}</span><small class="game-loader">${loader}</small></td><td><span class="channel-label channel-${release.channel === "beta" ? "beta" : "stable"}"><i></i>${release.channel === "beta" ? "Beta" : "Stable"}</span></td><td>${statusMarkup(release)}</td><td>${readinessMarkup(release)}</td><td><button class="icon-button row-open" data-open-release="${release.id}" aria-label="Открыть версию" title="Открыть"><svg><use href="/icons.svg#icon-arrow"></use></svg></button></td></tr>`;
}

function activityItem(item, compact = false) {
  const names = { pack_create: "Создана сборка", release_create: "Создан черновик", release_publish: "Опубликована версия", release_unpublish: "Снята с публикации", file_upload: "Загружен файл", login: "Вход в панель", release_clone: "Создана копия версии", release_archive: "Версия архивирована", release_restore: "Версия восстановлена" };
  const title = names[item.action] || item.action.replaceAll("_", " ");
  return `<article class="activity-item"><span class="activity-marker ${item.action === "release_publish" ? "marker-published" : ""}"><svg><use href="/icons.svg#${item.action === "release_publish" ? "icon-globe" : item.action === "file_upload" ? "icon-upload" : "icon-spark"}"></use></svg></span><div class="activity-copy"><strong>${escapeHtml(title)}</strong><span>${escapeHtml(item.detail || "Административное действие")}</span></div><time>${dateLabel(item.at)}</time>${compact ? "" : `<span class="actor-label">${escapeHtml(item.actor || "admin")}</span>`}</article>`;
}

function renderDashboard() {
  if (!state.dashboard) return;
  const { packs = [], releases = [], releaseSummary = {}, storage = {}, audit = [] } = state.dashboard;
  $("#metric-packs").textContent = packs.length.toLocaleString("ru-RU");
  $("#metric-published").textContent = Number(releaseSummary.published || 0).toLocaleString("ru-RU");
  $("#metric-drafts").textContent = Number(releaseSummary.drafts || 0).toLocaleString("ru-RU");
  $("#metric-downloads").textContent = Number(storage.downloads || 0).toLocaleString("ru-RU");
  const rows = releases.slice(0, 7);
  $("#recent-releases").innerHTML = rows.map(releaseRow).join("");
  $("#empty-releases").hidden = rows.length > 0;
  $("#recent-activity").innerHTML = audit.slice(0, 4).map((item) => activityItem(item, true)).join("") || `<div class="activity-empty">Действий пока нет.</div>`;
  $("#activity-feed").innerHTML = audit.map((item) => activityItem(item)).join("") || `<div class="empty-state"><h3>Журнал пока пуст</h3></div>`;
  $("#filter-count-all").textContent = releases.length;
  $("#filter-count-draft").textContent = releases.filter((item) => item.status === "draft").length;
  $("#filter-count-published").textContent = releases.filter((item) => item.status === "published").length;
  renderAllReleases();
}

function renderAllReleases() {
  const releases = state.dashboard?.releases || [];
  const query = state.search.trim().toLocaleLowerCase("ru-RU");
  const filtered = releases.filter((item) => (state.filter === "all" || item.status === state.filter) && `${item.packId} ${item.version} ${item.gameVersion}`.toLocaleLowerCase("ru-RU").includes(query));
  $("#all-releases").innerHTML = filtered.map(releaseRow).join("");
  $("#empty-all-releases").hidden = filtered.length > 0;
}

function setView(view) {
  state.view = view;
  const names = { overview: "Обзор", releases: "Сборки и версии", activity: "Журнал действий" };
  $$(".view-section").forEach((section) => { section.hidden = section.id !== `${view}-view`; });
  $$(".nav-link").forEach((button) => button.classList.toggle("is-active", button.dataset.view === view));
  $("#breadcrumb-current").textContent = names[view] || "Обзор";
  if (location.hash !== `#${view}`) history.replaceState(null, "", `#${view}`);
}

function showToast(message, kind = "success") {
  const toast = $("#toast");
  toast.textContent = message;
  toast.className = `toast is-visible toast-${kind}`;
  clearTimeout(showToast.timeout);
  showToast.timeout = setTimeout(() => toast.classList.remove("is-visible"), kind === "error" ? 12000 : 4200);
}

function confirmAction({ eyebrow = "ПОДТВЕРЖДЕНИЕ", title, message, confirmLabel = "Продолжить", danger = false }) {
  const dialog = $("#confirm-dialog");
  const acceptButton = $("#confirm-accept");
  $("#confirm-eyebrow").textContent = eyebrow;
  $("#confirm-title").textContent = title;
  $("#confirm-message").textContent = message;
  acceptButton.textContent = confirmLabel;
  acceptButton.className = `button ${danger ? "button-danger" : "button-primary"}`;

  return new Promise((resolve) => {
    let settled = false;
    const finish = (accepted) => {
      if (settled) return;
      settled = true;
      if (dialog.open) dialog.close();
      resolve(accepted);
    };
    $("#confirm-cancel").onclick = () => finish(false);
    acceptButton.onclick = () => finish(true);
    dialog.addEventListener("cancel", (event) => {
      event.preventDefault();
      finish(false);
    }, { once: true });
    dialog.addEventListener("close", () => finish(false), { once: true });
    dialog.showModal();
    $("#confirm-cancel").focus();
  });
}

function setBusy(button, busy, label) {
  const text = $("span", button);
  if (!button.dataset.originalLabel && text) button.dataset.originalLabel = text.textContent;
  button.disabled = busy;
  button.classList.toggle("is-busy", busy);
  if (text) text.textContent = busy ? label : button.dataset.originalLabel || text.textContent;
}

function openCreateDialog() {
  state.editingRelease = null;
  state.editingFiles = [];
  const select = $("#pack-choice");
  const packs = state.dashboard?.packs || [];
  select.innerHTML = `<option value="new">＋ Создать новую сборку</option>${packs.map((pack) => `<option value="${escapeHtml(pack.id)}">${escapeHtml(pack.name || pack.title || pack.id)}</option>`).join("")}`;
  $("#new-pack-fields").hidden = false;
  $("#pack-name").required = true;
  $("#pack-id").required = true;
  $("#release-form").reset();
  $("#release-form").classList.remove("is-editing");
  select.disabled = false;
  $("#pack-id").closest(".field").hidden = false;
  $("#pack-id").required = true;
  $("#release-version").disabled = false;
  $("#release-channel").disabled = false;
  delete $("#pack-id").dataset.edited;
  $("#release-version").value = "1.0.0";
  $("#java-major").value = "21";
  $("#memory-mb").value = "4096";
  $("#window-width").value = "1280";
  $("#window-height").value = "720";
  $("#file-queue").hidden = true;
  $("#file-queue").replaceChildren();
  $("#release-files").value = "";
  state.uploadFiles = [];
  $("#release-dialog .eyebrow").textContent = "НОВАЯ ВЕРСИЯ";
  $("#release-dialog h2").textContent = "Создать сборку";
  $("#release-dialog .dialog-header p").textContent = "Настройте сборку и подготовьте первый релиз.";
  $("#create-release-submit span").textContent = "Создать черновик";
  $("#create-release-submit").dataset.originalLabel = "Создать черновик";
  togglePackFields();
  loadMinecraftVersions();
  if (!$("#release-dialog").open) $("#release-dialog").showModal();
}

function openEditDialog(release, pendingFiles = []) {
  const manifest = release.manifest;
  const minecraft = manifest.minecraft || {};
  const settings = manifest.settings || {};
  openCreateDialog();
  state.editingRelease = release;
  state.editingFiles = [...(manifest.files || [])].map((file) => ({ ...file }));
  state.uploadFiles = pendingFiles;
  $("#release-form").classList.add("is-editing");
  const packChoice = $("#pack-choice");
  if (![...packChoice.options].some((option) => option.value === release.packId)) {
    packChoice.add(new Option(manifest.name || release.packId, release.packId));
  }
  packChoice.value = release.packId;
  $("#pack-choice").disabled = true;
  $("#new-pack-fields").hidden = false;
  $("#pack-id").closest(".field").hidden = true;
  $("#pack-id").required = false;
  $("#pack-name").value = manifest.name || release.packId;
  $("#pack-description").value = manifest.description || "";
  $("#pack-authors").value = (manifest.authors || []).join(", ");
  $("#pack-categories").value = (manifest.categories || []).join(", ");
  $("#release-version").value = release.version;
  $("#release-version").disabled = true;
  $("#release-channel").value = release.channel;
  $("#release-channel").disabled = true;
  $("#loader").value = minecraft.loader || "vanilla";
  $("#java-major").value = String(manifest.java?.major || 21);
  $("#memory-mb").value = String(settings.memoryMb || 4096);
  $("#window-width").value = String(settings.windowWidth || 1280);
  $("#window-height").value = String(settings.windowHeight || 720);
  $("#fullscreen").checked = Boolean(settings.fullscreen);
  $("#release-dialog .eyebrow").textContent = "РЕДАКТИРОВАНИЕ ВЕРСИИ";
  $("#release-dialog h2").textContent = "Редактировать сборку";
  $("#release-dialog .dialog-header p").textContent = `Изменения сохранятся в черновике ${release.packId} v${release.version}.`;
  $("#create-release-submit span").textContent = "Сохранить изменения";
  $("#create-release-submit").dataset.originalLabel = "Сохранить изменения";
  setUploadFiles(pendingFiles);
  loadMinecraftVersions(minecraft.version || "", false, minecraft.loaderVersion || "");
}

function togglePackFields() {
  const isNew = $("#pack-choice").value === "new";
  $("#new-pack-fields").hidden = !isNew;
  $("#pack-name").required = isNew;
  $("#pack-id").required = isNew;
}

const refreshLoaderVersionsButton = document.createElement("button");
refreshLoaderVersionsButton.className = "button button-quiet";
refreshLoaderVersionsButton.type = "button";
refreshLoaderVersionsButton.textContent = "Обновить список";
refreshLoaderVersionsButton.addEventListener("click", () => loadLoaderVersions($("#loader-version").value, true));
const loaderVersionTools = document.createElement("div");
loaderVersionTools.className = "field-tools";
loaderVersionTools.append(loaderVersionHint, refreshLoaderVersionsButton);
loaderVersionField.append(loaderVersionTools);

async function loadMinecraftVersions(preferredVersion = "", force = false, preferredLoaderVersion = "") {
  const requestId = ++state.minecraftRequest;
  minecraftVersionSelect.disabled = true;
  refreshMinecraftVersionsButton.disabled = true;
  minecraftVersionSelect.replaceChildren(new Option("Загрузка версий Minecraft…", ""));
  minecraftVersionHint.textContent = "Получаем официальный Mojang manifest…";
  try {
    const result = await api(`/api/admin/options/minecraft?refresh=${force ? "1" : "0"}`);
    if (requestId !== state.minecraftRequest) return;
    const versions = Array.isArray(result.versions) ? result.versions : [];
    minecraftVersionSelect.replaceChildren();
    for (const version of versions) {
      const releaseDate = version.releaseTime ? new Date(version.releaseTime).toLocaleDateString() : "";
      const label = `${version.id} · ${version.type}${releaseDate ? ` · ${releaseDate}` : ""}`;
      minecraftVersionSelect.add(new Option(label, version.id));
    }
    const defaultVersion = preferredVersion || result.latestRelease || versions[0]?.id || "";
    minecraftVersionSelect.value = versions.some((version) => version.id === defaultVersion) ? defaultVersion : versions[0]?.id || "";
    minecraftVersionSelect.disabled = versions.length === 0;
    refreshMinecraftVersionsButton.disabled = false;
    minecraftVersionHint.textContent = versions.length
      ? `Доступно версий: ${versions.length}. Список включает релизы, снапшоты и legacy.`
      : "В официальном manifest нет доступных версий Minecraft.";
    if (minecraftVersionSelect.value) {
      loadLoaderVersions(preferredLoaderVersion);
      loadJavaVersion(minecraftVersionSelect.value);
    }
  } catch (error) {
    if (requestId !== state.minecraftRequest) return;
    minecraftVersionSelect.replaceChildren(new Option("Не удалось загрузить версии", ""));
    minecraftVersionHint.textContent = error.message;
    refreshMinecraftVersionsButton.disabled = false;
  }
}

async function loadLoaderVersions(preferredVersion = "", force = false) {
  const requestId = ++state.loaderRequest;
  const field = $("#loader-version-field");
  const select = $("#loader-version");
  const hint = $("#loader-hint");
  const loader = $("#loader").value;
  const gameVersion = $("#game-version").value.trim();
  const vanilla = loader === "vanilla";

  field.hidden = vanilla;
  refreshLoaderVersionsButton.hidden = vanilla || !gameVersion;
  refreshLoaderVersionsButton.disabled = vanilla || !gameVersion;
  select.required = !vanilla;
  select.disabled = vanilla;
  if (vanilla) {
    select.replaceChildren(new Option("Не требуется", ""));
    hint.textContent = "Для Vanilla версия загрузчика не требуется.";
    return;
  }
  if (!gameVersion) {
    select.disabled = true;
    select.replaceChildren(new Option("Сначала укажите Minecraft", ""));
    hint.textContent = "Укажите версию Minecraft, чтобы загрузить список.";
    return;
  }

  select.disabled = true;
  refreshLoaderVersionsButton.disabled = true;
  select.replaceChildren(new Option("Загружаем версии…", ""));
  hint.textContent = `Ищем версии ${loader} для Minecraft ${gameVersion}…`;
  try {
    const result = await api(`/api/admin/options/loaders?gameVersion=${encodeURIComponent(gameVersion)}&loader=${encodeURIComponent(loader)}&refresh=${force ? "1" : "0"}`);
    if (requestId !== state.loaderRequest) return;
    const versions = Array.isArray(result.versions) ? result.versions : [];
    const metadata = new Map((Array.isArray(result.items) ? result.items : []).map((item) => [item.loaderVersion, item]));
    select.replaceChildren();
    if (!versions.length) {
      select.add(new Option("Нет доступных версий", ""));
      hint.textContent = `Для Minecraft ${gameVersion} не найдены версии ${loader}.`;
      refreshLoaderVersionsButton.disabled = false;
      return;
    }
    for (const version of versions) {
      const item = metadata.get(version);
      const labels = [];
      if (item?.recommended) labels.push("рекомендуемая");
      if (item?.latest) labels.push("последняя");
      if (item?.stable === true) labels.push("stable");
      else if (item?.stable === false) labels.push("preview");
      select.add(new Option(labels.length ? `${version} · ${labels.join(" · ")}` : version, version));
    }
    select.value = versions.includes(preferredVersion) ? preferredVersion : versions[0];
    select.disabled = false;
    refreshLoaderVersionsButton.disabled = false;
    hint.textContent = `Найдено версий: ${versions.length}.`;
  } catch (error) {
    if (requestId !== state.loaderRequest) return;
    select.replaceChildren(new Option("Не удалось загрузить версии", ""));
    refreshLoaderVersionsButton.disabled = false;
    hint.textContent = error.message;
  }
}

async function loadJavaVersion(version) {
  if (!version) return;
  $("#java-hint").textContent = "Проверяем официальные метаданные…";
  try {
    const result = await api(`/api/admin/options/java?gameVersion=${encodeURIComponent(version)}`);
    const major = Number(result.major ?? result.javaMajor ?? result);
    if ([8, 11, 16, 17, 21, 25].includes(major)) $("#java-major").value = String(major);
    $("#java-hint").textContent = `Рекомендована Java ${major}. Можно изменить вручную.`;
  } catch {
    $("#java-hint").textContent = "Автоопределение недоступно. Укажите Java вручную.";
  }
}

function toPackId(value) {
  return value.normalize("NFKD").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 63);
}

function fileCountLabel(count) {
  const lastTwo = count % 100;
  const last = count % 10;
  if (lastTwo >= 11 && lastTwo <= 14) return `${count} файлов`;
  if (last === 1) return `${count} файл`;
  if (last >= 2 && last <= 4) return `${count} файла`;
  return `${count} файлов`;
}

function versionCountLabel(count) {
  const lastTwo = count % 100;
  const last = count % 10;
  if (lastTwo >= 11 && lastTwo <= 14) return `${count} версий`;
  if (last === 1) return `${count} версию`;
  if (last >= 2 && last <= 4) return `${count} версии`;
  return `${count} версий`;
}

function fileRelativePath(file) {
  return file.webkitRelativePath || state.filePaths.get(file) || "";
}

function defaultUploadPath(file, files) {
  if (state.uploadPaths.has(file)) return state.uploadPaths.get(file);
  const relativePath = fileRelativePath(file);
  const destination = $("#upload-folder").value;
  let path;
  if (!relativePath) {
    path = [destination, file.name].filter(Boolean).join("/");
  } else {
    const parts = relativePath.replaceAll("\\", "/").split("/").filter(Boolean);
    const roots = files.map(fileRelativePath).filter(Boolean).map((value) => value.split(/[\\/]/)[0]);
    const sharedRoot = roots.length > 0 && roots.every((root) => root.toLowerCase() === roots[0]?.toLowerCase());
    const folderRoot = state.folderRoots.get(file) || (sharedRoot ? roots[0] : "");
    if (folderRoot && parts[0]?.toLowerCase() === folderRoot.toLowerCase()) parts.shift();
    if (destination && parts[0]?.toLowerCase() === destination.toLowerCase()) parts.shift();
    path = [destination, ...parts].filter(Boolean).join("/") || file.name;
  }
  state.uploadPaths.set(file, path);
  return path;
}

function renderFileQueue() {
  const files = state.uploadFiles;
  const queue = $("#file-queue");
  const expandedFolders = new Set($$(".queued-folder[open]", queue).map((folder) => folder.dataset.folderPath));
  const items = [
    ...state.editingFiles.map((file, index) => ({ kind: "existing", index, file, path: file.path })),
    ...files.map((file, index) => ({ kind: "new", index, file, path: defaultUploadPath(file, files) })),
  ];
  queue.hidden = items.length === 0;
  if (!items.length) {
    queue.replaceChildren();
    return;
  }

  const root = { folders: new Map(), files: [] };
  for (const item of items) {
    const parts = item.path.split("/").filter(Boolean);
    let node = root;
    for (const folder of parts.slice(0, -1)) {
      if (!node.folders.has(folder)) node.folders.set(folder, { folders: new Map(), files: [] });
      node = node.folders.get(folder);
    }
    node.files.push({ ...item, name: parts.at(-1) || item.file.name });
  }

  function summarize(node) {
    const nested = [...node.folders.values()].map(summarize);
    return {
      count: node.files.length + nested.reduce((sum, item) => sum + item.count, 0),
      bytes: node.files.reduce((sum, item) => sum + item.file.size, 0) + nested.reduce((sum, item) => sum + item.bytes, 0),
    };
  }

  function renderFile(item) {
    const existing = item.kind === "existing";
    const rowAttribute = existing ? `data-existing-file-row="${item.index}"` : `data-new-file-row="${item.index}"`;
    const pathAttribute = existing ? "data-existing-path" : "data-new-path";
    const requiredAttribute = existing ? "data-existing-required" : "data-new-required";
    const removeAttribute = existing ? `data-remove-existing="${item.index}"` : `data-remove-file="${item.index}"`;
    const required = existing ? item.file.required : true;
    return `<div class="queued-file ${existing ? "queued-file-existing" : ""}" ${rowAttribute}><span class="queued-file-icon"><svg><use href="/icons.svg#icon-file"></use></svg></span><span class="queued-file-info"><strong>${escapeHtml(item.name)}</strong><small>${formatBytes(item.file.size)}</small></span><label class="queued-path-label"><span>Путь в игре</span><input ${pathAttribute} value="${escapeHtml(item.path)}" aria-label="Путь файла ${escapeHtml(item.path)}" /></label><label class="required-file"><input ${requiredAttribute} type="checkbox" ${required ? "checked" : ""} /><span>Обязательный</span></label><button class="icon-button remove-file" type="button" ${removeAttribute} title="Убрать файл" aria-label="Убрать ${escapeHtml(item.path)}"><svg><use href="/icons.svg#icon-close"></use></svg></button></div>`;
  }

  function renderNode(node, parentPath = "") {
    const folders = [...node.folders.entries()].map(([name, child]) => {
      const summary = summarize(child);
      const path = [parentPath, name].filter(Boolean).join("/");
      return `<details class="queued-folder" data-folder-path="${escapeHtml(path)}"><summary><svg><use href="/icons.svg#icon-folder"></use></svg><strong>${escapeHtml(name)}</strong><span>${fileCountLabel(summary.count)}</span><small>${formatBytes(summary.bytes)}</small></summary><button class="icon-button remove-folder" type="button" data-remove-folder="${escapeHtml(path)}" title="Удалить папку целиком" aria-label="Удалить папку ${escapeHtml(path)}"><svg><use href="/icons.svg#icon-trash"></use></svg></button><div class="queued-folder-content">${renderNode(child, path)}</div></details>`;
    }).join("");
    const filesMarkup = node.files.map(renderFile).join("");
    return folders + filesMarkup;
  }

  const totals = summarize(root);
  const looseFiles = root.files.length ? `<details class="queued-folder" data-folder-path=""><summary><svg><use href="/icons.svg#icon-folder"></use></svg><strong>Корень версии</strong><span>${fileCountLabel(root.files.length)}</span><small>${formatBytes(root.files.reduce((sum, item) => sum + item.file.size, 0))}</small></summary><button class="icon-button remove-folder" type="button" data-remove-folder="" title="Удалить файлы из корня версии" aria-label="Удалить файлы из корня версии"><svg><use href="/icons.svg#icon-trash"></use></svg></button><div class="queued-folder-content">${root.files.map(renderFile).join("")}</div></details>` : "";
  queue.innerHTML = `<div class="queued-summary"><strong>${fileCountLabel(totals.count)}</strong><span>${formatBytes(totals.bytes)}</span><small>Папки раскрываются по нажатию</small></div><div class="queued-tree">${renderNode(root)}${looseFiles}</div>`;
  $$(".queued-folder", queue).forEach((folder) => { folder.open = expandedFolders.has(folder.dataset.folderPath); });
}

function isFileInFolder(filePath, folderPath) {
  const normalizedPath = filePath.replaceAll("\\", "/").replace(/^\/+|\/+$/g, "");
  return folderPath ? normalizedPath === folderPath || normalizedPath.startsWith(`${folderPath}/`) : !normalizedPath.includes("/");
}

function setUploadFiles(files) {
  state.uploadFiles = files;
  const transfer = new DataTransfer();
  for (const file of files) transfer.items.add(file);
  $("#release-files").files = transfer.files;
  renderFileQueue();
}

function mergeUploadFiles(files) {
  const merged = [...state.uploadFiles];
  const known = new Set(merged.map((file) => (fileRelativePath(file) || file.name).toLowerCase()));
  for (const file of files) {
    const path = (fileRelativePath(file) || file.name).toLowerCase();
    if (known.has(path)) continue;
    known.add(path);
    merged.push(file);
  }
  setUploadFiles(merged);
}

function mergeManifestFiles(existingFiles, uploadedFiles) {
  const filesByPath = new Map();
  for (const file of [...existingFiles, ...uploadedFiles]) {
    const path = file.path.trim().replaceAll("\\", "/").replace(/^\/+|\/+$/g, "").toLowerCase();
    filesByPath.set(path, file);
  }
  return [...filesByPath.values()];
}

async function uploadArtifact(file, signal) {
  return api(`/api/admin/files?name=${encodeURIComponent(file.name)}`, {
    method: "POST",
    headers: { "Content-Type": "application/octet-stream" },
    body: file,
    signal,
  });
}

async function createRelease(event) {
  event.preventDefault();
  if (state.busy) return;
  const form = event.currentTarget;
  if (!form.reportValidity()) return;
  const editing = state.editingRelease;
  const loaderVersion = $("#loader-version");
  if ($("#loader").value !== "vanilla" && (!loaderVersion.value || loaderVersion.disabled)) {
    showToast("Выберите доступную версию загрузчика перед сохранением релиза.", "error");
    return;
  }
  const submit = $("#create-release-submit");
  const cancelUpload = $("#cancel-upload");
  const closeButtons = $$('[data-close-dialog]', $("#release-dialog"));
  state.busy = true;
  closeButtons.forEach((button) => { button.disabled = true; });
  setBusy(submit, true, editing ? "Сохраняем изменения…" : "Создаём черновик…");
  const files = [...state.uploadFiles];
  try {
    let pack;
    const selectedPackId = editing ? editing.packId : $("#pack-choice").value;
    const authors = $("#pack-authors").value.split(",").map((item) => item.trim()).filter(Boolean);
    const categories = $("#pack-categories").value.split(",").map((item) => item.trim()).filter(Boolean);
    const packData = {
      id: selectedPackId === "new" ? toPackId($("#pack-id").value) : selectedPackId,
      name: $("#pack-name").value.trim(),
      description: $("#pack-description").value.trim(),
      authors,
      categories,
      iconUrl: editing?.manifest.iconUrl || null,
    };
    if (editing) {
      const currentPack = (state.dashboard?.packs || []).find((item) => item.id === editing.packId);
      const original = currentPack || { name: editing.manifest.name, description: editing.manifest.description, authors: editing.manifest.authors, categories: editing.manifest.categories, iconUrl: editing.manifest.iconUrl };
      pack = { ...packData, iconUrl: original.iconUrl || null };
      const packChanged = pack.name !== (original.name || "")
        || pack.description !== (original.description || "")
        || pack.iconUrl !== (original.iconUrl ?? null)
        || JSON.stringify(pack.authors) !== JSON.stringify(original.authors || [])
        || JSON.stringify(pack.categories) !== JSON.stringify(original.categories || []);
      if (packChanged) {
        await api(`/api/admin/packs/${encodeURIComponent(pack.id)}`, { method: "PUT", body: JSON.stringify(pack) });
        state.editingRelease = await api(`/api/admin/releases/${editing.id}`);
      }
    } else if (selectedPackId === "new") {
      if (packData.id.length < 3) throw new Error("Идентификатор сборки должен содержать не менее 3 символов.");
      await api("/api/admin/packs", { method: "POST", body: JSON.stringify(packData) });
      pack = packData;
    } else {
      pack = (state.dashboard?.packs || []).find((item) => item.id === selectedPackId);
      if (!pack) throw new Error("Выбранная сборка не найдена. Обновите страницу.");
    }
    const uploaded = [];
    const failedUploads = [];
    for (let index = 0; index < files.length; index += 1) {
      setBusy(submit, true, `Загрузка ${index + 1} из ${files.length}…`);
      const row = $(`[data-new-file-row="${index}"]`);
      const pathInput = $(`[data-new-path]`, row);
      const requiredInput = $(`[data-new-required]`, row);
      const uploadPath = pathInput?.value.trim() || defaultUploadPath(files[index], files);
      const controller = new AbortController();
      state.uploadController = controller;
      cancelUpload.hidden = false;
      try {
        const result = await uploadArtifact(files[index], controller.signal);
        uploaded.push({ path: uploadPath, hash: result.hash, size: result.size, url: result.url, required: requiredInput?.checked ?? true });
      } catch (error) {
        if (controller.signal.aborted) throw new Error("Загрузка отменена. Черновик не сохранён.");
        failedUploads.push({ file: files[index], path: uploadPath, message: error.message || "ошибка загрузки" });
      } finally {
        state.uploadController = null;
        cancelUpload.hidden = true;
      }
    }
    setBusy(submit, true, editing ? "Сохраняем изменения…" : "Сохраняем версию…");
    const keptFiles = $$("[data-existing-file-row]", $("#file-queue")).map((row) => {
      const file = state.editingFiles[Number(row.dataset.existingFileRow)];
      return { ...file, path: $("[data-existing-path]", row).value.trim(), required: $("[data-existing-required]", row).checked };
    });
    const gameVersion = $("#game-version").value.trim();
    const loader = $("#loader").value;
    const selectedLoaderVersion = loaderVersion.value.trim() || null;
    const manifest = { manifestVersion: 1, packId: pack.id, version: editing ? editing.version : $("#release-version").value.trim(), name: pack.name || pack.title || pack.id, description: pack.description || "", authors: pack.authors || [], categories: pack.categories || [], iconUrl: pack.iconUrl || null, minecraft: { version: gameVersion, loader, loaderVersion: loader === "vanilla" ? null : selectedLoaderVersion }, java: { major: Number($("#java-major").value) }, files: mergeManifestFiles(keptFiles, uploaded), settings: { memoryMb: Number($("#memory-mb").value), jvmArgs: editing?.manifest.settings?.jvmArgs || "", windowWidth: Number($("#window-width").value), windowHeight: Number($("#window-height").value), fullscreen: $("#fullscreen").checked } };
    let result;
    if (editing) {
      result = await api(`/api/admin/releases/${editing.id}`, { method: "PUT", headers: { "If-Match": state.editingRelease.etag || editing.etag }, body: JSON.stringify(manifest) });
      result.id = editing.id;
    } else {
      result = await api("/api/admin/releases", { method: "POST", body: JSON.stringify({ packId: pack.id, version: manifest.version, channel: $("#release-channel").value, manifest }) });
    }
    if (failedUploads.length) {
      const savedRelease = await api(`/api/admin/releases/${result.id}`);
      openEditDialog(savedRelease, failedUploads.map((failure) => failure.file));
      showToast(`Черновик сохранён. Не загружено файлов: ${failedUploads.length}. Они оставлены в очереди.\n${failedUploads.map(({ path, message }) => `${path}: ${message}`).join("\n")}`, "error");
      return;
    }
    $("#release-dialog").close();
    state.editingRelease = null;
    const savedMessage = editing ? `Изменения ${pack.id} v${manifest.version} сохранены.` : `Черновик ${result.packId} ${result.version} создан.`;
    await refreshDashboard();
    setView("releases");
    showToast(savedMessage);
    openRelease(result.id);
  } catch (error) {
    showToast(error.message, "error");
    $("#login-error").textContent = "";
  } finally {
    state.busy = false;
    state.uploadController = null;
    cancelUpload.hidden = true;
    closeButtons.forEach((button) => { button.disabled = false; });
    setBusy(submit, false);
  }
}

async function openRelease(releaseId) {
  const dialog = $("#detail-dialog");
  const content = $("#detail-content");
  content.innerHTML = `<div class="detail-loading"><span class="spinner"></span>Загружаем версию…</div>`;
  if (!dialog.open) dialog.showModal();
  try {
    const release = await api(`/api/admin/releases/${releaseId}`);
    state.activeRelease = release;
    renderReleaseDetail(release);
    appendUnavailablePublishButton(release);
    appendEditButton(release);
    appendDeleteReleaseButton(release);
    appendDeletePackButton(release);
  } catch (error) {
    content.innerHTML = `<div class="detail-loading">${escapeHtml(error.message)}</div>`;
  }
}

function appendEditButton(release) {
  if (!(["draft", "published"].includes(release.status))) return;
  const actions = $(".detail-footer .dialog-actions", $("#detail-content"));
  if (!actions) return;
  const button = document.createElement("button");
  button.className = "button button-quiet";
  button.type = "button";
  button.dataset.editRelease = String(release.id);
  button.innerHTML = `<svg><use href="/icons.svg#icon-edit"></use></svg>Редактировать`;
  actions.prepend(button);
}

function appendDeletePackButton(release) {
  const actions = $(".detail-footer .dialog-actions", $("#detail-content"));
  if (!actions || actions.querySelector("[data-delete-pack]")) return;
  const button = document.createElement("button");
  button.className = "button button-danger";
  button.type = "button";
  button.dataset.deletePack = release.packId;
  button.innerHTML = `<svg><use href="/icons.svg#icon-trash"></use></svg>Удалить сборку`;
  actions.append(button);
}

function appendDeleteReleaseButton(release) {
  if (release.immutable || release.status === "published") return;
  const actions = $(".detail-footer .dialog-actions", $("#detail-content"));
  if (!actions || actions.querySelector("[data-delete-release]")) return;
  const button = document.createElement("button");
  button.className = "button button-danger";
  button.type = "button";
  button.dataset.deleteRelease = String(release.id);
  button.innerHTML = `<svg><use href="/icons.svg#icon-trash"></use></svg>Удалить версию`;
  actions.append(button);
}

function appendUnavailablePublishButton(release, validation = null) {
  if (release.status !== "draft" || release.immutable || validation?.ready) return;
  const actions = $(".detail-footer .dialog-actions", $("#detail-content"));
  if (!actions || actions.querySelector("[data-publish]")) return;
  const button = document.createElement("button");
  button.className = "button button-primary";
  button.type = "button";
  button.dataset.publish = String(release.id);
  button.disabled = true;
  button.title = "Опубликовать можно после успешной проверки версии.";
  button.innerHTML = `<svg><use href="/icons.svg#icon-globe"></use></svg>Опубликовать`;
  actions.append(button);
}

async function deleteRelease(releaseId) {
  const release = state.activeRelease;
  if (!release || String(release.id) !== String(releaseId)) return;
  const confirmed = await confirmAction({
    eyebrow: "УДАЛЕНИЕ ВЕРСИИ",
    title: `Удалить ${release.packId} v${release.version}?`,
    message: "Версия будет удалена без возможности восстановления.",
    confirmLabel: "Удалить версию",
    danger: true,
  });
  if (!confirmed) return;
  try {
    await api(`/api/admin/releases/${release.id}`, { method: "DELETE" });
    $("#detail-dialog").close();
    await refreshDashboard();
    showToast(`Версия ${release.packId} v${release.version} удалена.`);
  } catch (error) {
    showToast(error.message, "error");
  }
}

async function deletePack(packId) {
  let deletedDrafts = 0;
  try {
    const firstPage = await api("/api/admin/dashboard?page=1&perPage=200");
    const releases = [...(firstPage.releases || [])];
    const pages = firstPage.releasePagination?.pages || 1;
    for (let page = 2; page <= pages; page += 1) {
      const result = await api(`/api/admin/dashboard?page=${page}&perPage=200`);
      releases.push(...(result.releases || []));
    }
    const packReleases = releases.filter((release) => release.packId === packId);
    if (packReleases.some((release) => release.immutable || release.status === "published")) {
      showToast("Нельзя удалить сборку: в ней есть опубликованные версии, которые нельзя удалить.", "error");
      return;
    }
    const pack = (firstPage.packs || []).find((item) => item.id === packId);
    const title = pack?.name || packId;
    const versionsNote = packReleases.length ? ` и ${versionCountLabel(packReleases.length)}` : "";
    const confirmed = await confirmAction({
      eyebrow: "УДАЛЕНИЕ СБОРКИ",
      title: `Удалить «${title}»?`,
      message: `Будут удалены сборка${versionsNote}. Версии нельзя будет восстановить.`,
      confirmLabel: "Удалить сборку",
      danger: true,
    });
    if (!confirmed) return;

    for (const release of packReleases) {
      await api(`/api/admin/releases/${release.id}`, { method: "DELETE" });
      deletedDrafts += 1;
    }
    await api(`/api/admin/packs/${encodeURIComponent(packId)}`, { method: "DELETE" });
    $("#detail-dialog").close();
    await refreshDashboard();
    setView("releases");
    showToast(`Сборка «${title}» удалена.`);
  } catch (error) {
    if (deletedDrafts) $("#detail-dialog").close();
    await refreshDashboard();
    const message = deletedDrafts
      ? `Удалено версий: ${deletedDrafts}. Не удалось удалить сборку: ${error.message}`
      : error.message;
    showToast(message, "error");
  }
}

function nextDraftVersion(release) {
  const match = /^(\d+)\.(\d+)\.(\d+)(?:[-+][0-9A-Za-z.-]+)?$/.exec(release.version);
  if (!match) throw new Error("Не удалось автоматически определить следующую SemVer-версию.");
  const usedVersions = new Set((state.dashboard?.releases || []).filter((item) => item.packId === release.packId).map((item) => item.version));
  let patch = Number(match[3]);
  let version;
  do {
    patch += 1;
    version = `${match[1]}.${match[2]}.${patch}`;
  } while (usedVersions.has(version));
  return version;
}

async function startEditingRelease() {
  const release = state.activeRelease;
  if (!release) return;
  if (release.status === "draft" && !release.immutable) {
    $("#detail-dialog").close();
    openEditDialog(release);
    return;
  }
  try {
    const version = nextDraftVersion(release);
    const clone = await api(`/api/admin/releases/${release.id}/clone`, {
      method: "POST",
      body: JSON.stringify({ version, channel: release.channel }),
    });
    const draft = await api(`/api/admin/releases/${clone.id}`);
    $("#detail-dialog").close();
    await refreshDashboard();
    setView("releases");
    showToast(`Создан черновик v${version}. Опубликованная версия сохранена без изменений.`);
    openEditDialog(draft);
  } catch (error) {
    showToast(error.message, "error");
  }
}

function renderReleaseDetail(release, validation = null) {
  const manifest = release.manifest;
  const published = release.status === "published";
  const immutable = release.immutable;
  const errors = validation?.validationErrors || [];
  const errorList = errors.length ? `<div class="validation-errors"><strong>Нужно исправить перед публикацией</strong><ul>${errors.map((error) => `<li>${escapeHtml(typeof error === "string" ? error : error.message || JSON.stringify(error))}</li>`).join("")}</ul></div>` : "";
  const fileList = (manifest.files || []).map((file) => `<li><span><svg><use href="/icons.svg#icon-file"></use></svg>${escapeHtml(file.path)}</span><small>${formatBytes(file.size)}</small></li>`).join("") || `<li class="no-files">Файлы ещё не добавлены</li>`;
  const banner = validation ? `<div class="validation-banner ${validation.ready ? "validation-success" : "validation-warning"}"><svg><use href="/icons.svg#${validation.ready ? "icon-check" : "icon-alert"}"></use></svg><span><strong>${validation.ready ? "Версия готова к публикации" : "Проверка нашла замечания"}</strong><small>${validation.ready ? "Манифест и файлы проверены сервером." : "Исправьте пункты ниже и запустите проверку снова."}</small></span></div>` : "";
  $("#detail-content").innerHTML = `<div class="dialog-header detail-header"><div><span class="eyebrow">${published ? "ОПУБЛИКОВАННАЯ ВЕРСИЯ" : "ЧЕРНОВИК РЕЛИЗА"}</span><h2>${escapeHtml(manifest.name || release.packId)}</h2><p>${escapeHtml(release.packId)} <span class="detail-separator">/</span> v${escapeHtml(release.version)}</p></div><button class="icon-button dialog-close" type="button" data-close-detail aria-label="Закрыть"><svg><use href="/icons.svg#icon-close"></use></svg></button></div><div class="detail-body">${banner}<div class="detail-facts"><div><span>ИГРА</span><strong>${escapeHtml(manifest.minecraft.version)}</strong></div><div><span>ЗАГРУЗЧИК</span><strong>${escapeHtml(manifest.minecraft.loader)}${manifest.minecraft.loaderVersion ? ` ${escapeHtml(manifest.minecraft.loaderVersion)}` : ""}</strong></div><div><span>КАНАЛ</span><strong>${release.channel === "beta" ? "Beta" : "Stable"}</strong></div><div><span>JAVA</span><strong>${escapeHtml(manifest.java.major)}</strong></div></div><div class="detail-block"><div class="detail-block-heading"><h3>Состав сборки</h3><span>${(manifest.files || []).length} файлов</span></div><ul class="detail-file-list">${fileList}</ul></div>${errorList}<div class="detail-block"><div class="detail-block-heading"><h3>Описание</h3></div><p class="detail-description">${escapeHtml(manifest.description || "Описание не добавлено.")}</p></div><div class="detail-id">Релиз #${release.id} <span>·</span> SHA-256 каждого файла проверяется сервером</div></div><div class="dialog-footer detail-footer"><span class="dialog-note">${published ? `<svg><use href="/icons.svg#icon-lock"></use></svg>Опубликованный манифест неизменяем.` : `<svg><use href="/icons.svg#icon-shield"></use></svg>Проверка готовности действует 15 минут.`}</span><div class="dialog-actions">${published ? `<button class="button button-quiet" data-unpublish="${release.id}">Снять с публикации</button>` : `${immutable ? "" : `<button class="button button-quiet" data-validate="${release.id}"><svg><use href="/icons.svg#icon-check"></use></svg>Проверить версию</button>`}${validation?.ready ? `<button class="button button-primary" data-publish="${release.id}"><svg><use href="/icons.svg#icon-globe"></use></svg>Опубликовать</button>` : ""}`}</div></div>`;
}

async function validateRelease(releaseId, button) {
  button.disabled = true;
  button.classList.add("is-busy");
  try {
    const result = await api(`/api/admin/releases/${releaseId}/validate`, { method: "POST", body: "{}" });
    const release = await api(`/api/admin/releases/${releaseId}`);
    state.activeRelease = release;
    renderReleaseDetail(release, result);
    appendUnavailablePublishButton(release, result);
    appendEditButton(release);
    appendDeleteReleaseButton(release);
    appendDeletePackButton(release);
    await refreshDashboard();
    showToast(result.ready ? "Проверка пройдена. Версия готова к публикации." : "Проверка завершена. Посмотрите замечания.", result.ready ? "success" : "error");
  } catch (error) {
    showToast(error.message, "error");
    button.disabled = false;
    button.classList.remove("is-busy");
  }
}

async function publishRelease(releaseId, publish) {
  const action = publish ? "publish" : "unpublish";
  const message = publish
    ? "Версия станет доступна игрокам. После публикации манифест будет неизменяемым."
    : "Версия станет недоступна игрокам и вернётся в черновики. После этого её можно будет редактировать или удалить.";
  const confirmed = await confirmAction({
    eyebrow: publish ? "ПУБЛИКАЦИЯ ВЕРСИИ" : "ИЗМЕНЕНИЕ ПУБЛИКАЦИИ",
    title: publish ? "Опубликовать версию?" : "Снять версию с публикации?",
    message,
    confirmLabel: publish ? "Опубликовать" : "Снять с публикации",
    danger: !publish,
  });
  if (!confirmed) return;
  try {
    await api(`/api/admin/releases/${releaseId}/${action}`, { method: "POST", body: "{}" });
    $("#detail-dialog").close();
    await refreshDashboard();
    showToast(publish ? "Сборка опубликована." : "Версия снята с публикации.");
  } catch (error) {
    showToast(error.message, "error");
  }
}

$("#login-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const button = $(".login-submit", event.currentTarget);
  $("#login-error").textContent = "";
  setBusy(button, true, "Проверяем пароль…");
  try {
    const result = await api("/api/admin/login", { method: "POST", body: JSON.stringify({ password: $("#login-password").value }) });
    state.csrf = result.csrfToken;
    $("#login-password").value = "";
    showApp();
  } catch (error) {
    $("#login-error").textContent = error.message;
  } finally {
    setBusy(button, false);
  }
});

$(".password-toggle").addEventListener("click", (event) => {
  const input = $("#login-password");
  input.type = input.type === "password" ? "text" : "password";
  event.currentTarget.setAttribute("aria-label", input.type === "password" ? "Показать пароль" : "Скрыть пароль");
});

$("#logout-button").addEventListener("click", async () => {
  try { await api("/api/admin/logout", { method: "POST", body: "{}" }); } catch { /* The local session is cleared below. */ }
  state.csrf = "";
  showLogin("");
});

$("#refresh-button").addEventListener("click", refreshDashboard);
$("#new-release-button").addEventListener("click", openCreateDialog);
$("#theme-toggle").addEventListener("click", () => {
  const theme = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
  setTheme(theme);
  try { localStorage.setItem("mc-launcher-admin-theme", theme); } catch { /* The theme still applies for this session. */ }
});
$("#release-form").addEventListener("submit", createRelease);
$("#cancel-upload").addEventListener("click", () => state.uploadController?.abort());
$("#upload-folder").addEventListener("change", renderFileQueue);
$("#file-queue").addEventListener("input", (event) => {
  const pathInput = event.target.closest("[data-new-path]");
  if (!pathInput) return;
  const row = pathInput.closest("[data-new-file-row]");
  const file = state.uploadFiles[Number(row?.dataset.newFileRow)];
  if (file) state.uploadPaths.set(file, pathInput.value);
});
$("#pack-choice").addEventListener("change", togglePackFields);
$("#pack-name").addEventListener("input", (event) => { if (!$("#pack-id").dataset.edited) $("#pack-id").value = toPackId(event.target.value); });
$("#pack-id").addEventListener("input", (event) => { event.target.dataset.edited = "true"; event.target.value = toPackId(event.target.value); });
$("#pack-id").addEventListener("focus", (event) => { event.target.dataset.edited = "true"; });
$("#release-files").addEventListener("change", (event) => mergeUploadFiles([...event.currentTarget.files]));
folderPicker.addEventListener("click", () => folderInput.click());
folderInput.addEventListener("change", () => {
  const files = [...folderInput.files];
  for (const file of files) {
    const path = file.webkitRelativePath || file.name;
    state.filePaths.set(file, path);
    state.folderRoots.set(file, path.split(/[\\/]/)[0]);
  }
  mergeUploadFiles(files);
  folderInput.value = "";
});
$("#file-queue").addEventListener("click", (event) => {
  const removeFolder = event.target.closest("[data-remove-folder]");
  if (removeFolder) {
    const folderPath = removeFolder.dataset.removeFolder;
    state.editingFiles = state.editingFiles.filter((file) => !isFileInFolder(file.path, folderPath));
    const retainedFiles = state.uploadFiles.filter((file) => !isFileInFolder(defaultUploadPath(file, state.uploadFiles), folderPath));
    setUploadFiles(retainedFiles);
    return;
  }
  const removeExisting = event.target.closest("[data-remove-existing]");
  if (removeExisting) {
    state.editingFiles.splice(Number(removeExisting.dataset.removeExisting), 1);
    renderFileQueue();
    return;
  }
  const remove = event.target.closest("[data-remove-file]");
  if (!remove) return;
  const index = Number(remove.dataset.removeFile);
  state.uploadFiles.splice(index, 1);
  setUploadFiles(state.uploadFiles);
});
$("#upload-destination").addEventListener("dragover", (event) => { event.preventDefault(); event.currentTarget.classList.add("is-dragging"); });
$("#upload-destination").addEventListener("dragleave", (event) => event.currentTarget.classList.remove("is-dragging"));
$("#upload-destination").addEventListener("drop", (event) => { event.preventDefault(); event.currentTarget.classList.remove("is-dragging"); mergeUploadFiles([...event.dataTransfer.files]); });
$("#loader").addEventListener("change", () => loadLoaderVersions());
$("#game-version").addEventListener("change", async (event) => {
  const version = event.target.value.trim();
  loadLoaderVersions();
  await loadJavaVersion(version);
});

document.addEventListener("click", (event) => {
  const nav = event.target.closest("[data-view]");
  if (nav) { setView(nav.dataset.view); return; }
  if (event.target.closest("[data-action='new-release']")) { openCreateDialog(); return; }
  const close = event.target.closest("[data-close-dialog]");
  if (close) { close.closest("dialog").close(); return; }
  if (event.target.closest("[data-close-detail]")) { $("#detail-dialog").close(); return; }
  const validate = event.target.closest("[data-validate]");
  if (validate) { validateRelease(validate.dataset.validate, validate); return; }
  const publish = event.target.closest("[data-publish]");
  if (publish) { publishRelease(publish.dataset.publish, true); return; }
  const unpublish = event.target.closest("[data-unpublish]");
  if (unpublish) { publishRelease(unpublish.dataset.unpublish, false); return; }
  if (event.target.closest("[data-edit-release]")) { startEditingRelease(); return; }
  const deleteReleaseButton = event.target.closest("[data-delete-release]");
  if (deleteReleaseButton) { deleteRelease(deleteReleaseButton.dataset.deleteRelease); return; }
  const deletePackButton = event.target.closest("[data-delete-pack]");
  if (deletePackButton) { deletePack(deletePackButton.dataset.deletePack); return; }
  const open = event.target.closest("[data-open-release]") || event.target.closest("[data-release-id]");
  if (open) openRelease(open.dataset.openRelease || open.dataset.releaseId);
});

document.addEventListener("keydown", (event) => {
  if (event.key === "Enter" && event.target.matches("[data-release-id]")) openRelease(event.target.dataset.releaseId);
  if (event.key === "Escape") $$("dialog[open]").forEach((dialog) => {
    if (dialog.id !== "release-dialog" || !state.busy) dialog.close();
  });
});

$("#release-search").addEventListener("input", (event) => { state.search = event.target.value; renderAllReleases(); });
$$('.filter-tab').forEach((button) => button.addEventListener("click", () => { state.filter = button.dataset.filter; $$('.filter-tab').forEach((tab) => tab.classList.toggle("is-selected", tab === button)); renderAllReleases(); }));

const initialView = ["overview", "releases", "activity"].includes(location.hash.slice(1)) ? location.hash.slice(1) : "overview";
setView(initialView);
bootstrap();