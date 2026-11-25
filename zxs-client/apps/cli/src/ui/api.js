import { $, action, create, dialog, field, show } from "./widgets.js";

export async function post(route, data) {
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

export const api = (action, args = {}) => post("/api", { action, ...args });
export const mediaUrl = (id, version) => "/media/" + id + "/" + version;

export function download(id, version) {
  const link = create("a");
  link.href = mediaUrl(id, version) + "?download=1";
  link.download = "";
  link.click();
}

export async function checkout(app, id, { version = null, to = null, open = false, choice = "if_clean" } = {}) {
  try {
    const result = await api("checkout", { id, version, to, choice });
    if (open) await api("open", { path: result.local });
    show("已检出 " + result.local + (result.backup ? "，备份位于 " + result.backup : ""));
    if (app.state.view === "workspace") await app.load();
  } catch (error) {
    if (error.status !== 409 || !error.message.includes("backed up") || choice !== "if_clean") throw error;
    dialog("本地文件有修改", [create("p", "备份当前修改后检出所选版本。")], {
      label: "备份并检出",
      submit: () => checkout(app, id, { version, to, open, choice: "backup" }),
    });
  }
}

export function connect(app) {
  dialog("连接服务器", [
    field("server", "服务器地址", { value: app.state.server || "http://127.0.0.1:7738", required: true }),
    field("password", "口令", { type: "password", required: true, autocomplete: "current-password" }),
    field("public", "允许公网明文 HTTP", { type: "checkbox" }),
  ], { label: "登录", submit: async form => {
    app.setConnection(await api("login", {
      server: form.get("server"), password: form.get("password"), allow_public_http: form.has("public"),
    }));
    $("dialog-body").replaceChildren();
    show("");
    await app.load();
  } });
}

export function switchAccount(app) {
  if (!app.state.server) { connect(app); return; }
  dialog("断开连接", [create("p", "断开与 " + app.state.server + " 的当前会话。")], {
    label: "断开连接", submit: async () => {
      app.setConnection(await api("logout"));
      app.closeDetail();
      await app.load();
    },
  });
}

export function edit(app, container) {
  dialog("编辑文件信息", [
    field("name", "名称", { value: container.name, required: true }),
    field("folder", "目录", { value: container.folder, required: true }),
    field("tags", "标签，用逗号分隔", { value: container.tags.join(", ") }),
  ], { label: "保存", submit: async form => {
    const patch = { name: form.get("name"), folder: form.get("folder"), tags: tags(form.get("tags")) };
    await api("patch", { id: container.container_id, patch });
    await app.load();
    await app.detail(container.container_id);
  } });
}

export function recycleFile(app, container) {
  dialog("移入回收站", [create("p", "「" + container.name + "」将移入回收站，可从回收站还原。")], {
    label: "移入回收站", danger: true, submit: async () => {
      await api("recycle", { id: container.container_id });
      app.closeDetail();
      await app.load();
      show("已移入回收站。");
    },
  });
}

export function restoreEntry(app, entry) {
  dialog("还原文件", [
    field("name", "名称", { value: entry.container.name, required: true }),
    field("folder", "目录", { value: entry.container.folder, required: true }),
  ], { label: "还原", submit: async form => {
    const patch = { name: form.get("name"), folder: form.get("folder") };
    await api("restore", { id: entry.recycle.recycle_id, patch });
    await app.load();
    show("文件已还原。");
  } });
}

export async function revert(app, container, version) {
  const id = container.container_id;
  await api("revert", { id, expected: container.latest_version, version });
  await app.load();
  if (app.state.selectedId === id) await app.detail(id);
  show("已从历史版本创建新版。");
}

function tags(input) { return input.split(/[,，]/).map(tag => tag.trim()).filter(Boolean); }

export function commitForm(app, path = "") {
  dialog("提交本地文件", [
    create("p", app.state.workspace, "meta"),
    field("path", "工作区内的文件或目录包路径", { value: path, required: true, placeholder: "项目/笔记.md" }),
    field("name", "新文件的远端名称", { placeholder: "留空使用本地名称" }),
    field("folder", "新文件的远端目录", { value: path ? "" : app.state.folder }),
    field("tags", "新文件的标签", { placeholder: "用逗号分隔" }),
    action("为目录创建 .zxsignore", () => {
      const local = new FormData($("dialog-form")).get("path");
      if (!local) throw new Error("输入要打包的目录路径。");
      return api("init_bundle", { path: local }).then(() => show("已创建 .zxsignore。"));
    }, { variant: "ghost dialog-link", inDialog: true }),
  ], { label: "提交", submit: form => commit(app, {
    path: form.get("path"), folder: form.get("folder") || null, name: form.get("name") || null,
    tags: tags(form.get("tags")), choice: "stop",
  }) });
}

async function commit(app, request) {
  try {
    const result = await api("commit", request);
    await app.load();
    if (result.outcome === "pulled") {
      show("已检出远端最新版" + (result.data.backup ? "，本地备份位于 " + result.data.backup : ""));
    } else { show("已提交 v" + result.data.version.version + "。"); }
  } catch (error) {
    if (!error.remoteConflict || request.choice !== "stop") throw error;
    conflict(app, request, error.message);
  }
}

function conflict(app, request, message) {
  const controls = create("div", null, "actions conflict-actions");
  const finish = async choice => {
    await commit(app, { ...request, choice });
    $("form-dialog").close();
  };
  const saveNew = () => dialog("另存为新文件", [
    field("name", "新文件名", { required: true }),
    field("folder", "新目录", { value: request.folder || app.state.folder }),
  ], { label: "另存", submit: form => commit(app, {
    ...request, choice: "new", name: form.get("name"), folder: form.get("folder") || null,
  }) });
  controls.append(
    action("追加到最新版", () => finish("append"), { variant: "primary", inDialog: true }),
    action("另存为新文件", saveNew, { variant: "secondary" }),
    action("备份本地后拉取", () => finish("pull"), { variant: "secondary", inDialog: true }),
  );
  dialog("提交冲突", [controls], { warning: message + "。选择追加、另存或备份后拉取。" });
}
