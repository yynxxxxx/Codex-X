use super::backup::{create_provider_sync_backup, prune_provider_sync_backups};
use super::catalog::{scan_catalog_sync, CatalogSyncScan};
use super::maintenance::run_with_stopped_desktop;
use super::storage::{
    current_model_provider, discover_sqlite_databases, ensure_sqlite_discovery_writable,
    list_session_previews_with_paths, scan_provider_rollouts, scan_rollouts_for_thread_ids,
    scan_sqlite_with_paths, SqliteDiscovery,
};
use super::transaction::{
    execute_provider_sync_mutation, mutation_error, prepare_sqlite_updates, rollback_mutation,
    rollback_open_transactions, MutationJournal, MutationPoint,
};
use super::types::{RolloutScan, SessionSyncResult, SessionSyncStatus, SqliteScan};
use crate::error::{CodexxError, Result};
use crate::file_io::{ensure_directory, io_err};
use crate::platform;
use crate::resolve_codex_dir;
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::Path;

fn scan_failure_error(failures: &[String]) -> CodexxError {
    CodexxError::Config(
        failures
            .first()
            .cloned()
            .unwrap_or_else(|| "无法确认当前会话同步状态。".to_string()),
    )
}

fn scan_provider_buckets(
    codex_dir: &Path,
    target_provider: &str,
    sqlite: &SqliteScan,
) -> Result<RolloutScan> {
    let mut rollouts = scan_rollouts_for_thread_ids(
        codex_dir,
        target_provider,
        &sqlite.syncable_thread_ids,
        &sqlite.rollout_paths_by_thread_id,
    )?;
    // Provider synchronization must not repair or rewrite the independent cwd index.
    rollouts.cwd_by_thread_id.clear();
    Ok(rollouts)
}

fn scan_legacy_index_warnings(
    codex_dir: &Path,
    target_provider: &str,
    discovery: &SqliteDiscovery,
) -> Vec<String> {
    if discovery.active_paths.is_empty() {
        return Vec::new();
    }
    let legacy_paths = discovery
        .thread_paths
        .iter()
        .filter(|path| !discovery.active_paths.contains(path))
        .cloned()
        .collect::<Vec<_>>();
    if legacy_paths.is_empty() {
        return Vec::new();
    }
    let failures = scan_sqlite_with_paths(&legacy_paths, &RolloutScan::default(), target_provider)
        .and_then(|sqlite| scan_provider_buckets(codex_dir, target_provider, &sqlite))
        .map(|scan| scan.scan_failures)
        .unwrap_or_else(|error| vec![error.to_string()]);
    failures
        .into_iter()
        .map(|failure| format!("已忽略旧会话索引异常：{failure}"))
        .collect()
}

struct ProviderSyncScan {
    active_sqlite: SqliteScan,
    sqlite: SqliteScan,
    rollouts: RolloutScan,
    catalog: CatalogSyncScan,
    syncable_thread_ids: HashSet<String>,
    scan_failures: Vec<String>,
    warnings: Vec<String>,
}

fn scan_provider_sync_data(
    codex_dir: &Path,
    target_provider: &str,
    discovery: &SqliteDiscovery,
) -> Result<ProviderSyncScan> {
    let active_sqlite = scan_sqlite_with_paths(
        &discovery.active_paths,
        &RolloutScan::default(),
        target_provider,
    )?;
    let mut sqlite = scan_sqlite_with_paths(
        &discovery.thread_paths,
        &RolloutScan::default(),
        target_provider,
    )?;
    let mut syncable_thread_ids = sqlite.syncable_thread_ids.clone();
    let mut archived_thread_ids = sqlite.archived_thread_ids.clone();
    if active_sqlite.sqlite_dbs > 0 {
        syncable_thread_ids.retain(|id| !active_sqlite.archived_thread_ids.contains(id));
        archived_thread_ids.retain(|id| !active_sqlite.syncable_thread_ids.contains(id));
    }
    // Without an authoritative active database, conflicting legacy rows are unsafe to mutate.
    syncable_thread_ids.retain(|id| !archived_thread_ids.contains(id));
    sqlite
        .mismatched_thread_ids
        .retain(|id| syncable_thread_ids.contains(id));
    sqlite.mismatched_threads = sqlite.mismatched_thread_ids.len();

    let indexed_rollouts = if active_sqlite.sqlite_dbs > 0 {
        scan_provider_buckets(codex_dir, target_provider, &active_sqlite)?
    } else {
        scan_provider_buckets(codex_dir, target_provider, &sqlite)?
    };
    let legacy_index_warnings = scan_legacy_index_warnings(codex_dir, target_provider, discovery);
    let mut rollouts = scan_provider_rollouts(codex_dir, target_provider, &archived_thread_ids)?;
    // Provider synchronization changes provider routing only; cwd remains independently managed.
    rollouts.cwd_by_thread_id.clear();
    syncable_thread_ids.extend(rollouts.thread_ids.iter().cloned());
    syncable_thread_ids.retain(|id| !archived_thread_ids.contains(id));
    let catalog = scan_catalog_sync(
        &discovery.thread_paths,
        &discovery.related_paths,
        target_provider,
        &syncable_thread_ids,
    )?;

    let mut scan_failures = discovery.active_scan_failures.clone();
    scan_failures.extend(active_sqlite.scan_failures.iter().cloned());
    scan_failures.extend(sqlite.scan_failures.iter().cloned());
    scan_failures.extend(indexed_rollouts.scan_failures.iter().cloned());
    for path in &discovery.unreadable_paths {
        scan_failures.push(format!("无法读取会话数据库: {}", path.display()));
    }

    let indexed_failures = indexed_rollouts
        .scan_failures
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let mut warnings = rollouts.warnings.clone();
    warnings.extend(active_sqlite.warnings.iter().cloned());
    warnings.extend(sqlite.warnings.iter().cloned());
    warnings.extend(legacy_index_warnings);
    warnings.extend(
        rollouts
            .scan_failures
            .iter()
            .filter(|failure| !indexed_failures.contains(*failure))
            .map(|failure| format!("已跳过未进入会话索引的异常文件：{failure}")),
    );
    let mut seen = HashSet::new();
    scan_failures.retain(|failure| seen.insert(failure.clone()));
    seen.clear();
    warnings.retain(|warning| seen.insert(warning.clone()));

    Ok(ProviderSyncScan {
        active_sqlite,
        sqlite,
        rollouts,
        catalog,
        syncable_thread_ids,
        scan_failures,
        warnings,
    })
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
    let scan = scan_provider_sync_data(codex_dir, &target, discovery)?;
    let display_sqlite = if scan.active_sqlite.sqlite_dbs > 0 {
        &scan.active_sqlite
    } else {
        &scan.sqlite
    };
    let session_limit = display_sqlite.sqlite_threads.clamp(50, 1000);
    let preview_paths = if discovery.active_paths.is_empty() {
        discovery.active_first_session_paths()
    } else {
        discovery.active_paths.clone()
    };
    let (mut sessions, session_failures) = match list_session_previews_with_paths(
        &preview_paths,
        &scan.rollouts,
        &target,
        session_limit,
    ) {
        Ok(result) => result,
        Err(error) => {
            let mut failures = scan.scan_failures.clone();
            failures.push(format!("无法读取当前活动会话列表: {error}"));
            return Ok(SessionSyncStatus {
                codex_dir: codex_dir.display().to_string(),
                target_provider: target,
                rollout_files: scan.rollouts.rollout_files,
                session_meta_count: scan.rollouts.session_meta_count,
                mismatched_rollouts: scan.rollouts.mismatched_rollouts,
                mismatched_session_meta: scan.rollouts.mismatched_session_meta,
                sqlite_dbs: scan.sqlite.sqlite_dbs,
                sqlite_threads: display_sqlite.sqlite_threads,
                top_level_threads: display_sqlite.top_level_threads,
                subagent_threads: display_sqlite.subagent_threads,
                mismatched_threads: scan.sqlite.mismatched_threads,
                mismatched_sessions: 0,
                needs_sync: false,
                scan_complete: false,
                scan_failures: failures,
                backup_dir: None,
                warnings: scan.warnings,
                sessions: Vec::new(),
            });
        }
    };
    let mut scan_failures = scan.scan_failures;
    scan_failures.extend(session_failures);
    let mut seen_failures = HashSet::new();
    scan_failures.retain(|failure| seen_failures.insert(failure.clone()));
    let scan_complete = scan_failures.is_empty();
    let mut sqlite_mismatch_ids = scan.sqlite.mismatched_thread_ids.clone();
    sqlite_mismatch_ids.extend(scan.catalog.mismatched_thread_ids.iter().cloned());
    let mut mismatched_ids = sqlite_mismatch_ids.clone();
    mismatched_ids.extend(scan.rollouts.mismatched_thread_ids.iter().cloned());
    for session in &mut sessions {
        if !session.archived && mismatched_ids.contains(&session.id) {
            session.needs_sync = true;
        }
    }
    let needs_sync = !scan.rollouts.changes.is_empty()
        || scan.sqlite.mismatched_threads > 0
        || scan.catalog.total_updates() > 0;
    Ok(SessionSyncStatus {
        codex_dir: codex_dir.display().to_string(),
        target_provider: target,
        rollout_files: scan.rollouts.rollout_files,
        session_meta_count: scan.rollouts.session_meta_count,
        mismatched_rollouts: scan.rollouts.mismatched_rollouts,
        mismatched_session_meta: scan.rollouts.mismatched_session_meta,
        sqlite_dbs: scan.sqlite.sqlite_dbs,
        sqlite_threads: display_sqlite.sqlite_threads,
        top_level_threads: display_sqlite.top_level_threads,
        subagent_threads: display_sqlite.subagent_threads,
        mismatched_threads: sqlite_mismatch_ids.len(),
        mismatched_sessions: mismatched_ids.len(),
        needs_sync,
        scan_complete,
        scan_failures,
        backup_dir: None,
        warnings: scan.warnings,
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
    sync_sessions_provider_with_lifecycle(
        config_dir,
        target_provider,
        |_| Ok(()),
        platform::stop_codex_desktop,
        |state| state.was_running(),
        |state| platform::start_codex_desktop(state, false),
    )
}

#[cfg(test)]
pub(super) fn sync_sessions_provider_with_hook<F>(
    config_dir: Option<String>,
    target_provider: Option<String>,
    hook: F,
) -> Result<SessionSyncResult>
where
    F: FnMut(MutationPoint) -> Result<()>,
{
    sync_sessions_provider_with_lifecycle(
        config_dir,
        target_provider,
        hook,
        platform::stop_codex_desktop,
        |state| state.was_running(),
        |state| platform::start_codex_desktop(state, false),
    )
}

fn sync_sessions_provider_with_lifecycle<F, S, Stop, WasRunning, Restore>(
    config_dir: Option<String>,
    target_provider: Option<String>,
    mut hook: F,
    stop: Stop,
    was_running: WasRunning,
    restore: Restore,
) -> Result<SessionSyncResult>
where
    F: FnMut(MutationPoint) -> Result<()>,
    Stop: FnOnce() -> std::result::Result<S, String>,
    WasRunning: FnOnce(&S) -> bool,
    Restore: FnOnce(S) -> std::result::Result<(), String>,
{
    let codex_dir = resolve_codex_dir(config_dir)?;
    ensure_directory(&codex_dir)?;
    let target_provider = current_model_provider(&codex_dir, target_provider)?;
    let _maintenance_lock = acquire_session_maintenance_lock(&codex_dir)?;
    let discovery = discover_sqlite_databases(&codex_dir);
    ensure_sqlite_discovery_writable(&discovery)?;
    let initial_status =
        session_sync_status_with_discovery(&codex_dir, target_provider.clone(), &discovery)?;
    if !initial_status.scan_complete {
        return Err(scan_failure_error(&initial_status.scan_failures));
    }
    let preflight_scan = scan_provider_sync_data(&codex_dir, &target_provider, &discovery)?;
    if !preflight_scan.scan_failures.is_empty() {
        return Err(scan_failure_error(&preflight_scan.scan_failures));
    }
    if preflight_scan.rollouts.changes.is_empty()
        && preflight_scan.sqlite.mismatched_threads == 0
        && preflight_scan.catalog.total_updates() == 0
    {
        return Ok(SessionSyncResult {
            status: initial_status,
            updated_rollouts: 0,
            updated_threads: 0,
            backup_dir: String::new(),
            desktop_was_running: false,
            desktop_restarted: false,
            desktop_lifecycle_warning: None,
        });
    }

    let (mut result, lifecycle) = run_with_stopped_desktop(
        stop,
        was_running,
        || {
            // Desktop may flush SQLite, WAL, rollout, and catalog data while closing.
            // Discover and scan again only after the verified stop completes.
            let discovery = discover_sqlite_databases(&codex_dir);
            ensure_sqlite_discovery_writable(&discovery)?;
            hook(MutationPoint::BeforeSqliteLock)?;
            let mut pending_sqlite = prepare_sqlite_updates(&discovery.related_paths)?;
            let scan = match scan_provider_sync_data(&codex_dir, &target_provider, &discovery) {
                Ok(scan) => scan,
                Err(error) => {
                    rollback_open_transactions(&mut pending_sqlite);
                    return Err(error);
                }
            };
            if !scan.scan_failures.is_empty() {
                rollback_open_transactions(&mut pending_sqlite);
                return Err(scan_failure_error(&scan.scan_failures));
            }
            if scan.rollouts.changes.is_empty()
                && scan.sqlite.mismatched_threads == 0
                && scan.catalog.total_updates() == 0
            {
                rollback_open_transactions(&mut pending_sqlite);
                let status = session_sync_status_with_discovery(
                    &codex_dir,
                    target_provider.clone(),
                    &discovery,
                )?;
                return Ok(SessionSyncResult {
                    status,
                    updated_rollouts: 0,
                    updated_threads: 0,
                    backup_dir: String::new(),
                    desktop_was_running: false,
                    desktop_restarted: false,
                    desktop_lifecycle_warning: None,
                });
            }

            let changed_rollouts = scan
                .rollouts
                .changes
                .iter()
                .map(|change| change.path.clone())
                .collect::<Vec<_>>();
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
                &scan.rollouts,
                &mut pending_sqlite,
                &target_provider,
                &scan.catalog.sources,
                &scan.syncable_thread_ids,
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
            let mut status =
                session_sync_status_with_discovery(&codex_dir, target_provider.clone(), &discovery)
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
                desktop_was_running: false,
                desktop_restarted: false,
                desktop_lifecycle_warning: None,
            })
        },
        restore,
    )?;
    result.desktop_was_running = lifecycle.was_running;
    result.desktop_restarted = lifecycle.restarted;
    result.desktop_lifecycle_warning = lifecycle.warning;
    if let Some(warning) = &result.desktop_lifecycle_warning {
        result.status.warnings.push(warning.clone());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, Ordering};

    const SHARED_SESSION_PROVIDER: &str = "custom";

    fn temp_codex_dir(label: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "codex-x-session-sync-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test Codex directory");
        path
    }

    fn write_config(codex_dir: &Path, provider: &str) {
        fs::write(
            codex_dir.join("config.toml"),
            format!("model_provider = {provider:?}\n"),
        )
        .expect("write Codex config");
    }

    fn create_thread_database(path: &Path, id: &str, provider: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create SQLite parent");
        }
        let conn = Connection::open(path).expect("create session database");
        conn.execute_batch(
            "CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                model_provider TEXT NOT NULL,
                title TEXT,
                archived INTEGER NOT NULL DEFAULT 0
             );",
        )
        .expect("create threads table");
        conn.execute(
            "INSERT INTO threads (id, model_provider, title) VALUES (?1, ?2, 'test')",
            (id, provider),
        )
        .expect("insert thread");
    }

    fn create_thread_database_with_rollout(path: &Path, id: &str, provider: &str, rollout: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create SQLite parent");
        }
        let conn = Connection::open(path).expect("create session database");
        conn.execute_batch(
            "CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                model_provider TEXT NOT NULL,
                title TEXT,
                rollout_path TEXT,
                archived INTEGER NOT NULL DEFAULT 0
             );",
        )
        .expect("create threads table with rollout path");
        conn.execute(
            "INSERT INTO threads (id, model_provider, title, rollout_path)
             VALUES (?1, ?2, 'test', ?3)",
            (id, provider, rollout.display().to_string()),
        )
        .expect("insert thread with rollout path");
    }

    fn create_catalog_database(path: &Path, rows: &[(&str, &str)]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create catalog database parent");
        }
        let conn = Connection::open(path).expect("create catalog database");
        conn.execute_batch(
            "CREATE TABLE local_thread_catalog (
                host_id TEXT NOT NULL,
                thread_id TEXT NOT NULL,
                display_title TEXT NOT NULL,
                source_created_at REAL NOT NULL,
                source_updated_at REAL NOT NULL,
                cwd TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                source_detail TEXT,
                model_provider TEXT NOT NULL,
                git_branch TEXT,
                observation_sequence INTEGER NOT NULL,
                missing_candidate INTEGER NOT NULL DEFAULT 0,
                thread_source TEXT,
                source_recency_at REAL NOT NULL DEFAULT 0,
                PRIMARY KEY (host_id, thread_id)
             );
             CREATE TABLE local_thread_catalog_hosts (
                host_id TEXT PRIMARY KEY,
                host_kind TEXT NOT NULL
             );
             INSERT INTO local_thread_catalog_hosts VALUES ('local', 'local');
             CREATE TABLE local_thread_catalog_metadata (
                id INTEGER PRIMARY KEY,
                catalog_revision INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO local_thread_catalog_metadata VALUES (1, 7);
             CREATE TABLE local_thread_catalog_sync_state (
                host_id TEXT PRIMARY KEY,
                watermark_updated_at REAL,
                initial_build_complete INTEGER NOT NULL DEFAULT 0,
                observation_sequence INTEGER NOT NULL DEFAULT 0,
                last_full_reconciled_at INTEGER
             );
             INSERT INTO local_thread_catalog_sync_state VALUES ('local', 100, 1, 0, 100);",
        )
        .expect("create catalog tables");
        for (index, (id, provider)) in rows.iter().enumerate() {
            conn.execute(
                "INSERT INTO local_thread_catalog (
                    host_id, thread_id, display_title, source_created_at, source_updated_at,
                    cwd, source_kind, source_detail, model_provider, git_branch,
                    observation_sequence, missing_candidate, thread_source, source_recency_at
                 ) VALUES ('local', ?1, ?1, 100, 100, '/tmp/project', 'cli', '', ?2,
                    NULL, ?3, 0, 'user', 100)",
                (id, provider, index as i64 + 1),
            )
            .expect("insert catalog row");
        }
    }

    fn catalog_provider(path: &Path, id: &str) -> String {
        Connection::open(path)
            .expect("open catalog database")
            .query_row(
                "SELECT model_provider FROM local_thread_catalog WHERE thread_id = ?1",
                [id],
                |row| row.get(0),
            )
            .expect("read catalog provider")
    }

    fn thread_provider(path: &Path, id: &str) -> String {
        Connection::open(path)
            .expect("open session database")
            .query_row(
                "SELECT model_provider FROM threads WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .expect("read thread provider")
    }

    fn thread_provider_and_archived(path: &Path, id: &str) -> (String, i64) {
        Connection::open(path)
            .expect("open session database")
            .query_row(
                "SELECT model_provider, archived FROM threads WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read thread provider and archive state")
    }

    fn catalog_provider_and_visibility(path: &Path, id: &str) -> Option<(String, i64)> {
        Connection::open(path)
            .expect("open catalog database")
            .query_row(
                "SELECT model_provider, missing_candidate FROM local_thread_catalog \
                 WHERE thread_id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok()
    }

    fn write_rollout(codex_dir: &Path, id: &str, provider: &str) -> std::path::PathBuf {
        let path = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
        write_rollout_at(&path, id, provider);
        path
    }

    fn write_rollout_at(path: &Path, id: &str, provider: &str) {
        fs::create_dir_all(path.parent().expect("rollout parent")).expect("create rollout parent");
        fs::write(
            path,
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"model_provider\":\"{provider}\"}}}}\n"
            ),
        )
        .expect("write rollout");
    }

    #[test]
    fn live_provider_is_used_as_the_sync_target() {
        let codex_dir = temp_codex_dir("live-provider-target");
        let provider = "wujin_provider_1785657600000";
        write_config(&codex_dir, provider);
        let id = "019f6000-0000-7000-8000-000000000500";
        let database = codex_dir.join("state_5.sqlite");
        create_thread_database(&database, id, "openai");
        let rollout = write_rollout(&codex_dir, id, "openai");

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("read live provider status");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert_eq!(status.target_provider, provider);
        assert!(status.needs_sync);

        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("synchronize to the live provider");
        assert_eq!(result.status.target_provider, provider);
        assert!(!result.status.needs_sync);
        assert_eq!(thread_provider(&database, id), provider);
        assert!(fs::read_to_string(rollout)
            .expect("read synchronized rollout")
            .contains(&format!("\"model_provider\":\"{provider}\"")));

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn explicit_target_overrides_config_without_rewriting_it() {
        let codex_dir = temp_codex_dir("explicit-provider-target");
        write_config(&codex_dir, "configured-provider");
        let original_config = fs::read(codex_dir.join("config.toml")).expect("read config");
        let id = "019f6000-0000-7000-8000-000000000501";
        let database = codex_dir.join("state_5.sqlite");
        create_thread_database(&database, id, "openai");
        let rollout = write_rollout(&codex_dir, id, "openai");

        let result = sync_sessions_provider_inner(
            Some(codex_dir.display().to_string()),
            Some("manual-provider".to_string()),
        )
        .expect("synchronize to explicit target");
        assert_eq!(result.status.target_provider, "manual-provider");
        assert_eq!(thread_provider(&database, id), "manual-provider");
        assert!(fs::read_to_string(rollout)
            .expect("read synchronized rollout")
            .contains("\"model_provider\":\"manual-provider\""));
        assert_eq!(
            fs::read(codex_dir.join("config.toml")).expect("read unchanged config"),
            original_config
        );

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn missing_config_defaults_to_openai() {
        let codex_dir = temp_codex_dir("default-provider-target");
        let id = "019f6000-0000-7000-8000-000000000502";
        let database = codex_dir.join("state_5.sqlite");
        create_thread_database(&database, id, "custom");

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("read default provider status");
        assert_eq!(status.target_provider, "openai");
        assert!(status.needs_sync);
        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("synchronize to default provider");
        assert_eq!(result.status.target_provider, "openai");
        assert_eq!(thread_provider(&database, id), "openai");

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn unreadable_jsonl_cannot_report_all_sessions_synced() {
        let codex_dir = temp_codex_dir("unreadable-jsonl");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let id = "019f6000-0000-7000-8000-000000000501";
        create_thread_database(&codex_dir.join("state_5.sqlite"), id, "custom");
        let rollout = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
        fs::create_dir_all(rollout.parent().expect("rollout parent"))
            .expect("create rollout parent");
        fs::write(&rollout, "not-json\n").expect("write malformed rollout");

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("read incomplete status");
        assert!(!status.scan_complete);
        assert!(!status.needs_sync);
        assert!(status
            .scan_failures
            .iter()
            .any(|failure| failure.contains("无法解析的 JSON")));
        sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect_err("incomplete JSONL scan must block synchronization");

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn malformed_standard_orphan_rollout_does_not_block_active_sessions() {
        let codex_dir = temp_codex_dir("malformed-standard-orphan");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let active_id = "019f6000-0000-7000-8000-000000000502";
        let orphan_id = "019f6000-0000-7000-8000-000000000503";
        create_thread_database(
            &codex_dir.join("state_5.sqlite"),
            active_id,
            SHARED_SESSION_PROVIDER,
        );
        write_rollout(&codex_dir, active_id, SHARED_SESSION_PROVIDER);
        fs::write(
            codex_dir.join(format!("sessions/rollout-test-{orphan_id}.jsonl")),
            b"\xff",
        )
        .expect("write invalid orphan rollout");

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan active sessions only");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert_eq!(status.rollout_files, 2);
        assert_eq!(status.session_meta_count, 1);
        assert!(!status.needs_sync);
        assert_eq!(status.warnings.len(), 1);

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn malformed_unreferenced_nonstandard_rollout_does_not_block_active_sessions() {
        let codex_dir = temp_codex_dir("malformed-unreferenced-rollout");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let active_id = "019f6000-0000-7000-8000-000000000504";
        create_thread_database(
            &codex_dir.join("state_5.sqlite"),
            active_id,
            SHARED_SESSION_PROVIDER,
        );
        write_rollout(&codex_dir, active_id, SHARED_SESSION_PROVIDER);
        fs::write(
            codex_dir.join("sessions/rollout-imported-orphan.jsonl"),
            b"\xff",
        )
        .expect("write invalid unreferenced rollout");

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan referenced sessions only");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert_eq!(status.rollout_files, 2);
        assert_eq!(status.session_meta_count, 1);
        assert_eq!(status.warnings.len(), 1);

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn sqlite_referenced_nonstandard_rollout_is_synchronized() {
        let codex_dir = temp_codex_dir("referenced-nonstandard-rollout");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let id = "019f6000-0000-7000-8000-000000000505";
        let rollout = codex_dir.join("sessions/rollout-imported-name.jsonl");
        write_rollout_at(&rollout, id, "openai");
        let database = codex_dir.join("state_5.sqlite");
        create_thread_database_with_rollout(&database, id, "openai", &rollout);

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan referenced imported rollout");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert!(status.needs_sync);
        assert_eq!(status.rollout_files, 1);
        assert_eq!(status.mismatched_sessions, 1);

        let result = sync_sessions_provider_inner(
            Some(codex_dir.display().to_string()),
            Some(SHARED_SESSION_PROVIDER.to_string()),
        )
        .expect("synchronize referenced imported rollout");
        assert_eq!(result.updated_rollouts, 1);
        assert_eq!(result.updated_threads, 1);
        assert_eq!(thread_provider(&database, id), SHARED_SESSION_PROVIDER);
        assert!(fs::read_to_string(&rollout)
            .expect("read synchronized imported rollout")
            .contains("\"model_provider\":\"custom\""));

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn sqlite_referenced_rollout_with_a_different_session_id_blocks_sync() {
        let codex_dir = temp_codex_dir("referenced-rollout-id-mismatch");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let sqlite_id = "019f6000-0000-7000-8000-000000000506";
        let rollout_id = "019f6000-0000-7000-8000-000000000507";
        let rollout = codex_dir.join("sessions/rollout-imported-name.jsonl");
        write_rollout_at(&rollout, rollout_id, "openai");
        let database = codex_dir.join("state_5.sqlite");
        create_thread_database_with_rollout(&database, sqlite_id, "openai", &rollout);

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan mismatched referenced rollout");
        assert!(!status.scan_complete);
        assert!(status
            .scan_failures
            .iter()
            .any(|failure| failure.contains("线程 ID 不一致")));

        let error = sync_sessions_provider_inner(
            Some(codex_dir.display().to_string()),
            Some(SHARED_SESSION_PROVIDER.to_string()),
        )
        .expect_err("mismatched referenced rollout must block synchronization");
        assert!(error.to_string().contains("线程 ID 不一致"));
        assert_eq!(thread_provider(&database, sqlite_id), "openai");
        assert!(fs::read_to_string(&rollout)
            .expect("read unchanged rollout")
            .contains("\"model_provider\":\"openai\""));

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn missing_sqlite_referenced_rollout_blocks_sync_without_using_uuid_fallback() {
        let codex_dir = temp_codex_dir("missing-referenced-rollout");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let id = "019f6000-0000-7000-8000-000000000506";
        let missing = codex_dir.join("sessions/missing/rollout-selected.jsonl");
        let duplicate = write_rollout(&codex_dir, id, "openai");
        create_thread_database_with_rollout(
            &codex_dir.join("state_5.sqlite"),
            id,
            SHARED_SESSION_PROVIDER,
            &missing,
        );

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan missing referenced rollout");
        assert!(!status.scan_complete);
        assert!(status.needs_sync);
        assert_eq!(status.rollout_files, 1);
        assert!(status
            .scan_failures
            .iter()
            .any(|failure| failure.contains("会话文件不存在")));
        sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect_err("missing referenced rollout must block synchronization");
        assert!(fs::read_to_string(duplicate)
            .expect("read untouched UUID fallback")
            .contains("\"model_provider\":\"openai\""));

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn unreadable_sqlite_referenced_rollout_cannot_report_synced() {
        let codex_dir = temp_codex_dir("unreadable-referenced-rollout");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let id = "019f6000-0000-7000-8000-000000000507";
        let rollout = codex_dir.join("sessions/rollout-unreadable.jsonl");
        fs::create_dir_all(rollout.parent().expect("rollout parent"))
            .expect("create rollout parent");
        fs::write(&rollout, b"\xff").expect("write invalid UTF-8 rollout");
        create_thread_database_with_rollout(
            &codex_dir.join("state_5.sqlite"),
            id,
            SHARED_SESSION_PROVIDER,
            &rollout,
        );

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan unreadable referenced rollout");
        assert!(!status.scan_complete);
        assert!(!status.needs_sync);
        assert_eq!(status.rollout_files, 1);
        assert!(status
            .scan_failures
            .iter()
            .any(|failure| failure.contains("无法读取会话文件")));
        sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect_err("unreadable referenced rollout must block synchronization");

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn provider_sync_updates_all_rollouts_even_when_sqlite_selects_one() {
        let codex_dir = temp_codex_dir("referenced-rollout-excludes-duplicates");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let id = "019f6000-0000-7000-8000-000000000508";
        let selected = codex_dir.join("sessions/rollout-selected-name.jsonl");
        write_rollout_at(&selected, id, "openai");
        let duplicate = write_rollout(&codex_dir, id, "openai");
        let database = codex_dir.join("state_5.sqlite");
        create_thread_database_with_rollout(&database, id, "openai", &selected);

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan every rollout");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert_eq!(status.rollout_files, 2);
        assert_eq!(status.mismatched_rollouts, 2);

        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("synchronize every rollout");
        assert_eq!(result.updated_rollouts, 2);
        assert_eq!(result.updated_threads, 1);
        assert_eq!(thread_provider(&database, id), SHARED_SESSION_PROVIDER);
        assert!(fs::read_to_string(&selected)
            .expect("read selected rollout")
            .contains("\"model_provider\":\"custom\""));
        assert!(fs::read_to_string(&duplicate)
            .expect("read duplicate rollout")
            .contains("\"model_provider\":\"custom\""));

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn sqlite_referenced_rollout_outside_session_storage_is_rejected() {
        let codex_dir = temp_codex_dir("outside-referenced-rollout");
        let outside_dir = temp_codex_dir("outside-referenced-rollout-target");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let id = "019f6000-0000-7000-8000-000000000509";
        let rollout = outside_dir.join("rollout-external.jsonl");
        write_rollout_at(&rollout, id, "openai");
        create_thread_database_with_rollout(
            &codex_dir.join("state_5.sqlite"),
            id,
            SHARED_SESSION_PROVIDER,
            &rollout,
        );

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("reject external referenced rollout");
        assert!(!status.scan_complete);
        assert!(!status.needs_sync);
        assert_eq!(status.rollout_files, 0);
        assert!(status
            .scan_failures
            .iter()
            .any(|failure| failure.contains("超出 Codex 会话目录")));
        sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect_err("external referenced rollout must block synchronization");
        assert!(fs::read_to_string(&rollout)
            .expect("read untouched external rollout")
            .contains("\"model_provider\":\"openai\""));

        fs::remove_dir_all(codex_dir).expect("remove test directory");
        fs::remove_dir_all(outside_dir).expect("remove external test directory");
    }

    #[test]
    fn unreadable_active_sqlite_cannot_report_all_sessions_synced() {
        let codex_dir = temp_codex_dir("unreadable-active-sqlite");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        fs::write(codex_dir.join("state_5.sqlite"), b"SQLite format 3\0")
            .expect("write truncated SQLite");

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("read incomplete status");
        assert!(!status.scan_complete);
        assert_eq!(status.sqlite_threads, 0);
        assert!(status
            .scan_failures
            .iter()
            .any(|failure| failure.contains("活动会话数据库")));
        sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect_err("unreadable active SQLite must block synchronization");

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn all_thread_databases_are_synchronized_without_inflating_the_active_list() {
        let codex_dir = temp_codex_dir("active-only");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let active = codex_dir.join("state_5.sqlite");
        let legacy = codex_dir.join("sqlite/state_5.sqlite");
        let active_id = "019f6000-0000-7000-8000-000000000511";
        let legacy_id = "019f6000-0000-7000-8000-000000000512";
        create_thread_database(&active, active_id, "openai");
        create_thread_database(&legacy, legacy_id, "openai");

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("read active status");
        assert!(status.scan_complete);
        assert_eq!(status.sqlite_dbs, 2);
        assert_eq!(status.sqlite_threads, 1);
        assert_eq!(status.mismatched_threads, 2);
        assert_eq!(status.sessions.len(), 1);
        assert_eq!(status.sessions[0].id, active_id);

        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("sync active database");
        assert_eq!(result.updated_threads, 2);
        assert_eq!(thread_provider(&active, active_id), "custom");
        assert_eq!(thread_provider(&legacy, legacy_id), "custom");

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn stale_legacy_rollout_reference_warns_without_blocking_active_sync() {
        let codex_dir = temp_codex_dir("stale-legacy-rollout");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let active_id = "019f6000-0000-7000-8000-000000000513";
        let legacy_id = "019f6000-0000-7000-8000-000000000514";
        let active_rollout = write_rollout(&codex_dir, active_id, "openai");
        let active = codex_dir.join("state_5.sqlite");
        create_thread_database_with_rollout(&active, active_id, "openai", &active_rollout);
        let stale_rollout = codex_dir.join(format!("sessions/rollout-deleted-{legacy_id}.jsonl"));
        let legacy = codex_dir.join("sqlite/state_5.sqlite");
        create_thread_database_with_rollout(&legacy, legacy_id, "openai", &stale_rollout);

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan with stale legacy reference");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert!(status.needs_sync);
        assert!(status
            .warnings
            .iter()
            .any(|warning| warning.contains("已忽略旧会话索引异常")));

        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("synchronize despite stale legacy reference");
        assert_eq!(result.updated_threads, 2);
        assert_eq!(thread_provider(&active, active_id), "custom");
        assert_eq!(thread_provider(&legacy, legacy_id), "custom");
        assert!(fs::read_to_string(active_rollout)
            .expect("read active rollout")
            .contains("\"model_provider\":\"custom\""));

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn configured_sqlite_home_controls_display_while_all_databases_are_synchronized() {
        let codex_dir = temp_codex_dir("configured-sqlite-home");
        fs::write(
            codex_dir.join("config.toml"),
            "model_provider = \"custom\"\nsqlite_home = \"active-sqlite\"\n",
        )
        .expect("write configured SQLite home");
        let configured = codex_dir.join("active-sqlite/state_5.sqlite");
        let root_copy = codex_dir.join("state_10.sqlite");
        let configured_id = "019f6000-0000-7000-8000-000000000515";
        let root_id = "019f6000-0000-7000-8000-000000000516";
        create_thread_database(&configured, configured_id, "openai");
        create_thread_database(&root_copy, root_id, "openai");

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("read configured active status");
        assert!(status.scan_complete);
        assert_eq!(status.sqlite_threads, 1);
        assert_eq!(status.sessions[0].id, configured_id);

        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("sync configured active database");
        assert_eq!(result.updated_threads, 2);
        assert_eq!(thread_provider(&configured, configured_id), "custom");
        assert_eq!(thread_provider(&root_copy, root_id), "custom");

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn legacy_only_database_can_still_be_synchronized() {
        let codex_dir = temp_codex_dir("legacy-only");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let legacy = codex_dir.join("sqlite/state_5.sqlite");
        let id = "019f6000-0000-7000-8000-000000000521";
        create_thread_database(&legacy, id, "openai");

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("read legacy-only status");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert_eq!(status.sqlite_threads, 1);
        assert_eq!(status.sessions.len(), 1);
        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("synchronize legacy-only database");
        assert_eq!(result.updated_threads, 1);
        assert_eq!(thread_provider(&legacy, id), "custom");

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn conflicting_legacy_archive_state_is_not_syncable_without_active_database() {
        let codex_dir = temp_codex_dir("legacy-archive-conflict");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let id = "019f6000-0000-7000-8000-000000000522";
        let archived_database = codex_dir.join("sqlite/state_4.sqlite");
        let active_database = codex_dir.join("sqlite/state_5.sqlite");
        create_thread_database(&archived_database, id, "openai");
        create_thread_database(&active_database, id, "openai");
        Connection::open(&archived_database)
            .expect("open archived legacy database")
            .execute("UPDATE threads SET archived = 1 WHERE id = ?1", [id])
            .expect("archive legacy thread");
        let rollout = write_rollout(&codex_dir, id, "openai");
        let original_rollout = fs::read(&rollout).expect("read conflicting legacy rollout");
        let catalog = codex_dir.join("sqlite/catalog.db");
        create_catalog_database(&catalog, &[(id, "openai")]);

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan conflicting legacy archive state");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert!(!status.needs_sync);
        assert_eq!(status.mismatched_sessions, 0);
        assert!(status.sessions.iter().all(|session| !session.needs_sync));

        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("conflicting legacy state is a no-op");
        assert_eq!(result.updated_rollouts, 0);
        assert_eq!(result.updated_threads, 0);
        assert!(result.backup_dir.is_empty());
        assert_eq!(thread_provider(&archived_database, id), "openai");
        assert_eq!(thread_provider(&active_database, id), "openai");
        assert_eq!(catalog_provider(&catalog, id), "openai");
        assert_eq!(
            fs::read(rollout).expect("read preserved conflicting legacy rollout"),
            original_rollout
        );

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn orphan_rollout_is_synchronized_like_codex_plusplus() {
        let codex_dir = temp_codex_dir("orphan-rollout");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let active_id = "019f6000-0000-7000-8000-000000000531";
        let orphan_id = "019f6000-0000-7000-8000-000000000532";
        create_thread_database(
            &codex_dir.join("state_5.sqlite"),
            active_id,
            SHARED_SESSION_PROVIDER,
        );
        let orphan = write_rollout(&codex_dir, orphan_id, "openai");

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("read active-only status");
        assert!(status.scan_complete);
        assert!(status.needs_sync);
        assert_eq!(status.mismatched_sessions, 1);
        assert_eq!(status.mismatched_rollouts, 1);

        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("synchronize orphan rollout");
        assert_eq!(result.updated_rollouts, 1);
        assert!(fs::read_to_string(orphan)
            .expect("read orphan rollout")
            .contains("\"model_provider\":\"custom\""));

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn active_orphan_catalog_row_is_synchronized() {
        let codex_dir = temp_codex_dir("orphan-catalog");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let indexed_id = "019f6000-0000-7000-8000-000000000539";
        create_thread_database(
            &codex_dir.join("state_5.sqlite"),
            indexed_id,
            SHARED_SESSION_PROVIDER,
        );
        let orphan_id = "019f6000-0000-7000-8000-000000000540";
        let orphan = write_rollout(&codex_dir, orphan_id, SHARED_SESSION_PROVIDER);
        let original_rollout = fs::read(&orphan).expect("read matching orphan rollout");
        let catalog = codex_dir.join("sqlite/codex-dev.db");
        create_catalog_database(
            &catalog,
            &[(indexed_id, SHARED_SESSION_PROVIDER), (orphan_id, "openai")],
        );

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan orphan catalog mismatch");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert!(status.needs_sync);
        assert_eq!(status.mismatched_rollouts, 0);
        assert_eq!(status.mismatched_sessions, 1);

        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("synchronize orphan catalog row");
        assert_eq!(result.updated_rollouts, 0);
        assert_eq!(result.updated_threads, 1);
        assert!(!result.status.needs_sync);
        assert_eq!(
            catalog_provider(&catalog, orphan_id),
            SHARED_SESSION_PROVIDER
        );
        assert_eq!(
            fs::read(orphan).expect("read preserved matching orphan rollout"),
            original_rollout
        );

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn archived_thread_is_visible_but_not_a_sync_candidate() {
        let codex_dir = temp_codex_dir("archived-not-syncable");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let id = "019f6000-0000-7000-8000-000000000533";
        let rollout = codex_dir.join(format!(
            "archived_sessions/2026/08/18/rollout-test-{id}.jsonl"
        ));
        write_rollout_at(&rollout, id, "openai");
        let original_rollout = fs::read(&rollout).expect("read archived rollout");
        let state = codex_dir.join("state_5.sqlite");
        create_thread_database_with_rollout(&state, id, "openai", &rollout);
        Connection::open(&state)
            .expect("open archived thread database")
            .execute("UPDATE threads SET archived = 1 WHERE id = ?1", [id])
            .expect("archive thread");
        let catalog = codex_dir.join("sqlite/codex-dev.db");
        create_catalog_database(&catalog, &[(id, "openai")]);
        Connection::open(&catalog)
            .expect("open archived catalog")
            .execute(
                "UPDATE local_thread_catalog SET missing_candidate = 1 WHERE thread_id = ?1",
                [id],
            )
            .expect("hide archived catalog row");

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan archived thread");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert!(!status.needs_sync);
        assert_eq!(status.mismatched_threads, 0);
        assert_eq!(status.mismatched_rollouts, 0);
        assert_eq!(status.mismatched_sessions, 0);
        let preview = status
            .sessions
            .iter()
            .find(|session| session.id == id)
            .expect("archived preview remains visible");
        assert!(preview.archived);
        assert!(!preview.needs_sync);

        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("archived-only sync is a no-op");
        assert_eq!(result.updated_rollouts, 0);
        assert_eq!(result.updated_threads, 0);
        assert!(result.backup_dir.is_empty());
        assert_eq!(
            thread_provider_and_archived(&state, id),
            ("openai".to_string(), 1)
        );
        assert_eq!(
            catalog_provider_and_visibility(&catalog, id),
            Some(("openai".to_string(), 1))
        );
        assert_eq!(
            fs::read(&rollout).expect("read preserved archived rollout"),
            original_rollout
        );

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn archive_between_scan_and_sqlite_lock_is_excluded_from_sync() {
        let codex_dir = temp_codex_dir("archive-during-sync");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let id = "019f6000-0000-7000-8000-000000000543";
        let rollout = write_rollout(&codex_dir, id, "openai");
        let original_rollout = fs::read(&rollout).expect("read pre-archive rollout");
        let state = codex_dir.join("state_5.sqlite");
        create_thread_database_with_rollout(&state, id, "openai", &rollout);
        let catalog = codex_dir.join("sqlite/codex-dev.db");
        create_catalog_database(&catalog, &[(id, "openai")]);
        let mut archive_hook_called = false;

        let result = sync_sessions_provider_with_hook(
            Some(codex_dir.display().to_string()),
            None,
            |point| match point {
                MutationPoint::BeforeSqliteLock => {
                    archive_hook_called = true;
                    Connection::open(&state)
                        .expect("open thread database before sync lock")
                        .execute("UPDATE threads SET archived = 1 WHERE id = ?1", [id])
                        .expect("archive thread before sync lock");
                    Connection::open(&catalog)
                        .expect("open catalog before sync lock")
                        .execute(
                            "UPDATE local_thread_catalog SET missing_candidate = 1 \
                             WHERE thread_id = ?1",
                            [id],
                        )
                        .expect("hide catalog row before sync lock");
                    Ok(())
                }
                _ => Ok(()),
            },
        )
        .expect("newly archived thread is a no-op");

        assert!(archive_hook_called);
        assert_eq!(result.updated_rollouts, 0);
        assert_eq!(result.updated_threads, 0);
        assert!(result.backup_dir.is_empty());
        assert!(!result.status.needs_sync);
        let preview = result
            .status
            .sessions
            .iter()
            .find(|session| session.id == id)
            .expect("newly archived preview remains visible");
        assert!(preview.archived);
        assert!(!preview.needs_sync);
        assert_eq!(
            thread_provider_and_archived(&state, id),
            ("openai".to_string(), 1)
        );
        assert_eq!(
            catalog_provider_and_visibility(&catalog, id),
            Some(("openai".to_string(), 1))
        );
        assert_eq!(
            fs::read(rollout).expect("read preserved newly archived rollout"),
            original_rollout
        );

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn sync_repairs_active_thread_without_mutating_archived_storage_or_catalog() {
        let codex_dir = temp_codex_dir("mixed-active-archived");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let active_id = "019f6000-0000-7000-8000-000000000534";
        let hidden_archived_id = "019f6000-0000-7000-8000-000000000535";
        let missing_archived_id = "019f6000-0000-7000-8000-000000000536";
        let active_rollout = write_rollout(&codex_dir, active_id, "openai");
        let hidden_archived_rollout = codex_dir.join(format!(
            "archived_sessions/2026/08/18/rollout-test-{hidden_archived_id}.jsonl"
        ));
        let missing_archived_rollout = codex_dir.join(format!(
            "archived_sessions/2026/08/18/rollout-test-{missing_archived_id}.jsonl"
        ));
        write_rollout_at(&hidden_archived_rollout, hidden_archived_id, "openai");
        write_rollout_at(&missing_archived_rollout, missing_archived_id, "openai");
        let hidden_original =
            fs::read(&hidden_archived_rollout).expect("read hidden archived rollout");
        let missing_original =
            fs::read(&missing_archived_rollout).expect("read missing archived rollout");

        let state = codex_dir.join("state_5.sqlite");
        create_thread_database_with_rollout(&state, active_id, "openai", &active_rollout);
        let state_conn = Connection::open(&state).expect("open mixed thread database");
        for (id, rollout) in [
            (hidden_archived_id, &hidden_archived_rollout),
            (missing_archived_id, &missing_archived_rollout),
        ] {
            state_conn
                .execute(
                    "INSERT INTO threads (id, model_provider, title, rollout_path, archived) \
                     VALUES (?1, 'openai', 'archived', ?2, 1)",
                    (id, rollout.display().to_string()),
                )
                .expect("insert archived thread");
        }
        drop(state_conn);

        let catalog = codex_dir.join("sqlite/codex-dev.db");
        create_catalog_database(
            &catalog,
            &[(active_id, "openai"), (hidden_archived_id, "openai")],
        );
        Connection::open(&catalog)
            .expect("open mixed catalog")
            .execute(
                "UPDATE local_thread_catalog SET missing_candidate = 1 WHERE thread_id = ?1",
                [hidden_archived_id],
            )
            .expect("hide archived catalog row");

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan mixed threads");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert!(status.needs_sync);
        assert_eq!(status.mismatched_sessions, 1);
        assert!(status
            .sessions
            .iter()
            .filter(|session| session.archived)
            .all(|session| !session.needs_sync));

        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("sync active thread only");
        assert_eq!(result.updated_rollouts, 1);
        assert!(!result.status.needs_sync);
        assert_eq!(thread_provider(&state, active_id), SHARED_SESSION_PROVIDER);
        assert_eq!(
            thread_provider_and_archived(&state, hidden_archived_id),
            ("openai".to_string(), 1)
        );
        assert_eq!(
            thread_provider_and_archived(&state, missing_archived_id),
            ("openai".to_string(), 1)
        );
        assert!(fs::read_to_string(&active_rollout)
            .expect("read synchronized active rollout")
            .contains("\"model_provider\":\"custom\""));
        assert_eq!(
            fs::read(&hidden_archived_rollout).expect("read preserved hidden archived rollout"),
            hidden_original
        );
        assert_eq!(
            fs::read(&missing_archived_rollout).expect("read preserved missing archived rollout"),
            missing_original
        );
        assert_eq!(
            catalog_provider(&catalog, active_id),
            SHARED_SESSION_PROVIDER
        );
        assert_eq!(
            catalog_provider_and_visibility(&catalog, hidden_archived_id),
            Some(("openai".to_string(), 1))
        );
        assert_eq!(
            catalog_provider_and_visibility(&catalog, missing_archived_id),
            None
        );

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn archived_orphan_rollout_is_ignored_while_active_orphan_still_syncs() {
        let codex_dir = temp_codex_dir("orphan-storage-scope");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let indexed_id = "019f6000-0000-7000-8000-000000000537";
        create_thread_database(
            &codex_dir.join("state_5.sqlite"),
            indexed_id,
            SHARED_SESSION_PROVIDER,
        );
        let active_orphan_id = "019f6000-0000-7000-8000-000000000538";
        let archived_orphan_id = "019f6000-0000-7000-8000-000000000539";
        let active_orphan = write_rollout(&codex_dir, active_orphan_id, "openai");
        let archived_orphan = codex_dir.join(format!(
            "archived_sessions/2026/08/18/rollout-test-{archived_orphan_id}.jsonl"
        ));
        write_rollout_at(&archived_orphan, archived_orphan_id, "openai");
        let archived_original = fs::read(&archived_orphan).expect("read archived orphan");

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan orphan storage scope");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert_eq!(status.mismatched_rollouts, 1);
        assert_eq!(status.mismatched_sessions, 1);

        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("sync active orphan only");
        assert_eq!(result.updated_rollouts, 1);
        assert!(!result.status.needs_sync);
        assert!(fs::read_to_string(active_orphan)
            .expect("read synchronized active orphan")
            .contains("\"model_provider\":\"custom\""));
        assert_eq!(
            fs::read(archived_orphan).expect("read preserved archived orphan"),
            archived_original
        );

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn mismatch_count_is_the_union_of_active_session_ids() {
        let codex_dir = temp_codex_dir("mismatch-union");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let database = codex_dir.join("state_5.sqlite");
        let sqlite_mismatch = "019f6000-0000-7000-8000-000000000541";
        let rollout_mismatch = "019f6000-0000-7000-8000-000000000542";
        create_thread_database(&database, sqlite_mismatch, "openai");
        Connection::open(&database)
            .expect("open active database")
            .execute(
                "INSERT INTO threads (id, model_provider, title) VALUES (?1, 'custom', 'test')",
                [rollout_mismatch],
            )
            .expect("insert second active thread");
        write_rollout(&codex_dir, rollout_mismatch, "openai");

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("read mismatch union");
        assert!(status.scan_complete);
        assert_eq!(status.mismatched_threads, 1);
        assert_eq!(status.mismatched_rollouts, 1);
        assert_eq!(status.mismatched_sessions, 2);
        assert!(status
            .sessions
            .iter()
            .find(|session| session.id == rollout_mismatch)
            .is_some_and(|session| session.needs_sync));

        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("sync mismatch union");
        assert_eq!(result.updated_threads, 1);
        assert_eq!(result.updated_rollouts, 1);
        assert_eq!(result.status.mismatched_sessions, 0);

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn provider_sync_updates_and_repairs_the_client_catalog() {
        let codex_dir = temp_codex_dir("catalog-repair");
        let provider = "wujin_provider_1785657600001";
        write_config(&codex_dir, provider);
        let first_id = "019f6000-0000-7000-8000-000000000551";
        let second_id = "019f6000-0000-7000-8000-000000000552";
        let state = codex_dir.join("state_5.sqlite");
        create_thread_database(&state, first_id, provider);
        let state_conn = Connection::open(&state).expect("open state database");
        state_conn
            .execute(
                "INSERT INTO threads (id, model_provider, title) VALUES (?1, ?2, 'Second')",
                (second_id, provider),
            )
            .expect("insert second thread");
        state_conn
            .execute_batch(
                "ALTER TABLE threads ADD COLUMN updated_at_ms INTEGER;
                 ALTER TABLE threads ADD COLUMN recency_at_ms INTEGER;
                 UPDATE threads SET updated_at_ms = 200000, recency_at_ms = 300000;",
            )
            .expect("seed thread timestamps");
        drop(state_conn);
        let catalog = codex_dir.join("sqlite/codex-dev.db");
        create_catalog_database(&catalog, &[(first_id, "openai")]);

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan catalog drift");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert!(status.needs_sync);
        assert_eq!(status.mismatched_sessions, 2);

        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("repair client catalog");
        assert_eq!(result.updated_threads, 2);
        assert!(!result.status.needs_sync);
        assert_eq!(catalog_provider(&catalog, first_id), provider);
        assert_eq!(catalog_provider(&catalog, second_id), provider);
        let catalog_conn = Connection::open(&catalog).expect("open repaired catalog");
        let (revision, observation_sequence, recency): (i64, i64, f64) = catalog_conn
            .query_row(
                "SELECT
                    (SELECT catalog_revision FROM local_thread_catalog_metadata WHERE id = 1),
                    (SELECT observation_sequence FROM local_thread_catalog_sync_state WHERE host_id = 'local'),
                    (SELECT source_recency_at FROM local_thread_catalog WHERE thread_id = ?1)",
                [second_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read catalog metadata");
        assert_eq!(revision, 8);
        assert!(observation_sequence >= 2);
        assert_eq!(recency, 300.0);
        drop(catalog_conn);

        let second = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("repeat catalog synchronization");
        assert_eq!(second.updated_rollouts, 0);
        assert_eq!(second.updated_threads, 0);
        assert!(second.backup_dir.is_empty());

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn injected_failure_restores_catalog_rows_metadata_and_sync_state() {
        let codex_dir = temp_codex_dir("catalog-rollback");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let first_id = "019f6000-0000-7000-8000-000000000561";
        let second_id = "019f6000-0000-7000-8000-000000000562";
        let state = codex_dir.join("state_5.sqlite");
        create_thread_database(&state, first_id, "openai");
        Connection::open(&state)
            .expect("open state database")
            .execute(
                "INSERT INTO threads (id, model_provider, title) VALUES (?1, 'openai', 'Second')",
                [second_id],
            )
            .expect("insert second thread");
        let rollout = write_rollout(&codex_dir, first_id, "openai");
        let original_rollout = fs::read(&rollout).expect("read original rollout");
        let catalog = codex_dir.join("sqlite/codex-dev.db");
        create_catalog_database(&catalog, &[(first_id, "openai")]);

        let error = sync_sessions_provider_with_hook(
            Some(codex_dir.display().to_string()),
            None,
            |point| match point {
                MutationPoint::AfterSqliteCommit(1) => {
                    assert_eq!(catalog_provider(&catalog, first_id), "custom");
                    assert_eq!(catalog_provider(&catalog, second_id), "custom");
                    Err(CodexxError::Config("catalog rollback test".to_string()))
                }
                _ => Ok(()),
            },
        )
        .expect_err("inject failure after catalog commit");
        assert_eq!(error.to_string(), "配置错误: catalog rollback test");
        assert_eq!(thread_provider(&state, first_id), "openai");
        assert_eq!(thread_provider(&state, second_id), "openai");
        assert_eq!(catalog_provider(&catalog, first_id), "openai");
        let catalog_conn = Connection::open(&catalog).expect("open restored catalog");
        let (rows, revision, watermark, sequence, reconciled): (i64, i64, f64, i64, i64) =
            catalog_conn
                .query_row(
                    "SELECT
                        (SELECT COUNT(*) FROM local_thread_catalog),
                        (SELECT catalog_revision FROM local_thread_catalog_metadata WHERE id = 1),
                        (SELECT watermark_updated_at FROM local_thread_catalog_sync_state WHERE host_id = 'local'),
                        (SELECT observation_sequence FROM local_thread_catalog_sync_state WHERE host_id = 'local'),
                        (SELECT last_full_reconciled_at FROM local_thread_catalog_sync_state WHERE host_id = 'local')",
                    [],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .expect("read restored catalog state");
        assert_eq!(
            (rows, revision, watermark, sequence, reconciled),
            (1, 7, 100.0, 0, 100)
        );
        let quick_check: String = catalog_conn
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .expect("check restored catalog");
        assert_eq!(quick_check, "ok");
        assert_eq!(
            fs::read(rollout).expect("read restored rollout"),
            original_rollout
        );

        drop(catalog_conn);
        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn session_maintenance_lock_blocks_a_second_mutation() {
        let codex_dir = temp_codex_dir("maintenance-lock");
        let first = acquire_session_maintenance_lock(&codex_dir).expect("acquire first lock");
        let second = match acquire_session_maintenance_lock(&codex_dir) {
            Ok(_) => panic!("second maintenance operation must be blocked"),
            Err(error) => error,
        };
        assert!(second.to_string().contains("会话维护正在进行"));
        drop(first);
        drop(
            acquire_session_maintenance_lock(&codex_dir)
                .expect("lock becomes available after first operation"),
        );
        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn no_op_sync_does_not_stop_desktop() {
        let codex_dir = temp_codex_dir("no-op-does-not-stop");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let id = "019f6000-0000-7000-8000-000000000601";
        create_thread_database(
            &codex_dir.join("state_5.sqlite"),
            id,
            SHARED_SESSION_PROVIDER,
        );
        let stop_called = Cell::new(false);

        let result = sync_sessions_provider_with_lifecycle(
            Some(codex_dir.display().to_string()),
            None,
            |_| Ok(()),
            || {
                stop_called.set(true);
                Ok(false)
            },
            |running| *running,
            |_| Ok(()),
        )
        .expect("no-op synchronization");
        assert!(!stop_called.get());
        assert_eq!(result.updated_threads, 0);
        assert!(!result.desktop_was_running);
        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn sync_rescans_data_after_desktop_stop() {
        let codex_dir = temp_codex_dir("rescan-after-stop");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let id = "019f6000-0000-7000-8000-000000000602";
        let database = codex_dir.join("state_5.sqlite");
        create_thread_database(&database, id, "openai");
        let restored = Cell::new(false);

        let result = sync_sessions_provider_with_lifecycle(
            Some(codex_dir.display().to_string()),
            None,
            |_| Ok(()),
            || {
                Connection::open(&database)
                    .expect("open database during stop")
                    .execute(
                        "UPDATE threads SET model_provider = ?1 WHERE id = ?2",
                        (SHARED_SESSION_PROVIDER, id),
                    )
                    .expect("simulate Desktop final flush");
                Ok(true)
            },
            |running| *running,
            |running| {
                restored.set(running);
                Ok(())
            },
        )
        .expect("rescan after stop");
        assert_eq!(
            result.updated_threads, 0,
            "stale preflight data must not mutate"
        );
        assert!(!result.status.needs_sync);
        assert!(result.desktop_was_running);
        assert!(result.desktop_restarted);
        assert!(restored.get());
        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn stop_failure_leaves_sync_storage_unchanged() {
        let codex_dir = temp_codex_dir("stop-failure");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let id = "019f6000-0000-7000-8000-000000000603";
        let database = codex_dir.join("state_5.sqlite");
        create_thread_database(&database, id, "openai");

        let error = sync_sessions_provider_with_lifecycle(
            Some(codex_dir.display().to_string()),
            None,
            |_| Ok(()),
            || Err::<bool, _>("close timeout".to_string()),
            |running| *running,
            |_| Ok(()),
        )
        .expect_err("stop failure must cancel sync");
        assert!(error.to_string().contains("会话操作已取消"));
        assert_eq!(thread_provider(&database, id), "openai");
        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn global_state_drift_does_not_trigger_provider_sync() {
        let codex_dir = temp_codex_dir("ignore-global-state-drift");
        fs::write(
            codex_dir.join("config.toml"),
            "model_provider = \"custom\"\n",
        )
        .expect("write official config");
        let global_state = codex_dir.join(".codex-global-state.json");
        let original = br#"{"electron-saved-workspace-roots":"/tmp/project"}"#;
        fs::write(&global_state, original).expect("write global state drift");

        let status = session_sync_status_inner(
            Some(codex_dir.display().to_string()),
            Some("custom".to_string()),
        )
        .expect("read shared session status");
        assert_eq!(status.target_provider, SHARED_SESSION_PROVIDER);
        assert!(!status.needs_sync);

        let result = sync_sessions_provider_inner(
            Some(codex_dir.display().to_string()),
            Some("custom".to_string()),
        )
        .expect("global state drift is not a provider migration");
        assert_eq!(result.status.target_provider, SHARED_SESSION_PROVIDER);
        assert_eq!(result.updated_rollouts, 0);
        assert_eq!(result.updated_threads, 0);
        assert!(result.backup_dir.is_empty());
        assert_eq!(
            fs::read(&global_state).expect("read unchanged state"),
            original
        );
        assert!(!codex_dir.join(".codex-global-state.json.bak").exists());

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }
}
