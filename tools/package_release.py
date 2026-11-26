#!/usr/bin/env python3
"""Package the native release binaries for a single ZXS target."""

import argparse
import os
from pathlib import Path
import subprocess
import tarfile
from tempfile import TemporaryDirectory
import tomllib


ROOT = Path(__file__).resolve().parents[1]
TARGETS = (
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
)


def output(*args):
    return subprocess.check_output(args, cwd=ROOT, encoding="utf-8").strip()


def binary(project, target, name, version):
    path = ROOT / project / "target" / target / "release" / name
    actual = output(path, "--version").rsplit(maxsplit=1)[-1]
    if actual != version:
        raise SystemExit(f"{name} has version {actual}, expected {version}")
    return path


def package(target, server):
    manifest = (ROOT / "zxs-client/Cargo.toml").read_text(encoding="utf-8")
    version = tomllib.loads(manifest)["workspace"]["package"]["version"]
    if os.environ.get("GITHUB_REF_TYPE") == "tag" and os.environ["GITHUB_REF_NAME"] != f"v{version}":
        raise SystemExit(f"release tag must match the Cargo version v{version}")
    project = "zxs-server" if server else "zxs-client"
    package_name = "zxs-server" if server else "zxs-cli"
    executable = "zxs" if server else "zxs-cli"
    stem = f"{package_name}-{version}-{target}"
    files = [binary(project, target, executable, version), ROOT / "README.md", ROOT / "LICENSE"]
    archive = ROOT / "dist" / f"{stem}.tar.gz"
    archive.parent.mkdir(exist_ok=True)
    with TemporaryDirectory(prefix="zxs-release-") as work:
        metadata = Path(work) / "BUILD.txt"
        metadata.write_text(
            f"version={version}\ncommit={output('git', 'rev-parse', 'HEAD')}\n"
            f"target={target}\ncompiler={output('rustc', '--version')}\n",
            encoding="utf-8",
            newline="\n",
        )
        files.append(metadata)
        with tarfile.open(archive, "w:gz") as compressed:
            for file in files:
                compressed.add(file, f"{stem}/{file.name}")
    print(archive.name)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=TARGETS)
    parser.add_argument("--server", action="store_true", help="package the server executable")
    args = parser.parse_args()
    package(args.target, args.server)
