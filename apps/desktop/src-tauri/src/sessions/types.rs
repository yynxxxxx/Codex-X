use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionPreview {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) model_provider: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) rollout_path: Option<String>,
    pub(crate) updated_at_ms: Option<i64>,
    pub(crate) archived: bool,
    pub(crate) has_user_event: bool,
    pub(crate) is_subagent: bool,
    pub(crate) needs_sync: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionSyncStatus {
    pub(crate) codex_dir: String,
    pub(crate) target_provider: String,
    pub(crate) rollout_files: usize,
    pub(crate) session_meta_count: usize,
    pub(crate) mismatched_rollouts: usize,
    pub(crate) mismatched_session_meta: usize,
    pub(crate) sqlite_dbs: usize,
    pub(crate) sqlite_threads: usize,
    pub(crate) top_level_threads: usize,
    pub(crate) subagent_threads: usize,
    pub(crate) mismatched_threads: usize,
    pub(crate) needs_sync: bool,
    pub(crate) backup_dir: Option<String>,
    pub(crate) warnings: Vec<String>,
    pub(crate) sessions: Vec<SessionPreview>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionSyncResult {
    pub(crate) status: SessionSyncStatus,
    pub(crate) updated_rollouts: usize,
    pub(crate) updated_threads: usize,
    pub(crate) backup_dir: String,
}

#[derive(Debug, Default)]
pub(crate) struct RolloutScan {
    pub(crate) rollout_files: usize,
    pub(crate) session_meta_count: usize,
    pub(crate) mismatched_rollouts: usize,
    pub(crate) mismatched_session_meta: usize,
    pub(crate) changes: Vec<SessionFileChange>,
    pub(crate) cwd_by_thread_id: HashMap<String, String>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionFileChange {
    pub(crate) path: PathBuf,
    pub(crate) original_text: String,
    pub(crate) next_text: String,
    pub(crate) original_mtime: Option<SystemTime>,
}

#[derive(Debug, Default)]
pub(crate) struct SqliteScan {
    pub(crate) sqlite_dbs: usize,
    pub(crate) sqlite_threads: usize,
    pub(crate) top_level_threads: usize,
    pub(crate) subagent_threads: usize,
    pub(crate) mismatched_threads: usize,
    pub(crate) warnings: Vec<String>,
}
