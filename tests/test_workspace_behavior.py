"""User-visible workspace rules and retention of local edits."""

from io import BytesIO
import json
import os
from pathlib import Path
import sqlite3
import subprocess
from zipfile import ZipFile

from behavior_support import ServerCase
from smoke import CLIENT, proxy_requests
from workspace_selection import ui


def bundle_bytes(rules, files):
    output = BytesIO()
    with ZipFile(output, "w") as archive:
        archive.writestr(".zxsignore", rules)
        for path, contents in files.items():
            archive.writestr(path, contents)
    return output.getvalue()


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

    def test_cli_file_search_respects_the_selected_folder(self):
        t = self.zxs
        folder = "/disk1/Selected"
        selected = self.create(folder, "report.txt", b"scoped", tags=["work"])["container"]
        nested = self.create(folder + "/Child", "report.txt", b"nested", tags=["work"])["container"]
        other = self.create("/disk2", "report.txt", b"other disk", tags=["work"])["container"]
        t.cli(t.a, "login", t.endpoint)

        page = t.cli(t.a, "ls", folder, "--q", "report", "--tag", "work")
        self.assertEqual(page["containers"], [selected])
        self.assertEqual(page["folders"], [{"folder": folder + "/Child"}])
        page = t.cli(t.a, "ls", "--q", "report", "--tag", "work")
        self.assertCountEqual(page["containers"], [selected, nested, other])
        self.assertEqual(page["folders"], [])

    def test_bundle_checks_its_final_name_and_keeps_the_zip_suffix(self):
        t = self.zxs
        t.cli(t.a, "login", t.endpoint)
        ordinary = self.create("/disk1/abc", "remote.txt", b"independent remote file")["container"]
        for directory, name in (("abc", "abc.zip"), ("already.ZIP", "already.ZIP")):
            with self.subTest(directory=directory):
                local = t.a / "disk1" / directory
                local.mkdir()
                (local / "local.txt").write_text("bundle member")
                initialized = t.a / t.cli(t.a, "init-bundle", local)
                self.assertEqual(initialized, local.with_name(name))
                self.assertEqual((initialized / "local.txt").read_text(), "bundle member")
                self.assertTrue((initialized / ".zxsignore").is_file())
                if local != initialized:
                    self.assertFalse(local.exists())
                commit = t.cli(t.a, "commit", initialized)["data"]["container"]
                self.assertEqual(commit["name"], name)
        self.assertEqual(self.listing(folder="/disk1/abc")["containers"], [ordinary])

        t.http("POST", "/folders", 201, {"folder": "/disk1/occupied.ZIP"})
        local = t.a / "disk1/occupied"
        local.mkdir()
        self.assertIn("已被占用", self.failure(t.a, "init-bundle", local))
        self.assertFalse((local / ".zxsignore").exists())

        local = t.a / "disk1/local"
        local.mkdir()
        (local / "draft.txt").write_bytes(b"working copy")
        occupied = local.with_suffix(".zip")
        occupied.mkdir()
        self.assertIn("已被占用", self.failure(t.a, "init-bundle", local))
        self.assertEqual((local / "draft.txt").read_bytes(), b"working copy")
        self.assertEqual(list(occupied.iterdir()), [])
        self.assertFalse((local / ".zxsignore").exists())

    def test_submit_rejects_a_directory_bundle_after_its_suffix_is_removed(self):
        t = self.zxs
        t.cli(t.a, "login", t.endpoint)
        bare = t.a / "disk1/album"
        bare.mkdir()
        (bare / "photo.txt").write_text("original")
        initial = t.a / t.cli(t.a, "init-bundle", bare)
        container = t.cli(t.a, "commit", initial)["data"]["container"]
        initial.rename(bare)
        (bare / "photo.txt").write_text("updated")

        self.assertIn("须以 .zip 结尾", self.failure(t.a, "commit", bare, "--name", "album.zip"))
        with ui(t.a, t.config) as action:
            report = action("commit_directory", directory="disk1")
        self.assertEqual(report["committed"], 0)
        issue, = report["issues"]
        self.assertIn("须以 .zip 结尾", issue["reason"])
        self.assertEqual(self.listing(folder="/disk1", recursive="true")["containers"], [container])
        self.assertEqual((bare / "photo.txt").read_text(), "updated")
        bare.rename(initial)
        result = t.cli(t.a, "commit", initial)["data"]
        self.assertEqual(result["version"]["version"], 2)
        self.assertEqual(result["container"]["container_id"], container["container_id"])

    def test_rename_keeps_a_known_directory_bundles_zip_suffix(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        archive = BytesIO()
        with ZipFile(archive, "w") as zipped:
            zipped.writestr(".zxsignore", "")
            zipped.writestr("member.txt", "first")
        cid = self.create("/disk1", "project.zip", archive.getvalue())["container"]["container_id"]
        self.checkout(cid)

        renamed = t.cli(t.b, "patch", cid, "--name", "renamed")
        self.assertEqual(renamed["name"], "renamed.zip")
        local = t.b / "disk1/renamed.zip"
        (local / "member.txt").write_text("second")
        self.assertEqual(t.cli(t.b, "commit", local)["data"]["version"]["version"], 2)
        result = self.checkout(cid, t.c)
        self.assertTrue(result["bundle"])
        self.assertEqual((t.c / result["local"] / "member.txt").read_text(), "second")

    def test_custom_checkout_checks_the_suffix_only_for_directory_bundles(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        bundled = bundle_bytes("", {"note.txt": "bundle member"})
        cid = self.create("/disk1", "project.zip", bundled)["container"]["container_id"]
        alias = "disk1/custom"
        self.assertIn("须以 .zip 结尾", self.failure(t.b, "checkout", cid, "--to", alias))
        self.assertFalse((t.b / alias).exists())
        destination = self.checkout(cid, t.b, "--to", alias + ".ZIP")
        self.assertTrue(destination["bundle"])
        self.assertEqual((t.b / destination["local"] / "note.txt").read_text(), "bundle member")

        stream = BytesIO()
        with ZipFile(stream, "w") as archive:
            archive.writestr("note.txt", "ordinary ZIP member")
        ordinary = self.create("/disk1", "ordinary.zip", stream.getvalue())["container"]["container_id"]
        destination = self.checkout(ordinary, t.b, "--to", alias)
        self.assertFalse(destination["bundle"])
        self.assertEqual((t.b / alias).read_bytes(), stream.getvalue())

    def test_historical_bundle_uses_its_committed_name_for_type_detection(self):
        t = self.zxs
        bundled = bundle_bytes("", {"note.txt": "historical bundle member"})
        cid = self.create("/disk1", "project.zip", bundled)["container"]["container_id"]
        t.http("PATCH", f"/containers/{cid}", 200, {"name": "project.txt"})
        self.append(cid, 1, b"ordinary current version")
        requests = []

        def trace(request, _response, _data):
            if request.command == "GET" and request.path.startswith(f"/containers/{cid}"):
                requests.append(request.path)
            return True

        with proxy_requests(t.port, trace) as endpoint:
            t.cli(t.b, "login", endpoint)
            latest = self.checkout(cid)
            local = t.b / latest["local"]
            local.write_bytes(b"unsubmitted local edit")
            expected = [f"/containers/{cid}", f"/containers/{cid}/versions/1/payload"]
            requests.clear()
            self.assertIn("须以 .zip 结尾", self.failure(t.b, "checkout", cid, "--version", 1, "--backup"))
            self.assertEqual(local.read_bytes(), b"unsubmitted local edit")
            self.assertEqual(requests, expected)
            self.assertFalse((t.b / ".zxs/backups").exists())

            requests.clear()
            restored = self.checkout(cid, t.c, "--version", 1, "--to", "disk1/restored.ZIP")
            self.assertTrue(restored["bundle"])
            self.assertEqual(restored["restored_from"], 1)
            self.assertEqual((t.c / restored["local"] / "note.txt").read_text(), "historical bundle member")
            self.assertEqual(requests, expected)

        self.assertEqual(self.detail(cid)["container"]["latest_version"], 2)

    def test_recycled_bundle_restored_under_a_bare_name_preserves_local_directory(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        cid = self.create("/disk1", "project.zip", bundle_bytes("", {"note.txt": "v1"}))["container"]["container_id"]
        self.append(cid, 1, bundle_bytes("", {"note.txt": "v2"}))
        original = t.b / self.checkout(cid)["local"]
        archived, _ = t.http("DELETE", f"/containers/{cid}", 200)
        with ui(t.b, t.config) as action:
            restored = action("restore", id=archived["recycle"]["recycle_id"],
                              patch={"folder": "/disk2", "name": "restored"})
        self.assertEqual(restored["container"]["name"], "restored")
        issue, = restored["issues"]
        self.assertIn("须以 .zip 结尾", issue["reason"])

        report = t.cli(t.b, "sync", expected=1)
        self.assertEqual(report["updated"], 0)
        issue, = report["issues"]
        self.assertIn("须以 .zip 结尾", issue["reason"])
        result = self.failure(t.b, "checkout", cid, "--version", 1, "--backup")
        self.assertIn("须以 .zip 结尾", result)
        self.assertEqual((original / "note.txt").read_text(), "v2")
        self.assertFalse((t.b / "disk2/restored").exists())
        self.assertEqual(self.detail(cid)["container"]["latest_version"], 2)

        self.assertEqual(t.cli(t.b, "patch", cid, "--name", "restored")["name"], "restored.zip")
        historical = self.checkout(cid, t.b, "--version", 1)
        self.assertEqual(historical["local"], "disk2/restored.zip")
        self.assertEqual((t.b / historical["local"] / "note.txt").read_text(), "v1")
        self.assertFalse(original.exists())
        published = t.cli(t.b, "commit", historical["local"])["data"]
        self.assertEqual((published["container"]["container_id"], published["version"]["version"]), (cid, 3))

    def test_bundle_sync_refuses_to_unignore_existing_local_files(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        for includes_private in (False, True):
            with self.subTest(includes_private=includes_private):
                initial = bundle_bytes("private.local\n", {"note.txt": "v1"})
                name = f"project-{includes_private}.zip"
                cid = self.create("/disk1", name, initial)["container"]["container_id"]
                directory = t.b / self.checkout(cid)["local"]
                private = directory / "private.local"
                private.write_bytes(b"local data to exclude")
                files = {"note.txt": "v2"}
                if includes_private:
                    files["private.local"] = "remote data"
                self.append(cid, 1, bundle_bytes("", files))

                report = t.cli(t.b, "sync", expected=1)
                self.assertEqual(report["updated"], 0)
                issue = next(issue for issue in report["issues"] if issue["path"] == "disk1/" + name)
                self.assertIn("private.local", issue["reason"])
                self.assertIn("private.local", self.failure(t.b, "checkout", cid, "--backup"))
                self.assertEqual((directory / ".zxsignore").read_text(), "private.local\n")
                self.assertEqual((directory / "note.txt").read_bytes(), b"v1")
                self.assertEqual(private.read_bytes(), b"local data to exclude")
                row = next(row for row in t.cli(t.b, "status")["entries"] if row["container_id"] == cid)
                self.assertEqual((row["state"], row["version"]), ("clean", 1))

    def test_bundle_dir_to_file_checks_nested_ignored_items_before_moving_members(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        manifest = "*.local\n"
        directory = bundle_bytes(manifest, {"config/nested/settings.txt": "published setting"})
        regular = bundle_bytes(manifest, {"config": "plain configuration"})
        for historical in (False, True):
            with self.subTest(historical=historical):
                folder = "/disk1/history" if historical else "/disk1/update"
                cid = self.create(folder, "project.zip", regular if historical else directory)["container"]["container_id"]
                if historical:
                    self.append(cid, 1, directory)
                local = t.b / self.checkout(cid)["local"]
                if not historical:
                    self.append(cid, 1, regular)
                member = local / "config/nested/settings.txt"
                private = member.with_name("private.local")
                private.write_bytes(b"kept only in the workspace")

                if historical:
                    reason = self.failure(t.b, "checkout", cid, "--version", 1, "--backup")
                else:
                    report = t.cli(t.b, "sync", "--folder", folder, expected=1)
                    self.assertEqual(report["updated"], 0)
                    issue, = report["issues"]
                    reason = issue["reason"]
                self.assertIn("本地存在未纳入目录包的内容", reason)
                self.assertEqual(member.read_text(), "published setting")
                self.assertEqual(private.read_bytes(), b"kept only in the workspace")
                row = next(entry for entry in t.cli(t.b, "status")["entries"] if entry["container_id"] == cid)
                self.assertEqual((row["state"], row["version"]), ("clean", 2 if historical else 1))

                private.unlink()
                if historical:
                    self.checkout(cid, t.b, "--version", 1, "--backup")
                else:
                    self.assertEqual(t.cli(t.b, "sync", "--folder", folder)["updated"], 1)
                self.assertEqual((local / "config").read_text(), "plain configuration")

    def test_bundle_case_rename_does_not_leave_an_ignored_directory_under_an_alias(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        rules = "*.local\n"
        original = bundle_bytes(rules, {"config/nested/settings.txt": "old setting", "Note.txt": "old note"})
        cid = self.create("/disk1", "project.zip", original)["container"]["container_id"]
        local = t.b / self.checkout(cid)["local"]
        renamed = local / "Config"
        if renamed.exists():
            self.skipTest("requires a case-sensitive workspace filesystem")

        private = local / "config/nested/private.local"
        private.write_bytes(b"local-only data")
        incoming = bundle_bytes(rules, {"Config/nested/settings.txt": "new setting", "note.txt": "new note"})
        self.append(cid, 1, incoming)
        report = t.cli(t.b, "sync", expected=1)
        self.assertEqual(report["updated"], 0)
        issue, = report["issues"]
        self.assertIn("本地存在未纳入目录包的内容", issue["reason"])
        self.assertFalse(renamed.exists())
        self.assertEqual((local / "config/nested/settings.txt").read_text(), "old setting")
        self.assertEqual((local / "Note.txt").read_text(), "old note")
        self.assertEqual(private.read_bytes(), b"local-only data")
        row, = t.cli(t.b, "status")["entries"]
        self.assertEqual((row["state"], row["version"]), ("clean", 1))

        private.unlink()
        self.assertEqual(t.cli(t.b, "sync")["updated"], 1)
        self.assertFalse((local / "config").exists())
        self.assertFalse((local / "Note.txt").exists())
        self.assertEqual((renamed / "nested/settings.txt").read_text(), "new setting")
        self.assertEqual((local / "note.txt").read_text(), "new note")
        row, = t.cli(t.b, "status")["entries"]
        self.assertEqual((row["state"], row["version"]), ("clean", 2))

    def test_bundle_preserves_literal_manifest_and_ignores_decomposed_names(self):
        t = self.zxs
        t.cli(t.a, "login", t.endpoint)
        directory = t.a / "disk1/project.zip"
        directory.mkdir()
        excluded = "Cafe\u0301.txt"
        manifest = (excluded + "\n").encode()
        (directory / ".zxsignore").write_bytes(manifest)
        (directory / excluded).write_bytes(b"kept locally")
        (directory / "public.txt").write_bytes(b"uploaded")

        cid = t.cli(t.a, "commit", directory)["data"]["container"]["container_id"]
        data, _ = t.http("GET", f"/containers/{cid}/versions/1/payload", 200)
        with ZipFile(BytesIO(data)) as archive:
            self.assertEqual(archive.namelist(), [".zxsignore", "public.txt"])
            self.assertEqual(archive.read(".zxsignore"), manifest)

    def test_bundle_honors_first_ignore_pattern_with_utf8_bom(self):
        t = self.zxs
        t.cli(t.a, "login", t.endpoint)
        directory = t.a / "disk1/project.zip"
        directory.mkdir()
        manifest = b"\xef\xbb\xbfprivate.txt\n"
        (directory / ".zxsignore").write_bytes(manifest)
        (directory / "private.txt").write_bytes(b"kept locally")
        (directory / "public.txt").write_bytes(b"uploaded")

        cid = t.cli(t.a, "commit", directory)["data"]["container"]["container_id"]
        data, _ = t.http("GET", f"/containers/{cid}/versions/1/payload", 200)
        with ZipFile(BytesIO(data)) as archive:
            self.assertEqual(archive.namelist(), [".zxsignore", "public.txt"])
            self.assertEqual(archive.read(".zxsignore"), manifest)

    def test_moved_bundle_update_leaves_a_bundle_with_ignored_files_intact(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        initial = bundle_bytes("*.local\n", {"note.txt": "v1"})
        cid = self.create("/disk1", "project.zip", initial)["container"]["container_id"]
        original = t.b / self.checkout(cid)["local"]
        private = original / "private.local"
        private.write_bytes(b"local data to exclude")
        destination = t.b / "disk2/next/project.zip"
        t.http("PATCH", f"/containers/{cid}", 200, {"folder": "/disk2/next"})
        self.append(cid, 1, bundle_bytes("*.local\n", {"note.txt": "v2"}))

        self.assertTrue(t.cli(t.b, "sync", expected=1)["issues"])
        self.assertFalse(destination.exists())
        self.assertEqual((original / ".zxsignore").read_text(), "*.local\n")
        self.assertEqual((original / "note.txt").read_bytes(), b"v1")
        self.assertEqual(private.read_bytes(), b"local data to exclude")
        with ui(t.b, t.config) as action:
            report = action("commit_directory", directory="disk1")
        self.assertEqual((report["committed"], report["issues"]), (0, []))
        self.assertEqual(self.listing(folder="/disk1", recursive="true")["containers"], [])
        private.rename(t.root / "saved-local-data")
        self.assertEqual(t.cli(t.b, "sync")["updated"], 1)
        self.assertEqual((destination / "note.txt").read_bytes(), b"v2")
        self.assertFalse(original.exists())

    def test_checkout_does_not_turn_an_occupied_directory_into_a_bundle(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        archive = bundle_bytes("", {"shared.txt": "remote content"})
        cid = self.create("/disk1", "project.zip", archive)["container"]["container_id"]
        occupied = t.b / "disk1/project.zip"
        occupied.mkdir()
        private = occupied / "private.txt"
        private.write_bytes(b"unrelated local content")

        self.assertIn("本地目标已被占用", self.failure(t.b, "checkout", cid))
        report = t.cli(t.b, "sync", "--folder", "/disk1", expected=1)
        self.assertEqual(report["updated"], 0)
        self.assertIn("本地目标已被占用", report["issues"][0]["reason"])
        self.assertEqual(list(occupied.iterdir()), [private])
        self.assertEqual(private.read_bytes(), b"unrelated local content")
        row, = t.cli(t.b, "status")["entries"]
        self.assertEqual((row["path"], row["state"]), ("disk1/project.zip/private.txt", "untracked"))
        self.assertEqual(self.detail(cid)["container"]["latest_version"], 1)

    def test_interrupted_checkout_keeps_its_kind_after_a_remote_rename(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        t.cli(t.b, "status")
        archive = bundle_bytes("", {"note.txt": "published content", "next.txt": "remaining member"})
        for old_name, new_name, kind in (("docs.zip", "docs", "directory"),
                                         ("draft.bin", "draft.zip", "file")):
            with self.subTest(kind=kind):
                first = self.create("/disk1", old_name, archive)
                cid = first["container"]["container_id"]
                path = f"disk1/{old_name}"
                local = t.b / path
                if kind == "directory":
                    local.mkdir()
                    (local / ".zxsignore").write_bytes(b"")
                    (local / "note.txt").write_text("published content")
                else:
                    local.write_bytes(archive)
                pending = dict(path=path, container_id=cid, version=1, endpoint=t.endpoint,
                               kind=kind, payload_hash=first["version"]["blake3"])
                with sqlite3.connect(t.b / ".zxs/workspace.sqlite") as db:
                    db.execute("INSERT INTO interrupted_checkout VALUES (?, ?, ?)",
                               (cid, path, json.dumps(pending)))
                t.http("PATCH", f"/containers/{cid}", 200, {"name": new_name})

                recovered = self.checkout(cid, t.b, "--backup")
                self.assertEqual((recovered["local"], recovered["bundle"]), (path, kind == "directory"))
                if kind == "directory":
                    self.assertEqual((local / "note.txt").read_text(), "published content")
                    self.assertEqual((local / "next.txt").read_text(), "remaining member")
                else:
                    self.assertEqual(local.read_bytes(), archive)
                track = next(row for row in t.cli(t.b, "status")["entries"] if row["container_id"] == cid)
                self.assertEqual((track["path"], track["state"]), (path, "clean"))

    def test_first_interrupted_checkout_remembers_mirror_or_custom_path(self):
        t = self.zxs
        contents = b"original file contents"
        for folder, local_name, mirrors in (("mirror", "source.txt", True), ("alias", "chosen.txt", False)):
            with self.subTest(mirrors=mirrors):
                first = self.create(f"/disk1/{folder}", "source.txt", contents)
                cid = first["container"]["container_id"]
                relative = f"disk1/{folder}/{local_name}"
                local = t.b / relative
                local.parent.mkdir()
                blocked = False

                def interrupt(request, _response, _data):
                    nonlocal blocked
                    if request.path.endswith("/payload") and not blocked:
                        local.parent.rmdir()
                        local.parent.write_bytes(b"publication blocked")
                        blocked = True
                    return True

                with proxy_requests(t.port, interrupt) as endpoint:
                    t.cli(t.b, "login", endpoint)
                    flags = () if mirrors else ("--to", relative)
                    t.cli(t.b, "checkout", cid, *flags, expected=1)
                    self.assertTrue(blocked)
                    self.assertEqual(local.parent.read_bytes(), b"publication blocked")
                    local.parent.unlink()
                    local.parent.mkdir()
                    with sqlite3.connect(t.b / ".zxs/workspace.sqlite") as db:
                        saved, = db.execute("SELECT data FROM interrupted_checkout WHERE id=?", (cid,)).fetchone()

                    t.http("PATCH", f"/containers/{cid}", 200, {"name": "chosen.txt"})
                    recovered = self.checkout(cid, t.b, "--backup")
                    self.assertEqual((recovered["local"], local.read_bytes()), (relative, contents))
                    if not mirrors:
                        t.http("PATCH", f"/containers/{cid}", 200, {"name": "remote-only.txt"})
                    report = t.cli(t.b, "sync")
                    self.assertEqual((report["updated"], report["issues"]), (int(mirrors), []))

                destination = local.with_name("chosen.txt") if mirrors else local
                self.assertEqual(destination.read_bytes(), contents)
                self.assertFalse(local.with_name("remote-only.txt").exists())
                self.assertEqual(local.exists(), not mirrors)
                self.assertIs(json.loads(saved)["mirrored"], mirrors)

    def test_moved_bundle_update_does_not_adopt_an_occupied_directory(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        initial = bundle_bytes("", {"shared.txt": "v1"})
        cid = self.create("/disk1", "project.zip", initial)["container"]["container_id"]
        original = t.b / self.checkout(cid)["local"]
        occupied = t.b / "disk2/project.zip"
        occupied.mkdir(parents=True)
        private = occupied / "private.txt"
        private.write_bytes(b"unrelated local content")
        t.http("PATCH", f"/containers/{cid}", 200, {"folder": "/disk2"})
        self.append(cid, 1, bundle_bytes("", {"shared.txt": "v2"}))

        report = t.cli(t.b, "sync", expected=1)
        self.assertEqual(report["updated"], 0)
        self.assertIn("本地目标已被占用", report["issues"][0]["reason"])
        self.assertEqual(list(occupied.iterdir()), [private])
        self.assertEqual(private.read_bytes(), b"unrelated local content")
        self.assertEqual((original / "shared.txt").read_bytes(), b"v1")
        track = next(row for row in t.cli(t.b, "status")["entries"] if row["container_id"] == cid)
        self.assertEqual((track["path"], track["version"]), ("disk1/project.zip", 1))

    def test_bundle_moving_into_its_old_workspace_path_leaves_it_unchanged(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        directory = t.b / "disk1/project.zip"
        directory.mkdir()
        (directory / ".zxsignore").write_bytes(b"")
        (directory / "note.txt").write_bytes(b"v1")
        cid = t.cli(t.b, "commit", directory)["data"]["container"]["container_id"]
        t.http("PATCH", f"/containers/{cid}", 200, {"name": "temporary.zip"})
        t.http("PATCH", f"/containers/{cid}", 200, {"folder": "/disk1/project.zip/year"})

        self.assertTrue(t.cli(t.b, "sync", expected=1)["issues"])
        self.assertFalse((directory / "year").exists())
        self.append(cid, 1, bundle_bytes("", {"note.txt": "v2"}))
        self.assertTrue(t.cli(t.b, "sync", expected=1)["issues"])
        self.assertFalse((directory / "year").exists())
        self.assertEqual((directory / "note.txt").read_bytes(), b"v1")
        row, = t.cli(t.b, "status")["entries"]
        self.assertEqual((row["state"], row["version"]), ("clean", 1))

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

    def test_append_conflict_does_not_publish_for_an_already_tracked_alias(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        cid = self.create("/disk1", "report.txt", b"original")["container"]["container_id"]
        self.checkout(cid, t.b, "--to", "disk1/alias.txt")
        new = t.b / "disk1/report.txt"
        new.write_bytes(b"other content")

        reason = self.failure(t.b, "commit", new, "--on-conflict", "append")
        self.assertIn("disk1/alias.txt", reason)
        self.assertEqual(self.detail(cid)["container"]["latest_version"], 1)
        self.assertEqual(new.read_bytes(), b"other content")
        self.assertEqual((t.b / "disk1/alias.txt").read_bytes(), b"original")

    def test_commit_reuses_known_summary_and_refreshes_after_a_post_conflict(self):
        t = self.zxs
        cid = self.create("/disk1", "report.txt", b"first")["container"]["container_id"]
        summary = f"/containers?ids={cid}"
        detail = f"/containers/{cid}"
        versions = f"{detail}/versions"
        requests = []
        concurrent_commit = False

        def trace(request, response, _data):
            if request.path.startswith("/containers"):
                requests.append((request.command, request.path, response.status))
                if concurrent_commit and request.path == summary and len(requests) == 1:
                    self.append(cid, 2, b"saved by another client")
            return True

        with proxy_requests(t.port, trace) as endpoint:
            t.cli(t.b, "login", endpoint)
            local = t.b / self.checkout(cid)["local"]
            remote = self.append(cid, 1, b"second")
            local.write_bytes(b"local draft")

            requests.clear()
            self.assertIn("expected version 1, latest is 2", self.failure(t.b, "commit", local))
            self.assertEqual(requests, [("GET", summary, 200)])
            self.assertEqual(local.read_bytes(), b"local draft")

            local.write_bytes(b"second")
            requests.clear()
            recognized = t.cli(t.b, "commit", local)["data"]
            self.assertEqual(recognized, remote)
            self.assertEqual(requests, [("GET", summary, 200), ("GET", detail, 200)])

            local.write_bytes(b"append my changes")
            requests.clear()
            concurrent_commit = True
            appended = t.cli(t.b, "commit", local, "--on-conflict", "append")["data"]
            self.assertEqual(appended["version"]["version"], 4)
            self.assertEqual(requests, [
                ("GET", summary, 200), ("POST", versions, 409),
                ("GET", summary, 200), ("POST", versions, 201),
            ])

        self.assertEqual(t.http("GET", f"{versions}/3/payload", 200)[0], b"saved by another client")
        self.assertEqual(t.http("GET", f"{versions}/4/payload", 200)[0], b"append my changes")

    def test_directory_bundle_content_claim_requires_remote_zip_suffix(self):
        t = self.zxs
        local = t.b / "disk1/project.ZIP"
        local.mkdir()
        (local / ".zxsignore").write_bytes(b"private.local\n")
        (local / "private.local").write_bytes(b"kept only in this workspace")
        (local / "note.txt").write_bytes(b"published bundle member")
        requests = []

        def trace(request, _response, _data):
            if request.path.startswith("/containers"):
                requests.append((request.command, request.path))
            return True

        with proxy_requests(t.port, trace) as endpoint:
            t.cli(t.b, "login", endpoint)
            original = t.cli(t.b, "commit", local)["data"]
            cid = original["container"]["container_id"]
            summary = f"/containers?ids={cid}"
            detail = f"/containers/{cid}"
            t.http("PATCH", detail, 200, {"name": "project"})
            requests.clear()
            reason = self.failure(t.b, "commit", local, "--on-conflict", "append")
            self.assertIn("目录包名称须以 .zip 结尾", reason)
            self.assertEqual(requests, [("GET", summary)])
            self.assertEqual(self.detail(cid)["versions"], [original["version"]])

            t.http("PATCH", detail, 200, {"name": "renamed.ZIP"})
            requests.clear()
            recognized = t.cli(t.b, "commit", local, "--on-conflict", "append")["data"]
            self.assertEqual(recognized["container"]["name"], "renamed.ZIP")
            self.assertEqual(recognized["version"], original["version"])
            self.assertEqual(requests, [("GET", summary), ("GET", detail)])

        self.assertEqual(self.detail(cid)["versions"], [original["version"]])
        self.assertEqual((local / ".zxsignore").read_bytes(), b"private.local\n")
        self.assertEqual((local / "private.local").read_bytes(), b"kept only in this workspace")
        self.assertEqual((local / "note.txt").read_bytes(), b"published bundle member")

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

    def test_same_remote_name_keeps_a_custom_checkout_after_commit_or_checkout(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        for operation in ("commit", "checkout"):
            with self.subTest(operation=operation):
                folder = f"/disk1/{operation}"
                cid = self.create(folder, "original.txt", b"original")["container"]["container_id"]
                relative = f"{folder[1:]}/my-copy.txt"
                local = t.b / relative
                self.checkout(cid, t.b, "--to", relative)
                t.http("PATCH", f"/containers/{cid}", 200, {"name": local.name})

                if operation == "commit":
                    local.write_bytes(b"local edit")
                    result = t.cli(t.b, "commit", relative)["data"]
                    self.assertEqual(result["version"]["version"], 2)
                else:
                    result = self.checkout(cid, t.b, "--version", 1)
                    self.assertEqual(result["local"], relative)
                saved = local.read_bytes()
                t.http("PATCH", f"/containers/{cid}", 200, {"name": "new-remote-name.txt"})

                report = t.cli(t.b, "sync")
                self.assertEqual((report["updated"], report["issues"]), (0, []))
                self.assertEqual(local.read_bytes(), saved)
                self.assertFalse(local.with_name("new-remote-name.txt").exists())
                tracked = next(entry for entry in t.cli(t.b, "status")["entries"] if entry["container_id"] == cid)
                self.assertEqual((tracked["path"], tracked["state"]), (relative, "clean"))

    def test_sync_move_preserves_an_untracked_destination_with_different_case(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        cid = self.create("/disk1", "draft.txt", b"remote draft")["container"]["container_id"]
        original = t.b / self.checkout(cid)["local"]
        alternative = original.with_name("REPORT.txt")
        alternative.write_bytes(b"unrelated local content")
        destination = original.with_name("report.txt")
        if destination.exists():
            self.skipTest("requires a case-sensitive workspace filesystem")
        t.http("PATCH", f"/containers/{cid}", 200, {"name": destination.name})

        report = t.cli(t.b, "sync", expected=1)
        self.assertEqual(report["updated"], 0)
        self.assertIn("大小写不一致", report["issues"][0]["reason"])
        self.assertEqual(original.read_bytes(), b"remote draft")
        self.assertEqual(alternative.read_bytes(), b"unrelated local content")
        self.assertFalse(destination.exists())
        entries = {row["path"]: row["state"] for row in t.cli(t.b, "status")["entries"]}
        self.assertEqual(entries, {"disk1/draft.txt": "clean", "disk1/REPORT.txt": "untracked"})

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

    def test_checkout_follows_a_type_change_after_remote_case_only_rename(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        raw = BytesIO()
        with ZipFile(raw, "w") as archive:
            archive.writestr("note.txt", "ordinary ZIP")
        ordinary = raw.getvalue()
        bundled = bundle_bytes("", {"note.txt": "directory bundle"})
        for name, before, after, is_bundle in (("notes", ordinary, bundled, True),
                                                ("report", bundled, ordinary, False)):
            with self.subTest(is_bundle=is_bundle):
                cid = self.create("/disk1", name + ".zip", before)["container"]["container_id"]
                initial = self.checkout(cid)
                self.assertNotEqual(initial["bundle"], is_bundle)
                self.append(cid, 1, after)
                renamed = name.capitalize() + ".zip"
                t.http("PATCH", f"/containers/{cid}", 200, {"name": renamed})

                restored = self.checkout(cid, t.b, "--version", 2)
                self.assertEqual((restored["local"], restored["bundle"]),
                                 (initial["local"], is_bundle))
                self.assertEqual(t.cli(t.b, "sync")["updated"], 1)
                track = next(row for row in t.cli(t.b, "status")["entries"] if row["container_id"] == cid)
                self.assertEqual((track["path"], track["version"], track["state"]),
                                 ("disk1/" + renamed, 2, "clean"))
                destination = t.b / "disk1" / renamed
                if is_bundle:
                    self.assertEqual((destination / "note.txt").read_bytes(), b"directory bundle")
                else:
                    self.assertEqual(destination.read_bytes(), ordinary)

    def test_checkout_and_sync_delete_protect_a_local_case_variant(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        cid = self.create("/disk1", "notes.txt", b"original")["container"]["container_id"]
        expected = t.b / self.checkout(cid)["local"]
        actual = expected.rename(expected.with_name("Notes.txt"))
        if expected.exists():
            self.skipTest("requires a case-sensitive workspace filesystem")
        actual.write_bytes(b"local edit at the alternate spelling")

        self.assertIn("大小写不一致", self.failure(t.b, "sync-delete", "disk1/notes.txt"))
        self.assertIn("大小写不一致", self.failure(t.b, "checkout", cid))
        self.assertFalse(expected.exists())
        self.assertEqual(actual.read_bytes(), b"local edit at the alternate spelling")
        self.assertEqual(self.detail(cid)["container"]["latest_version"], 1)

    def test_sync_uses_temporary_retirement_but_preserves_explicit_backups(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        cid = self.create("/disk1", "report.txt", b"version one")["container"]["container_id"]
        local = t.b / self.checkout(cid)["local"]
        local.write_bytes(b"my local changes")
        saved = Path(self.checkout(cid, t.b, "--backup")["backup"])

        archive = BytesIO()
        with ZipFile(archive, "w") as zipped:
            zipped.writestr(".zxsignore", "")
            zipped.writestr("part.txt", "version one")
        bundle = self.create("/disk1", "bundle.zip", archive.getvalue())["container"]["container_id"]
        part = t.b / self.checkout(bundle)["local"] / "part.txt"
        archive = BytesIO()
        with ZipFile(archive, "w") as zipped:
            zipped.writestr(".zxsignore", "")
            zipped.writestr("part.txt", "version two")

        self.append(cid, 1, b"version two")
        self.append(bundle, 1, archive.getvalue())
        self.assertEqual(t.cli(t.b, "sync")["updated"], 2)
        self.assertEqual(local.read_bytes(), b"version two")
        self.assertEqual(part.read_bytes(), b"version two")
        self.assertEqual(saved.read_bytes(), b"my local changes")
        self.assertEqual(list(saved.parent.iterdir()), [saved])

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

    def test_directory_sync_handles_a_folder_reusing_a_renamed_file_path(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        cid = self.create("/disk1", "original", b"original")["container"]["container_id"]
        self.checkout(cid)
        repl = self.create("/disk1", "a.txt", b"renamed file")["container"]["container_id"]
        self.checkout(repl)
        t.http("PATCH", f"/containers/{cid}", 200, {"name": "renamed"})
        t.http("PATCH", f"/containers/{repl}", 200, {"name": "b.txt"})
        t.http("POST", "/folders", 201, {"folder": "/disk1/original/empty"})
        self.create("/disk1/original", "nested.txt", b"new member")
        self.create("/disk1", "a.txt", b"replacement")

        report = t.cli(t.b, "sync", "--folder", "/disk1")
        self.assertEqual((report["updated"], report["issues"]), (4, []))
        self.assertTrue((t.b / "disk1/original/empty").is_dir())
        self.assertEqual((t.b / "disk1/original/nested.txt").read_bytes(), b"new member")
        self.assertEqual((t.b / "disk1/renamed").read_bytes(), b"original")
        self.assertEqual((t.b / "disk1/a.txt").read_bytes(), b"replacement")
        self.assertEqual((t.b / "disk1/b.txt").read_bytes(), b"renamed file")

    def test_sync_resolves_a_chain_of_remote_renames_in_one_run(self):
        t = self.zxs
        t.cli(t.a, "login", t.endpoint)
        folder = "/disk1/chain"
        first = self.create(folder, "a.txt", b"first")["container"]["container_id"]
        second = self.create(folder, "b.txt", b"second")["container"]["container_id"]
        for workspace in (t.a, t.b):
            t.cli(workspace, "sync", "--folder", folder)

        t.http("PATCH", f"/containers/{second}", 200, {"name": "c.txt"})
        t.http("PATCH", f"/containers/{first}", 200, {"name": "b.txt"})
        self.create(folder, "a.txt", b"replacement")
        scoped = t.cli(t.a, "sync", "--folder", folder)
        tracked = t.cli(t.b, "refresh")
        self.assertEqual((scoped["updated"], scoped["issues"]), (3, []))
        self.assertEqual(len(tracked["entries"]), 2)
        for workspace in (t.a, t.b):
            local = workspace / "disk1/chain"
            self.assertEqual((local / "b.txt").read_bytes(), b"first")
            self.assertEqual((local / "c.txt").read_bytes(), b"second")
        self.assertEqual((t.a / "disk1/chain/a.txt").read_bytes(), b"replacement")
        self.assertFalse((t.b / "disk1/chain/a.txt").exists())

        t.http("PATCH", f"/containers/{first}", 200, {"name": "temporary.txt"})
        t.http("PATCH", f"/containers/{second}", 200, {"name": "b.txt"})
        t.http("PATCH", f"/containers/{first}", 200, {"name": "c.txt"})
        blocked = t.cli(t.b, "sync", expected=1)
        self.assertEqual((blocked["updated"], len(blocked["issues"])), (0, 2))
        self.assertEqual((t.b / "disk1/chain/b.txt").read_bytes(), b"first")
        self.assertEqual((t.b / "disk1/chain/c.txt").read_bytes(), b"second")

    def test_directory_sync_preserves_a_missing_track_when_the_path_is_reused(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        cid = self.create("/disk1", "a.txt", b"original")["container"]["container_id"]
        path = t.b / self.checkout(cid)["local"]
        path.unlink()
        t.http("PATCH", f"/containers/{cid}", 200, {"name": "b.txt"})
        self.create("/disk1", "a.txt", b"replacement")

        report = t.cli(t.b, "sync", "--folder", "/disk1", expected=1)
        self.assertEqual(report["updated"], 0)
        self.assertFalse(path.exists())
        self.assertFalse(path.with_name("b.txt").exists())
        row, = t.cli(t.b, "status")["entries"]
        self.assertEqual((row["container_id"], row["path"], row["state"]),
                         (cid, "disk1/a.txt", "missing"))

    def test_directory_sync_preserves_a_conflicting_parent_spelling(self):
        t = self.zxs
        existing = t.b / "disk1/docs"
        existing.mkdir()
        other_spelling = existing.with_name("Docs")
        if other_spelling.exists():
            self.skipTest("requires a case-sensitive workspace filesystem")
        (existing / "draft.txt").write_bytes(b"local draft")
        t.cli(t.b, "login", t.endpoint)
        cid = self.create("/disk1/Docs", "remote.txt", b"remote data")["container"]["container_id"]

        report = t.cli(t.b, "sync", "--folder", "/disk1", expected=1)
        self.assertEqual(report["updated"], 0)
        self.assertIn("大小写不一致", report["issues"][0]["reason"])
        self.assertIn("大小写不一致", self.failure(t.b, "checkout", cid))
        self.assertFalse(other_spelling.exists())
        self.assertEqual((existing / "draft.txt").read_bytes(), b"local draft")

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

    def test_directory_commit_checks_the_disk_once_for_new_bundles(self):
        t = self.zxs

        def make_bundle(path):
            path.mkdir(parents=True)
            (path / ".zxsignore").write_text("*.local\n")
            (path / "note.txt").write_text("included member")
            (path / "secret.local").write_text("ignored member")

        root = t.b / "disk1/work"
        make_bundle(root / "first.zip")
        make_bundle(root / "nested/second.zip")
        (root / "ordinary.txt").write_text("ordinary file")
        checks = []

        def trace(request, _response, _data):
            if request.command == "GET" and request.path == "/folders?folder=%2F":
                checks.append(request.path)
            return True

        with proxy_requests(t.port, trace) as endpoint:
            t.cli(t.b, "login", endpoint)
            with ui(t.b, t.config) as action:
                result = action("commit_directory", directory="disk1/work")
            self.assertEqual((result["committed"], result["issues"], len(checks)), (3, [], 1))

            checks.clear()
            standalone = t.b / "disk1/standalone.zip"
            make_bundle(standalone)
            result = t.cli(t.b, "commit", standalone)["data"]
            self.assertEqual((result["container"]["name"], len(checks)), ("standalone.zip", 1))

            checks.clear()
            unregistered = t.b / "not-a-disk/unregistered.zip"
            make_bundle(unregistered)
            t.cli(t.b, "commit", unregistered, expected=1)
            self.assertEqual(len(checks), 1)

    def test_invalid_tracked_bundle_does_not_block_other_workspace_changes(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        root = t.b / "disk1/work"
        directory = root / "project.zip"
        directory.mkdir(parents=True)
        manifest = directory / ".zxsignore"
        manifest.write_text("*.tmp\n")
        (directory / "note.txt").write_bytes(b"bundle contents")
        cid = t.cli(t.b, "commit", directory)["data"]["container"]["container_id"]
        invalid = "*.tmp\n".encode("utf-16")
        manifest.write_bytes(invalid)
        (root / "separate.txt").write_bytes(b"independent file")
        states = {"disk1/work/project.zip": "modified", "disk1/work/separate.txt": "untracked"}
        local = t.cli(t.b, "status")["entries"]
        self.assertEqual({entry["path"]: entry["state"] for entry in local}, states)

        with ui(t.b, t.config) as action:
            page = action("browse_workspace", directory="disk1/work")
            self.assertEqual({entry["path"]: entry["state"] for entry in page["entries"]}, states)
            report = action("commit_directory", directory="disk1/work")
        self.assertEqual(report["committed"], 1)
        issue, = report["issues"]
        self.assertEqual(issue["path"], "disk1/work/project.zip")
        self.assertIn(".zxsignore must be UTF-8", issue["reason"])
        self.assertEqual(manifest.read_bytes(), invalid)
        self.assertEqual(self.detail(cid)["container"]["latest_version"], 1)
        separate, = self.listing(folder="/disk1/work", q="separate")["containers"]
        payload = f"/containers/{separate['container_id']}/versions/1/payload"
        self.assertEqual(t.http("GET", payload, 200)[0], b"independent file")

        issue, = t.cli(t.b, "sync", expected=1)["issues"]
        self.assertEqual(issue["path"], "disk1/work/project.zip")
        self.assertIn(".zxsignore must be UTF-8", issue["reason"])

    def test_directory_commit_retries_a_restored_remote_without_another_sync(self):
        t = self.zxs
        folder = "/disk1/work"
        t.cli(t.b, "login", t.endpoint)
        cid = self.create(folder, "report.txt", b"old data")["container"]["container_id"]
        relative = self.checkout(cid)["local"]
        local = t.b / relative
        moved, _ = t.http("DELETE", f"/containers/{cid}", 200)
        self.assertEqual(t.cli(t.b, "sync")["remote_missing"], [relative])
        checked = local.stat()
        local.write_bytes(b"new data")
        os.utime(local, ns=(checked.st_atime_ns, checked.st_mtime_ns))
        (local.parent / "separate.txt").write_bytes(b"unrelated file")

        with ui(t.b, t.config) as action:
            report = action("commit_directory", directory="disk1/work")
            self.assertEqual(report["committed"], 1)
            issue, = report["issues"]
            self.assertEqual(issue["path"], relative)
            self.assertIn("404", issue["reason"])
            self.assertEqual(local.read_bytes(), b"new data")
            self.assertEqual(local.stat().st_mtime_ns, checked.st_mtime_ns)
            self.assertEqual(local.stat().st_size, checked.st_size)
            self.assertEqual([entry["name"] for entry in self.listing(folder=folder)["containers"]],
                             ["separate.txt"])

            rid = moved["recycle"]["recycle_id"]
            t.http("POST", f"/recycle/{rid}/restore", 200, {"folder": folder})
            report = action("commit_directory", directory="disk1/work")
            self.assertEqual((report["committed"], report["issues"]), (1, []))

        self.assertEqual(self.detail(cid)["container"]["latest_version"], 2)
        self.assertEqual(t.http("GET", f"/containers/{cid}/versions/2/payload", 200)[0], b"new data")
        row = next(entry for entry in t.cli(t.b, "status")["entries"] if entry["path"] == relative)
        self.assertEqual((row["container_id"], row["state"], row["remote_missing"]), (cid, "clean", False))

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

    def test_directory_commit_skips_both_spellings_of_conflicting_ancestors(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        tree = t.b / "disk1/Tree"
        (tree / "Docs").mkdir(parents=True)
        if (tree / "docs").exists():
            self.skipTest("requires a case-sensitive workspace filesystem")
        (tree / "Docs/Empty").mkdir()
        (tree / "docs").mkdir()
        (tree / "Docs/a.txt").write_bytes(b"one branch")
        (tree / "docs/b.txt").write_bytes(b"other branch")
        (tree / "safe.txt").write_bytes(b"independent entry")

        with ui(t.b, t.config) as action:
            report = action("commit_directory", directory="disk1/Tree")
        self.assertEqual(report["committed"], 1)
        self.assertEqual(report["created_folders"], 0)
        self.assertEqual({issue["path"] for issue in report["issues"]},
                         {"disk1/Tree/Docs/Empty", "disk1/Tree/Docs/a.txt",
                          "disk1/Tree/docs/b.txt"})
        self.assertEqual([entry["name"] for entry in
                          self.listing(folder="/disk1/Tree", recursive="true")["containers"]],
                         ["safe.txt"])

    def test_directory_commit_skips_unicode_aliases_and_submits_other_entries(self):
        t = self.zxs
        t.cli(t.b, "login", t.endpoint)
        tree = t.b / "disk1"
        composed, decomposed = tree / "Café", tree / "Cafe\u0301"
        composed.mkdir()
        if decomposed.exists():
            self.skipTest("requires distinct Unicode spellings on the workspace filesystem")
        decomposed.mkdir()
        (composed / "one.txt").write_bytes(b"first spelling")
        (decomposed / "two.txt").write_bytes(b"second spelling")
        (tree / "safe.txt").write_bytes(b"independent entry")

        with ui(t.b, t.config) as action:
            report = action("commit_directory", directory="disk1")
        self.assertEqual(report["committed"], 1)
        self.assertEqual({issue["path"] for issue in report["issues"]},
                         {"disk1/Café/one.txt", "disk1/Café/two.txt"})
        entries = self.listing(folder="/disk1", recursive="true")["containers"]
        self.assertEqual([entry["name"] for entry in entries], ["safe.txt"])
        self.assertEqual((composed / "one.txt").read_bytes(), b"first spelling")
        self.assertEqual((decomposed / "two.txt").read_bytes(), b"second spelling")

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
