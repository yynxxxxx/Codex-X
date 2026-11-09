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
        with self.assertRaises(sqlite3.DatabaseError):
            HELPER.snapshot(self.home, self.output)
        self.assertEqual(list(self.output.iterdir()), [])

    def test_native_argument_entrypoint_supports_spaces_without_shell_interpretation(self):
        self.database(self.home / "state_5.sqlite", "Fixture")
        result = subprocess.run([sys.executable, str(HELPER_PATH), "--home", str(self.home), "--output", str(self.output)], capture_output=True, text=True, check=True)
        self.assertEqual(json.loads(result.stdout)["active"], "snapshot-0.sqlite")


if __name__ == "__main__":
    unittest.main()
