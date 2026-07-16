use super::global_state::{
    apply_global_state_update_with_journal, restore_global_write, GlobalStateWrite,
};
use super::storage::{apply_session_changes, restore_session_changes};
use super::types::{RolloutScan, SessionFileChange};
use crate::error::{CodexxError, Result};
use crate::file_io::io_err;
use crate::sqlite_utils::{sqlite_has_table, table_column_set};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Default)]
pub(super) struct SqliteUpdateCounts {
    provider_rows: usize,
    cwd_rows: usize,
}

impl SqliteUpdateCounts {
    pub(super) fn total(&self) -> usize {
        self.provider_rows + self.cwd_rows
    }

    fn add(&mut self, other: Self) {
        self.provider_rows += other.provider_rows;
        self.cwd_rows += other.cwd_rows;
    }
}

#[derive(Debug, Default)]
pub(super) struct MutationJournal {
    applied_rollouts: Vec<SessionFileChange>,
    sqlite_restore_attempts: Vec<SqliteRestoreAttempt>,
    global_writes: Vec<GlobalStateWrite>,
}

#[derive(Debug, Clone)]
struct SqliteRestoreAttempt {
    path: PathBuf,
    expected_data_version: i64,
}

pub(super) struct PendingSqliteUpdate {
    path: PathBuf,
    conn: Connection,
    observer: Connection,
    columns: HashSet<String>,
    counts: SqliteUpdateCounts,
    transaction_open: bool,
}

impl PendingSqliteUpdate {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

pub(super) fn rollback_open_transactions(updates: &mut [PendingSqliteUpdate]) {
    for update in updates.iter_mut().rev() {
        if update.transaction_open {
            let _ = update.conn.execute_batch("ROLLBACK");
            update.transaction_open = false;
        }
    }
}

fn sqlite_data_version(conn: &Connection) -> Result<i64> {
    conn.query_row("PRAGMA data_version", [], |row| row.get(0))
        .map_err(|error| CodexxError::Database(error.to_string()))
}

fn create_sqlite_rollback_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TEMP TABLE codexx_session_rollback (
            id TEXT PRIMARY KEY,
            model_provider TEXT,
            cwd TEXT,
            provider_changed INTEGER NOT NULL DEFAULT 0,
            cwd_changed INTEGER NOT NULL DEFAULT 0
         );",
    )
    .map_err(|error| CodexxError::Database(error.to_string()))
}

pub(super) fn prepare_sqlite_updates(sqlite_paths: &[PathBuf]) -> Result<Vec<PendingSqliteUpdate>> {
    let mut pending = Vec::new();
    let mut seen_databases = HashSet::new();
    let prepare_result = (|| -> Result<()> {
        for path in sqlite_paths {
            if !path.exists() {
                return Err(CodexxError::Database(format!(
                    "SQLite 文件不存在: {}",
                    path.display()
                )));
            }
            let identity = path.canonicalize().map_err(|error| io_err(path, error))?;
            if !seen_databases.insert(identity.clone()) {
                continue;
            }
            let conn = Connection::open_with_flags(
                &identity,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|error| {
                CodexxError::Database(format!("打开 SQLite 失败 {}: {error}", path.display()))
            })?;
            conn.busy_timeout(Duration::from_secs(5))
                .map_err(|error| CodexxError::Database(error.to_string()))?;
            if !sqlite_has_table(&conn, "threads")? {
                continue;
            }
            let columns = table_column_set(&conn, "threads")?;
            if !columns.contains("id") || !columns.contains("model_provider") {
                continue;
            }
            conn.execute_batch("BEGIN IMMEDIATE")
                .map_err(|error| CodexxError::Database(error.to_string()))?;
            if let Err(error) = create_sqlite_rollback_table(&conn) {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
            let observer = match (|| -> Result<Connection> {
                let observer = Connection::open_with_flags(
                    &identity,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .map_err(|error| {
                    CodexxError::Database(format!(
                        "打开 SQLite 观察连接失败 {}: {error}",
                        identity.display()
                    ))
                })?;
                observer
                    .busy_timeout(Duration::from_secs(5))
                    .map_err(|error| CodexxError::Database(error.to_string()))?;
                sqlite_data_version(&observer)?;
                Ok(observer)
            })() {
                Ok(observer) => observer,
                Err(error) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(error);
                }
            };
            pending.push(PendingSqliteUpdate {
                path: identity,
                conn,
                observer,
                columns,
                counts: SqliteUpdateCounts::default(),
                transaction_open: true,
            });
        }
        Ok(())
    })();
    if let Err(error) = prepare_result {
        rollback_open_transactions(&mut pending);
        return Err(error);
    }
    Ok(pending)
}

fn apply_sqlite_updates(
    pending: &mut [PendingSqliteUpdate],
    rollouts: &RolloutScan,
    target_provider: &str,
) -> Result<()> {
    for update in pending.iter_mut() {
        update
            .conn
            .execute(
                "INSERT INTO temp.codexx_session_rollback
                    (id, model_provider, provider_changed)
                 SELECT id, model_provider, 1 FROM threads
                 WHERE COALESCE(model_provider, '') <> ?1
                 ON CONFLICT(id) DO UPDATE SET
                    model_provider = excluded.model_provider,
                    provider_changed = 1",
                [target_provider],
            )
            .map_err(|error| CodexxError::Database(error.to_string()))?;
        update.counts.provider_rows = update
            .conn
            .execute(
                "UPDATE threads SET model_provider = ?1 WHERE COALESCE(model_provider, '') <> ?1",
                [target_provider],
            )
            .map_err(|error| CodexxError::Database(error.to_string()))?;

        if update.columns.contains("id") && update.columns.contains("cwd") {
            for (thread_id, cwd) in &rollouts.cwd_by_thread_id {
                update
                    .conn
                    .execute(
                        "INSERT INTO temp.codexx_session_rollback
                            (id, cwd, cwd_changed)
                         SELECT id, cwd, 1 FROM threads
                         WHERE id = ?2 AND COALESCE(cwd, '') <> ?1
                         ON CONFLICT(id) DO UPDATE SET
                            cwd = excluded.cwd,
                            cwd_changed = 1",
                        (cwd, thread_id),
                    )
                    .map_err(|error| CodexxError::Database(error.to_string()))?;
                update.counts.cwd_rows += update
                    .conn
                    .execute(
                        "UPDATE threads SET cwd = ?1 WHERE id = ?2 AND COALESCE(cwd, '') <> ?1",
                        (cwd, thread_id),
                    )
                    .map_err(|error| CodexxError::Database(error.to_string()))?;
            }
        }
    }
    Ok(())
}

fn commit_sqlite_updates<F>(
    pending: &mut [PendingSqliteUpdate],
    journal: &mut MutationJournal,
    hook: &mut F,
) -> Result<SqliteUpdateCounts>
where
    F: FnMut(MutationPoint) -> Result<()>,
{
    let mut updated = SqliteUpdateCounts::default();
    for index in 0..pending.len() {
        let before_commit = sqlite_data_version(&pending[index].observer)?;
        journal.sqlite_restore_attempts.push(SqliteRestoreAttempt {
            path: pending[index].path.clone(),
            expected_data_version: before_commit,
        });
        let attempt_index = journal.sqlite_restore_attempts.len() - 1;
        if let Err(error) = pending[index].conn.execute_batch("COMMIT") {
            let rollback_succeeded = !pending[index].conn.is_autocommit()
                && pending[index].conn.execute_batch("ROLLBACK").is_ok()
                && pending[index].conn.is_autocommit();
            pending[index].transaction_open = !pending[index].conn.is_autocommit();
            if rollback_succeeded {
                journal.sqlite_restore_attempts.remove(attempt_index);
            }
            rollback_open_transactions(&mut pending[index + 1..]);
            return Err(CodexxError::Database(error.to_string()));
        }
        pending[index].transaction_open = false;
        journal.sqlite_restore_attempts[attempt_index].expected_data_version =
            sqlite_data_version(&pending[index].observer)?;
        updated.add(std::mem::take(&mut pending[index].counts));
        if let Err(error) = hook(MutationPoint::AfterSqliteCommit(index)) {
            rollback_open_transactions(&mut pending[index + 1..]);
            return Err(error);
        }
    }
    Ok(updated)
}

fn restore_sqlite_update(
    update: &mut PendingSqliteUpdate,
    expected_data_version: i64,
) -> Result<()> {
    update
        .conn
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    update.transaction_open = true;
    let result = (|| -> Result<()> {
        let current = sqlite_data_version(&update.observer)?;
        if current != expected_data_version {
            return Err(CodexxError::Config(format!(
                "会话数据库已发生变化，已保留备份且未覆盖: {}",
                update.path.display()
            )));
        }

        let mut statements = vec![
            "UPDATE threads
             SET model_provider = (
                SELECT rollback.model_provider
                FROM temp.codexx_session_rollback AS rollback
                WHERE rollback.id = threads.id
             )
             WHERE id IN (
                SELECT id FROM temp.codexx_session_rollback WHERE provider_changed = 1
             )",
        ];
        if update.columns.contains("cwd") {
            statements.push(
                "UPDATE threads
                 SET cwd = (
                    SELECT rollback.cwd
                    FROM temp.codexx_session_rollback AS rollback
                    WHERE rollback.id = threads.id
                 )
                 WHERE id IN (
                    SELECT id FROM temp.codexx_session_rollback WHERE cwd_changed = 1
                 )",
            );
        }
        statements.push("DROP TABLE temp.codexx_session_rollback");
        update
            .conn
            .execute_batch(&statements.join(";"))
            .map_err(|error| CodexxError::Database(error.to_string()))?;
        update
            .conn
            .execute_batch("COMMIT")
            .map_err(|error| CodexxError::Database(error.to_string()))?;
        update.transaction_open = false;
        Ok(())
    })();
    if result.is_err() && update.transaction_open {
        let _ = update.conn.execute_batch("ROLLBACK");
        update.transaction_open = false;
    }
    result
}

pub(super) fn rollback_mutation(
    journal: &MutationJournal,
    pending_sqlite: &mut [PendingSqliteUpdate],
) -> Vec<String> {
    let mut errors = Vec::new();
    for write in journal.global_writes.iter().rev() {
        if let Err(error) = restore_global_write(write) {
            errors.push(error.to_string());
        }
    }
    for attempt in journal.sqlite_restore_attempts.iter().rev() {
        let Some(update) = pending_sqlite
            .iter_mut()
            .find(|update| update.path == attempt.path)
        else {
            errors.push(format!("缺少 SQLite 恢复连接: {}", attempt.path.display()));
            continue;
        };
        if let Err(error) = restore_sqlite_update(update, attempt.expected_data_version) {
            errors.push(error.to_string());
        }
    }
    if let Err(error) = restore_session_changes(&journal.applied_rollouts) {
        errors.push(error.to_string());
    }
    errors
}

pub(super) fn mutation_error(original: CodexxError, recovery_errors: Vec<String>) -> CodexxError {
    if recovery_errors.is_empty() {
        original
    } else {
        CodexxError::Config(format!(
            "同步失败，自动恢复也未完成：{original}；{}",
            recovery_errors.join("；")
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MutationPoint {
    AfterSqliteCommit(usize),
    AfterGlobalMainWrite,
}

pub(super) struct MutationResult {
    pub(super) applied_rollouts: usize,
    pub(super) skipped_rollouts: Vec<PathBuf>,
    pub(super) sqlite_updates: SqliteUpdateCounts,
}

pub(super) fn execute_provider_sync_mutation<F>(
    rollouts: &RolloutScan,
    pending_sqlite: &mut [PendingSqliteUpdate],
    target_provider: &str,
    global_state_path: &Path,
    journal: &mut MutationJournal,
    hook: &mut F,
) -> Result<MutationResult>
where
    F: FnMut(MutationPoint) -> Result<()>,
{
    let result = (|| -> Result<MutationResult> {
        let (applied_rollouts, skipped_rollouts) = apply_session_changes(&rollouts.changes)?;
        journal.applied_rollouts = applied_rollouts;
        apply_sqlite_updates(pending_sqlite, rollouts, target_provider)?;
        apply_global_state_update_with_journal(
            global_state_path,
            &mut journal.global_writes,
            &mut || hook(MutationPoint::AfterGlobalMainWrite),
        )?;
        let sqlite_updates = commit_sqlite_updates(pending_sqlite, journal, hook)?;
        Ok(MutationResult {
            applied_rollouts: journal.applied_rollouts.len(),
            skipped_rollouts,
            sqlite_updates,
        })
    })();
    if result.is_err() {
        rollback_open_transactions(pending_sqlite);
    }
    result
}

#[cfg(test)]
#[path = "transaction_tests.rs"]
mod tests;
