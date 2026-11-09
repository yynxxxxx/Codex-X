use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionPreview {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) first_user_message: Option<String>,
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
    #[serde(default)]
    pub(crate) directory_identities: HashMap<String, String>,
    #[serde(default)]
    pub(crate) pin_scope_key: Option<String>,
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
    pub(crate) mismatched_sessions: usize,
    pub(crate) needs_sync: bool,
    pub(crate) scan_complete: bool,
    pub(crate) read_only_reason: Option<String>,
    pub(crate) scan_failures: Vec<String>,
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
    pub(crate) discovered_rollout_files: usize,
    pub(crate) rollout_files: usize,
    pub(crate) session_meta_count: usize,
    pub(crate) mismatched_rollouts: usize,
    pub(crate) mismatched_session_meta: usize,
    pub(crate) changes: Vec<SessionFileChange>,
    pub(crate) provider_candidate_paths: HashSet<PathBuf>,
    /// SHA-256 of every completely validated candidate's original JSONL. Keep
    /// matched rollouts covered without retaining another copy of their text.
    pub(crate) verified_rollout_hashes: HashMap<PathBuf, [u8; 32]>,
    pub(crate) cwd_by_thread_id: HashMap<String, String>,
    pub(crate) thread_ids: HashSet<String>,
    /// Non-user threads identified from their own rollout metadata. Retained
    /// even when the rollout is excluded from provider synchronization.
    pub(crate) internal_thread_ids: HashSet<String>,
    pub(crate) mismatched_thread_ids: HashSet<String>,
    pub(crate) warnings: Vec<String>,
    pub(crate) scan_failures: Vec<String>,
    /// Incomplete user-rollout scans that must block Provider/index mutations,
    /// including unindexed files that would otherwise become only warnings.
    pub(crate) blocked_failures: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionFileChange {
    pub(crate) path: PathBuf,
    pub(crate) original_text: String,
    pub(crate) next_text: String,
    pub(crate) original_mtime: Option<SystemTime>,
    // Production snapshots live on disk; inline text is retained for small fixtures.
    pub(super) streamed: Option<Arc<super::rollout_stream::StreamedSnapshot>>,
}

impl SessionFileChange {
    pub(super) fn original_hash(&self) -> [u8; 32] {
        self.streamed.as_ref().map_or_else(
            || Sha256::digest(self.original_text.as_bytes()).into(),
            |snapshot| snapshot.original_hash,
        )
    }

    pub(super) fn next_hash(&self) -> [u8; 32] {
        self.streamed.as_ref().map_or_else(
            || Sha256::digest(self.next_text.as_bytes()).into(),
            |snapshot| snapshot.next_hash,
        )
    }
}

#[derive(Debug, Default)]
pub(crate) struct SqliteScan {
    pub(crate) sqlite_dbs: usize,
    pub(crate) sqlite_threads: usize,
    pub(crate) top_level_threads: usize,
    pub(crate) subagent_threads: usize,
    pub(crate) mismatched_threads: usize,
    pub(crate) thread_ids: HashSet<String>,
    pub(crate) syncable_thread_ids: HashSet<String>,
    pub(crate) archived_thread_ids: HashSet<String>,
    pub(crate) subagent_thread_ids: HashSet<String>,
    pub(crate) rollout_paths_by_thread_id: HashMap<String, String>,
    pub(crate) mismatched_thread_ids: HashSet<String>,
    pub(crate) warnings: Vec<String>,
    pub(crate) scan_failures: Vec<String>,
}
