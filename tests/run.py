#!/usr/bin/env python3
"""Run the ZXS checks by layer using the existing platform tools."""

import argparse
import os
from pathlib import Path
import subprocess
import sys
from tempfile import gettempdir


ROOT = Path(__file__).resolve().parent.parent
LINUX = sys.platform.startswith("linux")
SUITES = ("rust", "integration", "desktop", "browser", "android")


def command(arguments, *, cwd=ROOT, env=None):
    names = [str(arg) for arg in arguments]
    print("\n$ " + " ".join(names), flush=True)
    subprocess.run(names, cwd=cwd, env=env, check=True)


def rust(_options, _env):
    projects = ("zxs-server", "zxs-client", "tools/zxs-prepare")
    for project in projects:
        command(["cargo", "fmt", "--all", "--", "--check"], cwd=ROOT / project)
    command([sys.executable, "tests/check_line_budget.py"])
    for project in projects if LINUX else projects[1:]:
        cwd = ROOT / project
        if project != "tools/zxs-prepare":
            command([sys.executable, "tools/sync_vendor.py", "--check"], cwd=cwd)
        command(["cargo", "clippy", "--offline", "--locked", "--all-targets", "--all-features", "--",
                 "-D", "clippy::pedantic", "-D", "warnings"], cwd=cwd)
        command(["cargo", "test", "--offline", "--locked", "--workspace"], cwd=cwd)
        command(["cargo", "build", "--offline", "--locked", "--workspace"], cwd=cwd)


def integration(options, env):
    command([sys.executable, "-m", "unittest", "discover", "-s", "tests", "-v"], env=env)
    command([sys.executable, "tests/smoke.py"], env=env)
    if options.recycle_parent is not None:
        cross = env | {"ZXS_SMOKE_RECYCLE_PARENT": str(options.recycle_parent.resolve())}
        command([sys.executable, "tests/regressions.py"], env=cross)
        command([sys.executable, "tests/smoke.py"], env=cross)
    else:
        command([sys.executable, "tests/regressions.py"], env=env)


def desktop(_options, env):
    if os.name != "nt":
        command([sys.executable, "tests/workspace_selection.py"], env=env)
    command([sys.executable, "tests/platform_history.py"], env=env)


def browser(_options, env):
    command(["node", "--test", "tests/browser.mjs", "tests/browser_recycle.mjs"], env=env)


def android(options, env):
    command([sys.executable, "tests/android_smoke.py", options.android_apk.resolve()], env=env)


def test_environment():
    env = dict(os.environ, PYTHONUTF8="1", PYTHONDONTWRITEBYTECODE="1")
    suffix = ".exe" if os.name == "nt" else ""
    library = "zxs_ffi.dll" if os.name == "nt" else "libzxs_ffi.dylib" if sys.platform == "darwin" else "libzxs_ffi.so"
    for variable, project, artifact in [
        ("ZXS_SERVER_BIN", "zxs-server", "zxs" + suffix),
        ("ZXS_CLIENT_BIN", "zxs-client", "zxs-cli" + suffix),
        ("ZXS_PREPARE_BIN", "tools/zxs-prepare", "zxs-prepare" + suffix),
        ("ZXS_FFI_LIB", "zxs-client", library),
    ]:
        target = Path(env.get("CARGO_TARGET_DIR", "target"))
        env.setdefault(variable, str(ROOT / project / target / "debug" / artifact))
    env.pop("ZXS_SMOKE_RECYCLE_PARENT", None)
    return env


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--suite", choices=SUITES, action="append", help="choose layers, repeat for more than one")
    parser.add_argument("--recycle-parent", type=Path, help="temporary recycle parent on a second filesystem")
    parser.add_argument("--android-apk", type=Path, help="debug APK matching a connected Android test device")
    options = parser.parse_args()
    suites = options.suite or (["rust", "integration", "desktop", "browser"] if LINUX else ["rust", "desktop"])
    if not LINUX and any(suite in ("integration", "android") for suite in suites):
        parser.error("integration and Android tests use the real Linux server")
    if "android" in suites and (options.android_apk is None or not options.android_apk.is_file()):
        parser.error("Android tests require an existing --android-apk")
    if options.recycle_parent is not None and (
        not options.recycle_parent.is_dir() or options.recycle_parent.stat().st_dev == Path(gettempdir()).stat().st_dev
    ):
        parser.error("--recycle-parent must be on a different filesystem from the temporary workspace")
    env = test_environment()
    actions = dict(rust=rust, integration=integration, desktop=desktop, browser=browser, android=android)
    for suite in dict.fromkeys(suites):
        print(f"\nRunning {suite} tests", flush=True)
        actions[suite](options, env)
    print("\nSelected ZXS test suites passed.")


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        sys.exit(error.returncode)
