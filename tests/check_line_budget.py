"""Check the production source limits for the server and client."""

from pathlib import Path
import os


ROOT = Path(__file__).resolve().parent.parent
LIMITS = {
    "zxs-server": (5_500, ("src",)),
    "zxs-client": (12_000, ("apps", "crates")),
}
# One patched tiny_http line and eight lines in the client's vendor refresh rule.
LOCAL_VENDOR_FIX_LINES = {"zxs-client": 9}
SOURCE_SUFFIXES = {".rs", ".js", ".css", ".html", ".java", ".swift", ".c", ".cpp", ".h"}
SKIP_DIRECTORIES = {"target", "build", ".gradle", ".cxx", "tests", "vendor"}


def source_files(directories):
    for directory in directories:
        for current, subdirectories, files in os.walk(directory):
            subdirectories[:] = [name for name in subdirectories if name not in SKIP_DIRECTORIES]
            for name in files:
                path = Path(current) / name
                if path.suffix in SOURCE_SUFFIXES and path.stem not in {"tests", "test_support"}:
                    yield path


def production_lines(path):
    lines = iter(path.read_text(encoding="utf-8").splitlines())
    comment_end = None
    for raw in lines:
        if path.suffix == ".rs" and raw == "#[cfg(test)]":
            raw = next(lines, "")
            if raw.startswith("mod "):
                if raw.endswith("{"):
                    for raw in lines:
                        if raw == "}":
                            break
                continue
        line = raw.strip()
        if comment_end:
            _, found, line = line.partition(comment_end)
            if not found:
                continue
            comment_end = None
            line = line.strip()
        for start, end in (("/*", "*/"), ("<!--", "-->")):
            if line.startswith(start):
                _, found, line = line.partition(end)
                comment_end = None if found else end
                line = line.strip()
        if line and not line.startswith("//"):
            yield line


def check():
    failed = False
    for project, (limit, directories) in LIMITS.items():
        paths = source_files(ROOT / project / directory for directory in directories)
        count = sum(sum(1 for _ in production_lines(path)) for path in paths)
        count += LOCAL_VENDOR_FIX_LINES.get(project, 0)
        print(f"{project}: {count:,} / {limit:,} production lines", flush=True)
        failed |= count > limit
    return not failed


if __name__ == "__main__":
    raise SystemExit(0 if check() else 1)
