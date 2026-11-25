import { api, checkout, commitForm, download, edit, mediaUrl, recycleFile, restoreEntry, revert } from "./api.js";
import { $, action, bytes, create, icon, time } from "./widgets.js";

const views = {
  files: { title: "文件", columns: [["名称", "name"], ["标签", "tags"], ["版本", "version"], ["更新时间", "time"]] },
  workspace: { title: "工作区", columns: [["本地路径", "name"], ["版本", "version"], ["状态", "status"], ["操作", "action"]] },
  recycle: { title: "回收站", columns: [["名称", "name"], ["原目录", "path"], ["回收时间", "time"], ["操作", "action"]] },
};

export function renderHeading(viewName) {
  const view = views[viewName];
  $("title").textContent = view.title;
  $("filters").hidden = viewName !== "files";
  $("submit-local").hidden = viewName === "recycle";
  for (const nav of document.querySelectorAll("button[data-view]")) {
    if (nav.dataset.view === viewName) nav.setAttribute("aria-current", "page");
    else nav.removeAttribute("aria-current");
  }
  $("table").dataset.view = viewName;
  const heading = create("tr");
  const widths = [];
  for (const [label, key] of view.columns) {
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

function fileIcon(kind, large = false) {
  const node = create("span", null, large ? "file-icon large" : "file-icon");
  node.dataset.kind = kind;
  if (kind === "archive") node.title = "压缩包";
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

function row(view, { key, name, kind }, cells, open) {
  const tr = create("tr");
  tr.dataset.rowKey = key;
  const label = action(name, open, { variant: "file-link" });
  label.replaceChildren(fileIcon(kind), create("span", name, "ellipsis"));
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
    for (const { folder, count } of page.folders) {
      const entry = { key: folder, name: folder.split("/").at(-1), kind: "folder" };
      rows.push(row("files", entry, [count + " 项", "", ""], () => app.select("files", folder)));
    }
  }
  for (const c of page.containers) {
    const entry = { key: c.container_id, name: c.name, kind: fileKind(c.name) };
    rows.push(row("files", entry, [tagList(c.tags), versionLabel(c.latest_version), time(c.updated_at)],
      () => app.detail(c.container_id, true)));
  }
  return rows;
}

function workspaceRows(app, items) {
  const labels = { clean: "已检出", modified: "有本地修改", missing: "本地缺失", pending: "等待回执" };
  return items.map(entry => {
    const badge = create("span", labels[entry.state], "badge");
    badge.dataset.status = entry.state;
    if (entry.state === "clean") badge.prepend(create("span", null, "status-dot"));
    const missing = entry.state === "missing";
    const job = missing && entry.container_id
      ? () => checkout(app, entry.container_id, { to: entry.path }) : () => commitForm(app, entry.path);
    const control = action(missing ? "重新检出" : "提交", job, { variant: "secondary" });
    const file = { key: entry.path, name: entry.path, kind: fileKind(entry.path) };
    return row("workspace", file, [versionLabel(entry.version), badge, control],
      () => missing ? job() : api("open", { path: entry.path }));
  });
}

function recycleRows(app, entries) {
  return entries.map(({ recycle, container }) => {
    const job = () => restoreEntry(app, { recycle, container });
    const file = { key: recycle.recycle_id, name: container.name, kind: fileKind(container.name) };
    const restore = action("还原", job, { variant: "secondary" });
    return row("recycle", file, [container.folder, time(recycle.recycled_at), restore], job);
  });
}

export function renderList(app, page, append) {
  const { view, cursor } = app.state;
  const rows = view === "files" ? fileRows(app, page, append)
    : view === "workspace" ? workspaceRows(app, page) : recycleRows(app, page.entries);
  const body = $("rows");
  if (append) body.append(...rows);
  else body.replaceChildren(...rows);
  $("next").hidden = !cursor;
  $("count").textContent = (cursor ? "已加载 " : "") + body.rows.length + " 项";
  $("empty").hidden = body.rows.length > 0;
  if (!body.rows.length) {
    const messages = {
      files: ["此处没有文件。", "folder"],
      workspace: ["检出或提交文件后，工作区会显示本地状态。", "workspace"],
      recycle: ["回收站为空。", "trash"],
    };
    empty(...messages[view]);
  }
}

export function renderDetail(app, { container, versions }) {
  const latest = versions.at(-1);
  $("detail-name").textContent = container.name;
  $("detail-icon").replaceChildren(fileIcon(fileKind(container.name), true));
  $("detail-meta").replaceChildren(
    create("span", container.folder, "path"), versionLabel(container.latest_version),
    create("span", bytes(latest.size), "numeric"), create("span", time(container.updated_at), "numeric"),
  );
  if (container.tags.length) $("detail-meta").append(tagList(container.tags, container.tags.length));
  const sections = [detailActions(app, container, latest.version)];
  const preview = previewSection(container.container_id, latest);
  if (preview) sections.push(preview);
  sections.push(versionHistory(app, container, versions));
  $("detail-content").replaceChildren(...sections);
  $("details").hidden = false;
  $("inspector-backdrop").hidden = false;
  $("details").scrollTop = 0;
}

function detailActions(app, container, version) {
  const id = container.container_id;
  const acts = create("section", null, "actions detail-actions");
  acts.setAttribute("aria-label", "文件操作");
  acts.append(
    action("检出并打开", () => checkout(app, id, { open: true }), { variant: "primary" }),
    action("下载", () => download(id, version), { variant: "secondary", glyph: "download" }),
    action("编辑信息", () => edit(app, container), { variant: "secondary" }),
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
  controls.append(action("检出", () => checkout(app, id, { version: version.version })),
    action("下载", () => download(id, version.version)));
  if (!current) controls.append(action("以此创建新版", () => revert(app, container, version.version), { variant: "secondary" }));
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
