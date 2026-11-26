"""HTTP contract and on-disk invariants exercised through the real service."""

from concurrent.futures import ThreadPoolExecutor
import json
from threading import Barrier
from time import monotonic, sleep
from urllib.parse import unquote

from behavior_support import ServerCase
from smoke import SERVER, context_connection, upload_headers


class ServerBehavior(ServerCase):
    def test_requests_without_a_session_leave_versions_intact(self):
        initial = self.create("/disk1/docs", "note.txt", b"original")
        cid = initial["container"]["container_id"]
        base = f"/containers/{cid}"
        t = self.zxs
        for method, path, data, headers in [
            ("GET", "/containers", None, {}),
            ("GET", base, None, {}),
            ("HEAD", base + "/versions/1/payload", None, {}),
            ("GET", base + "/versions/1/payload", None, {"Range": "bytes=0-1"}),
            ("PATCH", base, {"name": "other.txt"}, {}),
            ("POST", base + "/versions", b"new", upload_headers({"expected_version": 1})),
            ("DELETE", base, None, {}),
            ("POST", "/folders", {"folder": "/disk2/unwritten"}, {}),
            ("DELETE", "/recycle", None, {}),
        ]:
            with self.subTest(method=method, path=path):
                rejected, _ = t.http(method, path, 401, data, headers, authenticated=False)
                if method == "HEAD":
                    self.assertEqual(rejected, b"")
        self.assertEqual(self.detail(cid), {"container": initial["container"], "versions": [initial["version"]]})
        self.assertFalse((t.data / "disk2/unwritten").exists())
        self.assertEqual(t.http("GET", "/recycle", 200)[0]["entries"], [])

    def test_full_partial_and_empty_payload_contract(self):
        body = b"0123456789"
        commit = self.create("/disk1/docs", "原件 1.txt", body)
        path = f"/containers/{commit['container']['container_id']}/versions/1/payload"
        for method, requested, status, expected, span, length in [
            ("GET", None, 200, body, None, "10"),
            ("HEAD", None, 200, b"", None, "10"),
            ("GET", "bytes=2-4", 206, b"234", "bytes 2-4/10", "3"),
            ("HEAD", "bytes=7-99", 206, b"", "bytes 7-9/10", "3"),
            ("GET", "bytes=-3", 206, b"789", "bytes 7-9/10", "3"),
            ("GET", "bytes=0-0,2-2", 416, b"", "bytes */10", None),
            ("GET", "bytes=10-", 416, b"", "bytes */10", None),
        ]:
            with self.subTest(method=method, requested=requested):
                headers = {"Range": requested} if requested else None
                found, reply = self.zxs.http(method, path, status, headers=headers)
                self.assertEqual(found, expected)
                self.assertEqual(reply.get("Content-Range"), span)
                self.assertEqual(reply["ETag"], f'"{commit["version"]["blake3"]}"')
                if length is not None:
                    self.assertEqual(reply["Content-Length"], length)
                    self.assertTrue(unquote(reply["Content-Disposition"]).endswith("原件 1.txt"))
        empty = self.create("/disk1/docs", "empty.bin")
        path = f"/containers/{empty['container']['container_id']}/versions/1/payload"
        found, headers = self.zxs.http("GET", path, 200)
        self.assertEqual((found, headers["Content-Length"]), (b"", "0"))
        self.assertEqual(self.zxs.http("GET", path, 416, headers={"Range": "bytes=0-0"})[1]["Content-Range"],
                         "bytes */0")

    def test_paginated_filter_and_id_lookup_return_the_same_records(self):
        folder = "/disk1/资料"
        expected = [self.create(folder, name, tags=("主题",))["container"]
                    for name in ("café-a.txt", "café-b.txt", "café-c.txt")]
        other = self.create("/disk2/资料", "café-a.txt", tags=("另一主题",))["container"]
        cursor = self.listing(folder=folder, limit=1)["next_cursor"]
        self.assertTrue(cursor)
        self.listing(400, folder=other["folder"], cursor=cursor)

        filters = {"folder": folder, "q": "CAFE\u0301", "tag": "主题", "limit": 1}
        first = self.listing(**filters)
        second = self.listing(**filters, cursor=first["next_cursor"])
        third = self.listing(**filters, cursor=second["next_cursor"])
        self.assertEqual(first["containers"] + second["containers"] + third["containers"], expected)
        self.assertIsNone(third["next_cursor"])
        self.assertEqual(self.listing(q="CAFÉ")["containers"], expected + [other])
        ids = ",".join(item["container_id"] for item in (expected[0], other))
        by_id = self.listing(ids=ids)["containers"]
        self.assertEqual({item["container_id"]: item for item in by_id},
                         {item["container_id"]: item for item in (expected[0], other)})
        self.listing(400, ids=ids, q="café")

    def test_concurrent_writers_with_one_baseline_publish_once(self):
        t = self.zxs
        cid = self.create("/disk1/race", "draft.txt", b"v1")["container"]["container_id"]
        path = f"/containers/{cid}/versions"
        barrier = Barrier(2)

        def compete(data):
            headers = upload_headers({"expected_version": 1}) | {"Authorization": f"Bearer {t.token}"}
            with context_connection(t.port) as connection:
                barrier.wait(timeout=5)
                connection.request("POST", path, data, headers)
                response = connection.getresponse()
                return response.status, json.loads(response.read()), data

        with ThreadPoolExecutor(max_workers=2) as pool:
            outcomes = list(pool.map(compete, (b"first writer", b"second writer")))
        self.assertCountEqual([status for status, _, _ in outcomes], [201, 409])
        _, receipt, accepted = next(outcome for outcome in outcomes if outcome[0] == 201)
        self.assertEqual(receipt["version"]["version"], 2)
        self.assertEqual(t.http("GET", path + "/2/payload", 200)[0], accepted)
        self.assertEqual(sorted(v["version"] for v in self.detail(cid)["versions"]), [1, 2])
        t.stop()
        self.assertIn("Verified 2 versions", t.offline("verify"))

    def test_disconnected_request_cleans_stage_and_leaves_no_version_gap(self):
        t = self.zxs
        first = self.create("/disk1/docs", "note.txt", b"v1")
        cid = first["container"]["container_id"]
        stage = t.data / "disk1/.zxs/staging"
        headers = upload_headers({"expected_version": 1}) | {
            "Content-Length": "100000", "Authorization": f"Bearer {t.token}", "Expect": "100-continue",
        }
        with context_connection(t.port) as connection:
            connection.putrequest("POST", f"/containers/{cid}/versions")
            for name, value in headers.items():
                connection.putheader(name, value)
            connection.endheaders(b"partial bytes")
            self.wait_for(lambda: any(stage.iterdir()), "upload did not reach staging")
        self.wait_for(lambda: not any(stage.iterdir()), "abandoned upload left staging files")
        self.assertEqual(self.detail(cid)["versions"], [first["version"]])
        self.assertEqual(self.append(cid, 1, b"retried body")["version"]["version"], 2)

    def test_offline_commands_require_lock_and_prune_retains_latest(self):
        t = self.zxs
        first = self.create("/disk1/docs", "retained.txt", b"version one")
        cid = first["container"]["container_id"]
        last = self.append(cid, 1, b"version two")
        for command in ("verify", "rebuild-index", "sync-disks"):
            with self.subTest(command=command):
                t.local(SERVER, ["--data-dir", t.data, command], expected=1)
        t.stop()
        t.local(SERVER, ["--data-dir", t.data, "prune", "--keep-last", 0], expected=1)
        self.assertIn("Verified 2 versions", t.offline("verify"))
        self.assertIn("Pruned 1 versions", t.offline("prune", "--keep-last", 1, "--container", cid))
        self.assertIn("Verified 1 versions", t.offline("verify"))
        t.offline("rebuild-index")
        t.start()
        t.login_session()
        self.assertEqual(self.detail(cid)["versions"], [last["version"]])
        self.assertEqual(self.zxs.http("GET", f"/containers/{cid}/versions/2/payload", 200)[0], b"version two")
        self.assertEqual(self.append(cid, 2, b"version three")["version"]["version"], 3)

    def wait_for(self, available, problem):
        deadline = monotonic() + 5
        while monotonic() < deadline:
            if available():
                return
            sleep(.025)
        self.fail(problem)
