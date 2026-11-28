import assert from "node:assert/strict";
import { chmod, mkdir, readFile, rename, rm, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { startFixture } from "./browser_fixture.mjs";

const fileDetails = (page, name) => page.getByRole("complementary", { name, exact: true });

test("workspace submission and restoration show progress and verify file contents", { timeout: 30_000 }, async t => {
  const { browser, entrance, folder, mirror, data } = await startFixture(t, { slowDownloads: true });
  const contents = Buffer.alloc(6 * 1024 * 1024, 42);
  const name = "传输测试.bin";
  const local = join(mirror, folder, name);
  await rm(join(mirror, folder, ".zxsignore"));
  await writeFile(local, contents);
  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const row = name => page.locator("#rows").getByRole("button", { name, exact: true });
  const sidebar = page.locator(".sidebar");
  const transfers = sidebar.locator(".transfer");
  const assertSidebarStatus = async (item, aboveConnection = false) => {
    await item.waitFor();
    const fits = await item.evaluate((element, above) => {
      const panel = element.closest(".sidebar");
      const bounds = panel.getBoundingClientRect();
      const connection = panel.querySelector(".connection").getBoundingClientRect();
      const box = element.getBoundingClientRect();
      const center = document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2);
      const besideConnection = above ? box.bottom <= connection.top : box.top >= connection.bottom;
      return box.left >= bounds.left && box.right <= bounds.right &&
        box.top >= bounds.top && box.bottom <= bounds.bottom && besideConnection && element.contains(center);
    }, aboveConnection);
    assert.ok(fits, "Status should be visible on the expected side of the connection");
  };
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("工作区").click();
  await row("disk0").click();
  await row("资料").click();
  await page.getByRole("row").filter({ has: button(name) }).getByRole("button", { name: "提交" }).click();
  const upload = transfers.filter({ hasText: /上传 100%/ });
  await assertSidebarStatus(upload, true);
  assert.match(await upload.locator("p").first().textContent(), /上传 100%.*\/s/);
  assert.equal(await upload.locator("p").nth(1).textContent(), "6.0 MiB / 6.0 MiB");
  assert.ok(await upload.evaluate(row => [row, row.querySelector("progress")].every(item => {
    const style = getComputedStyle(item);
    return style.borderTopWidth === "0px" && style.boxShadow === "none";
  })));
  await sidebar.getByText("已提交 v1。", { exact: true }).waitFor();
  await button("提交当前目录").click();
  const clean = sidebar.getByRole("status").filter({ hasText: "没有待提交的变更。" });
  await assertSidebarStatus(clean);
  await clean.getByRole("button", { name: "关闭提示" }).click();
  await clean.waitFor({ state: "hidden" });
  await rm(local);

  await button("文件").click();
  await row("disk0").click();
  await row("资料").click();
  await row(name).click();
  const restore = button("恢复到工作区");
  await restore.waitFor();
  const { width: detailWidth } = await page.locator("#details").boundingBox();
  const { width: sidebarWidth } = await sidebar.boundingBox();
  assert.ok(Math.abs(detailWidth - sidebarWidth) < 1,
    "The desktop navigation and file detail panes should be equally wide");
  assert.equal(await button("检出并打开").count(), 0);
  assert.equal(await button("下载").count(), 0);
  await page.setViewportSize({ width: 480, height: 760 });
  await restore.click();
  await assertSidebarStatus(transfers.filter({ hasText: /下载 [1-9]\d?%.*\/s/ }));
  await transfers.filter({ hasText: /下载 100%.*\/s/ }).waitFor();
  await sidebar.getByText(/已恢复 v1 到工作区/).waitFor();
  await page.setViewportSize({ width: 1280, height: 720 });
  assert.deepEqual(await readFile(local), contents);

  const payload = join(data, folder, name, "v000001", name);
  await chmod(payload, 0o644);
  await writeFile(payload, Buffer.alloc(contents.length, 43));
  await chmod(payload, 0o444);
  await rm(local);
  const checkout = page.waitForResponse(response =>
    response.url().endsWith("/api") && response.request().postDataJSON().action === "checkout");
  await restore.click();
  await page.getByRole("dialog").getByText(/BLAKE3/).waitFor();
  await assert.rejects(stat(local), { code: "ENOENT" });
  const transfer = (await checkout).request().headers()["x-zxs-transfer"];
  const report = await page.request.get(new URL("/transfer/" + transfer, entrance).href);
  const progress = await report.json();
  assert.ok(progress.transferred < progress.total);
});

test("desktop removes page titles and shows only version history in file details", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror, source } = fixture;
  const archive = "压缩包.zip";
  const files = ["图片.png", "视频.mp4", "音频.aac", archive];
  await rm(join(mirror, folder, ".zxsignore"));
  for (const name of files) {
    const contents = name === archive ? Buffer.from("UEsFBgAAAAAAAAAAAAAAAAAAAAAAAA==", "base64") : `original ${name}`;
    await writeFile(join(source, folder, name), contents);
    fixture.cli(source, "commit", `${folder}/${name}`);
  }
  await writeFile(join(source, folder, files[0]), "second version");
  fixture.cli(source, "commit", `${folder}/${files[0]}`);

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  for (const name of ["工作区", "回收站", "文件"]) {
    await button(name).click();
    await page.getByRole("table", { name, exact: true }).waitFor();
    assert.equal(await page.locator("h1, #title").count(), 0);
  }
  await button("disk0").click();
  await button("资料").click();

  for (const name of [...files, "原件.txt"]) {
    await button(name).click();
    const detail = fileDetails(page, name);
    await detail.getByRole("heading", { name: "历史版本", level: 2, exact: true }).waitFor();
    assert.equal(await detail.locator("#detail-name").count(), 0);
    assert.deepEqual(await detail.getByRole("heading").allTextContents(), ["历史版本"]);
    assert.equal(await detail.getByText(name, { exact: true }).count(), 0);
    assert.equal(await detail.getByText(`/${folder}`, { exact: true }).count(), 0);
    assert.equal(await detail.getByRole("button", { name: "预览原件", exact: true }).count(), 0);
    assert.equal(await detail.locator("img, video, audio, object, embed").count(), 0);
    const history = detail.locator("#detail-content");
    const versions = name === files[0] ? ["v2", "v1"] : ["v1"];
    assert.equal(await history.locator(":scope > *").count(), 1);
    assert.deepEqual(await history.locator(".version-meta > .badge:first-child").allTextContents(), versions);
    const restore = history.getByRole("button");
    assert.deepEqual(await restore.allTextContents(), versions.map(() => "恢复到工作区"));
    if (name !== files[0]) continue;
    await restore.last().click();
    await page.getByText(/已恢复 v1 到工作区/).waitFor();
    assert.equal(await readFile(join(mirror, folder, name), "utf8"), `original ${name}`);
  }
});

test("the next file page replaces a renamed container without duplicating it", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, source, mirror } = fixture;
  await rm(join(mirror, folder, ".zxsignore"));
  await Promise.all(Array.from({ length: 101 }, (_, index) => {
    const name = "plan-" + String(index).padStart(3, "0") + ".txt";
    return writeFile(join(mirror, folder, name), "file contents");
  }));
  fixture.cli(source, "mkdir", `/${folder}/分页目录`);

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const rows = page.locator("#rows tr");
  async function openFolder(view) {
    await button(view).click();
    await button("disk0").click();
    await button("资料").click();
  }
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await openFolder("工作区");
  await button("提交当前目录").click();
  await page.getByText("已提交 101 项。", { exact: true }).waitFor();
  await openFolder("文件");
  const more = button("加载下一页");
  await more.waitFor();
  assert.equal(await rows.count(), 101);
  await button("分页目录").waitFor();

  const original = "plan-000.txt";
  const renamed = "renamed-plan.txt";
  const id = await rows.filter({ has: button(original) }).getAttribute("data-row-key");
  await button(original).click();
  await fileDetails(page, original).waitFor();
  fixture.cli(source, "patch", id, "--name", renamed);

  const { response, release } = await holdApiResponse(page, request => request.action === "list" && request.filter.cursor);
  try {
    await more.click();
    assert.equal(await response, 200);
    const unchanged = await button("plan-001.txt").elementHandle();
    await unchanged.focus();
    release();
    await button(renamed).waitFor();
    assert.equal(await page.locator(`#rows tr[data-row-key="${id}"]`).count(), 1);
    assert.equal(await button(original).count(), 0);
    assert.equal(await button(renamed).getAttribute("aria-current"), "true");
    assert.equal(await rows.count(), 103);
    assert.equal(await button("分页目录").count(), 1);
    assert.deepEqual((await rows.locator(".file-link").allTextContents()).slice(-3),
      ["plan-100.txt", renamed, "原件.txt"]);
    assert.ok(await unchanged.evaluate(node => node.isConnected && node === document.activeElement));
  } finally {
    release();
    await page.unrouteAll({ behavior: "wait" });
  }
});

function observeApi(page) {
  const requests = [];
  page.on("request", request => {
    if (request.url().endsWith("/api")) requests.push(request.postDataJSON());
  });
  return action => requests.filter(request => request.action === action);
}

test("a workspace switch stops submission from a stale tab", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror, root } = fixture;
  const other = join(root, "第二个工作区");
  const path = folder + "/同名.txt";
  const name = "同名.txt";
  await mkdir(join(other, folder), { recursive: true });
  await rm(join(mirror, folder, ".zxsignore"));
  await writeFile(join(mirror, path), "first workspace");
  await writeFile(join(other, path), "second workspace");

  const context = await browser.newContext();
  context.setDefaultTimeout(8_000);
  const first = await context.newPage();
  const second = await context.newPage();
  const button = (page, name) => page.getByRole("button", { name, exact: true });
  await first.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button(first, "工作区").click();
  await button(first, "disk0").click();
  await button(first, "资料").click();
  const submit = first.getByRole("row").filter({ has: button(first, name) })
    .getByRole("button", { name: "提交", exact: true });
  await submit.waitFor();

  await second.goto(entrance.origin, { waitUntil: "domcontentloaded" });
  await button(second, "工作区").click();
  await button(second, "选择工作区").click();
  await second.getByRole("textbox", { name: "工作区目录" }).fill(other);
  await button(second, "保存").click();
  await button(second, "第二个工作区").waitFor();

  await submit.click();
  await first.getByRole("dialog").getByText(/工作区或服务器已切换/).waitFor();
  assert.ok(!fixture.list("/" + folder).some(item => item.name === name));
  assert.equal(await readFile(join(mirror, path), "utf8"), "first workspace");
  assert.equal(await readFile(join(other, path), "utf8"), "second workspace");
  await first.getByRole("dialog").getByRole("button", { name: "关闭", exact: true }).click();
  await first.locator("#refresh").click();
  await button(first, "第二个工作区").waitFor();
});

test("refresh updates the selected file's current version and closes removed details", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror } = fixture;
  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const name = "原件.txt";
  const details = page.locator("#details");
  const current = details.locator(".version[data-current=true]");
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").click();
  await button("资料").click();
  await button(name).click();
  await current.getByText("v1", { exact: true }).waitFor();

  await fixture.updateOriginal("updated version");
  await page.locator("#refresh").click();
  await current.getByText("v2", { exact: true }).waitFor();
  await current.getByRole("button", { name: "恢复到工作区", exact: true }).click();
  await page.getByText(/已恢复 v2 到工作区/).waitFor();
  assert.equal(await readFile(join(mirror, folder, name), "utf8"), "updated version");

  const [file] = fixture.list("/" + folder);
  fixture.cli(mirror, "recycle", "move", file.container_id);
  await page.locator("#refresh").click();
  await details.waitFor({ state: "hidden" });
  assert.ok(await page.getByRole("dialog").isHidden());
});

test("refresh clears a directory and its details after the directory is moved elsewhere", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, source } = fixture;
  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const details = page.locator("#details");
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").click();
  await button("资料").click();
  await button("原件.txt").click();
  await fileDetails(page, "原件.txt").waitFor();
  fixture.cli(source, "mv-folder", "/" + folder, "/disk0/资料归档");
  await button("刷新").click();

  const dialog = page.getByRole("dialog");
  await dialog.getByRole("alert").waitFor();
  await dialog.getByRole("button", { name: "关闭", exact: true }).click();
  assert.ok(await button("原件.txt").isHidden());
  assert.ok(await details.isHidden());
  assert.equal(await button("根目录").getAttribute("aria-current"), "location");
  await button("disk0").click();
  await button("资料归档").click();
  const path = page.getByRole("navigation", { name: "当前路径" });
  assert.equal(await path.getByRole("button", { name: "资料归档", exact: true }).getAttribute("aria-current"), "location");
  await button("原件.txt").click();
  await fileDetails(page, "原件.txt").waitFor();
  await details.getByRole("button", { name: "恢复到工作区", exact: true }).waitFor();
});

test("renaming a file keeps a concurrent move to another disk", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t, { secondDisk: true });
  const { browser, entrance, folder, source } = fixture;
  const [file] = fixture.list("/" + folder);
  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const requests = observeApi(page);
  const button = name => page.getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  const save = async () => {
    await dialog.getByRole("button", { name: "保存", exact: true }).click();
    await dialog.waitFor({ state: "hidden" });
  };
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").click();
  await button("资料").click();
  await button("移动或重命名文件").click();
  await save();
  assert.deepEqual(requests("patch"), []);

  await button("移动或重命名文件").click();
  assert.equal(await dialog.getByRole("textbox", { name: "目录路径" }).inputValue(), "/" + folder);
  const destination = "/disk1/已移动";
  fixture.cli(source, "patch", file.container_id, "--folder", destination);
  const name = "只改名.txt";
  await dialog.getByRole("textbox", { name: "名称" }).fill(name);
  await save();
  assert.deepEqual(requests("patch"), [{ action: "patch", id: file.container_id, patch: { name } }]);
  assert.deepEqual(fixture.list("/" + folder), []);
  const [renamed] = fixture.list(destination);
  assert.deepEqual([renamed.container_id, renamed.name, renamed.latest_version], [file.container_id, name, 1]);
});

test("refresh preserves the newer file selection while its details are loading", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, source } = fixture;
  const name = "第二个文件.txt";
  await writeFile(join(source, folder, name), "new file");
  const { data: { container } } = fixture.cli(source, "commit", folder + "/" + name);
  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const details = page.locator("#details");
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").click();
  await button("资料").click();
  await button("原件.txt").click();
  await fileDetails(page, "原件.txt").waitFor();

  let captured;
  let resume;
  const pending = new Promise(resolve => { captured = resolve; });
  const gate = new Promise(resolve => { resume = resolve; });
  const requested = [];
  await page.route("**/api", async route => {
    const { action, id } = route.request().postDataJSON();
    if (action === "detail") requested.push(id);
    if (action !== "detail" || id !== container.container_id) return route.continue();
    const response = await route.fetch();
    captured();
    await gate;
    await route.fulfill({ response });
  });
  try {
    await button(name).click();
    await pending;
    await details.waitFor({ state: "hidden" });
    await button("刷新").click();
    await page.waitForFunction(() => !document.getElementById("refresh").disabled);
    resume();
    await fileDetails(page, name).waitFor();
    assert.deepEqual(requested, [container.container_id]);
  } finally {
    resume();
    await page.unrouteAll({ behavior: "wait" });
  }
});

test("saving an unchanged folder leaves the local mirror untouched but a case-only rename still runs", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, source, mirror } = fixture;
  const name = "Reports";
  const path = `disk0/${name}`;
  fixture.cli(source, "mv-folder", `/${folder}`, `/${path}`);
  const [file] = fixture.list(`/${path}`);
  fixture.cli(mirror, "checkout", file.container_id);
  const original = join(mirror, path, file.name);
  const renamed = "远端改名.txt";
  fixture.cli(source, "patch", file.container_id, "--name", renamed);

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = label => page.getByRole("button", { name: label, exact: true });
  const dialog = page.getByRole("dialog");
  const requests = observeApi(page);
  const edit = () => page.getByRole("row").filter({ has: button(name) })
    .getByRole("button", { name: "移动或重命名文件夹", exact: true }).click();
  const save = () => dialog.getByRole("button", { name: "保存", exact: true }).click();
  const nameField = dialog.getByRole("textbox", { name: "名称" });
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").click();
  await edit();
  assert.equal(await nameField.inputValue(), name);
  assert.equal(await dialog.getByRole("textbox", { name: "目录路径" }).inputValue(), "/disk0");
  await save();
  await dialog.waitFor({ state: "hidden" });
  assert.deepEqual(requests("move_folder"), []);
  assert.equal(await readFile(original, "utf8"), "published content");
  await assert.rejects(stat(join(mirror, path, renamed)), { code: "ENOENT" });

  await edit();
  const upper = "REPORTS";
  await nameField.fill(upper);
  const changed = page.waitForResponse(response => response.url().endsWith("/api") &&
    response.request().postDataJSON().action === "move_folder");
  await save();
  const response = await changed;
  assert.equal(response.status(), 200);
  assert.equal((await response.json()).folder, `/disk0/${upper}`);
  assert.deepEqual(requests("move_folder"), [{ action: "move_folder", from: `/${path}`, name: upper, folder: "/disk0" }]);
  const [moved] = fixture.list(`/disk0/${upper}`);
  assert.deepEqual([moved.container_id, moved.folder, moved.name], [file.container_id, `/disk0/${upper}`, renamed]);
});

async function checkLateFolderMove(t, changeWorkspace) {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror, root } = fixture;
  const [file] = fixture.list("/" + folder);
  fixture.cli(mirror, "checkout", file.container_id);
  const local = join(mirror, folder, file.name);
  await writeFile(local, "unsubmitted local edits");
  const other = join(root, "第二个工作区");
  await mkdir(join(other, "disk0"), { recursive: true });
  await writeFile(join(other, "disk0", "保留.txt"), "independent data");

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const row = name => page.getByRole("row").filter({ has: button(name) });
  const dialog = page.getByRole("dialog");
  const requests = observeApi(page);
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").click();
  await row("资料").getByRole("button", { name: "移动或重命名文件夹", exact: true }).click();
  await dialog.getByRole("textbox", { name: "名称" }).fill("已移动");
  const finish = await watchDialogSubmit(page);
  const { response, release } = await holdApiResponse(page, request => request.action === "move_folder");
  try {
    await dialog.getByRole("button", { name: "保存", exact: true }).click();
    assert.equal(await response, 200);
    assert.equal(fixture.list("/disk0/已移动")[0].container_id, file.container_id);
    assert.equal(await readFile(local, "utf8"), "unsubmitted local edits");

    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "hidden" });
    await button("工作区").click();
    await row("disk0").waitFor();
    await button("选择工作区").click();
    const path = dialog.getByRole("textbox", { name: "工作区目录" });
    if (changeWorkspace) {
      await path.fill(other);
      await dialog.getByRole("button", { name: "保存", exact: true }).click();
      await button("第二个工作区").waitFor();
      await row("disk0").waitFor();
      await button("选择工作区").click();
    }
    const draft = join(root, "未保存的选择");
    await path.fill(draft);
    const listings = requests("browse_workspace").length;
    release();
    await finish();
    assert.equal(await dialog.getByRole("heading").textContent(), "选择工作区");
    assert.equal(await path.inputValue(), draft);
    assert.equal(requests("browse_workspace").length, listings + (changeWorkspace ? 0 : 1));
  } finally {
    release();
    await page.unrouteAll({ behavior: "wait" });
  }
}

test("a late folder move preserves the new workspace's open dialog", { timeout: 30_000 },
  t => checkLateFolderMove(t, true));
test("a late folder move preserves a newer dialog in the same workspace", { timeout: 30_000 },
  t => checkLateFolderMove(t, false));

test("disconnect clears the local connection even when the server is offline", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, root } = fixture;
  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").waitFor();
  await fixture.stopServer();
  await button("断开连接").click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("button", { name: "断开连接", exact: true }).click();
  await dialog.waitFor({ state: "hidden" });
  await button("连接服务器").waitFor();
  assert.equal(await page.locator("#server").textContent(), "尚未连接");
  await assert.rejects(stat(join(root, "profile", "profile.json")), { code: "ENOENT" });
});

test("an outstanding list cannot restore remote rows after disconnect", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const page = await fixture.browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  await page.goto(fixture.entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").click();
  await button("资料").click();
  await button("原件.txt").waitFor();

  let captured;
  let resume;
  const pending = new Promise(resolve => { captured = resolve; });
  const gate = new Promise(resolve => { resume = resolve; });
  await page.route("**/api", async route => {
    if (route.request().postDataJSON().action !== "list") return route.continue();
    const response = await route.fetch();
    captured(response.status());
    await gate;
    await route.fulfill({ response });
  });
  try {
    await button("刷新").click();
    assert.equal(await pending, 200);
    await button("断开连接").click();
    await page.getByRole("dialog").getByRole("button", { name: "断开连接", exact: true }).click();
    await button("连接服务器").waitFor();
    resume();
    await page.waitForFunction(() => !document.querySelector("#refresh").disabled);
    assert.equal(await page.locator("#server").textContent(), "尚未连接");
    assert.ok(await button("原件.txt").isHidden());
    assert.equal(await page.locator("#table").getAttribute("aria-busy"), null);
  } finally {
    resume();
    await page.unroute("**/api");
  }
});

test("cancelling login after a password reset leaves the account disconnected", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const page = await fixture.browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  await page.goto(fixture.entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").waitFor();
  await fixture.restart("different-fixture-password");
  await button("刷新").click();
  await dialog.getByRole("heading", { name: "连接服务器", exact: true }).waitFor();
  assert.equal(await dialog.getByRole("textbox", { name: "服务器地址" }).inputValue(), fixture.endpoint);
  await dialog.getByRole("button", { name: "取消", exact: true }).click();
  assert.equal(await page.locator("#server").textContent(), "尚未连接");
  await button("连接服务器").click();
  await dialog.getByRole("textbox", { name: "服务器地址" }).fill(fixture.endpoint);
  await dialog.getByLabel("口令").fill("different-fixture-password");
  await dialog.getByRole("button", { name: "登录", exact: true }).click();
  await button("disk0").waitFor();
});

test("saving unchanged tags preserves tags added elsewhere", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, source } = fixture;
  const [file] = fixture.list("/" + folder);
  fixture.cli(source, "patch", file.container_id, "--tags", "原有");

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const requests = observeApi(page);
  const button = name => page.getByRole("button", { name, exact: true });
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").click();
  await button("资料").click();
  await button(file.name).click();
  await page.locator("#edit-tags").click();
  const dialog = page.getByRole("dialog");
  assert.equal(await dialog.locator("input[name=tags]").inputValue(), "原有");

  fixture.cli(source, "patch", file.container_id, "--tags", "原有,另一端新增");
  await dialog.getByRole("button", { name: "保存", exact: true }).click();
  await dialog.waitFor({ state: "hidden" });
  assert.deepEqual(requests("patch"), []);
  assert.deepEqual(new Set(fixture.list("/" + folder)[0].tags), new Set(["原有", "另一端新增"]));
});

test("desktop tag filtering keeps saved tags with and without spaces distinct", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, source } = fixture;
  const [original] = fixture.list("/" + folder);
  const tag = "发版";
  const spacedTag = tag + " ";
  fixture.cli(source, "patch", original.container_id, "--tags", spacedTag);
  const other = "另一份.txt";
  const path = folder + "/" + other;
  await writeFile(join(source, path), "independent content");
  fixture.cli(source, "commit", path, "--tag", tag);

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const rows = page.locator("#rows tr");
  const filter = page.getByRole("textbox", { name: "按标签筛选" });
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").waitFor();
  for (const [label, included, excluded] of [[spacedTag, original.name, other], [tag, other, original.name]]) {
    const listing = page.waitForResponse(result => {
      if (!result.url().endsWith("/api")) return false;
      const request = result.request().postDataJSON();
      return request.action === "list" && request.filter.tag !== null;
    });
    await filter.fill(label);
    assert.equal((await listing).request().postDataJSON().filter.tag, label);
    await button(included).waitFor();
    assert.equal(await rows.count(), 1);
    assert.equal(await button(excluded).count(), 0);
  }
  await filter.fill("");
  await button("disk0").waitFor();
  await page.getByRole("searchbox", { name: "搜索文件名" }).fill(" 原件 ");
  await button(original.name).waitFor();
  assert.equal(await button(other).count(), 0);
});

test("saving tags preserves existing punctuation and allows removal", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, source } = fixture;
  const name = "标签.txt";
  const tag = "资料，审阅";
  const path = folder + "/" + name;
  await writeFile(join(source, path), "tagged content");
  fixture.cli(source, "commit", path, "--tag", tag);
  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  const save = async () => {
    await dialog.getByRole("button", { name: "保存", exact: true }).click();
    await dialog.waitFor({ state: "hidden" });
  };
  const tags = () => fixture.list("/" + folder).find(item => item.name === name).tags;
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").click();
  await button("资料").click();
  await button(name).click();
  await page.locator("#edit-tags").click();
  const existing = dialog.getByRole("checkbox", { name: tag, exact: true });
  assert.ok(await existing.isChecked());
  await dialog.locator("input[name=tags]").fill("备忘");
  await save();
  assert.deepEqual(new Set(tags()), new Set([tag, "备忘"]));
  await page.locator("#edit-tags").click();
  await existing.uncheck();
  await save();
  assert.deepEqual(tags(), ["备忘"]);
});

test("synchronizing a batch reports both updated files and retained local changes", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror, source } = fixture;
  const [first] = fixture.list("/" + folder);
  const secondPath = folder + "/第二个文件.txt";
  await writeFile(join(source, secondPath), "before sync");
  const { data: second } = fixture.cli(source, "commit", secondPath);
  for (const id of [first.container_id, second.container.container_id]) fixture.cli(mirror, "checkout", id);
  await writeFile(join(mirror, folder, first.name), "unsaved local change");
  await fixture.updateOriginal("other server version");
  await writeFile(join(source, secondPath), "after sync");
  fixture.cli(source, "commit", secondPath);

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("工作区").click();
  await button("同步已追踪").click();
  await page.getByText("已同步 1 项到本地工作区，已保留 1 项本地修改。", { exact: true }).waitFor();
  assert.equal(await readFile(join(mirror, folder, first.name), "utf8"), "unsaved local change");
  assert.equal(await readFile(join(mirror, secondPath), "utf8"), "after sync");
});

async function checkLateSync(t, failSync, changeWorkspace = true) {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror, root } = fixture;
  const [file] = fixture.list("/" + folder);
  const synchronized = "contents synchronized in the first workspace";
  fixture.cli(mirror, "checkout", file.container_id);
  if (!failSync) await fixture.updateOriginal(synchronized);
  const other = join(root, "第二个工作区");
  const otherFile = join(other, "disk0", "保持.txt");
  await mkdir(join(other, "disk0"), { recursive: true });
  await writeFile(otherFile, "independent content");

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const requests = observeApi(page);
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("工作区").click();
  const sync = button("同步已追踪");
  const disk = page.locator("#rows").getByRole("button", { name: "disk0", exact: true });
  const dialog = page.getByRole("dialog");
  const warning = page.locator("#bundle-warning");
  await disk.waitFor();
  if (failSync) {
    await fixture.stopServer();
    await sync.click();
    await dialog.getByRole("heading", { name: "操作失败", exact: true }).waitFor();
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "hidden" });
  }
  const { response, release } = await holdApiResponse(page, request => request.action === "sync");
  try {
    await sync.click();
    assert.equal(await response, failSync ? 502 : 200);
    assert.equal(await readFile(join(mirror, folder, file.name), "utf8"), failSync ? "published content" : synchronized);
    await button("选择工作区").click();
    const path = dialog.getByRole("textbox", { name: "工作区目录" });
    const draft = join(root, "尚未保存的草稿");
    if (changeWorkspace) {
      await path.fill(other);
      await button("保存").click();
      await button("第二个工作区").waitFor();
      await disk.waitFor();
      assert.ok(await warning.isHidden());
    } else await path.fill(draft);
    const listings = requests("browse_workspace").length;
    release();
    await page.waitForFunction(() => !document.getElementById("sync").hasAttribute("aria-busy"));
    if (changeWorkspace) {
      assert.equal(await page.locator("#form-dialog").getAttribute("open"), null,
        "a sync error from the old workspace must not open a dialog");
      assert.ok(await warning.isHidden());
    } else {
      assert.equal(await path.inputValue(), draft);
      assert.ok(await dialog.getByRole("alert").isHidden());
    }
    assert.ok(await page.locator("#notice").isHidden());
    assert.equal(requests("browse_workspace").length, listings);
    assert.equal(await readFile(otherFile, "utf8"), "independent content");
  } finally {
    release();
    await page.unrouteAll({ behavior: "wait" });
  }
}

test("a late sync response leaves the new workspace's warnings and list alone", { timeout: 30_000 },
  t => checkLateSync(t, false));
test("a late sync failure does not open a dialog in the new workspace", { timeout: 30_000 },
  t => checkLateSync(t, true));
test("a late sync failure does not add an error to a newer form in the same workspace", { timeout: 30_000 },
  t => checkLateSync(t, true, false));

test("a late sync report with file issues leaves a newer form alone", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror, root, source } = fixture;
  const [file] = fixture.list("/" + folder);
  const local = join(mirror, folder, file.name);
  fixture.cli(mirror, "checkout", file.container_id);
  await writeFile(local, "unsubmitted local edits");
  fixture.cli(source, "patch", file.container_id, "--name", "远端改名.txt");

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  const alert = dialog.getByRole("alert");
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("工作区").click();
  const sync = button("同步已追踪");
  await sync.click();
  const issue = `${folder}/${file.name} — 内容已改变，请先提交或备份再移动`;
  await alert.getByText(issue, { exact: true }).waitFor();
  await page.keyboard.press("Escape");
  await dialog.waitFor({ state: "hidden" });

  const { response, release } = await holdApiResponse(page, request => request.action === "sync",
    async result => ({ status: result.status(), body: await result.json() }));
  try {
    await sync.click();
    const { status, body } = await response;
    assert.equal(status, 200);
    assert.equal(body.issues[0].path, folder + "/" + file.name);
    await button("选择工作区").click();
    const path = dialog.getByRole("textbox", { name: "工作区目录" });
    const draft = join(root, "尚未保存的草稿");
    await path.fill(draft);
    release();
    await page.waitForFunction(() => !document.getElementById("sync").hasAttribute("aria-busy"));
    assert.equal(await path.inputValue(), draft);
    assert.ok(await alert.isHidden());
    assert.ok(await page.locator("#notice").isHidden());
    assert.equal(await readFile(local, "utf8"), "unsubmitted local edits");
  } finally {
    release();
    await page.unrouteAll({ behavior: "wait" });
  }
});

test("workspace synchronization reports missing remote files from the parent view", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror, source } = fixture;
  const [file] = fixture.list("/" + folder);
  const path = `${folder}/${file.name}`;
  fixture.cli(mirror, "checkout", file.container_id);
  fixture.cli(source, "recycle", "move", file.container_id);
  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  const requests = observeApi(page);
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("工作区").click();
  await button("同步已追踪").click();
  await page.getByText("1 项远端不可用。", { exact: true }).waitFor();
  await button("disk0").click();
  await button("资料").click();
  await page.getByText("远端当前不可用", { exact: true }).waitFor();
  assert.equal(await readFile(join(mirror, path), "utf8"), "published content");
  const local = button(file.name);
  const trigger = await local.elementHandle();
  await local.click();
  await page.waitForFunction(node => !node.hasAttribute("aria-busy"), trigger);
  assert.deepEqual(requests("open"), [{ action: "open", path }]);
  if (await page.locator("#form-dialog").evaluate(element => element.open)) {
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "hidden" });
  }
  assert.equal(fixture.cli(mirror, "status").entries.find(item => item.path === path).container_id, file.container_id);

  await button("解除追踪").click();
  await dialog.getByRole("heading", { name: "解除追踪", exact: true }).waitFor();
  await page.keyboard.press("Escape");
  await dialog.waitFor({ state: "hidden" });

  await rm(join(mirror, path));
  await button("刷新").click();
  await page.waitForFunction(() => !document.getElementById("refresh").hasAttribute("aria-busy"));
  await local.click();
  await dialog.getByRole("heading", { name: "解除追踪", exact: true }).waitFor();
  assert.equal(requests("open").length, 1);
});

test("synchronizing an empty server directory reports success", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, mirror, source } = fixture;
  const directory = "disk0/空目录";
  fixture.cli(source, "mkdir", "/" + directory);
  const local = join(mirror, directory);
  await assert.rejects(stat(local), { code: "ENOENT" });

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").click();
  await button("空目录").click();
  await button("同步此目录").click();
  await page.getByText("目录已同步。", { exact: true }).waitFor();
  assert.ok((await stat(local)).isDirectory());
});

test("directory sync refreshes the displayed current version without rechecking the session", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror } = fixture;
  const [file] = fixture.list("/" + folder);
  const path = folder + "/" + file.name;
  await rm(join(mirror, folder, ".zxsignore"));
  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const row = page.getByRole("row").filter({ has: button(file.name) });
  const details = page.locator("#details");
  const current = details.locator(".version[data-current=true]");
  const requests = observeApi(page);
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").click();
  await button("资料").click();
  await button(file.name).click();
  await current.getByText("v1", { exact: true }).waitFor();
  const initialSessions = requests("session");

  const latest = "latest published content";
  await fixture.updateOriginal(latest);
  await button("同步此目录").click();
  await page.getByText("已同步 1 项到本地工作区。", { exact: true }).waitFor();
  assert.equal(await row.getByText("v2", { exact: true }).count(), 1);
  assert.equal(await readFile(join(mirror, path), "utf8"), latest);
  assert.equal(await current.getByText("v2", { exact: true }).count(), 1, "details must mark v2 as current");
  assert.equal(await details.locator(".version[data-current=false]").getByText("v1", { exact: true }).count(), 1);
  assert.deepEqual(requests("session"), initialSessions);

  await current.getByRole("button", { name: "恢复到工作区", exact: true }).click();
  await page.getByText(/已恢复 v2 到工作区/).waitFor();
  assert.deepEqual(requests("checkout").map(({ id, version }) => [id, version]), [[file.container_id, 2]]);
  assert.equal(await readFile(join(mirror, path), "utf8"), latest);
  const entry = fixture.cli(mirror, "status").entries.find(entry => entry.path === path);
  assert.deepEqual([entry.version, entry.state], [2, "clean"]);
});

test("directory sync and history restoration preserve the ignore rules of a local bundle", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror, source } = fixture;
  const bundle = "disk0/项目.ZIP";
  const local = join(mirror, bundle);
  const privateName = "private.local";
  const privateContents = "kept only in this workspace";
  const child = bundle + "/远端子目录";
  const remoteName = "原件.txt";
  await rm(join(mirror, folder, ".zxsignore"));
  await mkdir(local);
  assert.equal(fixture.cli(mirror, "init-bundle", bundle), bundle);
  await writeFile(join(local, ".zxsignore"), privateName + "\n");
  await writeFile(join(local, privateName), privateContents);
  await mkdir(join(source, child), { recursive: true });
  await writeFile(join(source, child, remoteName), "other workspace contents");
  fixture.cli(source, "commit", child + "/" + remoteName);

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").click();
  const synchronized = page.waitForResponse(reply => reply.url().endsWith("/api") &&
    reply.request().postDataJSON().action === "sync");
  await button("同步此目录").click();
  const syncReport = await (await synchronized).json();
  assert.ok(syncReport.issues.some(issue => issue.path === bundle && /目录包/.test(issue.reason)));
  const warning = dialog.getByRole("alert");
  await warning.getByText(/目录包/).waitFor();
  assert.match(await warning.textContent(), /disk0\/项目\.ZIP/);
  await dialog.getByRole("button", { name: "关闭", exact: true }).click();
  await assert.rejects(stat(join(mirror, child, remoteName)), { code: "ENOENT" });
  assert.equal(await readFile(join(mirror, folder, remoteName), "utf8"), "published content");

  await button("项目.ZIP").click();
  await button("远端子目录").click();
  await button(remoteName).click();
  await button("恢复到工作区").click();
  await warning.getByText(/目录包/).waitFor();
  await dialog.getByRole("button", { name: "关闭", exact: true }).click();
  await assert.rejects(stat(join(mirror, child, remoteName)), { code: "ENOENT" });

  await button("工作区").click();
  await button("disk0").click();
  const submitted = page.waitForResponse(reply => reply.url().endsWith("/api") &&
    reply.request().postDataJSON().action === "commit_directory");
  await button("提交当前目录").click();
  const report = await (await submitted).json();
  assert.equal(report.committed, 0);
  assert.ok(report.issues.some(issue => issue.path === bundle));
  assert.deepEqual(fixture.list("/" + bundle), []);
  assert.deepEqual(fixture.list("/" + child).map(file => file.name), [remoteName]);
  assert.equal(await readFile(join(local, privateName), "utf8"), privateContents);
  assert.equal(await readFile(join(local, ".zxsignore"), "utf8"), privateName + "\n");
});

test("bundle initialization preserves .ZIP and submission reports a removed suffix", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, mirror } = fixture;
  const name = "大写后缀.ZIP";
  const original = join(mirror, "disk0", name);
  const renamed = join(mirror, "disk0", "大写后缀");
  await mkdir(original);
  await writeFile(join(original, "成员.txt"), "packed member");
  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  const row = name => page.locator("#rows").getByRole("button", { name, exact: true });

  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("工作区").click();
  await row("disk0").click();
  await row(name).click();
  await button("创建 .zxsignore").click();
  await row(".zxsignore").waitFor();
  await page.locator("#crumbs").getByRole("button", { name, exact: true }).waitFor();
  assert.ok(await dialog.isHidden());
  assert.ok((await stat(join(original, ".zxsignore"))).isFile());
  await assert.rejects(stat(original + ".zip"), { code: "ENOENT" });

  await button("提交目录包").click();
  await dialog.getByRole("button", { name: "提交", exact: true }).click();
  await dialog.waitFor({ state: "hidden" });
  await rename(original, renamed);
  await page.locator("#crumbs").getByRole("button", { name: "disk0", exact: true }).click();
  await row("大写后缀").click();
  await button("提交目录包").click();
  await dialog.getByRole("button", { name: "提交", exact: true }).click();
  await dialog.getByRole("alert").getByText(/目录包.*\.zip/).waitFor();
  assert.deepEqual(fixture.list("/disk0").map(file => [file.name, file.latest_version]), [[name, 1]]);
  assert.equal(await readFile(join(renamed, "成员.txt"), "utf8"), "packed member");
});

async function checkBundleNavigation(t, enterChild) {
  const fixture = await startFixture(t);
  const name = "等待改名";
  const child = "子目录";
  const member = "成员.txt";
  const original = join(fixture.mirror, "disk0", name);
  await mkdir(join(original, child), { recursive: true });
  await writeFile(join(original, child, member), "packed member");
  const page = await fixture.browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const row = name => page.locator("#rows").getByRole("button", { name, exact: true });
  const crumb = name => page.locator("#crumbs").getByRole("button", { name, exact: true });
  await page.goto(fixture.entrance.href, { waitUntil: "domcontentloaded" });
  await button("工作区").click();
  await row("disk0").click();
  await row(name).click();

  let captured;
  let resume;
  const pending = new Promise(resolve => { captured = resolve; });
  const gate = new Promise(resolve => { resume = resolve; });
  await page.route("**/api", async route => {
    if (route.request().postDataJSON().action === "init_bundle") {
      captured();
      await gate;
    }
    await route.continue();
  });
  try {
    await button("创建 .zxsignore").click();
    await pending;
    await (enterChild ? row(child) : crumb("disk0")).click();
    await row(enterChild ? member : name).waitFor();
    resume();
    if (enterChild) {
      await crumb(name + ".zip").waitFor();
      await row(member).waitFor();
    } else {
      await row(name + ".zip").waitFor();
      assert.ok(await row(name).isHidden());
    }
    assert.equal(await crumb(enterChild ? child : "disk0").getAttribute("aria-current"), "location");
    assert.equal(await readFile(join(original + ".zip", child, member), "utf8"), "packed member");
    await assert.rejects(stat(original), { code: "ENOENT" });
  } finally {
    resume();
    await page.unroute("**/api");
  }
}

test("bundle initialization refreshes the parent when the user has already returned there", { timeout: 30_000 },
  t => checkBundleNavigation(t, false));
test("bundle initialization keeps a browsed child at the renamed path", { timeout: 30_000 },
  t => checkBundleNavigation(t, true));

async function checkConflictDialog(t, failUpload) {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror, root } = fixture;
  const [file] = fixture.list("/" + folder);
  fixture.cli(mirror, "checkout", file.container_id);
  await writeFile(join(mirror, folder, file.name), "my local edits");
  await fixture.updateOriginal("remote revision");
  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("工作区").click();
  await button("disk0").click();
  await button("资料").click();
  await page.getByRole("row").filter({ has: button(file.name) }).getByRole("button", { name: "提交", exact: true }).click();
  await dialog.getByRole("heading", { name: "提交冲突", exact: true }).waitFor();

  let captured;
  let resume;
  const pending = new Promise(resolve => { captured = resolve; });
  const gate = new Promise(resolve => { resume = resolve; });
  await page.route("**/api", async route => {
    const { action, choice } = route.request().postDataJSON();
    if (action !== "commit" || choice !== "append") return route.continue();
    if (failUpload) {
      captured();
      await gate;
      return route.continue();
    }
    const response = await route.fetch();
    captured(response.status());
    await gate;
    await route.fulfill({ response });
  });
  try {
    const append = dialog.getByRole("button", { name: "追加到最新版", exact: true });
    const uploadButton = await append.elementHandle();
    await append.click();
    if (!failUpload) assert.equal(await pending, 200);
    else await pending;
    for (const label of ["追加到最新版", "另存为新文件", "备份本地后拉取"]) {
      assert.ok(await dialog.getByRole("button", { name: label, exact: true }).isDisabled(), label);
    }
    assert.ok(await dialog.getByRole("button", { name: "取消", exact: true }).isEnabled());
    await page.keyboard.press("Escape");
    await button("选择工作区").click();
    const path = dialog.getByRole("textbox", { name: "工作区目录" });
    const draft = join(root, "新工作区");
    await path.fill(draft);
    if (failUpload) await rm(join(mirror, folder, file.name));
    resume();
    await page.waitForFunction(node => !node.hasAttribute("aria-busy"), uploadButton);
    assert.equal(await path.inputValue(), draft);
    assert.ok(await dialog.getByRole("alert").isHidden());
    assert.equal(fixture.list("/" + folder)[0].latest_version, failUpload ? 2 : 3);
  } finally {
    resume();
    await page.unrouteAll({ behavior: "wait" });
  }
}

test("a finishing conflict upload keeps a newer dialog open", { timeout: 30_000 },
  t => checkConflictDialog(t, false));
test("a failed conflict upload does not report an error in a newer dialog", { timeout: 30_000 },
  t => checkConflictDialog(t, true));

async function checkStaleCommitTarget(t, operation) {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror, source } = fixture;
  const [file] = fixture.list("/" + folder);
  const path = folder + "/" + file.name;
  const local = join(mirror, path);
  await rm(join(mirror, folder, ".zxsignore"));
  fixture.cli(mirror, "checkout", file.container_id);
  await writeFile(local, "original unpublished draft");
  await fixture.updateOriginal("published v2");

  const handlingOnly = operation === "handling";
  const batch = operation === "batch" || handlingOnly;
  const pull = operation === "pull";
  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  const requests = observeApi(page);
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("工作区").click();
  await button("disk0").click();
  await button("资料").click();
  if (batch) {
    await button("提交当前目录").click();
    await dialog.getByRole("heading", { name: "目录提交结果", exact: true }).waitFor();
  } else {
    await page.getByRole("row").filter({ has: button(file.name) })
      .getByRole("button", { name: "提交", exact: true }).click();
    await dialog.getByRole("heading", { name: "提交冲突", exact: true }).waitFor();
  }

  let current = file;
  if (pull || handlingOnly) {
    fixture.cli(mirror, "checkout", file.container_id, "--backup");
    assert.equal(await readFile(local, "utf8"), "published v2");
  } else {
    const other = folder + "/另一个容器.txt";
    await writeFile(join(source, other), "another container v1");
    current = fixture.cli(source, "commit", other).data.container;
    fixture.cli(mirror, "untrack", path);
    await rm(local);
    fixture.cli(mirror, "checkout", current.container_id, "--to", path);
  }
  const draft = "edits from the other window";
  await writeFile(local, draft);

  if (batch) {
    await dialog.getByRole("button", { name: "处理冲突", exact: true }).click();
    await dialog.getByRole("heading", { name: "提交冲突", exact: true }).waitFor();
    assert.deepEqual(requests("commit"), []);
  }
  if (handlingOnly) {
    await dialog.getByRole("button", { name: "取消", exact: true }).click();
  } else {
    const response = page.waitForResponse(replies => replies.url().endsWith("/api") &&
      replies.request().postDataJSON().action === "commit");
    const label = pull ? "备份本地后拉取" : "追加到最新版";
    await dialog.getByRole("button", { name: label, exact: true }).click();
    const reply = await response;
    assert.equal(reply.ok(), false, label + " returned HTTP " + reply.status());
    assert.equal(reply.request().postDataJSON().expected_container, file.container_id);
    const warning = pull ? /已恢复同步/ : /本地追踪对象已更换/;
    await dialog.getByRole("alert").getByText(warning).waitFor();
  }
  const versions = fixture.list("/" + folder);
  assert.equal(versions.find(entry => entry.container_id === file.container_id).latest_version, 2);
  if (current !== file) assert.equal(versions.find(entry => entry.container_id === current.container_id).latest_version, 1);
  const tracked = fixture.cli(mirror, "status").entries.find(entry => entry.path === path);
  assert.deepEqual([tracked.container_id, tracked.state], [current.container_id, "modified"]);
  assert.equal(await readFile(local, "utf8"), draft);
}

test("a stale append confirmation cannot submit to a replacement container", { timeout: 30_000 },
  t => checkStaleCommitTarget(t, "append"));
test("an old batch conflict cannot submit to a replacement container", { timeout: 30_000 },
  t => checkStaleCommitTarget(t, "batch"));
test("opening a resolved batch conflict does not publish the local draft", { timeout: 30_000 },
  t => checkStaleCommitTarget(t, "handling"));
test("a stale backup-and-pull confirmation cannot upload the local draft", { timeout: 30_000 },
  t => checkStaleCommitTarget(t, "pull"));

async function checkFirstCommitConfirmation(t, { choice, change, batch = false }) {
  const serverRequests = [];
  const fixture = await startFixture(t, {
    observeServer: request => serverRequests.push(request.method + " " + request.url.split("?")[0]),
  });
  const { browser, entrance, folder, mirror, source, data } = fixture;
  const list = async () => (await fixture.cliAsync(mirror, "ls", "/" + folder)).containers;
  const [file] = await list();
  const path = folder + "/" + file.name;
  const local = join(mirror, path);
  const draft = "untracked private draft";
  await rm(join(mirror, folder, ".zxsignore"));
  await writeFile(local, draft);

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  const dialogButton = name => dialog.getByRole("button", { name, exact: true });
  const conflict = dialog.getByRole("heading", { name: "提交冲突", exact: true });
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("工作区").click();
  await button("disk0").click();
  await button("资料").click();
  if (batch) {
    await button("提交当前目录").click();
    await dialog.getByRole("heading", { name: "目录提交结果", exact: true }).waitFor();
  } else {
    await page.getByRole("row").filter({ has: button(file.name) })
      .getByRole("button", { name: "提交", exact: true }).click();
    await conflict.waitFor();
  }

  let replacement;
  if (change) await fixture.cliAsync(source, "recycle", "move", file.container_id);
  if (change === "replaced") {
    const other = folder + "/新容器.txt";
    await writeFile(join(source, other), "replacement v1");
    replacement = (await fixture.cliAsync(source, "commit", other, "--name", file.name)).data.container;
  }
  const firstRequest = serverRequests.length;
  if (batch) {
    await dialogButton("处理冲突").click();
    await conflict.waitFor();
    assert.equal(serverRequests.length, firstRequest);
  }

  const newName = "独立草稿.txt";
  if (choice === "new") {
    await dialogButton("另存为新文件").click();
    await dialog.getByRole("textbox", { name: "新文件名" }).fill(newName);
    assert.equal(serverRequests.length, firstRequest);
  }
  const reply = page.waitForResponse(response => response.url().endsWith("/api") &&
    response.request().postDataJSON().choice === choice);
  const labels = { append: "追加到最新版", pull: "备份本地后拉取", new: "另存" };
  await dialogButton(labels[choice]).click();
  const response = await reply;
  assert.deepEqual(response.request().postDataJSON().expected_container, { same_name: file.container_id });
  const requests = serverRequests.slice(firstRequest).filter(request => request.includes(" /containers"));

  if (change && choice !== "new") {
    assert.equal(response.ok(), false);
    await dialog.getByRole("alert").getByText(/同名文件已变化/).waitFor();
    assert.deepEqual(requests, ["GET /containers"]);
    assert.deepEqual((await list()).map(entry => [entry.container_id, entry.latest_version]),
      replacement ? [[replacement.container_id, 1]] : []);
    assert.equal(await readFile(local, "utf8"), draft);
    return;
  }

  assert.equal(response.status(), 200);
  const result = await response.json();
  await dialog.waitFor({ state: "hidden" });
  if (choice === "pull") {
    assert.equal(result.outcome, "pulled");
    assert.equal(await readFile(result.data.backup, "utf8"), draft);
    assert.equal(await readFile(local, "utf8"), "published content");
    assert.equal((await list())[0].latest_version, 1);
    assert.deepEqual(requests, ["GET /containers", "GET /containers/" + file.container_id,
      "GET /containers/" + file.container_id + "/versions/1/payload"]);
  } else if (choice === "append") {
    assert.equal(result.data.container.container_id, file.container_id);
    assert.equal(result.data.version.version, 2);
    assert.equal(await readFile(join(data, path, "v000002", file.name), "utf8"), draft);
    assert.deepEqual(requests, ["GET /containers", "POST /containers/" + file.container_id + "/versions"]);
  } else {
    assert.equal(result.data.container.name, newName);
    assert.equal(result.data.version.version, 1);
    assert.equal(await readFile(join(data, folder, newName, "v000001", newName), "utf8"), draft);
    assert.deepEqual(requests, ["POST /containers"]);

    const cliPath = folder + "/CLI 新文件.txt";
    await writeFile(join(mirror, cliPath), "CLI first edition");
    const cli = await fixture.cliAsync(mirror, "commit", cliPath, "--on-conflict", "pull");
    assert.equal(cli.outcome, "committed");
    assert.equal(cli.data.version.version, 1);
  }
}

test("a first same-name pull cannot publish after the target is removed", { timeout: 30_000 },
  t => checkFirstCommitConfirmation(t, { choice: "pull", change: "removed" }));
test("a first same-name append cannot publish to a replacement", { timeout: 30_000 },
  t => checkFirstCommitConfirmation(t, { choice: "append", change: "replaced" }));
test("a first same-name batch conflict only opens the choices and guards the original target", { timeout: 30_000 },
  t => checkFirstCommitConfirmation(t, { choice: "pull", change: "removed", batch: true }));
test("a confirmed first same-name append uses one lookup and appends directly", { timeout: 30_000 },
  t => checkFirstCommitConfirmation(t, { choice: "append" }));
test("a confirmed first same-name pull backs up the local draft", { timeout: 30_000 },
  t => checkFirstCommitConfirmation(t, { choice: "pull" }));
test("a first same-name conflict can save as an explicit new name after remote removal", { timeout: 30_000 },
  t => checkFirstCommitConfirmation(t, { choice: "new", change: "removed" }));

test("an old bundle toolbar confirmation cannot submit a replacement at the same path", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, mirror, source } = fixture;
  const member = "成员.txt";
  async function publishBundle(name) {
    const path = "disk0/" + name;
    await mkdir(join(source, path));
    await writeFile(join(source, path, member), name);
    fixture.cli(source, "init-bundle", path);
    return fixture.cli(source, "commit", path).data.container;
  }
  const file = await publishBundle("原目录.zip");
  const replacement = await publishBundle("替换目录.zip");
  const path = "disk0/" + file.name;
  const local = join(mirror, path);
  fixture.cli(mirror, "checkout", file.container_id);
  await writeFile(join(local, member), "old unpublished edit");

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("工作区").click();
  await button("disk0").click();
  await button(file.name).click();
  await button("提交目录包").click();
  await dialog.getByRole("heading", { name: "提交目录包", exact: true }).waitFor();

  fixture.cli(mirror, "untrack", path);
  await rm(local, { recursive: true });
  fixture.cli(mirror, "checkout", replacement.container_id, "--to", path);
  await writeFile(join(local, member), "new unpublished edit");
  const response = page.waitForResponse(reply => reply.url().endsWith("/api") &&
    reply.request().postDataJSON().action === "commit");
  await dialog.getByRole("button", { name: "提交", exact: true }).click();
  const reply = await response;
  assert.equal(reply.ok(), false, "stale bundle confirmation returned HTTP " + reply.status());
  assert.equal(reply.request().postDataJSON().expected_container, file.container_id);
  await dialog.getByRole("alert").getByText(/本地追踪对象已更换/).waitFor();
  assert.deepEqual(fixture.list("/disk0").map(entry => entry.latest_version), [1, 1]);
  assert.equal(await readFile(join(local, member), "utf8"), "new unpublished edit");
});

async function checkLateCommit(t, batch) {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror, root } = fixture;
  const [file] = fixture.list("/" + folder);
  await rm(join(mirror, folder, ".zxsignore"));
  fixture.cli(mirror, "checkout", file.container_id);
  await fixture.updateOriginal("latest server version");
  const local = join(mirror, folder, file.name);
  await writeFile(local, "unsubmitted local edits");
  if (batch) await writeFile(join(mirror, folder, "独立.txt"), "independent new file");

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("工作区").click();
  await button("disk0").click();
  await button("资料").click();
  const submit = batch ? button("提交当前目录") : page.getByRole("row").filter({ has: button(file.name) })
    .getByRole("button", { name: "提交", exact: true });
  const trigger = await submit.elementHandle();
  const operation = batch ? "commit_directory" : "commit";
  const { response, release } = await holdApiResponse(page, request => request.action === operation,
    async result => ({ status: result.status(), body: await result.json() }));
  try {
    await submit.click();
    const { status, body } = await response;
    if (batch) {
      assert.equal(status, 200);
      assert.equal(body.committed, 1);
      assert.ok(body.issues.some(issue => issue.conflict && issue.path === folder + "/" + file.name));
    } else {
      assert.equal(status, 409);
      assert.equal(body.code, "commit_conflict");
    }
    await button("选择工作区").click();
    const path = dialog.getByRole("textbox", { name: "工作区目录" });
    const draft = join(root, "尚未保存的草稿");
    await path.fill(draft);
    release();
    await page.waitForFunction(node => !node.hasAttribute("aria-busy"), trigger);
    assert.equal(await dialog.getByRole("heading").textContent(), "选择工作区");
    assert.equal(await path.inputValue(), draft);
    if (batch) await page.locator("#notice").getByText("已提交 1 项，1 项待处理。", { exact: true }).waitFor();
    assert.equal(await readFile(local, "utf8"), "unsubmitted local edits");
    assert.equal(fixture.list("/" + folder).find(item => item.container_id === file.container_id).latest_version, 2);
  } finally {
    release();
    await page.unrouteAll({ behavior: "wait" });
  }
}

test("a late batch report keeps the new form and shows its summary", { timeout: 30_000 }, t => checkLateCommit(t, true));
test("a late file commit conflict preserves the new form", { timeout: 30_000 }, t => checkLateCommit(t, false));

test("a late deletion retry does not replace a newer form", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror, root } = fixture;
  const [file] = fixture.list("/" + folder);
  fixture.cli(mirror, "checkout", file.container_id);
  await rm(join(mirror, folder, file.name));
  await fixture.updateOriginal("latest server version");

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("工作区").click();
  await button("disk0").click();
  await button("资料").click();
  await button("同步删除").click();
  const finish = await watchDialogSubmit(page);
  const deletion = page.waitForResponse(result =>
    result.url().endsWith("/api") && result.request().postDataJSON().action === "sync_delete");
  const { response, release } = await holdApiResponse(page, request => request.action === "detail");
  try {
    await dialog.getByRole("button", { name: "移入回收站", exact: true }).click();
    assert.equal((await deletion).status(), 409);
    assert.equal(await response, 200);
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "hidden" });
    await button("选择工作区").click();
    const path = dialog.getByRole("textbox", { name: "工作区目录" });
    const draft = join(root, "尚未保存的草稿");
    await path.fill(draft);
    release();
    await finish();
    assert.equal(await dialog.getByRole("heading").textContent(), "选择工作区");
    assert.equal(await path.inputValue(), draft);
    assert.equal(fixture.list("/" + folder)[0].latest_version, 2);
  } finally {
    release();
    await page.unrouteAll({ behavior: "wait" });
  }
});

async function watchDialogSubmit(page) {
  const form = page.locator("#dialog-form");
  await form.evaluate(element => {
    const submit = element.onsubmit;
    element.onsubmit = event => { element.completedSubmit = submit(event); };
  });
  return () => form.evaluate(element => element.completedSubmit);
}

async function holdApiResponse(page, matches, read = result => result.status()) {
  const captured = Promise.withResolvers();
  const gate = Promise.withResolvers();
  await page.route("**/api", async route => {
    if (!matches(route.request().postDataJSON())) return route.continue();
    const response = await route.fetch();
    captured.resolve(await read(response));
    await gate.promise;
    await route.fulfill({ response });
  });
  return { response: captured.promise, release: gate.resolve };
}

const holdCheckoutResponse = page => holdApiResponse(page, ({ action, choice }) => action === "checkout" && choice === "if_clean");

test("a late restoration conflict preserves another file's tag dialog", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror, source } = fixture;
  const [original] = fixture.list("/" + folder);
  const name = "第二个文件.txt";
  const local = join(mirror, folder, original.name);
  await writeFile(join(source, folder, name), "another file");
  fixture.cli(source, "commit", folder + "/" + name);
  fixture.cli(mirror, "checkout", original.container_id);
  await writeFile(local, "unsaved local changes");

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").click();
  await button("资料").click();
  await button(original.name).click();
  const restore = button("恢复到工作区");
  const trigger = await restore.elementHandle();
  const { response, release } = await holdCheckoutResponse(page);
  try {
    await restore.click();
    assert.equal(await response, 409);
    await button(name).click();
    await fileDetails(page, name).waitFor();
    await page.locator("#edit-tags").click();
    const tags = dialog.locator("input[name=tags]");
    await tags.fill("尚未保存");
    release();
    await page.waitForFunction(node => !node.hasAttribute("aria-busy"), trigger);
    assert.equal(await dialog.getByRole("heading").textContent(), "编辑标签");
    assert.equal(await tags.inputValue(), "尚未保存");
    assert.equal(await readFile(local, "utf8"), "unsaved local changes");
  } finally {
    release();
    await page.unrouteAll({ behavior: "wait" });
  }
});

test("a newer historical restore owns the backup prompt for the same file", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror } = fixture;
  const [file] = fixture.list("/" + folder);
  const local = join(mirror, folder, file.name);
  await rm(join(mirror, folder, ".zxsignore"));
  await fixture.updateOriginal("published second version");
  await fixture.updateOriginal("published third version");
  fixture.cli(mirror, "checkout", file.container_id);
  await writeFile(local, "unpublished working draft");

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  const restore = version => page.locator(".history-list .version")
    .filter({ has: page.getByText("v" + version, { exact: true }) })
    .getByRole("button", { name: "恢复到工作区", exact: true });
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").click();
  await button("资料").click();
  await button(file.name).click();
  const older = await restore(1).elementHandle();
  const { response, release } = await holdApiResponse(page, ({ action, choice, version }) =>
    action === "checkout" && choice === "if_clean" && version === 1);
  try {
    await restore(1).click();
    assert.equal(await response, 409);
    await restore(2).click();
    const backup = dialog.getByRole("button", { name: "备份后恢复", exact: true });
    await backup.waitFor();
    const prompt = await dialog.locator("#dialog-body").textContent();
    await backup.click();
    await dialog.waitFor({ state: "hidden" });
    assert.equal(await readFile(local, "utf8"), "published second version");

    release();
    await page.waitForFunction(node => !node.hasAttribute("aria-busy"), older);
    assert.ok(await dialog.isHidden(), "the older request reopened a backup prompt");
    assert.equal(prompt, "备份当前修改后恢复 v2。");
    assert.equal(await readFile(local, "utf8"), "published second version");
  } finally {
    release();
    await page.unrouteAll({ behavior: "wait" });
  }
});

test("a late successful restoration does not refresh the new workspace", { timeout: 30_000 }, async t => {
  const { browser, entrance, folder, mirror, root } = await startFixture(t);
  const other = join(root, "第二个工作区");
  const name = "原件.txt";
  const otherFile = join(other, folder, name);
  await mkdir(join(other, folder), { recursive: true });
  await writeFile(otherFile, "independent content");

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const requests = observeApi(page);
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").click();
  await button("资料").click();
  await button(name).click();
  const restore = button("恢复到工作区");
  const trigger = await restore.elementHandle();
  const { response, release } = await holdCheckoutResponse(page);
  try {
    await restore.click();
    assert.equal(await response, 200);
    assert.equal(await readFile(join(mirror, folder, name), "utf8"), "published content");
    await button("工作区").click();
    await button("选择工作区").click();
    await page.getByRole("textbox", { name: "工作区目录" }).fill(other);
    await button("保存").click();
    await button("第二个工作区").waitFor();
    await page.locator("#rows").getByRole("button", { name: "disk0", exact: true }).waitFor();
    const listings = requests("browse_workspace").length;
    release();
    await page.waitForFunction(node => !node.hasAttribute("aria-busy"), trigger);
    assert.equal(requests("browse_workspace").length, listings);
    assert.ok(await page.locator("#notice").isHidden());
    assert.equal(await readFile(otherFile, "utf8"), "independent content");
  } finally {
    release();
    await page.unrouteAll({ behavior: "wait" });
  }
});

test("restoring the same historical file again only asks for backup after a local edit", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror } = fixture;
  const [file] = fixture.list("/" + folder);
  const local = join(mirror, folder, file.name);
  await fixture.updateOriginal("latest file content");

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").click();
  await button("资料").click();
  await button(file.name).click();
  const restore = page.locator(".history-list .version").last()
    .getByRole("button", { name: "恢复到工作区", exact: true });
  await restore.click();
  await page.getByText(/已恢复 v1 到工作区.*待提交/).waitFor();
  assert.equal(await readFile(local, "utf8"), "published content");

  const repeated = page.waitForResponse(result => result.url().endsWith("/api") &&
    result.request().postDataJSON().action === "checkout");
  await restore.click();
  const response = await repeated;
  assert.equal(response.status(), 200);
  const result = await response.json();
  assert.equal(result.backup, null);
  assert.equal(result.restored_from, 1);
  assert.ok(await dialog.isHidden());

  const edits = "unsubmitted changes with a different file length";
  await writeFile(local, edits);
  await restore.click();
  await dialog.getByRole("heading", { name: "本地文件有修改", exact: true }).waitFor();
  assert.equal(await readFile(local, "utf8"), edits);
  await dialog.getByRole("button", { name: "备份后恢复", exact: true }).click();
  const notice = await page.getByText(/已恢复 v1 到工作区.*备份位于/).textContent();
  assert.equal(await readFile(notice.split("，备份位于 ")[1], "utf8"), edits);
  assert.equal(await readFile(local, "utf8"), "published content");
  assert.equal(fixture.list("/" + folder)[0].latest_version, 2);
});

test("a historical directory bundle rejects a non-ZIP default path and preserves the local file", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror, source, root } = fixture;
  const bundle = `${folder}/历史项目.zip`;
  const name = "旧项目.txt";
  const latest = `${folder}/${name}`;
  const writer = join(root, "writer");
  await mkdir(join(source, bundle));
  await writeFile(join(source, bundle, "成员.txt"), "original bundle content");
  fixture.cli(source, "init-bundle", bundle);
  const { data: { container } } = fixture.cli(source, "commit", bundle);
  fixture.cli(mirror, "patch", container.container_id, "--name", name);
  await mkdir(join(writer, folder), { recursive: true });
  await writeFile(join(writer, latest), "latest plain file content");
  const { data: { version } } = fixture.cli(writer, "commit", latest, "--on-conflict", "append");
  assert.equal(version.version, 2);
  fixture.cli(mirror, "checkout", container.container_id);
  const local = join(mirror, latest);
  assert.equal(await readFile(local, "utf8"), "latest plain file content");

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("disk0").click();
  await button("资料").click();
  await button(name).click();
  const restore = page.locator(".history-list .version").last()
    .getByRole("button", { name: "恢复到工作区", exact: true });
  const historical = page.waitForResponse(result => result.url().endsWith("/api") &&
    result.request().postDataJSON().action === "checkout");
  await restore.click();
  assert.equal((await historical).status(), 409);
  await page.getByRole("dialog").getByRole("alert").getByText(/目录包名称须以 \.zip 结尾/).waitFor();
  assert.equal(await readFile(local, "utf8"), "latest plain file content");
  assert.equal(await readFile(join(source, bundle, "成员.txt"), "utf8"), "original bundle content");
  assert.equal(fixture.list("/" + folder).find(item => item.container_id === container.container_id).latest_version, 2);
});

test("a historical version is restored locally before explicit submission", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror } = fixture;
  const file = join(mirror, folder, "原件.txt");
  await fixture.updateOriginal("latest content");
  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const action = name => page.getByRole("button", { name, exact: true });
  const row = name => page.locator("#rows").getByRole("button", { name, exact: true });
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await row("disk0").waitFor();
  await assert.rejects(stat(file), { code: "ENOENT" });
  await row("disk0").click();
  await row("资料").click();
  await row("原件.txt").click();
  const history = page.locator(".history-list .version").last().getByRole("button");
  await history.waitFor();
  assert.deepEqual(await history.allTextContents(), ["恢复到工作区"]);
  await history.click();
  await page.getByText(/已恢复 v1 到工作区.*待提交/).waitFor();
  assert.equal(await readFile(file, "utf8"), "published content");
  assert.equal(fixture.list("/" + folder)[0].latest_version, 2);
  await action("关闭详情").click();
  await action("工作区").click();
  await action("同步已追踪").click();
  await row("disk0").click();
  await row("资料").click();
  await page.getByText("有本地修改", { exact: true }).waitFor();
  assert.equal(await readFile(file, "utf8"), "published content");
  await page.locator("#rows").getByRole("button", { name: "提交", exact: true }).click();
  await page.getByText("已提交 v3。", { exact: true }).waitFor();
  assert.equal(fixture.list("/" + folder)[0].latest_version, 3);
});

test("a cross-disk move asks before submission and restores history at the new path", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t, { secondDisk: true });
  const { browser, entrance, folder, mirror, source, data } = fixture;
  await fixture.updateOriginal("latest content");
  const [entry] = fixture.list("/" + folder);
  const name = entry.name;
  const oldPath = folder + "/" + name;
  const newFolder = "disk1/新位置";
  const newPath = newFolder + "/" + name;
  fixture.cli(mirror, "checkout", entry.container_id);
  await writeFile(join(mirror, oldPath), "local edits before the move");
  await mkdir(join(mirror, folder, "本地空目录", "内层"), { recursive: true });
  fixture.cli(source, "patch", entry.container_id, "--folder", "/" + newFolder);

  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const row = name => page.locator("#rows").getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");
  await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
  await button("工作区").click();
  await row("disk0").click();
  await row("资料").click();
  const empty = page.getByRole("row").filter({ has: button("本地空目录") });
  await empty.getByRole("button", { name: "提交目录", exact: true }).click();
  await page.getByText("已创建 1 个空文件夹。", { exact: true }).waitFor();
  assert.ok((await stat(join(data, folder, "本地空目录", "内层"))).isDirectory());

  await button("提交当前目录").click();
  await dialog.getByRole("heading", { name: "目录提交结果" }).waitFor();
  assert.ok((await dialog.textContent()).includes("/" + newPath));
  assert.equal(fixture.list("/" + newFolder)[0].latest_version, 2);
  await button("关闭").click();

  const file = page.getByRole("row").filter({ has: button(name) });
  await file.getByRole("button", { name: "提交", exact: true }).click();
  await dialog.getByRole("heading", { name: "提交冲突" }).waitFor();
  assert.ok((await dialog.textContent()).includes(oldPath));
  assert.ok((await dialog.textContent()).includes("/" + newPath));
  assert.equal(fixture.list("/" + newFolder)[0].latest_version, 2);
  assert.equal(await readFile(join(mirror, oldPath), "utf8"), "local edits before the move");
  await button("追加到最新版").click();
  await page.getByText("已提交 v3 到 /" + newPath + "。", { exact: true }).waitFor();
  assert.equal(fixture.list("/" + newFolder)[0].latest_version, 3);

  await writeFile(join(mirror, oldPath), "another unsaved edit");
  await button("文件").click();
  await row("disk1").click();
  await row("新位置").click();
  await row(name).click();
  await page.locator(".history-list .version").last().getByRole("button").click();
  await dialog.getByRole("heading", { name: "本地文件有修改" }).waitFor();
  await assert.rejects(stat(join(mirror, newPath)), { code: "ENOENT" });
  await button("备份后恢复").click();
  const notice = page.getByText(/已恢复 v1 到工作区.*备份位于/);
  const message = await notice.textContent();
  assert.ok(message.includes(newPath));
  assert.equal(await readFile(message.split("，备份位于 ")[1], "utf8"), "another unsaved edit");
  assert.equal(await readFile(join(mirror, newPath), "utf8"), "published content");
  await assert.rejects(stat(join(mirror, oldPath)), { code: "ENOENT" });
  assert.equal(fixture.list("/" + newFolder)[0].latest_version, 3);
});

test("desktop workspace in Chromium", { timeout: 100_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror, data } = fixture;
  const context = await browser.newContext();
  const page = await context.newPage();
  page.setDefaultTimeout(10_000);
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  const requests = observeApi(page);
  const button = name => page.getByRole("button", { name, exact: true });
  const row = name => page.locator("#rows").getByRole("button", { name, exact: true });
  const crumb = name => page.locator("#crumbs").getByRole("button", { name, exact: true });
  const dialog = page.getByRole("dialog");

  await t.test("one-time browser entry and the desktop mirror", async () => {
    const post = (route, body, origin = entrance.origin) => fetch(new URL(route, entrance), {
      method: "POST",
      headers: { "Content-Type": "application/json", Origin: origin },
      body: JSON.stringify(body),
    });
    const access = { token: entrance.hash.slice(1) };
    assert.equal((await post("/access", access, "http://127.0.0.1:1")).status, 403);
    assert.equal((await post("/api", { action: "session" })).status, 401);
    await page.goto(entrance.href, { waitUntil: "domcontentloaded" });
    await row("disk0").waitFor();
    const mark = await page.locator(".brand img").evaluate(async image => {
      await image.decode();
      return { src: image.getAttribute("src"), width: image.naturalWidth };
    });
    assert.equal(await page.locator('link[rel="icon"]').getAttribute("href"), mark.src);
    assert.equal(mark.width, 96);
    assert.deepEqual(requests("sync"), []);
    await assert.rejects(stat(join(mirror, folder, "原件.txt")), { code: "ENOENT" });
    assert.equal(new URL(page.url()).hash, "");
    const [cookie] = await context.cookies(entrance.origin);
    assert.ok(cookie?.httpOnly);
    assert.equal(cookie.sameSite, "Strict");
    assert.equal((await post("/access", access)).status, 401);
    await row("disk0").click();
    await row("资料").click();
    await button("同步此目录").click();
    await page.getByText("已同步 1 项到本地工作区。", { exact: true }).waitFor();
    assert.deepEqual(requests("sync").at(-1), { action: "sync", folder: "/" + folder });
    assert.equal(await readFile(join(mirror, folder, "原件.txt"), "utf8"), "published content");
    await button("工作区").click();
    await row("disk0").click();
    await row("资料").click();
    await page.getByText("没有本地变更。", { exact: true }).waitFor();
    assert.match(await page.locator("#bundle-warning").textContent(), /资料.*\.zxsignore/);
    await button("工作区").click();
  });

  await t.test("one-click upload uses the workspace path and tags are editable afterwards", async () => {
    const name = "新增.txt";
    const local = `${folder}/${name}`;
    await writeFile(join(mirror, local), "local draft");
    await button("刷新").click();
    await row("disk0").click();
    await row("资料").click();
    const draft = page.getByRole("row").filter({ has: button(name) });
    await draft.getByRole("button", { name: "提交", exact: true }).click();
    await row(name).waitFor({ state: "detached" });
    assert.ok(await dialog.isHidden());
    assert.deepEqual(requests("commit").at(-1), { action: "commit", path: local, expected_container: null, choice: "stop" });
    assert.equal(await readFile(join(data, local, "v000001", name), "utf8"), "local draft");

    await button("文件").click();
    await page.getByRole("searchbox", { name: "搜索文件名" }).fill(name);
    await row(name).click();
    await page.locator(".toolbar-actions").getByRole("button", { name: "添加标签" }).click();
    await dialog.locator("input[name=tags]").fill("重要，资料");
    await dialog.getByRole("button", { name: "保存", exact: true }).click();
    await dialog.waitFor({ state: "hidden" });
    const entry = fixture.list(`/${folder}`).find(item => item.name === name);
    assert.deepEqual(new Set(entry.tags), new Set(["重要", "资料"]));
    assert.equal(entry.latest_version, 1);
    await page.setViewportSize({ width: 480, height: 760 });
    await page.locator(".detail-head").getByRole("button", { name: "添加标签" }).click();
    await dialog.locator("input[name=tags]").fill("资料");
    await dialog.getByRole("button", { name: "保存", exact: true }).click();
    await dialog.waitFor({ state: "hidden" });
    assert.deepEqual(fixture.list(`/${folder}`).find(item => item.name === name).tags, ["资料"]);
    await page.setViewportSize({ width: 1280, height: 720 });
  });

  await t.test("directory submission keeps the tree, versions, and per-file conflicts", async () => {
    const original = folder + "/原件.txt";
    const missing = folder + "/新增.txt";
    const project = folder + "/新项目";
    const local = project + "/子目录/文件.txt";
    const bundle = project + "/需要打包.zip";
    const empty = project + "/空目录/子目录";
    await mkdir(join(mirror, project, "子目录"), { recursive: true });
    await mkdir(join(mirror, empty), { recursive: true });
    await mkdir(join(mirror, bundle));
    await writeFile(join(mirror, local), "nested file");
    await writeFile(join(mirror, bundle, ".zxsignore"), "");
    await writeFile(join(mirror, bundle, "成员.txt"), "bundle member");
    await writeFile(join(mirror, original), "local revision");
    await rm(join(mirror, missing));
    await fixture.updateOriginal("concurrent remote revision");

    await button("文件").click();
    await row("disk0").click();
    await button("同步此目录").waitFor();
    assert.equal(await button("新建文件夹").count(), 0);
    assert.ok(await page.locator("#submit-local").isHidden());
    await button("工作区").click();
    assert.ok(await page.locator("#submit-local").isHidden());
    await row("disk0").click();
    await button("提交当前目录").click();
    await dialog.getByRole("heading", { name: "目录提交结果" }).waitFor();
    assert.deepEqual(requests("commit_directory").at(-1), { action: "commit_directory", directory: "disk0" });
    assert.equal(await dialog.locator("input").count(), 0);
    assert.match(await dialog.textContent(), /已提交 2 项.*1 项待处理/);
    assert.match(await dialog.textContent(), /已忽略 disk0\/资料 中的 \.zxsignore/);
    assert.ok((await stat(join(data, empty))).isDirectory());
    assert.equal(await readFile(join(data, local, "v000001", "文件.txt"), "utf8"), "nested file");
    const archive = await readFile(join(data, bundle, "v000001", "需要打包.zip"));
    assert.equal(archive.subarray(30, 40).toString(), ".zxsignore");
    assert.ok(fixture.list("/" + folder).some(item => item.name === "新增.txt" && item.latest_version === 1));

    await button("处理冲突").click();
    await dialog.getByRole("heading", { name: "提交冲突" }).waitFor();
    await button("追加到最新版").click();
    await dialog.waitFor({ state: "hidden" });
    assert.equal(await readFile(join(data, original, "v000003", "原件.txt"), "utf8"), "local revision");
    await button("提交当前目录").click();
    await dialog.getByRole("heading", { name: "目录提交结果" }).waitFor();
    assert.match(await dialog.textContent(), /没有待提交的变更/);
    assert.equal(fixture.list("/" + project + "/子目录")[0].latest_version, 1);
    await button("关闭").click();

    const next = project + "/分组/下一层/文件.txt";
    await mkdir(join(mirror, project, "分组", "下一层"), { recursive: true });
    await writeFile(join(mirror, next), "group submission");
    await button("工作区").click();
    await row("disk0").click();
    await row("资料").click();
    const group = page.getByRole("row").filter({ has: button("新项目") });
    await group.getByRole("button", { name: "提交目录", exact: true }).click();
    await page.getByText("已提交 1 项。", { exact: true }).waitFor();
    await row("新项目").waitFor();
    assert.deepEqual(requests("commit_directory").at(-1), { action: "commit_directory", directory: project });
    assert.equal(await readFile(join(data, next, "v000001", "文件.txt"), "utf8"), "group submission");
  });

  await t.test("bundle creation, nested browsing and submission stay in the browser", async () => {
    const bundle = "disk0/目录包";
    const renamed = bundle + ".zip";
    const nested = `${bundle}/子目录`;
    await mkdir(join(mirror, nested, "空目录"), { recursive: true });
    await mkdir(join(mirror, bundle, "node_modules"));
    await writeFile(join(mirror, nested, "成员.txt"), "packed member");
    await writeFile(join(mirror, bundle, "node_modules", "仅本地.txt"), "ignored member");
    await button("工作区").click();
    await row("disk0").click();
    const createMarker = button("创建 .zxsignore");
    await createMarker.click();
    await dialog.getByRole("alert").getByText(/目录包须在已登记的数据盘内创建/).waitFor();
    assert.equal(await dialog.getByRole("textbox").count(), 0);
    assert.deepEqual(requests("init_bundle").at(-1), { action: "init_bundle", path: "disk0" });
    await assert.rejects(stat(join(mirror, "disk0/.zxsignore")), { code: "ENOENT" });
    await dialog.getByRole("button", { name: "关闭", exact: true }).click();
    await dialog.waitFor({ state: "hidden" });

    await row("目录包").click();
    await createMarker.click();
    await page.getByText("已创建 .zxsignore。", { exact: true }).waitFor();
    assert.ok(await dialog.isHidden());
    assert.deepEqual(requests("init_bundle").at(-1), { action: "init_bundle", path: bundle });
    await crumb("目录包.zip").waitFor();
    await assert.rejects(stat(join(mirror, bundle)), { code: "ENOENT" });
    await row(".zxsignore").waitFor();
    assert.ok(await row("node_modules").isVisible());
    await crumb("disk0").click();
    await row("目录包.zip").click();
    await row("子目录").click();
    await row("成员.txt").waitFor();
    assert.ok(await button("提交").isHidden());
    await row("空目录").click();
    await page.getByText("此目录为空。", { exact: true }).waitFor();
    await page.keyboard.press("Backspace");
    await row("成员.txt").waitFor();
    await button("提交目录包").click();
    assert.equal(await dialog.locator("input").count(), 0);
    await dialog.getByRole("button", { name: "提交", exact: true }).click();
    await dialog.waitFor({ state: "hidden" });
    assert.deepEqual(requests("commit").at(-1), { action: "commit", path: renamed, expected_container: null, choice: "stop" });
    assert.ok(await row("成员.txt").isVisible());
    const archive = await readFile(join(data, renamed, "v000001", "目录包.zip"));
    assert.equal(archive.subarray(30, 40).toString(), ".zxsignore");

    await button("文件").click();
    await row("disk0").click();
    const archivedBundle = page.getByRole("row").filter({ has: button("目录包.zip") });
    await archivedBundle.getByRole("button", { name: "移入回收站", exact: true }).waitFor();
    await row("目录包.zip").click();
    await button("恢复到工作区").waitFor();
    assert.equal(await button("检出并浏览").count(), 0);
    await button("恢复到工作区").click();
    await page.getByText(/已恢复 v1 到工作区/).waitFor();
    await button("工作区").click();
    await row("disk0").click();
    await row("目录包.zip").click();
    await row("子目录").waitFor();
    assert.deepEqual(requests("open"), []);
    assert.equal(await readFile(join(mirror, renamed, "node_modules", "仅本地.txt"), "utf8"), "ignored member");
  });

  await t.test("file and folder edits update the mirrored tree and preserve unsaved work", async () => {
    await button("文件").click();
    await row("disk0").click();
    await row("资料").click();
    const file = page.getByRole("row").filter({ has: button("原件.txt") });
    await file.getByRole("button", { name: "移动或重命名文件", exact: true }).click();
    await dialog.getByRole("textbox", { name: "名称" }).fill("新名称.txt");
    await dialog.getByRole("textbox", { name: "目录路径" }).fill("/disk0/移动后的文件");
    await button("保存").click();
    await dialog.waitFor({ state: "hidden" });
    assert.equal(await readFile(join(mirror, "disk0/移动后的文件/新名称.txt"), "utf8"), "local revision");
    await assert.rejects(stat(join(mirror, folder, "原件.txt")), { code: "ENOENT" });
    const [movedFile] = fixture.list("/disk0/移动后的文件");
    assert.equal(movedFile.name, "新名称.txt");
    assert.equal(movedFile.latest_version, 3);

    const original = folder + "/新项目";
    const destination = "disk0/新位置/新项目名称";
    const empty = "空目录/子目录";
    const dirty = "分组/下一层/文件.txt";
    await writeFile(join(mirror, original, dirty), "local content pending submission");
    const directory = page.getByRole("row").filter({ has: button("新项目") });
    await directory.getByRole("button", { name: "移动或重命名文件夹", exact: true }).click();
    await dialog.getByRole("textbox", { name: "名称" }).fill("新项目名称");
    await dialog.getByRole("textbox", { name: "目录路径" }).fill("/disk0/新位置");
    await button("保存").click();
    await dialog.getByRole("heading", { name: "服务器已保存，本地镜像有待处理项" }).waitFor();
    assert.match(await dialog.textContent(), /分组\/下一层\/文件\.txt/);
    await assert.rejects(stat(join(mirror, destination, empty)), { code: "ENOENT" });
    assert.ok((await stat(join(mirror, original, empty))).isDirectory());
    assert.equal(await readFile(join(mirror, destination, "子目录/文件.txt"), "utf8"), "nested file");
    assert.equal(await readFile(join(mirror, original, dirty), "utf8"), "local content pending submission");
    assert.equal(await readFile(join(data, destination, dirty, "v000001", "文件.txt"), "utf8"), "group submission");
    await button("关闭").click();

    await crumb("disk0").click();
    await row("新位置").click();
    await row("新项目名称").click();
    await button("同步此目录").click();
    await button("同步此目录").waitFor();
    await page.waitForFunction(() => !document.querySelector("#sync").disabled);
    assert.ok((await stat(join(mirror, destination, empty))).isDirectory());
    if (await dialog.isVisible()) await button("关闭").click();
    const bundle = page.getByRole("row").filter({ has: button("需要打包.zip") });
    await bundle.getByRole("button", { name: "移动或重命名目录包", exact: true }).click();
    assert.equal(await dialog.getByRole("textbox", { name: "名称" }).inputValue(), "需要打包.zip");
    await dialog.getByRole("textbox", { name: "名称" }).fill("改名后的目录包");
    await button("保存").click();
    await dialog.waitFor({ state: "hidden" });
    assert.equal(await readFile(join(mirror, destination, "改名后的目录包.zip/成员.txt"), "utf8"), "bundle member");
    assert.ok(fixture.list("/" + destination).some(item => item.name === "改名后的目录包.zip"));
  });

  await t.test("the active browser session reconnects after server restart", async () => {
    await fixture.restart();
    await button("文件").click();
    await row("disk0").click();
    await row("目录包.zip").waitFor();
    assert.ok(await dialog.isHidden());
    assert.deepEqual(errors, []);
  });

  await t.test("a pending search invalidates the previous page cursor", async () => {
    const route = "**/api";
    let startSearch;
    let releaseSearch;
    const started = new Promise(ready => { startSearch = ready; });
    const pending = new Promise(ready => { releaseSearch = ready; });
    const target = "新名称.txt";
    await page.route(route, async intercepted => {
      const { action, filter } = intercepted.request().postDataJSON();
      if (action !== "list") return intercepted.continue();
      if (filter.q === target && !filter.cursor) {
        startSearch();
        await pending;
      }
      if (filter.folder !== "/disk0") return intercepted.continue();
      const response = await intercepted.fetch();
      const data = await response.json();
      await intercepted.fulfill({ response, json: { ...data, next_cursor: "previous-page" } });
    });
    try {
      await button("文件").click();
      await row("disk0").click();
      const more = button("加载下一页");
      await more.waitFor();
      await page.getByRole("searchbox", { name: "搜索文件名" }).fill(target);
      await Promise.race([started, delay(5_000).then(() => assert.fail("search did not start"))]);
      assert.ok(await more.isHidden());
      releaseSearch();
      await row(target).waitFor();
      assert.equal(await page.locator("#rows tr").count(), 1);
    } finally {
      releaseSearch();
      await page.unroute(route);
    }
  });

  await t.test("selecting a workspace preserves files and tracking in the previous directory", async () => {
    const newName = "我的本地工作区";
    const target = join(fixture.root, newName);
    const pending = "disk0/新草稿.txt";
    await mkdir(target);
    await writeFile(join(mirror, pending), "unsubmitted local content");
    const index = await readFile(join(mirror, ".zxs", "workspace.sqlite"));
    const serverFileCount = fixture.list("/disk0").length;

    await button("工作区").click();
    await button("选择工作区").click();
    const path = dialog.getByRole("textbox", { name: "工作区目录" });
    assert.equal(await path.inputValue(), mirror);
    await path.fill(target);
    await dialog.getByRole("button", { name: "保存", exact: true }).click();
    await dialog.waitFor({ state: "hidden" });
    await crumb(newName).waitFor();
    assert.equal(await readFile(join(mirror, pending), "utf8"), "unsubmitted local content");
    assert.deepEqual(await readFile(join(mirror, ".zxs", "workspace.sqlite")), index);
    assert.equal(fixture.status().entries.length, 0);
    assert.equal(fixture.list("/disk0").length, serverFileCount);
    assert.deepEqual(errors, []);
  });
});
