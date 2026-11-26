"""Check historical restore with the native desktop binary and a portable HTTP fixture."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
from subprocess import run
from tempfile import TemporaryDirectory
from threading import Thread
from urllib.parse import urlsplit


ID = "8" * 32
TOKEN = "a" * 64
FOLDER = "/disk0/资料"
NAME = "原件.txt"
PAYLOADS = [b"published content", b"latest content"]
DIGESTS = [
    "862a7598931edd00dd4f59d9391231c2e21b5c38817fe6fd288ebeef2c661f40",
    "253b560e29385d2b87b396f0415ca168abf5f800ec36fc538b239569816f0674",
]
CONTAINER = dict(container_id=ID, folder=FOLDER, name=NAME, tags=[], latest_version=2,
                 latest_blake3=DIGESTS[1], created_at=1, updated_at=2)
VERSIONS = [
    dict(version=index, size=len(payload), blake3=digest, created_at=index, name_at_commit=NAME)
    for index, (payload, digest) in enumerate(zip(PAYLOADS, DIGESTS), 1)
]


class Fixture(BaseHTTPRequestHandler):
    def reply(self, status, value=None, digest=None):
        body = value if isinstance(value, bytes) else json.dumps(value).encode() if value is not None else b""
        self.send_response(status)
        self.send_header("Content-Type", "application/octet-stream" if digest else "application/json")
        self.send_header("Content-Length", str(len(body)))
        if digest:
            self.send_header("ETag", f'"{digest}"')
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if self.path == "/sessions":
            self.reply(201, {"token": TOKEN})
        else:
            self.server.writes.append(self.path)
            self.reply(405)

    def do_DELETE(self):
        if self.path != "/sessions/current":
            self.server.writes.append(self.path)
        self.reply(204)

    def do_GET(self):
        if self.headers.get("Authorization") != f"Bearer {TOKEN}":
            return self.reply(401)
        path = urlsplit(self.path).path
        if path == "/containers":
            return self.reply(200, {"containers": [CONTAINER], "next_cursor": None})
        if path == "/folders":
            return self.reply(200, {"folders": [{"folder": "/disk0"}, {"folder": FOLDER}]})
        if path == f"/containers/{ID}":
            return self.reply(200, {"container": CONTAINER, "versions": VERSIONS})
        for index, payload in enumerate(PAYLOADS, 1):
            if path == f"/containers/{ID}/versions/{index}/payload":
                return self.reply(200, payload, DIGESTS[index - 1])
        return self.reply(404)

    def log_message(self, *_):
        pass


def main():
    binary = Path(os.environ["ZXS_CLIENT_BIN"]).resolve()
    with TemporaryDirectory(prefix="zxs-platform-history-") as path, ThreadingHTTPServer(("127.0.0.1", 0), Fixture) as server:
        server.writes = []
        worker = Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            root = Path(path)
            workspace = root / "workspace"
            workspace.mkdir()
            env = dict(os.environ, ZXS_CONFIG_DIR=str(root / "profile"), ZXS_PASSWORD="fixture")
            target = workspace / FOLDER.lstrip("/") / NAME

            def cli(*args, expected=0):
                result = run([binary, "--workspace", workspace, *args], env=env,
                             capture_output=True, text=True, encoding="utf-8", timeout=20)
                assert result.returncode == expected, (args, result.returncode, result.stdout, result.stderr)
                return json.loads(result.stdout) if result.stdout else None

            cli("login", f"http://127.0.0.1:{server.server_port}")
            cli("checkout", ID)
            assert target.read_bytes() == PAYLOADS[1]
            target.write_bytes(b"unsaved local edit")
            cli("checkout", ID, "--version", "1", expected=3)
            assert target.read_bytes() == b"unsaved local edit"
            restored = cli("checkout", ID, "--version", "1", "--backup")
            assert target.read_bytes() == PAYLOADS[0]
            assert Path(restored["backup"]).read_bytes() == b"unsaved local edit"
            assert restored["restored_from"] == 1
            cli("sync")
            assert target.read_bytes() == PAYLOADS[0]
            assert cli("status")["entries"][0]["state"] == "modified"
            assert not server.writes, server.writes
        finally:
            server.shutdown()
            worker.join()
    print("Native desktop historical restore and local edit protection passed")


if __name__ == "__main__":
    main()
