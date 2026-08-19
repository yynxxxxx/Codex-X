use crate::error::{CodexxError, Result};
use crate::file_io::ensure_directory;
use crate::paths::app_home;
use crate::sqlite_utils::table_column_set;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const APP_DB_SCHEMA_VERSION: i64 = 2;

struct DatabaseInitializer {
    migration_lock: Mutex<()>,
}

impl DatabaseInitializer {
    fn new() -> Self {
        Self {
            migration_lock: Mutex::new(()),
        }
    }

    fn open_at(&self, path: &Path) -> Result<Connection> {
        self.open_at_with(path, initialize_schema)
    }

    fn open_at_with(
        &self,
        path: &Path,
        initialize: impl FnOnce(&Connection) -> Result<()>,
    ) -> Result<Connection> {
        if let Some(parent) = path.parent() {
            ensure_directory(parent)?;
        }
        let conn = Connection::open(path).map_err(|e| CodexxError::Database(e.to_string()))?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|e| CodexxError::Database(e.to_string()))?;

        if schema_is_current(&conn)? {
            return Ok(conn);
        }

        // Serialize first-time migrations inside this process. The persistent
        // schema version also prevents repeated migrations across launches and
        // detects a database replaced at the same path.
        let _migration_guard = self
            .migration_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !schema_is_current(&conn)? {
            migrate_schema(&conn, initialize)?;
        }
        Ok(conn)
    }
}

fn schema_version(conn: &Connection) -> Result<i64> {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| CodexxError::Database(error.to_string()))
}

fn schema_is_current(conn: &Connection) -> Result<bool> {
    Ok(schema_version(conn)? >= APP_DB_SCHEMA_VERSION)
}

fn migrate_schema(
    conn: &Connection,
    initialize: impl FnOnce(&Connection) -> Result<()>,
) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| CodexxError::Database(error.to_string()))?;

    let migration = (|| {
        // Another Codex-X process may have completed the migration while this
        // connection waited for SQLite's write lock.
        if !schema_is_current(conn)? {
            initialize(conn)?;
            conn.pragma_update(None, "user_version", APP_DB_SCHEMA_VERSION)
                .map_err(|error| CodexxError::Database(error.to_string()))?;
        }
        conn.execute_batch("COMMIT")
            .map_err(|error| CodexxError::Database(error.to_string()))
    })();

    match migration {
        Ok(()) => Ok(()),
        Err(error) => match conn.execute_batch("ROLLBACK") {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(CodexxError::Database(format!(
                "{error}; app database migration rollback failed: {rollback_error}"
            ))),
        },
    }
}

fn database_initializer() -> &'static DatabaseInitializer {
    static INITIALIZER: OnceLock<DatabaseInitializer> = OnceLock::new();
    INITIALIZER.get_or_init(DatabaseInitializer::new)
}

#[cfg(test)]
pub(crate) fn test_db_guard() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static TEST_DB_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    TEST_DB_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("test app database lock poisoned")
}

fn db_path() -> Result<PathBuf> {
    Ok(app_home()?.join("codexx.db"))
}

fn ensure_sqlite_column(
    conn: &Connection,
    table: &str,
    column: &str,
    alter_sql: &str,
) -> Result<()> {
    let cols = table_column_set(conn, table)?;
    if cols.contains(column) {
        return Ok(());
    }
    match conn.execute(alter_sql, []) {
        Ok(_) => Ok(()),
        Err(e) => {
            let message = e.to_string().to_ascii_lowercase();
            if message.contains("duplicate column") || message.contains("duplicate column name") {
                // Another running Codex-X process may have applied the same
                // lightweight migration between our PRAGMA check and ALTER.
                Ok(())
            } else {
                Err(CodexxError::Database(e.to_string()))
            }
        }
    }
}

fn initialize_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS providers (
            id TEXT PRIMARY KEY,
            provider_name TEXT NOT NULL,
            base_url TEXT NOT NULL,
            model TEXT NOT NULL,
            api_key TEXT,
            toml_config TEXT,
            wire_api TEXT NOT NULL DEFAULT 'responses',
            requires_openai_auth INTEGER NOT NULL DEFAULT 1,
            source TEXT NOT NULL DEFAULT 'manual',
            source_id TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_providers_updated_at ON providers(updated_at DESC);
        CREATE TABLE IF NOT EXISTS prompts (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            filename TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_prompts_updated_at ON prompts(updated_at DESC);
        CREATE TABLE IF NOT EXISTS builtin_prompt_cache (
            id TEXT PRIMARY KEY,
            filename TEXT NOT NULL,
            source_url TEXT NOT NULL,
            content TEXT NOT NULL,
            checked_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS builtin_prompt_overrides (
            template_id TEXT PRIMARY KEY,
            content TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS managed_mcp_servers (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            server_config TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS managed_skills (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT,
            directory TEXT NOT NULL,
            source_path TEXT,
            content_hash TEXT,
            enabled INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS skills_mcp_notes (
            codex_dir TEXT NOT NULL,
            item_kind TEXT NOT NULL,
            item_id TEXT NOT NULL,
            note TEXT NOT NULL DEFAULT '',
            updated_at TEXT NOT NULL,
            PRIMARY KEY (codex_dir, item_kind, item_id)
        );",
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    ensure_sqlite_column(
        conn,
        "providers",
        "toml_config",
        "ALTER TABLE providers ADD COLUMN toml_config TEXT",
    )?;
    ensure_sqlite_column(
        conn,
        "providers",
        "source",
        "ALTER TABLE providers ADD COLUMN source TEXT NOT NULL DEFAULT 'manual'",
    )?;
    ensure_sqlite_column(
        conn,
        "providers",
        "source_id",
        "ALTER TABLE providers ADD COLUMN source_id TEXT",
    )?;
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_providers_source_identity
         ON providers(source, source_id)
         WHERE source_id IS NOT NULL;",
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    conn.execute(
        "DELETE FROM prompts
         WHERE id LIKE 'external-%'
           AND EXISTS (
             SELECT 1 FROM prompts AS kept
             WHERE lower(kept.filename) = lower(prompts.filename)
               AND kept.id NOT LIKE 'external-%'
           )",
        [],
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    conn.execute(
        "DELETE FROM prompts
         WHERE id LIKE 'external-%'
           AND EXISTS (
             SELECT 1 FROM prompts AS kept
             WHERE kept.content = prompts.content
               AND kept.id NOT LIKE 'external-%'
           )",
        [],
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    conn.execute(
        "DELETE FROM prompts
         WHERE id LIKE 'external-%'
           AND EXISTS (
             SELECT 1 FROM prompts AS kept
             WHERE kept.content = prompts.content
               AND kept.id LIKE 'external-%'
               AND kept.rowid <> prompts.rowid
               AND (kept.updated_at > prompts.updated_at OR (kept.updated_at = prompts.updated_at AND kept.rowid > prompts.rowid))
           )",
        [],
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    Ok(())
}

pub(crate) fn open() -> Result<Connection> {
    database_initializer().open_at(&db_path()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;

    fn test_db_path(name: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let suffix = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!(
                "codex-x-app-db-{name}-{}-{suffix}",
                std::process::id()
            ))
            .join("codexx.db")
    }

    fn remove_test_db(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::remove_dir_all(parent).expect("remove app database test directory");
        }
    }

    #[test]
    fn repeated_opens_do_not_rerun_legacy_cleanup() {
        let path = test_db_path("cleanup-once");
        let initializer = DatabaseInitializer::new();
        let conn = initializer.open_at(&path).expect("initialize database");
        conn.execute_batch(
            "INSERT INTO prompts (id, title, filename, content, created_at, updated_at)
             VALUES
               ('kept', 'Kept', 'same.md', 'same', '1', '1'),
               ('external-duplicate', 'Duplicate', 'same.md', 'same', '2', '2');",
        )
        .expect("seed a post-migration duplicate");
        drop(conn);

        let reopened_initializer = DatabaseInitializer::new();
        let conn = reopened_initializer
            .open_at(&path)
            .expect("reopen database in a new process lifecycle");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
            .expect("count prompts after reopen");
        assert_eq!(count, 2, "reopen must not rerun migration cleanup");
        drop(conn);
        remove_test_db(&path);
    }

    #[test]
    fn database_replaced_at_the_same_path_is_initialized_again() {
        let path = test_db_path("replace-database");
        let initializer = DatabaseInitializer::new();
        let conn = initializer.open_at(&path).expect("initialize database");
        assert_eq!(schema_version(&conn).expect("read schema version"), 2);
        drop(conn);

        fs::remove_file(&path).expect("replace initialized database");
        let conn = initializer
            .open_at(&path)
            .expect("initialize replacement database");
        let provider_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM providers", [], |row| row.get(0))
            .expect("query replacement database schema");
        assert_eq!(provider_count, 0);
        assert_eq!(schema_version(&conn).expect("read replacement version"), 2);
        drop(conn);
        remove_test_db(&path);
    }

    #[test]
    fn version_one_database_migrates_without_losing_existing_data() {
        let path = test_db_path("v1-to-v2");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create legacy database directory");
        }
        let legacy = Connection::open(&path).expect("create legacy database");
        initialize_schema(&legacy).expect("create legacy tables");
        legacy
            .execute_batch(
                "INSERT INTO providers (
                    id, provider_name, base_url, model, wire_api, requires_openai_auth,
                    source, created_at, updated_at
                 ) VALUES ('provider', 'Provider', 'https://example.test', 'model',
                    'responses', 0, 'manual', '1', '1');
                 INSERT INTO prompts (id, title, filename, content, created_at, updated_at)
                 VALUES ('prompt', 'Prompt', 'prompt.md', 'content', '1', '1');
                 INSERT INTO managed_skills (
                    id, name, directory, enabled, updated_at
                 ) VALUES ('skill', 'Skill', 'skill', 1, '1');
                 INSERT INTO managed_mcp_servers (
                    id, name, server_config, enabled, updated_at
                 ) VALUES ('mcp', 'MCP', '{}', 1, '1');
                 DROP TABLE skills_mcp_notes;
                 PRAGMA user_version = 1;",
            )
            .expect("seed version one database");
        drop(legacy);

        let conn = DatabaseInitializer::new()
            .open_at(&path)
            .expect("migrate version one database");
        assert_eq!(schema_version(&conn).expect("read migrated version"), 2);
        for table in [
            "providers",
            "prompts",
            "managed_skills",
            "managed_mcp_servers",
        ] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("count preserved rows");
            assert_eq!(count, 1, "{table} row must survive migration");
        }
        conn.execute(
            "INSERT INTO skills_mcp_notes (codex_dir, item_kind, item_id, note, updated_at)
             VALUES ('home', 'skill', 'skill', 'note', '1')",
            [],
        )
        .expect("write note after migration");
        drop(conn);
        remove_test_db(&path);
    }

    #[test]
    fn concurrent_first_opens_initialize_once() {
        let path = test_db_path("concurrent-init");
        let attempts = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();

        for _ in 0..2 {
            let path = path.clone();
            // Separate initializers model independent Codex-X processes; the
            // SQLite transaction and persistent version still permit one run.
            let initializer = DatabaseInitializer::new();
            let attempts = Arc::clone(&attempts);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                initializer.open_at_with(&path, |conn| {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(25));
                    conn.execute_batch("CREATE TABLE initialized_once (id INTEGER PRIMARY KEY);")
                        .map_err(|error| CodexxError::Database(error.to_string()))
                })
            }));
        }

        barrier.wait();
        for worker in workers {
            drop(
                worker
                    .join()
                    .expect("join concurrent database opener")
                    .expect("open database concurrently"),
            );
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        remove_test_db(&path);
    }

    #[test]
    fn failed_initialization_can_retry() {
        let path = test_db_path("retry-init");
        let initializer = DatabaseInitializer::new();
        let attempts = AtomicUsize::new(0);

        let error = initializer
            .open_at_with(&path, |_| {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(CodexxError::Database(
                    "injected transient initialization failure".to_string(),
                ))
            })
            .expect_err("first initialization must fail");
        assert!(error
            .to_string()
            .contains("transient initialization failure"));

        let conn = initializer
            .open_at_with(&path, |conn| {
                attempts.fetch_add(1, Ordering::SeqCst);
                conn.execute_batch("CREATE TABLE retry_succeeded (id INTEGER PRIMARY KEY);")
                    .map_err(|error| CodexxError::Database(error.to_string()))
            })
            .expect("retry initialization");
        drop(conn);
        let conn = initializer
            .open_at_with(&path, |_| {
                attempts.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .expect("open initialized database");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        drop(conn);
        remove_test_db(&path);
    }

    #[test]
    fn initialized_read_connection_does_not_need_a_write_lock() {
        let path = test_db_path("read-with-writer");
        let initializer = DatabaseInitializer::new();
        let writer = initializer.open_at(&path).expect("initialize database");
        writer
            .execute_batch("BEGIN IMMEDIATE")
            .expect("hold database write reservation");

        let reader = initializer
            .open_at(&path)
            .expect("open reader while write reservation is held");
        let count: i64 = reader
            .query_row("SELECT COUNT(*) FROM providers", [], |row| row.get(0))
            .expect("read while another connection holds write reservation");
        assert_eq!(count, 0);

        drop(reader);
        writer
            .execute_batch("ROLLBACK")
            .expect("release write lock");
        drop(writer);
        remove_test_db(&path);
    }
}
