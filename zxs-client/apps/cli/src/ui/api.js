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
    error.code = result.code;
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
    if (open && result.bundle) {
      await app.select("workspace", result.local);
    } else {
      if (open) await api("open", { path: result.local });
      if (app.state.view === "workspace") await app.load();
    }
    const message = version === null ? "已检出 " + result.local : "已恢复 v" + result.version + " 到工作区 · " + result.local;
    show(message + (result.restored_from ? "，待提交" : "") + (result.backup ? "，备份位于 " + result.backup : ""));
  } catch (error) {
    if (choice !== "if_clean" ||
        !["checkout_conflict", "checkout_interrupted"].includes(error.code)) throw error;
    dialog("本地文件有修改", [create("p", "备份当前修改后恢复所选版本。")], {
      label: "备份后恢复",
      submit: () => checkout(app, id, { version, to, open, choice: "backup" }),
    });
  }
}

export function syncDelete(app, entry, expected = entry.version) {
  const message = expected === entry.version
    ? "本地文件已删除或移走。确认后，将回收服务器上的 v" + expected + " 并解除本地追踪。"
    : "服务器上已有更新的 v" + expected + "。确认回收这个版本并解除本地追踪？";
  dialog("同步删除", [create("p", message)], {
    label: "移入回收站", danger: true, submit: async () => {
      try {
        await api("sync_delete", { path: entry.path, expected_version: expected });
      } catch (error) {
        if (error.status !== 409) throw error;
        const detail = await api("detail", { id: entry.container_id });
        if (detail.container.latest_version === expected) throw error;
        return syncDelete(app, entry, detail.container.latest_version);
      }
      await app.load();
      show("原文件容器已移入回收站。");
    },
  });
}

export function untrack(app, entry) {
  dialog("解除追踪", [create("p", "解除追踪后，工作区会保留「" + entry.path + "」的本地内容。")], {
    label: "解除追踪", submit: async () => {
      await api("untrack", { path: entry.path });
      await app.load();
      show("已解除追踪。");
    },
  });
}

export function selectWorkspace(app) {
  dialog("选择工作区", [
    field("path", "工作区目录", { value: app.state.workspace, required: true, spellcheck: false }),
  ], { label: "保存", submit: async form => {
    app.setConnection(await api("select_workspace", { path: form.get("path") }));
    app.state.bundleWarnings = [];
    await app.select("workspace", ".");
  } });
}

export function createFolder(app) {
  const base = app.state.folder === "/" ? "" : app.state.folder;
  dialog("新建文件夹", [field("folder", "目录路径", { value: base + "/", required: true })], {
    label: "创建", submit: async form => {
      await api("create_folder", { folder: form.get("folder") });
      await app.load();
    },
  });
}

export function moveFolder(app, from) {
  const slash = from.lastIndexOf("/");
  const name = from.slice(slash + 1);
  const folder = from.slice(0, slash) || "/";
  dialog("移动或重命名文件夹", pathFields(name, folder), {
    label: "保存", submit: async form => {
      const { issues } = await api("move_folder", { from, name: form.get("name"), folder: form.get("folder") });
      await app.load();
      showPathUpdate(issues);
    },
  });
}

export function deleteFolder(app, folder) {
  dialog("删除文件夹", [create("p", folder),
    field("recursive", "将子目录中的全部文件移入回收站", { type: "checkbox" }),
  ], {
    label: "删除", danger: true, submit: async form => {
      await api("delete_folder", { folder, recursive: form.has("recursive") });
      await app.load();
    },
  });
}

export function purge(app, entry = null) {
  const title = entry ? "彻底删除" : "清空回收站";
  const message = entry ? "将彻底删除「" + entry.container.name + "」的全部版本。" : "将彻底删除回收站中的所有项目及其版本。";
  dialog(title, [create("p", message)], { label: title, danger: true, submit: async () => {
    await api("purge", { id: entry?.recycle.recycle_id ?? null });
    await app.load();
  } });
}

export function connect(app) {
  dialog("连接服务器", [
    field("server", "服务器地址", { value: app.state.server || "http://127.0.0.1:7738", required: true }),
    field("password", "口令", { type: "password", autocomplete: "current-password" }),
  ], { label: "登录", submit: async form => {
    app.setConnection(await api("login", {
      server: form.get("server"), password: form.get("password"),
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

export function edit(app, container, bundle) {
  const title = bundle ? "移动或重命名目录包" : "移动或重命名文件";
  dialog(title, pathFields(container.name, container.folder), { label: "保存", submit: async form => {
    const name = form.get("name");
    const patch = { name: bundle && !/\.zip$/i.test(name) ? name + ".zip" : name, folder: form.get("folder") };
    const { issues } = await api("patch", { id: container.container_id, patch });
    await app.load();
    if (app.state.selectedFile?.container_id === container.container_id) await app.detail(container.container_id);
    showPathUpdate(issues);
  } });
}

function pathFields(name, folder) {
  return [
    field("name", "名称", { value: name, required: true }),
    field("folder", "目录路径", { value: folder, required: true }),
  ];
}

function showPathUpdate(issues) {
  if (issues.length) {
    const messages = issues.map(issue => create("p", issue.path + " — " + issue.reason));
    dialog("服务器已保存，本地镜像有待处理项", messages, { dismiss: "关闭" });
  } else show("已保存。");
}

export function editTags(app, container) {
  const id = container.container_id;
  dialog("编辑标签", [
    create("p", container.folder + "/" + container.name, "meta"),
    field("tags", "标签，用逗号分隔", { value: container.tags.join(", ") }),
  ], { label: "保存", submit: async form => {
    const tags = form.get("tags").split(/[,，]/).map(tag => tag.trim()).filter(Boolean);
    await api("patch", { id, patch: { tags } });
    await app.load();
    if (app.state.selectedFile?.container_id === id) await app.detail(id);
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
    const { issues } = await api("restore", { id: entry.recycle.recycle_id, patch });
    await app.load();
    if (issues.length) showPathUpdate(issues);
    else show("文件已还原。");
  } });
}

export function submitCurrentDirectory(app) {
  const { view, folder, workspaceFolder, workspaceBundle } = app.state;
  if (workspaceBundle) {
    dialog("提交目录包", [create("p", workspaceBundle)], {
      label: "提交", submit: () => submitLocal(app, workspaceBundle),
    });
    return;
  }
  return submitDirectory(app, view === "workspace" ? workspaceFolder : folder.slice(1));
}

export async function submitDirectory(app, directory) {
  const result = await api("commit_directory", { directory });
  await app.load();
  const summary = [];
  if (result.committed) summary.push("已提交 " + result.committed + " 项");
  if (result.created_folders) summary.push("已创建 " + result.created_folders + " 个空文件夹");
  if (result.issues.length) summary.push(result.issues.length + " 项待处理");
  if (result.remaining) summary.push(result.remaining + " 项尚未尝试");
  const message = (summary.join("，") || "没有待提交的变更") + "。";
  const details = result.issues.map(issue => {
    const item = create("div");
    item.append(create("p", issue.path, "path"));
    if (issue.conflict) {
      item.append(action("处理冲突", () => submitLocal(app, issue.path), { variant: "secondary", inDialog: true }));
    } else { item.append(create("p", issue.reason, "meta")); }
    return item;
  });
  for (const path of result.bundle_warnings) {
    details.push(create("p", "已忽略 " + path + " 中的 .zxsignore。", "meta"));
  }
  if (result.symlink_warnings.length) {
    details.push(create("p", "已跳过符号链接 " + result.symlink_warnings.join("、") + "。", "meta"));
  }
  if (details.length) dialog("目录提交结果", [create("p", message), ...details], { dismiss: "关闭" });
  else show(message);
}

export function initBundle(app) {
  const { workspace, workspaceFolder } = app.state;
  dialog("创建 .zxsignore", [
    create("p", workspace, "meta"),
    field("path", "工作区内的目录", { value: workspaceFolder === "." ? "" : workspaceFolder, required: true }),
  ], { label: "创建", submit: async form => {
    const { path } = await api("init_bundle", { path: form.get("path") });
    await app.select("workspace", path);
    show("已创建 .zxsignore。");
  } });
}

export function submitLocal(app, path) {
  return commit(app, { path, choice: "stop" });
}

async function commit(app, request) {
  try {
    const result = await api("commit", request);
    await app.load();
    if (result.outcome === "pulled") {
      show("已检出远端最新版" + (result.data.backup ? "，本地备份位于 " + result.data.backup : ""));
    } else { show("已提交 v" + result.data.version.version + "。"); }
  } catch (error) {
    if (error.code !== "commit_conflict" || request.choice !== "stop") throw error;
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
  ], { label: "另存", submit: form => commit(app, {
    ...request, choice: "new", name: form.get("name"),
  }) });
  controls.append(
    action("追加到最新版", () => finish("append"), { variant: "primary", inDialog: true }),
    action("另存为新文件", saveNew, { variant: "secondary" }),
    action("备份本地后拉取", () => finish("pull"), { variant: "secondary", inDialog: true }),
  );
  dialog("提交冲突", [controls], { warning: message + "。选择追加、另存或备份后拉取。" });
}
