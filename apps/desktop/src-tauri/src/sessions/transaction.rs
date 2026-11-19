use super::catalog::{
    apply_catalog_updates, catalog_columns, create_catalog_rollback_tables,
    restore_catalog_updates, CatalogRepairThread,
};
use super::rollout_stream::ensure_current_rollout_representation;
use super::storage::{
    apply_session_changes_with_hook, hash_rollout_file, restore_session_changes,
    rollout_path_has_syncable_identity, rollout_text_is_internal, sqlite_subagent_thread_ids,
};
use super::types::{RolloutScan, SessionFileChange};
use crate::error::{CodexxError, Result};
use crate::file_io::io_err;
use crate::sqlite_utils::{sqlite_has_table, table_column_set};
use rusqlite::{Connection, OpenFlags};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Default)]
pub(super) struct SqliteUpdateCounts {
    provider_rows: usize,
    cwd_rows: usize,
    catalog_insert_rows: usize,
}

impl SqliteUpdateCounts {
    pub(super) fn total(&self) -> usize {
        self.provider_rows + self.cwd_rows + self.catalog_insert_rows
    }

    fn add(&mut self, other: Self) {
        self.provider_rows += other.provider_rows;
        self.cwd_rows += other.cwd_rows;
        self.catalog_insert_rows += other.catalog_insert_rows;
    }
}

#[derive(Debug, Default)]
pub(super) struct MutationJournal {
    applied_rollouts: Vec<SessionFileChange>,
    expected_rollout_hashes: HashMap<PathBuf, [u8; 32]>,
    sqlite_restore_attempts: Vec<SqliteRestoreAttempt>,
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
    thread_columns: HashSet<String>,
    catalog_columns: HashSet<String>,
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
    .map_err(|error| CodexxError::Database(error.to_string()))?;
    create_catalog_rollback_tables(conn)
}

pub(super) fn prepare_sqlite_updates(
    sqlite_paths: &[PathBuf],
    catalog_paths: &[PathBuf],
) -> Result<Vec<PendingSqliteUpdate>> {
    let catalog_identities = catalog_paths
        .iter()
        .map(|path| path.canonicalize().map_err(|error| io_err(path, error)))
        .collect::<Result<HashSet<_>>>()?;
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
            let thread_columns = if sqlite_has_table(&conn, "threads")? {
                table_column_set(&conn, "threads")?
            } else {
                HashSet::new()
            };
            let catalog_columns = if catalog_identities.contains(&identity) {
                catalog_columns(&conn)?
            } else {
                HashSet::new()
            };
            let supports_thread_updates =
                thread_columns.contains("id") && thread_columns.contains("model_provider");
            let supports_catalog_updates =
                catalog_columns.contains("thread_id") && catalog_columns.contains("model_provider");
            if !supports_thread_updates && !supports_catalog_updates {
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
                thread_columns,
                catalog_columns,
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
    catalog_sources: &HashMap<String, CatalogRepairThread>,
    syncable_thread_ids: &HashSet<String>,
) -> Result<()> {
    let mut sorted_thread_ids = syncable_thread_ids.iter().collect::<Vec<_>>();
    sorted_thread_ids.sort();
    for update in pending.iter_mut() {
        let internal_ids = if update.thread_columns.contains("id") {
            sqlite_subagent_thread_ids(&update.conn, &update.thread_columns)?
        } else {
            HashSet::new()
        };
        if update.thread_columns.contains("id") && update.thread_columns.contains("model_provider")
        {
            let archived_filter = if update.thread_columns.contains("archived") {
                " AND COALESCE(archived, 0) = 0"
            } else {
                ""
            };
            let snapshot_sql = format!(
                "INSERT INTO temp.codexx_session_rollback
                        (id, model_provider, provider_changed)
                     SELECT id, model_provider, 1 FROM threads
                     WHERE id = ?2 AND COALESCE(model_provider, '') <> ?1{archived_filter}
                     ON CONFLICT(id) DO UPDATE SET
                        model_provider = excluded.model_provider,
                        provider_changed = 1"
            );
            let update_sql = format!(
                "UPDATE threads SET model_provider = ?1 \
                 WHERE id = ?2 AND COALESCE(model_provider, '') <> ?1{archived_filter}"
            );
            for thread_id in &sorted_thread_ids {
                if internal_ids.contains(*thread_id) {
                    continue;
                }
                update
                    .conn
                    .execute(&snapshot_sql, (target_provider, thread_id))
                    .map_err(|error| CodexxError::Database(error.to_string()))?;
                update.counts.provider_rows += update
                    .conn
                    .execute(&update_sql, (target_provider, thread_id))
                    .map_err(|error| CodexxError::Database(error.to_string()))?;
            }
        }

        if update.thread_columns.contains("id") && update.thread_columns.contains("cwd") {
            let archived_filter = if update.thread_columns.contains("archived") {
                " AND COALESCE(archived, 0) = 0"
            } else {
                ""
            };
            let snapshot_sql = format!(
                "INSERT INTO temp.codexx_session_rollback
                    (id, cwd, cwd_changed)
                 SELECT id, cwd, 1 FROM threads
                 WHERE id = ?2 AND COALESCE(cwd, '') <> ?1{archived_filter}
                 ON CONFLICT(id) DO UPDATE SET
                    cwd = excluded.cwd,
                    cwd_changed = 1"
            );
            let update_sql = format!(
                "UPDATE threads SET cwd = ?1 \
                 WHERE id = ?2 AND COALESCE(cwd, '') <> ?1{archived_filter}"
            );
            for (thread_id, cwd) in &rollouts.cwd_by_thread_id {
                if !syncable_thread_ids.contains(thread_id) || internal_ids.contains(thread_id) {
                    continue;
                }
                update
                    .conn
                    .execute(&snapshot_sql, (cwd, thread_id))
                    .map_err(|error| CodexxError::Database(error.to_string()))?;
                update.counts.cwd_rows += update
                    .conn
                    .execute(&update_sql, (cwd, thread_id))
                    .map_err(|error| CodexxError::Database(error.to_string()))?;
            }
        }
        let local_syncable_ids = syncable_thread_ids
            .difference(&internal_ids)
            .cloned()
            .collect();
        if !update.catalog_columns.is_empty() {
            let catalog_counts = apply_catalog_updates(
                &update.conn,
                &update.catalog_columns,
                target_provider,
                catalog_sources,
                &local_syncable_ids,
            )?;
            update.counts.provider_rows += catalog_counts.provider_rows;
            update.counts.catalog_insert_rows += catalog_counts.inserted_rows;
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
        validate_rollout_snapshots(&journal.expected_rollout_hashes)?;
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
    // The final commit hook can publish an external rollout update as well.
    // Returning success requires the complete candidate set to remain verified.
    validate_rollout_snapshots(&journal.expected_rollout_hashes)?;
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

        if !update.catalog_columns.is_empty() {
            restore_catalog_updates(&update.conn, &update.catalog_columns)?;
        }
        let mut statements = Vec::new();
        if update.thread_columns.contains("id") && update.thread_columns.contains("model_provider")
        {
            statements.push(
                "UPDATE threads
                 SET model_provider = (
                    SELECT rollback.model_provider
                    FROM temp.codexx_session_rollback AS rollback
                    WHERE rollback.id = threads.id
                 )
                 WHERE id IN (
                    SELECT id FROM temp.codexx_session_rollback WHERE provider_changed = 1
                 )",
            );
        }
        if update.thread_columns.contains("id") && update.thread_columns.contains("cwd") {
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
    BeforeSqliteLock,
    BeforeRolloutMutation,
    BeforeEachRolloutMutation(usize),
    AfterRolloutMutation,
    AfterSqliteCommit(usize),
}

pub(super) struct MutationResult {
    pub(super) applied_rollouts: usize,
    pub(super) sqlite_updates: SqliteUpdateCounts,
}

// A file may be reclassified after scanning. In particular, the general file
// writer skips concurrently changed rollouts; that must not leave their old IDs
// eligible for a catalog insert or a provider update in this transaction.
fn validate_rollout_classification(rollouts: &RolloutScan) -> Result<()> {
    let mut paths = rollouts.provider_candidate_paths.clone();
    for change in &rollouts.changes {
        let original_is_internal = if let Some(snapshot) = &change.streamed {
            !rollout_path_has_syncable_identity(&snapshot.original_path)?
        } else {
            rollout_text_is_internal(&change.original_text)
        };
        if original_is_internal {
            return Err(CodexxError::Config(
                "内部会话不能同步为普通会话。".to_string(),
            ));
        }
        paths.insert(change.path.clone());
    }
    for path in paths {
        if !rollout_path_has_syncable_identity(&path)? {
            return Err(CodexxError::Config(format!(
                "会话类型已变化，已停止同步；请重新检查会话：{}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn expected_rollout_hashes(rollouts: &RolloutScan) -> Result<HashMap<PathBuf, [u8; 32]>> {
    let mut expected = HashMap::new();
    for path in &rollouts.provider_candidate_paths {
        let hash = rollouts.verified_rollout_hashes.get(path).ok_or_else(|| {
            CodexxError::Config(format!(
                "缺少会话文件验证快照，已停止同步：{}",
                path.display()
            ))
        })?;
        expected.insert(path.clone(), *hash);
    }
    for change in &rollouts.changes {
        let original_hash = change.original_hash();
        if expected
            .get(&change.path)
            .is_some_and(|hash| *hash != original_hash)
        {
            return Err(CodexxError::Config(format!(
                "会话文件改写快照与扫描结果不一致，已停止同步：{}",
                change.path.display()
            )));
        }
        // A planned change already carries the scan's verified original snapshot.
        expected.insert(change.path.clone(), original_hash);
    }
    Ok(expected)
}

fn validate_rollout_snapshots(expected: &HashMap<PathBuf, [u8; 32]>) -> Result<()> {
    for (path, expected_hash) in expected {
        ensure_current_rollout_representation(path)?;
        if hash_rollout_file(path).map_err(|error| io_err(path, error))? != *expected_hash {
            return Err(CodexxError::Config(format!(
                "会话文件已发生变化，已停止同步；请重新检查会话：{}",
                path.display()
            )));
        }
    }
    Ok(())
}

pub(super) fn execute_provider_sync_mutation<F>(
    rollouts: &RolloutScan,
    pending_sqlite: &mut [PendingSqliteUpdate],
    target_provider: &str,
    catalog_sources: &HashMap<String, CatalogRepairThread>,
    syncable_thread_ids: &HashSet<String>,
    journal: &mut MutationJournal,
    hook: &mut F,
) -> Result<MutationResult>
where
    F: FnMut(MutationPoint) -> Result<()>,
{
    let result = (|| -> Result<MutationResult> {
        if !rollouts.blocked_failures.is_empty() {
            return Err(CodexxError::Config(format!(
                "会话扫描未完成，已停止同步：{}",
                rollouts.blocked_failures.join("；")
            )));
        }
        journal.expected_rollout_hashes = expected_rollout_hashes(rollouts)?;
        hook(MutationPoint::BeforeRolloutMutation)?;
        validate_rollout_classification(rollouts)?;
        validate_rollout_snapshots(&journal.expected_rollout_hashes)?;
        let (applied_rollouts, skipped_rollouts) =
            apply_session_changes_with_hook(&rollouts.changes, |index| {
                hook(MutationPoint::BeforeEachRolloutMutation(index))
            })?;
        journal.applied_rollouts = applied_rollouts;
        for change in &journal.applied_rollouts {
            journal
                .expected_rollout_hashes
                .insert(change.path.clone(), change.next_hash());
        }
        if !skipped_rollouts.is_empty() {
            return Err(CodexxError::Config(format!(
                "有 {} 个会话文件被占用或已发生变化，已停止同步；请退出 Codex 后重新检查会话。",
                skipped_rollouts.len()
            )));
        }
        hook(MutationPoint::AfterRolloutMutation)?;
        validate_rollout_classification(rollouts)?;
        validate_rollout_snapshots(&journal.expected_rollout_hashes)?;
        apply_sqlite_updates(
            pending_sqlite,
            rollouts,
            target_provider,
            catalog_sources,
            syncable_thread_ids,
        )?;
        let sqlite_updates = commit_sqlite_updates(pending_sqlite, journal, hook)?;
        Ok(MutationResult {
            applied_rollouts: journal.applied_rollouts.len(),
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
