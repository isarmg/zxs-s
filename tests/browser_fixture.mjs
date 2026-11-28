import assert from "node:assert/strict";
import { execFile, execFileSync, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer as createHttpServer, request as httpRequest } from "node:http";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { chromium } from "playwright";

const repository = fileURLToPath(new URL("../", import.meta.url));
const serverBin = resolve(repository, process.env.ZXS_SERVER_BIN ?? "zxs-server/target/debug/zxs");
const clientBin = resolve(repository, process.env.ZXS_CLIENT_BIN ?? "zxs-client/target/debug/zxs-cli");
const prepareBin = resolve(repository, process.env.ZXS_PREPARE_BIN ?? "tools/zxs-prepare/target/debug/zxs-prepare");
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

async function browserFixture(t) {
  const root = await mkdtemp(join(tmpdir(), "zxs-browser-"));
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

  async function openUi(binary, args) {
    const ui = background(binary, args, { ...env, PATH: "" });
    const lines = createInterface({ input: ui.stdout });
    const entrance = await Promise.race([
      once(lines, "line").then(([line]) => new URL(line)),
      once(ui, "exit").then(() => assert.fail("The fixture UI exited before listening")),
    ]);
    browser = await chromium.launch({ executablePath: process.env.ZXS_CHROMIUM_EXECUTABLE });
    return { browser, entrance };
  }

  return { root, env, command, background, openUi };
}

export async function startPrepareFixture(t) {
  const { root, openUi } = await browserFixture(t);
  const { browser, entrance } = await openUi(prepareBin, ["ui"]);
  return { root, browser, entrance };
}

export async function startFixture(t, { secondDisk = false, slowDownloads = false } = {}) {
  const { root, env, command, background, openUi } = await browserFixture(t);
  const data = join(root, "data");
  const recycle = join(root, "recycle");
  const mirror = join(root, "mirror");
  const source = join(root, "source");
  const folder = "disk0/资料";

  await Promise.all([join(data, "disk0"), join(recycle, "rec0"), join(mirror, folder), join(source, folder)]
    .map(path => mkdir(path, { recursive: true })));
  if (secondDisk) {
    await mkdir(join(data, "disk1"));
    command(prepareBin, ["init", join(data, "disk1"), "--kind", "data"]);
  }
  command(prepareBin, ["init", join(data, "disk0"), "--kind", "data"]);
  command(prepareBin, ["init", join(recycle, "rec0"), "--kind", "recycle"]);
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

  const { browser, entrance } = await openUi(clientBin, ["--workspace", mirror, "ui"]);
  return {
    browser, entrance, folder, mirror, source, data, root, endpoint,
    cli: (workspace, ...args) => JSON.parse(command(clientBin, ["--workspace", workspace, ...args]) || "null"),
    list: path => JSON.parse(command(clientBin, ["ls", path])).containers,
    status: () => JSON.parse(command(clientBin, ["status"])),
    stopServer: () => stop(server),
    async updateOriginal(contents) {
      const path = folder + "/原件.txt";
      await writeFile(join(source, path), contents);
      command(clientBin, ["--workspace", source, "commit", path]);
    },
    async restart(password) {
      await stop(server);
      if (password !== undefined) {
        await execute(serverBin, ["--data-dir", data, "reset-password"], {
          env: { ...env, ZXS_PASSWORD: password }, timeout: 20_000,
        });
      }
      server = background(serverBin, args);
      await waitForServer(endpoint);
    },
  };
}
