import { $, bytes, create } from "./widgets.js";

class Transfer {
  id = crypto.randomUUID();
  started = performance.now();
  closed = false;

  constructor(label) {
    this.label = label;
    this.caption = create("p", label + " · 准备传输", "meta numeric");
    this.bar = create("progress");
    this.bar.setAttribute("aria-label", label);
    this.row = create("div", null, "transfer");
    this.row.append(this.caption, this.bar);
    this.place();
  }

  place() {
    const host = $($("form-dialog").open ? "dialog-transfers" : "transfers");
    if (this.row.parentNode !== host) host.append(this.row);
  }

  async read() {
    const response = await fetch("/transfer/" + this.id);
    if (!response.ok) throw new Error("无法读取传输进度");
    const report = await response.json();
    if (this.closed) return report;
    this.place();
    if (report?.sample) this.render(report.sample);
    return report;
  }

  render(sample) {
    const { direction, transferred, total, bytes_per_second: rate } = sample;
    const fraction = total ? Math.min(1, transferred / total) : 1;
    this.bar.value = fraction;
    const detail = (direction === "upload" ? "上传" : "下载") + " " + Math.floor(100 * fraction) +
      "% · " + bytes(transferred) + " / " + bytes(total) + " · " + bytes(rate) + "/s";
    this.caption.textContent = this.label === "下载" ? detail : this.label + " · " + detail;
  }

  async poll() {
    while (!this.closed) {
      const report = await this.read();
      if (report?.done) return report;
      if (!report && performance.now() - this.started > 15000) throw new Error("传输未开始");
      await new Promise(resolve => setTimeout(resolve, 200));
    }
  }

  finish(keep) {
    this.closed = true;
    if (keep && this.bar.hasAttribute("value")) setTimeout(() => this.row.remove(), 1800);
    else this.row.remove();
  }
}

export async function withTransfer(label, run) {
  const transfer = new Transfer(label);
  try {
    const pending = run(transfer.id);
    transfer.poll().catch(() => {});
    const result = await pending;
    await transfer.read().catch(() => {});
    transfer.finish(true);
    return result;
  } catch (error) {
    transfer.finish(false);
    throw error;
  }
}

export async function downloadFile(url) {
  const transfer = new Transfer("下载");
  const link = create("a");
  link.href = url + "?download=1&transfer=" + transfer.id;
  link.download = "";
  link.click();
  try {
    const report = await transfer.poll();
    if (report.error) throw new Error(report.error);
    transfer.finish(true);
  } catch (error) {
    transfer.finish(false);
    throw error;
  }
}
