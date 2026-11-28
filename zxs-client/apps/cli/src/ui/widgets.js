export const $ = id => document.getElementById(id);

const paths = {
  disk: "M3 3.25h10l1.5 6.25v3.25h-13V9.5zM1.5 9.5h13m-9.75 1.65h.01m2.25 0h.01",
  folder: "M1.75 4.5A1.25 1.25 0 0 1 3 3.25h3l1.5 1.5H13A1.25 1.25 0 0 1 14.25 6v5.75A1.25 1.25 0 0 1 13 13H3a1.25 1.25 0 0 1-1.25-1.25z",
  bundle: "M8 12.75H3A1.25 1.25 0 0 1 1.75 11.5v-7A1.25 1.25 0 0 1 3 3.25h3l1.5 1.5H13A1.25 1.25 0 0 1 14.25 6v1M9.5 8.75h5v5h-5zM11 11.25h2",
  tag: "M2.75 2.75h5.5l5.5 5.5-5.5 5.5-5.5-5.5zM5.5 5.5h.01",
  edit: "M3 13l.75-3.5 7-7 2.75 2.75-7 7zM9.5 3.75l2.75 2.75",
  file: "M4 1.75h5l3 3v8.5H4zM9 1.75V5h3M6 8h4M6 10.5h4",
  media: "M2 3.5h12v9H2zM6.5 5.75l4 2.25-4 2.25z",
  archive: "M2 2.5h12v3H2zM3 5.5v7.25h10V5.5M6.5 8.5h3",
  workspace: "M2 3h12v10H2zM2 6h12M6 6v7",
  trash: "M2.5 4.25h11M6 4.25V2.5h4v1.75M4 4.25l.75 9.25h6.5L12 4.25M6.75 6.75v4.5m2.5-4.5v4.5",
  search: "M11 6.75a4.25 4.25 0 1 1-8.5 0 4.25 4.25 0 0 1 8.5 0zM10 10l3.5 3.5",
  refresh: "M13.5 6a5.7 5.7 0 1 0 .1 4.25M13.5 2.5V6H10",
  upload: "M8 10.5v-8m-3 3 3-3 3 3M2.5 10v3.5h11V10",
  download: "M8 2.5v8m-3-3 3 3 3-3M2.5 10v3.5h11V10",
  check: "M3 8l3.5 3.5L13 5",
  close: "M4 4l8 8m0-8-8 8",
  chevron: "m6 4 4 4-4 4",
  connect: "M6.5 3H3v10h3.5M5.5 8h8m-3-3 3 3-3 3",
  disconnect: "M8.5 3H3v10h5.5M7 8h6.5m-2.5-2.5L13.5 8 11 10.5",
  error: "M14 8A6 6 0 1 1 2 8a6 6 0 0 1 12 0zM8 4.75v3.75M8 11h.01",
};

export function create(tag, text, className) {
  const node = document.createElement(tag);
  if (text != null) node.textContent = text;
  if (className) node.className = className;
  return node;
}

export function icon(name, large = false) {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("class", large ? "svg large" : "svg");
  svg.setAttribute("viewBox", "0 0 16 16");
  svg.setAttribute("fill", "none");
  svg.setAttribute("stroke", "currentColor");
  svg.setAttribute("stroke-width", "1.5");
  svg.setAttribute("stroke-linecap", "round");
  svg.setAttribute("stroke-linejoin", "round");
  svg.setAttribute("aria-hidden", "true");
  const path = document.createElementNS(ns, "path");
  path.setAttribute("d", paths[name]);
  svg.append(path);
  return svg;
}

export function action(text, job, { variant = "ghost", glyph, iconOnly = false, inDialog = false } = {}) {
  const node = create("button", iconOnly ? null : text, variant + (iconOnly ? " icon-button" : ""));
  node.type = "button";
  if (glyph) node.prepend(icon(glyph));
  if (iconOnly) {
    node.title = text;
    node.setAttribute("aria-label", text);
  }
  node.addEventListener("click", () => busy(node, job, inDialog));
  return node;
}

export async function busy(node, job, inDialog = false) {
  if (node.disabled) return;
  const version = dialogVersion;
  node.disabled = true;
  node.setAttribute("aria-busy", "true");
  try { await job(); }
  catch (error) {
    if (!canReportDialog(version) || (inDialog && !$("form-dialog").open)) return;
    report(error.message);
  }
  finally { node.removeAttribute("aria-busy"); node.disabled = false; }
}

const pad = value => String(value).padStart(2, "0");
export function time(seconds) {
  const stamp = new Date(seconds * 1000);
  return stamp.getFullYear() + "-" + pad(stamp.getMonth() + 1) + "-" + pad(stamp.getDate()) +
    " " + pad(stamp.getHours()) + ":" + pad(stamp.getMinutes());
}

export function bytes(value) {
  const units = ["B", "KiB", "MiB", "GiB"];
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit++; }
  return (unit === 0 ? value : value.toFixed(1)) + " " + units[unit];
}

let noticeTimer;
export function show(message) {
  const notice = $("notice");
  clearTimeout(noticeTimer);
  notice.hidden = !message;
  if (!message) { notice.replaceChildren(); return; }
  notice.replaceChildren(
    icon("check"),
    create("p", message),
    action("关闭提示", () => show(""), { glyph: "close", iconOnly: true }),
  );
  noticeTimer = setTimeout(() => { notice.hidden = true; }, 3500);
}

export function field(name, label, options = {}) {
  const wrapper = create("label", label, options.type === "checkbox" ? "check" : "");
  const input = create("input");
  input.name = name;
  Object.assign(input, options);
  if (options.type === "checkbox") wrapper.prepend(input);
  else wrapper.append(input);
  return wrapper;
}

function inlineError(message) {
  const notice = $("dialog-error");
  notice.replaceChildren(icon("error"), create("p", message));
  notice.hidden = !message;
}

export function report(message, version = dialogVersion) {
  if (!$("form-dialog").open) dialog("操作失败", [create("p", "关闭提示后可重试当前操作。")], {
    warning: message, dismiss: "关闭",
  });
  else if (canReportDialog(version)) inlineError(message);
}

let dialogVersion = 0;
export const getDialogVersion = () => dialogVersion;
export const canReportDialog = version => version === dialogVersion || !$("form-dialog").open;
export function closeDialog() {
  ++dialogVersion;
  $("form-dialog").close();
  $("dialog-body").replaceChildren();
}

export function dialog(title, fields, { label, submit, danger = false, warning, dismiss = "取消" } = {}) {
  const popup = $("form-dialog");
  const version = ++dialogVersion;
  const close = () => { if (version === dialogVersion) closeDialog(); };
  $("dialog-title").textContent = title;
  $("dialog-body").replaceChildren(...fields);
  $("dialog-error").hidden = true;
  $("dialog-cancel").textContent = dismiss;
  if (warning) inlineError(warning);
  const submitButton = $("dialog-submit");
  submitButton.className = danger ? "danger-solid" : "primary";
  submitButton.textContent = label || "";
  submitButton.hidden = !submit;
  submitButton.disabled = false;
  if (!popup.open) popup.showModal();
  ($("dialog-body").querySelector("input, button") || $("dialog-cancel")).focus();
  $("dialog-form").onsubmit = async event => {
    event.preventDefault();
    if (!submit || submitButton.disabled || !popup.open || version !== dialogVersion) return;
    const form = new FormData(event.currentTarget);
    submitButton.disabled = true;
    try {
      await submit(form, () => canReportDialog(version));
      close();
    } catch (error) {
      if (popup.open && version === dialogVersion) inlineError(error.message);
    } finally {
      if (version === dialogVersion) submitButton.disabled = false;
    }
  };
  return close;
}
