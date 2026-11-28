import { $, action, canReportDialog, closeDialog, create, dialog, field, getDialogVersion, show } from "./widgets.js";
import { withTransfer } from "./progress.js";

let target;
export const getTarget = () => target;

export function setTarget(current) {
  if (target?.key === current) return false;
  target = { key: current };
  closeDialog();
  return true;
}

export async function post(route, data, transfer = null, expectedTarget = null) {
  const headers = { "Content-Type": "application/json" };
  if (transfer) headers["X-ZXS-Transfer"] = transfer;
  if (expectedTarget) headers["X-ZXS-Target"] = expectedTarget;
  const response = await fetch(route, {
    method: "POST",
    headers,
    body: JSON.stringify(data),
  });
  const result = await response.json();
  if (!response.ok) {
    const error = new Error(result.message || "操作失败");
    error.code = result.code;
    error.status = response.status;
    throw error;
  }
  return result;
}

const transferLabels = {
  checkout: "检出", commit: "提交", commit_directory: "提交目录", sync: "同步",
  move_folder: "移动目录", restore: "还原", patch: "更新文件",
};
export function api(action, args = {}, expectedTarget = target) {
  const data = { action, ...args };
  const label = action === "patch" && !args.patch.folder && !args.patch.name ? null : transferLabels[action];
  const send = id => post("/api", data, id, expectedTarget?.key);
  return label ? withTransfer(label, send) : send(null);
}
export const mediaUrl = (id, version) => "/media/" + id + "/" + version + "?target=" + target.key;

async function update(app, action, args) {
  const origin = target;
  const result = await api(action, args).catch(error => { if (origin === target) throw error; });
  if (origin !== target) return;
  await app.load();
  return origin === target ? result : undefined;
}

export async function checkout(app, id, { version = null, to = null, choice = "if_clean" } = {}) {
  const origin = { target, detailRequest: app.state.detailRequest };
  try {
    const result = await api("checkout", { id, version, to, choice });
    if (target === origin.target && app.state.view === "workspace") await app.load();
    const message = version === null ? "已检出 " + result.local : "已恢复 v" + result.version + " 到工作区 · " + result.local;
    if (target === origin.target) show(message + (result.restored_from ? "，待提交" : "") + (result.backup ? "，备份位于 " + result.backup : ""));
  } catch (error) {
    if (target !== origin.target) return;
    if (choice !== "if_clean" ||
        !["checkout_conflict", "checkout_interrupted"].includes(error.code)) throw error;
    if (app.state.detailRequest !== origin.detailRequest || $("form-dialog").open) return;
    dialog("本地文件有修改", [create("p", "备份当前修改后恢复所选版本。")], {
      label: "备份后恢复",
      submit: () => checkout(app, id, { version, to, choice: "backup" }),
    });
  }
}

export function syncDelete(app, entry, expected = entry.version) {
  const message = expected === entry.version
    ? "本地文件已删除或移走。确认后，将回收服务器上的 v" + expected + " 并解除本地追踪。"
    : "服务器上已有更新的 v" + expected + "。确认回收这个版本并解除本地追踪？";
  dialog("同步删除", [create("p", message)], {
    label: "移入回收站", danger: true, submit: async (_form, canReport) => {
      const expectedTarget = target;
      try {
        const args = { path: entry.path, expected_version: expected, expected_container: entry.container_id };
        if (await update(app, "sync_delete", args)) show("原文件容器已移入回收站。");
      } catch (error) {
        if (error.status !== 409 || !canReport()) throw error;
        const detail = await api("detail", { id: entry.container_id }, expectedTarget);
        if (detail.container.latest_version === expected) throw error;
        if (expectedTarget === target && canReport()) syncDelete(app, entry, detail.container.latest_version);
      }
    },
  });
}

export function untrack(app, entry) {
  dialog("解除追踪", [create("p", "解除追踪后，工作区会保留「" + entry.path + "」的本地内容。")], {
    label: "解除追踪", submit: async () => {
      if (await update(app, "untrack", { path: entry.path, id: entry.container_id })) show("已解除追踪。");
    },
  });
}

export function selectWorkspace(app) {
  dialog("选择工作区", [
    field("path", "工作区目录", { value: app.state.workspace, required: true, spellcheck: false }),
  ], { label: "保存", submit: async form => {
    app.setConnection(await api("select_workspace", { path: form.get("path") }));
    await app.select("workspace");
  } });
}

export function moveFolder(app, from) {
  const slash = from.lastIndexOf("/");
  const name = from.slice(slash + 1);
  const folder = from.slice(0, slash) || "/";
  dialog("移动或重命名文件夹", pathFields(name, folder), {
    label: "保存", submit: async (form, canReport) => {
      const args = { from, name: form.get("name"), folder: form.get("folder") };
      if (args.name === name && args.folder === folder) return;
      showPathUpdate(await update(app, "move_folder", args), canReport);
    },
  });
}

export function deleteFolder(app, folder) {
  dialog("删除文件夹", [create("p", folder),
    field("recursive", "将子目录中的全部文件移入回收站", { type: "checkbox" }),
  ], {
    label: "删除", danger: true,
    submit: form => update(app, "delete_folder", { folder, recursive: form.has("recursive") }),
  });
}

export function purge(app, entry = null) {
  const title = entry ? "彻底删除" : "清空回收站";
  const message = entry ? "将彻底删除「" + entry.container.name + "」的全部版本。" : "将彻底删除回收站中的所有项目及其版本。";
  dialog(title, [create("p", message)], { label: title, danger: true,
    submit: () => update(app, "purge", { id: entry?.recycle.recycle_id ?? null }),
  });
}

export function connect(app, server = app.state.server) {
  dialog("连接服务器", [
    field("server", "服务器地址", { value: server || "http://127.0.0.1:7738", required: true }),
    field("password", "口令", { type: "password", autocomplete: "current-password" }),
  ], { label: "登录", submit: async form => {
    await accountChanged(app, await api("login", { server: form.get("server"), password: form.get("password") }));
  } });
}

function accountChanged(app, session) {
  const { view, workspace, folder } = app.state;
  app.setConnection(session);
  show("");
  return app.select(view, view === "workspace" && session.workspace === workspace ? folder : undefined);
}

export function switchAccount(app) {
  if (!app.state.server) { connect(app); return; }
  dialog("断开连接", [create("p", "断开与 " + app.state.server + " 的当前会话。")], {
    label: "断开连接", submit: async () => accountChanged(app, await api("logout")),
  });
}

export function edit(app, container, bundle) {
  const title = bundle ? "移动或重命名目录包" : "移动或重命名文件";
  dialog(title, pathFields(container.name, container.folder), { label: "保存", submit: async (form, canReport) => {
    const patch = { name: form.get("name"), folder: form.get("folder") };
    for (const [field, value] of Object.entries(patch)) if (value === container[field]) delete patch[field];
    if (!Object.keys(patch).length) return;
    showPathUpdate(await updateContainer(app, container.container_id, patch), canReport);
  } });
}

function pathFields(name, folder) {
  return [
    field("name", "名称", { value: name, required: true }),
    field("folder", "目录路径", { value: folder, required: true }),
  ];
}

function showPathUpdate(result, canReport, success = "已保存。") {
  if (!result) return;
  const { issues } = result;
  if (issues.length) {
    if (!canReport()) return;
    const messages = issues.map(issue => create("p", issue.path + " — " + issue.reason));
    dialog("服务器已保存，本地镜像有待处理项", messages, { dismiss: "关闭" });
  } else show(success);
}

async function updateContainer(app, id, patch) {
  const origin = target;
  const result = await update(app, "patch", { id, patch });
  if (result && app.state.selectedFile?.container_id === id) await app.detail(id);
  return origin === target ? result : undefined;
}

export function editTags(app, container) {
  const needsCheckbox = tag => !tag || /[,，\r\n]/.test(tag) || tag.trim() !== tag;
  const exact = container.tags.filter(needsCheckbox);
  dialog("编辑标签", [
    create("p", container.folder + "/" + container.name, "meta"),
    field("tags", "标签，用逗号分隔", { value: container.tags.filter(tag => !needsCheckbox(tag)).join(", ") }),
    ...exact.map((tag, index) => field("keep_tag", tag || "空标签", { type: "checkbox", value: index, checked: true })),
  ], { label: "保存", submit: async form => {
    const tags = form.get("tags").split(/[,，]/).map(tag => tag.trim()).filter(Boolean);
    tags.push(...form.getAll("keep_tag").map(index => exact[index]));
    const unique = new Set(tags);
    if (unique.size === container.tags.length && container.tags.every(tag => unique.has(tag))) return;
    await updateContainer(app, container.container_id, { tags });
  } });
}

export function recycleFile(app, container) {
  dialog("移入回收站", [create("p", `「${container.folder}/${container.name}」将移入回收站，可从回收站还原。`)], {
    label: "移入回收站", danger: true, submit: async () => {
      if (!await update(app, "recycle", { id: container.container_id })) return;
      if (app.state.selectedFile?.container_id === container.container_id) app.closeDetail();
      show("已移入回收站。");
    },
  });
}

export function restoreEntry(app, entry) {
  dialog("还原文件", [
    field("name", "名称", { value: entry.container.name, required: true }),
    field("folder", "目标目录", { placeholder: "/数据盘/目录", required: true }),
  ], { label: "还原", submit: async (form, canReport) => {
    const patch = { name: form.get("name"), folder: form.get("folder") };
    showPathUpdate(await update(app, "restore", { id: entry.recycle.recycle_id, patch }), canReport, "文件已还原。");
  } });
}

export function submitCurrentDirectory(app) {
  const { folder, workspaceBundle } = app.state;
  if (!workspaceBundle) return submitDirectory(app, folder);
  dialog("提交目录包", [create("p", workspaceBundle.root)], {
    label: "提交", submit: () => submitLocal(app, workspaceBundle.root, workspaceBundle.container_id),
  });
}

export async function submitDirectory(app, directory) {
  const version = getDialogVersion();
  const result = await update(app, "commit_directory", { directory });
  if (!result) return;
  const summary = [];
  if (result.committed) summary.push("已提交 " + result.committed + " 项");
  if (result.created_folders) summary.push("已创建 " + result.created_folders + " 个空文件夹");
  if (result.issues.length) summary.push(result.issues.length + " 项待处理");
  if (result.remaining) summary.push(result.remaining + " 项尚未尝试");
  const message = (summary.join("，") || "没有待提交的变更") + "。";
  const details = result.issues.map(issue => {
    const item = create("div");
    item.append(create("p", issue.path, "path"), create("p", issue.reason, "meta"));
    if (issue.conflict) {
      item.append(action("处理冲突", () => submitLocal(app, issue.path, issue.container_id), { variant: "secondary", inDialog: true }));
    }
    return item;
  });
  for (const path of result.bundle_warnings) {
    details.push(create("p", "已忽略 " + path + " 中的 .zxsignore。", "meta"));
  }
  if (result.symlink_warnings.length) {
    details.push(create("p", "已跳过符号链接 " + result.symlink_warnings.join("、") + "。", "meta"));
  }
  if (details.length && canReportDialog(version)) dialog("目录提交结果", [create("p", message), ...details], { dismiss: "关闭" });
  else show(message);
}

export async function initBundle(app) {
  const { folder } = app.state;
  const expected = target;
  const { path } = await api("init_bundle", { path: folder });
  if (target !== expected || app.state.view !== "workspace") return;
  const current = app.state.folder;
  await (current === folder || current.startsWith(folder + "/")
    ? app.select("workspace", path + current.slice(folder.length)) : app.load());
  show("已创建 .zxsignore。");
}

export function submitLocal(app, path, expectedContainer = null) {
  return commit(app, { path, expected_container: expectedContainer, choice: "stop" });
}

async function commit(app, request) {
  const origin = { target, dialog: getDialogVersion() };
  try {
    const result = await update(app, "commit", request);
    if (!result) return;
    if (result.outcome === "pulled") {
      show("已检出远端最新版到 " + result.data.local + (result.data.backup ? "，本地备份位于 " + result.data.backup : ""));
    } else {
      const target = result.data.container;
      const path = request.choice === "stop" ? "" : " 到 " + target.folder + "/" + target.name;
      show("已提交 v" + result.data.version.version + path + "。");
    }
  } catch (error) {
    if (origin.target !== target) return;
    if (error.code !== "commit_conflict" || request.choice !== "stop") throw error;
    if (canReportDialog(origin.dialog)) conflict(app, request, error.message);
  }
}

function conflict(app, request, message) {
  const controls = create("div", null, "actions conflict-actions");
  const finish = async choice => {
    await commit(app, { ...request, choice });
    close();
  };
  const saveNew = () => dialog("另存为新文件", [
    field("name", "新文件名", { required: true }),
  ], { label: "另存", submit: form => commit(app, {
    ...request, choice: "new", name: form.get("name"),
  }) });
  controls.append(
    action("追加到最新版", () => finish("append"), { variant: "primary", inDialog: true }),
    action("另存为新文件", saveNew, { variant: "secondary" }),
    action("备份本地后拉取", () => finish("pull"), { variant: "secondary", inDialog: true }),
  );
  const close = dialog("提交冲突", [controls], { warning: message + "。选择追加、另存或备份后拉取。" });
}
