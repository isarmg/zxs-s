"""The C ABI used by both mobile applications, with real server storage."""

from base64 import b64decode
from io import BytesIO
import json
from pathlib import Path
from threading import Event
from time import monotonic
from zipfile import ZipFile

from behavior_support import ServerCase
from smoke import PASSWORD, mobile_call, mobile_photo, proxy_requests
from test_transfer_behavior import slow_payloads


PHOTO_PNG = b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC")


def workspace_zip(path, contents):
    output = BytesIO()
    with ZipFile(output, "w") as archive:
        archive.writestr(".zxsignore", "")
        archive.writestr(path, contents)
    return output.getvalue()


class MobileBehavior(ServerCase):
    def setUp(self):
        super().setUp()
        self.call = mobile_call()
        self.connection = {"server": self.zxs.endpoint, "password": PASSWORD}
        self.addCleanup(self.call, "logout", connection=self.connection)

    def test_logout_revokes_once_even_when_the_old_server_does_not_reply(self):
        t = self.zxs
        for stalled in [False, True]:
            with self.subTest(stalled=stalled):
                sessions, revocations = [], []
                release = Event()

                def trace(request, response, body):
                    if request.command == "POST" and request.path == "/sessions":
                        sessions.append(json.loads(body)["token"])
                    if request.command == "DELETE" and request.path == "/sessions/current":
                        revocations.append((request.headers["Authorization"], response.status))
                        if stalled:
                            release.wait(25)
                            return False
                    return True

                with proxy_requests(t.port, trace) as endpoint:
                    connection = self.connection | {"server": endpoint}
                    self.call("login", **connection)
                    headers = {"Authorization": f"Bearer {sessions[0]}"}
                    try:
                        if stalled:
                            start = monotonic()
                            response = self.call("logout", connection=connection, status=502)
                            self.assertIn("timeout", response["error"])
                            self.assertLess(monotonic() - start, 8)
                        else:
                            self.assertEqual(self.call("logout", connection=connection), {})
                    finally:
                        release.set()
                    self.assertEqual(self.call("logout", connection=connection), {})
                    self.assertEqual(revocations, [(headers["Authorization"], 204)])
                    t.http("GET", "/containers", 401, headers=headers, authenticated=False)
                    self.assertEqual(self.listing()["containers"], [])

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

    def test_photo_dedup_finds_numbered_unicode_names(self):
        t = self.zxs
        source = t.root / "selected"
        target = {"connection": self.connection, "file": str(source),
                  "folder": "/disk1/Photos", "name": "ΔΙΑΚΟΠΕΣ.jpg"}
        source.write_bytes(b"first photo")
        first = mobile_photo(self.call, **target)["commit"]["container"]
        source.write_bytes(b"another photo")
        numbered = mobile_photo(self.call, **target)["commit"]["container"]
        self.assertEqual(numbered["name"], "ΔΙΑΚΟΠΕΣ (2).jpg")
        t.http("PATCH", f"/containers/{first['container_id']}", 200, {"folder": "/disk2/Archive"})
        progress = []
        self.assertEqual(mobile_photo(self.call, progress=progress, **target),
                         {"outcome": "already_present", "current": numbered})
        self.assertEqual(progress, [])
        self.assertEqual(self.listing(folder=target["folder"])["containers"], [numbered])

    def test_explicit_photo_rename_reports_conflict_for_selected_name(self):
        t = self.zxs
        folder = "/disk1/Photos"
        source = t.root / "selected.png"
        contents = PHOTO_PNG
        source.write_bytes(contents)
        occupied = self.create(folder, "holiday.png", b"different source")["container"]
        numbered = self.create(folder, "holiday (2).png", contents)["container"]
        created = False

        def trace(request, _response, _body):
            nonlocal created
            if not created and request.command == "GET" and request.path.startswith("/folders?"):
                t.http("POST", "/folders", 201, {"folder": f"{folder}/{source.name}"})
                created = True
            return True

        with proxy_requests(t.port, trace) as endpoint:
            connection = self.connection | {"server": endpoint}
            request = {"connection": connection, "file": str(source)}
            target = {"folder": folder, "kind": "photo", "name": source.name}
            first = self.call("commit_file", **request, target=target)
            self.assertEqual(first["outcome"], "conflict")
            self.assertIn("message", first)

            manual = target | {"kind": "new", "name": "holiday.png"}
            self.assertEqual(self.call("commit_file", **request, target=manual),
                             {"outcome": "conflict", "current": occupied})
            saved = self.call("commit_file", **request, target=manual | {"name": "holiday-final.png"})
            self.call("logout", connection=connection)

        self.assertEqual(saved["outcome"], "committed")
        container = saved["commit"]["container"]
        self.assertEqual(container["name"], "holiday-final.png")
        self.assertCountEqual(self.listing(folder=folder)["containers"], [occupied, numbered, container])
        self.assertEqual(t.http("GET", f"/containers/{container['container_id']}/versions/1/payload", 200)[0], contents)
        self.assertEqual(source.read_bytes(), contents)

    def test_photo_with_invalid_source_name_can_be_saved_under_a_valid_name(self):
        t = self.zxs
        folder = "/disk1/Photos"
        source = t.root / "staged.png"
        source.write_bytes(PHOTO_PNG)
        t.http("POST", "/folders", 201, {"folder": f"{folder}/album.png"})
        uploads = []

        def trace(request, response, _):
            if request.command == "POST" and request.path == "/containers":
                uploads.append(response.status)
            return True

        with proxy_requests(t.port, trace) as endpoint:
            connection = self.connection | {"server": endpoint}
            request = {"connection": connection, "file": str(source)}
            target = {"kind": "photo", "folder": folder, "name": "aux.png"}
            invalid = self.call("commit_file", **request, target=target)
            self.assertEqual(invalid, {"outcome": "conflict", "message": "文件名无效，请改名另存。"})
            self.assertEqual(uploads, [])

            manual = target | {"kind": "new", "name": "aux-photo.png"}
            saved = self.call("commit_file", **request, target=manual)["commit"]["container"]
            numbered = self.call("commit_file", **request, target=target | {"name": "album.png"})["commit"]["container"]
            self.call("logout", connection=connection)

        self.assertEqual(uploads, [201, 201])
        self.assertEqual((saved["name"], numbered["name"]), ("aux-photo.png", "album (2).png"))
        for container in (saved, numbered):
            route = f"/containers/{container['container_id']}/versions/1/payload"
            self.assertEqual(t.http("GET", route, 200)[0], PHOTO_PNG)
        self.assertEqual(source.read_bytes(), PHOTO_PNG)

    def test_audio_originals_open_as_range_streams(self):
        t = self.zxs
        adts_silence = bytes.fromhex("fff16c40017ffc01182007") * 2
        for extension, payload in (("flac", b"fLaCsample"), ("aac", adts_silence)):
            with self.subTest(extension=extension):
                mime = f"audio/{extension}"
                cid = self.create("/disk1", f"recording.{extension}", payload)["container"]["container_id"]
                progress = []
                result = self.call("open", connection=self.connection, id=cid,
                                   cache=str(t.root / "cache"), progress=progress)
                route = f"/containers/{cid}/versions/1/payload"
                self.assertEqual(result["url"], t.endpoint + route)
                self.assertEqual(result["type"], mime)
                self.assertEqual(progress, [])
                part, headers = t.http("GET", route, 206, headers=result["headers"] | {"Range": "bytes=0-3"},
                                       authenticated=False)
                self.assertEqual((part, headers["Content-Type"]), (payload[:4], mime))

    def test_historical_audio_streams_after_latest_is_renamed_as_zip(self):
        t = self.zxs
        audio = bytes.fromhex("fff16c40017ffc01182007") * 2
        cid = self.create("/disk1", "recording.aac", audio)["container"]["container_id"]
        self.append(cid, 1, workspace_zip("recordings/notes.txt", b"album notes"))
        t.http("PATCH", f"/containers/{cid}", 200, {"name": "recordings.zip"})
        request = {"connection": self.connection, "id": cid, "cache": str(t.root / "cache")}

        latest = self.call("open", version=2, **request)
        self.assertTrue(latest["bundle"])
        self.assertIn("recordings", [entry["name"] for entry in latest["entries"]])

        progress = []
        historical = self.call("open", version=1, progress=progress, **request)
        route = f"/containers/{cid}/versions/1/payload"
        self.assertEqual(historical["url"], t.endpoint + route)
        self.assertEqual(historical["type"], "audio/aac")
        self.assertEqual(progress, [])
        part, headers = t.http("GET", route, 206,
                               headers=historical["headers"] | {"Range": "bytes=0-3"}, authenticated=False)
        self.assertEqual((part, headers["Content-Type"]), (audio[:4], "audio/aac"))

    def test_historical_zip_remains_browsable_after_newer_ordinary_version(self):
        t = self.zxs
        original = b"historical note"
        archive = workspace_zip("notes/member.txt", original)
        cid = self.create("/disk1", "work.zip", archive)["container"]["container_id"]
        request = {"connection": self.connection, "id": cid, "cache": str(t.root / "cache")}
        self.assertTrue(self.call("open", version=1, **request)["bundle"])
        self.call("browse", connection=self.connection, folder="/disk1")
        t.http("PATCH", f"/containers/{cid}", 200, {"name": "work.txt"})
        self.append(cid, 1, b"plain current note")

        progress = []
        historical = self.call("browse_bundle", directory="notes", version=1, progress=progress, **request)
        self.assertEqual([entry["name"] for entry in historical["entries"]], ["member.txt"])
        member = self.call("open_bundle_member", path="notes/member.txt", version=1, **request)
        self.assertEqual(Path(member["path"]).read_bytes(), original)
        self.assertTrue(self.call("open", version=1, progress=progress, **request)["bundle"])
        self.assertEqual(progress, [])

        latest = self.call("open", version=2, **request)
        self.assertFalse(latest["bundle"])
        self.assertEqual(Path(latest["path"]).read_bytes(), b"plain current note")

    def test_plain_zip_keeps_system_preview(self):
        output = BytesIO()
        with ZipFile(output, "w") as archive:
            archive.writestr("notes.txt", "not a workspace")
        original = output.getvalue()
        cid = self.create("/disk1", "plain.zip", original)["container"]["container_id"]
        opened = self.call("open", connection=self.connection, id=cid, version=1,
                           cache=str(self.zxs.root / "cache"))
        self.assertFalse(opened["bundle"])
        self.assertEqual(opened["type"], "application/zip")
        self.assertEqual(Path(opened["path"]).read_bytes(), original)

    def test_renamed_latest_zip_uses_the_new_preview_name_and_type(self):
        t = self.zxs
        output = BytesIO()
        with ZipFile(output, "w") as archive:
            archive.writestr("notes.txt", "ordinary ZIP")
        original = output.getvalue()
        for source, renamed in [("cached.bin", "cached.zip"), ("misnamed.aac", "misnamed.zip")]:
            with self.subTest(source=source):
                cid = self.create("/disk1", source, original)["container"]["container_id"]
                request = {"connection": self.connection, "id": cid, "version": 1,
                           "cache": str(t.root / "cache")}
                if source == "cached.bin":
                    self.assertEqual(Path(self.call("open", **request)["path"]).name, source)
                t.http("PATCH", f"/containers/{cid}", 200, {"name": renamed})

                progress = []
                opened = self.call("open", progress=progress, **request)
                path = Path(opened["path"])
                self.assertEqual((opened["bundle"], opened["type"], path.name),
                                 (False, "application/zip", renamed))
                self.assertEqual(path.read_bytes(), original)
                self.assertEqual(progress[-1][:3], (1, len(original), len(original)))
                progress.clear()
                self.assertEqual(self.call("open", progress=progress, **request), opened)
                self.assertEqual(progress, [])
                if source == "cached.bin":
                    self.append(cid, 1, original)
                    older = self.call("open", **request)
                    older_path = Path(older["path"])
                    self.assertEqual((older["type"], older_path.name), ("application/octet-stream", source))
                    self.assertEqual(older_path.read_bytes(), original)

    def test_invalid_conflict_rename_can_be_corrected_without_uploading(self):
        t = self.zxs
        original = self.create("/disk1", "note.txt", b"server original")["container"]
        source = t.root / "selected.txt"
        edited = b"mobile edit"
        source.write_bytes(edited)
        progress, uploads = [], []
        target = {"kind": "new", "folder": "/disk1", "name": "note.txt", "tags": ["mobile"]}
        request = {"connection": self.connection, "file": str(source), "progress": progress}
        self.assertEqual(self.call("commit_file", target=target, **request),
                         {"outcome": "conflict", "current": original})

        def trace(http_request, response, _):
            if http_request.command == "POST" and http_request.path == "/containers":
                uploads.append(response.status)
            return True

        with proxy_requests(t.port, trace) as endpoint:
            request["connection"] = self.connection | {"server": endpoint}
            invalid = self.call("commit_file", target=target | {"name": "note:2026.txt"}, **request)
            self.assertEqual(invalid, {"outcome": "conflict", "message": "文件名无效，请改名另存。"})
            self.assertEqual((progress, uploads), ([], []))
            self.assertEqual(self.listing(folder="/disk1")["containers"], [original])

            result = self.call("commit_file", target=target | {"name": "cafe\u0301 2026.txt"}, **request)
            self.assertEqual(result["outcome"], "committed")
            saved = result["commit"]["container"]
            self.assertEqual((saved["name"], saved["tags"], saved["latest_version"]), ("café 2026.txt", ["mobile"], 1))
            self.assertEqual(uploads, [201])
            self.assertEqual(progress[-1][:3], (0, len(edited), len(edited)))
            self.call("logout", connection=request["connection"])

        self.assertEqual(source.read_bytes(), edited)
        for container, expected in [(original, b"server original"), (saved, edited)]:
            route = f"/containers/{container['container_id']}/versions/1/payload"
            self.assertEqual(t.http("GET", route, 200)[0], expected)

    def test_removed_version_target_can_be_saved_as_a_new_file(self):
        t = self.zxs
        original = self.create("/disk1", "report.txt", b"server original")["container"]
        cid = original["container_id"]
        listed = self.call("browse", connection=self.connection, folder="/disk1")["containers"][0]
        self.assertEqual(listed, original)
        source = t.root / "phone-edit.txt"
        edited = b"phone edits to save"
        source.write_bytes(edited)
        progress = []
        request = {"connection": self.connection, "file": str(source), "progress": progress}
        target = {"kind": "version", "container_id": cid, "expected_version": listed["latest_version"]}
        t.http("DELETE", f"/containers/{cid}?expected_version=1", 200)

        for _ in range(2):
            conflict = self.call("commit_file", target=target, **request)
            self.assertEqual(conflict, {"outcome": "conflict", "message": "当前找不到原文件，请另存为新文件。"})
        self.assertEqual(progress, [])

        result = self.call("commit_file", **request,
                           target={"kind": "new", "folder": "/disk1", "name": "report (phone).txt"})
        saved = result["commit"]["container"]["container_id"]
        self.assertNotEqual(saved, cid)
        self.assertEqual(t.http("GET", f"/containers/{saved}/versions/1/payload", 200)[0], edited)
        recycled = t.http("GET", "/recycle", 200)[0]["entries"]
        self.assertEqual([entry["container"]["container_id"] for entry in recycled], [cid])
        self.assertEqual(source.read_bytes(), edited)

    def test_version_target_recycled_before_conflict_lookup_can_be_saved_as_a_new_file(self):
        t = self.zxs
        cid = self.create("/disk1", "report.txt", b"original")["container"]["container_id"]
        self.append(cid, 1, b"desktop edits")
        source = t.root / "phone-edit.txt"
        edited = b"phone edits"
        source.write_bytes(edited)
        versions = f"/containers/{cid}/versions"
        uploads = []

        def recycle_after_conflict(request, response, _):
            if request.command == "POST" and request.path == versions:
                uploads.append(response.status)
                if response.status == 409:
                    t.http("DELETE", f"/containers/{cid}?expected_version=2", 200)
            return True

        with proxy_requests(t.port, recycle_after_conflict) as endpoint:
            connection = self.connection | {"server": endpoint}
            target = {"kind": "version", "container_id": cid, "expected_version": 1}
            conflict = self.call("commit_file", connection=connection, file=str(source), target=target)
            self.assertEqual(conflict, {"outcome": "conflict", "message": "当前找不到原文件，请另存为新文件。"})
            self.assertEqual(uploads, [409])
            target = {"kind": "new", "folder": "/disk1", "name": "report (phone).txt"}
            saved = self.call("commit_file", connection=connection, file=str(source), target=target)["commit"]
            self.call("logout", connection=connection)

        saved_id = saved["container"]["container_id"]
        self.assertEqual(t.http("GET", f"/containers/{saved_id}/versions/1/payload", 200)[0], edited)
        recycled = t.http("GET", "/recycle", 200)[0]["entries"]
        self.assertEqual([entry["container"]["container_id"] for entry in recycled], [cid])
        self.assertEqual(source.read_bytes(), edited)

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

    def test_only_successful_return_to_file_list_closes_open_zip(self):
        t = self.zxs
        original = b"cached notes"
        cid = self.create("/disk1", "work.zip",
                          workspace_zip("notes/old.txt", original))["container"]["container_id"]
        cache = t.root / "cache"
        pinned = {"id": cid, "connection": self.connection, "cache": str(cache), "version": 1}
        listing = {"connection": self.connection, "folder": "/disk1"}
        self.assertTrue(self.call("open", **pinned)["bundle"])
        archive = next(cache.glob("*/work.zip")).resolve()
        descriptors = Path("/proc/self/fd")
        if descriptors.is_dir():
            self.assertIn(archive, [path.resolve() for path in descriptors.iterdir()])

        t.stop()
        try:
            self.call("browse", status=502, **listing)
            root = self.call("browse_bundle", directory=".", **pinned)
            self.assertIn("notes", [entry["name"] for entry in root["entries"]])
            child = self.call("browse_bundle", directory="notes", **pinned)
            self.assertEqual([entry["name"] for entry in child["entries"]], ["old.txt"])
            member = self.call("open_bundle_member", path="notes/old.txt", **pinned)
            self.assertEqual(Path(member["path"]).read_bytes(), original)
        finally:
            t.start()

        self.assertEqual(self.call("browse", **listing)["containers"][0]["container_id"], cid)
        self.assertTrue(archive.is_file())
        if descriptors.is_dir():
            self.assertNotIn(archive, [path.resolve() for path in descriptors.iterdir()])
        t.stop()
        try:
            self.call("browse_bundle", directory=".", status=502, **pinned)
        finally:
            t.start()

    def test_cancel_zip_refresh_preserves_open_version_for_offline_browsing(self):
        t = self.zxs
        cid = self.create("/disk1", "work.zip",
                          workspace_zip("notes/old.txt", b"cached notes"))["container"]["container_id"]
        cache = t.root / "cache"
        request = {"id": cid, "connection": self.connection, "cache": str(cache)}
        opened = self.call("open", **request)
        self.assertTrue(opened["bundle"])
        self.assertEqual(opened["version"], 1)
        before = set(cache.iterdir())
        updated = workspace_zip("notes/new.txt", b"updated notes")
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

    def test_failed_zip_refresh_preserves_open_version_for_offline_browsing(self):
        t = self.zxs
        original = b"cached notes"
        cid = self.create("/disk1", "work.zip",
                          workspace_zip("notes/old.txt", original))["container"]["container_id"]
        request = {"id": cid, "connection": self.connection, "cache": str(t.root / "cache")}
        self.call("open", **request)
        pinned = request | {"version": 1}
        self.call("browse_bundle", directory="notes", **pinned)

        self.append(cid, 1, workspace_zip("updated/new.txt", b"updated notes"))
        self.call("browse_bundle", directory="notes", status=404, **request)
        t.stop()
        try:
            page = self.call("browse_bundle", directory="notes", **pinned)
            self.assertEqual((page["version"], [entry["name"] for entry in page["entries"]]), (1, ["old.txt"]))
            member = self.call("open_bundle_member", path="notes/old.txt", **pinned)
            self.assertEqual(Path(member["path"]).read_bytes(), original)
        finally:
            t.start()
            t.login_session()
        page = self.call("browse_bundle", directory="updated", **request)
        self.assertEqual((page["version"], [entry["name"] for entry in page["entries"]]), (2, ["new.txt"]))

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

    def test_photo_rejects_incorrect_case_in_the_target_directory(self):
        t = self.zxs
        folder = "/disk1/Photos"
        source = t.root / "selected.png"
        source.write_bytes(PHOTO_PNG)
        original = self.create(folder, source.name, PHOTO_PNG)["container"]
        alias = folder.lower()
        self.assertEqual(self.listing(folder=alias)["containers"], [original])

        target = {"kind": "photo", "folder": alias, "name": source.name}
        rejected = self.call("commit_file", connection=self.connection, file=str(source), target=target, status=409)
        self.assertFalse(rejected["password_rejected"])
        self.assertEqual(self.listing(folder=folder)["containers"], [original])

    def test_photo_claim_rechecks_location_and_content_after_the_listing(self):
        t = self.zxs
        source = t.root / "selected.png"
        contents = PHOTO_PNG
        edited = b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNg+M8AAAICAQB7CYF4AAAAAElFTkSuQmCC")
        source.write_bytes(contents)
        for moved in (True, False):
            with self.subTest(moved=moved):
                folder = "/disk1/Move" if moved else "/disk1/Edit"
                previous = self.create(folder, source.name, contents)["container"]
                cid = previous["container_id"]
                events = []

                def trace(request, _response, _body):
                    route = request.path.split("?", 1)[0]
                    if not route.startswith("/sessions"):
                        events.append((request.command, route))
                    if request.command == "GET" and route == "/containers" and events.count(("GET", route)) == 1:
                        if moved:
                            t.http("PATCH", f"/containers/{cid}", 200, {"folder": "/disk2/Archive"})
                        else:
                            self.append(cid, 1, edited)
                    return True

                with proxy_requests(t.port, trace) as endpoint:
                    connection = self.connection | {"server": endpoint}
                    request = {"connection": connection, "file": str(source)}
                    ordinary = self.call("commit_file", **request,
                                         target={"kind": "new", "folder": folder, "name": "ordinary.png"})
                    self.assertEqual(ordinary["outcome"], "committed")
                    self.assertEqual(events, [("POST", "/containers")])
                    events.clear()
                    photo = self.call("commit_file", **request,
                                      target={"kind": "photo", "folder": folder, "name": source.name})
                    self.assertEqual(photo["outcome"], "committed")
                    self.assertEqual(events, [("GET", "/containers"), ("GET", "/containers"),
                                              ("GET", "/folders"), ("POST", "/containers")])
                    self.call("logout", connection=connection)

                saved = photo["commit"]["container"]
                expected_name = source.name if moved else "selected (2).png"
                self.assertEqual((saved["folder"], saved["name"]), (folder, expected_name))
                self.assertEqual(t.http("GET", f"/containers/{saved['container_id']}/versions/1/payload", 200)[0], contents)
                old = self.detail(cid)["container"]
                self.assertEqual((old["folder"], old["latest_version"]), ("/disk2/Archive", 1) if moved else (folder, 2))
                self.assertEqual(t.http("GET", f"/containers/{cid}/versions/{old['latest_version']}/payload", 200)[0],
                                 contents if moved else edited)
        self.assertEqual(source.read_bytes(), contents)
