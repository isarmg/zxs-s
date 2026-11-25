#!/usr/bin/env python3
"""Exercise the actual ZXS server, desktop CLI and C ABI on Linux."""

import base64
import ctypes
import http.client
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlencode
import zipfile

ROOT = Path(__file__).resolve().parents[1]
SERVER = Path(os.environ.get("ZXS_SERVER_BIN", ROOT / "zxs-server/target/debug/zxs"))
CLIENT = Path(os.environ.get("ZXS_CLIENT_BIN", ROOT / "zxs-client/target/debug/zxs-cli"))
FFI = Path(os.environ.get("ZXS_FFI_LIB", ROOT / "zxs-client/target/debug/libzxs_ffi.so"))
PASSWORD = "smoke-only-password"
EMPTY_HASH = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"


class Test:
    def __init__(self, root, recycle):
        self.root = root
        self.data = root / "data"
        self.recycle = recycle
        self.config = root / "profile"
        self.a, self.b, self.c = (root / n for n in ("one", "two", "three"))
        for workspace in (self.a, self.b, self.c):
            workspace.mkdir()
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            self.port = listener.getsockname()[1]
        self.endpoint = f"http://127.0.0.1:{self.port}"
        self.env = dict(os.environ, ZXS_CONFIG_DIR=str(self.config), ZXS_PASSWORD=PASSWORD)
        self.process = None
        self.token = None

    def local(self, binary, args, expected=0):
        process = subprocess.run([binary, *map(str, args)], env=self.env,
                                 timeout=60, text=True, capture_output=True)
        assert process.returncode == expected, (
            list(args), process.returncode, process.stdout, process.stderr
        )
        return process.stdout

    def offline(self, command, *args):
        out = self.local(SERVER, ["--data-dir", self.data, "--recycle-dir", self.recycle, command, *args])
        if command == "init":
            config = self.data / ".system/config.toml"
            config.write_text(config.read_text().replace("127.0.0.1:7738", f"127.0.0.1:{self.port}"))
        return out

    def cli(self, workspace, *args, expected=0):
        out = self.local(CLIENT, ["--workspace", workspace, *args], expected)
        return json.loads(out) if out.strip() else None

    def start(self):
        self.log = (self.root / "server.log").open("ab")
        self.process = subprocess.Popen(
            [SERVER, "serve", "--data-dir", self.data],
            stdout=self.log, stderr=self.log, env=self.env,
        )
        for i in range(200):
            if self.process.poll() is not None:
                raise AssertionError((self.root / "server.log").read_text())
            try:
                with socket.create_connection(("127.0.0.1", self.port), timeout=.1):
                    return
            except OSError:
                time.sleep(.025)
        raise AssertionError("server did not start")

    def stop(self, crash=False):
        if self.process:
            if crash:
                self.process.kill()
            else:
                self.process.terminate()
            self.process.wait(timeout=10)
            self.process = None
            self.log.close()

    def http(self, method, path, status, body=None, headers=None, authenticated=True):
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=8)
        try:
            headers = dict(headers or {})
            if authenticated and self.token:
                headers["Authorization"] = f"Bearer {self.token}"
            if isinstance(body, dict):
                headers["Content-Type"] = "application/json"
                body = json.dumps(body).encode()
            conn.request(method, path, body=body, headers=headers)
            response = conn.getresponse()
            payload = response.read()
            assert response.status == status, (method, path, status, response.status, payload[:300])
            if response.getheader("Content-Type", "").startswith("application/json"):
                payload = json.loads(payload)
            return payload, response.headers
        finally:
            conn.close()

    def partial(self, op, params, length):
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=4)
        conn.putrequest("POST", "/containers")
        for key, value in upload_headers(op, params, EMPTY_HASH).items():
            conn.putheader(key, value)
        conn.putheader("Authorization", f"Bearer {self.token}")
        conn.putheader("Content-Length", str(length))
        conn.endheaders()
        return conn

    def path(self, cid):
        return self.data / "containers" / cid[:2] / cid


def upload_headers(op, params, digest):
    encoded = base64.urlsafe_b64encode(json.dumps(params, ensure_ascii=False).encode()).rstrip(b"=")
    return {"Content-Type": "application/octet-stream", "Zxs-Params": encoded.decode(),
            "Zxs-Blake3": digest, "Zxs-Op-Id": op}


def session_and_versions(t):
    t.http("GET", "/containers", 401, authenticated=False)
    t.http("POST", "/sessions", 401, {"password": "wrong"}, {"Zxs-Op-Id": "a" * 32})
    login, _ = t.http("POST", "/sessions", 201, {"password": PASSWORD}, {"Zxs-Op-Id": "a" * 32})
    t.token = login["token"]
    replay, _ = t.http("POST", "/sessions", 201, {"password": PASSWORD}, {"Zxs-Op-Id": "a" * 32})
    assert replay == login
    t.cli(t.a, "login", t.endpoint)
    source = t.a / "demo.txt"
    source.write_bytes(b"abc")
    params = {"folder": "/文档", "name": "demo.txt", "tags": ["work"]}
    first = t.cli(t.a, "commit", source, "--folder", "/文档", "--tag", "work")["data"]
    cid = first["container"]["container_id"]
    version = first["version"]
    assert cid[12] == "7"
    assert (t.path(cid) / "versions/v000001/payload.txt").read_bytes() == b"abc"
    headers = upload_headers(version["op_id"], params, version["blake3"])
    assert t.http("POST", "/containers", 201, b"abc", headers)[0] == first
    t.http("POST", "/containers", 422, b"abc", upload_headers(version["op_id"], {**params, "name": "different"}, version["blake3"]))
    receipt, _ = t.http("GET", f"/ops/{version['op_id']}", 200)
    assert receipt["result"] == first and receipt["status"] == 201
    t.http("GET", "/ops/" + "b" * 32, 404)
    preflight = t.partial("b" * 32, {**params, "name": "DEMO.txt"}, 100_000_000)
    try:
        assert preflight.getresponse().status == 409
    finally:
        preflight.close()

    t.cli(t.b, "checkout", cid, "--to", "stale.txt")
    source.write_text("version two")
    status = t.cli(t.a, "status")[0]
    assert status["state"] == "modified" and status["bundle"] is False
    assert t.cli(t.a, "commit", source)["data"]["version"]["version"] == 2
    (t.b / "stale.txt").write_text("version three")
    assert t.cli(t.b, "commit", "stale.txt", expected=3) is None
    assert t.cli(t.b, "commit", "stale.txt", "--on-conflict", "append")["data"]["version"]["version"] == 3
    source.write_text("copy")
    copy = t.cli(t.a, "commit", source, "--on-conflict", "new", "--name", "copy.txt")["data"]
    assert copy["container"]["container_id"] != cid

    t.cli(t.c, "checkout", cid, "--version", 1, "--to", "pull.txt")
    (t.c / "pull.txt").write_text("save this edit")
    result = t.cli(t.c, "commit", "pull.txt", "--on-conflict", "pull")
    assert result["outcome"] == "pulled"
    assert Path(result["data"]["backup"]).read_text() == "save this edit"
    assert (t.c / "pull.txt").read_text() == "version three"
    reverted = t.cli(t.b, "revert", cid, 1, "--expected-version", 3)
    assert reverted["version"]["version"] == 4
    updated = t.cli(t.b, "patch", cid, "--name", "发布.txt", "--folder", "/文档/已归档",
                    "--tags", "work,archive")
    assert updated["name"] == "发布.txt"
    detail = t.cli(t.b, "detail", cid)
    assert [item["version"] for item in detail["versions"]] == [1, 2, 3, 4]
    page = t.cli(t.b, "ls", "/文档", "--tag", "work", "--q", "发布")
    assert page["folders"] == [{"folder": "/文档/已归档", "count": 1}]
    query = urlencode({"folder": "/文档/已归档", "tag": "archive", "q": "发", "limit": 1})
    filtered = t.http("GET", "/containers?" + query, 200)[0]
    assert set(filtered) == {"containers", "folders", "next_cursor"}
    assert filtered["containers"] == [updated]
    assert set(updated) == {"container_id", "folder", "name", "tags", "latest_version", "created_at", "updated_at"}
    assert detail["versions"][-1]["size"] == 3
    return cid, version["op_id"]


def payload_and_recycle(t, cid):
    url = f"/containers/{cid}/versions/4/payload"
    payload, headers = t.http("GET", url, 206, headers={"Range": "bytes=1-2"})
    assert payload == b"bc" and headers["Content-Range"] == "bytes 1-2/3"
    assert t.http("GET", url, 206, headers={"Range": "bytes=-2"})[0] == b"bc"
    assert t.http("HEAD", url, 200)[1]["Content-Length"] == "3"
    assert headers["ETag"].startswith('"') and len(headers["ETag"]) == 66
    assert t.http("GET", url, 416, headers={"Range": "bytes=0-0,2-2"})[1]["Content-Range"] == "bytes */3"
    assert t.http("GET", url + "?token=" + t.token, 200, authenticated=False)[0] == b"abc"
    t.http("GET", f"/containers/{cid}?token=" + t.token, 401, authenticated=False)
    entry = t.cli(t.a, "recycle", cid)
    rid = entry["recycle"]["recycle_id"]
    assert not t.path(cid).exists()
    assert (t.recycle / "containers" / rid[:2] / rid / "container/versions/v000004/payload.txt").is_file()
    assert t.cli(t.a, "recycle")["entries"][0]["recycle"]["recycle_id"] == rid
    t.http("GET", url, 404)
    t.cli(t.a, "restore", rid, "--folder", "/还原")
    restored, _ = t.http("GET", f"/containers/{cid}", 200)
    assert restored["container"]["folder"] == "/还原"
    assert t.http("GET", url, 200)[0] == b"abc"


def multiblock_payload(t):
    block = bytes(range(256)) * 16384
    source = t.a / "stream.bin"
    with source.open("wb") as file:
        file.write(block)
        file.write(block)
        file.write(b"tail")
    commit = t.cli(t.a, "commit", source)["data"]
    cid = commit["container"]["container_id"]
    url = f"/containers/{cid}/versions/1/payload"
    start = len(block) - 4
    received, _ = t.http("GET", url, 206, headers={"Range": f"bytes={start}-{start + 7}"})
    assert received == block[-4:] + block[:4]
    result = t.cli(t.b, "checkout", cid, "--to", "stream.bin")
    assert Path(result["path"]).read_bytes() == source.read_bytes()


def bundles(t):
    project = t.a / "project"
    project.mkdir()
    t.cli(t.a, "init-bundle", project)
    manifest = project / ".zxsignore"
    with manifest.open("a", encoding="utf-8") as file:
        file.write("*.local\n")
    (project / ".gitignore").write_text("source.txt\n")
    (project / "source.txt").write_text("included by ZXS")
    (project / "private.local").write_text("not uploaded")
    (project / "big.bin").write_bytes(b"x" * (4 * 1024 * 1024 + 1))
    (project / "empty").mkdir()
    first = t.cli(t.a, "commit", "project")["data"]
    cid = first["container"]["container_id"]
    stored = t.path(cid) / "versions/v000001/payload.zip"
    with zipfile.ZipFile(stored) as archive:
        assert archive.namelist() == [".gitignore", ".zxsignore", "big.bin", "empty/", "source.txt"]
        assert all(info.date_time == (1980, 1, 1, 0, 0, 0) for info in archive.infolist())
        assert archive.testzip() is None
    same = t.cli(t.a, "commit", "project")["data"]
    assert first["version"]["blake3"] == same["version"]["blake3"]
    t.cli(t.b, "checkout", cid, "--to", "project")
    local = t.b / "project"
    (local / "private.local").write_text("keep local")
    status = t.cli(t.b, "status")[0]
    assert status["state"] == "clean" and status["bundle"] is True
    (project / "source.txt").unlink()
    (project / "next.txt").write_text("updated")
    with manifest.open("a", encoding="utf-8") as file:
        file.write("big.bin\n")
    t.cli(t.a, "commit", "project")
    t.cli(t.b, "checkout", cid, "--to", "project")
    assert not (local / "source.txt").exists()
    assert (local / "next.txt").read_text() == "updated"
    assert (local / "big.bin").stat().st_size == 4 * 1024 * 1024 + 1
    assert (local / "private.local").read_text() == "keep local"

    (local / ".zxsignore").unlink()
    (local / "next.txt").write_text("uncommitted local edit")
    (local / "new-local.txt").write_text("untracked local file")
    t.cli(t.b, "checkout", cid, "--to", "project", expected=3)
    checked = t.cli(t.b, "checkout", cid, "--to", "project", "--backup")
    with zipfile.ZipFile(checked["backup"]) as archive:
        assert archive.read("next.txt") == b"uncommitted local edit"
        assert archive.read("new-local.txt") == b"untracked local file"
    assert (local / ".zxsignore").is_file()
    assert (local / "next.txt").read_text() == "updated"
    assert (local / "private.local").read_text() == "keep local"

    with zipfile.ZipFile(t.a / "unsafe.zip", "w") as archive:
        archive.writestr(".zxsignore", "")
        archive.writestr("../escaped", "unexpected")
    hostile = t.cli(t.a, "commit", "unsafe.zip")["data"]
    t.cli(t.b, "checkout", hostile["container"]["container_id"], "--to", "unsafe", expected=1)
    assert not (t.b / "escaped").exists() and not (t.b / "unsafe").exists()


def recovery(t, cid, op):
    broken = t.partial("b" * 32, {"folder": "/", "name": "crash.bin"}, 100_000_000)
    broken.send(b"x" * 128_000)
    for i in range(100):
        if list(t.data.glob("containers/*/*/versions/.staging-*/payload.bin")):
            break
        time.sleep(.025)
    else:
        raise AssertionError("upload did not enter staging")
    t.stop(crash=True)
    broken.close()
    (t.data / ".system/index.sqlite").unlink()
    for suffix in ("-wal", "-shm"):
        (t.data / f".system/index.sqlite{suffix}").unlink(missing_ok=True)
    t.start()
    t.http("GET", f"/ops/{op}", 200)
    t.http("GET", "/ops/" + "b" * 32, 404)
    assert not list(t.data.glob("containers/*/*/versions/.staging-*"))
    assert not list((t.data / ".system/staging").iterdir())
    t.stop()
    assert "Pruned 2 versions" in t.offline("prune", "--keep-last", 2, "--container", cid)
    assert "Verified" in t.offline("verify")
    assert "Index rebuilt" in t.offline("rebuild-index")
    t.start()
    detail, _ = t.http("GET", f"/containers/{cid}", 200)
    assert [v["version"] for v in detail["versions"]] == [3, 4]
    (t.c / "pull.txt").write_text("next after prune")
    committed = t.cli(t.c, "commit", "pull.txt", "--on-conflict", "append")["data"]
    assert committed["version"]["version"] == 5
    t.stop()
    change = committed["version"]["op_id"]
    system = t.data / ".system"
    record = system / "operations" / change[:2] / (change + ".json")
    pending = {"change": {"action": "Publish", "commit": committed}, "record": json.loads(record.read_bytes())}
    (system / "inflight" / cid).write_text(json.dumps(pending))
    previous = committed["container"] | {"latest_version": 4}
    metadata = t.path(cid) / "container.json"
    metadata.write_text(json.dumps(previous))
    record.unlink()
    t.start()
    assert json.loads(metadata.read_bytes()) == committed["container"]
    assert not (system / "inflight" / cid).exists()
    assert t.http("GET", f"/ops/{change}", 200)[0]["result"] == committed


@contextmanager
def lose_reply(port, *, interrupt_receipt=False):
    class Proxy(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"
        lost = False
        receipt_interrupted = False

        def log_message(self, *args):
            pass

        def do_request(self):
            if interrupt_receipt and self.path.startswith("/ops/") and Proxy.lost and not Proxy.receipt_interrupted:
                Proxy.receipt_interrupted = True
                self.send_response(503)
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            length = int(self.headers.get("Content-Length", 0))
            body = self.rfile.read(length) if length else None
            headers = {k: v for k, v in self.headers.items() if k.lower() not in ("expect", "host")}
            with context_connection(port) as remote:
                remote.request(self.command, self.path, body, headers)
                reply = remote.getresponse()
                content = reply.read()
                if self.path == "/containers" and self.command == "POST" and reply.status == 201 and not Proxy.lost:
                    Proxy.lost = True
                    self.close_connection = True
                    return
                self.send_response(reply.status)
                self.send_header("Content-Type", reply.getheader("Content-Type", "text/plain"))
                self.send_header("Content-Length", str(len(content)))
                self.end_headers()
                self.wfile.write(content)

        do_POST = do_GET = do_DELETE = do_request

    server = ThreadingHTTPServer(("127.0.0.1", 0), Proxy)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}"
        assert Proxy.lost
    finally:
        server.shutdown()
        server.server_close()


@contextmanager
def context_connection(port):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=8)
    try:
        yield connection
    finally:
        connection.close()


def ffi(t):
    alias = ctypes.CDLL(str(FFI))
    alias.zxs_call.argtypes = [ctypes.c_char_p, ctypes.c_size_t]
    alias.zxs_call.restype = ctypes.c_void_p
    alias.zxs_free.argtypes = [ctypes.c_void_p]
    alias.zxs_free.restype = None

    def call(action, **fields):
        request = json.dumps({"action": action, **fields}, ensure_ascii=False).encode()
        pointer = alias.zxs_call(request, len(request))
        assert pointer
        try:
            reply = json.loads(ctypes.string_at(pointer))
            assert reply["ok"], reply
            return reply["data"]
        finally:
            alias.zxs_free(pointer)

    login = call("login", server=t.endpoint, password=PASSWORD)
    connection = {"server": t.endpoint, "token": login["session"]["token"]}
    sample = t.root / "例.txt"
    sample.write_bytes(b"original")
    fields = {"connection": connection, "file": str(sample), "name": sample.name, "folder": "/手机"}
    entry = call("backup", **fields)["container"]
    assert call("backup", **fields)["already_present"]
    result = call("browse", connection=connection, folder="/手机")
    cid = entry["container_id"]
    assert result["containers"] == [entry]
    assert set(result) == {"containers", "folders", "next_cursor"}
    args = {"connection": connection, "id": cid, "cache": str(t.root / "documents")}
    first = call("open", **args)
    assert Path(first["path"]).read_bytes() == b"original"
    assert call("open", **args)["path"] == first["path"]
    sample.write_bytes(b"next mobile version")
    assert call("backup", **fields)["version"]["version"] == 2
    assert call("browse", connection=connection, folder="/手机")["containers"][0]["latest_version"] == 2
    latest = call("open", **args)
    assert latest["path"] != first["path"]
    assert Path(latest["path"]).read_bytes() == b"next mobile version"
    clip = t.root / "clip.mp4"
    clip.write_bytes(b"media fixture")
    video = call("backup", **(fields | {"file": str(clip), "name": clip.name}))["container"]
    stream = call("open", **(args | {"id": video["container_id"]}))
    assert stream["type"] == "video/mp4" and "token=" in stream["url"]
    call("logout", connection=connection)


def lost_replies(t):
    for name, edit in [("receipt-retry.txt", None), ("receipt-edited.txt", "new edits after timeout")]:
        with lose_reply(t.port, interrupt_receipt=True) as proxy:
            t.cli(t.c, "login", proxy)
            (t.c / name).write_text("first confirmed on server")
            t.cli(t.c, "commit", name, expected=1)
            statuses = t.cli(t.c, "status")
            pending = next(item for item in statuses if item["path"] == name)
            assert pending["state"] == "pending" and pending["op_id"]
            if edit is not None:
                (t.c / name).write_text(edit)
            committed = t.cli(t.c, "commit", name)["data"]
            version = committed["version"]["version"]
            cid = committed["container"]["container_id"]
            assert version == (1 if edit is None else 2)
            if edit is not None:
                body, _ = t.http("GET", f"/containers/{cid}/versions/2/payload", 200)
                assert body == edit.encode()
            refreshed = t.cli(t.c, "refresh")
            assert next(item for item in refreshed if item["path"] == name)["state"] == "clean"
            t.cli(t.c, "logout")


def session_receipts(t):
    logout = {"Zxs-Op-Id": "c" * 32}
    t.http("DELETE", "/sessions/current", 204, headers=logout)
    t.http("DELETE", "/sessions/current", 204, headers=logout)
    t.http("GET", "/containers", 401)
    for op in ("a" * 32, logout["Zxs-Op-Id"], "0" * 32):
        t.http("DELETE", "/sessions/current", 401, headers={"Zxs-Op-Id": op,
               "Authorization": "Bearer " + "0" * 64}, authenticated=False)
    t.http("POST", "/sessions", 401, {"password": PASSWORD}, {"Zxs-Op-Id": "a" * 32})
    fresh, _ = t.http("POST", "/sessions", 201, {"password": PASSWORD}, {"Zxs-Op-Id": "1" * 32})
    receipt, _ = t.http("GET", "/ops/" + "a" * 32, 200,
                         headers={"Authorization": "Bearer " + fresh["token"]}, authenticated=False)
    assert "token" not in receipt["result"]
    assert receipt["status"] == 201 and receipt["result"]["expires_at"] > 0


def main():
    with tempfile.TemporaryDirectory(prefix="zxs-smoke-") as root, tempfile.TemporaryDirectory(
        prefix="zxs-recycle-", dir=os.environ.get("ZXS_SMOKE_RECYCLE_PARENT")
    ) as recycle:
        t = Test(Path(root), Path(recycle))
        t.offline("init")
        imported = t.root / "import"
        imported.mkdir()
        (imported / "empty.txt").touch()
        (imported / "PHOTO.JPG").write_bytes(b"image")
        assert "Imported 2 containers" in t.offline("import", imported, "--folder", "/导入")
        try:
            t.start()
            cid, op = session_and_versions(t)
            photo = t.cli(t.a, "ls", "/导入", "--q", "photo")["containers"][0]
            photo_path = t.path(photo["container_id"]) / "versions/v000001/payload.jpg"
            assert photo_path.read_bytes() == b"image"
            payload_and_recycle(t, cid)
            multiblock_payload(t)
            bundles(t)
            recovery(t, cid, op)
            ffi(t)
            with lose_reply(t.port) as proxy:
                t.cli(t.c, "login", proxy)
                (t.c / "loss.txt").write_text("committed before connection broke")
                committed = t.cli(t.c, "commit", "loss.txt")["data"]
                receipt, _ = t.http("GET", f"/ops/{committed['version']['op_id']}", 200)
                assert receipt["status"] == 201
            lost_replies(t)
            session_receipts(t)
        except Exception:
            print((t.root / "server.log").read_text())
            raise
        finally:
            t.stop()
        print("ZXS server, CLI and C ABI smoke checks passed")


if __name__ == "__main__":
    main()
