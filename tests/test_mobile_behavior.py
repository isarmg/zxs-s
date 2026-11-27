"""The C ABI used by both mobile applications, with real server storage."""

from io import BytesIO
from pathlib import Path
from zipfile import ZipFile

from behavior_support import ServerCase
from smoke import PASSWORD, mobile_call, mobile_photo
from test_transfer_behavior import slow_payloads


class MobileBehavior(ServerCase):
    def setUp(self):
        super().setUp()
        self.call = mobile_call()
        self.connection = {"server": self.zxs.endpoint, "password": PASSWORD}
        self.addCleanup(self.call, "logout", connection=self.connection)

    def test_photo_batch_uses_the_selected_current_directory(self):
        t = self.zxs
        target = "/disk2/项目/素材"
        t.http("POST", "/folders", 201, {"folder": target})
        source = t.root / "已选原件"
        source.write_bytes(b"photo bytes")
        request = {"connection": self.connection, "file": str(source), "folder": target,
                   "name": "照片.jpg"}
        sent = []
        first = mobile_photo(self.call, progress=sent, **request)["commit"]["container"]
        self.assertEqual((first["folder"], first["name"]), (target, request["name"]))
        self.assertEqual(sent[-1][:3], (0, 11, 11))
        sent.clear()
        self.assertEqual(mobile_photo(self.call, progress=sent, **request),
                         {"outcome": "already_present", "current": first})
        self.assertEqual(sent, [])
        source.write_bytes(b"different photo")
        second = mobile_photo(self.call, **request)["commit"]["container"]
        self.assertEqual((second["folder"], second["name"]), (target, "照片 (2).jpg"))
        self.assertCountEqual(self.listing(folder=target)["containers"], [first, second])
        self.assertEqual(self.listing(folder="/disk1")["containers"], [])
        self.assertFalse((t.data / "disk1/相册").exists())
        self.assertFalse((t.data / "disk2/相册").exists())

    def test_version_preflight_and_cache_progress(self):
        t = self.zxs
        first = self.create("/disk1", "note.txt", b"v1")["container"]
        cid = first["container_id"]
        self.append(cid, 1, b"v2")
        source = t.root / "mobile-edit"
        source.write_bytes(b"uncommitted draft")
        request = {"connection": self.connection, "file": str(source),
                   "target": {"kind": "version", "container_id": cid, "expected_version": 1}}
        progress = []
        self.assertEqual(self.call("commit_file", progress=progress, **request)["outcome"], "conflict")
        self.assertEqual(progress, [])
        self.assertEqual(self.detail(cid)["container"]["latest_version"], 2)
        request["target"]["expected_version"] = 2
        result = self.call("commit_file", progress=progress, **request)
        self.assertEqual((result["outcome"], result["commit"]["version"]["version"]), ("committed", 3))
        self.assertEqual(progress[-1][:3], (0, len(source.read_bytes()), len(source.read_bytes())))
        progress.clear()
        args = {"connection": self.connection, "id": cid, "version": 3, "cache": str(t.root / "cache")}
        opened = self.call("open", progress=progress, **args)
        self.assertEqual(Path(opened["path"]).read_bytes(), source.read_bytes())
        self.assertEqual(progress[0][0], 1)
        progress.clear()
        self.assertEqual(self.call("open", progress=progress, **args), opened)
        self.assertEqual(progress, [])

    def test_empty_file_reports_zero_byte_transfers(self):
        t = self.zxs
        source = t.root / "empty.bin"
        source.touch()
        progress = []
        result = self.call("commit_file", connection=self.connection, file=str(source), progress=progress,
                           target={"kind": "new", "folder": "/disk1", "name": source.name, "tags": []})
        self.assertEqual(progress, [(0, 0, 0, 0)])
        progress.clear()
        opened = self.call("open", connection=self.connection, id=result["commit"]["container"]["container_id"],
                           cache=str(t.root / "cache"), progress=progress)
        self.assertEqual(progress, [(1, 0, 0, 0)])
        self.assertEqual(Path(opened["path"]).read_bytes(), b"")

    def test_cancel_download_during_transfer_and_retry(self):
        t = self.zxs
        first = self.create("/disk1", "document.bin", b"cached version")["container"]
        cid = first["container_id"]
        cache = t.root / "cache"
        request = {"connection": self.connection, "id": cid, "cache": str(cache)}
        cached = Path(self.call("open", version=1, **request)["path"])
        payload = bytes(range(256)) * (8 * 1024 * 1024 // 256)
        self.append(cid, 1, payload)
        progress = []
        with slow_payloads(t.port) as endpoint:
            limited = request | {"connection": self.connection | {"server": endpoint}}
            response = self.call("open", version=2, **limited, status=499, progress=progress,
                                 cancelled=lambda: bool(progress and progress[-1][1] > 0))
            self.assertEqual(response["error"], "下载已取消")
            self.assertEqual(progress[0], (1, 0, len(payload), 0))
            self.assertTrue(0 < progress[-1][1] < len(payload), progress)
            self.assertCountEqual(cache.iterdir(), [cache / ".lock", cached.parent])
            self.assertEqual(cached.read_bytes(), b"cached version")
            retried = self.call("open", version=2, **limited)
            self.assertEqual(Path(retried["path"]).read_bytes(), payload)

    def test_cancel_zip_refresh_preserves_open_version_for_offline_browsing(self):
        def bundle(name, contents):
            output = BytesIO()
            with ZipFile(output, "w") as archive:
                archive.writestr(".zxsignore", "")
                archive.writestr("notes/" + name, contents)
            return output.getvalue()

        t = self.zxs
        cid = self.create("/disk1", "work.zip", bundle("old.txt", b"cached notes"))["container"]["container_id"]
        cache = t.root / "cache"
        request = {"id": cid, "connection": self.connection, "cache": str(cache)}
        opened = self.call("open", **request)
        self.assertTrue(opened["bundle"])
        self.assertEqual(opened["version"], 1)
        before = set(cache.iterdir())
        updated = bundle("new.txt", b"updated notes")
        self.append(cid, 1, updated)

        progress = []
        self.call("browse_bundle", directory="notes", status=499, progress=progress,
                  cancelled=lambda: bool(progress), **request)
        self.assertEqual(progress, [(1, 0, len(updated), 0)])
        self.assertEqual(set(cache.iterdir()), before)

        t.stop()
        try:
            pinned = request | {"version": opened["version"]}
            page = self.call("browse_bundle", directory="notes", **pinned)
            self.assertEqual((page["version"], [entry["name"] for entry in page["entries"]]), (1, ["old.txt"]))
            member = self.call("open_bundle_member", path="notes/old.txt", **pinned)
            self.assertEqual(Path(member["path"]).read_bytes(), b"cached notes")
        finally:
            t.start()
            t.login_session()
        refreshed = self.call("browse_bundle", directory="notes", **request)
        self.assertEqual((refreshed["version"], [entry["name"] for entry in refreshed["entries"]]), (2, ["new.txt"]))
        member = self.call("open_bundle_member", path="notes/new.txt", version=2, **request)
        self.assertEqual(Path(member["path"]).read_bytes(), b"updated notes")

    def test_cancellation_at_last_progress_keeps_download_out_of_the_cache(self):
        t = self.zxs
        payload = b"one complete chunk"
        cid = self.create("/disk1", "note.txt", payload)["container"]["container_id"]
        cache = t.root / "cache"
        request = {"id": cid, "connection": self.connection, "cache": str(cache)}
        progress = []
        self.call("open", status=499, progress=progress, **request,
                  cancelled=lambda: bool(progress and progress[-1][1] == len(payload)))
        self.assertEqual([sample[:3] for sample in progress], [(1, 0, len(payload)), (1, len(payload), len(payload))])
        self.assertEqual(list(cache.iterdir()), [cache / ".lock"])
        retried = self.call("open", **request)
        self.assertEqual(Path(retried["path"]).read_bytes(), payload)

    def test_download_cancellation_callback_does_not_interrupt_upload(self):
        t = self.zxs
        source = t.root / "picked.txt"
        contents = b"ready to upload"
        source.write_bytes(contents)
        checks = []
        progress = []

        def cancel():
            checks.append(True)
            return True

        result = self.call("commit_file", connection=self.connection, file=str(source), progress=progress,
                           cancelled=cancel, target={"kind": "new", "folder": "/disk1", "name": source.name})
        self.assertEqual(result["outcome"], "committed")
        self.assertEqual(checks, [])
        self.assertEqual(progress[-1][:3], (0, len(contents), len(contents)))
        cid = result["commit"]["container"]["container_id"]
        self.assertEqual(t.http("GET", f"/containers/{cid}/versions/1/payload", 200)[0], contents)

    def test_photo_upload_uses_current_location_before_skipping_matching_bytes(self):
        t = self.zxs
        source = t.root / "selected.jpg"
        contents = b"photo in the selected folder"
        source.write_bytes(contents)
        target = {"kind": "photo", "name": source.name, "folder": "/disk1/Photos"}
        request = {"connection": self.connection, "file": str(source)}
        first = self.call("commit_file", **request, target=target)["commit"]["container"]
        cid = first["container_id"]
        moved, _ = t.http("PATCH", f"/containers/{cid}", 200, {"folder": "/disk2/Archive"})

        self.assertEqual(self.listing(folder=target["folder"])["containers"], [])
        retried = self.call("commit_file", **request, target=target)
        self.assertEqual(retried["outcome"], "committed")
        copy = retried["commit"]["container"]
        self.assertNotEqual(copy["container_id"], cid)
        self.assertEqual((copy["folder"], copy["name"], copy["latest_blake3"]),
                         (target["folder"], source.name, moved["latest_blake3"]))
        self.assertEqual(self.listing(folder="/disk2/Archive")["containers"], [moved])
        self.assertEqual(t.http("GET", f"/containers/{copy['container_id']}/versions/1/payload", 200)[0], contents)
        self.assertEqual(self.call("commit_file", **request, target=target),
                         {"outcome": "already_present", "current": copy})
