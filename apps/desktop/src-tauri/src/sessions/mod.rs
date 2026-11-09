mod app_server;
mod backup;
mod catalog;
mod delete;
mod directory_identity;
mod export;
mod global_state;
mod names;
mod rollout_stream;
mod storage;
mod sync;
mod transaction;
mod types;
mod wsl;

#[cfg(test)]
pub(crate) use backup::{
    backup_sqlite_to_backup, provider_sync_backup_root, prune_provider_sync_backups,
};
#[cfg(test)]
pub(crate) use delete::{active_session_ids_present, hard_delete_sessions_locally};
pub(crate) use delete::{delete_codex_sessions_inner, SessionDeleteInput, SessionDeleteResult};
pub(crate) use export::{export_codex_sessions_inner, SessionExportResult};
#[cfg(test)]
pub(crate) use storage::{
    apply_session_changes, list_session_previews, restore_session_changes, scan_rollouts,
    scan_sqlite, sqlite_session_db_paths,
};
pub(crate) use storage::{
    session_project_title, session_titles_by_id, source_kind_is_internal, sqlite_candidate_paths,
    sqlite_candidate_paths_with_timeout, thread_source_is_internal, usage_thread_identities,
    UsageThreadIdentity,
};
pub(crate) use sync::{session_sync_status_inner, sync_sessions_provider_inner};
pub(crate) use types::{SessionSyncResult, SessionSyncStatus};

/// Serialize operations that can restart the official client with session
/// synchronization and deletion, including callers from other windows or IPC.
/// Provider activation remains independent of this maintenance lock.
pub(crate) fn with_session_maintenance<T>(
    codex_dir: &std::path::Path,
    operation: impl FnOnce() -> crate::error::Result<T>,
) -> crate::error::Result<T> {
    let _guard = sync::acquire_session_maintenance_lock(codex_dir)?;
    operation()
}

#[cfg(test)]
mod maintenance_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn maintenance_conflict_never_executes_callback_and_release_allows_retry() {
        let directory = tempfile::tempdir().unwrap();
        let held = sync::acquire_session_maintenance_lock(directory.path()).unwrap();
        let called = Cell::new(false);
        let error = with_session_maintenance(directory.path(), || {
            called.set(true);
            Ok(())
        })
        .expect_err("another session operation must block the callback");
        assert!(!called.get());
        assert!(error.to_string().contains("会话维护正在进行"));
        drop(held);
        let result = with_session_maintenance(directory.path(), || {
            called.set(true);
            Ok(17)
        })
        .unwrap();
        assert!(called.get());
        assert_eq!(result, 17);
        let _available = sync::acquire_session_maintenance_lock(directory.path()).unwrap();
    }

    #[test]
    fn maintenance_guard_releases_when_callback_returns_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let error = with_session_maintenance(directory.path(), || {
            Err::<(), _>(crate::error::CodexxError::Config(
                "fixture operation failed".into(),
            ))
        })
        .unwrap_err();
        assert!(error.to_string().contains("fixture operation failed"));
        let _available = sync::acquire_session_maintenance_lock(directory.path()).unwrap();
    }
}
