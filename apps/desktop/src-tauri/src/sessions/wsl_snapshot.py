"""Create read-only, SQLite-managed session snapshots inside the selected WSL.

Invoked with fixed argv by Codex-X; no shell or account credentials are used.
The source is opened by Linux SQLite so WAL and native locks stay intact.
"""
import argparse
from contextlib import closing
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


class SnapshotError(Exception):
    def __init__(self, kind, message):
        super().__init__(message)
        self.kind = kind


def table_names(path):
    try:
        with closing(sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, timeout=1.0)) as source:
            return {row[0] for row in source.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    except sqlite3.Error as error:
        raise SnapshotError("database", "{}: {}".format(path, error)) from error


def snapshot(home, output, sqlite_home=None):
    deadline = time.monotonic() + 30

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
            candidates.append((priority, -int(version.group(1)) if version else 1, path.name != "codex-dev.db", path))

    # Select the current index before opening old indexes or optional title
    # catalogs. A broken retired state must not hide healthy current sessions;
    # a broken highest-priority state must never silently fall back to old data.
    candidates.sort()
    selected = None
    inspected = {}
    for _, _, _, path in candidates:
        check_deadline()
        tables = table_names(path)
        inspected[path] = tables
        if "threads" in tables:
            selected = path
            break
        if re.fullmatch(r"state_\d+\.sqlite", path.name):
            raise SnapshotError("database", "{}: active state database has no threads table".format(path))

    warnings = []
    manifest = {"databases": [], "active": None}

    def copy_snapshot(path):
        check_deadline()
        name = "snapshot-{}.sqlite".format(len(manifest["databases"]))
        destination = output / name
        created = False
        try:
            with closing(sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, timeout=1.0)) as source:
                # Restrict new snapshots to this account; never overwrite an
                # existing output or change any source database/sidecar.
                descriptor = os.open(str(destination), os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
                created = True
                os.close(descriptor)
                with closing(sqlite3.connect(str(destination))) as target:
                    def progress(status, remaining, total):
                        check_deadline()
                    source.backup(target, pages=256, progress=progress, sleep=0.05)
        except Exception:
            if created:
                for suffix in ("", "-wal", "-shm", "-journal"):
                    try:
                        Path(str(destination) + suffix).unlink()
                    except FileNotFoundError:
                        pass
            raise
        manifest["databases"].append(name)
        return name

    # Finish the required snapshot first. A slow/broken optional catalog cannot
    # consume the current index's budget or discard an already valid list.
    if selected:
        try:
            manifest["active"] = copy_snapshot(selected)
        except sqlite3.Error as error:
            raise SnapshotError("database", "{}: {}".format(selected, error)) from error

    for _, _, _, path in candidates:
        if path == selected:
            continue
        try:
            check_deadline()
            tables = inspected[path] if path in inspected else table_names(path)
            # Retired indexes may still contain unique display names, but their
            # conversation rows never replace the current index.
            if "local_thread_catalog" in tables:
                copy_snapshot(path)
        except TimeoutError:
            warnings.append("会话列表已读取，但补充名称读取超时，部分名称可能使用已有标题。")
            break
        except (SnapshotError, sqlite3.Error, OSError) as error:
            detail = str(error) if isinstance(error, SnapshotError) else "{}: {}".format(path, error)
            warnings.append("部分会话将使用已有标题，补充名称读取失败：{}".format(detail))
    if warnings:
        manifest["warnings"] = warnings

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
        kind = getattr(error, "kind", type(error).__name__)
        # The caller can distinguish a broken database from a missing Python
        # interpreter or unavailable distribution without parsing translated OS text.
        print(json.dumps({"error": {"kind": kind, "message": str(error)}}))
        print("WSL session snapshot: {}: {}".format(type(error).__name__, error), file=sys.stderr)
        sys.exit(1)
