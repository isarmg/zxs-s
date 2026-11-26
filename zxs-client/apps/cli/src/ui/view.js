import { api, checkout, deleteFolder, edit, mediaUrl, moveFolder, purge, recycleFile, restoreEntry, submitDirectory, submitLocal, syncDelete, untrack } from "./api.js";
import { $, action, bytes, create, icon, time } from "./widgets.js";

const views = {
  files: { title: "文件", columns: [["名称", "name"], ["标签", "tags"], ["版本", "version"], ["更新时间", "time"], ["操作", "action"]] },
  workspace: { title: "工作区", columns: [["名称", "name"], ["版本", "version"], ["状态", "status"], ["操作", "action"]] },
  bundle: { columns: [["名称", "name"]] },
  recycle: { title: "回收站", columns: [["名称", "name"], ["原目录", "path"], ["回收时间", "time"], ["操作", "action"]] },
};

export function renderHeading(viewName) {
  const view = views[viewName];
  $("title").textContent = view.title;
  $("filters").hidden = viewName !== "files";
  $("bundle-warning").hidden = true;
  for (const nav of document.querySelectorAll("button[data-view]")) {
    if (nav.dataset.view === viewName) nav.setAttribute("aria-current", "page");
    else nav.removeAttribute("aria-current");
  }
  renderColumns(viewName);
}

function renderColumns(viewName) {
  $("table").dataset.view = viewName;
  const heading = create("tr");
  const widths = [];
  for (const [label, key] of views[viewName].columns) {
    const th = create("th", label, columnClass(key));
    th.scope = "col";
    heading.append(th);
    widths.push(create("col", null, "col-" + key));
  }
  $("columns").replaceChildren(heading);
  $("table-widths").replaceChildren(...widths);
}

function columnClass(key) {
  const right = { version: "cell-number", time: "cell-time", action: "cell-action" };
  return key + "-column meta numeric " + (right[key] || "");
}

export function empty(text, glyph) {
  $("empty").replaceChildren(icon(glyph, true), create("p", text));
  $("empty").hidden = false;
}

function fileKind(name) {
  if (/\.zip$/i.test(name)) return "archive";
  if (/\.(mp4|webm|mov|mkv|m4v|mp3|wav|ogg|m4a|jpe?g|png|gif|webp|avif|heic|tiff?)$/i.test(name)) return "media";
  return "file";
}

function containerLabel(container, bundles) {
  return { name: container.name, kind: bundles.includes(container.container_id) ? "bundle" : fileKind(container.name) };
}

function fileIcon(kind, large = false) {
  const node = create("span", null, large ? "file-icon large" : "file-icon");
  node.dataset.kind = kind;
  if (kind === "archive") node.title = "压缩包";
  if (kind === "bundle") node.title = "目录包";
  node.append(icon(kind, large));
  return node;
}

const versionLabel = number => number ? create("span", "v" + number, "badge") : "";
function tagList(tags, limit = 2) {
  const node = create("div", null, "tag-pills");
  node.title = tags.join(", ");
  for (const tag of tags.slice(0, limit)) node.append(create("span", tag, "badge tag-pill"));
  if (tags.length > limit) node.append(create("span", "+" + (tags.length - limit), "badge tag-more"));
  return node;
}

function row(view, { key, name, kind, subtitle }, cells, open) {
  const tr = create("tr");
  tr.dataset.rowKey = key;
  const label = action(name, open, { variant: "file-link" });
  const title = create("span", null, "file-label");
  title.append(create("span", name, "ellipsis"));
  if (subtitle) title.append(create("span", subtitle, "meta ellipsis"));
  label.replaceChildren(fileIcon(kind), title);
  label.title = name;
  label.setAttribute("aria-label", name);
  tr.addEventListener("click", event => {
    if (!event.target.closest("button")) label.click();
  });
  const first = create("td");
  first.append(label);
  tr.append(first);
  for (const [index, cell] of cells.entries()) {
    const [, field] = views[view].columns[index + 1];
    const td = create("td", null, columnClass(field));
    if (typeof cell === "string") {
      const text = create("span", cell, "ellipsis");
      text.title = cell;
      td.append(text);
    } else { td.append(cell); }
    tr.append(td);
  }
  return tr;
}

function fileRows(app, page, append) {
  const rows = [];
  if (!append) {
    for (const { folder } of page.folders) {
      const disk = app.state.folder === "/";
      const entry = { key: folder, name: folder.split("/").at(-1), kind: disk ? "disk" : "folder" };
      const actions = create("div", null, "folder-actions");
      if (!disk) actions.append(
        action("移动或重命名文件夹", () => moveFolder(app, folder), { glyph: "edit", iconOnly: true }),
        action("删除文件夹", () => deleteFolder(app, folder), { variant: "ghost danger", glyph: "trash", iconOnly: true }),
      );
      rows.push(row("files", entry, ["", "", "", actions], () => app.select("files", folder)));
    }
  }
  for (const c of page.containers) {
    const label = containerLabel(c, page.bundles);
    const subtitle = app.state.search ? c.folder : "";
    const entry = { key: c.container_id, subtitle, ...label };
    const bundle = label.kind === "bundle";
    const editPath = action(bundle ? "移动或重命名目录包" : "移动或重命名文件",
      () => edit(app, c, bundle), { glyph: "edit", iconOnly: true });
    rows.push(row("files", entry, [tagList(c.tags), versionLabel(c.latest_version), time(c.updated_at), editPath],
      () => app.detail(c.container_id, true)));
  }
  return rows;
}

function workspaceRows(app, items) {
  const labels = {
    untracked: "新增未追踪", modified: "有本地修改", missing: "本地缺失", interrupted: "上次检出中断",
  };
  return items.map(entry => {
    const directory = entry.local_kind === "directory";
    const group = directory && !entry.bundle && !entry.container_id;
    const status = entry.remote_missing ? "远端已不存在" : labels[entry.state];
    let badge = "";
    if (!group && status && (entry.state !== "untracked" || entry.can_submit)) {
      badge = create("span", status, "badge");
      badge.dataset.status = entry.remote_missing ? "missing" : entry.state;
      if (entry.backup) badge.title = "备份位于 " + entry.backup;
    }
    const missing = entry.state === "missing";
    const job = missing && entry.container_id
      ? () => checkout(app, entry.container_id)
      : group ? () => submitDirectory(app, entry.path) : () => submitLocal(app, entry.path);
    const control = workspaceActions(app, entry, job, missing, group);
    const name = entry.path === "." ? "工作区根目录" : entry.path.split("/").at(-1);
    const kind = entry.bundle ? "bundle" : directory ? "folder" : fileKind(name);
    const file = { key: entry.path, name, kind };
    const open = entry.remote_missing ? () => untrack(app, entry)
      : directory ? () => app.select("workspace", entry.path)
      : missing ? job : () => api("open", { path: entry.path });
    return row("workspace", file, [versionLabel(entry.restored_from ?? entry.version), badge, control], open);
  });
}

function bundleRows(app, entries) {
  return entries.map(({ path, kind }) => {
    const name = path.split("/").at(-1);
    const directory = kind === "directory";
    const entry = { key: path, name, kind: directory ? "folder" : fileKind(name) };
    return row("bundle", entry, [], directory
      ? () => app.select("workspace", path)
      : () => api("open", { path }));
  });
}

function workspaceActions(app, entry, job, missing, group) {
  if (entry.state === "interrupted") {
    return action("重新检出", () => checkout(app, entry.container_id, { to: entry.path, version: entry.version }), { variant: "secondary" });
  }
  if (entry.remote_missing) return action("解除追踪", () => untrack(app, entry), { variant: "secondary" });
  if (!entry.can_submit) return "";
  const label = missing ? "重新检出" : group ? "提交目录" : "提交";
  const primary = action(label, job, { variant: "secondary" });
  if (!missing || !entry.container_id) return primary;
  const controls = create("div", null, "workspace-actions");
  controls.append(primary, action("同步删除", () => syncDelete(app, entry), { variant: "ghost danger" }));
  return controls;
}

function recycleRows(app, page) {
  return page.entries.map(entry => {
    const { recycle, container } = entry;
    const job = () => restoreEntry(app, entry);
    const file = { key: recycle.recycle_id, ...containerLabel(container, page.bundles) };
    const actions = create("div", null, "folder-actions");
    actions.append(action("还原", job, { variant: "secondary" }),
      action("彻底删除", () => purge(app, entry), { variant: "ghost danger", glyph: "trash", iconOnly: true }));
    return row("recycle", file, [container.folder, time(recycle.recycled_at), actions], job);
  });
}

export function renderList(app, page, append) {
  const { view, cursor } = app.state;
  const layout = view === "workspace" && page.mode === "bundle" ? "bundle" : view;
  if ($("table").dataset.view !== layout) renderColumns(layout);
  if (view === "workspace") {
    const warning = $("bundle-warning");
    const messages = [];
    if (layout === "workspace") {
      const paths = new Set([...page.bundle_warnings, ...app.state.bundleWarnings]);
      for (const path of paths) {
        const name = path === "." ? "工作区根目录" : path;
        const reason = path.includes("/") ? "目录下已有文件容器。" : "目录包须在数据盘的子目录内创建。";
        messages.push("已忽略 " + name + " 中的 .zxsignore。" + reason);
      }
      if (page.truncated) messages.push("已显示前 1,000 条新增项。提交目录时会处理该目录下的全部变更。");
      if (page.entries.some(entry => entry.state === "missing" && !entry.bundle && !entry.remote_missing)) {
        messages.push("本地缺失的文件可能已删除或移走。可重新检出，或确认将原容器移入回收站。");
      }
    }
    if (page.symlink_warnings.length) messages.push("已跳过符号链接 " + page.symlink_warnings.join("、") + "。");
    warning.hidden = !messages.length;
    warning.replaceChildren(...messages.map(message => create("span", message)));
  }
  const rows = layout === "files" ? fileRows(app, page, append)
    : layout === "recycle" ? recycleRows(app, page)
    : layout === "bundle" ? bundleRows(app, page.entries) : workspaceRows(app, page.entries);
  const body = $("rows");
  if (append) body.append(...rows);
  else body.replaceChildren(...rows);
  $("next").hidden = !cursor;
  $("count").textContent = (cursor ? "已加载 " : "") + body.rows.length + " 项";
  $("empty").hidden = body.rows.length > 0;
  if (!body.rows.length) {
    const messages = {
      files: ["此处没有文件。", "folder"],
      workspace: ["没有本地变更。", "workspace"],
      bundle: ["此目录为空。", "folder"],
      recycle: ["回收站为空。", "trash"],
    };
    empty(...messages[layout]);
  }
}

export function renderDetail(app, { container, versions, bundles }) {
  const latest = versions.at(-1);
  const label = containerLabel(container, bundles);
  $("detail-name").textContent = label.name;
  $("detail-icon").replaceChildren(fileIcon(label.kind, true));
  $("detail-meta").replaceChildren(
    create("span", container.folder, "path"), versionLabel(container.latest_version),
    create("span", bytes(latest.size), "numeric"), create("span", time(container.updated_at), "numeric"),
  );
  if (container.tags.length) $("detail-meta").append(tagList(container.tags, container.tags.length));
  const sections = [detailActions(app, container, label.kind === "bundle")];
  const preview = previewSection(container.container_id, latest);
  if (preview) sections.push(preview);
  sections.push(versionHistory(app, container, versions));
  $("detail-content").replaceChildren(...sections);
  $("details").hidden = false;
  $("inspector-backdrop").hidden = false;
  $("details").scrollTop = 0;
}

function detailActions(app, container, bundle) {
  const acts = create("section", null, "actions detail-actions");
  acts.setAttribute("aria-label", "文件操作");
  acts.append(
    action("移动或重命名", () => edit(app, container, bundle), { variant: "secondary" }),
    action("移入回收站", () => recycleFile(app, container), { variant: "ghost danger", glyph: "trash", iconOnly: true }),
  );
  return acts;
}

function versionHistory(app, container, versions) {
  const section = create("section", null, "history-section");
  section.append(create("h3", "历史版本"));
  const list = create("ol", null, "history-list");
  for (const version of versions.toReversed()) list.append(versionRow(app, container, version));
  section.append(list);
  return section;
}

function versionRow(app, container, version) {
  const id = container.container_id;
  const current = version.version === container.latest_version;
  const item = create("li", null, "version");
  item.dataset.current = current;
  const info = create("div", null, "version-meta meta numeric");
  info.append(versionLabel(version.version));
  if (current) info.append(create("span", "当前版", "badge current-version"));
  info.append(create("span", time(version.created_at)), create("span", bytes(version.size)));
  const controls = create("div", null, "actions version-actions");
  controls.append(action("恢复到工作区", () => checkout(app, id, { version: version.version })));
  item.append(info, controls);
  return item;
}

function previewSection(id, version) {
  const name = version.name_at_commit;
  let element = null;
  if (/\.(mp4|webm|mov|mkv|m4v)$/i.test(name)) element = "video";
  if (/\.(mp3|wav|ogg|m4a)$/i.test(name)) element = "audio";
  if (/\.(jpe?g|png|gif|webp|avif)$/i.test(name)) element = "img";
  if (!element) return null;
  const section = create("section", null, "preview-section");
  section.setAttribute("aria-label", "媒体预览");
  const frame = create("div", null, "preview-frame");
  frame.append(action("预览原件", () => {
    const node = create(element);
    if (element === "img") node.alt = name;
    else {
      node.controls = true;
      node.preload = "metadata";
    }
    node.src = mediaUrl(id, version.version);
    frame.replaceChildren(node);
    if (element !== "img") node.focus({ preventScroll: true });
  }, { variant: "secondary", glyph: "media" }));
  section.append(frame);
  return section;
}
