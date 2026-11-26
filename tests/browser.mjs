import assert from "node:assert/strict";
import { execFile, execFileSync, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { createServer as createHttpServer, request as httpRequest } from "node:http";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { chromium } from "playwright";

const repository = fileURLToPath(new URL("../", import.meta.url));
const serverBin = resolve(repository, process.env.ZXS_SERVER_BIN ?? "zxs-server/target/debug/zxs");
const clientBin = resolve(repository, process.env.ZXS_CLIENT_BIN ?? "zxs-client/target/debug/zxs-cli");
const execute = promisify(execFile);

async function freeAddress() {
  const listener = createServer().listen(0, "127.0.0.1");
  await once(listener, "listening");
  const { port } = listener.address();
  await new Promise((resolveClose, reject) => listener.close(error => error ? reject(error) : resolveClose()));
  return `127.0.0.1:${port}`;
}

async function stop(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  const exited = once(child, "exit");
  child.kill("SIGTERM");
  await exited;
}

async function waitForServer(endpoint) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const response = await fetch(`${endpoint}/containers`).catch(() => null);
    if (response?.status === 401) return;
    await delay(50);
  }
  assert.fail("The fixture server never started");
}

async function limitDownloads(t, origin) {
  const proxy = createHttpServer((request, response) => {
    const upstream = httpRequest(origin + request.url, {
      method: request.method, headers: request.headers,
    }, async reply => {
      response.writeHead(reply.statusCode, reply.headers);
      for await (const block of reply) {
        response.write(block);
        if (request.url.endsWith("/payload")) await delay(30);
      }
      response.end();
    });
    upstream.on("error", error => response.destroy(error));
    request.pipe(upstream);
  }).listen(0, "127.0.0.1");
  await once(proxy, "listening");
  t.after(() => { proxy.closeAllConnections(); proxy.close(); });
  return `http://127.0.0.1:${proxy.address().port}`;
}

async function startFixture(t, { secondDisk = false, slowDownloads = false } = {}) {
  const root = await mkdtemp(join(tmpdir(), "zxs-browser-"));
  const data = join(root, "data");
  const recycle = join(root, "recycle");
  const mirror = join(root, "mirror");
  const source = join(root, "source");
  const folder = "disk0/资料";
  const env = { ...process.env, ZXS_CONFIG_DIR: join(root, "profile"), ZXS_PASSWORD: "browser-fixture-password" };
  const children = [];
  let browser;
  t.after(async () => {
    await browser?.close();
    await Promise.all(children.map(stop));
    await rm(root, { force: true, recursive: true });
  });

  function command(binary, args) {
    return execFileSync(binary, args, { env, encoding: "utf8", timeout: 20_000 });
  }
  function background(binary, args, options = env) {
    const child = spawn(binary, args, { env: options, stdio: ["ignore", "pipe", "pipe"] });
    child.stderr.resume();
    children.push(child);
    return child;
  }

  await Promise.all([join(data, "disk0"), join(recycle, "rec0"), join(mirror, folder), join(source, folder)]
    .map(path => mkdir(path, { recursive: true })));
  if (secondDisk) await mkdir(join(data, "disk1"));
  await writeFile(join(source, folder, "原件.txt"), "published content");
  await writeFile(join(mirror, folder, ".zxsignore"), "manually added marker");
  command(serverBin, ["init", "--data-dir", data, "--recycle-dir", recycle]);
  const address = await freeAddress();
  const origin = `http://${address}`;
  const args = ["serve", "--data-dir", data, "--listen", address];
  let server = background(serverBin, args);
  await waitForServer(origin);
  const endpoint = slowDownloads ? await limitDownloads(t, origin) : origin;
  await execute(clientBin, ["login", endpoint], { env, timeout: 20_000 });
  await execute(clientBin, ["--workspace", source, "commit", `${folder}/原件.txt`], { env, timeout: 20_000 });

  const ui = background(clientBin, ["--workspace", mirror, "ui"], { ...env, PATH: "" });
  const lines = createInterface({ input: ui.stdout });
  const entrance = await Promise.race([
    once(lines, "line").then(([line]) => new URL(line)),
    once(ui, "exit").then(() => assert.fail("The fixture UI exited before listening")),
  ]);
  browser = await chromium.launch({ executablePath: process.env.ZXS_CHROMIUM_EXECUTABLE });
  return {
    browser, entrance, folder, mirror, source, data, root,
    cli: (workspace, ...args) => JSON.parse(command(clientBin, ["--workspace", workspace, ...args])),
    list: path => JSON.parse(command(clientBin, ["ls", path])).containers,
    status: () => JSON.parse(command(clientBin, ["status"])),
    async updateOriginal(contents) {
      const path = folder + "/原件.txt";
      await writeFile(join(source, path), contents);
      command(clientBin, ["--workspace", source, "commit", path]);
    },
    async restart() {
      await stop(server);
      server = background(serverBin, args);
      await waitForServer(endpoint);
    },
  };
}

test("file transfers expose real progress and preserve the browser download", { timeout: 30_000 }, async t => {
  const { browser, entrance, folder, mirror } = await startFixture(t, { slowDownloads: true });
  const contents = Buffer.alloc(4 * 1024 * 1024, 42);
  const name = "传输测试.bin";
  await rm(join(mirror, folder, ".zxsignore"));
  await writeFile(join(mirror, folder, name), contents);
  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const button = name => page.getByRole("button", { name, exact: true });
  const row = name => page.locator("#rows").getByRole("button", { name, exact: true });
  await page.goto(entrance.href);
  await button("工作区").click();
  await row("disk0").click();
  await row("资料").click();
  await page.getByRole("row").filter({ has: button(name) }).getByRole("button", { name: "提交" }).click();
  await page.getByText(/上传 100%.*4\.0 MiB \/ 4\.0 MiB.*\/s/).waitFor();
  await page.getByText("已提交 v1。", { exact: true }).waitFor();

  await button("文件").click();
  await row("disk0").click();
  await row("资料").click();
  await row(name).click();
  const downloading = page.waitForEvent("download");
  await button("下载").click();
  await page.getByText(/下载 [1-9]\d?%.*\/s/).waitFor();
  await page.getByText(/下载 100%.*\/s/).waitFor();
  const download = await downloading;
  assert.equal(download.suggestedFilename(), name);
  assert.deepEqual(await readFile(await download.path()), contents);
});

function observeApi(page) {
  const requests = [];
  page.on("request", request => {
    if (request.url().endsWith("/api")) requests.push(request.postDataJSON());
  });
  return action => requests.filter(request => request.action === action);
}

test("a historical version is restored locally before explicit submission", { timeout: 30_000 }, async t => {
  const fixture = await startFixture(t);
  const { browser, entrance, folder, mirror } = fixture;
  const file = join(mirror, folder, "原件.txt");
  await fixture.updateOriginal("latest content");
  const page = await browser.newPage();
  page.setDefaultTimeout(8_000);
  const action = name => page.getByRole("button", { name, exact: true });
  const row = name => page.locator("#rows").getByRole("button", { name, exact: true });
  await page.goto(entrance.href);
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
  await page.goto(entrance.href);
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
    await page.goto(entrance.href);
    await row("disk0").waitFor();
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
    await page.getByText("没有本地变更。", { exact: true }).waitFor();
    assert.match(await page.locator("#bundle-warning").textContent(), /资料.*\.zxsignore/);
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
    assert.deepEqual(requests("commit").at(-1), { action: "commit", path: local, choice: "stop" });
    assert.equal(await readFile(join(data, local, "v000001", name), "utf8"), "local draft");

    await button("文件").click();
    await page.getByRole("searchbox", { name: "搜索文件名" }).fill(name);
    await row(name).click();
    await page.locator(".top-actions").getByRole("button", { name: "添加标签" }).click();
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
    const bundle = project + "/需要打包";
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
    await button("提交当前目录").click();
    await dialog.getByRole("heading", { name: "目录提交结果" }).waitFor();
    assert.deepEqual(requests("commit_directory").at(-1), { action: "commit_directory", directory: "disk0" });
    assert.equal(await dialog.locator("input").count(), 0);
    assert.match(await dialog.textContent(), /已提交 2 项.*1 项待处理/);
    assert.match(await dialog.textContent(), /已忽略 disk0\/资料 中的 \.zxsignore/);
    assert.ok((await stat(join(data, empty))).isDirectory());
    assert.equal(await readFile(join(data, local, "v000001", "文件.txt"), "utf8"), "nested file");
    const archive = await readFile(join(data, bundle + ".zip", "v000001", "需要打包.zip"));
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
    const nested = `${bundle}/子目录`;
    await mkdir(join(mirror, nested, "空目录"), { recursive: true });
    await mkdir(join(mirror, bundle, "node_modules"));
    await writeFile(join(mirror, nested, "成员.txt"), "packed member");
    await writeFile(join(mirror, bundle, "node_modules", "仅本地.txt"), "ignored member");
    await button("工作区").click();
    await button("创建 .zxsignore").click();
    await dialog.locator("input[name=path]").fill(join(mirror, bundle));
    await dialog.getByRole("button", { name: "创建", exact: true }).click();
    await row(".zxsignore").waitFor();
    assert.ok(await row("node_modules").isVisible());
    await crumb("disk0").click();
    await row("目录包").click();
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
    assert.deepEqual(requests("commit").at(-1), { action: "commit", path: bundle, choice: "stop" });
    assert.ok(await row("成员.txt").isVisible());
    const archive = await readFile(join(data, `${bundle}.zip`, "v000001", "目录包.zip"));
    assert.equal(archive.subarray(30, 40).toString(), ".zxsignore");

    await button("文件").click();
    await row("disk0").click();
    await row("目录包.zip").click();
    await button("检出并浏览").click();
    await row("子目录").waitFor();
    assert.deepEqual(requests("open"), []);
    assert.equal(await readFile(join(mirror, bundle, "node_modules", "仅本地.txt"), "utf8"), "ignored member");
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
