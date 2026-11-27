"""The independent preparation tool and service use the same disk and path rules."""

from io import BytesIO
import json
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest import TestCase
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
