"""Isolated real-server fixtures for the behavior tests."""

from pathlib import Path
from tempfile import TemporaryDirectory
from unittest import TestCase
from urllib.parse import urlencode

from smoke import Test, upload_headers


class ServerCase(TestCase):
    def setUp(self):
        directory = self.enterContext(TemporaryDirectory(prefix="zxs-behavior-"))
        root = Path(directory)
        self.zxs = Test(root, root / "recycle")
        self.addCleanup(self.zxs.stop)
        self.zxs.offline("init")
        self.zxs.start()
        self.zxs.login_session()

    def create(self, folder, name, data=b"", tags=()):
        headers = upload_headers({"folder": folder, "name": name, "tags": list(tags)})
        return self.zxs.http("POST", "/containers", 201, data, headers)[0]

    def listing(self, status=200, **filters):
        return self.zxs.http("GET", "/containers?" + urlencode(filters), status)[0]

    def detail(self, container_id):
        return self.zxs.http("GET", f"/containers/{container_id}", 200)[0]

    def append(self, container_id, expected, data):
        return self.zxs.http("POST", f"/containers/{container_id}/versions", 201, data,
                             upload_headers({"expected_version": expected}))[0]
