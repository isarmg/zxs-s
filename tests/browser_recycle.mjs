import assert from "node:assert/strict";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { basename, join } from "node:path";
import { test } from "node:test";
import { startFixture, startPrepareFixture } from "./browser_fixture.mjs";

test("an old purge confirmation cannot clear the newly selected server", { timeout: 30_000 }, async t => {
  const first = await startFixture(t);
  const second = await startFixture(t);
  for (const fixture of [first, second]) {
    const [file] = fixture.list("/" + fixture.folder);
    fixture.cli(fixture.mirror, "recycle", "move", file.container_id);
  }

  const page = await first.browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  await page.goto(first.entrance.href, { waitUntil: "domcontentloaded" });
  await button("回收站").click();
  await button("原件.txt").waitFor();
  await button("清空回收站").click();
  first.cli(first.mirror, "login", second.endpoint);

  assert.equal(await page.locator("#server").textContent(), first.endpoint);
  await dialog.getByRole("button", { name: "清空回收站", exact: true }).click();
  await dialog.getByText(/工作区或服务器已切换/).waitFor();
  assert.equal(second.cli(second.mirror, "recycle").entries.length, 1);
  first.cli(first.mirror, "login", first.endpoint);
  assert.equal(first.cli(first.mirror, "recycle").entries.length, 1);
});

test("refreshing to another server closes the old folder confirmation", { timeout: 30_000 }, async t => {
  const first = await startFixture(t);
  const second = await startFixture(t);
  const page = await first.browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  await page.goto(first.entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").click();
  await button("资料").waitFor();
  first.cli(first.mirror, "login", second.endpoint);

  let started;
  let resume;
  const pending = new Promise(resolve => { started = resolve; });
  const gate = new Promise(resolve => { resume = resolve; });
  await page.route("**/api", async request => {
    if (request.request().postDataJSON().action === "session") {
      started();
      await gate;
    }
    await request.continue();
  });
  try {
    await button("刷新").click();
    await pending;
    await button("删除文件夹").click();
    const dialog = page.getByRole("dialog");
    await dialog.getByRole("checkbox").check();
    resume();
    await dialog.waitFor({ state: "hidden" });
    assert.equal(await page.locator("#server").textContent(), second.endpoint);
    assert.equal(second.list("/" + second.folder).length, 1);
    assert.deepEqual(second.cli(second.mirror, "recycle").entries, []);
    first.cli(first.mirror, "login", first.endpoint);
    assert.equal(first.list("/" + first.folder).length, 1);
  } finally {
    resume();
    await page.unroute("**/api");
  }
});

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
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
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

test("a stale sync-delete confirmation cannot recycle a replacement with the same version", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror, source } = fixture;
  const [original] = fixture.list("/" + folder);
  const other = folder + "/另一个文件.txt";
  const path = folder + "/" + original.name;
  await writeFile(join(source, other), "replacement contents");
  const { data: { container: replacement } } = fixture.cli(source, "commit", other);
  assert.equal(original.latest_version, replacement.latest_version);
  fixture.cli(mirror, "checkout", original.container_id);
  await rm(join(mirror, path));

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("工作区").click();
  await button("disk0").click();
  await button("资料").click();
  const row = page.getByRole("row").filter({ has: button(original.name) });
  await row.getByRole("button", { name: "同步删除", exact: true }).click();
  await dialog.getByText(/回收服务器上的 v1/).waitFor();

  fixture.cli(mirror, "untrack", path);
  fixture.cli(mirror, "checkout", replacement.container_id, "--to", path);
  await rm(join(mirror, path));
  await dialog.getByRole("button", { name: "移入回收站", exact: true }).click();
  await dialog.getByRole("alert").getByText(/本地追踪对象已更换/).waitFor();
  assert.deepEqual(new Set(fixture.list("/" + folder).map(file => file.container_id)),
    new Set([original.container_id, replacement.container_id]));
  assert.deepEqual(fixture.cli(mirror, "recycle").entries, []);
  assert.equal(fixture.cli(mirror, "status").entries[0].container_id, replacement.container_id);
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
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
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
  assert.equal(await page.getByRole("columnheader", { name: "原目录" }).count(), 0);
  await row("原件.txt").getByRole("button", { name: "还原", exact: true }).click();
  await dialog.getByRole("textbox", { name: "名称" }).fill("已还原.txt");
  const destination = dialog.getByRole("textbox", { name: "目标目录" });
  assert.equal(await destination.inputValue(), "");
  assert.equal(await destination.getAttribute("required"), "");
  await destination.fill("/disk0/已还原");
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

test("an empty trailing recycle page leaves the remaining entries clearable", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, source, mirror } = fixture;
  await rm(join(mirror, folder, ".zxsignore"));
  await Promise.all(Array.from({ length: 100 }, (_, index) =>
    writeFile(join(mirror, folder, `page-${index}.txt`), "file content")));

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("工作区").click();
  await button("disk0").click();
  await button("资料").click();
  await button("提交当前目录").click();
  await page.getByText("已提交 100 项。", { exact: true }).waitFor();
  fixture.cli(source, "rmdir", `/${folder}`, "--recursive");

  await button("回收站").click();
  const more = button("加载下一页");
  const purge = page.locator("#purge-all");
  const rows = page.locator("#rows tr");
  await more.waitFor();
  assert.equal(await rows.count(), 100);
  const before = fixture.cli(source, "recycle", "ls", "--limit", "200");
  assert.equal(before.entries.length, 101);
  fixture.cli(source, "recycle", "purge", before.entries[100].recycle.recycle_id);

  const tail = page.waitForResponse(response => {
    if (!response.url().endsWith("/api")) return false;
    const request = response.request().postDataJSON();
    return request.action === "recycle_list" && Boolean(request.filter.cursor);
  });
  await more.click();
  const response = await tail;
  assert.deepEqual((await response.json()).entries, []);
  await more.waitFor({ state: "hidden" });
  assert.equal(await rows.count(), 100);
  assert.ok(await purge.isVisible());

  await purge.click();
  await page.getByRole("dialog").getByRole("button", { name: "清空回收站", exact: true }).click();
  await page.getByText("回收站为空。", { exact: true }).waitFor();
  assert.ok(await purge.isHidden());
  assert.deepEqual(fixture.cli(source, "recycle", "ls").entries, []);
});

test("editing the preparation path invalidates both the old plan and a delayed scan", { timeout: 30_000 }, async t => {
  const { root, browser, entrance } = await startPrepareFixture(t);
  const oldDisk = join(root, "old-disk");
  const newDisk = join(root, "new-disk");
  await Promise.all([oldDisk, newDisk].map(path => mkdir(path)));
  await writeFile(join(oldDisk, "old.txt"), "old bytes");
  await writeFile(join(newDisk, "new.txt"), "new bytes");

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  await page.goto(entrance.href);
  const input = page.locator("#root");
  const read = page.locator("#read");
  const workspace = page.locator("#workspace");
  await input.fill(oldDisk);
  await read.click();
  await page.getByText("old.txt", { exact: true }).waitFor();
  await input.fill(newDisk);
  assert.ok(!(await workspace.isVisible()));

  let signalScan;
  let releaseScan;
  const pending = new Promise(resolve => { signalScan = resolve; });
  const gate = new Promise(resolve => { releaseScan = resolve; });
  await page.route("**/api", async route => {
    const response = await route.fetch();
    signalScan();
    await gate;
    await route.fulfill({ response });
  });
  try {
    await read.click();
    await pending;
    await input.evaluate((element, path) => {
      element.value = path;
      element.dispatchEvent(new Event("input", { bubbles: true }));
    }, oldDisk);
    releaseScan();
    await page.waitForFunction(() => !document.getElementById("read").disabled);
    assert.equal(await input.inputValue(), oldDisk);
    assert.ok(!(await workspace.isVisible()));
  } finally {
    releaseScan();
    await page.unroute("**/api");
  }

  await input.fill(newDisk);
  await read.click();
  await page.getByText("new.txt", { exact: true }).waitFor();
  await page.locator("#prepare").click();
  assert.equal(basename(await page.locator("#confirm-root").textContent()), "new-disk");
  await page.getByRole("button", { name: "开始准备", exact: true }).click();
  await page.locator("#notice[data-error=false]").getByText(/磁盘已准备/).waitFor();
  assert.equal(await readFile(join(newDisk, "new.txt/v000001/new.txt"), "utf8"), "new bytes");
  assert.equal(await readFile(join(oldDisk, "old.txt"), "utf8"), "old bytes");
  await assert.rejects(readFile(join(oldDisk, ".zxs/disk.toml")), { code: "ENOENT" });
});

test("reading a different empty disk resets the preparation purpose", { timeout: 30_000 }, async t => {
  const { root, browser, entrance } = await startPrepareFixture(t);
  const first = join(root, "first");
  const second = join(root, "second");
  await Promise.all([first, second].map(path => mkdir(path)));

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  await page.goto(entrance.href);
  const button = name => page.getByRole("button", { name, exact: true });
  const path = page.getByLabel("磁盘目录", { exact: true });
  const data = page.getByRole("radio", { name: "数据盘", exact: true });
  const recycle = page.getByRole("radio", { name: "回收盘", exact: true });
  const prepare = button("准备磁盘");
  await path.fill(first);
  await button("读取目录").click();
  await recycle.check();
  await prepare.click();
  assert.equal(await page.locator("#confirm-detail").textContent(), "为此空目录写入回收盘身份。");
  await button("取消").click();

  await path.fill(second);
  await button("读取目录").click();
  await prepare.waitFor();
  assert.ok(await data.isChecked(), "a new empty disk must default to data");
  await prepare.click();
  assert.equal(basename(await page.locator("#confirm-root").textContent()), "second");
  assert.equal(await page.locator("#confirm-detail").textContent(), "为此空目录写入数据盘身份。");
  await button("开始准备").click();
  await page.locator("#notice[data-error=false]").getByText("磁盘已准备。", { exact: true }).waitFor();
  assert.match(await readFile(join(second, ".zxs/disk.toml"), "utf8"), /^kind = "data"$/m);
  await assert.rejects(readFile(join(first, ".zxs/disk.toml")), { code: "ENOENT" });
});
