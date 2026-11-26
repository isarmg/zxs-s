"""Rate and byte progress with a throttled HTTP relay and an interactive terminal."""

from errno import EIO
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from select import select
from subprocess import PIPE, Popen
from threading import Thread
from time import monotonic, sleep
import re

from behavior_support import ServerCase
from smoke import CLIENT


class SlowPayloads(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    @staticmethod
    def pipe(source, write, length, limited):
        while length:
            block = source.read(min(length, 128 * 1024))
            if not block:
                raise EOFError("fixture relay received a short body")
            write(block)
            length -= len(block)
            if limited:
                sleep(.035)

    def relay(self):
        remote = HTTPConnection("127.0.0.1", self.server.upstream, timeout=15)
        try:
            remote.putrequest(self.command, self.path)
            for name, value in self.headers.items():
                if name.lower() not in ("expect", "host"):
                    remote.putheader(name, value)
            remote.endheaders()
            self.pipe(self.rfile, remote.send, int(self.headers.get("Content-Length", 0)),
                      self.headers.get("Content-Type") == "application/octet-stream")
            result = remote.getresponse()
            self.send_response(result.status)
            for name, value in result.getheaders():
                if name.lower() not in ("date", "server", "connection"):
                    self.send_header(name, value)
            self.end_headers()
            self.pipe(result, self.wfile.write, int(result.getheader("Content-Length", "0")),
                      self.path.endswith("/payload"))
        finally:
            remote.close()

    do_POST = do_GET = do_DELETE = relay


class TransferBehavior(ServerCase):
    def test_terminal_tracks_both_directions_and_retains_json(self):
        t = self.zxs
        relay = self.enterContext(ThreadingHTTPServer(("127.0.0.1", 0), SlowPayloads))
        relay.upstream = t.port
        worker = Thread(target=relay.serve_forever, daemon=True)
        worker.start()
        self.addCleanup(worker.join)
        self.addCleanup(relay.shutdown)
        t.cli(t.a, "login", f"http://127.0.0.1:{relay.server_port}")
        contents = bytes(range(256)) * (8 * 1024 * 1024 // 256)
        source = t.a / "disk1/bytes.bin"
        source.write_bytes(contents)
        result, upload = self.run_terminal(t.a, "commit", "disk1/bytes.bin")
        cid = result["data"]["container"]["container_id"]
        checkout, download = self.run_terminal(t.b, "checkout", cid)
        self.assertEqual((t.b / checkout["local"]).read_bytes(), contents)
        for direction, output in [("上传", upload), ("下载", download)]:
            with self.subTest(direction=direction):
                percentages = [int(value) for value in re.findall(direction + r" \[[^\]]+\]\s+(\d+)%", output)]
                self.assertTrue(any(0 < value < 100 for value in percentages), output)
                self.assertEqual(percentages, sorted(percentages))
                self.assertEqual((percentages[0], percentages[-1]), (0, 100))
                self.assertRegex(output, r"[1-9]\d*\.\d [KMG]iB/s")

    def run_terminal(self, workspace, *args):
        import pty

        terminal, writer = pty.openpty()
        try:
            child = Popen([CLIENT, "--workspace", workspace, *args], env=self.zxs.env, stdout=PIPE, stderr=writer)
        finally:
            os.close(writer)
        with child:
            try:
                output = self.read_terminal(terminal)
                stdout, _ = child.communicate(timeout=5)
            except BaseException:
                child.kill()
                raise
            finally:
                os.close(terminal)
            self.assertEqual(child.returncode, 0, output)
            return json.loads(stdout), output

    def read_terminal(self, terminal):
        chunks = []
        deadline = monotonic() + 25
        while (remaining := deadline - monotonic()) > 0 and select([terminal], [], [], remaining)[0]:
            try:
                chunk = os.read(terminal, 4096)
            except OSError as error:
                if error.errno == EIO:
                    break
                raise
            if not chunk:
                break
            chunks.append(chunk)
        else:
            self.fail("CLI transfer timed out")
        return b"".join(chunks).decode()
