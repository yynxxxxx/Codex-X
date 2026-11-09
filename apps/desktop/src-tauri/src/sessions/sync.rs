use super::backup::{create_provider_sync_backup, prune_provider_sync_backups};
use super::catalog::{scan_catalog_internal_thread_ids, scan_catalog_sync, CatalogSyncScan};
use super::directory_identity::session_directory_identities;
use super::storage::{
    current_model_provider, discover_sqlite_databases, ensure_sqlite_discovery_writable,
    inspect_provider_rollouts, list_session_previews_with_paths, rollout_internal_thread_ids,
    scan_provider_rollouts, scan_sqlite_with_paths, validate_rollout_references, SqliteDiscovery,
};
use super::transaction::{
    execute_provider_sync_mutation, mutation_error, prepare_sqlite_updates, rollback_mutation,
    rollback_open_transactions, MutationJournal, MutationPoint,
};
use super::types::{RolloutScan, SessionSyncResult, SessionSyncStatus, SqliteScan};
use crate::error::{CodexxError, Result};
use crate::file_io::{ensure_directory, io_err};
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
        .map(|sqlite| validate_rollout_references(codex_dir, &sqlite.rollout_paths_by_thread_id))
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
    stage_changes: bool,
) -> Result<ProviderSyncScan> {
    let active_unclassified = scan_sqlite_with_paths(
        &discovery.active_paths,
        &RolloutScan::default(),
        target_provider,
    )?;
    let (mut internal_thread_ids, _classification_failures) =
        rollout_internal_thread_ids(codex_dir, &active_unclassified.rollout_paths_by_thread_id);
    internal_thread_ids.extend(scan_catalog_internal_thread_ids(&discovery.related_paths)?);
    let classification = RolloutScan {
        internal_thread_ids,
        ..RolloutScan::default()
    };
    let active_sqlite =
        scan_sqlite_with_paths(&discovery.active_paths, &classification, target_provider)?;
    let mut sqlite =
        scan_sqlite_with_paths(&discovery.thread_paths, &classification, target_provider)?;
    let mut syncable_thread_ids = sqlite.syncable_thread_ids.clone();
    let mut archived_thread_ids = sqlite.archived_thread_ids.clone();
    let mut subagent_thread_ids = sqlite.subagent_thread_ids.clone();
    let mut mismatched_thread_ids = sqlite.mismatched_thread_ids.clone();
    if active_sqlite.sqlite_dbs > 0 {
        syncable_thread_ids.retain(|id| !active_sqlite.thread_ids.contains(id));
        archived_thread_ids.retain(|id| !active_sqlite.thread_ids.contains(id));
        subagent_thread_ids.retain(|id| !active_sqlite.thread_ids.contains(id));
        mismatched_thread_ids.retain(|id| !active_sqlite.thread_ids.contains(id));
        syncable_thread_ids.extend(active_sqlite.syncable_thread_ids.iter().cloned());
        archived_thread_ids.extend(active_sqlite.archived_thread_ids.iter().cloned());
        subagent_thread_ids.extend(active_sqlite.subagent_thread_ids.iter().cloned());
        mismatched_thread_ids.extend(active_sqlite.mismatched_thread_ids.iter().cloned());
    }
    let mut excluded_thread_ids = archived_thread_ids.clone();
    excluded_thread_ids.extend(subagent_thread_ids.iter().cloned());
    excluded_thread_ids.extend(classification.internal_thread_ids.iter().cloned());
    // Without an active authority, conflicting legacy classifications stay conservatively excluded.
    syncable_thread_ids.retain(|id| !excluded_thread_ids.contains(id));
    mismatched_thread_ids.retain(|id| syncable_thread_ids.contains(id));
    sqlite.syncable_thread_ids = syncable_thread_ids.clone();
    sqlite.archived_thread_ids = archived_thread_ids;
    sqlite.subagent_thread_ids = subagent_thread_ids;
    sqlite.mismatched_thread_ids = mismatched_thread_ids;
    sqlite.mismatched_threads = sqlite.mismatched_thread_ids.len();

    let indexed_rollout_failures = {
        let authority = if active_sqlite.sqlite_dbs > 0 {
            &active_sqlite
        } else {
            &sqlite
        };
        validate_rollout_references(codex_dir, &authority.rollout_paths_by_thread_id)
    };
    let legacy_index_warnings = scan_legacy_index_warnings(codex_dir, target_provider, discovery);
    let scan_rollouts = if stage_changes {
        scan_provider_rollouts
    } else {
        inspect_provider_rollouts
    };
    let mut rollouts = scan_rollouts(
        codex_dir,
        target_provider,
        &excluded_thread_ids,
        &active_unclassified.rollout_paths_by_thread_id,
    )?;
    // Provider synchronization changes provider routing only; cwd remains independently managed.
    rollouts.cwd_by_thread_id.clear();
    // Rollout metadata can reveal an internal thread even when the SQLite row
    // has no source column (or an older index still calls it a user thread).
    excluded_thread_ids.extend(rollouts.internal_thread_ids.iter().cloned());
    rollouts
        .internal_thread_ids
        .extend(classification.internal_thread_ids.iter().cloned());
    sqlite
        .subagent_thread_ids
        .extend(rollouts.internal_thread_ids.iter().cloned());
    sqlite
        .syncable_thread_ids
        .retain(|id| !excluded_thread_ids.contains(id));
    sqlite
        .mismatched_thread_ids
        .retain(|id| !excluded_thread_ids.contains(id));
    sqlite.mismatched_threads = sqlite.mismatched_thread_ids.len();
    syncable_thread_ids.extend(rollouts.thread_ids.iter().cloned());
    syncable_thread_ids.retain(|id| !excluded_thread_ids.contains(id));
    let catalog = scan_catalog_sync(
        &discovery.thread_paths,
        &discovery.related_paths,
        target_provider,
        &syncable_thread_ids,
    )?;

    let mut scan_failures = discovery.active_scan_failures.clone();
    scan_failures.extend(active_sqlite.scan_failures.iter().cloned());
    scan_failures.extend(sqlite.scan_failures.iter().cloned());
    scan_failures.extend(indexed_rollout_failures.iter().cloned());
    // A size/buffer limit means the rollout was not completely inspected.
    // This is blocking even for an orphan, rather than a success with a warning.
    scan_failures.extend(rollouts.blocked_failures.iter().cloned());
    // A failed eligible rollout read/parse is an incomplete check, including
    // unindexed files. It must not become an apparently successful sync.
    scan_failures.extend(rollouts.scan_failures.iter().cloned());
    for path in &discovery.unreadable_paths {
        scan_failures.push(format!("无法读取会话数据库: {}", path.display()));
    }

    let mut warnings = rollouts.warnings.clone();
    // The full provider/index scans below own warnings and blocking failures;
    // repeating the lightweight identity scan's messages would duplicate them.
    warnings.extend(active_sqlite.warnings.iter().cloned());
    warnings.extend(sqlite.warnings.iter().cloned());
    warnings.extend(legacy_index_warnings);
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
    if super::wsl::is_wsl_path(&codex_dir) {
        return super::wsl::session_status(&codex_dir, target);
    }
    let discovery = discover_sqlite_databases(&codex_dir);
    session_sync_status_with_discovery(&codex_dir, target, &discovery)
}

pub(super) fn session_sync_status_with_discovery(
    codex_dir: &Path,
    target: String,
    discovery: &SqliteDiscovery,
) -> Result<SessionSyncStatus> {
    let scan = scan_provider_sync_data(codex_dir, &target, discovery, false)?;
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
            let (directory_identities, pin_scope_key) =
                session_directory_identities(codex_dir, &[]);
            return Ok(SessionSyncStatus {
                codex_dir: codex_dir.display().to_string(),
                directory_identities,
                pin_scope_key: Some(pin_scope_key),
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
                read_only_reason: None,
                scan_failures: failures,
                backup_dir: None,
                warnings: scan.warnings,
                sessions: Vec::new(),
            });
        }
    };
    super::names::apply_display_names(
        codex_dir,
        &discovery.active_first_related_paths(),
        &mut sessions,
    );
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
        // The same eligibility set drives both UI status and every mutation path.
        session.needs_sync = !session.archived
            && !session.is_subagent
            && scan.syncable_thread_ids.contains(&session.id)
            && mismatched_ids.contains(&session.id);
    }
    let needs_sync = !scan.rollouts.changes.is_empty()
        || !scan.rollouts.mismatched_thread_ids.is_empty()
        || scan.sqlite.mismatched_threads > 0
        || scan.catalog.total_updates() > 0;
    let (directory_identities, pin_scope_key) = session_directory_identities(codex_dir, &sessions);
    Ok(SessionSyncStatus {
        codex_dir: codex_dir.display().to_string(),
        directory_identities,
        pin_scope_key: Some(pin_scope_key),
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
        read_only_reason: None,
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
    super::wsl::ensure_writable(codex_dir)?;
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
    super::wsl::ensure_writable(&codex_dir)?;
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
    // This status already completed the same full scan. Repeating it before
    // acquiring SQLite locks neither establishes a newer protected snapshot
    // nor improves the mutation guard; the locked scan below does that.
    if !initial_status.needs_sync {
        return Ok(SessionSyncResult {
            status: initial_status,
            updated_rollouts: 0,
            updated_threads: 0,
            backup_dir: String::new(),
        });
    }

    hook(MutationPoint::BeforeSqliteLock)?;
    let mut pending_sqlite = prepare_sqlite_updates(&discovery.related_paths)?;
    let scan = match scan_provider_sync_data(&codex_dir, &target_provider, &discovery, true) {
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
        let status = session_sync_status_with_discovery(&codex_dir, target_provider, &discovery)?;
        if !status.scan_complete {
            return Err(scan_failure_error(&status.scan_failures));
        }
        if status.needs_sync {
            return Err(CodexxError::Config(
                "会话状态在检查后发生变化，未完成同步；请重新检查。".to_string(),
            ));
        }
        return Ok(SessionSyncResult {
            status,
            updated_rollouts: 0,
            updated_threads: 0,
            backup_dir: String::new(),
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

    let mut status = session_sync_status_with_discovery(&codex_dir, target_provider, &discovery)
        .and_then(|status| {
            if !status.scan_complete {
                return Err(scan_failure_error(&status.scan_failures));
            }
            if status.needs_sync {
                return Err(CodexxError::Config(
                    "会话日志与索引仍不一致，未完成同步；请重新检查。".to_string(),
                ));
            }
            Ok(status)
        })
        .map_err(|error| {
            let recovery_errors = rollback_mutation(&journal, &mut pending_sqlite);
            mutation_error(
                CodexxError::Config(format!("同步后复查未通过，本轮同步未完成：{error}")),
                recovery_errors,
            )
        })?;
    let prune_warning = prune_provider_sync_backups(&codex_dir).err();
    status.backup_dir = Some(backup.dir.display().to_string());
    if prune_warning.is_some() {
        status
            .warnings
            .push("同步已完成，但旧备份暂未清理。".to_string());
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
    use rusqlite::Connection;
    use sha2::{Digest, Sha256};
    use std::io::{BufRead, Read, Seek, SeekFrom};
    use std::sync::atomic::{AtomicU64, Ordering};

    const SHARED_SESSION_PROVIDER: &str = "custom";
    const LEGACY_LIMIT: u64 = 32 * 1024 * 1024;

    mod compressed {
        use super::*;
        include!("compressed_sync_tests.rs");
    }

    mod benchmark {
        use super::*;
        include!("session_sync_benchmark.rs");
    }

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

    fn pad_rollout_to_size(path: &Path, size: u64) {
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        let mut remaining = size.checked_sub(file.metadata().unwrap().len()).unwrap();
        let padding = [b' '; 64 * 1024];
        while remaining > 0 {
            let count = remaining.min(padding.len() as u64) as usize;
            file.write_all(&padding[..count]).unwrap();
            remaining -= count as u64;
        }
    }

    fn rollout_digest(path: &Path) -> Vec<u8> {
        rollout_digest_from_offset(path, 0)
    }

    fn rollout_digest_from_offset(path: &Path, offset: u64) -> Vec<u8> {
        let mut file = fs::File::open(path).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        let mut digest = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = file.read(&mut buffer).unwrap();
            if count == 0 {
                return digest.finalize().to_vec();
            }
            digest.update(&buffer[..count]);
        }
    }

    fn append_chat_record(path: &Path) {
        fs::OpenOptions::new().append(true).open(path).unwrap()
            .write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"keep this conversation\"}}\n")
            .unwrap();
    }

    fn read_first_rollout_record(path: &Path) -> String {
        let mut first_record = String::new();
        std::io::BufReader::new(fs::File::open(path).unwrap())
            .read_line(&mut first_record)
            .unwrap();
        first_record
    }

    #[test]
    fn streamed_rollouts_below_at_and_above_legacy_limit_sync_logs_databases_and_catalogs() {
        for (name, size) in [
            ("normal", 512),
            ("below-old-limit", LEGACY_LIMIT - 1),
            ("exact-old-limit", LEGACY_LIMIT),
            ("above-old-limit", LEGACY_LIMIT + 1),
        ] {
            let dir = temp_codex_dir(name);
            write_config(&dir, "custom");
            let id = "019f6000-0000-7000-8000-000000000701";
            let rollout = write_rollout(&dir, id, "openai");
            let old_record_len = read_first_rollout_record(&rollout).len() as u64;
            append_chat_record(&rollout);
            pad_rollout_to_size(&rollout, size);
            let conversation_before = rollout_digest_from_offset(&rollout, old_record_len);
            let database = dir.join("state_5.sqlite");
            create_thread_database_with_rollout(&database, id, "openai", &rollout);
            let catalog = dir.join("sqlite/codex-dev.db");
            create_catalog_database(&catalog, &[(id, "openai")]);

            let status = session_sync_status_inner(Some(dir.display().to_string()), None).unwrap();
            assert!(status.scan_complete, "{name}: {:?}", status.scan_failures);
            assert!(status.needs_sync, "{name}");
            let result =
                sync_sessions_provider_inner(Some(dir.display().to_string()), None).unwrap();
            assert!(
                result.status.scan_complete && !result.status.needs_sync,
                "{name}"
            );
            assert_eq!(result.updated_rollouts, 1, "{name}");
            assert_eq!(thread_provider(&database, id), "custom", "{name}");
            assert_eq!(catalog_provider(&catalog, id), "custom", "{name}");
            let first_record = read_first_rollout_record(&rollout);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&first_record).unwrap()["payload"]
                    ["model_provider"],
                "custom"
            );
            assert_eq!(fs::metadata(&rollout).unwrap().len(), size, "{name}");
            assert_eq!(
                rollout_digest_from_offset(&rollout, first_record.len() as u64),
                conversation_before,
                "conversation bytes must be preserved: {name}"
            );
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn streamed_ninety_seven_mib_single_chat_line_syncs_without_losing_conversation_bytes() {
        let dir = temp_codex_dir("streamed-97-mib-chat-record");
        write_config(&dir, "custom");
        let id = "019f6000-0000-7000-8000-000000000709";
        let rollout = write_rollout(&dir, id, "openai");
        let first_record_len = read_first_rollout_record(&rollout).len() as u64;
        let total_size = 97 * 1024 * 1024u64;
        let prefix =
            b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"";
        let suffix = b"\"}}\n";
        let mut file = fs::OpenOptions::new().append(true).open(&rollout).unwrap();
        file.write_all(prefix).unwrap();
        let mut remaining =
            total_size - first_record_len - prefix.len() as u64 - suffix.len() as u64;
        let body_chunk = [b'x'; 64 * 1024];
        while remaining > 0 {
            let count = remaining.min(body_chunk.len() as u64) as usize;
            file.write_all(&body_chunk[..count]).unwrap();
            remaining -= count as u64;
        }
        file.write_all(suffix).unwrap();
        file.sync_all().unwrap();
        drop(file);
        let conversation_before = rollout_digest_from_offset(&rollout, first_record_len);
        let database = dir.join("state_5.sqlite");
        create_thread_database_with_rollout(&database, id, "openai", &rollout);
        let catalog = dir.join("sqlite/codex-dev.db");
        create_catalog_database(&catalog, &[(id, "openai")]);

        let result = sync_sessions_provider_inner(Some(dir.display().to_string()), None)
            .expect("a large valid single JSONL record must synchronize successfully");

        assert!(
            result.status.scan_complete && !result.status.needs_sync,
            "{:?}",
            result.status.scan_failures
        );
        assert_eq!(result.updated_rollouts, 1);
        assert_eq!(thread_provider(&database, id), "custom");
        assert_eq!(catalog_provider(&catalog, id), "custom");
        let first_record = read_first_rollout_record(&rollout);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&first_record).unwrap()["payload"]
                ["model_provider"],
            "custom"
        );
        assert_eq!(fs::metadata(&rollout).unwrap().len(), total_size);
        assert_eq!(
            rollout_digest_from_offset(&rollout, first_record.len() as u64),
            conversation_before,
            "the large chat record must retain every original byte"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn streamed_multiple_rollouts_exceeding_legacy_snapshot_budget_sync_together() {
        let dir = temp_codex_dir("streamed-multi-large-rollout-budget");
        write_config(&dir, "custom");
        let ids = [
            "019f6000-0000-7000-8000-000000000711",
            "019f6000-0000-7000-8000-000000000712",
            "019f6000-0000-7000-8000-000000000713",
        ];
        let file_size = 45 * 1024 * 1024u64;
        assert!(file_size * ids.len() as u64 > 128 * 1024 * 1024);
        let mut rollouts = Vec::new();
        let mut conversation_digests = Vec::new();
        for id in ids {
            let rollout = write_rollout(&dir, id, "openai");
            let first_record_len = read_first_rollout_record(&rollout).len() as u64;
            append_chat_record(&rollout);
            pad_rollout_to_size(&rollout, file_size);
            conversation_digests.push(rollout_digest_from_offset(&rollout, first_record_len));
            rollouts.push(rollout);
        }
        let database = dir.join("state_5.sqlite");
        create_thread_database_with_rollout(&database, ids[0], "openai", &rollouts[0]);
        let conn = Connection::open(&database).unwrap();
        for index in 1..ids.len() {
            conn.execute(
                "INSERT INTO threads (id, model_provider, title, rollout_path) VALUES (?1, 'openai', 'large history', ?2)",
                (ids[index], rollouts[index].display().to_string()),
            ).unwrap();
        }
        drop(conn);
        let catalog = dir.join("sqlite/codex-dev.db");
        create_catalog_database(&catalog, &ids.map(|id| (id, "openai")));

        let result = sync_sessions_provider_inner(Some(dir.display().to_string()), None)
            .expect("the combined size of valid rollouts must not block synchronization");

        assert!(
            result.status.scan_complete && !result.status.needs_sync,
            "{:?}",
            result.status.scan_failures
        );
        assert_eq!(result.updated_rollouts, ids.len());
        for index in 0..ids.len() {
            assert_eq!(thread_provider(&database, ids[index]), "custom");
            assert_eq!(catalog_provider(&catalog, ids[index]), "custom");
            let first_record = read_first_rollout_record(&rollouts[index]);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&first_record).unwrap()["payload"]
                    ["model_provider"],
                "custom"
            );
            assert_eq!(fs::metadata(&rollouts[index]).unwrap().len(), file_size);
            assert_eq!(
                rollout_digest_from_offset(&rollouts[index], first_record.len() as u64),
                conversation_digests[index],
                "conversation bytes must remain unchanged for {}",
                ids[index]
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn large_invalid_jsonl_rollout_reports_parse_failure_and_keeps_indexes_unchanged() {
        for (name, file_provider, database_provider) in [
            ("file-and-index-old", "openai", "openai"),
            ("index-already-target", "openai", "custom"),
            ("first-record-matches", "custom", "custom"),
        ] {
            let dir = temp_codex_dir(name);
            write_config(&dir, "custom");
            let id = "019f6000-0000-7000-8000-000000000702";
            let rollout = write_rollout(&dir, id, file_provider);
            fs::OpenOptions::new()
                .write(true)
                .open(&rollout)
                .unwrap()
                .set_len(LEGACY_LIMIT + 1)
                .unwrap();
            let before = rollout_digest(&rollout);
            let database = dir.join("state_5.sqlite");
            create_thread_database_with_rollout(&database, id, database_provider, &rollout);
            let catalog = dir.join("sqlite/codex-dev.db");
            create_catalog_database(&catalog, &[(id, database_provider)]);

            let status = session_sync_status_inner(Some(dir.display().to_string()), None).unwrap();
            assert!(!status.scan_complete, "{name}");
            assert!(
                status
                    .scan_failures
                    .iter()
                    .any(|failure| failure.contains("JSONL")),
                "{name}: {:?}",
                status.scan_failures
            );
            let error =
                sync_sessions_provider_inner(Some(dir.display().to_string()), None).unwrap_err();
            assert!(error.to_string().contains("JSONL"), "{name}: {error}");
            assert_eq!(thread_provider(&database, id), database_provider, "{name}");
            assert_eq!(catalog_provider(&catalog, id), database_provider, "{name}");
            assert_eq!(rollout_digest(&rollout), before, "{name}");
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn large_invalid_orphan_blocks_sync_without_changing_normal_logs_or_indexes() {
        let dir = temp_codex_dir("large-invalid-orphan-blocked");
        write_config(&dir, "custom");
        let ordinary_id = "019f6000-0000-7000-8000-000000000703";
        let ordinary = write_rollout(&dir, ordinary_id, "openai");
        let before = fs::read(&ordinary).unwrap();
        let database = dir.join("state_5.sqlite");
        create_thread_database_with_rollout(&database, ordinary_id, "openai", &ordinary);
        let catalog = dir.join("sqlite/codex-dev.db");
        create_catalog_database(&catalog, &[(ordinary_id, "openai")]);
        let orphan_id = "019f6000-0000-7000-8000-000000000704";
        let invalid_orphan = write_rollout(&dir, orphan_id, "openai");
        fs::OpenOptions::new()
            .write(true)
            .open(&invalid_orphan)
            .unwrap()
            .set_len(LEGACY_LIMIT + 1)
            .unwrap();
        let orphan_before = rollout_digest(&invalid_orphan);

        let status = session_sync_status_inner(Some(dir.display().to_string()), None).unwrap();
        assert!(!status.scan_complete && status.needs_sync);
        let error =
            sync_sessions_provider_inner(Some(dir.display().to_string()), None).unwrap_err();
        assert!(error.to_string().contains("JSONL"), "{error}");
        assert_eq!(fs::read(&ordinary).unwrap(), before);
        assert_eq!(rollout_digest(&invalid_orphan), orphan_before);
        assert_eq!(thread_provider(&database, ordinary_id), "openai");
        assert_eq!(catalog_provider(&catalog, ordinary_id), "openai");
        assert!(catalog_provider_and_visibility(&catalog, orphan_id).is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_orphan_read_or_parse_cannot_be_reported_as_successful_sync() {
        for suffix in [b"\xff".as_slice(), b"not-json\n".as_slice()] {
            let dir = temp_codex_dir("failed-orphan-check-blocks-sync");
            write_config(&dir, "custom");
            let id = "019f6000-0000-7000-8000-000000000705";
            let ordinary = write_rollout(&dir, id, "openai");
            let original = fs::read(&ordinary).unwrap();
            let database = dir.join("state_5.sqlite");
            create_thread_database_with_rollout(&database, id, "openai", &ordinary);
            let catalog = dir.join("sqlite/codex-dev.db");
            create_catalog_database(&catalog, &[(id, "openai")]);
            let orphan = write_rollout(&dir, "019f6000-0000-7000-8000-000000000706", "openai");
            fs::OpenOptions::new()
                .append(true)
                .open(&orphan)
                .unwrap()
                .write_all(suffix)
                .unwrap();
            let orphan_original = fs::read(&orphan).unwrap();

            let status = session_sync_status_inner(Some(dir.display().to_string()), None).unwrap();
            assert!(!status.scan_complete && !status.scan_failures.is_empty());
            assert!(sync_sessions_provider_inner(Some(dir.display().to_string()), None).is_err());
            assert_eq!(thread_provider(&database, id), "openai");
            assert_eq!(catalog_provider(&catalog, id), "openai");
            assert_eq!(fs::read(&ordinary).unwrap(), original);
            assert_eq!(fs::read(&orphan).unwrap(), orphan_original);
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn failed_final_recheck_rolls_back_committed_logs_and_indexes() {
        for scenario in ["invalid", "large-invalid", "new-mismatch"] {
            let dir = temp_codex_dir("failed-final-recheck");
            write_config(&dir, "custom");
            let id = "019f6000-0000-7000-8000-000000000707";
            let ordinary = write_rollout(&dir, id, "openai");
            let original = fs::read(&ordinary).unwrap();
            let database = dir.join("state_5.sqlite");
            create_thread_database_with_rollout(&database, id, "openai", &ordinary);
            let catalog = dir.join("sqlite/codex-dev.db");
            create_catalog_database(&catalog, &[(id, "openai")]);
            let late = dir.join("sessions/rollout-test-019f6000-0000-7000-8000-000000000708.jsonl");
            let mut late_digest = None;

            let error =
                sync_sessions_provider_with_hook(Some(dir.display().to_string()), None, |point| {
                    if matches!(point, MutationPoint::AfterSqliteCommit(_)) && !late.exists() {
                        if scenario == "invalid" {
                            fs::write(&late, b"\xff").unwrap();
                        } else {
                            write_rollout_at(
                                &late,
                                "019f6000-0000-7000-8000-000000000708",
                                "openai",
                            );
                            if scenario == "large-invalid" {
                                fs::OpenOptions::new()
                                    .write(true)
                                    .open(&late)
                                    .unwrap()
                                    .set_len(LEGACY_LIMIT + 1)
                                    .unwrap();
                            }
                        }
                        late_digest = Some(rollout_digest(&late));
                    }
                    Ok(())
                })
                .expect_err("a failed final check must not return a successful sync");

            assert!(
                error.to_string().contains("同步后复查未通过"),
                "{scenario}: {error}"
            );
            assert_eq!(thread_provider(&database, id), "openai", "{scenario}");
            assert_eq!(catalog_provider(&catalog, id), "openai", "{scenario}");
            assert_eq!(fs::read(&ordinary).unwrap(), original, "{scenario}");
            assert_eq!(
                rollout_digest(&late),
                late_digest.unwrap(),
                "preserve external file: {scenario}"
            );
            fs::remove_dir_all(dir).unwrap();
        }
    }

    fn write_subagent_rollout(
        codex_dir: &Path,
        id: &str,
        parent_id: &str,
        provider: &str,
    ) -> std::path::PathBuf {
        let path = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
        fs::create_dir_all(path.parent().expect("rollout parent")).expect("create rollout parent");
        let record = serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": id,
                "model_provider": provider,
                "source": {
                    "subagent": {
                        "thread_spawn": {
                            "parent_thread_id": parent_id,
                            "depth": 1,
                            "agent_path": "/root/test-agent"
                        }
                    }
                }
            }
        });
        fs::write(&path, format!("{record}\n")).expect("write subagent rollout");
        path
    }

    fn rows_except_parent(
        path: &Path,
        table: &str,
        id_column: &str,
        parent: &str,
    ) -> Vec<Vec<rusqlite::types::Value>> {
        let conn = Connection::open(path).expect("open snapshot database");
        let mut stmt = conn
            .prepare(&format!(
                "SELECT * FROM {table} WHERE {id_column} <> ?1 ORDER BY {id_column}"
            ))
            .expect("prepare row snapshot");
        let column_count = stmt.column_count();
        stmt.query_map([parent], |row| {
            (0..column_count).map(|index| row.get(index)).collect()
        })
        .expect("read row snapshots")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("collect row snapshots")
    }

    #[test]
    fn internal_rollout_evidence_prevents_catalog_reactivation_and_insertion() {
        let codex_dir = temp_codex_dir("internal-catalog-regression");
        write_config(&codex_dir, "custom");
        let parent = "019f6000-0000-7000-8000-000000000900";
        let hidden = "019f6000-0000-7000-8000-000000000901";
        let missing = "019f6000-0000-7000-8000-000000000902";
        let catalog_only = "019f6000-0000-7000-8000-000000000903";
        let source_only = "019f6000-0000-7000-8000-000000000904";
        let parent_rollout = write_rollout(&codex_dir, parent, "openai");
        let state = codex_dir.join("state_10.sqlite");
        create_thread_database_with_rollout(&state, parent, "openai", &parent_rollout);
        let conn = Connection::open(&state).expect("open regression index");
        conn.execute_batch(
            "ALTER TABLE threads ADD COLUMN source TEXT;
            ALTER TABLE threads ADD COLUMN thread_source TEXT;
            ALTER TABLE threads ADD COLUMN parent_thread_id TEXT;
            ALTER TABLE threads ADD COLUMN cwd TEXT;",
        )
        .expect("extend source metadata");
        conn.execute("UPDATE threads SET title = ?1, source = 'cli', thread_source = 'user', cwd = '/tmp/project' WHERE id = ?2",
            ("Review this error: The following is the Codex agent history whose request action you are assessing", parent))
            .expect("set genuine parent title");
        let mut internal_files = Vec::new();
        for (id, rollout_source, sqlite_source) in [
            (hidden, serde_json::json!({"internal": "guardian"}), "cli"),
            (
                missing,
                serde_json::json!({"subagent": {"review": null}}),
                "cli",
            ),
            (catalog_only, serde_json::json!("cli"), "cli"),
            (source_only, serde_json::json!("cli"), "subagent_review"),
        ] {
            let path = write_rollout(&codex_dir, id, "openai");
            let record = serde_json::json!({"type":"session_meta", "payload":{
                "id": id, "model_provider":"openai", "source":rollout_source, "cwd":"/tmp/private"
            }});
            fs::write(&path, format!("{record}\n")).expect("write source metadata");
            conn.execute("INSERT INTO threads (id, model_provider, title, rollout_path, source, thread_source, cwd)
                VALUES (?1, 'openai', 'codex-auto-review', ?2, ?3, NULL, '/tmp/private')",
                (id, path.display().to_string(), sqlite_source)).expect("insert internal source");
            internal_files.push((
                path.clone(),
                fs::read(&path).expect("snapshot internal rollout"),
            ));
        }
        drop(conn);
        let catalog = codex_dir.join("sqlite/codex-dev.db");
        create_catalog_database(
            &catalog,
            &[
                (parent, "openai"),
                (hidden, "openai"),
                (catalog_only, "openai"),
            ],
        );
        let conn = Connection::open(&catalog).expect("open regression catalog");
        conn.execute(
            "UPDATE local_thread_catalog SET missing_candidate = 1 WHERE thread_id = ?1",
            [hidden],
        )
        .expect("keep review hidden");
        conn.execute("UPDATE local_thread_catalog SET source_kind = 'internal', thread_source = 'guardian_review', missing_candidate = 1 WHERE thread_id = ?1", [catalog_only])
            .expect("mark catalog-only internal source");
        drop(conn);
        let before_threads = rows_except_parent(&state, "threads", "id", parent);
        let before_catalog =
            rows_except_parent(&catalog, "local_thread_catalog", "thread_id", parent);
        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan mixed sources");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert_eq!(status.mismatched_sessions, 1, "{status:?}");
        assert!(
            status
                .sessions
                .iter()
                .find(|session| session.id == parent)
                .expect("parent preview")
                .needs_sync
        );
        for session in status
            .sessions
            .iter()
            .filter(|session| session.id != parent)
        {
            assert!(session.is_subagent, "internal preview {}", session.id);
            assert!(
                !session.needs_sync,
                "internal must not be pending {}",
                session.id
            );
        }
        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("sync only parent");
        assert!(!result.status.needs_sync);
        assert_eq!(result.updated_rollouts, 1);
        assert_eq!(thread_provider(&state, parent), "custom");
        assert_eq!(catalog_provider(&catalog, parent), "custom");
        assert_eq!(
            rows_except_parent(&state, "threads", "id", parent),
            before_threads
        );
        assert_eq!(
            rows_except_parent(&catalog, "local_thread_catalog", "thread_id", parent),
            before_catalog
        );
        assert!(catalog_provider_and_visibility(&catalog, missing).is_none());
        assert!(catalog_provider_and_visibility(&catalog, source_only).is_none());
        for (path, original) in internal_files {
            assert_eq!(
                fs::read(path).expect("read preserved internal rollout"),
                original
            );
        }
        fs::remove_dir_all(codex_dir).expect("remove regression fixture");
    }

    #[test]
    fn authoritative_user_rollout_is_not_hidden_by_an_internal_duplicate() {
        let codex_dir = temp_codex_dir("user-internal-duplicate");
        write_config(&codex_dir, "custom");
        let id = "019f6000-0000-7000-8000-000000000905";
        let active = codex_dir.join("sessions/rollout-active-name.jsonl");
        write_rollout_at(&active, id, "openai");
        let stale = write_subagent_rollout(&codex_dir, id, "old-parent", "openai");
        let stale_bytes = fs::read(&stale).expect("snapshot stale internal duplicate");
        let database = codex_dir.join("state_10.sqlite");
        create_thread_database_with_rollout(&database, id, "openai", &active);
        let catalog = codex_dir.join("sqlite/codex-dev.db");
        create_catalog_database(&catalog, &[]);
        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan authoritative user");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert_eq!(status.subagent_threads, 0);
        assert_eq!(status.mismatched_sessions, 1);
        assert!(!status.sessions[0].is_subagent);
        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("sync authoritative user");
        assert_eq!(result.updated_rollouts, 1);
        assert_eq!(thread_provider(&database, id), "custom");
        assert_eq!(catalog_provider(&catalog, id), "custom");
        assert_eq!(
            fs::read(stale).expect("read untouched internal duplicate"),
            stale_bytes
        );
        fs::remove_dir_all(codex_dir).expect("remove duplicate fixture");
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
    fn malformed_standard_orphan_keeps_check_incomplete_without_mutation() {
        let codex_dir = temp_codex_dir("malformed-standard-orphan");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let active_id = "019f6000-0000-7000-8000-000000000502";
        let orphan_id = "019f6000-0000-7000-8000-000000000503";
        create_thread_database(
            &codex_dir.join("state_5.sqlite"),
            active_id,
            SHARED_SESSION_PROVIDER,
        );
        let active = write_rollout(&codex_dir, active_id, SHARED_SESSION_PROVIDER);
        let active_before = fs::read(&active).unwrap();
        let database = codex_dir.join("state_5.sqlite");
        let database_before = fs::read(&database).unwrap();
        let orphan = codex_dir.join(format!("sessions/rollout-test-{orphan_id}.jsonl"));
        fs::write(&orphan, b"\xff").expect("write invalid orphan rollout");

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan active sessions only");
        assert!(!status.scan_complete);
        assert!(!status.scan_failures.is_empty());
        assert_eq!(status.rollout_files, 2);
        assert_eq!(status.session_meta_count, 1);
        assert!(!status.needs_sync);
        assert!(sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None).is_err());
        assert_eq!(fs::read(&active).unwrap(), active_before);
        assert_eq!(fs::read(&database).unwrap(), database_before);
        assert_eq!(fs::read(&orphan).unwrap(), b"\xff");

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn malformed_unreferenced_rollout_keeps_check_incomplete_without_mutation() {
        let codex_dir = temp_codex_dir("malformed-unreferenced-rollout");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let active_id = "019f6000-0000-7000-8000-000000000504";
        create_thread_database(
            &codex_dir.join("state_5.sqlite"),
            active_id,
            SHARED_SESSION_PROVIDER,
        );
        let active = write_rollout(&codex_dir, active_id, SHARED_SESSION_PROVIDER);
        let active_before = fs::read(&active).unwrap();
        let database = codex_dir.join("state_5.sqlite");
        let database_before = fs::read(&database).unwrap();
        let orphan = codex_dir.join("sessions/rollout-imported-orphan.jsonl");
        fs::write(&orphan, b"\xff").expect("write invalid unreferenced rollout");

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan referenced sessions only");
        assert!(!status.scan_complete);
        assert!(!status.scan_failures.is_empty());
        assert_eq!(status.rollout_files, 2);
        assert_eq!(status.session_meta_count, 1);
        assert!(sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None).is_err());
        assert_eq!(fs::read(&active).unwrap(), active_before);
        assert_eq!(fs::read(&database).unwrap(), database_before);
        assert_eq!(fs::read(&orphan).unwrap(), b"\xff");

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
    fn reverted_rollout_sync_uses_stable_thread_id_and_keeps_internal_threads_hidden() {
        let codex_dir = temp_codex_dir("reverted-rollout-sync");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let parent = "019f6000-0000-7000-8000-000000000920";
        let parent_rollout_id = "019f6000-0000-7000-8000-000000000921";
        let internal = "019f6000-0000-7000-8000-000000000922";
        let internal_rollout_id = "019f6000-0000-7000-8000-000000000923";
        let parent_path = codex_dir.join(format!(
            "sessions/rollout-2026-09-17T13-10-16-{parent}_{parent_rollout_id}.jsonl"
        ));
        write_rollout_at(&parent_path, parent, "openai");
        let internal_path = codex_dir.join(format!(
            "sessions/rollout-2026-09-17T13-10-17-{internal}_{internal_rollout_id}.jsonl"
        ));
        let internal_metadata = serde_json::json!({"type":"session_meta","payload":{
            "id":internal, "model_provider":"openai", "source":{"internal":"guardian"}
        }});
        fs::write(&internal_path, format!("{internal_metadata}\n"))
            .expect("write reverted internal rollout");
        let internal_bytes = fs::read(&internal_path).expect("snapshot internal rollout");
        let database = codex_dir.join("state_5.sqlite");
        create_thread_database_with_rollout(&database, parent, "openai", &parent_path);
        Connection::open(&database).expect("open reverted thread index").execute(
            "INSERT INTO threads (id, model_provider, title, rollout_path) VALUES (?1, 'openai', 'internal review', ?2)",
            (internal, internal_path.display().to_string()),
        ).expect("insert internal thread without database source markers");
        let before_internal = rows_except_parent(&database, "threads", "id", parent);
        let catalog = codex_dir.join("sqlite/codex-dev.db");
        create_catalog_database(&catalog, &[]);

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("check reverted rollouts");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert!(status
            .warnings
            .iter()
            .all(|warning| !warning.contains("线程 ID 不一致")));
        assert_eq!(status.mismatched_sessions, 1);
        assert_eq!(status.subagent_threads, 1);
        for preview in &status.sessions {
            assert_eq!(preview.is_subagent, preview.id == internal);
            assert_eq!(preview.needs_sync, preview.id == parent);
        }
        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("synchronize reverted user rollout through transaction guards");
        assert!(result.status.scan_complete);
        assert!(!result.status.needs_sync);
        assert_eq!(result.updated_rollouts, 1);
        assert_eq!(thread_provider(&database, parent), SHARED_SESSION_PROVIDER);
        assert_eq!(catalog_provider(&catalog, parent), SHARED_SESSION_PROVIDER);
        for excluded in [parent_rollout_id, internal, internal_rollout_id] {
            assert!(catalog_provider_and_visibility(&catalog, excluded).is_none());
        }
        assert_eq!(
            rows_except_parent(&database, "threads", "id", parent),
            before_internal
        );
        assert_eq!(
            fs::read(&internal_path).expect("read unchanged internal rollout"),
            internal_bytes
        );
        let record: serde_json::Value = serde_json::from_str(
            fs::read_to_string(&parent_path)
                .expect("read synchronized parent")
                .trim(),
        )
        .expect("parse synchronized parent metadata");
        assert_eq!(record["payload"]["id"], parent);
        assert_eq!(record["payload"]["model_provider"], SHARED_SESSION_PROVIDER);
        fs::remove_dir_all(codex_dir).expect("remove reverted rollout fixture");
    }

    #[test]
    fn reverted_rollout_cannot_be_indexed_by_its_distinct_rollout_id() {
        let codex_dir = temp_codex_dir("reverted-rollout-wrong-index-id");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let thread_id = "019f6000-0000-7000-8000-000000000924";
        let rollout_id = "019f6000-0000-7000-8000-000000000925";
        let path = codex_dir.join(format!(
            "sessions/rollout-2026-09-17T13-10-16-{thread_id}_{rollout_id}.jsonl"
        ));
        write_rollout_at(&path, thread_id, "openai");
        let original = fs::read(&path).expect("snapshot original rollout");
        let database = codex_dir.join("state_5.sqlite");
        create_thread_database_with_rollout(&database, rollout_id, "openai", &path);
        let catalog = codex_dir.join("sqlite/codex-dev.db");
        create_catalog_database(&catalog, &[]);

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("check mismatched reverted rollout reference");
        assert!(!status.scan_complete);
        assert!(status
            .scan_failures
            .iter()
            .any(|failure| failure.contains("线程 ID 不一致")));
        sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect_err("rollout ID must not substitute for its stable thread ID");
        assert_eq!(thread_provider(&database, rollout_id), "openai");
        assert_eq!(fs::read(&path).expect("read protected rollout"), original);
        assert!(catalog_provider_and_visibility(&catalog, thread_id).is_none());
        assert!(catalog_provider_and_visibility(&catalog, rollout_id).is_none());
        fs::remove_dir_all(codex_dir).expect("remove invalid reverted reference fixture");
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
    fn active_user_thread_overrides_stale_legacy_subagent_marker() {
        let codex_dir = temp_codex_dir("active-user-legacy-subagent");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let id = "019f6000-0000-7000-8000-000000000575";
        let active = codex_dir.join("state_5.sqlite");
        let legacy = codex_dir.join("sqlite/state_5.sqlite");
        for database in [&active, &legacy] {
            create_thread_database(database, id, "openai");
            Connection::open(database)
                .expect("open thread classification database")
                .execute_batch("ALTER TABLE threads ADD COLUMN thread_source TEXT;")
                .expect("add thread source column");
        }
        Connection::open(&active)
            .expect("open active user database")
            .execute(
                "UPDATE threads SET thread_source = 'user' WHERE id = ?1",
                [id],
            )
            .expect("mark active thread as user");
        Connection::open(&legacy)
            .expect("open legacy subagent database")
            .execute(
                "UPDATE threads SET thread_source = 'subagent' WHERE id = ?1",
                [id],
            )
            .expect("mark legacy thread as subagent");
        let rollout = write_rollout(&codex_dir, id, "openai");
        let catalog = codex_dir.join("sqlite/codex-dev.db");
        create_catalog_database(&catalog, &[(id, "openai")]);

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan active user with stale legacy subagent marker");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert!(status.needs_sync);
        assert_eq!(status.subagent_threads, 0);
        assert_eq!(status.mismatched_threads, 1);
        assert_eq!(status.mismatched_sessions, 1);
        let preview = status
            .sessions
            .iter()
            .find(|session| session.id == id)
            .expect("active user preview");
        assert!(!preview.is_subagent);
        assert!(preview.needs_sync);

        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("synchronize authoritative active user thread");
        assert!(!result.status.needs_sync);
        assert_eq!(result.updated_rollouts, 1);
        assert!(result.updated_threads > 0);
        assert_eq!(thread_provider(&active, id), SHARED_SESSION_PROVIDER);
        assert_eq!(thread_provider(&legacy, id), "openai");
        assert_eq!(catalog_provider(&catalog, id), SHARED_SESSION_PROVIDER);
        assert!(fs::read_to_string(rollout)
            .expect("read synchronized active user rollout")
            .contains("\"model_provider\":\"custom\""));

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
    fn subagents_are_visible_but_excluded_from_provider_sync() {
        let codex_dir = temp_codex_dir("subagents-not-syncable");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let parent_id = "019f6000-0000-7000-8000-000000000570";
        let edge_child_id = "019f6000-0000-7000-8000-000000000571";
        let source_child_id = "019f6000-0000-7000-8000-000000000572";
        let parent_rollout = write_rollout(&codex_dir, parent_id, SHARED_SESSION_PROVIDER);
        let edge_child_rollout =
            write_subagent_rollout(&codex_dir, edge_child_id, parent_id, "openai");
        let source_child_rollout =
            write_subagent_rollout(&codex_dir, source_child_id, parent_id, "openai");
        let edge_child_original = fs::read(&edge_child_rollout).expect("read edge child rollout");
        let source_child_original =
            fs::read(&source_child_rollout).expect("read source child rollout");

        let state = codex_dir.join("state_5.sqlite");
        create_thread_database_with_rollout(
            &state,
            parent_id,
            SHARED_SESSION_PROVIDER,
            &parent_rollout,
        );
        let state_conn = Connection::open(&state).expect("open subagent thread database");
        state_conn
            .execute_batch(
                "ALTER TABLE threads ADD COLUMN thread_source TEXT;
                 ALTER TABLE threads ADD COLUMN source TEXT;
                 CREATE TABLE thread_spawn_edges (
                    parent_thread_id TEXT NOT NULL,
                    child_thread_id TEXT NOT NULL
                 );",
            )
            .expect("add subagent metadata");
        state_conn
            .execute(
                "INSERT INTO threads (id, model_provider, title, rollout_path)
                 VALUES (?1, 'openai', 'edge child', ?2)",
                (edge_child_id, edge_child_rollout.display().to_string()),
            )
            .expect("insert edge child thread");
        state_conn
            .execute(
                "INSERT INTO thread_spawn_edges (parent_thread_id, child_thread_id)
                 VALUES (?1, ?2)",
                (parent_id, edge_child_id),
            )
            .expect("insert child spawn edge");
        let source = serde_json::json!({
            "subagent": {
                "thread_spawn": {
                    "parent_thread_id": parent_id,
                    "depth": 1
                }
            }
        })
        .to_string();
        state_conn
            .execute(
                "INSERT INTO threads (id, model_provider, title, rollout_path, source)
                 VALUES (?1, 'openai', 'source child', ?2, ?3)",
                (
                    source_child_id,
                    source_child_rollout.display().to_string(),
                    source,
                ),
            )
            .expect("insert source child thread");
        drop(state_conn);

        let catalog = codex_dir.join("sqlite/codex-dev.db");
        create_catalog_database(
            &catalog,
            &[
                (parent_id, SHARED_SESSION_PROVIDER),
                (edge_child_id, "openai"),
                (source_child_id, "openai"),
            ],
        );

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan subagent threads");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert!(!status.needs_sync);
        assert_eq!(status.sqlite_threads, 3);
        assert_eq!(status.top_level_threads, 1);
        assert_eq!(status.subagent_threads, 2);
        assert_eq!(status.mismatched_threads, 0);
        assert_eq!(status.mismatched_rollouts, 0);
        assert_eq!(status.mismatched_session_meta, 0);
        assert_eq!(status.mismatched_sessions, 0);
        for child_id in [edge_child_id, source_child_id] {
            let preview = status
                .sessions
                .iter()
                .find(|session| session.id == child_id)
                .expect("subagent preview remains visible");
            assert!(preview.is_subagent);
            assert!(!preview.needs_sync);
        }

        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("subagent-only mismatch is a no-op");
        assert_eq!(result.updated_rollouts, 0);
        assert_eq!(result.updated_threads, 0);
        assert!(result.backup_dir.is_empty());
        assert_eq!(thread_provider(&state, edge_child_id), "openai");
        assert_eq!(thread_provider(&state, source_child_id), "openai");
        assert_eq!(catalog_provider(&catalog, edge_child_id), "openai");
        assert_eq!(catalog_provider(&catalog, source_child_id), "openai");
        assert_eq!(
            fs::read(edge_child_rollout).expect("read preserved edge child rollout"),
            edge_child_original
        );
        assert_eq!(
            fs::read(source_child_rollout).expect("read preserved source child rollout"),
            source_child_original
        );

        fs::remove_dir_all(codex_dir).expect("remove test directory");
    }

    #[test]
    fn rollout_only_subagent_source_is_excluded_from_provider_sync() {
        let codex_dir = temp_codex_dir("rollout-only-subagent");
        write_config(&codex_dir, SHARED_SESSION_PROVIDER);
        let parent_id = "019f6000-0000-7000-8000-000000000573";
        let child_id = "019f6000-0000-7000-8000-000000000574";
        create_thread_database(
            &codex_dir.join("state_5.sqlite"),
            parent_id,
            SHARED_SESSION_PROVIDER,
        );
        write_rollout(&codex_dir, parent_id, SHARED_SESSION_PROVIDER);
        let child_rollout = write_subagent_rollout(&codex_dir, child_id, parent_id, "openai");
        let child_original = fs::read(&child_rollout).expect("read rollout-only subagent");
        let catalog = codex_dir.join("sqlite/codex-dev.db");
        create_catalog_database(
            &catalog,
            &[(parent_id, SHARED_SESSION_PROVIDER), (child_id, "openai")],
        );

        let status = session_sync_status_inner(Some(codex_dir.display().to_string()), None)
            .expect("scan rollout-only subagent");
        assert!(status.scan_complete, "{:?}", status.scan_failures);
        assert!(!status.needs_sync);
        assert_eq!(status.mismatched_threads, 0);
        assert_eq!(status.mismatched_rollouts, 0);
        assert_eq!(status.mismatched_session_meta, 0);
        assert_eq!(status.mismatched_sessions, 0);

        let result = sync_sessions_provider_inner(Some(codex_dir.display().to_string()), None)
            .expect("rollout-only subagent is a no-op");
        assert_eq!(result.updated_rollouts, 0);
        assert_eq!(result.updated_threads, 0);
        assert!(result.backup_dir.is_empty());
        assert_eq!(catalog_provider(&catalog, child_id), "openai");
        assert_eq!(
            fs::read(child_rollout).expect("read preserved rollout-only subagent"),
            child_original
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
