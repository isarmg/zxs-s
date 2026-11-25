import { api, commitForm, connect, createFolder, editTags, initBundle, post, purge, switchAccount } from "./api.js";
import { empty, renderDetail, renderHeading, renderList } from "./view.js";
import { $, action, busy, create, icon, report, show } from "./widgets.js";

const state = {
  view: "files", folder: "/", cursor: null, selectedFile: null, server: null, workspace: "",
  workspaceFolder: ".", workspaceBundle: null, bundleWarnings: [], search: false, listRequest: 0, detailRequest: 0,
};
const app = { state, select, setConnection, load, detail, closeDetail, syncLocal, refreshLocal };
let filterTimer;
let workspaceSync;

function setConnection(info) {
  state.server = info.server;
  state.workspace = info.workspace;
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
}

function renderPath() {
  const bar = $("crumbs");
  bar.replaceChildren();
  const view = state.view;
  if (view !== "workspace" && view !== "files") return;
  const local = view === "workspace";
  const current = local ? state.workspaceFolder : state.folder;
  const add = (text, path, title = text) => {
    const node = action(text, () => select(view, path), { variant: "ghost crumb" });
    node.replaceChildren(create("span", text, "ellipsis"));
    node.title = title;
    if (path === current) node.setAttribute("aria-current", "location");
    bar.append(node);
  };
  add("根目录", local ? "." : "/", local ? state.workspace || "根目录" : "根目录");
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
  state.folder = view === "workspace" ? "/" : folder ?? "/";
  state.workspaceFolder = view === "workspace" ? folder ?? "." : ".";
  state.workspaceBundle = null;
  state.cursor = null;
  $("filters").reset();
  state.search = false;
  $("purge-all").hidden = true;
  closeDetail();
  $("rows").replaceChildren();
  $("count").textContent = "";
  $("next").hidden = true;
  $("empty").hidden = true;
  document.querySelector(".workspace").scrollTop = 0;
  renderHeading(view);
  renderPath();
  return load();
}

async function load(append = false) {
  const id = ++state.listRequest;
  const cursor = append ? state.cursor : null;
  const query = new FormData($("filters"));
  $("table").setAttribute("aria-busy", "true");
  try {
    const folder = state.view === "workspace" ? state.workspaceFolder : state.folder;
    const page = await loadView(state.view, folder, cursor, query);
    if (id !== state.listRequest) return;
    state.search = state.view === "files" && ["q", "tag"].some(field => query.get(field).trim());
    state.workspaceBundle = state.view === "workspace" && page.mode === "bundle" ? page.root : null;
    renderFileActions();
    state.cursor = state.view === "workspace" ? null : page.next_cursor;
    renderList(app, page, append);
    $("purge-all").hidden = state.view !== "recycle" || !page.entries.length;
    markSelection();
  } catch (error) {
    if (id !== state.listRequest) return;
    if (error.status === 401 && state.view !== "workspace") {
      $("rows").replaceChildren();
      $("count").textContent = "0 项";
      $("next").hidden = true;
      empty("连接 ZXS 服务器后即可查看文件。", "connect");
      $("signal").classList.remove("online");
      if (!$("form-dialog").open) connect(app);
    } else { report(error.message); }
  } finally {
    if (id === state.listRequest) $("table").removeAttribute("aria-busy");
  }
}

function renderFileActions() {
  const files = state.view === "files";
  const inDisk = files && state.folder !== "/" && !state.search;
  const workspace = state.view === "workspace";
  $("create-folder").hidden = !state.server || !inDisk;
  const submit = $("submit-local");
  submit.hidden = !state.server || !(inDisk || workspace);
  submit.querySelector("span").textContent = state.workspaceBundle ? "提交目录包" : "提交本地";
  submit.setAttribute("aria-label", state.workspaceBundle ? "提交目录包" : "提交本地文件");
  $("init-bundle").hidden = !state.server || !workspace || Boolean(state.workspaceBundle);
  const tags = $("edit-tags");
  tags.hidden = !state.server || !files;
  tags.disabled = !state.selectedFile;
  tags.title = state.selectedFile ? "添加标签" : "选择文件后添加标签";
}

function syncLocal() {
  workspaceSync ??= api("sync").then(result => {
    state.bundleWarnings = result.bundle_warnings;
    if (result.issues.length) {
      report(result.issues.map(issue => issue.path + " — " + issue.reason).join("；"));
    } else if (result.updated) {
      show("已同步 " + result.updated + " 项到本地工作区。");
    }
  }).finally(() => { workspaceSync = null; });
  return workspaceSync;
}

async function refreshLocal() {
  try {
    const result = await api("refresh");
    if (result.issues.length) report(result.issues.map(issue => issue.path + " — " + issue.reason).join("；"));
  } catch (error) { report(error.message); }
}

async function loadView(view, folder, cursor, query) {
  if (view === "workspace") {
    if (workspaceSync) await workspaceSync;
    return api("browse_workspace", { directory: folder });
  }
  if (view === "recycle") return api("recycle_list", { filter: { cursor, limit: 100 } });
  const q = query.get("q").trim() || null;
  const tag = query.get("tag").trim() || null;
  return api("list", { filter: {
    folder: q || tag ? null : folder, cursor, limit: 100, q, tag,
  } });
}

async function detail(id, focus = false) {
  const request = ++state.detailRequest;
  const data = await api("detail", { id });
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
  } else if (event.key === "Backspace" && state.view === "files" && state.folder !== "/") {
    event.preventDefault();
    select("files", state.folder.slice(0, state.folder.lastIndexOf("/")) || "/");
  } else if (event.key === "Backspace" && state.view === "workspace" && state.workspaceFolder !== ".") {
    event.preventDefault();
    const parent = state.workspaceFolder.split("/").slice(0, -1).join("/");
    select("workspace", parent || ".");
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
  clearTimeout(filterTimer);
  filterTimer = setTimeout(() => { if (state.view === "files") load(); }, 180);
});
$("refresh").addEventListener("click", event => busy(event.currentTarget, async () => {
  if (state.view === "workspace" && state.server) await syncLocal();
  await load();
}));
$("next").addEventListener("click", event => busy(event.currentTarget, () => load(true)));
$("close-detail").addEventListener("click", () => closeDetail(true));
$("inspector-backdrop").addEventListener("click", () => closeDetail(true));
$("dialog-close").addEventListener("click", () => $("form-dialog").close());
$("dialog-cancel").addEventListener("click", () => $("form-dialog").close());
$("submit-local").addEventListener("click", () => commitForm(app));
$("init-bundle").addEventListener("click", () => initBundle(app));
for (const id of ["edit-tags", "detail-edit-tags"]) {
  $(id).addEventListener("click", () => {
    if (state.selectedFile) editTags(app, state.selectedFile);
  });
}
$("create-folder").addEventListener("click", () => createFolder(app));
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
  await load();
  if (state.server) await syncLocal();
} catch (error) { report(error.message); }
