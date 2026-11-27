"""Regressions for workspace data integrity, disk registration and HTTP origins."""

from concurrent.futures import ThreadPoolExecutor
import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
from tempfile import TemporaryDirectory
from threading import Thread
import time
import zipfile

from smoke import CLIENT, SERVER, Test, context_connection, proxy_requests, upload_headers
from workspace_selection import ui


def registration(t):
    arguments = ["--data-dir", t.data, "--recycle-dir", t.recycle]
    config = t.data / ".zxs/config.toml"
    unexpected = t.data / "disk1/existing.txt"
    unexpected.touch()
    t.local(SERVER, [*arguments, "init"], expected=1)
    assert not config.exists()
    unexpected.unlink()
    t.offline("init")
    index = t.data / ".zxs/index.sqlite"
    previous = config.read_bytes(), index.read_bytes()
    bad = t.data / "disk3"
    bad.mkdir()
    (bad / unexpected.name).touch()
    assert "No new disks" in t.offline("sync-disks")
    assert not (bad / ".zxs").exists()
    assert (config.read_bytes(), index.read_bytes()) == previous
    bad.rename(t.root / bad.name)
    t.start()
    try:
        t.login_session()
        assert t.http("GET", "/folders?folder=/", 200)[0]["folders"] == [
            {"folder": "/disk1"}, {"folder": "/disk2"},
        ]
        live, expired = (
            t.http("POST", "/containers", 201, name.encode(),
                   headers=upload_headers(dict(folder="/disk1", name=name, tags=[])))[0]["container"]
            for name in ("live.txt", "archived.txt")
        )
        archived = t.http("DELETE", f"/containers/{expired['container_id']}", 200)[0]
    finally:
        t.stop()
    previous = config.read_bytes(), index.read_bytes()
    index_inode = index.stat().st_ino
    assert "No new disks" in t.offline("sync-disks")
    assert (config.read_bytes(), index.read_bytes()) == previous
    assert index.stat().st_ino == index_inode

    (t.data / "disk3").mkdir()
    t.prepare_disk(t.data / "disk3", "data")
    hidden = t.root / "hidden"
    for path in (t.path(live), t.archived(archived)):
        path.rename(hidden)
        t.local(SERVER, [*arguments, "sync-disks"], expected=1)
        assert (config.read_bytes(), index.read_bytes()) == previous
        hidden.rename(path)
    assert "Disk registrations updated" in t.offline("sync-disks")
    assert "Verified 2 versions" in t.offline("verify")


def ignored_and_recycled(t):
    project = t.a / "disk1/project"
    (project / "cache").mkdir(parents=True)
    t.cli(t.a, "init-bundle", project)
    (project / ".zxsignore").write_text("cache\n!cache/\n")
    container = t.cli(t.a, "commit", project)["data"]["container"]
    cid = container["container_id"]
    t.cli(t.b, "checkout", cid)
    local = t.b / "disk1/project.zip/cache"
    local.rmdir()
    local.write_text("ignored local data")
    (project / "cache").rmdir()
    (project / "member.txt").write_text("version two")
    t.cli(t.a, "commit", project)
    result = t.cli(t.b, "checkout", cid, "--backup")
    assert result["version"] == 2 and local.read_text() == "ignored local data"

    t.cli(t.a, "untrack", project)
    named = t.cli(t.a, "commit", project, "--name", "backup")["data"]["container"]
    assert named["name"] == "backup.zip"
    renamed = t.cli(t.c, "patch", named["container_id"], "--name", "renamed.zip")
    assert renamed["name"] == "renamed.zip"
    rid = t.cli(t.c, "recycle", "move", named["container_id"])["recycle"]["recycle_id"]
    t.cli(t.c, "mkdir", "/disk1/renamed")
    assert t.cli(t.c, "restore", rid, "--folder", "/disk1/renamed", "--name", "restored.zip")["name"] == "restored.zip"


def checkout_types(t):
    source = t.a / "disk1/converted.zip"
    with zipfile.ZipFile(source, "w") as archive:
        archive.writestr("plain.txt", "version one")
    ordinary = source.read_bytes()
    cid = t.cli(t.a, "commit", source)["data"]["container"]["container_id"]
    with zipfile.ZipFile(source, "w") as archive:
        archive.writestr(".zxsignore", "local.txt\n")
        archive.writestr("member.txt", "version two")
    t.cli(t.a, "commit", source)
    t.cli(t.b, "checkout", cid)
    target = t.b / "disk1/converted.zip"
    (target / "local.txt").write_text("ignored")
    t.cli(t.b, "checkout", cid, "--version", 1, "--backup", expected=3)
    assert (target / "member.txt").read_text() == "version two"
    assert (target / "local.txt").read_text() == "ignored"
    (target / "local.txt").unlink()
    previous = t.cli(t.b, "checkout", cid, "--version", 1)
    assert target.read_bytes() == ordinary
    assert (Path(previous["backup"]) / "member.txt").read_text() == "version two"
    latest = t.cli(t.b, "checkout", cid, "--backup")
    assert (target / "member.txt").read_text() == "version two"
    assert Path(latest["backup"]).read_bytes() == ordinary

    t.cli(t.c, "checkout", cid, "--version", 1, "--to", "custom")
    t.cli(t.c, "checkout", cid, "--to", "custom", "--backup")
    assert (t.c / "custom/member.txt").read_text() == "version two"
    (t.c / "custom/local.txt").write_text("keep this ignored file")
    t.cli(t.c, "checkout", cid, "--version", 1, "--to", "custom", "--backup", expected=3)
    assert (t.c / "custom/local.txt").read_text() == "keep this ignored file"
    assert (t.c / "custom/member.txt").read_text() == "version two"


def unicode_names(t):
    composed = t.a / "disk1/café.txt"
    decomposed = composed.with_name("cafe\u0301.txt")
    composed.write_bytes(b"one file")
    decomposed.write_bytes(b"another file")
    assert not composed.samefile(decomposed)
    t.cli(t.a, "status", expected=1)
    t.cli(t.a, "commit", decomposed, expected=1)
    assert t.cli(t.a, "ls", "/disk1", "--q", "café")["containers"] == []
    composed.unlink()
    result = t.cli(t.a, "commit", decomposed)["data"]
    assert (t.path(result["container"]) / "v000001/café.txt").read_bytes() == b"another file"
    assert t.cli(t.a, "ls", "/disk1", "--q", "CAFE\u0301")["containers"] == [result["container"]]
    composed.write_bytes(b"distinct file using the other spelling")
    t.cli(t.a, "commit", composed, expected=1)
    assert t.cli(t.a, "detail", result["container"]["container_id"])["container"]["latest_version"] == 1


def historical_restore(t):
    relative = "disk1/restore.txt"
    source, target = t.a / relative, t.b / relative
    source.write_text("original")
    first = t.cli(t.a, "commit", relative)["data"]
    cid = first["container"]["container_id"]
    source.write_text("latest")
    second = t.cli(t.a, "commit", relative)["data"]
    t.cli(t.b, "checkout", cid)
    t.cli(t.c, "checkout", cid)
    with target.open("r+b") as editor:
        result = t.cli(t.b, "checkout", cid, "--version", 1)
        editor.write(b"editor saved after restore")
        editor.truncate()
    assert Path(result["backup"]).read_text() == "editor saved after restore"
    assert target.read_text() == "original"
    t.cli(t.b, "sync")
    row = next(row for row in t.cli(t.b, "status")["entries"] if row["path"] == relative)
    assert (row["state"], row["version"], row["restored_from"]) == ("modified", 2, 1)
    assert t.cli(t.b, "detail", cid)["container"]["latest_version"] == 2
    assert target.read_text() == "original"
    assert t.cli(t.b, "commit", relative)["data"]["version"]["version"] == 3

    retained = t.c / ".zxs/backups/interrupted-original"
    retained.parent.mkdir(exist_ok=True)
    (t.c / relative).rename(retained)
    pending = {
        "path": "disk1/interrupted.txt", "container_id": cid, "endpoint": t.endpoint,
        "kind": "file", "version": 1, "payload_hash": first["version"]["blake3"],
        "restored_against": {"version": 2, "blake3": second["version"]["blake3"]},
        "backup": str(retained.relative_to(t.c)),
    }
    with sqlite3.connect(t.c / ".zxs/workspace.sqlite") as db:
        db.execute("INSERT INTO interrupted_checkout VALUES (?, ?, ?)",
                   (cid, pending["path"], json.dumps({k: v for k, v in pending.items() if k != "payload_hash"})))
    states = [row["state"] for row in t.cli(t.c, "status")["entries"]
              if row["container_id"] == cid]
    assert states == ["interrupted"]
    t.cli(t.c, "sync-delete", relative, "--expected-version", 3, expected=3)
    t.cli(t.c, "checkout", cid, "--backup", expected=3)
    assert retained.read_text() == "latest" and not (t.c / pending["path"]).exists()
    with sqlite3.connect(t.c / ".zxs/workspace.sqlite") as db:
        db.execute("UPDATE interrupted_checkout SET data=? WHERE id=?", (json.dumps(pending), cid))
    recovered = t.cli(t.c, "checkout", cid, "--backup")
    assert (recovered["version"], Path(recovered["path"]).read_text()) == (1, "original")
    assert Path(recovered["backup"]).read_text() == "latest"
    Path(recovered["path"]).write_text("local edits after recovery")
    t.cli(t.c, "commit", recovered["local"], expected=3)

    target.write_text("unsubmitted edit")
    payloads = []

    def trace(request, _response, _data):
        if request.path.endswith("/payload"):
            payloads.append(request.path)
        return True

    with proxy_requests(t.port, trace) as endpoint:
        t.cli(t.b, "login", endpoint)
        t.cli(t.b, "checkout", cid, "--version", 1, expected=3)
    assert payloads == []
    assert target.read_text() == "unsubmitted edit"
    t.cli(t.b, "login", t.endpoint)

    fresh = t.root / "fresh"
    fresh.mkdir()
    restored = t.cli(fresh, "checkout", cid, "--version", 2)
    assert restored["restored_from"] == 2
    assert Path(restored["path"]).read_text() == "latest"
    assert t.cli(fresh, "commit", relative)["data"]["version"]["version"] == 4


def transport(t):
    class Authority(BaseHTTPRequestHandler):
        def do_POST(self):
            self.rfile.read(int(self.headers["Content-Length"]))
            if self.headers["Host"] != f"localhost:{self.server.server_port}":
                self.send_error(401)
                return
            payload = json.dumps({"token": "a" * 64}).encode()
            self.send_response(201)
            self.end_headers()
            self.wfile.write(payload)

        def do_DELETE(self):
            self.send_response(204)
            self.end_headers()

        def log_message(self, *_):
            pass

    with ThreadingHTTPServer(("127.0.0.1", 0), Authority) as listener:
        worker = Thread(target=listener.serve_forever, daemon=True)
        worker.start()
        try:
            server = f"http://localhost:{listener.server_port}"
            assert t.cli(t.a, "login", server)["server"] == server
        finally:
            listener.shutdown()
            worker.join()

    command = [CLIENT, "--workspace", t.a, "login"]
    rejected = subprocess.run([*command, "http://127.0.0.1:65536"], env=t.env,
                              capture_output=True, text=True, timeout=5, check=False)
    assert rejected.returncode == 1 and "port" in rejected.stderr

    with socket.create_server(("127.0.0.1", 0)) as listener:
        listener.settimeout(5)
        address = f"http://127.0.0.1:{listener.getsockname()[1]}"
        with subprocess.Popen([*command, address], env=t.env, stderr=subprocess.PIPE, text=True) as client:
            try:
                connection, _ = listener.accept()
                with connection:
                    _, error = client.communicate(timeout=16)
                assert client.returncode == 1 and "timeout" in error.lower(), error
            finally:
                if client.poll() is None:
                    client.kill()


def directory_and_sync(t):
    t.cli(t.a, "login", t.endpoint)
    path = t.a / "disk1/unchanged-stamp.txt"
    path.write_bytes(b"before\n")
    entry = t.cli(t.a, "commit", path)["data"]["container"]
    cid = entry["container_id"]
    stamp = path.stat()
    path.write_bytes(b"after!\n")
    os.utime(path, ns=(stamp.st_atime_ns, stamp.st_mtime_ns))
    assert t.cli(t.a, "status")["entries"][0]["state"] == "clean"
    with ui(t.a, t.config) as action:
        report = action("commit_directory", directory="disk1")
        assert report["committed"] == 1 and not report["issues"], report
    assert t.http("GET", f"/containers/{cid}/versions/2/payload", 200)[0] == b"after!\n"

    for index in range(40):
        t.http("POST", "/folders", 201, {"folder": f"/disk1/empty-{index}/child"})
    calls = []
    folder_posts = []

    def trace(request, response, _data):
        if request.command == "GET":
            calls.append(request.path)
        if request.command == "POST" and request.path == "/folders":
            folder_posts.append(response.status)
        return True

    with proxy_requests(t.port, trace) as endpoint:
        t.cli(t.a, "login", endpoint)
        calls.clear()
        t.cli(t.a, "sync", "--folder", "/disk1/empty-39")
        assert calls == [f"/{kind}?folder=%2Fdisk1%2Fempty-39&recursive=true"
                         for kind in ("folders", "containers")], calls
        assert (t.a / "disk1/empty-39/child").is_dir()
        assert not (t.a / "disk1/empty-38").exists()
        calls.clear()
        assert not t.cli(t.a, "sync")["issues"]
        assert len(calls) == 1 and calls[0].startswith("/containers?ids=") and cid in calls[0], calls
        calls.clear()
        with ui(t.a, t.config) as action:
            assert not action("sync")["issues"]
        assert len(calls) == 1 and calls[0].startswith("/containers?ids=") and cid in calls[0], calls
        calls.clear()
        path.write_bytes(b"append once\n")
        assert t.cli(t.a, "commit", path)["data"]["version"]["version"] == 3
        assert len(calls) == 1 and calls[0].startswith("/containers?ids=") and cid in calls[0], calls

        for name in ("first", "second"):
            (t.a / "disk1/empty-submission" / name).mkdir(parents=True)
        calls.clear()
        with ui(t.a, t.config) as action:
            created = action("commit_directory", directory="disk1/empty-submission")
            unchanged = action("commit_directory", directory="disk1/empty-submission")
        assert created["created_folders"] == 2 and not created["issues"], created
        assert unchanged["created_folders"] == 0 and not unchanged["issues"], unchanged
        assert calls == ["/folders?folder=%2F", "/folders?folder=%2Fdisk1%2Fempty-submission&recursive=true"] * 2, calls
        assert folder_posts == [201, 201], folder_posts

    t.cli(t.a, "login", t.endpoint)
    collision = t.a / "disk1/mixed-names"
    collision.mkdir()
    for name in ("Note.txt", "note.txt", "separate.txt"):
        (collision / name).write_text(name)
    with ui(t.a, t.config) as action:
        report = action("commit_directory", directory="disk1/mixed-names")
    assert report["committed"] == 1, report
    assert {issue["path"] for issue in report["issues"]} == {
        "disk1/mixed-names/Note.txt", "disk1/mixed-names/note.txt",
    }, report
    assert [item["name"] for item in t.cli(t.a, "ls", "/disk1/mixed-names")["containers"]] == ["separate.txt"]

    local = t.b / "disk1" / path.name
    local.write_bytes(b"unsaved local file")
    t.cli(t.b, "login", t.endpoint)
    assert not t.cli(t.b, "sync")["issues"]
    assert t.cli(t.b, "sync", "--folder", "/disk1", expected=1)["issues"]
    assert local.read_bytes() == b"unsaved local file"
    t.stop()
    t.cli(t.a, "refresh", expected=1)


def scoped_listing(t):
    overflow = t.a / "disk1/aaa-overflow"
    target = t.a / "disk1/zzz-selected"
    overflow.mkdir()
    target.mkdir()
    for index in range(1001):
        (overflow / f"{index:04}.txt").touch()
        (t.a / "disk1" / f"browse-empty-{index:04}").mkdir()
    (target / "new.txt").write_bytes(b"new local file")
    with ui(t.a, t.config) as action:
        page = action("browse_workspace", directory="disk1/zzz-selected")
        parent = action("browse_workspace", directory="disk1")
    assert [entry["path"] for entry in page["entries"]] == ["disk1/zzz-selected/new.txt"], page
    assert not page["truncated"], page
    paths = {row["path"] for row in parent["entries"]}
    assert {f"disk1/browse-empty-{index:04}" for index in range(1001)} <= paths
    assert {"disk1/aaa-overflow", "disk1/zzz-selected"} <= paths and not parent["truncated"]


def scoped_remote_edits(t):
    t.login_session()
    parent = t.a / "disk1/scope"
    parent.mkdir()
    local = parent / "original.txt"
    local.write_text("kept in place")
    cid = t.cli(t.a, "commit", local)["data"]["container"]["container_id"]
    t.http("PATCH", f"/containers/{cid}", 200, {"folder": "/disk2", "name": "moved.txt"})
    t.cli(t.a, "mv-folder", "/disk1/scope", "/disk1/renamed-scope")
    assert local.read_text() == "kept in place"

    archived = t.http("POST", "/containers", 201, b"independent",
        headers=upload_headers(dict(folder="/disk2", name="restored.txt", tags=[])))[0]["container"]
    archived_id = archived["container_id"]
    recycled = t.http("DELETE", f"/containers/{archived_id}", 200)[0]
    with ui(t.a, t.config, "--workspace", str(t.a)) as action:
        restored = action("restore", id=recycled["recycle"]["recycle_id"], patch={"folder": "/disk2"})
        assert restored["container"]["container_id"] == archived_id and not restored["issues"]
        action("delete_folder", folder="/disk1/renamed-scope", recursive=False)
        assert local.read_text() == "kept in place"
        action("sync")
        assert (t.a / "disk2/moved.txt").read_text() == "kept in place"


def edit_during_upload(t):
    source = t.a / "disk1/editing.bin"
    block = 1024 * 1024
    source.write_bytes(b"A" * (32 * block))
    stamp = source.stat()
    edited = False

    def read_body(request, length):
        nonlocal edited
        if request.path != "/containers":
            return request.rfile.read(length)
        assert request.headers.get("Zxs-Blake3")
        prefix = request.rfile.read(4 * block)
        assert prefix == b"A" * len(prefix)
        with source.open("r+b") as file:
            file.write(b"B" * length)
        os.utime(source, ns=(stamp.st_atime_ns, stamp.st_mtime_ns))
        edited = True
        return prefix + request.rfile.read(length - len(prefix))

    with proxy_requests(t.port, lambda *_: True, read_body=read_body) as endpoint:
        t.cli(t.a, "login", endpoint)
        t.cli(t.a, "commit", source, expected=1)
    assert edited
    assert not t.http("GET", "/containers?q=editing", 200)[0]["containers"]
    assert source.read_bytes() == b"B" * (32 * block)
    source.unlink()
    t.cli(t.a, "login", t.endpoint)


def storage_integrity(t):
    folder = "/disk1/integrity"
    name = "original.txt"
    content = b"original"
    entry = t.http("POST", "/containers", 201, content,
                   headers=upload_headers(dict(folder=folder, name=name, tags=[])))[0]["container"]
    cid = entry["container_id"]
    payload = t.path(entry) / "v000001" / name

    def rewrite(path, value):
        path.chmod(0o644)
        path.write_bytes(value)
        path.chmod(0o444)

    rewrite(payload, b"damaged!")
    t.http("PATCH", f"/containers/{cid}", 500, {"folder": "/disk2"})
    t.http("PATCH", "/folders", 500, {"from": folder, "to": "/disk2/integrity"})
    assert payload.is_file()
    assert not (t.data / "disk2" / name).exists()
    assert not (t.data / "disk2/integrity").exists()
    t.http("DELETE", f"/containers/{cid}", 500)
    assert payload.is_file()
    rewrite(payload, content)
    extra = payload.parents[2] / "outside-index.txt"
    shutil.copytree(t.path(entry), extra)
    t.http("PATCH", "/folders", 409, {"from": folder, "to": "/disk2/integrity"})
    assert (extra / "v000001" / name).read_bytes() == content
    assert not (t.data / "disk2/integrity").exists()
    shutil.rmtree(extra)
    recycled = t.http("DELETE", f"/containers/{cid}", 200)[0]
    archived = t.archived(recycled) / name / "v000001" / name
    rid = recycled["recycle"]["recycle_id"]
    rewrite(archived, b"damaged!")
    t.stop()
    t.local(SERVER, ["--data-dir", t.data, "verify"], expected=1)
    t.start()
    t.login_session()
    t.http("POST", f"/recycle/{rid}/restore", 500, {"folder": "/disk2"})
    assert archived.is_file()
    t.http("GET", f"/containers/{cid}", 404)
    rewrite(archived, content)
    t.http("POST", f"/recycle/{rid}/restore", 200, {"folder": "/disk2"})
    assert t.http("GET", f"/containers/{cid}/versions/1/payload", 200)[0] == content


def missing_history(t):
    folder = "/disk1/history"
    name = "history.txt"
    entry = t.http("POST", "/containers", 201, b"first",
                   headers=upload_headers(dict(folder=folder, name=name, tags=[])))[0]["container"]
    cid = entry["container_id"]
    t.http("POST", f"/containers/{cid}/versions", 201, b"second",
           headers=upload_headers({"expected_version": 1}))
    original = t.path(entry) / "v000001"
    hidden = t.root / "v1"
    original.rename(hidden)
    t.http("PATCH", f"/containers/{cid}", 500, {"folder": "/disk2"})
    t.http("PATCH", "/folders", 500, {"from": folder, "to": "/disk2/history"})
    t.http("DELETE", f"/containers/{cid}", 500)
    t.stop()
    for command in ("verify", "rebuild-index"):
        t.local(SERVER, ["--data-dir", t.data, command], expected=1)
    index = t.data / ".zxs/index.sqlite"
    saved_index = t.root / "previous.sqlite"
    index.rename(saved_index)
    for command in ("verify", "rebuild-index"):
        t.local(SERVER, ["--data-dir", t.data, command], expected=1)
    saved_index.replace(index)
    hidden.rename(original)
    t.start()
    t.login_session()
    assert t.http("GET", f"/containers/{cid}/versions/1/payload", 200)[0] == b"first"
    live = t.path(entry)
    live.rename(hidden)
    t.http("PATCH", "/folders", 500, {"from": folder, "to": "/disk2/history"})
    assert not (t.data / "disk2/history").exists()
    hidden.rename(live)
    entry = t.http("DELETE", f"/containers/{cid}", 200)[0]
    archive = t.archived(entry)
    manifest = archive / ".zxs"
    record = manifest.read_bytes()
    manifest.unlink()
    t.stop()
    for command in ("verify", "rebuild-index"):
        t.local(SERVER, ["--data-dir", t.data, command], expected=1)
    index = t.data / ".zxs/index.sqlite"
    saved_index = t.root / "previous.sqlite"
    index.rename(saved_index)
    for command in ("verify", "rebuild-index"):
        t.local(SERVER, ["--data-dir", t.data, command], expected=1)
    saved_index.replace(index)
    manifest.write_bytes(record)
    archived_first = archive / name / "v000001"
    hidden_archived = archive / "v1.saved"
    archived_first.rename(hidden_archived)
    t.local(SERVER, ["--data-dir", t.data, "verify"], expected=1)
    hidden_archived.rename(archived_first)
    t.start()
    t.login_session()
    archived_first.rename(hidden_archived)
    rid = entry["recycle"]["recycle_id"]
    t.http("POST", f"/recycle/{rid}/restore", 500, {"folder": folder})
    hidden_archived.rename(archived_first)
    t.http("POST", f"/recycle/{rid}/restore", 200, {"folder": folder})
    assert t.http("GET", f"/containers/{cid}/versions/1/payload", 200)[0] == b"first"
    assert t.http("GET", f"/containers/{cid}/versions/2/payload", 200)[0] == b"second"


def frozen_folder_move(t):
    parent = "/disk1/migrate"
    source = parent + "/photos"
    sibling = parent + "/photos-old"
    target = "/disk2/photos"

    def create(folder, name):
        return t.http("POST", "/containers", 201, b"first",
                      headers=upload_headers(dict(folder=folder, name=name, tags=[])))[0]["container"]

    original = create(source, "original.txt")
    unrelated = create(sibling, "unrelated.txt")
    archived = create("/disk2", "archive.txt")
    cid, other = (entry["container_id"] for entry in (original, unrelated))
    rid = t.http("DELETE", f"/containers/{archived['container_id']}", 200)[0]["recycle"]["recycle_id"]
    separate = parent + "/separate"
    create(separate, "separate.txt")

    def start_append(connection, container):
        connection.putrequest("POST", f"/containers/{container['container_id']}/versions")
        for name, value in upload_headers({"expected_version": 1}).items():
            connection.putheader(name, value)
        connection.putheader("Authorization", f"Bearer {t.token}")
        connection.putheader("Content-Length", "4")
        connection.endheaders(b"ne")
        staging = t.data / "disk1/.zxs/staging"
        for _ in range(200):
            if any(staging.glob(f"*/version/{container['name']}")):
                return
            time.sleep(.025)
        raise AssertionError("append did not reach staging")

    def finish_append(connection):
        connection.send(b"xt")
        response = connection.getresponse()
        body = response.read()
        assert response.status == 201, body

    def wait_for_freeze(probe):
        for _ in range(200):
            probe.request("POST", "/folders", json.dumps({"folder": source}), {
                "Content-Type": "application/json", "Authorization": f"Bearer {t.token}",
            })
            response = probe.getresponse()
            body = response.read()
            if response.status == 423:
                assert json.loads(body)["code"] == "folder_moving", body
                return
            assert response.status == 200, body
            time.sleep(.025)
        raise AssertionError("source folder was not frozen")

    with (
        ThreadPoolExecutor(max_workers=1) as worker,
        context_connection(t.port) as opened,
        context_connection(t.port) as other_opened,
        context_connection(t.port) as probe,
    ):
        start_append(opened, original)
        start_append(other_opened, unrelated)
        move = worker.submit(t.http, "PATCH", "/folders", 200, {"from": source, "to": target})
        wait_for_freeze(probe)

        assert not move.done()
        blocked = t.partial(dict(folder=source, name="blocked.txt", tags=[]), 4)
        try:
            response = blocked.getresponse()
            assert response.status == 423 and b"moving across disks" in response.read()
        finally:
            blocked.close()
        t.http("POST", f"/containers/{cid}/versions", 423, b"late",
               headers=upload_headers({"expected_version": 1}))
        t.http("PATCH", f"/containers/{cid}", 423, {"tags": ["late"]})
        t.http("PATCH", f"/containers/{other}", 423, {"folder": source})
        t.http("DELETE", f"/containers/{cid}", 423)
        t.http("POST", f"/recycle/{rid}/restore", 423, {"folder": source})
        t.http("PATCH", "/folders", 423, {"from": parent, "to": "/disk1/renamed"})
        t.http("DELETE", f"/folders?folder={parent}&recursive=true", 423)
        t.http("PATCH", "/folders", 423, {"from": sibling, "to": "/disk2/second-move"})
        t.http("DELETE", f"/folders?folder={separate}&recursive=true", 200)
        t.http("POST", "/folders", 201, {"folder": sibling + "/new"})
        t.http("POST", "/folders", 201, {"folder": "/disk2/other-work"})
        assert t.http("GET", f"/containers/{cid}/versions/1/payload", 200)[0] == b"first"

        finish_append(opened)
        assert move.result(timeout=8)[0] == {"folder": target}
        finish_append(other_opened)

    detail = t.http("GET", f"/containers/{cid}", 200)[0]
    assert detail["container"]["folder"] == target
    assert [version["version"] for version in detail["versions"]] == [1, 2]
    assert t.http("GET", f"/containers/{cid}/versions/2/payload", 200)[0] == b"next"
    recovered = create(source, "after-move.txt")
    with (
        ThreadPoolExecutor(max_workers=1) as worker,
        context_connection(t.port) as opened,
        context_connection(t.port) as probe,
    ):
        start_append(opened, recovered)
        recycle = worker.submit(t.http, "DELETE", f"/folders?folder={source}&recursive=true", 200)
        wait_for_freeze(probe)
        assert not recycle.done()
        t.http("PATCH", "/folders", 423, {"from": target, "to": source + "/incoming"})
        t.http("GET", f"/containers/{recovered['container_id']}/versions/1/payload", 200)
        finish_append(opened)
        assert recycle.result(timeout=8)[0] == {"folder": source}
    entry = next(entry for entry in t.http("GET", "/recycle", 200)[0]["entries"]
                 if entry["container"]["container_id"] == recovered["container_id"])
    assert entry["container"]["latest_version"] == 2
    t.http("POST", f"/recycle/{entry['recycle']['recycle_id']}/restore", 200, {"folder": source})
    assert t.http("GET", f"/containers/{recovered['container_id']}/versions/2/payload", 200)[0] == b"next"


if __name__ == "__main__":
    with TemporaryDirectory(prefix="zxs-regressions-") as root:
        t = Test(Path(root), Path(root) / "recycle")
        registration(t)
        t.start()
        try:
            t.cli(t.a, "login", t.endpoint)
            ignored_and_recycled(t)
            checkout_types(t)
            unicode_names(t)
            historical_restore(t)
            scoped_remote_edits(t)
        finally:
            t.stop()
        transport(t)
    with TemporaryDirectory(prefix="zxs-integrity-") as root, TemporaryDirectory(
        prefix="zxs-integrity-recycle-", dir=os.environ.get("ZXS_SMOKE_RECYCLE_PARENT")
    ) as recycle:
        t = Test(Path(root), Path(recycle))
        t.offline("init")
        t.start()
        t.login_session()
        try:
            storage_integrity(t)
            missing_history(t)
            frozen_folder_move(t)
            edit_during_upload(t)
            directory_and_sync(t)
            scoped_listing(t)
        finally:
            t.stop()
    print("ZXS data integrity and HTTP regressions passed")
