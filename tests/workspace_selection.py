#!/usr/bin/env python3
"""Exercise workspace selection through the local UI on macOS and Linux."""

import http.client
import json
import os
from contextlib import ExitStack, contextmanager
from pathlib import Path
from selectors import DefaultSelector, EVENT_READ
import socket
import subprocess
from tempfile import TemporaryDirectory
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[1]
CLIENT = Path(os.environ.get("ZXS_CLIENT_BIN", ROOT / "zxs-client/target/debug/zxs-cli")).resolve()


@contextmanager
def ui_address(cwd, config, *args):
    env = dict(os.environ, PATH="", ZXS_CONFIG_DIR=str(config), ZXS_WORKSPACE=str(cwd))
    process = subprocess.Popen([CLIENT, *args, "ui"], cwd=cwd, env=env, stdout=subprocess.PIPE, text=True)
    try:
        with DefaultSelector() as selector:
            selector.register(process.stdout, EVENT_READ)
            assert selector.select(10), "the workspace UI did not start"
        address = urlsplit(process.stdout.readline().strip())
        assert address.hostname == "127.0.0.1" and address.port and address.fragment, "missing UI URL"
        yield address
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        process.stdout.close()


@contextmanager
def ui(cwd, config, *args):
    with ui_address(cwd, config, *args) as address:
        def post(path, body, cookie=None, target=None):
            connection = http.client.HTTPConnection(address.hostname, address.port, timeout=10)
            headers = {"Content-Type": "application/json", "Origin": f"{address.scheme}://{address.netloc}"}
            if cookie:
                headers["Cookie"] = cookie
            if target:
                headers["X-ZXS-Target"] = target
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
        session_cookie = cookie.split(";", 1)[0]
        session, _ = post("/api", {"action": "session"}, session_cookie)
        target = session["target"]

        def action(operation, **fields):
            nonlocal target
            data, _ = post("/api", {"action": operation, **fields}, session_cookie, target)
            target = data.get("target", target)
            return data

        yield action


def check_parallel_assets(root):
    affinity = os.sched_getaffinity(0)
    os.sched_setaffinity(0, {min(affinity)})
    try:
        for _ in range(6):
            with ui_address(root, root / "profile") as address, ExitStack() as sockets:
                request = f"GET /widgets.js HTTP/1.1\r\nHost: {address.netloc}\r\nConnection: keep-alive\r\n\r\n".encode()
                clients = []
                for _ in range(6):
                    connection = sockets.enter_context(socket.socket())
                    connection.setblocking(False)
                    connection.connect_ex((address.hostname, address.port))
                    clients.append(connection)
                for connection in clients:
                    connection.settimeout(2)
                    connection.sendall(request)
                for connection in clients:
                    with http.client.HTTPResponse(connection) as response:
                        response.begin()
                        assert response.status == 200 and response.read(), "missing local script response"
    finally:
        os.sched_setaffinity(0, affinity)


def check_selection(root):
    initial = root / "workspace"
    selected = root / "other workspace"
    config = root / "profile"
    draft = Path("disk1/drafts/local.txt")
    (initial / draft.parent).mkdir(parents=True)
    (initial / draft).write_bytes(b"unsubmitted contents")
    selected.mkdir()
    (selected / "separate.txt").write_bytes(b"separate workspace")

    with ui(root, config, "--workspace", str(initial)) as action:
        assert action("session")["workspace"] == str(initial)
        action("browse_workspace", directory=".")
        index = initial / ".zxs/workspace.sqlite"
        before = index.read_bytes()
        assert action("select_workspace", path=str(selected))["workspace"] == str(selected)
        page = action("browse_workspace", directory=".")
        assert [entry["path"] for entry in page["entries"]] == ["separate.txt"]
        assert index.read_bytes() == before
        assert (initial / draft).read_bytes() == b"unsubmitted contents"
        assert not (selected / draft).exists()
        assert json.loads((config / "workspace-location.json").read_bytes()) == {"path": str(selected)}

    with ui(root, config) as action:
        assert action("session")["workspace"] == str(selected)
        assert action("select_workspace", path=str(initial))["workspace"] == str(initial)
        page = action("browse_workspace", directory=draft.parent.as_posix())
        assert [entry["path"] for entry in page["entries"]] == [draft.as_posix()]

    moved = initial.rename(root / "manually moved")
    with ui(root, config) as action:
        assert action("session")["workspace"] == str(initial)
        assert action("select_workspace", path=str(moved))["workspace"] == str(moved)
        page = action("browse_workspace", directory=draft.parent.as_posix())
        assert [entry["path"] for entry in page["entries"]] == [draft.as_posix()]
        assert (moved / draft).read_bytes() == b"unsubmitted contents"
        assert (moved / ".zxs/workspace.sqlite").read_bytes() == before
        assert not initial.exists()


if __name__ == "__main__":
    with TemporaryDirectory(prefix="zxs-workspace-") as root:
        root = Path(root).resolve()
        if hasattr(os, "sched_getaffinity"):
            check_parallel_assets(root)
        check_selection(root)
    print("Workspace selection, persistence and local file preservation passed")
