import { api, connect, editTags, getTarget, initBundle, selectWorkspace, post, purge, setTarget, submitCurrentDirectory, switchAccount } from "./api.js";
import { empty, renderDetail, renderHeading, renderList } from "./view.js";
import { $, action, busy, closeDialog, create, getDialogVersion, icon, report, show } from "./widgets.js";

const state = {
  view: "files", folder: "/", cursor: null, selectedFile: null, server: null, workspace: "",
  workspaceName: null,
  workspaceBundle: null, bundleWarnings: [], search: false, listRequest: 0, detailRequest: 0,
};
const app = { state, select, setConnection, load, detail, closeDetail };
let filterTimer;

function setConnection(info) {
  const changed = setTarget(info.target);
  if (changed) state.bundleWarnings = [];
  state.server = info.server;
  state.workspace = info.workspace;
  state.workspaceName = info.workspace_name;
  const server = $("server");
  server.textContent = info.server || "尚未连接";
  server.title = server.textContent;
  $("signal").classList.toggle("online", Boolean(info.server));
  const account = $("account");
  account.replaceChildren(icon(info.server ? "disconnect" : "connect"));
  account.title = info.server ? "断开连接" : "连接服务器";
  account.setAttribute("aria-label", account.title);
  renderFileActions();
  renderPath();
  return changed;
}

function renderPath() {
  const bar = $("crumbs");
  bar.replaceChildren();
  const view = state.view;
  if (view !== "workspace" && view !== "files") return;
  const local = view === "workspace";
  const current = state.folder;
  const add = (text, path, title = text) => {
    const node = action(text, () => select(view, path), { variant: "ghost crumb" });
    node.replaceChildren(create("span", text, "ellipsis"));
    node.title = title;
    if (path === current) node.setAttribute("aria-current", "location");
    bar.append(node);
  };
  add(local ? state.workspaceName || "根目录" : "根目录", local ? "." : "/", local ? state.workspace || "根目录" : "根目录");
  let path = "";
  for (const part of current.split("/").filter(part => part && part !== ".")) {
    path += "/" + part;
    bar.append(icon("chevron"));
    add(part, local ? path.slice(1) : path);
  }
}

function select(view, folder) {
  clearTimeout(filterTimer);
  state.view = view;
  state.folder = folder ?? (view === "workspace" ? "." : "/");
  state.workspaceBundle = null;
  $("filters").reset();
  state.search = false;
  $("purge-all").hidden = true;
  $("rows").replaceChildren();
  closeDetail();
  $("count").textContent = "";
  $("empty").hidden = true;
  document.querySelector(".workspace").scrollTop = 0;
  renderHeading(view);
  renderPath();
  return load();
}

async function load(append = false) {
  const cursor = append ? state.cursor : null;
  if (append && !cursor) return;
  if (!append) resetPagination();
  const id = ++state.listRequest;
  if (state.view !== "workspace" && !state.server) {
    $("table").removeAttribute("aria-busy");
    return empty("连接 ZXS 服务器后即可查看文件。", "connect");
  }
  const query = new FormData($("filters"));
  $("table").setAttribute("aria-busy", "true");
  try {
    const page = await loadView(state.view, state.folder, cursor, query);
    if (id !== state.listRequest) return;
    state.search = state.view === "files" && Boolean(query.get("q").trim() || query.get("tag"));
    state.workspaceBundle = state.view === "workspace" && page.mode === "bundle" ? page : null;
    renderFileActions();
    state.cursor = state.view === "workspace" ? null : page.next_cursor;
    renderList(app, page, append);
    $("purge-all").hidden = state.view !== "recycle" || !$("rows").rows.length;
    markSelection();
    return true;
  } catch (error) {
    if (id !== state.listRequest) return;
    if (error.status === 401 && state.view !== "workspace") {
      const server = state.server;
      const session = await api("session").catch(() => null);
      if (id !== state.listRequest) return;
      if (session && setConnection(session)) {
        await select(state.view);
        if (session.server) return;
      }
      $("signal").classList.remove("online");
      if (!$("form-dialog").open) connect(app, server);
    } else {
      report(error.message);
      if (error.status === 404 && state.view === "files" && state.folder !== "/") return select("files");
    }
  } finally {
    if (id === state.listRequest) $("table").removeAttribute("aria-busy");
  }
}

async function refresh(checkSession = true) {
  if (checkSession && setConnection(await api("session"))) return select(state.view);
  const { selectedFile, detailRequest } = state;
  if (await load() && selectedFile && detailRequest === state.detailRequest) {
    await detail(selectedFile.container_id);
  }
}

function resetPagination() {
  state.cursor = null;
  $("next").hidden = true;
}

function renderFileActions() {
  const files = state.view === "files";
  const inDisk = files && state.folder !== "/" && !state.search;
  const workspace = state.view === "workspace";
  const sync = $("sync");
  sync.hidden = !state.server || !(workspace || inDisk);
  sync.querySelector("span").textContent = workspace ? "同步已追踪" : "同步此目录";
  $("workspace-location").hidden = !workspace;
  const submit = $("submit-local");
  submit.hidden = !state.server || !workspace || state.folder === ".";
  submit.querySelector("span").textContent = state.workspaceBundle ? "提交目录包" : "提交当前目录";
  $("init-bundle").hidden = !state.server || !workspace || Boolean(state.workspaceBundle);
  const tags = $("edit-tags");
  tags.hidden = !state.server || !files;
  tags.disabled = !state.selectedFile;
  tags.title = state.selectedFile ? "添加标签" : "选择文件后添加标签";
}

async function syncLocal() {
  const origin = { target: getTarget(), dialog: getDialogVersion() };
  const folder = state.view === "files" ? state.folder : null;
  const result = await api("sync", { folder }).catch(error => { if (origin.target === getTarget()) throw error; });
  if (!result || origin.target !== getTarget()) return;
  state.bundleWarnings = result.bundle_warnings;
  await refresh(false);
  if (origin.target !== getTarget()) return;
  const counts = [];
  if (result.updated) counts.push("已同步 " + result.updated + " 项到本地工作区");
  if (result.local_changes) counts.push("已保留 " + result.local_changes + " 项本地修改");
  if (result.remote_missing.length) counts.push(result.remote_missing.length + " 项远端不可用");
  if (result.issues.length) return report([...counts, ...result.issues.map(issue => issue.path + " — " + issue.reason)].join("\n"), origin.dialog);
  show((counts.join("，") || (folder ? "目录已同步" : "没有需要同步的项目")) + "。");
}

function loadView(view, folder, cursor, query) {
  if (view === "workspace") {
    return api("browse_workspace", { directory: folder });
  }
  if (view === "recycle") return api("recycle_list", { filter: { cursor, limit: 100 } });
  const q = query.get("q").trim() || null;
  const tag = query.get("tag") || null;
  return api("list", { filter: {
    folder: q || tag ? null : folder, cursor, limit: 100, q, tag,
  } });
}

async function detail(id, focus = false) {
  if (state.selectedFile && state.selectedFile.container_id !== id) closeDetail();
  const request = ++state.detailRequest;
  let data;
  try {
    data = await api("detail", { id });
  } catch (error) {
    if (request !== state.detailRequest || state.view !== "files") return;
    if (error.status !== 404 || state.selectedFile?.container_id !== id) throw error;
    closeDetail();
    return;
  }
  if (request !== state.detailRequest || state.view !== "files") return;
  state.selectedFile = data.container;
  renderDetail(app, data);
  renderFileActions();
  markSelection();
  if (focus) $("detail-name").focus({ preventScroll: true });
}

function closeDetail(restoreFocus = false) {
  const id = state.selectedFile?.container_id;
  ++state.detailRequest;
  state.selectedFile = null;
  $("details").hidden = true;
  $("inspector-backdrop").hidden = true;
  $("detail-content").replaceChildren();
  renderFileActions();
  markSelection();
  if (restoreFocus && id) {
    const row = Array.from($("rows").rows).find(row => row.dataset.rowKey === id);
    row?.querySelector(".file-link").focus({ preventScroll: true });
  }
}

function markSelection() {
  const id = state.selectedFile?.container_id;
  for (const row of $("rows").rows) {
    const current = row.dataset.rowKey === id;
    row.dataset.selected = current;
    const link = row.querySelector(".file-link");
    if (current) link.setAttribute("aria-current", "true");
    else link.removeAttribute("aria-current");
  }
}

function moveFocus(event) {
  const buttons = Array.from($("rows").querySelectorAll(".file-link"));
  if (!buttons.length) return;
  event.preventDefault();
  const focused = buttons.indexOf(document.activeElement);
  const current = focused >= 0 ? focused : buttons.findIndex(node => node.getAttribute("aria-current") === "true");
  const step = event.key === "ArrowDown" ? 1 : -1;
  const index = current < 0 ? (step > 0 ? 0 : buttons.length - 1)
    : Math.max(0, Math.min(buttons.length - 1, current + step));
  buttons[index].focus();
  buttons[index].scrollIntoView({ block: "nearest" });
}

function listKeys(event) {
  if ($("form-dialog").open || event.altKey || event.metaKey || event.ctrlKey) return;
  const inField = event.target instanceof Element && event.target.closest("input, textarea, select, [contenteditable]");
  if (event.key === "Escape") {
    if (state.selectedFile) { event.preventDefault(); closeDetail(true); return; }
    if (state.view === "files" && ($("search").value || $("filters").elements.tag.value)) {
      event.preventDefault();
      clearTimeout(filterTimer);
      $("filters").reset();
      load();
    }
    return;
  }
  if (inField) return;
  if (event.key === "/") {
    event.preventDefault();
    if (state.view !== "files") select("files");
    $("search").focus();
  } else if (event.key === "Backspace" && state.view !== "recycle") {
    const root = state.view === "workspace" ? "." : "/";
    if (state.folder === root) return;
    const slash = state.folder.lastIndexOf("/");
    event.preventDefault();
    select(state.view, slash > 0 ? state.folder.slice(0, slash) : root);
  } else if (["ArrowUp", "ArrowDown"].includes(event.key) && !$("details").contains(event.target)) {
    moveFocus(event);
  }
}

for (const node of document.querySelectorAll("[data-icon]")) node.prepend(icon(node.dataset.icon));
for (const nav of document.querySelectorAll("button[data-view]")) nav.addEventListener("click", () => select(nav.dataset.view));
$("filters").addEventListener("submit", event => {
  event.preventDefault();
  clearTimeout(filterTimer);
  load();
});
$("filters").addEventListener("input", () => {
  ++state.listRequest;
  resetPagination();
  clearTimeout(filterTimer);
  filterTimer = setTimeout(() => { if (state.view === "files") load(); }, 180);
});
$("refresh").addEventListener("click", event => busy(event.currentTarget, refresh));
$("sync").addEventListener("click", event => busy(event.currentTarget, syncLocal));
$("next").addEventListener("click", event => busy(event.currentTarget, () => load(true)));
$("close-detail").addEventListener("click", () => closeDetail(true));
$("inspector-backdrop").addEventListener("click", () => closeDetail(true));
$("dialog-close").addEventListener("click", closeDialog);
$("dialog-cancel").addEventListener("click", closeDialog);
$("form-dialog").addEventListener("cancel", event => { event.preventDefault(); closeDialog(); });
$("submit-local").addEventListener("click", event => busy(event.currentTarget, () => submitCurrentDirectory(app)));
$("workspace-location").addEventListener("click", () => selectWorkspace(app));
$("init-bundle").addEventListener("click", event => busy(event.currentTarget, () => initBundle(app)));
for (const id of ["edit-tags", "detail-edit-tags"]) {
  $(id).addEventListener("click", () => {
    if (state.selectedFile) editTags(app, state.selectedFile);
  });
}
$("purge-all").addEventListener("click", () => purge(app));
$("account").addEventListener("click", () => switchAccount(app));
document.addEventListener("keydown", listKeys);

renderHeading(state.view);
renderPath();
try {
  const secret = location.hash.slice(1);
  if (secret) {
    history.replaceState(null, "", "/");
    await post("/access", { token: secret });
  }
  setConnection(await api("session"));
  if (!state.server) connect(app);
  await load();
} catch (error) { report(error.message); }
