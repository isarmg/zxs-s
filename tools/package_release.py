#!/usr/bin/env python3
"""Package the native release binaries for a single ZXS target."""

import argparse
import os
from pathlib import Path
import subprocess
import tarfile
from tempfile import TemporaryDirectory
import tomllib
from zipfile import ZIP_DEFLATED, ZipFile


ROOT = Path(__file__).resolve().parents[1]
LINUX = "x86_64-unknown-linux-gnu"
WINDOWS = "x86_64-pc-windows-msvc"
MACOS = "aarch64-apple-darwin"
PREPARE = "tools/zxs-prepare"
PRODUCTS = {
    "client": ("zxs-client", "zxs-cli"),
    "server": ("zxs-server", "zxs"),
    "prepare": (PREPARE, "zxs-prepare"),
}


def output(*args):
    return subprocess.check_output(args, cwd=ROOT, encoding="utf-8").strip()


def binary(project, target, name, version):
    if target == WINDOWS:
        name += ".exe"
    path = ROOT / project / "target" / target / "release" / name
    actual = output(path, "--version").rsplit(maxsplit=1)[-1]
    if actual != version:
        raise SystemExit(f"{name} has version {actual}, expected {version}")
    return path


def version_of(project):
    manifest = tomllib.loads((ROOT / project / "Cargo.toml").read_text(encoding="utf-8"))
    return manifest.get("workspace", manifest)["package"]["version"]


def package(target, product):
    if (target == WINDOWS and product != "prepare") or (product == "server" and target != LINUX):
        raise SystemExit(f"{product} does not use this packager for {target}")
    project, executable = PRODUCTS[product]
    version = version_of(project)
    if os.environ.get("GITHUB_REF_TYPE") == "tag":
        tag = os.environ["GITHUB_REF_NAME"]
        tags = {f"v{version}"}
        if product == "client":
            tags.add(f"client-v{version}")
        if product == "prepare":
            tags = {f"prepare-v{version}"}
        if tag not in tags:
            raise SystemExit(f"release tag must match the {project} Cargo version {version}")
    package_name = "zxs-server" if product == "server" else executable
    stem = f"{package_name}-{version}-{target}"
    readme = ROOT / PREPARE / "README.md" if product == "prepare" else ROOT / "README.md"
    program = binary(project, target, executable, version)
    files = [(program, program.name), (readme, "README.md"), (ROOT / "LICENSE", "LICENSE")]
    if product == "server":
        prepare = binary(PREPARE, target, "zxs-prepare", version_of(PREPARE))
        files.extend([(prepare, prepare.name), (ROOT / PREPARE / "README.md", "PREPARE.md")])
    suffix = ".zip" if target == WINDOWS else ".tar.gz"
    archive = ROOT / "dist" / f"{stem}{suffix}"
    archive.parent.mkdir(exist_ok=True)
    with TemporaryDirectory(prefix="zxs-release-") as work:
        metadata = Path(work) / "BUILD.txt"
        metadata.write_text(
            f"version={version}\ncommit={output('git', 'rev-parse', 'HEAD')}\n"
            f"target={target}\ncompiler={output('rustc', '--version')}\n",
            encoding="utf-8",
            newline="\n",
        )
        files.append((metadata, metadata.name))
        if target == WINDOWS:
            with ZipFile(archive, "w", ZIP_DEFLATED) as compressed:
                for file, name in files:
                    compressed.write(file, f"{stem}/{name}")
        else:
            with tarfile.open(archive, "w:gz") as compressed:
                for file, name in files:
                    compressed.add(file, f"{stem}/{name}")
    print(archive.name)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=(LINUX, WINDOWS, MACOS))
    products = parser.add_mutually_exclusive_group()
    products.add_argument("--server", dest="product", action="store_const", const="server",
                          help="package the server with the disk preparation tool")
    products.add_argument("--prepare", dest="product", action="store_const", const="prepare",
                          help="package the standalone disk preparation tool")
    parser.set_defaults(product="client")
    args = parser.parse_args()
    package(args.target, args.product)
