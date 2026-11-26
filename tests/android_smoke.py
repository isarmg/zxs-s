"""Exercise the installed Android shell with the real Linux server."""

from io import BytesIO
from pathlib import Path
import re
from subprocess import check_output
import sys
from tempfile import TemporaryDirectory
from time import monotonic, sleep
from xml.etree import ElementTree
from zipfile import ZipFile

from smoke import PASSWORD, Test, upload_headers


PACKAGE = "local.zxs.mobile"
DUMP = "/sdcard/zxs-test-ui.xml"


def adb(*args, timeout=30):
    return check_output(["adb", *map(str, args)], encoding="utf-8", timeout=timeout).strip()


def expect_node(**attributes):
    deadline = monotonic() + 40
    while monotonic() < deadline:
        adb("shell", "uiautomator", "dump", DUMP)
        document = ElementTree.fromstring(adb("shell", "cat", DUMP))
        for node in document.iter("node"):
            if all(node.get(key) == value for key, value in attributes.items()):
                return node.attrib
        sleep(0.25)
    raise AssertionError(f"Android screen is missing {attributes}")


def tap(**attributes):
    node = expect_node(**attributes)
    left, top, right, bottom = map(int, re.findall(r"\d+", node["bounds"]))
    adb("shell", "input", "tap", (left + right) // 2, (top + bottom) // 2)


def zipped(name):
    contents = BytesIO()
    with ZipFile(contents, "w") as archive:
        archive.writestr(".zxsignore", "")
        archive.writestr(f"notes/{name}", "ZIP member")
    return contents.getvalue()


def main(apk):
    adb("install", "-r", apk, timeout=60)
    adb("shell", "am", "start", "-n", f"{PACKAGE}/.MainActivity")
    with TemporaryDirectory(prefix="zxs-android-") as root:
        t = Test(Path(root), Path(root) / "recycle")
        port = f"tcp:{t.port}"
        t.offline("init")
        adb("reverse", port, port)
        try:
            t.start()
            t.login_session()
            created = t.http("POST", "/containers", 201, zipped("offline.txt"),
                             headers=upload_headers(dict(folder="/disk1", name="workspace.zip", tags=[])))
            cid = created[0]["container"]["container_id"]
            tap(text="http://")
            adb("shell", "input", "keyevent", "KEYCODE_MOVE_END")
            adb("shell", "input", "text", f"127.0.0.1:{t.port}")
            tap(password="true", package=PACKAGE)
            adb("shell", "input", "text", PASSWORD)
            tap(text="连接")
            tap(text="disk1")
            tap(text="workspace.zip")
            expect_node(text="notes")
            t.stop()
            tap(text="notes")
            expect_node(text="offline.txt")
            t.start()
            t.login_session()
            t.http("POST", f"/containers/{cid}/versions", 201, zipped("reconnected.txt"),
                   headers=upload_headers({"expected_version": 1}))
            tap(**{"content-desc": "刷新"})
            expect_node(text="reconnected.txt")
        finally:
            t.stop()
            adb("reverse", "--remove", port)
    print("Android login, cached ZIP browsing and server reconnection passed")


if __name__ == "__main__":
    main(sys.argv[1])
