"""Create read-only, SQLite-managed session snapshots inside the selected WSL.

Invoked with fixed argv by Codex-X; no shell or account credentials are used.
The source is opened by Linux SQLite so WAL and native locks stay intact.
"""
import argparse
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import sys
import time


def contained(path, root):
    return os.path.commonpath((str(path), str(root))) == str(root)


def snapshot(home, output, sqlite_home=None):
    deadline = time.monotonic() + 12

    def check_deadline():
        if time.monotonic() > deadline:
            raise TimeoutError("WSL session snapshot timed out; close active Codex tasks and retry")

    home = Path(home).resolve(strict=True)
    output = Path(output).resolve(strict=True)
    if not home.is_dir() or not output.is_dir():
        raise ValueError("CODEX_HOME or snapshot directory does not exist")
    configured = sqlite_home or os.environ.get("CODEX_SQLITE_HOME")
    roots = []
    if configured:
        configured = Path(os.path.expanduser(configured))
        configured = configured if configured.is_absolute() else home / configured
        configured = configured.resolve(strict=True)
        if not contained(configured, home):
            raise ValueError("WSL preview requires sqlite_home to be inside the selected CODEX_HOME")
        roots.append((configured, 0, True))
    roots.extend(((home, 1 if configured else 0, False), (home / "sqlite", 2, True)))
    candidates = []
    catalogs = []
    seen = set()
    for root, priority, custom in roots:
        if not root.is_dir():
            continue
        for path in sorted(root.iterdir()):
            check_deadline()
            if path.is_symlink() or not path.is_file() or path.suffix.lower() not in (".sqlite", ".db", ".sqlite3"):
                continue
            version = re.fullmatch(r"state_(\d+)\.sqlite", path.name)
            if not custom and not version and path.name != "codex-dev.db":
                continue
            path = path.resolve(strict=True)
            if path in seen or not contained(path, home):
                continue
            seen.add(path)
            source = sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, timeout=1.0)
            try:
                tables = {row[0] for row in source.execute("SELECT name FROM sqlite_master WHERE type='table'")}
            finally:
                source.close()
            if "threads" in tables:
                candidates.append((priority, -int(version.group(1)) if version else 1, path.name != "codex-dev.db", path))
            if "local_thread_catalog" in tables:
                catalogs.append(path)
    selected = min(candidates)[3] if candidates else None
    paths = ([selected] if selected else []) + [path for path in catalogs if path != selected]
    manifest = {"databases": [], "active": None}
    for index, path in enumerate(paths):
        check_deadline()
        name = "snapshot-{}.sqlite".format(index)
        destination = output / name
        source = sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, timeout=1.0)
        # Restrict newly created snapshots to the current account on Linux.
        descriptor = os.open(str(destination), os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        os.close(descriptor)
        target = sqlite3.connect(str(destination))
        try:
            def progress(status, remaining, total):
                check_deadline()
            source.backup(target, pages=256, progress=progress, sleep=0.05)
        finally:
            source.close()
            target.close()
        manifest["databases"].append(name)
        if path == selected:
            manifest["active"] = name
    return manifest


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--home", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--sqlite-home")
    parser.add_argument("--windows-output", action="store_true")
    arguments = parser.parse_args()
    output = arguments.output
    if arguments.windows_output:
        output = subprocess.run(["wslpath", "-a", "-u", arguments.output], check=True, capture_output=True, text=True, timeout=3).stdout.strip()
    print(json.dumps(snapshot(arguments.home, output, arguments.sqlite_home)))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print("WSL session snapshot: {}: {}".format(type(error).__name__, error), file=sys.stderr)
        sys.exit(1)
