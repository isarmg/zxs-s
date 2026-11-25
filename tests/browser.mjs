import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const repository = fileURLToPath(new URL("../", import.meta.url));
const serverBin = resolve(repository, process.env.ZXS_SERVER_BIN ?? "zxs-server/target/debug/zxs");
const clientBin = resolve(repository, process.env.ZXS_CLIENT_BIN ?? "zxs-client/target/debug/zxs-cli");

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

async function startFixture(t) {
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
  await writeFile(join(source, folder, "原件.txt"), "published content");
  await writeFile(join(mirror, folder, ".zxsignore"), "manually added marker");
  command(serverBin, ["init", "--data-dir", data, "--recycle-dir", recycle]);
  const address = await freeAddress();
  const endpoint = `http://${address}`;
  const args = ["serve", "--data-dir", data, "--listen", address];
  let server = background(serverBin, args);
  await waitForServer(endpoint);
  command(clientBin, ["login", endpoint]);
  command(clientBin, ["--workspace", source, "commit", `${folder}/原件.txt`]);

  const ui = background(clientBin, ["--workspace", mirror, "ui"], { ...env, PATH: "" });
  const lines = createInterface({ input: ui.stdout });
  const entrance = await Promise.race([
    once(lines, "line").then(([line]) => new URL(line)),
    once(ui, "exit").then(() => assert.fail("The fixture UI exited before listening")),
  ]);
  browser = await chromium.launch();
  return {
    browser, entrance, folder, mirror, data,
    list: path => JSON.parse(command(clientBin, ["ls", path])).containers,
    async restart() {
      await stop(server);
      server = background(serverBin, args);
      await waitForServer(endpoint);
    },
  };
}

function observeApi(page) {
  const requests = [];
  page.on("request", request => {
    if (request.url().endsWith("/api")) requests.push(request.postDataJSON());
  });
  return action => requests.filter(request => request.action === action);
}

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
    await page.getByText("已同步 1 项到本地工作区。", { exact: true }).waitFor();
    assert.equal(new URL(page.url()).hash, "");
    const [cookie] = await context.cookies(entrance.origin);
    assert.ok(cookie?.httpOnly);
    assert.equal(cookie.sameSite, "Strict");
    assert.equal((await post("/access", access)).status, 401);
    assert.equal(await readFile(join(mirror, folder, "原件.txt"), "utf8"), "published content");
    assert.ok(await row("disk0").isVisible());
    assert.ok(await button("提交本地文件").isHidden());
    await button("工作区").click();
    await page.getByText("没有本地变更。", { exact: true }).waitFor();
    assert.match(await page.locator("#bundle-warning").textContent(), /资料.*\.zxsignore/);
  });

  await t.test("one-click upload uses the workspace path and tags are editable afterwards", async () => {
    const name = "新增.txt";
    const local = `${folder}/${name}`;
    await writeFile(join(mirror, local), "local draft");
    await button("同步并刷新").click();
    await row("disk0").click();
    await row("资料").click();
    await button("提交本地文件").click();
    assert.deepEqual(await dialog.locator("input").evaluateAll(inputs => inputs.map(input => input.name)), ["path"]);
    await dialog.getByRole("button", { name: "取消", exact: true }).click();
    await dialog.waitFor({ state: "hidden" });
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
    await row("目录包").click();
    await button("检出并浏览").click();
    await row("子目录").waitFor();
    assert.deepEqual(requests("open"), []);
    assert.equal(await readFile(join(mirror, bundle, "node_modules", "仅本地.txt"), "utf8"), "ignored member");
  });

  await t.test("the active browser session reconnects after server restart", async () => {
    await fixture.restart();
    await button("文件").click();
    await row("disk0").click();
    await row("目录包").waitFor();
    assert.ok(await dialog.isHidden());
    assert.deepEqual(errors, []);
  });
});
