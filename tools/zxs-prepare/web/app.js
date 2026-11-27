import { api, hasAccess } from "./api.js";

const $ = id => document.getElementById(id);

let disk;
let within = "";
let busy = false;
const bundles = new Set();

function element(tag, text, className) {
  const node = document.createElement(tag);
  node.textContent = text;
  if (className) node.className = className;
  return node;
}

function button(text, click, className) {
  const node = element("button", text, className);
  node.type = "button";
  node.addEventListener("click", click);
  return node;
}

function notice(message, error = false) {
  $("notice").textContent = message;
  $("notice").dataset.error = String(error);
  $("notice").hidden = !message;
}

async function run(job) {
  if (busy) return;
  busy = true;
  render();
  try {
    await job();
  } catch (error) {
    notice(error.message, true);
  } finally {
    busy = false;
    render();
  }
}

async function open(root, path) {
  disk = await api({ action: "scan", root, within: path });
  within = path;
}

function navigate(path) {
  run(async () => {
    notice("");
    await open(disk.root, path);
  });
}

function renderPath() {
  const nav = $("breadcrumbs");
  nav.replaceChildren(button("盘根", () => navigate("")));
  let path = "";
  if (!within) return;
  for (const part of within.split("/")) {
    path = path ? path + "/" + part : part;
    const parent = path;
    nav.append(element("span", "/", "crumb"));
    if (path === within) nav.append(element("span", part, "crumb"));
    else nav.append(button(part, () => navigate(parent)));
  }
}

function size(bytes) {
  for (const [factor, unit] of [[1073741824, "GiB"], [1048576, "MiB"], [1024, "KiB"]]) {
    if (bytes >= factor) return (bytes / factor).toFixed(1) + " " + unit;
  }
  return bytes + " B";
}

function setBundle(path, packed) {
  if (packed) {
    for (const child of bundles) {
      if (child.startsWith(path + "/")) bundles.delete(child);
    }
    bundles.add(path);
  } else {
    bundles.delete(path);
  }
}

function row(entry) {
  const tr = document.createElement("tr");
  const isDirectory = entry.kind === "directory";
  const packed = bundles.has(entry.path);
  const name = element("td", "");
  const label = isDirectory
    ? button(entry.name, () => navigate(entry.path), "file-name")
    : element("span", entry.name, "item-name");
  if (isDirectory) label.disabled = busy || packed;
  name.append(label);
  const choice = element("td", "");
  if (entry.kind === "unsupported") {
    choice.textContent = "不支持的类型";
  } else {
    const select = document.createElement("select");
    select.setAttribute("aria-label", entry.name + " 的处理方式");
    select.add(new Option(isDirectory ? "保留层级" : "单文件容器", "default"));
    select.add(new Option("ZIP 目录包", "bundle"));
    select.value = packed ? "bundle" : "default";
    select.disabled = busy;
    select.addEventListener("change", () => {
      setBundle(entry.path, select.value === "bundle");
      if (isDirectory) label.disabled = bundles.has(entry.path);
      renderPlan();
    });
    choice.append(select);
  }
  tr.append(name, element("td", entry.kind === "file" ? size(entry.bytes) : ""), choice);
  return tr;
}

function renderPlan() {
  $("plan-title").textContent = disk.status === "pending" ? "完成准备" : "准备方式";
  $("kind").hidden = disk.status === "pending" || disk.status === "prepared";
  $("kind").disabled = busy;
  $("recycle").disabled = disk.status !== "empty";
  if ($("recycle").disabled) document.querySelector('[value="data"]').checked = true;
  $("backup").hidden = disk.status !== "data";
  $("mount").hidden = disk.status !== "data";
  $("mount-name").disabled = busy;
  $("mount-name").placeholder = disk.mount_name ?? "填写数据根下的一级目录名";
  $("bundles").hidden = !bundles.size || disk.status !== "data";
  $("selection").replaceChildren(...Array.from(bundles).sort().map(path => {
    const li = element("li", path);
    const remove = button("移除", () => {
      bundles.delete(path);
      render();
    }, "remove");
    remove.disabled = busy;
    remove.setAttribute("aria-label", "移除 ZIP 选择 " + path);
    li.append(remove);
    return li;
  }));
  $("prepare").disabled = busy;
  $("prepare").hidden = disk.status === "prepared";
  $("prepare").textContent = disk.status === "pending" ? "继续准备" : "准备磁盘";
}

function render() {
  $("read").disabled = busy;
  $("root").disabled = busy;
  $("workspace").hidden = !disk;
  if (!disk) return;
  renderPath();
  for (const breadcrumb of $("breadcrumbs").querySelectorAll("button")) breadcrumb.disabled = busy;
  $("breadcrumbs").hidden = disk.status === "prepared" || disk.status === "pending";
  const rows = document.createDocumentFragment();
  for (const entry of disk.entries) rows.append(row(entry));
  $("rows").replaceChildren(rows);
  $("table").hidden = !disk.entries.length;
  $("empty").hidden = disk.entries.length > 0;
  $("empty").textContent = ({
    prepared: "此盘已准备。",
    pending: "发现未完成的目录切换。",
  })[disk.status] || "此目录为空。";
  renderPlan();
}

$("read-form").addEventListener("submit", event => {
  event.preventDefault();
  const root = $("root").value;
  run(async () => {
    disk = undefined;
    bundles.clear();
    $("mount-name").value = "";
    render();
    notice("");
    await open(root, "");
  });
});

$("prepare").addEventListener("click", () => {
  const dialog = $("confirm");
  const action = disk.status === "pending" ? "继续准备" : "准备磁盘";
  $("confirm-title").textContent = action;
  $("confirm-root").textContent = disk.root;
  $("confirm-detail").textContent = disk.status === "data"
    ? "将原地发布数据盘。原件会保存在盘内 .zxs/originals/。选择的 ZIP 共 " + bundles.size + " 项。"
    : disk.status === "pending" ? "按盘上的记录完成剩余切换。"
    : "为此空目录写入" + ($("recycle").checked ? "回收盘" : "数据盘") + "身份。";
  dialog.returnValue = "";
  dialog.showModal();
});

$("confirm").addEventListener("close", () => {
  if ($("confirm").returnValue !== "start") return;
  run(async () => {
    notice("正在准备磁盘…");
    const root = disk.root;
    const action = disk.status === "pending" ? { action: "resume", root }
      : disk.status === "empty" ? { action: "init", root, kind: $("recycle").checked ? "recycle" : "data" }
      : { action: "prepare", root, mount_name: $("mount-name").value || null, bundle: Array.from(bundles) };
    try {
      const result = await api(action);
      await open(root, "");
      bundles.clear();
      const count = result.containers + " 个文件容器。";
      notice("磁盘已准备。" + (result.originals ? count + " 原件保存在 " + result.originals : ""));
    } catch (error) {
      await open(root, "");
      throw error;
    }
  });
});

$("root").placeholder = navigator.userAgent.includes("Windows") ? "D:\\ZXS" : "/mnt/disk1";
if (!hasAccess) notice("请使用终端显示的本机链接。", true);
render();
