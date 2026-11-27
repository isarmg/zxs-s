import { $, bytes, create } from "./widgets.js";

class Transfer {
  id = crypto.randomUUID();
  closed = false;

  constructor(label) {
    this.label = label;
    this.caption = create("p", label + " · 准备传输", "meta numeric");
    this.bar = create("progress");
    this.bar.setAttribute("aria-label", label);
    this.row = create("div", null, "transfer");
    this.row.append(this.caption, this.bar);
    $("transfers").append(this.row);
  }

  async read() {
    const response = await fetch("/transfer/" + this.id);
    if (!response.ok) return;
    const sample = await response.json();
    if (this.closed) return;
    if (sample) this.render(sample);
  }

  render(sample) {
    const { direction, transferred, total, bytes_per_second: rate } = sample;
    const fraction = total ? Math.min(1, transferred / total) : 1;
    this.bar.value = fraction;
    const detail = (direction === "upload" ? "上传" : "下载") + " " + Math.floor(100 * fraction) +
      "% · " + bytes(transferred) + " / " + bytes(total) + " · " + bytes(rate) + "/s";
    this.caption.textContent = this.label + " · " + detail;
  }

  async poll() {
    while (!this.closed) {
      await this.read().catch(() => {});
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
    void transfer.poll();
    const result = await pending;
    await transfer.read().catch(() => {});
    transfer.finish(true);
    return result;
  } catch (error) {
    transfer.finish(false);
    throw error;
  }
}
