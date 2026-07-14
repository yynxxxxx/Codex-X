use crate::error::{CodexxError, Result};
use rusqlite::Connection;
use std::collections::HashSet;

pub(crate) fn sql_select_column(cols: &HashSet<String>, name: &str, fallback: &str) -> String {
    if cols.contains(name) {
        format!("\"{}\"", name.replace('"', "\"\""))
    } else {
        fallback.to_string()
    }
}

pub(crate) fn sqlite_has_table(conn: &Connection, table: &str) -> Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
        [table],
        |_| Ok(()),
    )
    .map(|_| true)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(false),
        other => Err(CodexxError::Database(other.to_string())),
    })
}

pub(crate) fn table_column_set(conn: &Connection, table: &str) -> Result<HashSet<String>> {
    let mut stmt = conn
        .prepare(&format!(
            "PRAGMA table_info(\"{}\")",
            table.replace('"', "\"\"")
        ))
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let mut cols = HashSet::new();
    for row in rows {
        cols.insert(row.map_err(|e| CodexxError::Database(e.to_string()))?);
    }
    Ok(cols)
}
