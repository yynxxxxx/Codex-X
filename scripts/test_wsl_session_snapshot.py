"""Exercise the WSL helper's native SQLite operations without a WSL install."""
import importlib.util
from contextlib import closing
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

HELPER_PATH = Path(__file__).resolve().parents[1] / "apps/desktop/src-tauri/src/sessions/wsl_snapshot.py"
SPEC = importlib.util.spec_from_file_location("wsl_snapshot", HELPER_PATH)
HELPER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HELPER)


class WslSessionSnapshotTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="codex-x-wsl-snapshot-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.home = self.root / "Codex Home"
        self.output = self.root / "Snapshots"
        self.home.mkdir()
        self.output.mkdir()
        self.environment = patch.dict(os.environ, {"CODEX_SQLITE_HOME": ""})
        self.environment.start()
        self.addCleanup(self.environment.stop)

    def database(self, path, title, wal=False):
        path.parent.mkdir(parents=True, exist_ok=True)
        connection = sqlite3.connect(path)
        self.addCleanup(connection.close)
        if wal:
            self.assertEqual(connection.execute("PRAGMA journal_mode=WAL").fetchone()[0], "wal")
        connection.execute("CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT)")
        connection.execute("INSERT INTO threads VALUES ('fixture',?)", (title,))
        connection.commit()
        return connection

    def test_reads_committed_wal_data_via_backup_without_changing_the_source(self):
        source = self.home / "state_5.sqlite"
        writer = self.database(source, "Before WAL", wal=True)
        writer.execute("UPDATE threads SET title='Committed WAL title'")
        writer.commit()
        writer.execute("INSERT INTO threads VALUES ('uncommitted','must stay private')")
        self.assertTrue(Path(str(source) + "-wal").exists())
        manifest = HELPER.snapshot(self.home, self.output)
        self.assertEqual(manifest, {"databases": ["snapshot-0.sqlite"], "active": "snapshot-0.sqlite"})
        snapshot = sqlite3.connect(self.output / manifest["active"])
        try:
            self.assertEqual(snapshot.execute("SELECT id,title FROM threads").fetchall(), [("fixture", "Committed WAL title")])
        finally:
            snapshot.close()
        writer.rollback()
        self.assertEqual(writer.execute("SELECT title FROM threads").fetchone()[0], "Committed WAL title")
        self.assertEqual(writer.execute("PRAGMA journal_mode").fetchone()[0], "wal")

    def test_current_highest_state_version_wins_over_legacy_index(self):
        self.database(self.home / "state_5.sqlite", "Old")
        self.database(self.home / "state_10.sqlite", "Current")
        self.database(self.home / "sqlite/state_99.sqlite", "Legacy")
        manifest = HELPER.snapshot(self.home, self.output)
        with closing(sqlite3.connect(self.output / manifest["active"])) as snapshot:
            self.assertEqual(snapshot.execute("SELECT title FROM threads").fetchone()[0], "Current")

    def test_explicit_home_inside_selected_directory_and_catalog_are_snapshotted(self):
        self.database(self.home / "state_5.sqlite", "Default")
        self.database(self.home / "custom/state.sqlite3", "Configured")
        with closing(sqlite3.connect(self.home / "codex-dev.db")) as catalog:
            catalog.execute("CREATE TABLE local_thread_catalog (thread_id TEXT, display_title TEXT)")
            catalog.execute("INSERT INTO local_thread_catalog VALUES ('fixture','Renamed')")
            catalog.commit()
        manifest = HELPER.snapshot(self.home, self.output, "custom")
        self.assertEqual(len(manifest["databases"]), 2)
        with closing(sqlite3.connect(self.output / manifest["active"])) as snapshot:
            self.assertEqual(snapshot.execute("SELECT title FROM threads").fetchone()[0], "Configured")

    def test_outside_configured_home_is_rejected_before_any_backup(self):
        outside = self.root / "Other Account"
        self.database(outside / "state_5.sqlite", "Outside")
        with self.assertRaisesRegex(ValueError, "inside the selected CODEX_HOME"):
            HELPER.snapshot(self.home, self.output, str(outside))
        self.assertEqual(list(self.output.iterdir()), [])

    def test_missing_source_is_not_created_and_invalid_database_is_not_silently_empty(self):
        missing = self.root / "Missing"
        with self.assertRaises(FileNotFoundError):
            HELPER.snapshot(missing, self.output)
        self.assertFalse(missing.exists())
        (self.home / "state_5.sqlite").write_bytes(b"not a database")
        with self.assertRaisesRegex(HELPER.SnapshotError, "state_5.sqlite"):
            HELPER.snapshot(self.home, self.output)
        self.assertEqual(list(self.output.iterdir()), [])

    def test_issue76_layout_reads_current_wal_despite_broken_old_and_unrelated_databases(self):
        # Match the reported WSL .codex layout. Side databases and retired
        # versions are not authorities for the current conversation list.
        for name in ("goals_1.sqlite", "logs_2.sqlite", "memories_1.sqlite", "state_4.sqlite"):
            (self.home / name).write_bytes(b"unreadable unrelated or retired database")
        writer = self.database(self.home / "state_5.sqlite", "Original", wal=True)
        writer.execute("UPDATE threads SET title='Current WAL session'")
        writer.commit()
        with patch.object(HELPER, "table_names", wraps=HELPER.table_names) as inspect:
            manifest = HELPER.snapshot(self.home, self.output)
        self.assertEqual([call.args[0].name for call in inspect.call_args_list], ["state_5.sqlite", "state_4.sqlite"])
        with closing(sqlite3.connect(self.output / manifest["active"])) as snapshot:
            self.assertEqual(snapshot.execute("SELECT title FROM threads").fetchone()[0], "Current WAL session")
        self.assertEqual(len(manifest["warnings"]), 1)
        self.assertIn("state_4.sqlite", manifest["warnings"][0])
        self.assertEqual(writer.execute("PRAGMA journal_mode").fetchone()[0], "wal")

    def test_old_state_catalog_preserves_names_without_replacing_current_sessions(self):
        old = self.database(self.home / "state_4.sqlite", "Stale")
        old.execute("CREATE TABLE local_thread_catalog (thread_id TEXT, display_title TEXT)")
        old.execute("INSERT INTO local_thread_catalog VALUES ('fixture','Retained name')")
        old.commit()
        self.database(self.home / "state_5.sqlite", "Current")
        manifest = HELPER.snapshot(self.home, self.output)
        self.assertEqual(manifest["databases"], ["snapshot-0.sqlite", "snapshot-1.sqlite"])
        with closing(sqlite3.connect(self.output / manifest["active"])) as active:
            self.assertEqual(active.execute("SELECT title FROM threads").fetchone()[0], "Current")
        with closing(sqlite3.connect(self.output / "snapshot-1.sqlite")) as catalog:
            self.assertEqual(catalog.execute("SELECT display_title FROM local_thread_catalog").fetchone()[0], "Retained name")

    def test_optional_catalog_timeout_keeps_current_snapshot_and_cleans_partial_file(self):
        self.database(self.home / "state_5.sqlite", "Current")
        with closing(sqlite3.connect(self.home / "codex-dev.db")) as catalog:
            catalog.execute("CREATE TABLE local_thread_catalog (thread_id TEXT, display_title TEXT)")
        with self.fail_backup(TimeoutError("fixture optional deadline"), "codex-dev.db"):
            manifest = HELPER.snapshot(self.home, self.output)
        self.assertEqual(manifest["active"], "snapshot-0.sqlite")
        self.assertEqual(manifest["databases"], ["snapshot-0.sqlite"])
        self.assertIn("补充名称读取超时", manifest["warnings"][0])
        self.assertEqual(sorted(path.name for path in self.output.iterdir()), ["snapshot-0.sqlite"])

    def test_broken_optional_title_catalog_does_not_hide_current_sessions(self):
        self.database(self.home / "state_5.sqlite", "Current")
        (self.home / "codex-dev.db").write_bytes(b"damaged optional title catalog")
        manifest = HELPER.snapshot(self.home, self.output)
        self.assertEqual(manifest["active"], "snapshot-0.sqlite")
        self.assertEqual(len(manifest["warnings"]), 1)
        self.assertIn("codex-dev.db", manifest["warnings"][0])
        with closing(sqlite3.connect(self.output / manifest["active"])) as snapshot:
            self.assertEqual(snapshot.execute("SELECT title FROM threads").fetchone()[0], "Current")

    def test_broken_current_state_never_falls_back_to_healthy_old_sessions(self):
        self.database(self.home / "state_4.sqlite", "Stale")
        (self.home / "state_5.sqlite").write_bytes(b"broken current database")
        with self.assertRaisesRegex(HELPER.SnapshotError, "state_5.sqlite"):
            HELPER.snapshot(self.home, self.output)
        self.assertEqual(list(self.output.iterdir()), [])

    def test_current_state_missing_threads_never_returns_old_sessions(self):
        self.database(self.home / "state_4.sqlite", "Stale")
        with closing(sqlite3.connect(self.home / "state_5.sqlite")) as current:
            current.execute("CREATE TABLE unrelated (id TEXT)")
        with self.assertRaisesRegex(HELPER.SnapshotError, "has no threads table"):
            HELPER.snapshot(self.home, self.output)
        self.assertEqual(list(self.output.iterdir()), [])

    def fail_backup(self, failure, selected_name):
        real_connect = sqlite3.connect
        class Connection:
            def __init__(self, connection, fail):
                self.connection = connection
                self.fail = fail
            def execute(self, *args):
                return self.connection.execute(*args)
            def close(self):
                return self.connection.close()
            def backup(self, target, **kwargs):
                if self.fail:
                    target.execute("CREATE TABLE partial_copy (id TEXT)")
                    target.commit()
                    raise failure
                return self.connection.backup(target, **kwargs)
        def connect(path, *args, **kwargs):
            connection = real_connect(path, *args, **kwargs)
            if str(path).endswith("?mode=ro"):
                return Connection(connection, selected_name in str(path))
            return connection
        return patch.object(HELPER.sqlite3, "connect", side_effect=connect)

    def test_backup_timeout_removes_partial_snapshot_and_preserves_source(self):
        source = self.database(self.home / "state_5.sqlite", "Untouched", wal=True)
        with self.fail_backup(TimeoutError("fixture deadline"), "state_5.sqlite"):
            with self.assertRaisesRegex(TimeoutError, "fixture deadline"):
                HELPER.snapshot(self.home, self.output)
        self.assertEqual(list(self.output.iterdir()), [])
        self.assertEqual(source.execute("SELECT title FROM threads").fetchone()[0], "Untouched")
        self.assertEqual(source.execute("PRAGMA journal_mode").fetchone()[0], "wal")

    def test_failed_optional_catalog_backup_cleans_partial_file_and_keeps_sessions(self):
        self.database(self.home / "state_5.sqlite", "Current")
        with closing(sqlite3.connect(self.home / "codex-dev.db")) as catalog:
            catalog.execute("CREATE TABLE local_thread_catalog (thread_id TEXT, display_title TEXT)")
        with self.fail_backup(sqlite3.DatabaseError("fixture backup failed"), "codex-dev.db"):
            manifest = HELPER.snapshot(self.home, self.output)
        self.assertEqual(manifest["databases"], ["snapshot-0.sqlite"])
        self.assertEqual(len(manifest["warnings"]), 1)
        self.assertIn("fixture backup failed", manifest["warnings"][0])
        self.assertEqual(sorted(path.name for path in self.output.iterdir()), ["snapshot-0.sqlite"])

    def test_existing_output_is_not_overwritten_or_deleted_on_failure(self):
        self.database(self.home / "state_5.sqlite", "Current")
        existing = self.output / "snapshot-0.sqlite"
        existing.write_bytes(b"not owned by this snapshot")
        with self.assertRaises(FileExistsError):
            HELPER.snapshot(self.home, self.output)
        self.assertEqual(existing.read_bytes(), b"not owned by this snapshot")

    def test_database_error_entrypoint_returns_structured_failure_without_empty_success(self):
        (self.home / "state_5.sqlite").write_bytes(b"broken current database")
        result = subprocess.run([sys.executable, str(HELPER_PATH), "--home", str(self.home), "--output", str(self.output)], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        failure = json.loads(result.stdout)["error"]
        self.assertEqual(failure["kind"], "database")
        self.assertIn("state_5.sqlite", failure["message"])
        self.assertEqual(list(self.output.iterdir()), [])

    def test_native_argument_entrypoint_supports_spaces_without_shell_interpretation(self):
        self.database(self.home / "state_5.sqlite", "Fixture")
        result = subprocess.run([sys.executable, str(HELPER_PATH), "--home", str(self.home), "--output", str(self.output)], capture_output=True, text=True, check=True)
        self.assertEqual(json.loads(result.stdout)["active"], "snapshot-0.sqlite")


if __name__ == "__main__":
    unittest.main()
