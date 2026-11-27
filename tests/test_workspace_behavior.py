"""User-visible workspace rules and retention of local edits."""

from io import BytesIO
import json
import os
from pathlib import Path
import subprocess
from zipfile import ZipFile

from behavior_support import ServerCase
from smoke import CLIENT, proxy_requests
from workspace_selection import ui


class WorkspaceBehavior(ServerCase):
    def checkout(self, cid, workspace=None, *flags):
        t = self.zxs
        return t.cli(workspace or t.b, "checkout", cid, *flags)

    def failure(self, workspace, *args):
        result = subprocess.run([CLIENT, "--workspace", workspace, *map(str, args)], env=self.zxs.env,
                                capture_output=True, encoding="utf-8", timeout=20)
        self.assertEqual(result.returncode, 3, (result.stdout, result.stderr))
        self.assertEqual(result.stdout, "")
        return result.stderr

    def test_checkout_avoids_unnecessary_payload_requests(self):
        t = self.zxs
        cid = self.create("/disk1", "note.txt", b"clean")["container"]["container_id"]
        archive = BytesIO()
        with ZipFile(archive, "w") as zipped:
            zipped.writestr(".zxsignore", "")
            zipped.writestr("note.txt", "bundle contents")
        bundle = self.create("/disk1", "bundle.zip", archive.getvalue())["container"]["container_id"]
        requests = []

        def trace(request, response, _data):
            if request.path.endswith("/payload"):
                requests.append((request.headers.get("Range"), response.status))
            return True

        with proxy_requests(t.port, trace) as endpoint:
            t.cli(t.b, "login", endpoint)
            self.checkout(cid, t.b, "--version", 1)
            self.checkout(cid, t.b, "--version", 1)
            self.assertEqual(requests, [(None, 200)])

            local = t.b / "disk1/note.txt"
            stamp = local.stat()
            local.write_bytes(b"edits")
            os.utime(local, ns=(stamp.st_atime_ns, stamp.st_mtime_ns))
            self.failure(t.b, "checkout", cid, "--version", 1)
            self.assertEqual(local.read_bytes(), b"edits")
            self.assertEqual(requests, [(None, 200)])

            self.checkout(bundle)
            self.assertEqual((t.b / "disk1/bundle.zip/note.txt").read_text(), "bundle contents")
            self.assertEqual(requests, [(None, 200), (None, 200)])

    def test_restore_after_cross_disk_move_reports_destination_and_backs_up_old_edit(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        first = self.create("/disk1/项目", "任务.txt", b"historical")
        cid = first["container"]["container_id"]
        self.append(cid, 1, b"latest")
        self.checkout(cid)
        old = t.b / "disk1/项目/任务.txt"
        old.write_bytes(b"unsaved edit")
        t.http("PATCH", f"/containers/{cid}", 200, {"folder": "/disk2/移动后"})
        message = self.failure(t.b, "commit", old)
        self.assertIn("disk1/项目/任务.txt", message)
        self.assertIn("/disk2/移动后/任务.txt", message)
        self.assertEqual(self.detail(cid)["container"]["latest_version"], 2)
        self.failure(t.b, "checkout", cid, "--version", 1)

        restored = self.checkout(cid, t.b, "--version", 1, "--backup")
        self.assertEqual(restored["local"], "disk2/移动后/任务.txt")
        self.assertEqual((t.b / restored["local"]).read_bytes(), b"historical")
        self.assertEqual(Path(restored["backup"]).read_bytes(), b"unsaved edit")
        self.assertFalse(old.exists())
        self.assertEqual(self.detail(cid)["container"]["latest_version"], 2)
        self.assertEqual(t.cli(t.b, "commit", restored["local"])["data"]["version"]["version"], 3)

    def test_moved_destination_preserves_untracked_occupant_and_custom_checkout(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        cid = self.create("/disk1", "note.txt", b"original")["container"]["container_id"]
        self.append(cid, 1, b"current")
        self.checkout(cid)
        self.checkout(cid, t.c, "--to", "my-copy.txt")
        t.http("PATCH", f"/containers/{cid}", 200, {"folder": "/disk2"})
        target = t.b / "disk2/note.txt"
        target.parent.mkdir()
        target.write_bytes(b"unrelated local document")
        self.failure(t.b, "checkout", cid, "--version", 1, "--backup")
        self.assertEqual(target.read_bytes(), b"unrelated local document")
        self.assertEqual((t.b / "disk1/note.txt").read_bytes(), b"current")
        custom = self.checkout(cid, t.c, "--version", 1)
        self.assertEqual(custom["local"], "my-copy.txt")
        self.assertEqual((t.c / "my-copy.txt").read_bytes(), b"original")

    def test_save_as_after_move_uses_only_the_selected_new_path(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        original = self.create("/disk1", "note.txt", b"same bytes")["container"]
        cid = original["container_id"]
        self.checkout(cid)
        local = t.b / "disk1/note.txt"
        t.http("PATCH", f"/containers/{cid}", 200, {"folder": "/disk2"})
        self.failure(t.b, "commit", local)
        copy = t.cli(t.b, "commit", local, "--on-conflict", "new", "--name", "copy.txt")["data"]
        self.assertNotEqual(copy["container"]["container_id"], cid)
        self.assertEqual((copy["container"]["folder"], copy["container"]["name"]), ("/disk2", "copy.txt"))
        self.assertEqual(copy["version"]["version"], 1)
        self.assertEqual(self.detail(cid)["container"]["latest_version"], 1)
        retry = t.cli(t.b, "commit", local, "--on-conflict", "new", "--name", "copy.txt")["data"]
        self.assertEqual(retry, copy)

    def test_sync_updates_clean_file_and_retains_other_local_edit(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        ids = [self.create("/disk1", name, b"first")["container"]["container_id"]
               for name in ("clean.txt", "edited.txt")]
        for cid in ids:
            self.checkout(cid)
            self.append(cid, 1, b"new remote data")
        (t.b / "disk1/edited.txt").write_bytes(b"working draft")
        report = t.cli(t.b, "sync")
        self.assertEqual(report["issues"], [])
        self.assertEqual(report["local_changes"], 1)
        self.assertEqual((t.b / "disk1/clean.txt").read_bytes(), b"new remote data")
        self.assertEqual((t.b / "disk1/edited.txt").read_bytes(), b"working draft")
        state = {row["path"]: row["state"] for row in t.cli(t.b, "status")["entries"]}
        self.assertEqual(state, {"disk1/clean.txt": "clean", "disk1/edited.txt": "modified"})

    def test_failed_download_leaves_the_previous_file_and_allows_retry(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        commit = self.create("/disk1", "note.txt", b"first")
        cid = commit["container"]["container_id"]
        self.checkout(cid)
        self.append(cid, 1, b"second")
        remote = t.path(commit["container"]) / "v000002/note.txt"

        def replace_bytes(contents):
            remote.chmod(0o644)
            remote.write_bytes(contents)
            remote.chmod(0o444)

        replace_bytes(b"broken")
        t.cli(t.b, "checkout", cid, expected=1)
        local = t.b / "disk1/note.txt"
        self.assertEqual(local.read_bytes(), b"first")
        row, = t.cli(t.b, "status")["entries"]
        self.assertEqual((row["state"], row["version"]), ("clean", 1))
        replace_bytes(b"second")
        self.assertEqual(self.checkout(cid)["version"], 2)
        self.assertEqual(local.read_bytes(), b"second")

    def test_sync_verifies_the_new_version_before_following_a_move(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        cid = self.create("/disk1", "move.txt", b"first")["container"]["container_id"]
        self.checkout(cid)
        self.append(cid, 1, b"second")
        moved, _ = t.http("PATCH", f"/containers/{cid}", 200, {"folder": "/disk2"})
        payload = t.path(moved) / "v000002/move.txt"
        payload.chmod(0o644)
        payload.write_bytes(b"broken")
        payload.chmod(0o444)
        old, new = t.b / "disk1/move.txt", t.b / "disk2/move.txt"
        self.assertTrue(t.cli(t.b, "sync", expected=1)["issues"])
        self.assertEqual(old.read_bytes(), b"first")
        self.assertFalse(new.exists())
        row, = t.cli(t.b, "status")["entries"]
        self.assertEqual((row["path"], row["version"]), ("disk1/move.txt", 1))
        payload.chmod(0o644)
        payload.write_bytes(b"second")
        payload.chmod(0o444)
        self.assertEqual(t.cli(t.b, "sync")["updated"], 1)
        self.assertFalse(old.exists())
        self.assertEqual(new.read_bytes(), b"second")

    def test_sync_follows_case_only_rename_when_there_is_a_new_version(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        cid = self.create("/disk1", "notes.txt", b"initial")["container"]["container_id"]
        self.checkout(cid)
        self.append(cid, 1, b"new bytes")
        t.http("PATCH", f"/containers/{cid}", 200, {"name": "Notes.txt"})
        report = t.cli(t.b, "sync")
        self.assertEqual(report["updated"], 1)
        self.assertEqual([p.name for p in (t.b / "disk1").iterdir()], ["Notes.txt"])
        self.assertEqual((t.b / "disk1/Notes.txt").read_bytes(), b"new bytes")

    def test_sync_rejects_version_rollback_and_same_version_different_bytes(self):
        t = self.zxs
        first = self.create("/disk1", "note.txt", b"first")
        cid = first["container"]["container_id"]
        self.append(cid, 1, b"second")
        digest = first["version"]["blake3"]
        revision = None
        payloads = []

        def rewrite(request, response, data):
            if request.path.endswith("/payload"):
                payloads.append(request.path)
            if revision is None or not request.path.startswith("/containers?ids="):
                return True
            page = json.loads(data)
            page["containers"][0].update(latest_version=revision, latest_blake3=digest)
            body = json.dumps(page).encode()
            request.send_response(response.status)
            request.send_header("Content-Type", "application/json")
            request.send_header("Content-Length", str(len(body)))
            request.end_headers()
            request.wfile.write(body)
            return False

        with proxy_requests(t.port, rewrite) as endpoint:
            t.cli(t.b, "login", endpoint)
            local = t.b / self.checkout(cid)["local"]
            payloads.clear()
            for revision, reason in [(1, "回退到 v1"), (2, "内容与已追踪的版本不一致")]:
                with self.subTest(remote_version=revision):
                    report = t.cli(t.b, "sync", expected=1)
                    issue, = report["issues"]
                    self.assertEqual(issue["path"], "disk1/note.txt")
                    self.assertIn(reason, issue["reason"])
                    self.assertEqual(report["updated"], 0)
                    self.assertEqual(local.read_bytes(), b"second")
                    row, = t.cli(t.b, "status")["entries"]
                    self.assertEqual((row["state"], row["version"]), ("clean", 2))
            self.assertEqual(payloads, [])
            revision = None
            self.append(cid, 2, b"third")
            self.assertEqual(t.cli(t.b, "sync")["updated"], 1)
            self.assertEqual(local.read_bytes(), b"third")
            self.assertEqual(payloads, [f"/containers/{cid}/versions/3/payload"])

    def test_directory_sync_reports_occupied_subtree_and_finishes_other_paths(self):
        t = self.zxs
        self.create("/disk1/Tree/Occupied/Child", "remote.txt", b"remote subtree")
        self.create("/disk1/Tree/Sibling", "safe.txt", b"another file")
        t.cli(t.b, "login", t.endpoint)
        root = t.b / "disk1/Tree"
        root.mkdir()
        occupied = root / "Occupied"
        occupied.write_bytes(b"unrelated local content")
        report = t.cli(t.b, "sync", "--folder", "/disk1/Tree", expected=1)
        self.assertEqual(report["updated"], 1)
        self.assertIn("disk1/Tree/Occupied", [entry["path"] for entry in report["issues"]])
        self.assertEqual(occupied.read_bytes(), b"unrelated local content")
        self.assertEqual((root / "Sibling/safe.txt").read_bytes(), b"another file")

        saved = occupied.rename(t.root / "saved-local-copy")
        retried = t.cli(t.b, "sync", "--folder", "/disk1/Tree")
        self.assertEqual((retried["updated"], retried["current"], retried["issues"]), (1, 1, []))
        self.assertEqual((occupied / "Child/remote.txt").read_bytes(), b"remote subtree")
        self.assertEqual(saved.read_bytes(), b"unrelated local content")

    def test_missing_local_file_requires_new_version_confirmation_to_sync_delete(self):
        t = self.zxs
        original = self.create("/disk1", "item.txt", b"one")["container"]
        cid = original["container_id"]
        t.cli(t.b, "login", t.endpoint)
        local = t.b / self.checkout(cid)["local"]
        self.failure(t.b, "sync-delete", local)
        self.assertEqual(local.read_bytes(), b"one")
        local.unlink()
        newest = self.append(cid, 1, b"two")
        self.failure(t.b, "sync-delete", local)
        self.assertEqual(self.detail(cid)["container"], newest["container"])
        self.assertEqual(t.http("GET", "/recycle", 200)[0]["entries"], [])
        row, = t.cli(t.b, "status")["entries"]
        self.assertEqual((row["state"], row["version"]), ("missing", 1))

        archived = t.cli(t.b, "sync-delete", local, "--expected-version", 2)
        t.http("GET", f"/containers/{cid}", 404)
        self.assertEqual(archived["container"]["latest_version"], 2)
        for version, contents in enumerate((b"one", b"two"), 1):
            self.assertEqual((t.archived(archived) / f"item.txt/v{version:06d}/item.txt").read_bytes(),
                             contents)
        self.assertEqual(t.cli(t.b, "status")["entries"], [])

    def test_directory_commit_retry_keeps_prior_successes_at_the_same_version(self):
        t = self.zxs
        folder = "/disk1/Project"
        remote = self.create(folder, "occupied.txt", b"existing bytes")
        t.cli(t.b, "login", t.endpoint)
        root = t.b / "disk1/Project"
        root.mkdir()
        (root / "ready.txt").write_bytes(b"first successful upload")
        draft = root / "occupied.txt"
        draft.write_bytes(b"separate local draft")
        with ui(t.b, t.config) as action:
            report = action("commit_directory", directory="disk1/Project")
            self.assertEqual(report["committed"], 1)
            issue, = report["issues"]
            self.assertEqual((issue["path"], issue["conflict"]), ("disk1/Project/occupied.txt", True))
            self.assertEqual(draft.read_bytes(), b"separate local draft")
            draft.rename(root / "resolved.txt")
            retried = action("commit_directory", directory="disk1/Project")
            self.assertEqual((retried["committed"], retried["issues"]), (1, []))
            self.assertEqual(action("commit_directory", directory="disk1/Project")["committed"], 0)
        self.assertEqual(self.detail(remote["container"]["container_id"])["versions"], [remote["version"]])
        expected = {"ready.txt": b"first successful upload", "resolved.txt": b"separate local draft",
                    "occupied.txt": b"existing bytes"}
        containers = self.listing(folder=folder)["containers"]
        self.assertCountEqual([item["name"] for item in containers], expected.keys())
        for item in containers:
            self.assertEqual(item["latest_version"], 1)
            path = f"/containers/{item['container_id']}/versions/1/payload"
            self.assertEqual(t.http("GET", path, 200)[0], expected[item["name"]])

    def test_pruned_version_cannot_replace_or_back_up_the_active_checkout(self):
        t = self.zxs
        cid = self.create("/disk1", "note.txt", b"expired")["container"]["container_id"]
        self.append(cid, 1, b"retained")
        t.cli(t.b, "login", t.endpoint)
        local = t.b / self.checkout(cid)["local"]
        unchanged = t.cli(t.b, "status")["entries"]
        backups = t.b / ".zxs/backups"
        before = list(backups.glob("**/*"))
        t.stop()
        t.offline("prune", "--keep-last", 1, "--container", cid)
        t.start()
        t.login_session()
        t.cli(t.b, "checkout", cid, "--version", 1, "--backup", expected=1)
        self.assertEqual(local.read_bytes(), b"retained")
        self.assertEqual(t.cli(t.b, "status")["entries"], unchanged)
        self.assertEqual(list(backups.glob("**/*")), before)
        self.assertEqual(self.checkout(cid, t.b, "--version", 2)["version"], 2)
