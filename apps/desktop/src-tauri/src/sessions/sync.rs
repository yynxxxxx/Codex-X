use super::storage::{
    apply_session_changes, current_model_provider, list_session_previews, normalize_workspace_path,
    restore_session_changes, scan_rollouts, scan_sqlite, sqlite_session_db_paths,
};
use super::types::{RolloutScan, SessionSyncResult, SessionSyncStatus};
use crate::error::{CodexxError, Result};
use crate::file_io::{io_err, json_err, write_json, write_text};
use crate::resolve_codex_dir;
use crate::sqlite_utils::{sqlite_has_table, table_column_set};
use chrono::Local;
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

fn copy_file_to_backup(codex_dir: &Path, backup_dir: &Path, source: &Path) -> Result<()> {
    if !source.exists() {
        return Ok(());
    }
    let target = backup_target_path(codex_dir, backup_dir, source)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    }
    fs::copy(source, &target).map_err(|e| io_err(&target, e))?;
    Ok(())
}

pub(crate) fn backup_sqlite_to_backup(
    codex_dir: &Path,
    backup_dir: &Path,
    source: &Path,
) -> Result<()> {
    use rusqlite::backup::{Backup, StepResult};

    if !source.exists() {
        return Ok(());
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
) -> Result<PathBuf> {
    let root = provider_sync_backup_root(codex_dir);
    fs::create_dir_all(&root).map_err(|e| io_err(&root, e))?;
    let mut backup_dir = root.join(Local::now().format("%Y%m%d%H%M%S").to_string());
    let mut suffix = 0;
    while backup_dir.exists() {
        suffix += 1;
        backup_dir = root.join(format!("{}-{suffix}", Local::now().format("%Y%m%d%H%M%S")));
    }
    fs::create_dir_all(&backup_dir).map_err(|e| io_err(&backup_dir, e))?;

    for name in [
        "config.toml",
        ".codex-global-state.json",
        ".codex-global-state.json.bak",
    ] {
        copy_file_to_backup(codex_dir, &backup_dir, &codex_dir.join(name))?;
    }
    for path in sqlite_session_db_paths(codex_dir) {
        backup_sqlite_to_backup(codex_dir, &backup_dir, &path)?;
    }
    for path in changed_rollouts {
        copy_file_to_backup(codex_dir, &backup_dir, path)?;
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
        }),
    )?;
    Ok(backup_dir)
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

fn apply_sqlite_provider_alignment(
    codex_dir: &Path,
    rollouts: &RolloutScan,
    target_provider: &str,
) -> Result<SqliteUpdateCounts> {
    let mut updated = SqliteUpdateCounts::default();
    for path in sqlite_session_db_paths(codex_dir) {
        if !path.exists() {
            continue;
        }
        let mut conn = Connection::open(&path).map_err(|error| {
            CodexxError::Database(format!("打开 SQLite 失败 {}: {error}", path.display()))
        })?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|error| CodexxError::Database(error.to_string()))?;
        if !sqlite_has_table(&conn, "threads")? {
            continue;
        }
        let cols = table_column_set(&conn, "threads")?;
        if !cols.contains("model_provider") {
            continue;
        }
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| CodexxError::Database(error.to_string()))?;
        let mut counts = SqliteUpdateCounts::default();
        counts.provider_rows = tx
            .execute(
                "UPDATE threads SET model_provider = ?1 WHERE COALESCE(model_provider, '') <> ?1",
                [target_provider],
            )
            .map_err(|error| CodexxError::Database(error.to_string()))?;

        if cols.contains("id") && cols.contains("has_user_event") {
            for thread_id in &rollouts.thread_ids_with_user_events {
                counts.user_event_rows += tx
                    .execute(
                        "UPDATE threads SET has_user_event = 1 WHERE id = ?1 AND COALESCE(has_user_event, 0) <> 1",
                        [thread_id],
                    )
                    .map_err(|error| CodexxError::Database(error.to_string()))?;
            }
        }
        if cols.contains("id") && cols.contains("cwd") {
            for (thread_id, cwd) in &rollouts.cwd_by_thread_id {
                counts.cwd_rows += tx
                    .execute(
                        "UPDATE threads SET cwd = ?1 WHERE id = ?2 AND COALESCE(cwd, '') <> ?1",
                        (cwd, thread_id),
                    )
                    .map_err(|error| CodexxError::Database(error.to_string()))?;
            }
        }
        tx.commit()
            .map_err(|error| CodexxError::Database(error.to_string()))?;
        updated.add(counts);
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

fn apply_global_state_update(path: &Path) -> Result<usize> {
    let mut state = load_global_state(path)?;
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
        write_text(path, &text)?;
        if let Some(parent) = path.parent() {
            let backup = parent.join(".codex-global-state.json.bak");
            write_text(&backup, &text)?;
        }
    }
    Ok(updated)
}

pub(crate) fn session_sync_status_inner(
    config_dir: Option<String>,
    target_provider: Option<String>,
) -> Result<SessionSyncStatus> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    let target = current_model_provider(&codex_dir, target_provider)?;
    let rollouts = scan_rollouts(&codex_dir, &target)?;
    let sqlite = scan_sqlite(&codex_dir, &rollouts, &target)?;
    let global_state_updates =
        count_global_state_updates(&codex_dir.join(".codex-global-state.json"))?;
    let session_limit = sqlite.sqlite_threads.max(50).min(1000);
    let (sessions, session_warnings) =
        list_session_previews(&codex_dir, &rollouts, &target, session_limit)?;
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

pub(crate) fn sync_sessions_provider_inner(
    config_dir: Option<String>,
    target_provider: Option<String>,
) -> Result<SessionSyncResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    fs::create_dir_all(&codex_dir).map_err(|error| io_err(&codex_dir, error))?;
    let target_provider = current_model_provider(&codex_dir, target_provider)?;
    let _maintenance_lock = acquire_session_maintenance_lock(&codex_dir)?;
    let rollouts = scan_rollouts(&codex_dir, &target_provider)?;
    let sqlite = scan_sqlite(&codex_dir, &rollouts, &target_provider)?;
    let global_state_path = codex_dir.join(".codex-global-state.json");
    let global_state_updates = count_global_state_updates(&global_state_path)?;

    if rollouts.changes.is_empty() && sqlite.mismatched_threads == 0 && global_state_updates == 0 {
        let status = session_sync_status_inner(
            Some(codex_dir.display().to_string()),
            Some(target_provider),
        )?;
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
    let backup_dir = create_provider_sync_backup(&codex_dir, &target_provider, &changed_rollouts)?;
    let (applied_rollouts, skipped_rollouts) = apply_session_changes(&rollouts.changes)?;

    let apply_result = (|| -> Result<SqliteUpdateCounts> {
        let sqlite_updates =
            apply_sqlite_provider_alignment(&codex_dir, &rollouts, &target_provider)?;
        apply_global_state_update(&global_state_path)?;
        Ok(sqlite_updates)
    })();
    let sqlite_updates = match apply_result {
        Ok(updates) => updates,
        Err(error) => {
            if let Err(rollback_error) = restore_session_changes(&applied_rollouts) {
                return Err(CodexxError::Config(format!(
                    "同步失败，恢复会话文件时也失败：{error}；{rollback_error}"
                )));
            }
            return Err(error);
        }
    };
    prune_provider_sync_backups(&codex_dir)?;

    let mut status =
        session_sync_status_inner(Some(codex_dir.display().to_string()), Some(target_provider))?;
    status.backup_dir = Some(backup_dir.display().to_string());
    if !skipped_rollouts.is_empty() {
        status.warnings.push(format!(
            "有 {} 个会话正在使用，已跳过；退出 Codex 后再同步即可。",
            skipped_rollouts.len()
        ));
    }
    Ok(SessionSyncResult {
        status,
        updated_rollouts: applied_rollouts.len(),
        updated_threads: sqlite_updates.total(),
        backup_dir: backup_dir.display().to_string(),
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
}
