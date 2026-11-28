"""The independent preparation tool and service use the same disk and path rules."""

from io import BytesIO
import json
from pathlib import Path
from shutil import which
from subprocess import run
from tempfile import TemporaryDirectory
from unittest import TestCase, skipUnless
from zipfile import ZipFile

from smoke import PREPARE, Test


class DiskPreparation(TestCase):
    def setUp(self):
        root = Path(self.enterContext(TemporaryDirectory(prefix="zxs-prepare-handoff-")))
        self.zxs = Test(root, root / "recycle")
        self.addCleanup(self.zxs.stop)
        self.zxs.offline("init")

    def test_sync_waits_for_preparation_to_publish_the_disk_identity(self):
        t = self.zxs
        disk = t.data / "projects"
        (disk / "Album").mkdir(parents=True)
        (disk / "Album/photo.jpg").write_bytes(b"photo")
        (disk / "notes.txt").write_bytes(b"text")
        report = json.loads(t.local(PREPARE, ["prepare", disk, "--bundle", "Album"]))
        system = disk / ".zxs"
        work = system / "prepare"
        stage = work / "tree"
        stage.mkdir()
        marker = system / "disk.toml"
        identity = marker.read_bytes()
        pending = work / "pending.json"
        # Reconstruct the on-disk state just after backing up the originals.
        marker.unlink()
        for name in ("Album.zip", "notes.txt"):
            (disk / name).rename(stage / name)
        pending.write_text(json.dumps({
            "kind": "data", "disk_id": report["disk_id"],
            "originals": ["Album", "notes.txt"], "published": ["Album.zip", "notes.txt"],
            "containers": 2,
        }))
        self.assertIn("No new disks", t.offline("sync-disks"))
        self.assertFalse(marker.exists())
        self.assertTrue(pending.is_file())
        self.assertEqual((system / "originals/notes.txt").read_bytes(), b"text")

        resumed = json.loads(t.local(PREPARE, ["resume", disk]))
        self.assertEqual(resumed["disk_id"], report["disk_id"])
        marker.chmod(0o444)
        self.assertIn("Disk registrations updated", t.offline("sync-disks"))
        self.assertEqual(marker.read_bytes(), identity)
        self.assertIn("Verified 2 versions", t.offline("verify"))
        t.start()
        t.login_session()
        entries = t.http("GET", "/containers?folder=/projects", 200)[0]["containers"]
        self.assertEqual([entry["name"] for entry in entries], ["Album.zip", "notes.txt"])
        payloads = [t.http("GET", f"/containers/{entry['container_id']}/versions/1/payload", 200)[0]
                    for entry in entries]
        self.assertEqual(payloads[1], b"text")
        with ZipFile(BytesIO(payloads[0])) as archive:
            self.assertEqual(archive.namelist()[0], ".zxsignore")
            self.assertEqual(archive.read("photo.jpg"), b"photo")

    @skipUnless(which("strace"), "strace is required to observe source payload reads")
    def test_output_name_collision_precedes_copying_siblings(self):
        t = self.zxs
        disk = t.root / "conflict"
        directory = disk / "Manual"
        directory.mkdir(parents=True)
        raw = directory / "large.raw"
        size = 32 * 1024 * 1024
        with raw.open("wb") as stream:
            stream.truncate(size)
        existing = disk / "Manual.zip"
        with ZipFile(existing, "w") as archive:
            archive.writestr("notes.txt", b"existing archive")
        original = existing.read_bytes()

        trace = t.root / "collision.trace"
        result = run(["strace", "-qq", "-y", "-s", "0", "-e", "trace=read",
                      "-o", trace, PREPARE, "prepare", disk, "--bundle", directory.name],
                     env=t.env, capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("输出名称冲突 Manual.zip 生成 Manual.zip", result.stderr)
        self.assertFalse(any(f"<{raw}>" in call for call in trace.read_text().splitlines()),
                         "prepare read the payload before rejecting the output name collision")
        self.assertEqual(raw.stat().st_size, size)
        self.assertEqual(existing.read_bytes(), original)
        self.assertFalse((disk / ".zxs/disk.toml").exists())
        self.assertFalse((disk / ".zxs/prepare/tree").exists())

        t.local(PREPARE, ["prepare", disk])
        self.assertEqual((raw / "v000001/large.raw").stat().st_size, size)
        self.assertEqual((existing / "v000001/Manual.zip").read_bytes(), original)

    @skipUnless(which("strace"), "strace is required for directory fsync fault injection")
    def test_prepare_and_resume_sync_the_pending_record_before_moving_originals(self):
        t = self.zxs
        trace = t.root / "prepare.trace"
        ptrace = ["-qq", "-y", "-e", "trace=fsync,rename,renameat,renameat2", "-o", trace]

        def traced(command, disk, fail_at=None):
            inject = [] if fail_at is None else ["-e", f"inject=fsync:error=EIO:when={fail_at}"]
            output = t.local("strace", [*ptrace, *inject, PREPARE, command, disk],
                             expected=0 if fail_at is None else 1)
            return output, trace.read_text().splitlines()

        def verify_sync_order(disk, calls):
            work = disk / ".zxs/prepare"
            sync = next(i for i, call in enumerate(calls) if
                        call.startswith("fsync(") and f"<{work}>" in call)
            backup = next(i for i, call in enumerate(calls) if
                          call.startswith("rename") and "/.zxs/originals/notes.txt" in call)
            self.assertLess(sync, backup)
            return sum(call.startswith("fsync(") for call in calls[:sync + 1])

        baseline = t.root / "baseline"
        disk = t.root / "interrupted"
        for directory in (baseline, disk):
            directory.mkdir()
            (directory / "notes.txt").write_bytes(b"original")

        _, calls = traced("prepare", baseline)
        first_work_sync = verify_sync_order(baseline, calls)
        _, calls = traced("prepare", disk, fail_at=first_work_sync)
        self.assertTrue(any("(INJECTED)" in call for call in calls))
        pending = disk / ".zxs/prepare/pending.json"
        disk_id = json.loads(pending.read_text())["disk_id"]
        self.assertEqual((disk / "notes.txt").read_bytes(), b"original")

        _, calls = traced("resume", disk, fail_at=1)
        self.assertTrue(any("(INJECTED)" in call for call in calls))
        self.assertEqual((disk / "notes.txt").read_bytes(), b"original")
        self.assertTrue(pending.exists())
        self.assertFalse((disk / ".zxs/disk.toml").exists())

        output, calls = traced("resume", disk)
        verify_sync_order(disk, calls)
        self.assertEqual(json.loads(output)["disk_id"], disk_id)
        self.assertEqual((disk / "notes.txt/v000001/notes.txt").read_bytes(), b"original")
        self.assertEqual((disk / ".zxs/originals/notes.txt").read_bytes(), b"original")

    def test_path_limit_counts_the_final_mount_name_and_utf8_bytes(self):
        t = self.zxs
        mount = "projects"
        parent = "/".join(["文" * 80] * 4)
        prefix = f"/{mount}/{parent}/"
        (t.data / mount).mkdir()
        t.prepare_disk(t.data / mount, "data")
        t.offline("sync-disks")
        t.start()
        t.login_session()
        t.cli(t.a, "login", t.endpoint)
        for length in (1024, 1025):
            with self.subTest(length=length):
                leaf = "p" * (length - len(prefix.encode()) - 4) + ".zip"
                relative = f"{parent}/{leaf}"
                raw_disk = t.root / str(length) / "d"
                selected = relative.removesuffix(".zip")
                directory = raw_disk / selected
                directory.mkdir(parents=True)
                valid = length == 1024
                t.local(PREPARE, ["prepare", raw_disk, "--mount-name", mount, "--bundle", selected],
                        expected=0 if valid else 1)
                self.assertEqual((raw_disk / ".zxs/disk.toml").exists(), valid)
                if valid:
                    self.assertTrue((raw_disk / relative / "v000001" / leaf).is_file())
                else:
                    self.assertTrue(directory.is_dir())
                folder = f"/{mount}/{relative}"
                self.assertEqual(len(folder.encode()), length)
                t.http("POST", "/folders", 201 if valid else 400, {"folder": folder})
                t.cli(t.a, "mkdir", folder, expected=0 if valid else 2)
