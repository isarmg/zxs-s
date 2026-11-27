"""HTTP contract and on-disk invariants exercised through the real service."""

from concurrent.futures import ThreadPoolExecutor
import json
from shutil import copytree
import sqlite3
from threading import Barrier
from time import monotonic, sleep
from urllib.parse import unquote

from behavior_support import ServerCase
from smoke import PASSWORD, SERVER, context_connection, upload_headers


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
        archived = t.http("DELETE", f"/containers/{cid}?expected_version=2", 200)[0]
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

    def wait_for(self, available, problem):
        deadline = monotonic() + 5
        while monotonic() < deadline:
            if available():
                return
            sleep(.025)
        self.fail(problem)
