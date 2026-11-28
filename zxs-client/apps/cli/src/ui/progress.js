import { $, bytes, create } from "./widgets.js";

export async function withTransfer(label, run) {
  const id = crypto.randomUUID();
  const percent = create("span", label + " · 准备传输");
  const speed = create("span");
  const summary = create("p", null, "transfer-summary");
  summary.append(percent, speed);
  const size = create("p");
  const bar = create("progress");
  bar.setAttribute("aria-label", label);
  const row = create("div", null, "transfer");
  row.append(summary, size, bar);
  $("transfers").append(row);
  let finished = false;
  let timer;
  let keep = false;

  async function sample() {
    try {
      const response = await fetch("/transfer/" + id);
      const data = response.ok && await response.json();
      if (finished || !data) return;
      const { direction, transferred, total, bytes_per_second: rate } = data;
      const fraction = total ? Math.min(1, transferred / total) : 1;
      bar.value = fraction;
      percent.textContent = (direction === "upload" ? "上传 " : "下载 ") + Math.floor(100 * fraction) + "%";
      speed.textContent = bytes(rate) + "/s";
      size.textContent = bytes(transferred) + " / " + bytes(total);
    } catch { /* The operation response owns errors. */ }
  }

  async function poll() {
    await sample();
    if (!finished) timer = setTimeout(poll, 200);
  }

  try {
    const pending = run(id);
    void poll();
    const result = await pending;
    await sample();
    keep = bar.hasAttribute("value");
    return result;
  } finally {
    finished = true;
    clearTimeout(timer);
    if (keep) setTimeout(() => row.remove(), 1800);
    else row.remove();
  }
}
