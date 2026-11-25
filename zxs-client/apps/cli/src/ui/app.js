import { api, commitForm, connect, post, switchAccount } from "./api.js";
import { empty, renderDetail, renderHeading, renderList } from "./view.js";
import { $, action, busy, create, icon, report } from "./widgets.js";

const state = {
  view: "files", folder: "/", cursor: null, selectedId: null, server: null, workspace: "",
  listRequest: 0, detailRequest: 0,
};
const app = { state, select, setConnection, load, detail, closeDetail };
let filterTimer;

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
  renderPath();
}

function renderPath() {
  const bar = $("crumbs");
  bar.replaceChildren();
  if (state.view === "workspace") {
    if (state.workspace) bar.append(create("span", state.workspace, "ellipsis"));
    return;
  }
  if (state.view !== "files") return;
  const add = (text, path) => {
    const node = action(text, () => select("files", path), { variant: "ghost crumb" });
    node.replaceChildren(create("span", text, "ellipsis"));
    node.title = text;
    if (path === state.folder) node.setAttribute("aria-current", "location");
    bar.append(node);
  };
  add("根目录", "/");
  let path = "";
  for (const part of state.folder.split("/").filter(Boolean)) {
    path += "/" + part;
    bar.append(icon("chevron"));
    add(part, path);
  }
}

function select(view, folder = "/") {
  clearTimeout(filterTimer);
  state.view = view;
  state.folder = folder;
  state.cursor = null;
  $("filters").reset();
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
    const page = await loadView(state.view, state.folder, cursor, query);
    if (id !== state.listRequest) return;
    state.cursor = state.view === "workspace" ? null : page.next_cursor;
    renderList(app, page, append);
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

function loadView(view, folder, cursor, query) {
  if (view === "workspace") return api("status");
  if (view === "recycle") return api("recycle_list", { filter: { cursor, limit: 100 } });
  return api("list", { filter: {
    folder, cursor, limit: 100, q: query.get("q").trim() || null, tag: query.get("tag").trim() || null,
  } });
}

async function detail(id, focus = false) {
  const request = ++state.detailRequest;
  const data = await api("detail", { id });
  if (request !== state.detailRequest || state.view !== "files") return;
  state.selectedId = id;
  renderDetail(app, data);
  markSelection();
  if (focus) $("detail-name").focus({ preventScroll: true });
}

function closeDetail(restoreFocus = false) {
  const id = state.selectedId;
  ++state.detailRequest;
  state.selectedId = null;
  $("details").hidden = true;
  $("inspector-backdrop").hidden = true;
  $("detail-content").replaceChildren();
  markSelection();
  if (restoreFocus && id) {
    const row = Array.from($("rows").rows).find(row => row.dataset.rowKey === id);
    row?.querySelector(".file-link").focus({ preventScroll: true });
  }
}

function markSelection() {
  for (const row of $("rows").rows) {
    const current = row.dataset.rowKey === state.selectedId;
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
    if (state.selectedId) { event.preventDefault(); closeDetail(true); return; }
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
$("refresh").addEventListener("click", event => busy(event.currentTarget, () => load()));
$("next").addEventListener("click", event => busy(event.currentTarget, () => load(true)));
$("close-detail").addEventListener("click", () => closeDetail(true));
$("inspector-backdrop").addEventListener("click", () => closeDetail(true));
$("dialog-close").addEventListener("click", () => $("form-dialog").close());
$("dialog-cancel").addEventListener("click", () => $("form-dialog").close());
$("submit-local").addEventListener("click", () => commitForm(app));
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
} catch (error) { report(error.message); }
