use super::storage::{
    apply_session_changes, current_model_provider, discover_sqlite_databases,
    ensure_sqlite_discovery_writable, list_session_previews_with_paths, normalize_workspace_path,
    restore_session_changes, scan_rollouts, scan_sqlite_with_paths, SqliteDiscovery,
};
use super::types::{RolloutScan, SessionSyncResult, SessionSyncStatus};
use crate::error::{CodexxError, Result};
use crate::file_io::{atomic_write, io_err, json_err, write_json, write_text};
use crate::resolve_codex_dir;
use crate::sqlite_utils::{sqlite_has_table, table_column_set};
use chrono::Local;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct BackupSnapshot {
    live_path: PathBuf,
    backup_path: PathBuf,
    existed: bool,
}

#[derive(Debug)]
struct ProviderSyncBackup {
    dir: PathBuf,
    snapshots: Vec<BackupSnapshot>,
}

impl ProviderSyncBackup {
    fn snapshot(&self, live_path: &Path) -> Option<&BackupSnapshot> {
        self.snapshots
            .iter()
            .find(|snapshot| snapshot.live_path == live_path)
    }
}

pub(crate) fn provider_sync_backup_root(codex_dir: &Path) -> PathBuf {
    codex_dir.join("backups_state").join("provider-sync")
}

fn backup_relative_path(codex_dir: &Path, source: &Path) -> PathBuf {
    match source.strip_prefix(codex_dir) {
        Ok(relative) if !relative.as_os_str().is_empty() => relative.to_path_buf(),
        _ => {
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(source.to_string_lossy().as_bytes());
            let key = digest[..8]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            PathBuf::from("external").join(key).join(
                source
                    .file_name()
                    .unwrap_or_else(|| std::ffi::OsStr::new("file")),
            )
        }
    }
}

fn backup_target_path(codex_dir: &Path, backup_dir: &Path, source: &Path) -> Result<PathBuf> {
    let target = backup_dir.join(backup_relative_path(codex_dir, source));
    if target == source {
        return Err(CodexxError::Config(format!(
            "拒绝将备份写回源文件: {}",
            source.display()
        )));
    }
    Ok(target)
}

fn copy_file_to_backup(
    codex_dir: &Path,
    backup_dir: &Path,
    source: &Path,
) -> Result<BackupSnapshot> {
    let target = backup_target_path(codex_dir, backup_dir, source)?;
    let existed = source.exists();
    if existed {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
        }
        fs::copy(source, &target).map_err(|e| io_err(&target, e))?;
    }
    Ok(BackupSnapshot {
        live_path: source.to_path_buf(),
        backup_path: target,
        existed,
    })
}

pub(crate) fn backup_sqlite_to_backup(
    codex_dir: &Path,
    backup_dir: &Path,
    source: &Path,
) -> Result<()> {
    use rusqlite::backup::{Backup, StepResult};

    if !source.exists() {
        return Err(CodexxError::Database(format!(
            "SQLite 快照源不存在: {}",
            source.display()
        )));
    }
    let target = backup_target_path(codex_dir, backup_dir, source)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    }
    let from = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| {
        CodexxError::Database(format!("打开 SQLite 备份源失败 {}: {e}", source.display()))
    })?;
    from.busy_timeout(Duration::from_secs(5))
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let mut to = Connection::open(&target).map_err(|e| {
        CodexxError::Database(format!("创建 SQLite 备份失败 {}: {e}", target.display()))
    })?;
    let deadline = Instant::now() + Duration::from_secs(15);
    {
        let backup = Backup::new(&from, &mut to)
            .map_err(|e| CodexxError::Database(format!("初始化 SQLite 快照失败: {e}")))?;
        loop {
            if Instant::now() >= deadline {
                return Err(CodexxError::Database(format!(
                    "SQLite 快照超时: {}",
                    source.display()
                )));
            }
            match backup
                .step(128)
                .map_err(|e| CodexxError::Database(format!("写入 SQLite 快照失败: {e}")))?
            {
                StepResult::Done => break,
                StepResult::More => {}
                StepResult::Busy | StepResult::Locked => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => {}
            }
        }
    }
    let quick_check: String = to
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|e| CodexxError::Database(format!("校验 SQLite 备份失败: {e}")))?;
    if quick_check != "ok" {
        return Err(CodexxError::Database(format!(
            "SQLite 备份校验失败 {}: {quick_check}",
            target.display()
        )));
    }
    if !target.is_file() {
        return Err(CodexxError::Database(format!(
            "SQLite 快照未生成: {}",
            target.display()
        )));
    }
    Ok(())
}

pub(crate) fn prune_provider_sync_backups(codex_dir: &Path) -> Result<()> {
    let root = provider_sync_backup_root(codex_dir);
    if !root.exists() {
        return Ok(());
    }
    let mut dirs = Vec::new();
    for entry in fs::read_dir(&root).map_err(|e| io_err(&root, e))? {
        let entry = entry.map_err(|e| io_err(&root, e))?;
        let path = entry.path();
        let metadata_path = path.join("metadata.json");
        if !path.is_dir() || !metadata_path.exists() {
            continue;
        }
        let is_v2_provider_sync_backup = fs::read_to_string(&metadata_path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .is_some_and(|metadata| {
                metadata.get("managedBy").and_then(Value::as_str)
                    == Some("Codex-X provider sync v2")
            });
        if is_v2_provider_sync_backup {
            dirs.push(path);
        }
    }
    dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    for path in dirs.into_iter().skip(5) {
        let _ = fs::remove_dir_all(path);
    }
    Ok(())
}

fn create_provider_sync_backup(
    codex_dir: &Path,
    target_provider: &str,
    changed_rollouts: &[PathBuf],
    sqlite_paths: &[PathBuf],
) -> Result<ProviderSyncBackup> {
    let root = provider_sync_backup_root(codex_dir);
    fs::create_dir_all(&root).map_err(|e| io_err(&root, e))?;
    let mut backup_dir = root.join(Local::now().format("%Y%m%d%H%M%S").to_string());
    let mut suffix = 0;
    while backup_dir.exists() {
        suffix += 1;
        backup_dir = root.join(format!("{}-{suffix}", Local::now().format("%Y%m%d%H%M%S")));
    }
    fs::create_dir_all(&backup_dir).map_err(|e| io_err(&backup_dir, e))?;

    let mut snapshots = Vec::new();
    for path in sqlite_paths {
        backup_sqlite_to_backup(codex_dir, &backup_dir, path)?;
        snapshots.push(BackupSnapshot {
            live_path: path.clone(),
            backup_path: backup_target_path(codex_dir, &backup_dir, path)?,
            existed: path.exists(),
        });
    }
    for name in [
        "config.toml",
        ".codex-global-state.json",
        ".codex-global-state.json.bak",
    ] {
        snapshots.push(copy_file_to_backup(
            codex_dir,
            &backup_dir,
            &codex_dir.join(name),
        )?);
    }
    for path in changed_rollouts {
        snapshots.push(copy_file_to_backup(codex_dir, &backup_dir, path)?);
    }
    write_json(
        &backup_dir.join("metadata.json"),
        &json!({
            "version": 1,
            "namespace": "provider-sync",
            "managedBy": "Codex-X provider sync v2",
            "codexHome": codex_dir.display().to_string(),
            "targetProvider": target_provider,
            "createdAt": Local::now().to_rfc3339(),
            "changedRolloutFiles": changed_rollouts.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "snapshots": snapshots.iter().map(|snapshot| json!({
                "livePath": snapshot.live_path.display().to_string(),
                "backupPath": snapshot.backup_path.display().to_string(),
                "existed": snapshot.existed,
            })).collect::<Vec<_>>(),
        }),
    )?;
    Ok(ProviderSyncBackup {
        dir: backup_dir,
        snapshots,
    })
}

#[derive(Debug, Default)]
struct SqliteUpdateCounts {
    provider_rows: usize,
    user_event_rows: usize,
    cwd_rows: usize,
}

impl SqliteUpdateCounts {
    fn total(&self) -> usize {
        self.provider_rows + self.user_event_rows + self.cwd_rows
    }

    fn add(&mut self, other: Self) {
        self.provider_rows += other.provider_rows;
        self.user_event_rows += other.user_event_rows;
        self.cwd_rows += other.cwd_rows;
    }
}

#[derive(Debug, Clone)]
struct GlobalStateWrite {
    path: PathBuf,
    original_bytes: Option<Vec<u8>>,
    written_bytes: Vec<u8>,
}

#[derive(Debug, Default)]
struct MutationJournal {
    applied_rollouts: Vec<super::types::SessionFileChange>,
    sqlite_restore_attempts: Vec<SqliteRestoreAttempt>,
    global_writes: Vec<GlobalStateWrite>,
}

#[derive(Debug, Clone)]
struct SqliteRestoreAttempt {
    path: PathBuf,
    expected_data_version: i64,
}

struct PendingSqliteUpdate {
    path: PathBuf,
    conn: Connection,
    observer: Connection,
    columns: HashSet<String>,
    counts: SqliteUpdateCounts,
    transaction_open: bool,
}

fn rollback_open_transactions(updates: &mut [PendingSqliteUpdate]) {
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

fn prepare_sqlite_updates(sqlite_paths: &[PathBuf]) -> Result<Vec<PendingSqliteUpdate>> {
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
            if !columns.contains("model_provider") {
                continue;
            }
            conn.execute_batch("BEGIN IMMEDIATE")
                .map_err(|error| CodexxError::Database(error.to_string()))?;
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
        update.counts.provider_rows = update
            .conn
            .execute(
                "UPDATE threads SET model_provider = ?1 WHERE COALESCE(model_provider, '') <> ?1",
                [target_provider],
            )
            .map_err(|error| CodexxError::Database(error.to_string()))?;

        if update.columns.contains("id") && update.columns.contains("has_user_event") {
            for thread_id in &rollouts.thread_ids_with_user_events {
                update.counts.user_event_rows += update
                    .conn
                    .execute(
                        "UPDATE threads SET has_user_event = 1 WHERE id = ?1 AND COALESCE(has_user_event, 0) <> 1",
                        [thread_id],
                    )
                    .map_err(|error| CodexxError::Database(error.to_string()))?;
            }
        }
        if update.columns.contains("id") && update.columns.contains("cwd") {
            for (thread_id, cwd) in &rollouts.cwd_by_thread_id {
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
fn load_global_state(path: &Path) -> Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let text = fs::read_to_string(path).map_err(|error| io_err(path, error))?;
    let value = serde_json::from_str::<Value>(&text).map_err(|error| json_err(path, error))?;
    Ok(value.as_object().cloned().unwrap_or_default())
}

pub(super) fn projectless_thread_ids(path: &Path) -> Result<HashSet<String>> {
    let state = load_global_state(path)?;
    Ok(state
        .get("projectless-thread-ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
        .collect())
}

fn normalized_path_array(value: &Value) -> Vec<String> {
    if let Some(items) = value.as_array() {
        items
            .iter()
            .filter_map(Value::as_str)
            .filter_map(normalize_workspace_path)
            .collect()
    } else {
        value
            .as_str()
            .and_then(normalize_workspace_path)
            .into_iter()
            .collect()
    }
}

fn dedupe_workspace_paths(paths: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| {
            seen.insert(
                path.replace('/', r"\")
                    .trim_end_matches('\\')
                    .to_ascii_lowercase(),
            )
        })
        .collect()
}

fn normalized_global_state(state: &Map<String, Value>) -> Map<String, Value> {
    let mut next = Map::new();
    for key in ["electron-saved-workspace-roots", "project-order"] {
        if let Some(value) = state.get(key) {
            next.insert(
                key.to_string(),
                json!(dedupe_workspace_paths(normalized_path_array(value))),
            );
        }
    }
    if let Some(value) = state.get("active-workspace-roots") {
        let normalized = dedupe_workspace_paths(normalized_path_array(value));
        let next_value = if value.is_array() {
            json!(normalized)
        } else if let Some(first) = normalized.first() {
            json!(first)
        } else {
            value.clone()
        };
        next.insert("active-workspace-roots".to_string(), next_value);
    }
    if let Some(labels) = state
        .get("electron-workspace-root-labels")
        .and_then(Value::as_object)
    {
        let mut normalized = Map::new();
        for (path, value) in labels {
            normalized.insert(
                normalize_workspace_path(path).unwrap_or_else(|| path.clone()),
                value.clone(),
            );
        }
        next.insert(
            "electron-workspace-root-labels".to_string(),
            Value::Object(normalized),
        );
    }
    if let Some(open_targets) = state
        .get("open-in-target-preferences")
        .and_then(Value::as_object)
    {
        let mut normalized = open_targets.clone();
        if let Some(per_path) = open_targets.get("perPath").and_then(Value::as_object) {
            let mut normalized_per_path = Map::new();
            for (path, value) in per_path {
                normalized_per_path.insert(
                    normalize_workspace_path(path).unwrap_or_else(|| path.clone()),
                    value.clone(),
                );
            }
            normalized.insert("perPath".to_string(), Value::Object(normalized_per_path));
        }
        next.insert(
            "open-in-target-preferences".to_string(),
            Value::Object(normalized),
        );
    }
    next
}

fn count_global_state_updates(path: &Path) -> Result<usize> {
    let state = load_global_state(path)?;
    let next = normalized_global_state(&state);
    Ok(next
        .iter()
        .filter(|(key, value)| state.get(*key) != Some(*value))
        .count())
}

#[cfg(test)]
fn apply_global_state_update(path: &Path) -> Result<usize> {
    apply_global_state_update_with_journal(path, &mut MutationJournal::default(), &mut |_| Ok(()))
}

fn read_optional_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_err(path, error)),
    }
}

fn apply_global_state_update_with_journal<F>(
    path: &Path,
    journal: &mut MutationJournal,
    hook: &mut F,
) -> Result<usize>
where
    F: FnMut(MutationPoint) -> Result<()>,
{
    let original_bytes = read_optional_bytes(path)?;
    let mut state = match &original_bytes {
        Some(bytes) => serde_json::from_slice::<Value>(bytes)
            .map_err(|error| json_err(path, error))?
            .as_object()
            .cloned()
            .unwrap_or_default(),
        None => Map::new(),
    };
    let next = normalized_global_state(&state);
    let updated = next
        .iter()
        .filter(|(key, value)| state.get(*key) != Some(*value))
        .count();
    if updated > 0 {
        for (key, value) in next {
            state.insert(key, value);
        }
        let text = serde_json::to_string_pretty(&Value::Object(state))
            .map_err(|error| json_err(path, error))?;
        let written_bytes = text.as_bytes().to_vec();
        if read_optional_bytes(path)? != original_bytes {
            return Err(CodexxError::Config(
                "全局状态已发生变化，请重试。".to_string(),
            ));
        }
        write_text(path, &text)?;
        journal.global_writes.push(GlobalStateWrite {
            path: path.to_path_buf(),
            original_bytes,
            written_bytes: written_bytes.clone(),
        });
        hook(MutationPoint::AfterGlobalMainWrite)?;
        if let Some(parent) = path.parent() {
            let backup = parent.join(".codex-global-state.json.bak");
            let original_bytes = read_optional_bytes(&backup)?;
            if read_optional_bytes(&backup)? != original_bytes {
                return Err(CodexxError::Config(
                    "全局状态已发生变化，请重试。".to_string(),
                ));
            }
            write_text(&backup, &text)?;
            journal.global_writes.push(GlobalStateWrite {
                path: backup,
                original_bytes,
                written_bytes,
            });
        }
    }
    Ok(updated)
}

fn restore_global_write(write: &GlobalStateWrite) -> Result<()> {
    match fs::read(&write.path) {
        Ok(current) if current == write.written_bytes => {}
        Ok(_) => {
            return Err(CodexxError::Config(format!(
                "全局状态已发生变化，无法安全恢复: {}",
                write.path.display()
            )));
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound && write.original_bytes.is_none() =>
        {
            return Ok(());
        }
        Err(error) => return Err(io_err(&write.path, error)),
    }

    if let Some(original) = &write.original_bytes {
        atomic_write(&write.path, original)
    } else {
        fs::remove_file(&write.path).map_err(|error| io_err(&write.path, error))
    }
}

fn restore_sqlite_snapshot(snapshot: &BackupSnapshot) -> Result<()> {
    use rusqlite::backup::{Backup, StepResult};

    if !snapshot.existed {
        return Err(CodexxError::Database(format!(
            "SQLite 原始快照不存在: {}",
            snapshot.live_path.display()
        )));
    }
    let from = Connection::open_with_flags(
        &snapshot.backup_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        CodexxError::Database(format!(
            "打开 SQLite 恢复源失败 {}: {error}",
            snapshot.backup_path.display()
        ))
    })?;
    let mut to = Connection::open_with_flags(
        &snapshot.live_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        CodexxError::Database(format!(
            "打开 SQLite 恢复目标失败 {}: {error}",
            snapshot.live_path.display()
        ))
    })?;
    to.busy_timeout(Duration::from_secs(5))
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    {
        let sqlite_backup = Backup::new(&from, &mut to)
            .map_err(|error| CodexxError::Database(format!("初始化 SQLite 恢复失败: {error}")))?;
        loop {
            if Instant::now() >= deadline {
                return Err(CodexxError::Database(format!(
                    "SQLite 恢复超时: {}",
                    snapshot.live_path.display()
                )));
            }
            match sqlite_backup
                .step(128)
                .map_err(|error| CodexxError::Database(format!("恢复 SQLite 失败: {error}")))?
            {
                StepResult::Done => break,
                StepResult::More => {}
                StepResult::Busy | StepResult::Locked => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => {}
            }
        }
    }
    let quick_check: String = to
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|error| CodexxError::Database(format!("校验 SQLite 恢复失败: {error}")))?;
    if quick_check != "ok" {
        return Err(CodexxError::Database(format!(
            "SQLite 恢复校验失败 {}: {quick_check}",
            snapshot.live_path.display()
        )));
    }
    let _ = to.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
    Ok(())
}

fn rollback_mutation(
    backup: &ProviderSyncBackup,
    journal: &MutationJournal,
    pending_sqlite: &[PendingSqliteUpdate],
) -> Vec<String> {
    let mut errors = Vec::new();
    for write in journal.global_writes.iter().rev() {
        if let Err(error) = restore_global_write(write) {
            errors.push(error.to_string());
        }
    }
    for attempt in journal.sqlite_restore_attempts.iter().rev() {
        let Some(update) = pending_sqlite
            .iter()
            .find(|update| update.path == attempt.path)
        else {
            errors.push(format!("缺少 SQLite 恢复连接: {}", attempt.path.display()));
            continue;
        };
        match sqlite_data_version(&update.observer) {
            Ok(current) if current != attempt.expected_data_version => {
                errors.push(format!(
                    "会话数据库已发生变化，已保留备份且未覆盖: {}",
                    attempt.path.display()
                ));
                continue;
            }
            Ok(_) => {}
            Err(error) => {
                errors.push(error.to_string());
                continue;
            }
        }
        match backup.snapshot(&attempt.path) {
            Some(snapshot) => {
                if let Err(error) = restore_sqlite_snapshot(snapshot) {
                    errors.push(error.to_string());
                }
            }
            None => errors.push(format!("缺少 SQLite 备份: {}", attempt.path.display())),
        }
    }
    if let Err(error) = restore_session_changes(&journal.applied_rollouts) {
        errors.push(error.to_string());
    }
    errors
}

fn mutation_error(original: CodexxError, recovery_errors: Vec<String>) -> CodexxError {
    if recovery_errors.is_empty() {
        original
    } else {
        CodexxError::Config(format!(
            "同步失败，自动恢复也未完成：{original}；{}",
            recovery_errors.join("；")
        ))
    }
}

pub(crate) fn session_sync_status_inner(
    config_dir: Option<String>,
    target_provider: Option<String>,
) -> Result<SessionSyncStatus> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    let target = current_model_provider(&codex_dir, target_provider)?;
    let discovery = discover_sqlite_databases(&codex_dir);
    session_sync_status_with_discovery(&codex_dir, target, &discovery)
}

pub(super) fn session_sync_status_with_discovery(
    codex_dir: &Path,
    target: String,
    discovery: &SqliteDiscovery,
) -> Result<SessionSyncStatus> {
    let rollouts = scan_rollouts(&codex_dir, &target)?;
    let sqlite = scan_sqlite_with_paths(&discovery.session_paths, &rollouts, &target)?;
    let global_state_updates =
        count_global_state_updates(&codex_dir.join(".codex-global-state.json"))?;
    let session_limit = sqlite.sqlite_threads.max(50).min(1000);
    let preview_paths = discovery.active_first_session_paths();
    let (sessions, session_warnings) =
        list_session_previews_with_paths(&preview_paths, &rollouts, &target, session_limit)?;
    let mut warnings = rollouts.warnings;
    warnings.extend(sqlite.warnings);
    warnings.extend(session_warnings);
    Ok(SessionSyncStatus {
        codex_dir: codex_dir.display().to_string(),
        target_provider: target,
        rollout_files: rollouts.rollout_files,
        session_meta_count: rollouts.session_meta_count,
        mismatched_rollouts: rollouts.mismatched_rollouts,
        mismatched_session_meta: rollouts.mismatched_session_meta,
        sqlite_dbs: sqlite.sqlite_dbs,
        sqlite_threads: sqlite.sqlite_threads,
        top_level_threads: sqlite.top_level_threads,
        subagent_threads: sqlite.subagent_threads,
        mismatched_threads: sqlite.mismatched_threads,
        needs_sync: rollouts.mismatched_rollouts > 0
            || sqlite.mismatched_threads > 0
            || global_state_updates > 0,
        backup_dir: None,
        warnings,
        sessions,
    })
}

pub(super) struct SessionMaintenanceLock {
    file: fs::File,
}

impl Drop for SessionMaintenanceLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub(super) fn acquire_session_maintenance_lock(codex_dir: &Path) -> Result<SessionMaintenanceLock> {
    let tmp_dir = codex_dir.join("tmp");
    fs::create_dir_all(&tmp_dir).map_err(|e| io_err(&tmp_dir, e))?;
    let legacy_lock = tmp_dir.join("provider-sync.lock");
    if legacy_lock.exists() {
        return Err(CodexxError::Config(format!(
            "会话维护正在进行: {}",
            legacy_lock.display()
        )));
    }
    let path = tmp_dir.join("session-maintenance.lock");
    if path.is_dir() {
        return Err(CodexxError::Config(format!(
            "检测到旧版会话维护锁，请确认没有其他 Codex-X 正在维护会话后删除: {}",
            path.display()
        )));
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| io_err(&path, e))?;
    file.try_lock()
        .map_err(|_| CodexxError::Config(format!("会话维护正在进行: {}", path.display())))?;
    file.set_len(0).map_err(|e| io_err(&path, e))?;
    write!(file, "pid={}\n", std::process::id()).map_err(|e| io_err(&path, e))?;
    file.sync_all().map_err(|e| io_err(&path, e))?;
    Ok(SessionMaintenanceLock { file })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MutationPoint {
    AfterSqliteCommit(usize),
    AfterGlobalMainWrite,
}

struct MutationResult {
    applied_rollouts: usize,
    skipped_rollouts: Vec<PathBuf>,
    sqlite_updates: SqliteUpdateCounts,
}

fn execute_provider_sync_mutation<F>(
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
        apply_global_state_update_with_journal(global_state_path, journal, hook)?;
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

pub(crate) fn sync_sessions_provider_inner(
    config_dir: Option<String>,
    target_provider: Option<String>,
) -> Result<SessionSyncResult> {
    sync_sessions_provider_with_hook(config_dir, target_provider, |_| Ok(()))
}

fn sync_sessions_provider_with_hook<F>(
    config_dir: Option<String>,
    target_provider: Option<String>,
    mut hook: F,
) -> Result<SessionSyncResult>
where
    F: FnMut(MutationPoint) -> Result<()>,
{
    let codex_dir = resolve_codex_dir(config_dir)?;
    fs::create_dir_all(&codex_dir).map_err(|error| io_err(&codex_dir, error))?;
    let target_provider = current_model_provider(&codex_dir, target_provider)?;
    let _maintenance_lock = acquire_session_maintenance_lock(&codex_dir)?;
    let discovery = discover_sqlite_databases(&codex_dir);
    ensure_sqlite_discovery_writable(&discovery)?;
    let rollouts = scan_rollouts(&codex_dir, &target_provider)?;
    let sqlite = scan_sqlite_with_paths(&discovery.session_paths, &rollouts, &target_provider)?;
    let global_state_path = codex_dir.join(".codex-global-state.json");
    let global_state_updates = count_global_state_updates(&global_state_path)?;

    if rollouts.changes.is_empty() && sqlite.mismatched_threads == 0 && global_state_updates == 0 {
        let status = session_sync_status_with_discovery(&codex_dir, target_provider, &discovery)?;
        return Ok(SessionSyncResult {
            status,
            updated_rollouts: 0,
            updated_threads: 0,
            backup_dir: String::new(),
        });
    }

    let changed_rollouts = rollouts
        .changes
        .iter()
        .map(|change| change.path.clone())
        .collect::<Vec<_>>();
    let mut pending_sqlite = prepare_sqlite_updates(&discovery.session_paths)?;
    let sqlite_snapshot_paths = pending_sqlite
        .iter()
        .map(|update| update.path.clone())
        .collect::<Vec<_>>();
    let backup = match create_provider_sync_backup(
        &codex_dir,
        &target_provider,
        &changed_rollouts,
        &sqlite_snapshot_paths,
    ) {
        Ok(backup) => backup,
        Err(error) => {
            rollback_open_transactions(&mut pending_sqlite);
            return Err(error);
        }
    };
    let mut journal = MutationJournal::default();
    let mutation = execute_provider_sync_mutation(
        &rollouts,
        &mut pending_sqlite,
        &target_provider,
        &global_state_path,
        &mut journal,
        &mut hook,
    );
    let mutation = match mutation {
        Ok(result) => result,
        Err(error) => {
            let recovery_errors = rollback_mutation(&backup, &journal, &pending_sqlite);
            return Err(mutation_error(error, recovery_errors));
        }
    };

    let prune_warning = prune_provider_sync_backups(&codex_dir).err();
    let mut status = session_sync_status_with_discovery(&codex_dir, target_provider, &discovery)
        .map_err(|error| {
            CodexxError::Config(format!(
                "同步已完成，但刷新会话列表失败，请重新进入页面：{error}"
            ))
        })?;
    status.backup_dir = Some(backup.dir.display().to_string());
    if prune_warning.is_some() {
        status
            .warnings
            .push("同步已完成，但旧备份暂未清理。".to_string());
    }
    if !mutation.skipped_rollouts.is_empty() {
        status.warnings.push(format!(
            "有 {} 个会话正在使用，已跳过；退出 Codex 后再同步即可。",
            mutation.skipped_rollouts.len()
        ));
    }
    Ok(SessionSyncResult {
        status,
        updated_rollouts: mutation.applied_rollouts,
        updated_threads: mutation.sqlite_updates.total(),
        backup_dir: backup.dir.display().to_string(),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "codex-x-session-sync-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test directory");
        path
    }

    fn write_rollout(path: &Path, id: &str) -> Vec<u8> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create rollout parent");
        }
        let content = format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"model_provider\":\"openai\",\"cwd\":\"/tmp/project\"}}}}\n"
        );
        fs::write(path, content.as_bytes()).expect("write rollout");
        content.into_bytes()
    }

    fn create_thread_database(path: &Path, id: &str, rollout: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create sqlite parent");
        }
        let conn = Connection::open(path).expect("create thread database");
        conn.execute_batch(
            "CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                model_provider TEXT NOT NULL,
                rollout_path TEXT
             );",
        )
        .expect("create threads table");
        conn.execute(
            "INSERT INTO threads (id, model_provider, rollout_path)
             VALUES (?1, 'openai', ?2)",
            (id, rollout.display().to_string()),
        )
        .expect("insert thread");
    }

    fn thread_provider(path: &Path, id: &str) -> String {
        Connection::open(path)
            .expect("open thread database")
            .query_row(
                "SELECT model_provider FROM threads WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .expect("read thread provider")
    }

    fn sqlite_quick_check(path: &Path) -> String {
        Connection::open(path)
            .expect("open sqlite for quick check")
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .expect("run quick check")
    }

    #[test]
    fn global_state_update_keeps_unknown_fields_and_matches_backup() {
        let dir = temp_dir("global-state");
        let path = dir.join(".codex-global-state.json");
        fs::write(
            &path,
            r#"{
  "electron-saved-workspace-roots": "/tmp/project",
  "unrelated-setting": { "enabled": true }
}"#,
        )
        .expect("write original global state");

        assert_eq!(
            apply_global_state_update(&path).expect("update global state"),
            1
        );

        let main_text = fs::read_to_string(&path).expect("read global state");
        let backup_path = dir.join(".codex-global-state.json.bak");
        let backup_text = fs::read_to_string(&backup_path).expect("read global state backup");
        assert_eq!(main_text, backup_text);
        let state: Value = serde_json::from_str(&main_text).expect("parse global state");
        assert_eq!(
            state.get("electron-saved-workspace-roots"),
            Some(&json!(["/tmp/project"]))
        );
        assert_eq!(
            state.get("unrelated-setting"),
            Some(&json!({ "enabled": true }))
        );

        fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn sqlite_update_failure_rolls_back_every_open_database_and_jsonl() {
        let codex_dir = temp_dir("multi-sqlite-update-failure");
        let id = "019f6000-0000-7000-8000-000000000401";
        let rollout = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
        let original_rollout = write_rollout(&rollout, id);
        let first = codex_dir.join("sqlite/custom.db");
        let second = codex_dir.join("state_10.sqlite");
        create_thread_database(&first, id, &rollout);
        create_thread_database(&second, id, &rollout);
        Connection::open(&second)
            .expect("open failing database")
            .execute_batch(
                "CREATE TRIGGER reject_provider_update
                 BEFORE UPDATE OF model_provider ON threads
                 BEGIN SELECT RAISE(ABORT, 'provider update blocked'); END;",
            )
            .expect("install rejecting trigger");

        let error = sync_sessions_provider_inner(
            Some(codex_dir.display().to_string()),
            Some("custom".to_string()),
        )
        .expect_err("second database update must fail");
        assert!(error.to_string().contains("provider update blocked"));
        assert_eq!(thread_provider(&first, id), "openai");
        assert_eq!(thread_provider(&second, id), "openai");
        assert_eq!(
            fs::read(&rollout).expect("read restored rollout"),
            original_rollout
        );

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn sqlite_commit_failpoint_restores_committed_and_uncommitted_databases() {
        let codex_dir = temp_dir("sqlite-commit-failpoint");
        let id = "019f6000-0000-7000-8000-000000000406";
        let rollout = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
        let original_rollout = write_rollout(&rollout, id);
        let first = codex_dir.join("sqlite/custom.db");
        let second = codex_dir.join("state_10.sqlite");
        create_thread_database(&first, id, &rollout);
        create_thread_database(&second, id, &rollout);

        let error = sync_sessions_provider_with_hook(
            Some(codex_dir.display().to_string()),
            Some("custom".to_string()),
            |point| match point {
                MutationPoint::AfterSqliteCommit(0) => {
                    Err(CodexxError::Config("提交后注入失败".to_string()))
                }
                _ => Ok(()),
            },
        )
        .expect_err("fail after first sqlite commit");

        assert_eq!(error.to_string(), "配置错误: 提交后注入失败");
        assert_eq!(thread_provider(&first, id), "openai");
        assert_eq!(thread_provider(&second, id), "openai");
        assert_eq!(
            fs::read(&rollout).expect("read restored rollout"),
            original_rollout
        );

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn failed_sqlite_commit_rolls_back_without_snapshot_restore() {
        let codex_dir = temp_dir("failed-sqlite-commit");
        let id = "019f6000-0000-7000-8000-000000000407";
        let rollout = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
        write_rollout(&rollout, id);
        let database = codex_dir.join("state_10.sqlite");
        create_thread_database(&database, id, &rollout);
        Connection::open(&database)
            .expect("open database for deferred constraint")
            .execute_batch(
                "CREATE TABLE parent (id TEXT PRIMARY KEY);
                 CREATE TABLE child (
                    parent_id TEXT REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED
                 );",
            )
            .expect("create deferred foreign key");

        let conn = Connection::open(&database).expect("open writer");
        conn.pragma_update(None, "foreign_keys", true)
            .expect("enable foreign keys");
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             INSERT INTO child (parent_id) VALUES ('missing');",
        )
        .expect("defer invalid foreign key until commit");
        let observer = Connection::open_with_flags(
            &database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("open observer");
        let mut pending = vec![PendingSqliteUpdate {
            path: database.clone(),
            conn,
            observer,
            columns: HashSet::new(),
            counts: SqliteUpdateCounts::default(),
            transaction_open: true,
        }];
        let mut journal = MutationJournal::default();

        let error = commit_sqlite_updates(&mut pending, &mut journal, &mut |_| Ok(()))
            .expect_err("deferred foreign key must reject commit");

        assert!(error.to_string().contains("FOREIGN KEY constraint failed"));
        assert!(journal.sqlite_restore_attempts.is_empty());
        assert!(!pending[0].transaction_open);
        let child_rows: i64 = Connection::open(&database)
            .expect("reopen database")
            .query_row("SELECT COUNT(*) FROM child", [], |row| row.get(0))
            .expect("count rolled-back child rows");
        assert_eq!(child_rows, 0);

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn sqlite_restore_does_not_overwrite_new_codex_writes() {
        let codex_dir = temp_dir("sqlite-concurrent-write");
        let id = "019f6000-0000-7000-8000-000000000409";
        let rollout = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
        let original_rollout = write_rollout(&rollout, id);
        let database = codex_dir.join("state_10.sqlite");
        create_thread_database(&database, id, &rollout);

        let error = sync_sessions_provider_with_hook(
            Some(codex_dir.display().to_string()),
            Some("custom".to_string()),
            |point| match point {
                MutationPoint::AfterSqliteCommit(0) => {
                    Connection::open(&database)
                        .expect("open concurrent Codex writer")
                        .execute_batch(
                            "CREATE TABLE concurrent_marker (value TEXT NOT NULL);
                             INSERT INTO concurrent_marker (value) VALUES ('keep-new-write');",
                        )
                        .expect("write after provider sync commit");
                    Err(CodexxError::Config("并发写入后注入失败".to_string()))
                }
                _ => Ok(()),
            },
        )
        .expect_err("recovery must detect the concurrent write");

        assert!(error.to_string().contains("会话数据库已发生变化"));
        assert_eq!(thread_provider(&database, id), "custom");
        let marker: String = Connection::open(&database)
            .expect("open database with concurrent write")
            .query_row("SELECT value FROM concurrent_marker", [], |row| row.get(0))
            .expect("read concurrent marker");
        assert_eq!(marker, "keep-new-write");
        assert_eq!(
            fs::read(&rollout).expect("read restored rollout"),
            original_rollout
        );
        assert!(fs::read_dir(provider_sync_backup_root(&codex_dir))
            .expect("read retained provider sync backup")
            .next()
            .is_some());

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[cfg(unix)]
    #[test]
    fn sqlite_prepare_deduplicates_symlink_aliases() {
        use std::os::unix::fs::symlink;

        let codex_dir = temp_dir("sqlite-symlink-alias");
        let id = "019f6000-0000-7000-8000-000000000410";
        let rollout = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
        write_rollout(&rollout, id);
        let database = codex_dir.join("state_10.sqlite");
        let alias = codex_dir.join("state-alias.sqlite");
        create_thread_database(&database, id, &rollout);
        symlink(&database, &alias).expect("create database symlink");

        let mut pending = prepare_sqlite_updates(&[database.clone(), alias])
            .expect("prepare aliased database once");

        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].path,
            database.canonicalize().expect("canonical db")
        );
        rollback_open_transactions(&mut pending);

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn global_main_failpoint_restores_prior_mutations_without_touching_backup() {
        let codex_dir = temp_dir("global-main-failpoint");
        let id = "019f6000-0000-7000-8000-000000000408";
        let rollout = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
        let original_rollout = write_rollout(&rollout, id);
        let database = codex_dir.join("state_10.sqlite");
        create_thread_database(&database, id, &rollout);
        let global = codex_dir.join(".codex-global-state.json");
        let global_backup = codex_dir.join(".codex-global-state.json.bak");
        let original_global = br#"{"electron-saved-workspace-roots":"/tmp/project"}"#;
        fs::write(&global, original_global).expect("write original global state");
        assert!(!global_backup.exists());

        let error = sync_sessions_provider_with_hook(
            Some(codex_dir.display().to_string()),
            Some("custom".to_string()),
            |point| match point {
                MutationPoint::AfterGlobalMainWrite => {
                    assert_eq!(thread_provider(&database, id), "openai");
                    assert!(fs::read_to_string(&rollout)
                        .expect("read mutated rollout")
                        .contains("\"model_provider\":\"custom\""));
                    assert!(!global_backup.exists());
                    Err(CodexxError::Config("主状态写入后注入失败".to_string()))
                }
                _ => Ok(()),
            },
        )
        .expect_err("fail after global main write");

        assert_eq!(error.to_string(), "配置错误: 主状态写入后注入失败");
        assert_eq!(thread_provider(&database, id), "openai");
        assert_eq!(
            fs::read(&rollout).expect("read restored rollout"),
            original_rollout
        );
        assert_eq!(
            fs::read(&global).expect("read restored global"),
            original_global
        );
        assert!(!global_backup.exists());

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn injected_failure_restores_sqlite_jsonl_and_global_state() {
        let codex_dir = temp_dir("full-mutation-rollback");
        let id = "019f6000-0000-7000-8000-000000000411";
        let rollout = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
        let original_rollout = write_rollout(&rollout, id);
        let database = codex_dir.join("state_10.sqlite");
        create_thread_database(&database, id, &rollout);
        let wal_guard = Connection::open(&database).expect("open database for WAL mode");
        wal_guard
            .pragma_update(None, "journal_mode", "WAL")
            .expect("enable WAL mode");
        wal_guard
            .execute_batch(
                "CREATE TABLE rollback_marker (value TEXT NOT NULL);
                 INSERT INTO rollback_marker (value) VALUES ('keep-me');",
            )
            .expect("write WAL marker");
        let wal_path = PathBuf::from(format!("{}-wal", database.display()));
        assert!(wal_path.exists());
        let global = codex_dir.join(".codex-global-state.json");
        let global_backup = codex_dir.join(".codex-global-state.json.bak");
        let original_global = br#"{
  "electron-saved-workspace-roots": "/tmp/project",
  "unrelated-setting": true
}"#;
        fs::write(&global, original_global).expect("write original global state");
        assert!(!global_backup.exists());

        let mut hook_called = false;
        let error = sync_sessions_provider_with_hook(
            Some(codex_dir.display().to_string()),
            Some("custom".to_string()),
            |point| match point {
                MutationPoint::AfterSqliteCommit(0) => {
                    hook_called = true;
                    assert_eq!(thread_provider(&database, id), "custom");
                    assert!(fs::read_to_string(&rollout)
                        .expect("read mutated rollout")
                        .contains("\"model_provider\":\"custom\""));
                    let main = fs::read_to_string(&global).expect("read mutated global state");
                    let backup =
                        fs::read_to_string(&global_backup).expect("read mutated global backup");
                    assert_eq!(main, backup);
                    Err(CodexxError::Config("测试注入失败".to_string()))
                }
                _ => Ok(()),
            },
        )
        .expect_err("hook must fail after all writes");

        assert!(hook_called);
        assert_eq!(error.to_string(), "配置错误: 测试注入失败");
        assert_eq!(thread_provider(&database, id), "openai");
        assert_eq!(sqlite_quick_check(&database), "ok");
        let marker: String = Connection::open(&database)
            .expect("open restored database")
            .query_row("SELECT value FROM rollback_marker", [], |row| row.get(0))
            .expect("read restored WAL marker");
        assert_eq!(marker, "keep-me");
        assert_eq!(
            fs::read(&rollout).expect("read restored rollout"),
            original_rollout
        );
        assert_eq!(
            fs::read(&global).expect("read restored global"),
            original_global
        );
        assert!(!global_backup.exists());
        assert!(fs::read_dir(provider_sync_backup_root(&codex_dir))
            .expect("read retained provider sync backup")
            .next()
            .is_some());

        drop(wal_guard);
        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn rollback_restores_distinct_existing_global_main_and_backup() {
        let codex_dir = temp_dir("distinct-global-snapshots");
        let main = codex_dir.join(".codex-global-state.json");
        let live_backup = codex_dir.join(".codex-global-state.json.bak");
        let original_main = br#"{"source":"main","value":1}"#;
        let original_backup = br#"{"source":"backup","value":2}"#;
        fs::write(&main, original_main).expect("write original main");
        fs::write(&live_backup, original_backup).expect("write original backup");

        let snapshot_dir = codex_dir.join("provider-sync-snapshot");
        fs::create_dir_all(&snapshot_dir).expect("create snapshot directory");
        let backup = ProviderSyncBackup {
            dir: snapshot_dir,
            snapshots: Vec::new(),
        };
        let written_text = r#"{"source":"mutated"}"#.to_string();
        write_text(&main, &written_text).expect("mutate global main");
        write_text(&live_backup, &written_text).expect("mutate global backup");
        let journal = MutationJournal {
            global_writes: vec![
                GlobalStateWrite {
                    path: main.clone(),
                    original_bytes: Some(original_main.to_vec()),
                    written_bytes: written_text.as_bytes().to_vec(),
                },
                GlobalStateWrite {
                    path: live_backup.clone(),
                    original_bytes: Some(original_backup.to_vec()),
                    written_bytes: written_text.into_bytes(),
                },
            ],
            ..MutationJournal::default()
        };

        assert!(rollback_mutation(&backup, &journal, &[]).is_empty());
        assert_eq!(fs::read(&main).expect("read restored main"), original_main);
        assert_eq!(
            fs::read(&live_backup).expect("read restored backup"),
            original_backup
        );

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }
}
