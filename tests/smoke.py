#!/usr/bin/env python3
"""Exercise the actual zxs-s server, desktop CLI and C ABI on Linux."""

import base64
import ctypes
import http.client
import json
import os
from pathlib import Path
import socket
import sqlite3
import stat
import subprocess
import tempfile
import threading
import time
from contextlib import contextmanager
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlencode
import zipfile

ROOT = Path(__file__).resolve().parents[1]
SERVER = Path(os.environ.get("ZXS_SERVER_BIN", ROOT / "zxs-server/target/debug/zxs"))
CLIENT = Path(os.environ.get("ZXS_CLIENT_BIN", ROOT / "zxs-client/target/debug/zxs-cli"))
PREPARE = Path(os.environ.get("ZXS_PREPARE_BIN", ROOT / "tools/zxs-prepare/target/debug/zxs-prepare"))
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
            (workspace / "disk1").mkdir(parents=True)
        for parent, disks in ((self.data, ("disk1", "disk2")), (self.recycle, ("rec1", "rec2"))):
            for disk in disks:
                (parent / disk).mkdir(parents=True)
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
        if command == "init":
            for parent, kind, names in ((self.data, "data", ("disk1", "disk2")),
                                        (self.recycle, "recycle", ("rec1", "rec2"))):
                for name in names:
                    self.prepare_disk(parent / name, kind)
        out = self.local(SERVER, ["--data-dir", self.data, "--recycle-dir", self.recycle, command, *args])
        if command == "init":
            config = self.data / ".zxs/config.toml"
            fresh = config.read_text()
            assert 'listen = "0.0.0.0:7738"' in fresh
            assert "layout =" not in fresh
            assert 'kind = "data"' in (self.data / "disk1/.zxs/disk.toml").read_text()
            assert 'kind = "recycle"' in (self.recycle / "rec1/.zxs/disk.toml").read_text()
        return out

    def prepare_disk(self, disk, kind):
        return json.loads(self.local(PREPARE, ["init", disk, "--kind", kind]))

    def login_session(self):
        login, _ = self.http("POST", "/sessions", 201, {"password": PASSWORD})
        self.token = login["token"]
        return self.token

    def cli(self, workspace, *args, expected=0):
        out = self.local(CLIENT, ["--workspace", workspace, *args], expected)
        return json.loads(out) if out.strip() else None

    def start(self):
        self.log = (self.root / "server.log").open("ab")
        self.process = subprocess.Popen(
            [SERVER, "serve", "--data-dir", self.data, "--listen", f"127.0.0.1:{self.port}"],
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
            if method != "HEAD" and response.getheader("Content-Type", "").startswith("application/json"):
                payload = json.loads(payload)
            return payload, response.headers
        finally:
            conn.close()

    def partial(self, params, length):
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=4)
        conn.putrequest("POST", "/containers")
        for key, value in upload_headers(params, EMPTY_HASH).items():
            conn.putheader(key, value)
        conn.putheader("Authorization", f"Bearer {self.token}")
        conn.putheader("Content-Length", str(length))
        conn.endheaders()
        return conn

    def path(self, container):
        return self.data / container["folder"].lstrip("/") / container["name"]

    def archived(self, entry):
        recycle = entry["recycle"]
        date = datetime.fromtimestamp(recycle["recycled_at"], timezone.utc).date()
        return self.recycle / entry["recycle_disk"] / f"{date}_{recycle['recycle_id']}"


def upload_headers(params, digest=None):
    encoded = base64.urlsafe_b64encode(json.dumps(params, ensure_ascii=False, separators=(",", ":")).encode()).rstrip(b"=")
    headers = {"Content-Type": "application/octet-stream", "Zxs-Params": encoded.decode()}
    if digest is not None:
        headers["Zxs-Blake3"] = digest
    return headers


def session_and_versions(t):
    t.http("GET", "/containers", 401, authenticated=False)
    t.http("POST", "/sessions", 401, {"password": "wrong"})
    login, _ = t.http("POST", "/sessions", 201, {"password": PASSWORD})
    t.token = login["token"]
    second, _ = t.http("POST", "/sessions", 201, {"password": PASSWORD})
    assert len(t.token) == 64 and second["token"] != t.token
    t.cli(t.a, "login", t.endpoint)
    assert set(json.loads((t.config / "profile.json").read_bytes())) == {"endpoint", "password"}
    source = t.a / "demo.txt"
    source.write_bytes(b"abc")
    params = {"folder": "/disk1/文档", "name": "demo.txt", "tags": ["work"]}
    first = t.cli(t.a, "commit", source, "--folder", "/disk1/文档", "--tag", "work")["data"]
    cid = first["container"]["container_id"]
    version = first["version"]
    assert cid[12] == "7"
    original = t.path(first["container"]) / "v000001/demo.txt"
    assert original.read_bytes() == b"abc" and stat.S_IMODE(original.stat().st_mode) == 0o444
    headers = upload_headers(params, version["blake3"])
    t.http("POST", "/containers", 409, b"abc", headers)
    preflight = t.partial({**params, "name": "DEMO.txt"}, 100_000_000)
    try:
        assert preflight.getresponse().status == 409
    finally:
        preflight.close()

    t.cli(t.b, "checkout", cid, "--to", "stale.txt")
    t.cli(t.c, "checkout", cid, "--to", "pull.txt")
    source.write_text("version two")
    status = next(row for row in t.cli(t.a, "status")["entries"] if row["path"] == "demo.txt")
    assert status["state"] == "modified" and status["bundle"] is False
    assert t.cli(t.a, "commit", source)["data"]["version"]["version"] == 2
    (t.b / "stale.txt").write_text("version three")
    assert t.cli(t.b, "commit", "stale.txt", expected=3) is None
    assert t.cli(t.b, "commit", "stale.txt", "--on-conflict", "append")["data"]["version"]["version"] == 3
    source.write_text("copy")
    copy = t.cli(t.a, "commit", source, "--on-conflict", "new", "--name", "copy.txt")["data"]
    assert copy["container"]["container_id"] != cid

    (t.c / "pull.txt").write_text("save this edit")
    history_reads = []

    def trace(request, _response, _data):
        if request.command == "GET" and request.path == f"/containers/{cid}":
            history_reads.append(request.path)
        return True

    with proxy_requests(t.port, trace) as endpoint:
        t.cli(t.c, "login", endpoint)
        result = t.cli(t.c, "commit", "pull.txt", "--on-conflict", "pull")
    t.cli(t.c, "login", t.endpoint)
    assert history_reads == [f"/containers/{cid}"]
    assert result["outcome"] == "pulled"
    assert Path(result["data"]["backup"]).read_text() == "save this edit"
    assert (t.c / "pull.txt").read_text() == "version three"
    restored = t.cli(t.b, "checkout", cid, "--version", 1)
    assert restored["restored_from"] == 1
    assert t.cli(t.b, "commit", "stale.txt")["data"]["version"]["version"] == 4
    t.http("POST", f"/containers/{cid}/versions", 400, b"",
           upload_headers({"expected_version": 4, "revert_to_version": 1}))
    updated = t.cli(t.b, "patch", cid, "--name", "发布.txt", "--folder", "/disk1/文档/已归档",
                    "--tags", "work,archive")
    assert updated["name"] == "发布.txt"
    assert not t.path(first["container"]).exists()
    previous = t.path(updated) / "v000001/demo.txt"
    original = t.path(updated) / "v000004/demo.txt"
    assert original.read_bytes() == previous.read_bytes() == b"abc"
    detail = t.cli(t.b, "detail", cid)
    assert [item["version"] for item in detail["versions"]] == [1, 2, 3, 4]
    page = t.cli(t.b, "ls", "--tag", "work", "--q", "发布")
    assert page["folders"] == [] and page["containers"] == [updated]
    folders = t.http("GET", "/folders?" + urlencode({"folder": "/disk1/文档"}), 200)[0]
    assert folders == {"folders": [{"folder": "/disk1/文档/已归档"}]}
    query = urlencode({"tag": "archive", "q": "发", "limit": 1})
    filtered = t.http("GET", "/containers?" + query, 200)[0]
    assert set(filtered) == {"containers", "next_cursor"}
    assert filtered["containers"] == [updated]
    assert set(updated) == {
        "container_id", "folder", "name", "tags", "latest_version",
        "latest_blake3", "created_at", "updated_at",
    }
    assert updated["latest_blake3"] == version["blake3"]
    assert detail["versions"][-1]["size"] == 3
    return cid


def payload_and_recycle(t, cid):
    url = f"/containers/{cid}/versions/4/payload"
    payload, headers = t.http("GET", url, 206, headers={"Range": "bytes=1-2"})
    assert payload == b"bc" and headers["Content-Range"] == "bytes 1-2/3"
    assert t.http("GET", url, 206, headers={"Range": "bytes=-2"})[0] == b"bc"
    assert t.http("HEAD", url, 200)[1]["Content-Length"] == "3"
    assert headers["ETag"].startswith('"') and len(headers["ETag"]) == 66
    assert t.http("GET", url, 416, headers={"Range": "bytes=0-0,2-2"})[1]["Content-Range"] == "bytes */3"
    t.http("GET", url + "?token=" + t.token, 401, authenticated=False)
    t.http("GET", f"/containers/{cid}?token=" + t.token, 401, authenticated=False)
    live = t.path(t.http("GET", f"/containers/{cid}", 200)[0]["container"])
    entry = t.cli(t.a, "recycle", "move", cid)
    rid = entry["recycle"]["recycle_id"]
    assert not live.exists()
    assert (t.data / "disk1/文档/已归档").is_dir()
    original = t.archived(entry)
    assert (original / "发布.txt/v000004/demo.txt").read_bytes() == b"abc"
    assert t.cli(t.a, "recycle")["entries"][0]["recycle"]["recycle_id"] == rid
    t.http("GET", url, 404)
    t.cli(t.a, "restore", rid, "--folder", "/disk1/还原")
    restored, _ = t.http("GET", f"/containers/{cid}", 200)
    assert restored["container"]["folder"] == "/disk1/还原"
    assert t.http("GET", url, 200)[0] == b"abc"
    assert not original.exists()


def physical_namespace(t):
    def create(folder, name, status):
        params = {"folder": folder, "name": name}
        return t.http("POST", "/containers", status, b"",
                      upload_headers(params, EMPTY_HASH))[0]

    create("/", "forbidden.txt", 400)
    t.http("POST", "/folders", 400, {"folder": "/disk4"})
    t.http("PATCH", "/folders", 400, {"from": "/disk1", "to": "/disk2/forbidden"})
    t.http("DELETE", "/folders?folder=/disk1&recursive=true", 400)
    original = create("/disk1/Case/Docs", "version.json", 201)
    path = t.data / "disk1/Case/Docs/version.json"
    assert (path / "v000001/version.json").read_bytes() == b""
    metadata = json.loads((path / "v000001/.zxs").read_bytes())
    assert set(metadata) == {"version", "created_at", "name_at_commit", "size", "blake3"}
    assert metadata["name_at_commit"] == "version.json"
    create("/disk1/case/Elsewhere", "other.txt", 409)
    create("/disk1/Case", "docs", 409)
    create("/disk1/Case/Docs/version.json/subdir", "child.txt", 409)
    create("/disk1/Case/Docs", "VERSION.JSON", 409)
    create("/disk1/Case/.ZxS", "other.txt", 400)
    create("/disk1/Case/Docs", ".ZxS", 400)
    create("/disk1/Case/Docs", "AUX.txt", 400)
    create("/disk1/Case/Docs", "trailing.", 400)
    create("/disk1/" + "/".join(["a" * 250] * 4), "b" * 21, 400)
    composed = create("/disk1/Case/Docs", "e\u0301.txt", 201)
    assert composed["container"]["name"] == "é.txt"
    assert t.path(composed["container"]).is_dir()
    create("/disk1/Case/Docs", "É.txt", 409)

    cid = original["container"]["container_id"]
    t.cli(t.a, "patch", cid, "--name", "recycle.json", "--folder", "/disk2/Cross/Docs")
    assert not path.exists()
    assert (t.data / "disk2/Cross/Docs/recycle.json/v000001/version.json").is_file()
    t.cli(t.a, "checkout", cid)
    t.cli(t.a, "commit", "disk2/Cross/Docs/recycle.json")
    entry = t.cli(t.a, "recycle", "move", cid)
    archived = t.archived(entry)
    assert (archived / ".zxs").is_file()
    assert (archived / "recycle.json/v000001/version.json").is_file()
    assert (archived / "recycle.json/v000002/recycle.json").is_file()
    t.cli(t.a, "restore", entry["recycle"]["recycle_id"], "--name", "restored.tmp", "--folder", "/disk1/Case/Docs")
    assert (t.data / "disk1/Case/Docs/restored.tmp/v000002/recycle.json").is_file()


def multiblock_payload(t):
    block = bytes(range(256)) * 16384
    source = t.a / "stream.bin"
    with source.open("wb") as file:
        file.write(block)
        file.write(block)
        file.write(b"tail")
    commit = t.cli(t.a, "commit", source, "--folder", "/disk1")["data"]
    cid = commit["container"]["container_id"]
    url = f"/containers/{cid}/versions/1/payload"
    start = len(block) - 4
    received, _ = t.http("GET", url, 206, headers={"Range": f"bytes={start}-{start + 7}"})
    assert received == block[-4:] + block[:4]
    result = t.cli(t.b, "checkout", cid, "--to", "stream.bin")
    assert Path(result["path"]).read_bytes() == source.read_bytes()


def upload_integrity_and_retry(t):
    folder = "/disk1/传输核验"
    name = "payload.bin"
    params = {"folder": folder, "name": name}
    originals = t.data / "disk1/传输核验" / name
    staging = t.data / "disk1/.zxs/staging"
    before = set(staging.iterdir())

    t.http("POST", "/containers", 400, b"first payload", upload_headers(params, EMPTY_HASH))
    assert not originals.exists()
    assert t.http("GET", f"/containers?{urlencode({'folder': folder})}", 200)[0]["containers"] == []
    assert set(staging.iterdir()) == before

    first, _ = t.http("POST", "/containers", 201, b"first payload", upload_headers(params))
    cid = first["container"]["container_id"]
    endpoint = f"/containers/{cid}"
    append = {"expected_version": 1}
    t.http("POST", f"{endpoint}/versions", 400, b"second payload", upload_headers(append, EMPTY_HASH))
    assert t.http("GET", endpoint, 200)[0]["versions"] == [first["version"]]
    assert (originals / "v000001" / name).read_bytes() == b"first payload"
    assert not (originals / "v000002").exists()
    assert set(staging.iterdir()) == before

    second, _ = t.http("POST", f"{endpoint}/versions", 201, b"second payload", upload_headers(append))
    assert second["version"]["version"] == 2
    assert t.http("GET", f"{endpoint}/versions/2/payload", 200)[0] == b"second payload"


def bundles(t):
    project = t.a / "disk1/project"
    project.mkdir()
    project = t.a / t.cli(t.a, "init-bundle", project)
    manifest = project / ".zxsignore"
    with manifest.open("a", encoding="utf-8") as file:
        file.write("*.local\n")
    (project / ".gitignore").write_text("source.txt\n")
    (project / "source.txt").write_text("included by ZXS")
    (project / "private.local").write_text("not uploaded")
    (project / "big.bin").write_bytes(b"x" * (4 * 1024 * 1024 + 1))
    (project / "empty").mkdir()
    first = t.cli(t.a, "commit", project)["data"]
    cid = first["container"]["container_id"]
    stored = t.path(first["container"]) / "v000001/project.zip"
    with zipfile.ZipFile(stored) as archive:
        assert archive.namelist() == [".zxsignore", ".gitignore", "big.bin", "empty/", "source.txt"]
        assert all(info.date_time == (1980, 1, 1, 0, 0, 0) for info in archive.infolist())
        assert archive.testzip() is None
    same = t.cli(t.a, "commit", project)["data"]
    assert first["version"]["blake3"] == same["version"]["blake3"]
    t.cli(t.b, "checkout", cid, "--to", "project.zip")
    local = t.b / "project.zip"
    (local / "private.local").write_text("keep local")
    status = next(row for row in t.cli(t.b, "status")["entries"] if row["path"] == "project.zip")
    assert status["state"] == "clean" and status["bundle"] is True
    (project / "source.txt").unlink()
    (project / "next.txt").write_text("updated")
    with manifest.open("a", encoding="utf-8") as file:
        file.write("big.bin\n")
    t.cli(t.a, "commit", project)
    t.cli(t.b, "checkout", cid, "--to", "project.zip")
    assert not (local / "source.txt").exists()
    assert (local / "next.txt").read_text() == "updated"
    assert (local / "big.bin").stat().st_size == 4 * 1024 * 1024 + 1
    assert (local / "private.local").read_text() == "keep local"

    with zipfile.ZipFile(t.a / "ordinary.zip", "w") as archive:
        archive.writestr("first.txt", "plain archive")
        archive.writestr(".zxsignore", "")
    ordinary = t.cli(t.a, "commit", "ordinary.zip", "--folder", "/disk1")["data"]["container"]["container_id"]
    page = t.cli(t.b, "ls", "--q", "ordinary")
    assert [entry["container_id"] for entry in page["containers"]] == [ordinary]
    assert ordinary not in page["bundles"]
    t.cli(t.b, "checkout", ordinary)
    assert (t.b / "disk1/ordinary.zip").read_bytes() == (t.a / "ordinary.zip").read_bytes()

    (local / ".zxsignore").unlink()
    (local / "next.txt").write_text("uncommitted local edit")
    (local / "new-local.txt").write_text("untracked local file")
    t.cli(t.b, "checkout", cid, "--to", "project.zip", expected=3)
    checked = t.cli(t.b, "checkout", cid, "--to", "project.zip", "--backup")
    assert (Path(checked["backup"]) / "next.txt").read_text() == "uncommitted local edit"
    assert (local / "new-local.txt").read_text() == "untracked local file"
    assert (local / ".zxsignore").is_file()
    assert (local / "next.txt").read_text() == "updated"
    assert (local / "private.local").read_text() == "keep local"

    with zipfile.ZipFile(t.a / "unsafe.zip", "w") as archive:
        archive.writestr(".zxsignore", "")
        archive.writestr("../escaped", "unexpected")
    hostile = t.cli(t.a, "commit", "unsafe.zip", "--folder", "/disk1")["data"]
    hostile_id = hostile["container"]["container_id"]
    t.cli(t.b, "checkout", hostile_id, "--to", "unsafe", expected=1)
    assert not (t.b / "escaped").exists() and not (t.b / "unsafe").exists()
    recycled = t.cli(t.a, "recycle", "move", hostile_id)
    t.cli(t.a, "recycle", "purge", recycled["recycle"]["recycle_id"])
    t.cli(t.a, "untrack", "unsafe.zip")
    (t.a / "unsafe.zip").unlink()


def bundle_names(t):
    path = t.a / "disk1/Projection.zip"
    t.cli(t.a, "mkdir", "/disk1/Projection")
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr(".zxsignore", "")
        archive.writestr("member.txt", "bundle")
    bundled = path.read_bytes()
    cid = t.cli(t.a, "commit", path)["data"]["container"]["container_id"]
    t.cli(t.a, "mkdir", "/disk1/Projection.zip", expected=3)
    t.cli(t.c, "checkout", cid)
    assert (t.c / "disk1/Projection.zip/member.txt").read_text() == "bundle"
    t.cli(t.c, "sync", "--folder", "/disk1/Projection")
    assert (t.c / "disk1/Projection").is_dir()

    raw = t.a / "disk1/raw.bin"
    raw.write_bytes(bundled)
    alternate = t.cli(t.a, "commit", raw)["data"]["container"]["container_id"]
    t.cli(t.a, "mkdir", "/disk1/RawBundle")
    t.cli(t.a, "patch", alternate, "--name", "RawBundle.zip")
    assert t.cli(t.a, "detail", alternate)["container"]["name"] == "RawBundle.zip"

    archived = t.cli(t.a, "recycle", "move", cid)
    rid = archived["recycle"]["recycle_id"]
    assert t.cli(t.a, "restore", rid, "--folder", "/disk1")["name"] == "Projection.zip"
    assert t.cli(t.a, "ls", "/disk1/Projection")["folders"] == []


def requested_page_sizes_and_tags(t):
    folder = "/disk1/page-size"
    tags = [f"tag-{index:03}" for index in range(130)] + ["长" * 130, "", "line\nbreak"]
    for index in range(205):
        params = {"folder": folder, "name": f"{index:03}.txt", "tags": tags if index == 0 else []}
        container = t.http("POST", "/containers", 201, b"", upload_headers(params))[0]["container"]
        if index == 0:
            assert container["tags"] == sorted(set(tags))
            changed = t.http("PATCH", f"/containers/{container['container_id']}", 200, {"tags": tags})[0]
            assert changed["tags"] == sorted(set(tags))
    page = t.cli(t.a, "ls", folder, "--limit", 202)
    assert len(page["containers"]) == 202 and page["next_cursor"]
    tail = t.cli(t.a, "ls", folder, "--limit", 2**64 - 1, "--cursor", page["next_cursor"])
    assert len(tail["containers"]) == 3 and tail["next_cursor"] is None
    empty = t.cli(t.a, "ls", folder, "--limit", 0)
    assert empty["containers"] == [] and empty["next_cursor"] is None
    t.cli(t.a, "rmdir", folder, "--recursive")
    recycle = t.cli(t.a, "recycle", "ls", "--limit", 202)
    assert len(recycle["entries"]) == 202 and recycle["next_cursor"]


def recovery(t, cid):
    broken = t.partial({"folder": "/disk1", "name": "crash.bin"}, 100_000_000)
    broken.send(b"x" * 128_000)
    for i in range(100):
        if any((t.data / "disk1/.zxs/staging").glob("*/version/crash.bin")):
            break
        time.sleep(.025)
    else:
        raise AssertionError("upload did not enter staging")
    t.stop(crash=True)
    broken.close()
    (t.data / ".zxs/index.sqlite").unlink()
    for suffix in ("-wal", "-shm"):
        (t.data / f".zxs/index.sqlite{suffix}").unlink(missing_ok=True)
    t.start()
    t.http("GET", f"/containers/{cid}", 401)
    t.login_session()
    t.cli(t.c, "ls", "/")
    assert t.http("GET", f"/containers/{cid}", 200)[0]["container"]["latest_version"] == 4
    assert not list((t.data / "disk1/.zxs/staging").iterdir())
    t.stop()
    assert "Pruned 2 versions" in t.offline("prune", "--keep-last", 2, "--container", cid)
    assert "Verified" in t.offline("verify")
    assert "Index rebuilt" in t.offline("rebuild-index")
    t.start()
    t.login_session()
    detail, _ = t.http("GET", f"/containers/{cid}", 200)
    assert [v["version"] for v in detail["versions"]] == [3, 4]
    (t.c / "pull.txt").write_text("next after prune")
    committed = t.cli(t.c, "commit", "pull.txt", "--on-conflict", "append")["data"]
    assert committed["version"]["version"] == 5
    t.stop()
    change = os.urandom(16).hex()
    system = t.data / ".zxs"
    pending = {"tx_id": change, "change": {"action": "Publish", "commit": committed}}
    journal = system / "inflight" / (change + ".json")
    metadata = t.path(committed["container"]) / ".zxs"
    current = json.loads(metadata.read_bytes())
    assert current["first_retained_version"] == 3
    stage = t.data / committed["container"]["folder"].split("/")[1] / ".zxs/staging" / change
    stage.mkdir()
    (stage / "container.json").write_text(json.dumps(current))
    journal.write_text(json.dumps(pending))
    previous = current | {"latest_version": 4}
    metadata.write_text(json.dumps(previous))
    t.start()
    t.login_session()
    assert json.loads(metadata.read_bytes()) == current
    assert not journal.exists()
    assert t.http("GET", f"/containers/{cid}", 200)[0]["versions"][-1] == committed["version"]
    t.stop()
    historical = t.path(committed["container"]) / "v000003/demo.txt"
    assert historical.is_file()
    historical.chmod(0o644)
    for command in ("verify", "rebuild-index"):
        t.local(SERVER, ["--data-dir", t.data, command], expected=1)
    historical.chmod(0o444)
    metadata.write_text(json.dumps(previous))
    t.local(SERVER, ["--data-dir", t.data, "rebuild-index"], expected=1)
    metadata.write_text(json.dumps(current))
    assert "Verified" in t.offline("verify")
    t.start()
    t.login_session()


@contextmanager
def proxy_requests(port, forward, read_body=None):
    class Proxy(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *args):
            pass

        def do_request(self):
            length = int(self.headers.get("Content-Length", 0))
            body = (read_body(self, length) if read_body else self.rfile.read(length)) if length else None
            headers = {k: v for k, v in self.headers.items() if k.lower() not in ("expect", "host")}
            with context_connection(port) as remote:
                remote.request(self.command, self.path, body, headers)
                reply = remote.getresponse()
                content = reply.read()
                if not forward(self, reply, content):
                    self.close_connection = True
                    return
                self.send_response(reply.status)
                self.send_header("Content-Type", reply.getheader("Content-Type", "text/plain"))
                for header in ("ETag", "Content-Range"):
                    if (value := reply.getheader(header)) is not None:
                        self.send_header(header, value)
                self.send_header("Content-Length", str(len(content)))
                self.end_headers()
                self.wfile.write(content)

        do_POST = do_GET = do_DELETE = do_request

    server = ThreadingHTTPServer(("127.0.0.1", 0), Proxy)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}"
    finally:
        server.shutdown()
        server.server_close()


@contextmanager
def lose_reply(port, path="/containers"):
    lost = False

    def forward(request, response, _):
        nonlocal lost
        if request.path == path and request.command == "POST" and response.status == 201 and not lost:
            lost = True
            return False
        return True

    with proxy_requests(port, forward) as endpoint:
        yield endpoint
        assert lost


@contextmanager
def context_connection(port):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=8)
    try:
        yield connection
    finally:
        connection.close()


def mobile_call():
    alias = ctypes.CDLL(str(FFI))
    listener_type = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_uint8, *([ctypes.c_uint64] * 3))
    cancel_type = ctypes.CFUNCTYPE(ctypes.c_bool, ctypes.c_void_p)
    alias.zxs_call_with_transfer.argtypes = [ctypes.c_char_p, ctypes.c_size_t, listener_type,
                                            cancel_type, ctypes.c_void_p]
    alias.zxs_call_with_transfer.restype = ctypes.c_void_p
    alias.zxs_free.argtypes = [ctypes.c_void_p]
    alias.zxs_free.restype = None

    def call(action, *, status=None, progress=None, cancelled=None, **fields):
        request = json.dumps({"action": action, **fields}, ensure_ascii=False).encode()
        listener = listener_type() if progress is None else listener_type(lambda _, *sample: progress.append(sample))
        cancel = cancel_type() if cancelled is None else cancel_type(lambda _: cancelled())
        pointer = alias.zxs_call_with_transfer(request, len(request), listener, cancel, None)
        assert pointer
        try:
            reply = json.loads(ctypes.string_at(pointer))
            if status is not None:
                assert not reply["ok"] and reply["status"] == status, reply
                return reply
            assert reply["ok"], reply
            return reply["data"]
        finally:
            alias.zxs_free(pointer)

    return call


def mobile_photo(call, *, connection, file, name, folder, progress=None):
    target = {"kind": "photo", "folder": folder, "name": name}
    return call("commit_file", connection=connection, file=file, target=target, progress=progress)


def ffi(t):
    call = mobile_call()
    login = call("login", server=t.endpoint, password=PASSWORD)
    connection = {"server": login["server"], "password": PASSWORD}
    rejected = call("login", server=t.endpoint, password="wrong password", status=401)
    assert rejected["password_rejected"]
    invalid_address = call("login", server="http://127.0.0.1:0", password=PASSWORD, status=400)
    assert not invalid_address["password_rejected"]
    sample = t.root / "例.txt"
    sample.write_bytes(b"original")
    fields = {"connection": connection, "file": str(sample), "name": sample.name, "folder": "/disk1/手机"}
    progress = []
    entry = mobile_photo(call, progress=progress, **fields)["commit"]["container"]
    assert progress[0] == (0, 0, 8, 0) and progress[-1][:3] == (0, 8, 8) and progress[-1][3] > 0
    progress.clear()
    assert mobile_photo(call, progress=progress, **fields) == {"outcome": "already_present", "current": entry}
    assert not progress
    result = call("browse", connection=connection, folder="/disk1/手机")
    cid = entry["container_id"]
    assert result["containers"] == [entry]
    assert set(result) == {"containers", "folders", "next_cursor", "bundles"}
    args = {"connection": connection, "id": cid, "cache": str(t.root / "documents")}
    first = call("open", **args, progress=progress)
    assert progress[0] == (1, 0, 8, 0) and progress[-1][:3] == (1, 8, 8) and progress[-1][3] > 0
    cached = Path(first["path"])
    assert cached.read_bytes() == b"original"
    cached.write_bytes(b"damaged!")
    assert call("open", **args)["path"] == first["path"]
    assert cached.read_bytes() == b"original"
    sample.write_bytes(b"next mobile version")
    renamed = mobile_photo(call, **fields)["commit"]
    assert renamed["version"]["version"] == 1 and renamed["container"]["name"] == "例 (2).txt"
    assert mobile_photo(call, **fields)["current"] == renamed["container"]
    latest = call("open", **(args | {"id": renamed["container"]["container_id"]}))
    assert latest["path"] != first["path"]
    assert Path(latest["path"]).read_bytes() == b"next mobile version"
    clip = t.root / "clip.mp4"
    clip.write_bytes(b"media fixture")
    video = mobile_photo(call, **(fields | {"file": str(clip), "name": clip.name}))["commit"]["container"]
    stream = call("open", **(args | {"id": video["container_id"]}))
    assert stream["type"] == "video/mp4" and "?" not in stream["url"]
    path = stream["url"].removeprefix(t.endpoint)
    assert t.http("GET", path, 200, headers=stream["headers"], authenticated=False)[0] == b"media fixture"
    t.stop()
    t.start()
    t.http("GET", path, 401, headers=stream["headers"], authenticated=False)
    renewed = call("open", **(args | {"id": video["container_id"]}))
    assert renewed["url"] == stream["url"] and renewed["headers"] != stream["headers"]
    headers = renewed["headers"] | {"Range": "bytes=6-12"}
    assert t.http("GET", path, 206, headers=headers, authenticated=False)[0] == b"fixture"
    t.login_session()
    call("logout", connection=connection)
    mobile_session_errors(t, call)
    mobile_albums(t, call)
    mobile_file_upload(t, call)
    mobile_bundle(t, call)


def mobile_file_upload(t, call):
    folder = "/disk2/手机文件"
    t.http("POST", "/folders", 201, {"folder": folder})
    connection = {"server": t.endpoint, "password": PASSWORD}
    source = t.root / "所选原件"
    payload = bytes(range(256)) * 16385
    source.write_bytes(payload)
    target = {"kind": "new", "folder": folder, "name": "文件.DAT"}
    fields = {"connection": connection, "file": str(source), "target": target}
    requests = []

    def trace(request, _response, _body):
        requests.append((request.command, request.path, request.headers.get("Zxs-Blake3")))
        return True

    with proxy_requests(t.port, trace) as endpoint:
        call("login", server=endpoint, password=PASSWORD)
        requests.clear()
        created = call("commit_file", **(fields | {"connection": connection | {"server": endpoint}}))
        assert created["outcome"] == "committed"
        assert requests == [("POST", "/containers", None)]
        requests.clear()
        source.write_bytes(b"other source")
        conflict = call("commit_file", **(fields | {"connection": connection | {"server": endpoint}}))
        assert conflict == {"outcome": "conflict", "current": created["commit"]["container"]}
        assert len(requests) == 2 and requests[1][0] == "GET" and requests[1][1].startswith("/containers?")
        source.write_bytes(payload)
    cid = created["commit"]["container"]["container_id"]
    versions = f"/containers/{cid}/versions"
    assert t.http("GET", f"{versions}/1/payload", 200)[0] == payload
    same_name = fields | {"target": target | {"name": "文件.dat"}}
    assert call("commit_file", **same_name) == {"outcome": "already_present", "current": created["commit"]["container"]}

    source.write_bytes(b"selected mobile edits")
    assert call("commit_file", **same_name) == {"outcome": "conflict", "current": created["commit"]["container"]}
    update = {"kind": "version", "container_id": cid, "expected_version": 1}
    desktop = t.http("POST", versions, 201, b"other client edits", headers=upload_headers({"expected_version": 1}))[0]
    assert call("commit_file", **(fields | {"target": update})) == {
        "outcome": "conflict", "current": desktop["container"],
    }
    updated = fields | {"target": update | {"expected_version": 2}}
    with lose_reply(t.port, versions) as endpoint:
        via_proxy = updated | {"connection": connection | {"server": endpoint}}
        call("commit_file", **via_proxy, status=502)
        recovered = call("commit_file", **via_proxy)
        assert recovered["outcome"] == "already_present" and recovered["current"]["latest_version"] == 3
    assert t.http("GET", f"{versions}/3/payload", 200)[0] == b"selected mobile edits"
    assert t.http("GET", f"{versions}/1/payload", 200)[0] == payload

    t.http("POST", "/folders", 201, {"folder": folder + "/同名目录"})
    occupied = call("commit_file", **(fields | {"target": target | {"name": "同名目录"}}))
    assert occupied["outcome"] == "conflict" and "改名另存" in occupied["message"]
    assert "current" not in occupied
    call("commit_file", **(fields | {"target": target | {"folder": "/"}}), status=400)
    source.write_bytes(b"")
    empty = call("commit_file", **(fields | {"target": target | {"name": "空文件.txt"}}))["commit"]
    assert empty["version"]["size"] == 0 and empty["version"]["blake3"] == EMPTY_HASH
    with zipfile.ZipFile(source, "w") as archive:
        archive.writestr("原件.txt", "unchanged ZIP contents")
    zipped = call("commit_file", **(fields | {"target": target | {"name": "另存.zip"}}))["commit"]
    zipped_id = zipped["container"]["container_id"]
    assert t.http("GET", f"/containers/{zipped_id}/versions/1/payload", 200)[0] == source.read_bytes()
    call("logout", connection=connection)


def mobile_albums(t, call):
    folder = "/disk1/相册批量"
    t.http("POST", "/folders", 201, {"folder": folder})
    queries = []

    def trace(request, _response, _data):
        if request.command == "GET":
            queries.append(request.path)
        return True

    with proxy_requests(t.port, trace) as endpoint:
        connection = {"server": endpoint, "password": PASSWORD}
        fields = dict(connection=connection, folder=folder)
        paths, uploaded = [], []
        for index in range(8):
            path = t.root / f"photo-{index}.jpg"
            path.write_bytes(f"photo fixture {index}".encode())
            paths.append(path)
            uploaded.append(mobile_photo(call, **fields, file=str(path), name=path.name)["commit"]["container"])
        assert len(queries) == 2 * len(paths), queries
        assert all("&q=" in query for query in queries if query.startswith("/containers?")), queries

        queries.clear()
        repeated = [mobile_photo(call, **fields, file=str(path), name=path.name) for path in paths]
        assert repeated == [{"outcome": "already_present", "current": entry} for entry in uploaded]
        assert len(queries) == 2 * len(paths), queries
        assert all(query.startswith("/containers?") for query in queries), queries
        first = uploaded[0]
        cid = first["container_id"]
        t.http("POST", f"/containers/{cid}/versions", 201, b"remote edit",
               headers=upload_headers({"expected_version": 1}))
        path = paths[0]
        saved = mobile_photo(call, **fields, file=str(path), name=path.name)["commit"]
        assert saved["container"]["name"] == "photo-0 (2).jpg"
        assert mobile_photo(call, **fields, file=str(path), name=path.name)["current"] == saved["container"]

        for index in range(101):
            t.http("POST", "/containers", 201, b"",
                   headers=upload_headers(dict(folder=folder, name=f"old-{index:03}.jpg", tags=[])))
        queries.clear()
        for index in range(2):
            path = paths[index]
            result = mobile_photo(call, **fields, file=str(path), name=f"new-{index}.jpg")["commit"]
            assert result["version"]["version"] == 1
        assert [query.split("?")[0] for query in queries] == ["/containers", "/folders"] * 2, queries
        assert all("&q=" in query for query in queries if query.startswith("/containers?")), queries
        call("logout", connection=connection)


def mobile_session_errors(t, call):
    reject_request = True
    logins = 0

    def trace(request, response, data):
        nonlocal logins
        if request.command == "POST" and request.path == "/sessions":
            logins += 1
        if reject_request and request.command == "GET" and request.path.startswith("/folders?"):
            request.send_response(401)
            request.send_header("Content-Length", "0")
            request.send_header("Connection", "close")
            request.end_headers()
            return False
        return True

    with proxy_requests(t.port, trace) as endpoint:
        connection = {"server": endpoint, "password": PASSWORD}
        fields = {"connection": connection, "folder": "/"}
        response = call("browse", **fields, status=401)
        assert logins == 2 and not response["password_rejected"]
        reject_request = False
        assert call("browse", **fields)["folders"]
        assert logins == 2
        call("logout", connection=connection)


def mobile_bundle(t, call):
    archive = t.root / "移动工作区.zip"
    selected = "资料/笔记.txt"
    contents = "目录包中的单个文件".encode()
    clip_name = "其他/clip.mp4"
    clip = bytes(300 * 1024)
    with zipfile.ZipFile(archive, "w") as zip_file:
        zip_file.writestr(".zxsignore", "*.hidden\n")
        zip_file.writestr(selected, contents, compress_type=zipfile.ZIP_DEFLATED)
        zip_file.writestr("资料/空目录/", "")
        zip_file.writestr("资料/忽略.hidden", "ignored")
        zip_file.writestr("其他/笔记.txt", "second entry with the same name")
        zip_file.writestr(clip_name, clip)
        zip_file.writestr("unrelated.bin", bytes(3 * 1024 * 1024))
        for index in range(101):
            zip_file.writestr(f"分页/{index:03d}", "")

    params = {"name": archive.name, "folder": "/disk1/手机", "tags": []}
    upload, _ = t.http("POST", "/containers", 201, archive.read_bytes(), headers=upload_headers(params))
    cid = upload["container"]["container_id"]
    child = params["folder"] + "/资料"
    t.http("POST", "/folders", 201, {"folder": child})
    payload = f"/containers/{cid}/versions/1/payload"
    requests, transfers = [], []
    corrupt_next = True

    def trace(request, response, data):
        nonlocal corrupt_next
        requests.append((request.command, request.path, request.headers["Host"]))
        if request.command == "GET" and request.path.startswith(f"/containers/{cid}/versions/"):
            span = request.headers.get("Range")
            transfers.append((request.path, span, response.status, len(data)))
            if corrupt_next and request.path == payload and span is None:
                corrupt_next = False
                request.send_response(response.status)
                request.send_header("ETag", response.getheader("ETag"))
                request.send_header("Content-Length", str(len(data)))
                request.end_headers()
                request.wfile.write(bytes([data[0] ^ 1]) + data[1:])
                return False
        return True

    with proxy_requests(t.port, trace) as endpoint:
        endpoint = endpoint.replace("127.0.0.1", "localhost")
        connection = {"server": endpoint, "password": PASSWORD}
        args = {"connection": connection, "id": cid}
        call("login", **connection)
        requests.clear()
        root_folders = call("browse", connection=connection, folder="/")
        assert root_folders["folders"] == [{"folder": "/disk1"}, {"folder": "/disk2"}]
        assert requests == [("GET", "/folders?folder=%2F", endpoint.removeprefix("http://"))]
        requests.clear()
        folder = call("browse", connection=connection, folder=params["folder"])
        assert upload["container"] in folder["containers"] and not folder["bundles"]
        assert folder["folders"] == [{"folder": child}]
        assert [path.split("?")[0] for _, path, _ in requests] == ["/folders", "/containers"]
        assert not transfers

        cache = t.root / "mobile-zip-cache"
        args["cache"] = str(cache)
        call("open", **args, status=400)
        assert list(cache.iterdir()) == [cache / ".lock"]
        root = call("open", **args)
        assert root["bundle"] and root["version"] == 1
        assert call("open", **args) == root
        assert [entry["name"] for entry in root["entries"] if entry["kind"] == "directory"] == ["其他", "分页", "资料"]
        pinned = args | {"version": root["version"]}
        nested = call("browse_bundle", **pinned, directory="资料")
        assert nested["entries"] == [
            {"name": "空目录", "path": "资料/空目录", "kind": "directory"},
            {"name": "笔记.txt", "path": selected, "kind": "file"},
        ]
        assert call("browse_bundle", **pinned, directory="资料/空目录")["entries"] == []
        call("browse_bundle", **pinned, directory=selected, status=404)
        page = call("browse_bundle", **pinned, directory="分页")
        assert len(page["entries"]) == 100 and page["next_cursor"] is not None
        remaining = call("browse_bundle", **pinned, directory="分页", cursor=page["next_cursor"])
        assert [entry["name"] for entry in remaining["entries"]] == ["100"] and remaining["next_cursor"] is None
        file = call("open_bundle_member", **pinned, path=selected)
        assert Path(file["path"]).read_bytes() == contents and file["type"].startswith("text/plain")
        Path(file["path"]).write_bytes(b"x" * len(contents))
        assert call("open_bundle_member", **pinned, path=selected) == file
        assert Path(file["path"]).read_bytes() == contents
        different = call("open_bundle_member", **pinned, path="其他/笔记.txt")
        assert different["path"] != file["path"]
        assert Path(different["path"]).read_text() == "second entry with the same name"
        call("open_bundle_member", **pinned, path="资料/忽略.hidden", status=404)
        opened_clip = call("open_bundle_member", **pinned, path=clip_name)
        assert Path(opened_clip["path"]).read_bytes() == clip and opened_clip["type"] == "video/mp4"
        cached = cache / f"{cid}-1-{upload['version']['blake3']}" / archive.name
        assert cached.read_bytes() == archive.read_bytes()

        regular = t.root / "普通.zip"
        with zipfile.ZipFile(regular, "w") as zip_file:
            zip_file.writestr("example.txt", "regular zip")
        t.http("POST", f"/containers/{cid}/versions", 201, regular.read_bytes(),
               headers=upload_headers({"expected_version": 1}))
        current = call("open", **args)
        assert not current["bundle"] and current["type"] == "application/zip"
        assert Path(current["path"]).read_bytes() == regular.read_bytes()
        assert call("open", **pinned) == root
        before = len(requests)
        t.stop()
        try:
            assert call("browse_bundle", **pinned, directory="资料") == nested
            assert call("open_bundle_member", **pinned, path=selected) == file
            assert len(requests) == before
        finally:
            t.start()
            t.login_session()
        call("logout", connection=connection)

    assert transfers == [
        (payload, None, 200, archive.stat().st_size),
        (payload, None, 200, archive.stat().st_size),
        (f"/containers/{cid}/versions/2/payload", None, 200, regular.stat().st_size),
    ], transfers


def lost_replies(t):
    for name, edit in [("lost-reply.txt", None), ("lost-reply-edited.txt", "new edits after timeout")]:
        with lose_reply(t.port) as proxy:
            t.cli(t.c, "login", proxy)
            source = t.c / name
            source.write_text("first confirmed on server")
            t.cli(t.c, "commit", name, "--folder", "/disk1", expected=1)
            pending = next(entry for entry in t.cli(t.c, "status")["entries"] if entry["path"] == name)
            assert pending["state"] == "untracked"
            if edit is None:
                retry = t.cli(t.c, "commit", name, "--folder", "/disk1")["data"]
                assert retry["version"]["version"] == 1
            else:
                source.write_text(edit)
                t.cli(t.c, "commit", name, "--folder", "/disk1", expected=3)
                retry = t.cli(t.c, "commit", name, "--folder", "/disk1", "--on-conflict", "append")["data"]
                assert retry["version"]["version"] == 2
                cid = retry["container"]["container_id"]
                assert t.http("GET", f"/containers/{cid}/versions/2/payload", 200)[0] == edit.encode()
            refreshed = t.cli(t.c, "refresh")["entries"]
            assert next(item for item in refreshed if item["path"] == name)["state"] == "clean"
            t.cli(t.c, "logout")


def sessions(t):
    old = t.token
    t.cli(t.a, "login", t.endpoint)
    t.stop()
    t.start()
    t.http("GET", "/containers", 401)
    t.cli(t.a, "ls", "/")
    if os.name == "posix":
        assert stat.S_IMODE((t.config / "profile.json").stat().st_mode) == 0o600
    assert t.login_session() != old
    t.http("GET", "/containers", 200)
    t.http("DELETE", "/sessions/current", 204)
    t.http("DELETE", "/sessions/current", 401)
    t.http("GET", "/containers", 401)
    t.http("DELETE", "/sessions/current", 401,
           headers={"Authorization": "Bearer " + "0" * 64}, authenticated=False)
    fresh, _ = t.http("POST", "/sessions", 201, {"password": PASSWORD})
    assert fresh["token"] != t.token and set(fresh) == {"token"}
    t.token = fresh["token"]


def workspace_upload_hierarchy(t):
    t.cli(t.a, "login", t.endpoint)
    nested = t.a / "disk1/层级/二级"
    nested.mkdir(parents=True)
    source = nested / "notes.txt"
    source.write_text("first")
    first = t.cli(t.a, "commit", source)["data"]
    cid = first["container"]["container_id"]
    assert first["container"]["folder"] == "/disk1/层级/二级"
    root = t.cli(t.a, "ls", "/")
    assert all(item["container_id"] != cid for item in root["containers"])
    assert root["folders"] == [{"folder": "/disk1"}, {"folder": "/disk2"}]
    assert {"folder": "/disk1/层级"} in t.cli(t.a, "ls", "/disk1")["folders"]
    assert t.cli(t.a, "ls", "/disk1/层级")["folders"] == [{"folder": "/disk1/层级/二级"}]
    assert t.cli(t.a, "ls", "/disk1/层级/二级")["containers"][0]["container_id"] == cid
    original = t.data / "disk1/层级/二级/notes.txt"
    assert (original / "v000001/notes.txt").read_text() == "first"

    source.write_text("second version")
    second = t.cli(t.a, "commit", source)["data"]
    assert second["container"]["container_id"] == cid
    assert second["container"]["folder"] == "/disk1/层级/二级"
    assert second["version"]["version"] == 2

    override = t.a / "root-override.txt"
    override.write_text("explicit remote destination")
    t.cli(t.a, "commit", override, expected=1)
    assert t.cli(t.a, "commit", override, "--folder", "/disk2")["data"]["container"]["folder"] == "/disk2"
    t.cli(t.a, "init-bundle", t.a / "disk1", expected=1)
    (t.a / "disk1/.zxsignore").write_text("ignore-all\n")
    warning = t.cli(t.a, "status")
    assert "disk1" in warning["bundle_warnings"] and all(row["path"] != "disk1" for row in warning["entries"])
    (t.a / "disk1/.zxsignore").unlink()

    bundle = t.a / "disk1/层级/目录包"
    bundle.mkdir()
    (bundle / "member.txt").write_text("bundle member")
    bundle = t.a / t.cli(t.a, "init-bundle", bundle)
    packaged = t.cli(t.a, "commit", bundle)["data"]
    assert packaged["container"]["folder"] == "/disk1/层级"
    assert packaged["container"]["name"] == "目录包.zip"

    t.cli(t.a, "init-bundle", nested, expected=3)
    (nested / ".zxsignore").write_text("notes.txt\n")
    t.cli(t.a, "commit", nested, expected=3)
    assert (nested / ".zxsignore").read_text() == "notes.txt\n"

    mirror = t.root / "mirror"
    mirror_nested = mirror / "disk1/层级/二级"
    mirror_nested.mkdir(parents=True)
    marker = mirror_nested / ".zxsignore"
    marker.write_text("notes.txt\n")
    synchronized = t.cli(mirror, "sync", "--folder", "/")
    assert "disk1/层级/二级" in synchronized["bundle_warnings"]
    assert marker.read_text() == "notes.txt\n"
    assert (mirror_nested / "notes.txt").read_text() == "second version"
    assert (mirror / "disk2/root-override.txt").read_text() == "explicit remote destination"
    assert (mirror / "disk1/层级/目录包.zip/member.txt").read_text() == "bundle member"

    (mirror_nested / "notes.txt").write_text("local edit")
    source.write_text("third version on the server")
    t.cli(t.a, "commit", source)
    override.write_text("new remote version")
    t.cli(t.a, "commit", override)
    synchronized = t.cli(mirror, "sync")
    assert synchronized["local_changes"] >= 1
    assert (mirror_nested / "notes.txt").read_text() == "local edit"
    assert (mirror / "disk2/root-override.txt").read_text() == "new remote version"


def folder_lifecycle(t):
    tree = t.data / "disk1/Tree"
    t.cli(t.a, "mkdir", "/disk1/Tree/Empty/Deeper")
    assert (tree / "Empty/Deeper").is_dir()
    assert t.cli(t.a, "ls", "/disk1/Tree/Empty")["folders"] == [{"folder": "/disk1/Tree/Empty/Deeper"}]
    t.cli(t.a, "rmdir", "/disk1/Tree/Empty", expected=3)
    t.cli(t.a, "rmdir", "/disk1/Tree/Empty/Deeper")
    assert (tree / "Empty").is_dir()
    t.cli(t.a, "mkdir", "/disk1/Tree/Nested")
    t.cli(t.a, "mkdir", "/disk1/tree/conflict", expected=3)
    for name in ("beta.txt", "Alpha.txt", "v000001", "Zulu.txt"):
        source = t.a / name
        source.write_text(name)
        t.cli(t.a, "commit", source, "--folder", "/disk1/Tree/Nested")
    meta = tree / "Nested/Alpha.txt/.zxs"
    saved, stamp = meta.read_bytes(), meta.stat().st_mtime_ns
    assert set(json.loads(saved)) == {"container_id", "tags", "first_retained_version", "latest_version", "created_at", "updated_at"}
    mirror = t.root / "tree-mirror"
    mirror.mkdir()
    t.cli(mirror, "sync", "--folder", "/disk1/Tree")
    assert (mirror / "disk1/Tree/Empty").is_dir()
    dirty = mirror / "disk1/Tree/Nested/beta.txt"
    dirty.write_text("unsubmitted edit")

    t.cli(t.a, "mv-folder", "/disk1/Tree", "/disk1/tree")
    assert not tree.exists()
    renamed = t.data / "disk1/tree/Nested/Alpha.txt/.zxs"
    assert renamed.read_bytes() == saved and renamed.stat().st_mtime_ns == stamp
    t.cli(t.a, "mv-folder", "/disk1/tree", "/disk1/tree/Nested/Inside", expected=3)
    too_long = "/disk1/" + "/".join(["x" * 240] * 4 + ["t" * 50])
    t.cli(t.a, "mv-folder", "/disk1/tree", too_long, expected=3)
    t.cli(t.a, "mv-folder", "/disk1/tree", "/disk2/Moved/Library")
    meta = t.data / "disk2/Moved/Library/Nested/Alpha.txt/.zxs"
    assert meta.read_bytes() == saved
    t.cli(mirror, "refresh", expected=1)
    assert (mirror / "disk2/Moved/Library/Nested/Alpha.txt").read_text() == "Alpha.txt"
    assert dirty.read_text() == "unsubmitted edit"
    assert not (mirror / "disk2/Moved/Library/Nested/beta.txt").exists()

    folder = "/disk2/Moved/Library/Nested"
    cursor = None
    names = []
    while True:
        query = {"folder": folder, "limit": 1}
        if cursor:
            query["cursor"] = cursor
        page = t.http("GET", "/containers?" + urlencode(query), 200)[0]
        names.extend(entry["name"] for entry in page["containers"])
        cursor = page["next_cursor"]
        if cursor is None:
            break
    assert names == ["Alpha.txt", "beta.txt", "v000001", "Zulu.txt"]
    t.cli(t.a, "rmdir", "/disk2/Moved/Library", expected=3)
    t.cli(t.a, "rmdir", "/disk2/Moved/Library", "--recursive")
    assert not (t.data / "disk2/Moved/Library").exists()
    rows = t.cli(t.a, "recycle", "ls")["entries"]
    assert {entry["container"]["name"] for entry in rows} == set(names)
    for entry in rows:
        assert t.archived(entry).is_dir()
        assert "folder" not in entry["container"]
    one = t.cli(t.a, "recycle", "ls", "--limit", 1)
    later = t.cli(t.a, "recycle", "ls", "--cursor", one["next_cursor"])
    assert one["entries"] + later["entries"] == rows
    restore, discard, *rest = rows
    restored = t.cli(t.a, "restore", restore["recycle"]["recycle_id"], "--folder", folder)
    assert t.path(restored).is_dir() and not t.archived(restore).exists()
    assert t.cli(t.a, "recycle", "purge", discard["recycle"]["recycle_id"]) == {"purged": 1}
    assert not t.archived(discard).exists()
    assert t.cli(t.a, "recycle", "purge", "--all") == {"purged": len(rest)}
    assert t.cli(t.a, "recycle")["entries"] == []


def workspace_lifecycle(t):
    workspace = t.root / "changes"
    workspace.mkdir()
    (workspace / "disk1").mkdir()
    independent = workspace / "disk1/piece.txt"
    independent.write_text("keep my content")
    created = t.cli(workspace, "commit", independent)["data"]["container"]
    bundle = workspace / "disk1/package"
    bundle.mkdir()
    bundle = workspace / t.cli(workspace, "init-bundle", bundle)
    t.cli(workspace, "commit", bundle)
    t.cli(workspace, "mkdir", "/disk1/package")
    independent.rename(bundle / "piece.txt")
    scan = t.cli(workspace, "status")
    by_path = {entry["path"]: entry for entry in scan["entries"]}
    assert by_path["disk1/piece.txt"]["state"] == "missing"
    assert by_path["disk1/package.zip"]["state"] == "modified"
    t.cli(workspace, "commit", bundle)
    t.cli(workspace, "sync-delete", independent)
    t.http("GET", f"/containers/{created['container_id']}", 404)
    (bundle / "piece.txt").rename(workspace / "disk1/moved-out.txt")
    scan = t.cli(workspace, "status")
    by_path = {entry["path"]: entry["state"] for entry in scan["entries"]}
    assert by_path == {"disk1/package.zip": "modified", "disk1/moved-out.txt": "untracked"}
    other = t.cli(workspace, "commit", "disk1/moved-out.txt")["data"]["container"]
    assert other["container_id"] != created["container_id"]
    t.cli(t.a, "recycle", "move", other["container_id"])
    refreshed = t.cli(workspace, "refresh")
    assert any(row["path"] == "disk1/moved-out.txt" and row["remote_missing"] for row in refreshed["entries"])
    assert (workspace / "disk1/moved-out.txt").read_text() == "keep my content"
    t.cli(workspace, "untrack", "disk1/moved-out.txt")
    assert next(row for row in t.cli(workspace, "status")["entries"]
                if row["path"] == "disk1/moved-out.txt")["state"] == "untracked"

    crowded = t.root / "crowded"
    crowded.mkdir()
    (crowded / "000-link").symlink_to(t.root)
    assert any(path.endswith("000-link") for path in t.cli(crowded, "status")["symlink_warnings"])
    for index in range(1001):
        (crowded / f"file-{index:04}").touch()
    scan = t.cli(crowded, "status")
    assert scan["truncated"] and len(scan["entries"]) == 1000


def concurrent_uploads(t):
    source = t.a / "in-progress.txt"
    source.write_text("first")
    first = t.cli(t.a, "commit", source, "--folder", "/disk1/Concurrent")["data"]["container"]
    cid = first["container_id"]

    def append_during(expected, change, status):
        headers = upload_headers({"expected_version": expected})
        with context_connection(t.port) as connection:
            connection.putrequest("POST", f"/containers/{cid}/versions")
            for name, value in headers.items():
                connection.putheader(name, value)
            connection.putheader("Authorization", f"Bearer {t.token}")
            connection.putheader("Expect", "100-continue")
            connection.putheader("Content-Length", "4")
            connection.endheaders(b"ne")
            stage = t.data / "disk1/.zxs/staging"
            for _ in range(100):
                if any(stage.iterdir()):
                    break
                time.sleep(.025)
            else:
                raise AssertionError("partial append did not reach staging")
            change()
            connection.send(b"xt")
            response = connection.getresponse()
            assert response.status == status, response.read()
            return json.loads(response.read())

    result = append_during(1, lambda: t.cli(t.a, "patch", cid, "--tags", "kept"), 201)
    assert result["container"]["tags"] == ["kept"]
    append_during(2, lambda: t.cli(t.a, "mv-folder", "/disk1/Concurrent", "/disk1/Concurrent-Moved"), 409)
    detail = t.http("GET", f"/containers/{cid}", 200)[0]
    assert detail["container"]["folder"] == "/disk1/Concurrent-Moved"
    assert [version["version"] for version in detail["versions"]] == [1, 2]


def disk_registration_and_version_history(t):
    source = t.a / "allocation.txt"
    source.write_text("published once")
    result = t.cli(t.a, "commit", source, "--folder", "/disk1")["data"]
    cid = result["container"]["container_id"]
    disk = t.path(result["container"]) / ".zxs"
    saved = json.loads(disk.read_bytes())
    config_path = t.data / ".zxs/config.toml"
    serve = ["serve", "--data-dir", t.data, "--listen", f"127.0.0.1:{t.port}"]
    t.stop()
    marker = t.recycle / "rec1/.zxs/disk.toml"
    hidden = t.recycle / "rec1/.zxs/.not-mounted"
    marker.rename(hidden)
    t.start()
    t.login_session()
    assert t.http("GET", f"/containers/{cid}", 200)[0]["container"]["latest_version"] == 1
    t.stop()
    assert "Verified" in t.offline("verify")
    hidden.rename(marker)

    second_marker = t.data / "disk2/.zxs/disk.toml"
    marker_text = second_marker.read_text()
    second_marker.write_text((t.data / "disk1/.zxs/disk.toml").read_text())
    t.local(SERVER, serve, expected=1)
    second_marker.write_text(marker_text)
    (t.data / "disk3/空目录").mkdir(parents=True)
    (t.recycle / "rec3").mkdir()
    t.local(PREPARE, ["prepare", t.data / "disk3"])
    t.prepare_disk(t.recycle / "rec3", "recycle")
    assert "Disk registrations updated" in t.offline("sync-disks")
    assert "layout =" not in config_path.read_text()
    assert 'kind = "data"' in (t.data / "disk3/.zxs/disk.toml").read_text()
    assert 'kind = "recycle"' in (t.recycle / "rec3/.zxs/disk.toml").read_text()

    disk.unlink()
    assert "Verified" not in t.local(SERVER, ["verify", "--data-dir", t.data], expected=1)
    t.local(SERVER, ["rebuild-index", "--data-dir", t.data], expected=1)
    disk.write_text(json.dumps(saved | {"latest_version": 2}))
    t.local(SERVER, ["rebuild-index", "--data-dir", t.data], expected=1)
    incomplete = saved.copy()
    del incomplete["first_retained_version"]
    disk.write_text(json.dumps(incomplete))
    for command in ("verify", "rebuild-index"):
        t.local(SERVER, [command, "--data-dir", t.data], expected=1)

    disk.write_text(json.dumps(saved))
    assert "Index rebuilt" in t.offline("rebuild-index")
    t.start()
    t.login_session()
    detail = t.http("GET", f"/containers/{cid}", 200)[0]
    assert detail["container"]["latest_version"] == 1
    assert t.cli(t.a, "ls", "/disk3")["folders"] == [{"folder": "/disk3/空目录"}]
    source.write_text("second available version")
    result = t.cli(t.a, "commit", source, "--folder", "/disk1")["data"]
    assert result["version"]["version"] == 2 and result["container"]["latest_version"] == 2


def guarded_local_changes(t):
    source = t.a / "disk1/guarded.txt"
    source.write_text("first")
    first = t.cli(t.a, "commit", source)["data"]["container"]
    cid = first["container_id"]
    local = t.root / "guarded-mirror"
    local.mkdir()
    t.cli(local, "checkout", cid)
    draft = local / "disk1" / source.name
    before = draft.stat()
    draft.write_text("other")
    os.utime(draft, ns=(before.st_atime_ns, before.st_mtime_ns))
    source.write_text("new remote")
    t.cli(t.a, "commit", source)
    t.cli(local, "checkout", cid, expected=3)
    assert draft.read_text() == "other"
    t.cli(t.a, "patch", cid, "--name", "guarded-new.txt")
    t.cli(local, "refresh", expected=1)
    assert draft.read_text() == "other" and not (local / "disk1/guarded-new.txt").exists()
    draft.unlink()
    t.cli(local, "sync-delete", draft, expected=3)
    t.http("GET", f"/containers/{cid}", 200)
    t.cli(local, "sync-delete", draft, "--expected-version", 2)
    t.http("GET", f"/containers/{cid}", 404)


def workspace_server_address_change(root):
    original = Test(root / "address-change", root / "address-change/recycle")
    unrelated = Test(root / "unrelated-server", root / "unrelated-server/recycle")

    def read_tracks(workspace):
        with sqlite3.connect(workspace / ".zxs/workspace.sqlite") as db:
            items = (json.loads(data) for data, in db.execute("SELECT data FROM tracks"))
            return {item["path"]: item for item in items}

    try:
        original.offline("init")
        original.start()
        original.cli(original.a, "login", original.endpoint)
        regular = "disk1/report.txt"
        directory = "disk1/project"
        (original.a / regular).write_text("published report")
        (original.a / directory).mkdir()
        directory = original.cli(original.a, "init-bundle", directory)
        (original.a / directory / "member.txt").write_text("published bundle member")
        file_id = original.cli(original.a, "commit", regular)["data"]["container"]["container_id"]
        bundle_commit = original.cli(original.a, "commit", directory)["data"]
        bundle_id = bundle_commit["container"]["container_id"]
        original.cli(original.b, "checkout", file_id)
        original.cli(original.b, "checkout", bundle_id, "--to", directory)
        original.cli(original.c, "checkout", bundle_id, "--to", directory)
        (original.b / regular).write_text("draft kept across address changes")
        before = read_tracks(original.b)
        assert before[directory]["mirrored"]

        with sqlite3.connect(original.c / ".zxs/workspace.sqlite") as db:
            pending = dict(path=directory, container_id=bundle_id, version=1,
                           endpoint=original.endpoint, kind="directory",
                           payload_hash=bundle_commit["version"]["blake3"])
            db.execute("INSERT INTO interrupted_checkout VALUES(?, ?, ?)",
                       (bundle_id, directory, json.dumps(pending)))
        (original.c / directory / "member.txt").write_text("interrupted checkout content")

        unrelated.offline("init")
        unrelated.start()
        unrelated.cli(unrelated.a, "login", unrelated.endpoint)
        (unrelated.a / regular).write_text("a different server at the same logical path")
        other_id = unrelated.cli(unrelated.a, "commit", regular)["data"]["container"]["container_id"]
        assert other_id != file_id
        original.cli(original.b, "login", unrelated.endpoint)
        assert original.cli(original.b, "sync", expected=1)["issues"]
        original.cli(original.b, "commit", regular, expected=3)
        assert read_tracks(original.b) == before
        assert (original.b / regular).read_text() == "draft kept across address changes"

        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            new_port = listener.getsockname()[1]
        original.stop()
        original.port = new_port
        original.endpoint = f"http://127.0.0.1:{new_port}"
        original.start()
        original.cli(original.b, "login", original.endpoint)
        sync = original.cli(original.b, "sync")
        assert sync["issues"] == [] and sync["current"] == 1 and sync["local_changes"] == 1
        assert read_tracks(original.b) == {
            path: record | {"endpoint": original.endpoint} for path, record in before.items()
        }
        assert (original.b / regular).read_text() == "draft kept across address changes"
        assert (original.b / directory / "member.txt").read_text() == "published bundle member"

        original.cli(original.c, "checkout", bundle_id, expected=3)
        with sqlite3.connect(original.c / ".zxs/workspace.sqlite") as db:
            saved = json.loads(db.execute("SELECT data FROM interrupted_checkout").fetchone()[0])
        assert saved == pending | {"endpoint": original.endpoint}
        recovered = original.cli(original.c, "checkout", bundle_id, "--backup")
        assert (Path(recovered["backup"]) / "member.txt").read_text() == "interrupted checkout content"
        assert (original.c / directory / "member.txt").read_text() == "published bundle member"
        (original.a / regular).write_text("submitted using the new address")
        reads = []

        def trace(request, _response, _data):
            if request.command == "GET":
                reads.append(request.path)
            return True

        with proxy_requests(original.port, trace) as endpoint:
            original.cli(original.a, "login", endpoint)
            result = original.cli(original.a, "commit", regular)["data"]
        assert reads == [f"/containers?ids={file_id}"]
        assert result["container"]["container_id"] == file_id and result["version"]["version"] == 2
    finally:
        unrelated.stop()
        original.stop()


def passwords_follow_the_metadata_contract(t):
    env = t.env.copy()
    env.pop("ZXS_PASSWORD", None)
    for password in ("新" * 440, ""):
        t.stop()
        reset = subprocess.run(
            [SERVER, "--data-dir", t.data, "reset-password"],
            env=env, input=password + "\r\n", encoding="utf-8", capture_output=True, timeout=60,
        )
        assert reset.returncode == 0, reset.stderr
        t.start()
        t.http("POST", "/sessions", 201, {"password": password}, authenticated=False)
        login = subprocess.run(
            [CLIENT, "login", t.endpoint],
            env=env, input=password + "\r\n", encoding="utf-8", capture_output=True, timeout=60,
        )
        assert login.returncode == 0, login.stderr


def main():
    with tempfile.TemporaryDirectory(prefix="zxs-smoke-") as root, tempfile.TemporaryDirectory(
        prefix="zxs-recycle-", dir=os.environ.get("ZXS_SMOKE_RECYCLE_PARENT")
    ) as recycle:
        t = Test(Path(root), Path(recycle))
        t.offline("init")
        with sqlite3.connect(t.data / ".zxs/index.sqlite") as db:
            assert {name for name, in db.execute("SELECT name FROM sqlite_schema WHERE type='table'")} == {
                "folders", "containers", "versions", "tags", "recycle_bin",
            }
        imported = t.root / "import"
        imported.mkdir()
        (imported / "Enclosed/Empty").mkdir(parents=True)
        (imported / "empty.txt").touch()
        (imported / "PHOTO.JPG").write_bytes(b"image")
        assert "Imported 2 containers" in t.offline("import", imported, "--folder", "/disk1/导入")
        try:
            t.start()
            cid = session_and_versions(t)
            assert (t.data / "disk1/导入/Enclosed/Empty").is_dir()
            photo = t.cli(t.a, "ls", "/disk1/导入", "--q", "photo")["containers"][0]
            photo_path = t.path(photo) / "v000001/PHOTO.JPG"
            assert photo_path.read_bytes() == b"image"
            payload_and_recycle(t, cid)
            physical_namespace(t)
            multiblock_payload(t)
            upload_integrity_and_retry(t)
            bundles(t)
            bundle_names(t)
            recovery(t, cid)
            ffi(t)
            lost_replies(t)
            sessions(t)
            workspace_upload_hierarchy(t)
            folder_lifecycle(t)
            workspace_lifecycle(t)
            concurrent_uploads(t)
            disk_registration_and_version_history(t)
            guarded_local_changes(t)
            requested_page_sizes_and_tags(t)
            passwords_follow_the_metadata_contract(t)
        except Exception:
            print((t.root / "server.log").read_text())
            raise
        finally:
            t.stop()
        workspace_server_address_change(t.root)
        print("zxs-s server, CLI and C ABI smoke checks passed")


if __name__ == "__main__":
    main()
