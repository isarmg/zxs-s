"""HTTP contract and on-disk invariants exercised through the real service."""

from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack, closing
import json
from shutil import copytree
import socket
import sqlite3
import subprocess
from threading import Barrier
from time import monotonic, sleep, time
from urllib.parse import unquote

from behavior_support import ServerCase
from smoke import EMPTY_HASH, PASSWORD, SERVER, context_connection, upload_headers


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

    def test_logout_only_revokes_the_selected_session(self):
        t = self.zxs
        second = t.http("POST", "/sessions", 201, {"password": PASSWORD}, authenticated=False)[0]
        self.assertNotEqual(t.token, second["token"])
        headers = {"Authorization": f"Bearer {second['token']}"}
        self.assertEqual(t.http("GET", "/containers", 200, headers=headers, authenticated=False)[0]["containers"], [])
        t.http("DELETE", "/sessions/current", 204, headers=headers, authenticated=False)
        t.http("GET", "/containers", 401, headers=headers, authenticated=False)
        saved = self.create("/disk1", "session.txt", b"first session still works")["container"]
        self.assertEqual(self.listing()["containers"], [saved])

    def test_password_reset_does_not_scan_or_change_the_index(self):
        t = self.zxs
        saved = self.create("/disk1", "history.txt", b"original")["container"]
        t.local(SERVER, ["--data-dir", t.data, "reset-password"], expected=1)
        t.stop()
        version = t.path(saved) / "v000001"
        hidden = t.root / "history-copy"
        version.rename(hidden)
        index = t.data / ".zxs/index.sqlite"
        previous = index.read_bytes()
        t.env["ZXS_PASSWORD"] = "new-test-password"
        self.assertIn("Password reset", t.offline("reset-password"))
        self.assertEqual(index.read_bytes(), previous)
        self.assertTrue(hidden.is_dir())
        hidden.rename(version)
        t.start()
        t.http("POST", "/sessions", 401, {"password": PASSWORD}, authenticated=False)
        t.http("POST", "/sessions", 201, {"password": t.env["ZXS_PASSWORD"]}, authenticated=False)

    def test_recursive_listing_paginates_only_the_requested_subtree(self):
        parent = self.create("/disk1/chosen", "a.txt")["container"]
        child = self.create("/disk1/chosen/sub", "b.txt")["container"]
        sibling = self.create("/disk1/chosen.next", "c.txt")["container"]
        filters = {"folder": "/disk1/chosen", "recursive": "true", "limit": 1}
        first = self.listing(**filters)
        self.assertEqual(first["containers"], [parent])
        second = self.listing(**filters, cursor=first["next_cursor"])
        self.assertEqual(second, {"containers": [child], "next_cursor": None})
        self.assertEqual(self.listing(folder="/disk1/chosen")["containers"], [parent])
        self.assertIn(sibling, self.listing()["containers"])

    def test_full_partial_and_empty_payload_contract(self):
        t = self.zxs
        body = b"0123456789"
        commit = self.create("/disk1/docs", "原件 1.txt", body, tags=("archive", "original"))
        cid = commit["container"]["container_id"]
        container = f"/containers/{cid}"
        path = container + "/versions/1/payload"
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
                found, reply = t.http(method, path, status, headers=headers)
                self.assertEqual(found, expected)
                self.assertEqual(reply.get("Content-Range"), span)
                self.assertEqual(reply["ETag"], f'"{commit["version"]["blake3"]}"')
                if length is not None:
                    self.assertEqual(reply["Content-Length"], length)
                    self.assertTrue(unquote(reply["Content-Disposition"]).endswith("原件 1.txt"))
        t.http("PATCH", container, 200, {"folder": "/disk1/Archive/Reports", "name": "current.pdf"})
        self.append(cid, 1, b"second version")
        found, headers = t.http("GET", path, 206, headers={"Range": "bytes=2-4"})
        self.assertEqual(found, b"234")
        self.assertTrue(unquote(headers["Content-Disposition"]).endswith("原件 1.txt"))
        self.assertEqual(headers["ETag"], f'"{commit["version"]["blake3"]}"')
        found, headers = t.http("GET", container + "/versions/2/payload", 200)
        self.assertEqual((found, headers["Content-Type"]), (b"second version", "application/pdf"))
        missing, _ = t.http("GET", container + "/versions/3/payload", 404)
        self.assertEqual(missing, {"code": "not_found", "message": "version not found"})
        missing, _ = t.http("GET", f"/containers/{'0' * 32}/versions/1/payload", 404)
        self.assertEqual(missing, {"code": "not_found", "message": "container not found"})
        empty = self.create("/disk1/docs", "empty.bin")
        path = f"/containers/{empty['container']['container_id']}/versions/1/payload"
        found, headers = t.http("GET", path, 200)
        self.assertEqual((found, headers["Content-Length"]), (b"", "0"))
        self.assertEqual(t.http("GET", path, 416, headers={"Range": "bytes=0-0"})[1]["Content-Range"],
                         "bytes */0")

    def test_octet_stream_type_is_case_insensitive_for_uploads(self):
        t = self.zxs
        params = {"folder": "/disk1", "name": "note.txt", "tags": []}
        t.http("POST", "/containers", 400, b"test", upload_headers(params) | {"Content-Type": "text/plain"})
        created, _ = t.http("POST", "/containers", 201, b"test",
                            upload_headers(params) | {"Content-Type": "Application/Octet-Stream"})
        cid = created["container"]["container_id"]
        path = f"/containers/{cid}/versions"
        appended, _ = t.http("POST", path, 201, b"updated",
                             upload_headers({"expected_version": 1}) | {"Content-Type": "APPLICATION/OCTET-STREAM"})
        self.assertEqual(appended["version"]["version"], 2)
        self.assertEqual(t.http("GET", path + "/2/payload", 200)[0], b"updated")

    def test_short_downloads_and_empty_uploads_finish_while_transfer_slots_are_busy(self):
        t = self.zxs
        preview_body = b"0123456789" * 5
        preview = self.create("/disk2", "preview.mp4", preview_body)
        path = f"/containers/{preview['container']['container_id']}/versions/1/payload"
        body = b"pending upload"
        with ExitStack() as stack:
            uploads = []
            for number in range(8):
                connection = stack.enter_context(context_connection(t.port))
                connection.putrequest("POST", "/containers")
                headers = upload_headers({"folder": "/disk1", "name": f"queued-{number}.bin", "tags": []}) | {
                    "Authorization": f"Bearer {t.token}", "Content-Length": str(len(body)), "Expect": "100-continue",
                }
                for name, value in headers.items():
                    connection.putheader(name, value)
                connection.endheaders()
                with connection.sock.makefile("rb") as acknowledgement:
                    self.assertEqual(acknowledgement.readline(), b"HTTP/1.1 100 Continue\r\n")
                    self.assertEqual(acknowledgement.readline(), b"\r\n")
                uploads.append(connection)

            payload, headers = t.http("GET", path, 206, headers={"Range": "bytes=0-39"})
            self.assertEqual(payload, preview_body[:40])
            self.assertEqual(headers["Content-Range"], "bytes 0-39/50")
            payload, _ = t.http("GET", path, 200)
            self.assertEqual(payload, preview_body)
            empty = self.create("/disk2", "empty.txt")
            cid = empty["container"]["container_id"]
            appended = self.append(cid, 1, b"")
            for version in (empty["version"], appended["version"]):
                self.assertEqual((version["size"], version["blake3"]), (0, EMPTY_HASH))
            for connection in uploads:
                connection.send(body)
                response = connection.getresponse()
                self.assertEqual(response.status, 201, response.read())

    def test_container_version_and_recycle_metadata_are_flat_files(self):
        name = ".zxs.tmp"
        contents = b"a real file whose name resembles a temporary file"
        commit = self.create("/disk1/docs", name, contents)
        cid = commit["container"]["container_id"]
        container = self.zxs.path(commit["container"])
        version = container / "v000001"
        self.assertEqual({entry.name for entry in container.iterdir()}, {".zxs", "v000001"})
        metadata = json.loads((container / ".zxs").read_bytes())
        self.assertEqual((metadata["container_id"], metadata["first_retained_version"],
                          metadata["latest_version"]), (cid, 1, 1))
        self.assertEqual({entry.name for entry in version.iterdir()}, {".zxs", name})
        self.assertEqual(json.loads((version / ".zxs").read_bytes()), commit["version"])
        self.assertEqual((version / name).read_bytes(), contents)

        updated = self.zxs.http("PATCH", f"/containers/{cid}", 200, {"tags": ["kept"]})[0]
        expected = metadata | {"tags": ["kept"], "updated_at": updated["updated_at"]}
        self.assertEqual(json.loads((container / ".zxs").read_bytes()), expected)

        entry = self.zxs.http("DELETE", f"/containers/{cid}", 200)[0]
        batch = self.zxs.archived(entry)
        self.assertEqual({item.name for item in batch.iterdir()}, {".zxs", name})
        self.assertEqual(json.loads((batch / ".zxs").read_bytes()),
                         entry["recycle"] | {"original_name": name})
        self.assertNotIn("folder", entry["container"])
        self.assertEqual((batch / name / "v000001" / name).read_bytes(), contents)

    def test_generated_config_and_recycle_manifest_reject_unknown_fields(self):
        t = self.zxs
        cid = self.create("/disk1", "note.txt", b"archived")["container"]["container_id"]
        entry = t.http("DELETE", f"/containers/{cid}", 200)[0]
        t.stop()
        config = t.data / ".zxs/config.toml"
        manifest = t.archived(entry) / ".zxs"
        metadata = json.loads(manifest.read_bytes())
        self.assertIn("Verified 1 versions", t.offline("verify"))
        for path, content, unknown in (
            (config, b'listen_addr = "0.0.0.0:7739"\n' + config.read_bytes(), "listen_addr"),
            (manifest, json.dumps(metadata | {"original_folder": "/disk1"}).encode(), "original_folder"),
        ):
            with self.subTest(field=unknown):
                saved = path.read_bytes()
                path.write_bytes(content)
                result = subprocess.run(
                    [SERVER, "--data-dir", t.data, "verify"], env=t.env,
                    capture_output=True, text=True, timeout=20,
                )
                self.assertEqual(result.returncode, 1, (result.stdout, result.stderr))
                self.assertIn("unknown field", result.stderr)
                self.assertIn(unknown, result.stderr)
                path.write_bytes(saved)
        self.assertIn("Verified 1 versions", t.offline("verify"))

    def test_equivalent_patch_leaves_metadata_and_timestamp_unchanged(self):
        t = self.zxs
        first = self.create("/disk1/Docs", "note.txt", b"original", tags=("alpha", "beta"))
        container = first["container"]
        path = f"/containers/{container['container_id']}"
        metadata = t.path(container) / ".zxs"
        before = metadata.read_bytes(), metadata.stat().st_ino
        self.wait_for(lambda: int(time()) > container["updated_at"], "timestamp did not advance")
        for patch in ({}, {"folder": container["folder"], "name": container["name"], "tags": ["beta", "alpha", "alpha"]}):
            updated, _ = t.http("PATCH", path, 200, patch)
            self.assertEqual(updated, container)
            self.assertEqual((metadata.read_bytes(), metadata.stat().st_ino), before)
        renamed, _ = t.http("PATCH", path, 200, {"name": "NOTE.txt"})
        self.assertEqual(renamed["name"], "NOTE.txt")
        self.assertGreater(renamed["updated_at"], container["updated_at"])
        self.assertEqual(self.detail(container["container_id"])["versions"], [first["version"]])

    def test_large_container_metadata_can_be_renamed_recycled_and_restored(self):
        t = self.zxs
        contents = b"original payload"
        commit = self.create("/disk1", "note.txt", contents)
        cid = commit["container"]["container_id"]
        path = f"/containers/{cid}"
        limit = 1024 * 1024
        oversized, _ = t.http("PATCH", path, 400, {"tags": ["x" * (limit - 80)]})
        self.assertTrue(oversized["message"].startswith("metadata exceeds"))
        tags = [f"tag-{number:05d}-" + "x" * 96 for number in range(6000)]
        updated, _ = t.http("PATCH", path, 200, {"tags": tags})
        self.assertLess((t.path(updated) / ".zxs").stat().st_size, limit)
        renamed, _ = t.http("PATCH", path, 200, {"name": "renamed.txt"})
        self.assertEqual(renamed["tags"], tags)
        archived, _ = t.http("DELETE", path, 200)
        rid = archived["recycle"]["recycle_id"]
        restored, _ = t.http("POST", f"/recycle/{rid}/restore", 200, {"folder": "/disk2"})
        self.assertEqual((restored["name"], restored["tags"]), ("renamed.txt", tags))
        self.assertEqual(t.http("GET", path + "/versions/1/payload", 200)[0], contents)

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

    def test_append_and_rebuild_preserve_tags_and_reject_folder_aliases(self):
        t = self.zxs
        folder = "/disk1/Docs/Projects"
        first = self.create(folder, "note.txt", b"first", tags=("kept",))
        cid = first["container"]["container_id"]
        latest = self.append(cid, 1, b"second")
        self.assertEqual(self.listing(folder=folder, tag="kept")["containers"], [latest["container"]])

        t.stop()
        alias = t.path(first["container"]).with_name("NOTE.TXT")
        alias.mkdir()
        t.local(SERVER, ["--data-dir", t.data, "rebuild-index"], expected=1)
        alias.rmdir()
        self.assertIn("Index rebuilt", t.offline("rebuild-index"))
        t.start()
        t.login_session()
        self.assertEqual(self.listing(folder=folder, tag="kept")["containers"], [latest["container"]])
        self.assertEqual(self.detail(cid)["versions"], [first["version"], latest["version"]])

    def test_container_conflicts_report_the_stored_path(self):
        t = self.zxs
        folder = "/disk1/Docs"
        name = "Résumé.TXT"
        alias = "résumé.txt"
        original = self.create(folder, name, b"original")
        conflict, _ = t.http("POST", "/containers", 409, b"another", upload_headers(
            {"folder": folder, "name": alias, "tags": []}))
        occupant = f"the path is occupied by a container: {folder}/{name}"
        self.assertEqual(conflict, {"code": "conflict", "message": occupant})
        conflict, _ = t.http("POST", "/folders", 409, {"folder": f"{folder}/{alias}"})
        self.assertEqual(conflict["message"], occupant)
        conflict, _ = t.http("POST", "/folders", 409, {"folder": f"{folder}/{alias}/Child"})
        self.assertEqual(conflict["message"], f"a container occupies ancestor {folder}/{name}")
        cid = original["container"]["container_id"]
        self.assertEqual(self.detail(cid)["versions"], [original["version"]])
        self.assertEqual(t.http("GET", f"/containers/{cid}/versions/1/payload", 200)[0], b"original")

    def test_literal_search_cursor_survives_recycling_its_last_result(self):
        t = self.zxs
        names = ("01%_report.txt", "02%_report.txt", "03%_report.txt")
        matching = [self.create("/disk1", name)["container"] for name in names]
        self.create("/disk1", "04ABreport.txt")
        self.create("/disk2", "05%_report.txt")
        query = {"q": "%_", "folder": "/disk1", "limit": 1}
        first = self.listing(**query)
        self.assertEqual(first["containers"], matching[:1])
        self.assertTrue(first["next_cursor"])
        cid = matching[0]["container_id"]
        t.http("DELETE", f"/containers/{cid}?expected_version=1", 200)
        second = self.listing(**query, cursor=first["next_cursor"])
        self.assertEqual(second["containers"], matching[1:2])
        third = self.listing(**query, cursor=second["next_cursor"])
        self.assertEqual(third["containers"], matching[2:])
        self.assertIsNone(third["next_cursor"])

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

    def test_shutdown_releases_storage_after_canceling_paused_transfers(self):
        t = self.zxs
        block = 4 * 1024 * 1024
        contents = b"download" * (3 * block // len(b"download"))
        first = self.create("/disk1/docs", "video.bin", contents)
        cid = first["container"]["container_id"]
        path = f"/containers/{cid}/versions"
        stage = t.data / "disk1/.zxs/staging"
        headers = {"Authorization": f"Bearer {t.token}"}

        with context_connection(t.port) as download, context_connection(t.port) as upload:
            download.connect()
            download.sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
            download.request("GET", path + "/1/payload", headers=headers)
            response = download.getresponse()
            self.assertEqual(response.status, 200)
            self.assertEqual(response.read(4096), contents[:4096])

            upload.putrequest("POST", path)
            for name, value in (upload_headers({"expected_version": 1}) | headers).items():
                upload.putheader(name, value)
            upload.putheader("Content-Length", str(2 * block))
            upload.endheaders(b"x" * block)
            self.wait_for(lambda: any(file.stat().st_size for file in stage.glob("*/version/video.bin")),
                          "incomplete upload did not write to staging")

            t.process.terminate()
            self.assertEqual(t.process.wait(timeout=5), 0)

        self.assertEqual(list(stage.iterdir()), [])
        self.assertIn("Verified 1 versions", t.offline("verify"))
        container = t.path(first["container"])
        self.assertEqual((container / "v000001/video.bin").read_bytes(), contents)
        self.assertFalse((container / "v000002").exists())

    def test_rename_during_append_rejects_the_old_upload_without_leaving_a_version(self):
        t = self.zxs
        first = self.create("/disk1", "before.txt", b"first version")
        cid = first["container"]["container_id"]
        path = f"/containers/{cid}"
        stage = t.data / "disk1/.zxs/staging"
        contents = b"upload started before rename"
        headers = upload_headers({"expected_version": 1}) | {
            "Content-Length": str(len(contents)), "Authorization": f"Bearer {t.token}",
        }
        with context_connection(t.port) as connection:
            connection.putrequest("POST", path + "/versions")
            for name, value in headers.items():
                connection.putheader(name, value)
            connection.endheaders(contents[:5])
            self.wait_for(lambda: any(stage.iterdir()), "upload did not reach staging")
            renamed, _ = t.http("PATCH", path, 200, {"name": "after.txt"})
            connection.send(contents[5:])
            response = connection.getresponse()
            self.assertEqual(response.status, 409, response.read())
        self.assertEqual(self.detail(cid), {"container": renamed, "versions": [first["version"]]})
        self.assertEqual(t.http("GET", path + "/versions/1/payload", 200)[0], b"first version")
        self.assertFalse(t.path(first["container"]).exists())
        self.assertFalse((t.path(renamed) / "v000002").exists())
        self.assertEqual(list(stage.iterdir()), [])
        retry = self.append(cid, 1, contents)
        self.assertEqual((retry["version"]["version"], retry["version"]["name_at_commit"]), (2, "after.txt"))
        self.assertEqual(t.http("GET", path + "/versions/2/payload", 200)[0], contents)

    def test_open_range_read_keeps_its_version_during_cross_disk_move(self):
        t = self.zxs
        contents = bytes(range(256)) * (8 * 1024 * 1024 // 256)
        first = self.create("/disk1", "video.mp4", contents)
        cid = first["container"]["container_id"]
        path = f"/containers/{cid}"
        start = 137
        with context_connection(t.port) as connection:
            headers = {"Authorization": f"Bearer {t.token}", "Range": f"bytes={start}-"}
            connection.request("GET", path + "/versions/1/payload", headers=headers)
            response = connection.getresponse()
            self.assertEqual(response.status, 206)
            self.assertEqual(response.getheader("ETag"), f'"{first["version"]["blake3"]}"')
            self.assertEqual(response.getheader("Content-Range"), f"bytes {start}-{len(contents)-1}/{len(contents)}")
            prefix = response.read(4096)
            moved, _ = t.http("PATCH", path, 200, {"folder": "/disk2/归档", "name": "renamed.mp4"})
            next_version = self.append(cid, 1, b"replacement bytes")["version"]
            self.assertEqual(prefix + response.read(), contents[start:])
        self.assertFalse(t.path(first["container"]).exists())
        self.assertEqual((t.path(moved) / "v000001/video.mp4").read_bytes(), contents)
        found, reply = t.http("GET", path + "/versions/2/payload", 200)
        self.assertEqual(found, b"replacement bytes")
        self.assertEqual(reply["ETag"], f'"{next_version["blake3"]}"')

    def test_unchanged_folder_move_keeps_index_and_spelling(self):
        t = self.zxs
        source = "/disk1/Photos"
        container = self.create(source + "/Album", "image.jpg", b"original")["container"]
        with sqlite3.connect(t.data / ".zxs/index.sqlite") as index:
            version = index.execute("PRAGMA data_version").fetchone()
            self.assertEqual(t.http("PATCH", "/folders", 200, {"from": source, "to": source})[0],
                             {"folder": source})
            self.assertEqual(index.execute("PRAGMA data_version").fetchone(), version)
            missing = source + "/missing"
            t.http("PATCH", "/folders", 404, {"from": missing, "to": missing})
            target = "/disk1/photos"
            t.http("PATCH", "/folders", 409, {"from": target, "to": target})
            self.assertEqual(index.execute("PRAGMA data_version").fetchone(), version)
        t.http("PATCH", "/folders", 200, {"from": source, "to": target})
        self.assertEqual(self.detail(container["container_id"])["container"]["folder"], target + "/Album")
        self.assertEqual((t.data / "disk1/photos/Album/image.jpg/v000001/image.jpg").read_bytes(), b"original")

    def test_folder_move_respects_the_descendant_path_byte_limit(self):
        t = self.zxs
        source = "/disk1/Original"
        tail = "/" + "/".join(letter * 250 for letter in "abc")
        name = "é" * 125 + "s.txt"
        self.assertEqual(len((source + tail + "/" + name).encode()), 1024)
        container = self.create(source + tail, name, b"protected bytes")["container"]
        cid = container["container_id"]
        longer = "/disk1/Originaö"
        rejected, _ = t.http("PATCH", "/folders", 409, {"from": source, "to": longer})
        self.assertIn("1024 bytes", rejected["message"])
        self.assertEqual(self.detail(cid)["container"], container)
        current = source
        for target in ("/disk1/original", "/disk1/short", source):
            t.http("PATCH", "/folders", 200, {"from": current, "to": target})
            self.assertEqual(self.detail(cid)["container"]["folder"], target + tail)
            current = target
        self.assertEqual(t.http("GET", f"/containers/{cid}/versions/1/payload", 200)[0], b"protected bytes")

    def test_invalid_folder_requests_do_not_freeze_an_active_upload(self):
        t = self.zxs
        source = "/disk1/Uploads"
        occupied = "/disk2/Occupied"
        first = self.create(source, "clip.mp4", b"original")
        cid = first["container"]["container_id"]
        path = f"/containers/{cid}"
        t.http("POST", "/folders", 201, {"folder": occupied})
        stage = t.data / "disk1/.zxs/staging"
        body = b"ongoing upload"
        headers = upload_headers({"expected_version": 1}) | {
            "Content-Length": str(len(body)), "Authorization": f"Bearer {t.token}",
        }
        with context_connection(t.port) as upload:
            upload.putrequest("POST", path + "/versions")
            for name, value in headers.items():
                upload.putheader(name, value)
            upload.endheaders(body[:5])
            self.wait_for(lambda: any(stage.glob("*/version/clip.mp4")), "upload did not reach staging")
            conflict, _ = t.http("PATCH", "/folders", 409, {"from": source, "to": occupied})
            self.assertEqual(conflict["message"], f"folder already exists as {occupied}")
            alias = source.lower()
            conflict, _ = t.http("PATCH", "/folders", 409, {"from": alias, "to": "/disk2/Unused"})
            self.assertEqual(conflict["message"], f"folder already exists as {source}")
            conflict, _ = t.http("DELETE", f"/folders?folder={alias}&recursive=true", 409)
            self.assertEqual(conflict["message"], f"folder already exists as {source}")
            t.http("POST", "/folders", 201, {"folder": source + "/New"})
            upload.send(body[5:])
            response = upload.getresponse()
            self.assertEqual(response.status, 201, response.read())
        self.assertEqual(t.http("GET", path + "/versions/2/payload", 200)[0], body)
        self.assertEqual(self.detail(cid)["container"]["folder"], source)
        self.assertEqual(t.http("GET", "/recycle", 200)[0]["entries"], [])

    def test_folder_move_conflict_leaves_both_trees_and_can_be_retried(self):
        t = self.zxs
        original = self.create("/disk1/Source/Nested", "note.txt", b"source")["container"]
        occupant = self.create("/disk2/Target", "note.txt", b"destination")["container"]
        t.http("POST", "/folders", 201, {"folder": "/disk1/Source/Empty/Child"})
        move = {"from": "/disk1/Source", "to": "/disk2/Target"}
        t.http("PATCH", "/folders", 409, move)
        self.assertEqual(self.detail(original["container_id"])["container"], original)
        self.assertEqual(self.detail(occupant["container_id"])["container"], occupant)
        self.assertTrue((t.data / "disk1/Source/Empty/Child").is_dir())
        self.assertEqual((t.path(original) / "v000001/note.txt").read_bytes(), b"source")
        self.assertEqual((t.path(occupant) / "v000001/note.txt").read_bytes(), b"destination")
        self.assertEqual(list((t.data / "disk2/.zxs/staging").iterdir()), [])
        t.http("PATCH", "/folders", 200, move | {"to": "/disk2/Moved"})
        self.assertFalse((t.data / "disk1/Source").exists())
        self.assertTrue((t.data / "disk2/Moved/Empty/Child").is_dir())
        moved = self.detail(original["container_id"])["container"]
        self.assertEqual((moved["folder"], moved["latest_blake3"]),
                         ("/disk2/Moved/Nested", original["latest_blake3"]))
        self.assertEqual((t.path(moved) / "v000001/note.txt").read_bytes(), b"source")
        self.assertEqual((t.path(occupant) / "v000001/note.txt").read_bytes(), b"destination")

    def test_directory_operations_preserve_a_tree_with_an_unindexed_container(self):
        t = self.zxs
        folder = "/disk1/Source"
        indexed = self.create(folder + "/Nested", "note.txt", b"saved original")["container"]
        extra = t.data / "disk1/Source/from-backup.txt"
        copytree(t.path(indexed), extra)
        meta_path = extra / ".zxs"
        metadata = json.loads(meta_path.read_bytes())
        metadata["container_id"] = "f" * 32
        meta_path.write_text(json.dumps(metadata), encoding="utf-8")

        move = {"from": folder, "to": "/disk2/Moved"}
        moved, _ = t.http("PATCH", "/folders", 409, move)
        recycled, _ = t.http("DELETE", f"/folders?folder={folder}&recursive=true", 409)
        for error in (moved, recycled):
            self.assertIn("container missing from index", error["message"])
            self.assertIn(extra.name, error["message"])
        self.assertEqual((extra / "v000001/note.txt").read_bytes(), b"saved original")
        self.assertEqual((t.path(indexed) / "v000001/note.txt").read_bytes(), b"saved original")
        self.assertFalse((t.data / "disk2/Moved").exists())
        self.assertEqual(t.http("GET", "/recycle", 200)[0]["entries"], [])

    def test_restore_conflict_preserves_archive_and_renamed_restore_keeps_history(self):
        t = self.zxs
        first = self.create("/disk1/Docs", "Café.txt", b"original bytes", tags=("draft",))
        cid = first["container"]["container_id"]
        second = self.append(cid, 1, b"updated bytes")["version"]
        stale, _ = t.http("DELETE", f"/containers/{cid}?expected_version=1", 409)
        self.assertEqual(stale, {"code": "conflict", "message": "expected version 1, latest is 2"})
        self.assertEqual(self.detail(cid)["versions"], [first["version"], second])
        self.assertEqual(t.http("GET", "/recycle", 200)[0]["entries"], [])
        archived = t.http("DELETE", f"/containers/{cid}?expected_version=2", 200)[0]
        t.http("DELETE", f"/containers/{cid}?expected_version=2", 404)
        archive = t.archived(archived)
        restore = f"/recycle/{archived['recycle']['recycle_id']}/restore"
        occupant = self.create("/disk1/Docs", "CAFÉ.TXT", b"other file")["container"]
        t.http("POST", restore, 400, {})
        t.http("POST", restore, 409, {"folder": "/disk1/Docs"})
        t.http("GET", f"/containers/{cid}", 404)
        self.assertEqual(t.http("GET", "/recycle", 200)[0]["entries"], [archived])
        self.assertEqual((archive / "Café.txt/v000002/Café.txt").read_bytes(), b"updated bytes")
        self.assertEqual((t.path(occupant) / "v000001/CAFÉ.TXT").read_bytes(), b"other file")
        restored, _ = t.http("POST", restore, 200, {"folder": "/disk2/Restored/Docs", "name": "recovered.txt"})
        self.assertEqual(restored["container_id"], cid)
        self.assertEqual(restored["tags"], ["draft"])
        self.assertEqual(self.detail(cid)["versions"], [first["version"], second])
        for version, contents in enumerate((b"original bytes", b"updated bytes"), 1):
            found, headers = t.http("GET", f"/containers/{cid}/versions/{version}/payload", 200)
            self.assertEqual(found, contents)
            self.assertTrue(unquote(headers["Content-Disposition"]).endswith("Café.txt"))
        self.assertFalse(archive.exists())
        self.assertEqual(t.http("GET", "/recycle", 200)[0]["entries"], [])
        self.assertEqual(self.append(cid, 2, b"after restore")["version"]["version"], 3)

    def test_restore_reports_that_the_archive_was_purged_during_preparation(self):
        import fcntl
        import os
        import signal
        from threading import Event

        t = self.zxs
        first = self.create("/disk1", "report.txt", b"archived content")
        cid = first["container"]["container_id"]
        archived, _ = t.http("DELETE", f"/containers/{cid}", 200)
        item = f"/recycle/{archived['recycle']['recycle_id']}"
        source = t.archived(archived) / "report.txt/.zxs"
        ready = Event()
        handler = signal.signal(signal.SIGIO, lambda *_: ready.set())
        try:
            with ThreadPoolExecutor(max_workers=1) as pool, source.open("rb") as metadata:
                # Pause restore after its index lookup, when it opens the archived metadata.
                fcntl.fcntl(metadata, fcntl.F_SETOWN, os.getpid())
                try:
                    fcntl.fcntl(metadata, fcntl.F_SETLEASE, fcntl.F_WRLCK)
                except OSError as error:
                    self.skipTest(f"test filesystem does not support file leases: {error}")
                try:
                    restore = pool.submit(t.http, "POST", item + "/restore", 404, {"folder": "/disk2"})
                    self.assertTrue(ready.wait(timeout=5), "restore did not open the archive")
                    t.http("DELETE", item, 200)
                finally:
                    fcntl.fcntl(metadata, fcntl.F_SETLEASE, fcntl.F_UNLCK)
                self.assertEqual(restore.result(timeout=5)[0]["code"], "not_found")
        finally:
            signal.signal(signal.SIGIO, handler)
        self.assertFalse((t.data / "disk2/report.txt").exists())
        self.assertEqual(list((t.data / "disk2/.zxs/staging").iterdir()), [])

    def test_restart_indexes_only_online_disks_and_restore_uses_the_selected_folder(self):
        t = self.zxs
        offline = self.create("/disk1/Offline", "work.txt", b"keep the only copy")["container"]
        t.cli(t.a, "login", t.endpoint)
        t.cli(t.a, "checkout", offline["container_id"])
        local_path = "disk1/Offline/work.txt"
        local = t.a / local_path
        local.write_bytes(b"unsubmitted edit")
        cid = self.create("/disk1/Archived", "history.txt", b"restore this")["container"]["container_id"]
        entry = t.http("DELETE", f"/containers/{cid}", 200)[0]
        restore = f"/recycle/{entry['recycle']['recycle_id']}/restore"
        t.stop()
        saved = t.data / "disk1"
        detached = t.root / "unmounted"
        saved.rename(detached)
        saved.mkdir()
        t.start()
        t.login_session()
        self.assertEqual(t.http("GET", "/folders?folder=/", 200)[0]["folders"], [{"folder": "/disk2"}])
        t.http("GET", f"/containers/{offline['container_id']}", 404)
        self.assertEqual(t.cli(t.a, "sync")["remote_missing"], [local_path])
        self.assertEqual(local.read_bytes(), b"unsubmitted edit")
        t.http("POST", restore, 400, {"folder": "/disk1/Archived"})
        restored = t.http("POST", restore, 200, {"folder": "/disk2/Restored"})[0]
        self.assertEqual((restored["folder"], restored["name"]), ("/disk2/Restored", "history.txt"))
        self.assertEqual((t.path(restored) / "v000001/history.txt").read_bytes(), b"restore this")
        self.assertEqual(list(saved.iterdir()), [])
        with sqlite3.connect(t.data / ".zxs/index.sqlite") as index:
            self.assertEqual(index.execute("SELECT id FROM containers").fetchall(), [(cid,)])

        t.stop()
        saved.rmdir()
        detached.rename(saved)
        t.start()
        t.login_session()
        self.assertEqual(self.detail(offline["container_id"])["container"], offline)
        self.assertEqual((t.path(offline) / "v000001/work.txt").read_bytes(), b"keep the only copy")
        self.assertEqual(t.cli(t.a, "sync")["remote_missing"], [])
        self.assertEqual(local.read_bytes(), b"unsubmitted edit")

    def test_recycle_reports_offline_disks_and_preserves_empty_folder_deletion(self):
        t = self.zxs
        commit = self.create("/disk1/Docs", "note.txt", b"original")
        cid = commit["container"]["container_id"]
        path = f"/containers/{cid}?expected_version=1"
        t.http("POST", "/folders", 201, {"folder": "/disk1/Empty/Nested"})
        t.stop()
        offline = t.root / "recycle-offline"
        t.recycle.rename(offline)
        t.start()
        t.login_session()
        expected = {"code": "recycle_storage_offline", "message": "no recycle disk is online"}
        self.assertEqual(t.http("DELETE", path, 507)[0], expected)
        self.assertEqual(t.http("DELETE", "/folders?folder=/disk1/Docs&recursive=true", 507)[0], expected)
        self.assertEqual(self.detail(cid)["versions"], [commit["version"]])
        self.assertEqual(t.http("GET", f"/containers/{cid}/versions/1/payload", 200)[0], b"original")
        t.http("DELETE", "/folders?folder=/disk1/Empty&recursive=true", 200)
        self.assertFalse((t.data / "disk1/Empty").exists())
        self.assertEqual(t.http("GET", "/recycle", 200)[0]["entries"], [])
        t.stop()
        offline.rename(t.recycle)
        t.start()
        t.login_session()
        archived, _ = t.http("DELETE", path, 200)
        self.assertEqual(archived["container"]["container_id"], cid)
        self.assertEqual((t.archived(archived) / "note.txt/v000001/note.txt").read_bytes(), b"original")

    def test_purge_all_replay_keeps_untouched_archives_on_offline_disk(self):
        t = self.zxs
        entries = []
        for name in ("detached.txt", "kept.txt"):
            cid = self.create("/disk1", name, name.encode())["container"]["container_id"]
            entries.append(t.http("DELETE", f"/containers/{cid}", 200)[0])
        detached, retained = entries
        self.assertEqual(detached["recycle_disk"], retained["recycle_disk"])
        t.stop()

        tx = "c" * 32
        disk = t.recycle / detached["recycle_disk"]
        trash = disk / ".zxs/purging" / tx
        trash.mkdir()
        journal = t.data / ".zxs/inflight" / (tx + ".json")
        journal.write_text(json.dumps({"tx_id": tx, "change": {
            "action": "PurgeAll", "disks": ["rec1", "rec2"],
        }}))
        t.archived(detached).rename(trash / detached["recycle"]["recycle_id"])
        offline = t.root / "offline-recycle-disk"
        disk.rename(offline)

        self.assertIn("Index rebuilt", t.offline("rebuild-index"))
        self.assertFalse(journal.exists())
        self.assertTrue((offline / trash.relative_to(disk)).is_dir())
        self.assertTrue((offline / t.archived(retained).name).is_dir())

        offline.rename(disk)
        t.start()
        t.login_session()
        self.assertFalse(trash.exists())
        self.assertEqual(t.http("GET", "/recycle", 200)[0]["entries"], [retained])
        original = t.archived(retained) / "kept.txt/v000001/kept.txt"
        self.assertEqual(original.read_bytes(), b"kept.txt")

    def test_restart_discards_copy_without_a_journal(self):
        t = self.zxs
        original = self.create("/disk1/Source", "work.txt", b"still here")["container"]
        t.stop()
        tx = "a" * 32
        staging = t.data / "disk2/.zxs/staging" / tx / "tree"
        incoming = t.recycle / "rec1/.zxs/incoming" / tx
        plan = t.data / ".zxs/inflight" / (tx + ".items")
        for path in (staging, incoming, plan):
            path.mkdir(parents=True)
            (path / "partial").write_bytes(b"unfinished")
        t.start()
        t.login_session()
        self.assertEqual(self.listing()["containers"], [original])
        self.assertEqual((t.path(original) / "v000001/work.txt").read_bytes(), b"still here")
        self.assertFalse(any(path.exists() for path in (staging, incoming, plan)))

    def test_restart_ends_a_published_move_with_the_source_disk_offline(self):
        t = self.zxs
        original = self.create("/disk1/Source", "work.txt", b"same contents on both disks")["container"]
        retained = self.create("/disk1/Saved", "untouched.txt", b"keep this file")["container"]
        t.stop()
        tx = "b" * 32
        source = t.path(original).parent
        detached = t.data / "disk1/.zxs/purging" / tx
        target = t.data / "disk2/Target"
        staged = t.data / "disk2/.zxs/staging" / tx / "tree"
        copytree(source, staged)
        journal = t.data / ".zxs/inflight" / (tx + ".json")
        journal.write_text(json.dumps({"tx_id": tx, "change": {
            "action": "MoveFolder", "from": "/disk1/Source", "to": "/disk2/Target", "staged": True,
        }}))
        source.rename(detached)
        target_disk = target.parent
        offline_target = t.root / "offline-target"
        target_disk.rename(offline_target)
        t.local(SERVER, ["--data-dir", t.data, "rebuild-index"], expected=1)
        self.assertTrue(journal.is_file())
        self.assertEqual((detached / "work.txt/v000001/work.txt").read_bytes(), b"same contents on both disks")
        offline_target.rename(target_disk)

        staged.rename(target)
        disk = t.data / "disk1"
        saved = t.root / "unmounted"
        disk.rename(saved)
        disk.mkdir()
        t.start()
        t.login_session()
        self.assertFalse(journal.exists())
        self.assertEqual(list(disk.iterdir()), [])
        expected = original | {"folder": "/disk2/Target"}
        self.assertEqual(self.listing()["containers"], [expected])
        self.assertEqual((saved / detached.relative_to(disk) / "work.txt/v000001/work.txt").read_bytes(),
                         b"same contents on both disks")
        t.stop()
        disk.rmdir()
        saved.rename(disk)
        t.start()
        t.login_session()
        self.assertEqual(self.listing()["containers"], [retained, expected])
        self.assertFalse(detached.exists())
        self.assertEqual((target / "work.txt/v000001/work.txt").read_bytes(), b"same contents on both disks")
        self.assertEqual((t.path(retained) / "v000001/untouched.txt").read_bytes(), b"keep this file")

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

    def test_invalid_prune_arguments_leave_index_unopened(self):
        t = self.zxs
        first = self.create("/disk1/docs", "history.txt", b"first")
        cid = first["container"]["container_id"]
        self.append(cid, 1, b"second")
        t.stop()
        index = t.data / ".zxs/index.sqlite"
        index.rename(t.root / "previous-index.sqlite")

        for arguments, error in (
            (["--keep-last", "0"], "keep-last must be at least 1"),
            (["--keep-last", "1", "--container", "invalid"], "invalid container ID"),
            (["--keep-last", "0", "--container", "invalid"], "invalid container ID"),
        ):
            with self.subTest(arguments=arguments):
                result = subprocess.run(
                    [SERVER, "--data-dir", t.data, "prune", *arguments],
                    env=t.env, text=True, capture_output=True, timeout=10,
                )
                self.assertEqual((result.returncode, result.stderr), (1, f"zxs: {error}\n"))
                self.assertFalse(index.exists())

        self.assertIn("Pruned 1 versions", t.offline("prune", "--keep-last", 1, "--container", cid))
        self.assertIn("Verified 1 versions", t.offline("verify"))

    def test_prune_all_uses_indexed_version_counts_and_keeps_newest(self):
        t = self.zxs
        first = self.create("/disk1/docs", "history.txt", b"first")
        cid = first["container"]["container_id"]
        latest = self.append(cid, 1, b"second")
        other = self.create("/disk2/docs", "single.txt", b"other")

        t.stop()
        only_version = other["container"]["container_id"]
        self.assertIn("Pruned 0 versions", t.offline("prune", "--keep-last", 1, "--container", only_version))
        t.local(SERVER, ["--data-dir", t.data, "prune", "--keep-last", 1, "--container", "0" * 32], expected=1)
        self.assertIn("Verified 3 versions", t.offline("verify"))
        self.assertIn("Pruned 0 versions", t.offline("prune", "--keep-last", str((1 << 64) - 1)))
        self.assertIn("Pruned 1 versions", t.offline("prune", "--keep-last", 1))
        self.assertIn("Verified 2 versions", t.offline("verify"))
        t.start()
        t.login_session()
        self.assertEqual(self.detail(cid)["versions"], [latest["version"]])
        self.assertEqual(self.detail(other["container"]["container_id"])["versions"], [other["version"]])

    def test_verify_treats_an_uninitialized_index_as_missing(self):
        t = self.zxs
        first = self.create("/disk1/docs", "first.txt", b"first")["container"]
        self.create("/disk2/docs", "second.txt", b"second")
        t.stop()

        index = t.data / ".zxs/index.sqlite"
        index.rename(t.root / "old-index.sqlite")
        self.assertIn("Verified 2 versions", t.offline("verify"))
        index.write_bytes(b"not a SQLite index")
        t.local(SERVER, ["--data-dir", t.data, "verify"], expected=1)
        index.unlink()
        with closing(sqlite3.connect(index)) as database:
            self.assertEqual(database.execute("PRAGMA journal_mode=WAL").fetchone(), ("wal",))
            self.assertEqual(database.execute("SELECT name FROM sqlite_schema").fetchall(), [])
        empty_index = index.read_bytes()
        self.assertIn("Verified 2 versions", t.offline("verify"))
        self.assertEqual(index.read_bytes(), empty_index)
        self.assertIn("Index rebuilt", t.offline("rebuild-index"))

        t.path(first).rename(t.root / "missing-container")
        t.local(SERVER, ["--data-dir", t.data, "verify"], expected=1)

    def test_rebuild_keeps_the_previous_index_when_online_history_moves_on_disk(self):
        t = self.zxs
        live = self.create("/disk1/folder", "Note.txt", b"live")["container"]
        discarded = self.create("/disk2", "Discarded.txt", b"archived")["container"]
        entry = t.http("DELETE", f"/containers/{discarded['container_id']}", 200)[0]
        t.stop()

        container = t.path(live)
        archive = t.archived(entry)
        other_recycle_disk = "rec2" if entry["recycle_disk"] == "rec1" else "rec1"
        index = t.data / ".zxs/index.sqlite"
        previous = index.read_bytes()
        for source, target in (
            (container, container.with_name("note.txt")),
            (archive, t.recycle / other_recycle_disk / archive.name),
        ):
            with self.subTest(source=source.name):
                source.rename(target)
                t.local(SERVER, ["--data-dir", t.data, "rebuild-index"], expected=1)
                self.assertEqual(index.read_bytes(), previous)
                target.rename(source)

        duplicate = container.with_name("Other.txt")
        copytree(container, duplicate)
        t.local(SERVER, ["--data-dir", t.data, "rebuild-index"], expected=1)
        self.assertEqual(index.read_bytes(), previous)
        duplicate.rename(t.root / "Duplicate.txt")

        self.assertIn("Index rebuilt", t.offline("rebuild-index"))
        t.start()
        t.login_session()
        self.assertEqual(self.listing()["containers"], [live])
        self.assertEqual(t.http("GET", "/recycle", 200)[0]["entries"], [entry])

    def wait_for(self, available, problem):
        deadline = monotonic() + 5
        while monotonic() < deadline:
            if available():
                return
            sleep(.025)
        self.fail(problem)
