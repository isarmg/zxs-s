const $ = id => document.getElementById(id);
const state = { view: "files", folder: "/", cursor: null, detail: null, server: null, workspace: "" };
const views = { files: ["文件", "存储"], workspace: ["工作区", "本地"], recycle: ["回收站", "存储"] };
const time = seconds => new Intl.DateTimeFormat("zh-CN", { dateStyle: "medium", timeStyle: "short" }).format(new Date(seconds * 1000));
const bytes = length => new Intl.NumberFormat("zh-CN", { notation: "compact" }).format(length) + " B";

function create(tag, text, className) {
  const node = document.createElement(tag);
  if (text != null) node.textContent = text;
  if (className) node.className = className;
  return node;
}

function button(text, handle, className = "quiet") {
  const node = create("button", text, className);
  node.type = "button";
  node.addEventListener("click", () => busy(node, handle));
  return node;
}

async function busy(node, job) {
  node.disabled = true;
  try { await job(); } catch (error) { show(error.message, true); }
  finally { node.disabled = false; }
}

async function post(route, data) {
  const response = await fetch(route, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  });
  const result = await response.json();
  if (!response.ok) {
    const error = new Error(result.message || "操作失败");
    error.remoteConflict = result.remoteConflict;
    error.status = response.status;
    throw error;
  }
  return result;
}

const api = (action, args = {}) => post("/api", { action, ...args });

function show(message, error = false) {
  const notice = $("notice");
  notice.textContent = message;
  notice.classList.toggle("error", error);
  notice.hidden = !message;
}

function connection(info) {
  state.server = info.server;
  state.workspace = info.workspace;
  $("server").textContent = info.server || "尚未连接";
  $("signal").classList.toggle("online", Boolean(info.server));
  $("account").textContent = info.server ? "断开连接" : "连接服务器";
}

function select(view, folder = "/") {
  state.view = view;
  state.folder = folder;
  $("filters").reset();
  closeDetail();
  return load();
}

async function load(append = false) {
  const view = state.view, folder = state.folder;
  const cursor = append ? state.cursor : null;
  const query = new FormData($("filters"));
  $("title").textContent = views[view][0];
  $("context").textContent = views[view][1];
  $("filters").hidden = view !== "files";
  $("submit-local").hidden = view === "recycle";
  for (const nav of document.querySelectorAll("[data-view]")) {
    nav.classList.toggle("current", nav.dataset.view === view);
    if (nav.dataset.view === view) nav.setAttribute("aria-current", "page");
    else nav.removeAttribute("aria-current");
  }
  crumbs(view, folder);
  if (!append) {
    $("rows").replaceChildren();
    $("empty").hidden = true;
    $("next").hidden = true;
    state.cursor = null;
  }
  try {
    const data = await loadView(view, folder, cursor, query);
    if (view !== state.view || folder !== state.folder) return;
    render(view, data, append);
  } catch (error) {
    if (error.status === 401 && view !== "workspace") {
      empty("连接 ZXS 服务器后即可查看文件。");
      if (!$("form-dialog").open) connect();
    } else { show(error.message, true); }
  }
}

function loadView(view, folder, cursor, query) {
  if (view === "workspace") return api("status");
  if (view === "recycle") return api("recycle_list", { filter: { cursor, limit: 100 } });
  return api("list", { filter: { folder, cursor, q: query.get("q").trim() || null, tag: query.get("tag").trim() || null, limit: 100 } });
}

function crumbs(view, folder) {
  const bar = $("crumbs");
  bar.replaceChildren();
  if (view === "workspace") { bar.append(create("span", state.workspace)); return; }
  if (view !== "files") return;
  bar.append(button("根目录", () => select("files")));
  let parent = "";
  for (const part of folder.split("/").filter(Boolean)) {
    const path = parent += "/" + part;
    bar.append(create("span", "/"), button(part, () => select("files", path)));
  }
}

function columns(names) {
  const row = create("tr");
  for (const name of names) row.append(create("th", name, name === "标签" ? "tag-cell" : ""));
  $("columns").replaceChildren(row);
}

function empty(text) {
  $("empty").textContent = text;
  $("empty").hidden = false;
}

function row(item, cells, icon, job) {
  const tr = create("tr");
  const label = button(item, job, "file-link");
  label.prepend(create("span", icon.symbol, "icon " + (icon.kind || "")));
  const first = create("td");
  first.append(label);
  tr.append(first);
  for (const [index, cell] of cells.entries()) {
    const td = create("td", typeof cell === "string" ? cell : null, "meta");
    if (typeof cell !== "string") td.append(cell);
    if (state.view === "files" && index === 2) td.classList.add("tag-cell");
    tr.append(td);
  }
  $("rows").append(tr);
}

function render(view, page, append) {
  if (view === "workspace") { renderWorkspace(page); return; }
  if (view === "recycle") {
    columns(["名称", "原目录", "回收时间", "操作"]);
    for (const entry of page.entries) {
      const restore = button("还原", () => restoreEntry(entry), "secondary");
      row(entry.container.name, [entry.container.folder, time(entry.recycle.recycled_at), restore], glyph(entry.container.name), () => restoreEntry(entry));
    }
    if (!$("rows").children.length) empty("回收站为空。");
  } else {
    columns(["名称", "版本", "更新时间", "标签"]);
    if (!append) for (const folder of page.folders) {
      row(folder.folder.split("/").at(-1), [folder.count + " 个文件", "—", "—"], { symbol: "▰", kind: "folder" }, () => select("files", folder.folder));
    }
    for (const entry of page.containers) {
      row(entry.name, ["v" + entry.latest_version, time(entry.updated_at), entry.tags.join(" · ") || "—"], glyph(entry.name), () => detail(entry.container_id));
    }
    if (!$("rows").children.length) empty("此处没有文件。");
  }
  state.cursor = page.next_cursor;
  $("next").hidden = !state.cursor;
}

function renderWorkspace(statuses) {
  const labels = { clean: "已检出", modified: "有本地修改", missing: "本地缺失", pending: "等待回执" };
  columns(["本地路径", "版本", "状态", "操作"]);
  for (const entry of statuses) {
    const action = entry.state === "missing" ? "重新检出" : "提交";
    const job = entry.state === "missing" && entry.container_id
      ? () => checkout(entry.container_id, null, entry.path) : () => commitForm(entry.path);
    row(entry.path, [entry.version ? "v" + entry.version : "—", labels[entry.state], button(action, job, "secondary")],
      glyph(entry.path), () => api("open", { path: entry.path }));
  }
  if (!statuses.length) empty("检出或提交文件后，工作区会显示本地状态。");
}

function glyph(name) {
  return /\.(mp4|webm|mov|mkv|mp3|wav|ogg|m4a)$/i.test(name) ? { symbol: "▶", kind: "media" } : { symbol: "▤" };
}

async function detail(id) {
  const data = await api("detail", { id });
  state.detail = data;
  const c = data.container;
  const latest = data.versions.at(-1);
  $("detail-name").textContent = c.name;
  $("details").hidden = false;
  const content = $("detail-content");
  content.replaceChildren(create("p", c.folder + " · v" + c.latest_version + " · " + bytes(latest.size)));
  if (c.tags.length) content.append(create("p", c.tags.join(" · ")));
  const acts = create("div", null, "actions");
  acts.append(button("检出并打开", () => checkout(id, null, null, true), "primary"),
    button("下载", () => download(id, latest.version)), button("编辑信息", () => edit(c)),
    button("移入回收站", async () => {
      if (!window.confirm("移入回收站？")) return;
      await api("recycle", { id });
      closeDetail();
      await load();
      show("已移入回收站。");
    }, "quiet danger"));
  content.append(acts);
  const preview = previewButton(id, latest);
  if (preview) content.append(preview);
  content.append(create("h3", "历史版本"));
  for (const version of data.versions.slice().reverse()) content.append(versionRow(c, version));
}

function versionRow(container, version) {
  const id = container.container_id;
  const item = create("div", null, "version");
  item.append(create("strong", "v" + version.version),
    create("span", "  ·  " + time(version.created_at) + "  ·  " + bytes(version.size), "meta"));
  const controls = create("div", null, "actions");
  controls.append(button("检出", () => checkout(id, version.version)), button("下载", () => download(id, version.version)));
  if (version.version !== container.latest_version) {
    controls.append(button("以此创建新版", async () => {
      await api("revert", { id, expected: container.latest_version, version: version.version });
      await load();
      await detail(id);
      show("已从历史版本创建新版。");
    }, "secondary"));
  }
  item.append(controls);
  return item;
}

function previewButton(id, version) {
  const name = version.name_at_commit;
  let tag = null;
  if (/\.(mp4|webm|mov|mkv|m4v)$/i.test(name)) tag = "video";
  if (/\.(mp3|wav|ogg|m4a)$/i.test(name)) tag = "audio";
  if (/\.(jpe?g|png|gif|webp|avif)$/i.test(name)) tag = "img";
  if (!tag) return null;
  const panel = create("div", null, "actions");
  panel.append(button("预览原件", () => {
    const node = create(tag);
    if (tag === "img") node.alt = name;
    else {
      node.controls = true;
      node.preload = "metadata";
    }
    node.src = mediaUrl(id, version.version);
    panel.replaceChildren(node);
  }, "secondary"));
  return panel;
}

function mediaUrl(id, version) { return "/media/" + id + "/" + version; }

function download(id, version) {
  const link = create("a");
  link.href = mediaUrl(id, version) + "?download=1";
  link.download = "";
  link.click();
}

async function checkout(id, version = null, to = null, open = false, choice = "if_clean") {
  try {
    const result = await api("checkout", { id, version, to, choice });
    if (open) await api("open", { path: result.local });
    show("已检出 " + result.local + (result.backup ? "，备份位于 " + result.backup : ""));
    if (state.view === "workspace") await load();
  } catch (error) {
    if (error.status === 409 && error.message.includes("backed up") && choice === "if_clean") {
      dialog("本地文件有修改", [create("p", "备份当前修改后检出所选版本。")], "备份并检出",
        () => checkout(id, version, to, open, "backup"));
    } else { throw error; }
  }
}

function closeDetail() {
  state.detail = null;
  $("details").hidden = true;
  $("detail-content").replaceChildren();
}

function field(name, label, options = {}) {
  const wrapper = create("label", label, options.type === "checkbox" ? "check" : "");
  const input = create("input");
  input.name = name;
  Object.assign(input, options);
  wrapper.append(input);
  return wrapper;
}

function dialog(title, fields, label, submit) {
  const popup = $("form-dialog");
  $("dialog-title").textContent = title;
  $("dialog-body").replaceChildren(...fields);
  $("dialog-error").hidden = true;
  $("dialog-submit").textContent = label;
  $("dialog-submit").hidden = false;
  if (!popup.open) popup.showModal();
  $("dialog-form").onsubmit = async event => {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const submitButton = $("dialog-submit");
    submitButton.disabled = true;
    try {
      const switched = await submit(form);
      if (!switched) popup.close();
    } catch (error) {
      $("dialog-error").textContent = error.message;
      $("dialog-error").hidden = false;
    } finally { submitButton.disabled = false; }
  };
}

function connect() {
  dialog("连接服务器", [
    field("server", "服务器地址", { value: state.server || "http://127.0.0.1:7738", required: true }),
    field("password", "口令", { type: "password", required: true, autocomplete: "current-password" }),
    field("public", "允许公网明文 HTTP", { type: "checkbox" }),
  ], "登录", async form => {
    connection(await api("login", { server: form.get("server"), password: form.get("password"), allow_public_http: form.has("public") }));
    $("dialog-body").replaceChildren();
    show("");
    await load();
  });
}

function edit(container) {
  dialog("编辑文件信息", [
    field("name", "名称", { value: container.name, required: true }),
    field("folder", "目录", { value: container.folder, required: true }),
    field("tags", "标签，用逗号分隔", { value: container.tags.join(", ") }),
  ], "保存", async form => {
    const patch = { name: form.get("name"), folder: form.get("folder"), tags: tags(form.get("tags")) };
    await api("patch", { id: container.container_id, patch });
    await load();
    await detail(container.container_id);
  });
}

function restoreEntry(entry) {
  dialog("还原文件", [
    field("name", "名称", { value: entry.container.name, required: true }),
    field("folder", "目录", { value: entry.container.folder, required: true }),
  ], "还原", async form => {
    await api("restore", { id: entry.recycle.recycle_id, patch: { name: form.get("name"), folder: form.get("folder") } });
    await load();
    show("文件已还原。");
  });
}

function tags(input) { return input.split(/[,，]/).map(tag => tag.trim()).filter(Boolean); }

function commitForm(path = "") {
  const note = create("p", state.workspace, "meta");
  dialog("提交本地文件", [
    note,
    field("path", "工作区内的文件或目录包路径", { value: path, required: true, placeholder: "项目/笔记.md" }),
    field("name", "新文件的远端名称", { placeholder: "留空使用本地名称" }),
    field("folder", "新文件的远端目录", { value: path ? "" : state.folder }),
    field("tags", "新文件的标签", { placeholder: "用逗号分隔" }),
    button("为目录创建 .zxsignore", () => {
      const local = new FormData($("dialog-form")).get("path");
      if (!local) throw new Error("输入要打包的目录路径。");
      return api("init_bundle", { path: local }).then(() => show("已创建 .zxsignore。"));
    }),
  ], "提交", form => commit({
    path: form.get("path"), folder: form.get("folder") || null, name: form.get("name") || null,
    tags: tags(form.get("tags")), choice: "stop",
  }));
}

async function commit(request) {
  try {
    const result = await api("commit", request);
    await load();
    if (result.outcome === "pulled") {
      show("已检出远端最新版" + (result.data.backup ? "，本地备份位于 " + result.data.backup : ""));
    } else { show("已提交 v" + result.data.version.version + "。"); }
    return false;
  } catch (error) {
    if (!error.remoteConflict || request.choice !== "stop") throw error;
    conflict(request, error.message);
    return true;
  }
}

function conflict(request, message) {
  const actions = create("div", null, "actions");
  const finish = async choice => {
    await commit({ ...request, choice });
    $("form-dialog").close();
  };
  actions.append(button("追加到最新版", () => finish("append"), "primary"),
    button("另存为新文件", () => dialog("另存为新文件", [
      field("name", "新文件名", { required: true }), field("folder", "新目录", { value: request.folder || state.folder }),
    ], "另存", form => commit({ ...request, choice: "new", name: form.get("name"), folder: form.get("folder") || null })), "secondary"),
    button("备份本地后拉取", () => finish("pull"), "secondary"));
  dialog("提交冲突", [create("p", message), actions], "关闭", () => false);
  $("dialog-submit").hidden = true;
  $("form-dialog").addEventListener("close", () => { $("dialog-submit").hidden = false; }, { once: true });
}

for (const nav of document.querySelectorAll("[data-view]")) nav.addEventListener("click", () => select(nav.dataset.view));
$("filters").addEventListener("submit", event => { event.preventDefault(); load(); });
$("refresh").addEventListener("click", () => load());
$("next").addEventListener("click", event => busy(event.currentTarget, () => load(true)));
$("close-detail").addEventListener("click", closeDetail);
$("dialog-close").addEventListener("click", () => $("form-dialog").close());
$("dialog-cancel").addEventListener("click", () => $("form-dialog").close());
$("submit-local").addEventListener("click", () => commitForm());
$("account").addEventListener("click", event => busy(event.currentTarget, async () => {
  if (!state.server) { connect(); return; }
  if (!window.confirm("退出当前服务器会话？")) return;
  connection(await api("logout"));
  closeDetail();
  await load();
}));

try {
  const secret = location.hash.slice(1);
  if (secret) {
    history.replaceState(null, "", "/");
    await post("/access", { token: secret });
  }
  connection(await api("session"));
  await load();
} catch (error) {
  show(error.message, true);
}
