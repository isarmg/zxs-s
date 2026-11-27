import assert from "node:assert/strict";
import { readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { startFixture } from "./browser_fixture.mjs";

test("a stale workspace deletion asks again before recycling the newer version", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror, data } = fixture;
  const name = "原件.txt";
  const [original] = fixture.list("/" + folder);
  fixture.cli(mirror, "checkout", original.container_id);
  await rm(join(mirror, folder, name));
  await fixture.updateOriginal("updated from another workspace");

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const row = page.getByRole("row").filter({ has: button(name) });
  const dialog = page.getByRole("dialog");
  await page.goto(entrance.href);
  await button("工作区").click();
  await button("disk0").click();
  await button("资料").click();
  await row.getByRole("button", { name: "同步删除", exact: true }).click();
  await dialog.getByText(/回收服务器上的 v1/).waitFor();
  await dialog.getByRole("button", { name: "移入回收站", exact: true }).click();
  await dialog.getByText(/服务器上已有更新的 v2/).waitFor();
  await dialog.getByRole("button", { name: "取消", exact: true }).click();
  await dialog.waitFor({ state: "hidden" });
  await row.getByRole("button", { name: "同步删除", exact: true }).waitFor();

  assert.equal(fixture.list("/" + folder)[0].latest_version, 2);
  assert.deepEqual(fixture.cli(mirror, "recycle", "ls").entries, []);
  assert.equal(await readFile(join(data, folder, name, "v000002", name), "utf8"),
    "updated from another workspace");
  const [tracked] = fixture.cli(mirror, "status").entries;
  assert.deepEqual([tracked.path, tracked.state, tracked.version], [folder + "/" + name, "missing", 1]);
});

test("the recycle view restores history and confirms single and bulk purge", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror, source, data } = fixture;
  await fixture.updateOriginal("published second version");
  const archivedNames = ["原件.txt", "单独删除.txt", "批量清空.txt"];
  for (const name of archivedNames.slice(1)) {
    await writeFile(join(source, folder, name), name);
    fixture.cli(source, "commit", folder + "/" + name);
  }
  const original = fixture.list("/" + folder).find(item => item.name === "原件.txt");

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const row = name => page.getByRole("row").filter({ has: button(name) });
  const details = page.locator("#details");
  const dialog = page.getByRole("dialog");
  const confirm = async name => {
    await dialog.getByRole("button", { name, exact: true }).click();
    await dialog.waitFor({ state: "hidden" });
  };
  await page.goto(entrance.href);
  await button("disk0").click();
  await button("资料").click();
  await row("原件.txt").getByRole("button", { name: "原件.txt", exact: true }).click();
  await details.getByRole("heading", { name: "历史版本", exact: true }).waitFor();
  assert.equal(await details.getByRole("button", { name: "移动或重命名", exact: true }).count(), 0);
  assert.equal(await details.getByRole("button", { name: "移入回收站", exact: true }).count(), 0);
  const recycle = async name => {
    await row(name).getByRole("button", { name: "移入回收站", exact: true }).click();
    await confirm("移入回收站");
    await row(name).waitFor({ state: "hidden" });
  };
  await recycle("单独删除.txt");
  assert.ok(await details.getByRole("heading", { name: "原件.txt", exact: true }).isVisible());
  await recycle("原件.txt");
  await details.waitFor({ state: "hidden" });
  await recycle("批量清空.txt");
  assert.deepEqual(fixture.list("/" + folder), []);
  await button("回收站").click();
  await row("原件.txt").getByRole("button", { name: "还原", exact: true }).click();
  await dialog.getByRole("textbox", { name: "名称" }).fill("已还原.txt");
  await dialog.getByRole("textbox", { name: "目录" }).fill("/disk0/已还原");
  await confirm("还原");
  await row("原件.txt").waitFor({ state: "hidden" });
  const [restored] = fixture.list("/disk0/已还原");
  assert.deepEqual([restored.container_id, restored.name, restored.latest_version],
    [original.container_id, "已还原.txt", 2]);
  const restoredPath = join(data, "disk0/已还原/已还原.txt");
  assert.equal(await readFile(join(restoredPath, "v000001/原件.txt"), "utf8"), "published content");
  assert.equal(await readFile(join(restoredPath, "v000002/原件.txt"), "utf8"), "published second version");

  await row("单独删除.txt").getByRole("button", { name: "彻底删除", exact: true }).click();
  await confirm("彻底删除");
  await row("单独删除.txt").waitFor({ state: "hidden" });
  await row("批量清空.txt").waitFor();
  await button("清空回收站").click();
  await confirm("取消");
  assert.deepEqual(fixture.cli(mirror, "recycle", "ls").entries.map(entry => entry.container.name), ["批量清空.txt"]);

  await button("清空回收站").click();
  await confirm("清空回收站");
  await page.getByText("回收站为空。", { exact: true }).waitFor();
  assert.deepEqual(fixture.cli(mirror, "recycle", "ls").entries, []);
  assert.equal(fixture.list("/disk0/已还原")[0].container_id, original.container_id);
});
