use super::backup::{create_provider_sync_backup, prune_provider_sync_backups};
use super::global_state::count_global_state_updates;
use super::storage::{
    current_model_provider, discover_sqlite_databases, ensure_sqlite_discovery_writable,
    list_session_previews_with_paths, scan_rollouts, scan_sqlite_with_paths, SqliteDiscovery,
};
use super::transaction::{
    execute_provider_sync_mutation, mutation_error, prepare_sqlite_updates, rollback_mutation,
    rollback_open_transactions, MutationJournal, MutationPoint,
};
use super::types::{SessionSyncResult, SessionSyncStatus};
use crate::error::{CodexxError, Result};
use crate::file_io::{ensure_directory, io_err};
use crate::resolve_codex_dir;
use std::fs;
use std::io::Write;
use std::path::Path;

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
    let rollouts = scan_rollouts(codex_dir, &target)?;
    let sqlite = scan_sqlite_with_paths(&discovery.session_paths, &rollouts, &target)?;
    let global_state_updates =
        count_global_state_updates(&codex_dir.join(".codex-global-state.json"))?;
    let session_limit = sqlite.sqlite_threads.clamp(50, 1000);
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
    ensure_directory(&tmp_dir)?;
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
    writeln!(file, "pid={}", std::process::id()).map_err(|e| io_err(&path, e))?;
    file.sync_all().map_err(|e| io_err(&path, e))?;
    Ok(SessionMaintenanceLock { file })
}

pub(crate) fn sync_sessions_provider_inner(
    config_dir: Option<String>,
    target_provider: Option<String>,
) -> Result<SessionSyncResult> {
    sync_sessions_provider_with_hook(config_dir, target_provider, |_| Ok(()))
}

pub(super) fn sync_sessions_provider_with_hook<F>(
    config_dir: Option<String>,
    target_provider: Option<String>,
    mut hook: F,
) -> Result<SessionSyncResult>
where
    F: FnMut(MutationPoint) -> Result<()>,
{
    let codex_dir = resolve_codex_dir(config_dir)?;
    ensure_directory(&codex_dir)?;
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
        .map(|update| update.path().to_path_buf())
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
            let recovery_errors = rollback_mutation(&journal, &mut pending_sqlite);
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
