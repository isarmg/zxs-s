#!/usr/bin/env python3
"""Package the native release binaries for a single ZXS target."""

import argparse
from pathlib import Path
import subprocess
import tarfile
from tempfile import TemporaryDirectory
import tomllib
from zipfile import ZIP_DEFLATED, ZipFile


ROOT = Path(__file__).resolve().parents[1]
TARGETS = (
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
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


def package(target):
    manifest = (ROOT / "zxs-client/Cargo.toml").read_text(encoding="utf-8")
    version = tomllib.loads(manifest)["workspace"]["package"]["version"]
    linux = target.endswith("-linux-gnu")
    windows = target.endswith("-msvc")
    stem = f"{'zxs' if linux else 'zxs-cli'}-{version}-{target}"
    cli = "zxs-cli.exe" if windows else "zxs-cli"
    files = [binary("zxs-client", target, cli, version), ROOT / "README.md"]
    if linux:
        files.insert(0, binary("zxs-server", target, "zxs", version))
    archive = ROOT / "dist" / (stem + (".zip" if windows else ".tar.gz"))
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
        if windows:
            with ZipFile(archive, "w", ZIP_DEFLATED) as zipped:
                for file in files:
                    zipped.write(file, f"{stem}/{file.name}")
        else:
            with tarfile.open(archive, "w:gz") as zipped:
                for file in files:
                    zipped.add(file, f"{stem}/{file.name}")
    print(archive.name)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=TARGETS)
    package(parser.parse_args().target)
