"""Regressions for historical workspace restores and local publication."""

import json
from pathlib import Path
import sqlite3
from tempfile import TemporaryDirectory
import zipfile

from smoke import Test, proxy_requests


def ignored_members(t):
    project = t.a / "disk1/project"
    (project / "cache").mkdir(parents=True)
    t.cli(t.a, "init-bundle", project)
    (project / ".zxsignore").write_text("cache\n!cache/\n")
    container = t.cli(t.a, "commit", project)["data"]["container"]
    cid = container["container_id"]
    t.cli(t.b, "checkout", cid)
    local = t.b / "disk1/project/cache"
    local.rmdir()
    local.write_text("ignored local data")
    (project / "cache").rmdir()
    (project / "member.txt").write_text("version two")
    t.cli(t.a, "commit", project)
    result = t.cli(t.b, "checkout", cid, "--backup")
    assert result["version"] == 2 and local.read_text() == "ignored local data"


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
    directory, file = t.b / "disk1/converted", t.b / "disk1/converted.zip"
    (directory / "local.txt").write_text("ignored")
    file.write_text("do not overwrite")
    t.cli(t.b, "checkout", cid, "--version", 1, "--backup", expected=3)
    assert (directory / "member.txt").read_text() == "version two"
    assert file.read_text() == "do not overwrite"
    file.unlink()
    previous = t.cli(t.b, "checkout", cid, "--version", 1)
    assert file.read_bytes() == ordinary
    assert (directory / "local.txt").read_text() == "ignored"
    assert (Path(previous["backup"]) / "member.txt").read_text() == "version two"
    latest = t.cli(t.b, "checkout", cid, "--backup")
    assert (directory / "member.txt").read_text() == "version two" and not file.exists()
    assert (directory / "local.txt").read_text() == "ignored"
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
                   (cid, pending["path"], json.dumps(pending)))
    states = [row["state"] for row in t.cli(t.c, "status")["entries"]
              if row["container_id"] == cid]
    assert states == ["interrupted"]
    t.cli(t.c, "sync-delete", relative, "--expected-version", 3, expected=3)
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

if __name__ == "__main__":
    with TemporaryDirectory(prefix="zxs-restore-") as root:
        t = Test(Path(root), Path(root) / "recycle")
        t.offline("init")
        t.start()
        try:
            t.cli(t.a, "login", t.endpoint)
            ignored_members(t)
            checkout_types(t)
            unicode_names(t)
            historical_restore(t)
        finally:
            t.stop()
    print("ZXS workspace restore regressions passed")
