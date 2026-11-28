"""Rate and byte progress with a throttled HTTP relay and an interactive terminal."""

from contextlib import contextmanager
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
from urllib.parse import parse_qsl, urlencode, urlsplit

from behavior_support import ServerCase
from smoke import CLIENT


class SlowPayloads(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def pipe(self, source, write, length, limited, pause=None):
        while length:
            block = source.read(min(length, 128 * 1024))
            if not block:
                raise EOFError("fixture relay received a short body")
            write(block)
            length -= len(block)
            if pause is not None and length:
                # Leave an idle window for UIAutomator between reads.
                pause.wait(2)
            elif limited:
                sleep(self.server.block_delay)

    def relay(self):
        remote = HTTPConnection("127.0.0.1", self.server.upstream, timeout=15)
        try:
            path = self.path
            if self.server.page_limit is not None and path.startswith("/containers?"):
                query = dict(parse_qsl(urlsplit(path).query, keep_blank_values=True))
                query.setdefault("limit", self.server.page_limit)
                path = "/containers?" + urlencode(query)
            remote.putrequest(self.command, path)
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
                      self.path.endswith("/payload"), self.server.paused_payloads.get(self.path))
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            remote.close()

    do_POST = do_GET = do_DELETE = relay


@contextmanager
def slow_payloads(port, block_delay=.035, page_limit=None, paused_payloads=None):
    with ThreadingHTTPServer(("127.0.0.1", 0), SlowPayloads) as relay:
        relay.upstream = port
        relay.block_delay = block_delay
        relay.page_limit = page_limit
        relay.paused_payloads = {} if paused_payloads is None else paused_payloads
        worker = Thread(target=relay.serve_forever, daemon=True)
        worker.start()
        try:
            yield f"http://127.0.0.1:{relay.server_port}"
        finally:
            relay.shutdown()
            worker.join()


class TransferBehavior(ServerCase):
    def test_terminal_tracks_both_directions_and_retains_json(self):
        t = self.zxs
        endpoint = self.enterContext(slow_payloads(t.port))
        t.cli(t.a, "login", endpoint)
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
