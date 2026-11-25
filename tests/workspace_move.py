#!/usr/bin/env python3
"""Exercise workspace relocation through the local UI on macOS and Linux."""

import http.client
import json
import os
from contextlib import contextmanager
from pathlib import Path
from selectors import DefaultSelector, EVENT_READ
import subprocess
from tempfile import TemporaryDirectory
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[1]
CLIENT = Path(os.environ.get("ZXS_CLIENT_BIN", ROOT / "zxs-client/target/debug/zxs-cli")).resolve()


@contextmanager
def ui(cwd, config, *args):
    env = dict(os.environ, PATH="", ZXS_CONFIG_DIR=str(config), ZXS_WORKSPACE=str(cwd))
    process = subprocess.Popen([CLIENT, *args, "ui"], cwd=cwd, env=env, stdout=subprocess.PIPE, text=True)
    try:
        with DefaultSelector() as selector:
            selector.register(process.stdout, EVENT_READ)
            assert selector.select(10), "the workspace UI did not start"
        address = urlsplit(process.stdout.readline().strip())
        assert address.hostname == "127.0.0.1" and address.port and address.fragment, "missing UI URL"

        def post(path, body, cookie=None):
            connection = http.client.HTTPConnection(address.hostname, address.port, timeout=10)
            headers = {"Content-Type": "application/json", "Origin": f"{address.scheme}://{address.netloc}"}
            if cookie:
                headers["Cookie"] = cookie
            try:
                connection.request("POST", path, body=json.dumps(body), headers=headers)
                response = connection.getresponse()
                data = response.read()
                assert response.status == 200, (path, response.status, data)
                return json.loads(data), response.getheader("Set-Cookie")
            finally:
                connection.close()

        _, cookie = post("/access", {"token": address.fragment})
        assert cookie, "the UI did not authorize its startup URL"

        def action(operation, **fields):
            data, _ = post("/api", {"action": operation, **fields}, cookie.split(";", 1)[0])
            return data

        yield action
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        process.stdout.close()


def check_move(root, destination):
    initial = root / "workspace"
    renamed = root / "WORKSPACE"
    final = destination / "Moved Workspace"
    config = root / "profile"
    draft = Path("disk1/drafts/local.txt")
    (initial / draft.parent / "empty").mkdir(parents=True)
    (initial / draft).write_bytes(b"unsubmitted contents")
    (initial / "draft-link").symlink_to(draft)
    before = (initial / draft).stat()
    if os.environ.get("ZXS_WORKSPACE_MOVE_PARENT"):
        assert initial.stat().st_dev != destination.stat().st_dev, "expected separate filesystems"

    # The explicit workspace bypasses reading the saved location at startup. Settings
    # still need to resolve relative to the initial cwd when the UI moves that directory.
    with ui(initial, "../profile", "--workspace", str(initial)) as action:
        assert action("session")["workspace"] == str(initial)
        action("browse_workspace", directory=".")
        db = Path(".zxs/workspace.sqlite")
        index = (initial / db).read_bytes()

        result = action("move_workspace", name=renamed.name, parent=str(root))
        assert not result["warnings"], result["warnings"]
        assert result["workspace"]["workspace"] == str(renamed)
        assert renamed.name in os.listdir(root)
        assert initial.name not in os.listdir(root)
        assert (renamed / db).read_bytes() == index

        result = action("move_workspace", name=final.name, parent=str(destination))
        assert not result["warnings"], result["warnings"]
        assert result["workspace"]["workspace"] == str(final)
        assert action("session")["workspace"] == str(final)
        assert not renamed.exists()
        assert (final / db).read_bytes() == index
        assert (final / draft).read_bytes() == b"unsubmitted contents"
        assert (final / draft).stat().st_mtime_ns == before.st_mtime_ns
        assert (final / draft.parent / "empty").is_dir()
        assert (final / "draft-link").readlink() == draft

    with ui(root, config) as action:
        assert action("session")["workspace"] == str(final)
        action("browse_workspace", directory=draft.parent.as_posix())


if __name__ == "__main__":
    with TemporaryDirectory(prefix="zxs-workspace-") as root, TemporaryDirectory(
        prefix="zxs-move-", dir=os.environ.get("ZXS_WORKSPACE_MOVE_PARENT", root)
    ) as destination:
        check_move(Path(root).resolve(), Path(destination).resolve())
    print("Workspace rename, move and restart passed")
