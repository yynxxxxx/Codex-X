use crate::error::{CodexxError, Result};
use crate::file_io::ensure_directory;
use crate::paths::app_home;
use crate::sqlite_utils::table_column_set;
use rusqlite::Connection;
use std::path::PathBuf;

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

pub(crate) fn open() -> Result<Connection> {
    let path = db_path()?;
    if let Some(parent) = path.parent() {
        ensure_directory(parent)?;
    }
    let conn = Connection::open(&path).map_err(|e| CodexxError::Database(e.to_string()))?;
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
        );",
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    ensure_sqlite_column(
        &conn,
        "providers",
        "toml_config",
        "ALTER TABLE providers ADD COLUMN toml_config TEXT",
    )?;
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
    Ok(conn)
}
