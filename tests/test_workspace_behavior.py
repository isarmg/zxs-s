"""User-visible workspace rules and retention of local edits."""

from pathlib import Path
import subprocess

from behavior_support import ServerCase
from smoke import CLIENT


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
