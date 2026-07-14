use crate::error::{CodexxError, Result};
use crate::file_io::{
    atomic_write, io_err, json_err, parse_toml_document, read_to_string_if_exists, write_json,
    write_text,
};
use crate::paths::home_dir;
use crate::platform;
use crate::sqlite_utils::{sql_select_column, sqlite_has_table, table_column_set};
use crate::{config_path, resolve_codex_dir, string_value};
use chrono::Local;
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};
use toml_edit::DocumentMut;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionDeleteInput {
    pub(crate) config_dir: Option<String>,
    pub(crate) session_ids: Vec<String>,
}

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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionDeleteResult {
    pub(crate) status: SessionSyncStatus,
    pub(crate) requested_sessions: usize,
    pub(crate) deleted_sessions: usize,
    pub(crate) failed_sessions: usize,
    pub(crate) failure_message: Option<String>,
    pub(crate) deleted_thread_rows: usize,
    pub(crate) deleted_rollout_files: usize,
    pub(crate) deleted_related_rows: usize,
}

#[derive(Debug, Default)]
pub(crate) struct RolloutScan {
    pub(crate) rollout_files: usize,
    pub(crate) session_meta_count: usize,
    pub(crate) mismatched_rollouts: usize,
    pub(crate) mismatched_session_meta: usize,
    pub(crate) changes: Vec<SessionFileChange>,
    pub(crate) thread_ids_with_user_events: HashSet<String>,
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

fn current_model_provider(codex_dir: &Path, explicit: Option<String>) -> Result<String> {
    if let Some(provider) = explicit
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Ok(provider);
    }
    let cfg = config_path(codex_dir);
    let text = read_to_string_if_exists(&cfg)?;
    let doc = parse_toml_document(&cfg, &text)?;
    Ok(string_value(&doc, "model_provider").unwrap_or_else(|| "openai".to_string()))
}

fn is_rollout_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
}

fn collect_rollout_paths(root: &Path, out: &mut Vec<PathBuf>, warnings: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(root) else {
        if root.exists() {
            warnings.push(format!("无法读取目录: {}", root.display()));
        }
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_rollout_paths(&path, out, warnings);
        } else if file_type.is_file() && is_rollout_file(&path) {
            out.push(path);
        }
    }
}

fn split_line_ending(segment: &str) -> (&str, &str) {
    if let Some(line) = segment.strip_suffix("\r\n") {
        (line, "\r\n")
    } else if let Some(line) = segment.strip_suffix('\n') {
        (line, "\n")
    } else {
        (segment, "")
    }
}

fn normalize_workspace_path(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with(r"\\?\unc\") {
        return Some(format!(r"\\{}", trimmed[8..].replace('/', r"\")));
    }
    if trimmed.starts_with(r"\\?\") {
        return Some(trimmed[4..].replace('\\', "/"));
    }
    Some(trimmed.to_string())
}

fn is_locked_io_error(error: &std::io::Error) -> bool {
    matches!(error.kind(), std::io::ErrorKind::PermissionDenied)
        || matches!(error.raw_os_error(), Some(32 | 33))
}

pub(crate) fn scan_rollouts(codex_dir: &Path, target_provider: &str) -> Result<RolloutScan> {
    let mut paths = Vec::new();
    let mut scan = RolloutScan::default();
    for dir in ["sessions", "archived_sessions"] {
        collect_rollout_paths(&codex_dir.join(dir), &mut paths, &mut scan.warnings);
    }
    paths.sort();
    scan.rollout_files = paths.len();

    for path in paths {
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if is_locked_io_error(&error) => {
                scan.warnings
                    .push(format!("跳过被占用/无权限会话文件: {}", path.display()));
                continue;
            }
            Err(error) => return Err(io_err(&path, error)),
        };
        let mut next_text = String::with_capacity(text.len());
        let mut rewrite_needed = false;
        let mut file_session_meta_count = 0usize;
        let mut thread_id = None;
        let mut cwd = None;

        for segment in text.split_inclusive('\n') {
            let (line, line_ending) = split_line_ending(segment);
            let mut next_line = line.to_string();
            if !line.trim().is_empty() {
                if let Ok(mut record) = serde_json::from_str::<Value>(line) {
                    if record.get("type").and_then(Value::as_str) == Some("session_meta") {
                        if let Some(payload) =
                            record.get_mut("payload").and_then(Value::as_object_mut)
                        {
                            file_session_meta_count += 1;
                            scan.session_meta_count += 1;
                            if thread_id.is_none() {
                                thread_id = payload
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .map(ToString::to_string);
                            }
                            if cwd.is_none() {
                                cwd = payload
                                    .get("cwd")
                                    .and_then(Value::as_str)
                                    .and_then(normalize_workspace_path);
                            }
                            if payload.get("model_provider").and_then(Value::as_str)
                                != Some(target_provider)
                            {
                                payload.insert(
                                    "model_provider".to_string(),
                                    Value::String(target_provider.to_string()),
                                );
                                next_line = serde_json::to_string(&record)
                                    .map_err(|error| json_err(&path, error))?;
                                rewrite_needed = true;
                                scan.mismatched_session_meta += 1;
                            }
                        }
                    }
                }
            }
            next_text.push_str(&next_line);
            next_text.push_str(line_ending);
        }

        if file_session_meta_count == 0 {
            continue;
        }
        if let Some(thread_id) = thread_id {
            if text.contains("\"user_message\"") || text.contains("\"user_input\"") {
                scan.thread_ids_with_user_events.insert(thread_id.clone());
            }
            if let Some(cwd) = cwd {
                scan.cwd_by_thread_id.insert(thread_id, cwd);
            }
        }
        if rewrite_needed {
            scan.mismatched_rollouts += 1;
            scan.changes.push(SessionFileChange {
                original_mtime: fs::metadata(&path)
                    .and_then(|metadata| metadata.modified())
                    .ok(),
                path,
                original_text: text,
                next_text,
            });
        }
    }
    let projectless = projectless_thread_ids(&codex_dir.join(".codex-global-state.json"))?;
    scan.cwd_by_thread_id
        .retain(|thread_id, _| !projectless.contains(thread_id));
    Ok(scan)
}

fn restore_file_mtime(path: &Path, mtime: Option<SystemTime>) {
    let Some(mtime) = mtime else { return };
    let Ok(file) = fs::File::options().write(true).open(path) else {
        return;
    };
    let _ = file.set_times(std::fs::FileTimes::new().set_modified(mtime));
}

#[cfg(all(target_os = "macos", not(test)))]
fn rollout_file_is_open(path: &Path) -> bool {
    std::process::Command::new("/usr/sbin/lsof")
        .args(["-t", "--"])
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(any(not(target_os = "macos"), test))]
fn rollout_file_is_open(_path: &Path) -> bool {
    false
}

pub(crate) fn apply_session_changes(
    changes: &[SessionFileChange],
) -> Result<(Vec<SessionFileChange>, Vec<PathBuf>)> {
    let mut applied = Vec::new();
    let mut skipped = Vec::new();
    for change in changes {
        if rollout_file_is_open(&change.path) {
            skipped.push(change.path.clone());
            continue;
        }
        match fs::read_to_string(&change.path) {
            Ok(current) if current == change.original_text => {}
            Ok(_) => {
                skipped.push(change.path.clone());
                continue;
            }
            Err(error) if is_locked_io_error(&error) => {
                skipped.push(change.path.clone());
                continue;
            }
            Err(error) => {
                let original_error = io_err(&change.path, error);
                return match restore_session_changes(&applied) {
                    Ok(()) => Err(original_error),
                    Err(rollback_error) => Err(CodexxError::Config(format!(
                        "{original_error}；回滚失败：{rollback_error}"
                    ))),
                };
            }
        }
        match atomic_write(&change.path, change.next_text.as_bytes()) {
            Ok(()) => {
                restore_file_mtime(&change.path, change.original_mtime);
                applied.push(change.clone());
            }
            Err(error) => {
                return match restore_session_changes(&applied) {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(CodexxError::Config(format!(
                        "{error}；回滚失败：{rollback_error}"
                    ))),
                };
            }
        }
    }
    Ok((applied, skipped))
}

pub(crate) fn restore_session_changes(changes: &[SessionFileChange]) -> Result<()> {
    let mut failed = 0usize;
    for change in changes {
        if rollout_file_is_open(&change.path) {
            failed += 1;
            continue;
        }
        let unchanged =
            fs::read_to_string(&change.path).is_ok_and(|current| current == change.next_text);
        if !unchanged {
            failed += 1;
            continue;
        }
        if atomic_write(&change.path, change.original_text.as_bytes()).is_err() {
            failed += 1;
            continue;
        }
        restore_file_mtime(&change.path, change.original_mtime);
    }
    if failed > 0 {
        return Err(CodexxError::Config(format!(
            "有 {failed} 个会话文件无法安全回滚；文件正在使用或已发生变化。"
        )));
    }
    Ok(())
}

fn expand_sqlite_home(codex_dir: &Path, value: &str) -> PathBuf {
    let trimmed = value.trim();
    if trimmed == "~" {
        return home_dir().unwrap_or_else(|_| codex_dir.to_path_buf());
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|_| PathBuf::from(trimmed));
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        path
    } else {
        codex_dir.join(path)
    }
}

fn configured_sqlite_home(codex_dir: &Path) -> Option<PathBuf> {
    let config = config_path(codex_dir);
    let configured = fs::read_to_string(&config)
        .ok()
        .and_then(|text| text.parse::<DocumentMut>().ok())
        .and_then(|doc| string_value(&doc, "sqlite_home"));
    #[cfg(test)]
    let environment = None;
    #[cfg(not(test))]
    let environment = std::env::var("CODEX_SQLITE_HOME").ok();
    configured
        .or(environment)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| expand_sqlite_home(codex_dir, &value))
}

fn sqlite_storage_roots(codex_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(configured) = configured_sqlite_home(codex_dir) {
        roots.push(configured);
    }
    roots.push(codex_dir.to_path_buf());
    roots.push(codex_dir.join("sqlite"));
    let mut seen = HashSet::new();
    roots.retain(|path| seen.insert(path.to_string_lossy().to_string()));
    roots
}

fn is_codex_sqlite_storage_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    if name == "codex-dev.db" {
        return true;
    }
    let Some(stem) = name.strip_suffix(".sqlite") else {
        return false;
    };
    let Some((kind, version)) = stem.rsplit_once('_') else {
        return false;
    };
    !version.is_empty()
        && version.chars().all(|ch| ch.is_ascii_digit())
        && matches!(
            kind,
            "state" | "logs" | "memories" | "goals" | "thread_history"
        )
}

fn all_codex_sqlite_paths(codex_dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for root in sqlite_storage_roots(codex_dir) {
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        let mut root_paths = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && is_codex_sqlite_storage_file(path))
            .collect::<Vec<_>>();
        root_paths.sort();
        for path in root_paths {
            if seen.insert(path.to_string_lossy().to_string()) {
                paths.push(path);
            }
        }
    }
    paths
}

fn sqlite_state_version(path: &Path) -> Option<u64> {
    path.file_name()
        .and_then(|value| value.to_str())
        .and_then(|name| name.strip_prefix("state_"))
        .and_then(|value| value.strip_suffix(".sqlite"))
        .and_then(|value| value.parse::<u64>().ok())
}

pub(crate) fn sqlite_candidate_paths(codex_dir: &Path) -> Vec<PathBuf> {
    for root in sqlite_storage_roots(codex_dir) {
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        let mut candidates = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && sqlite_state_version(path).is_some())
            .collect::<Vec<_>>();
        candidates.sort_by(|a, b| {
            sqlite_state_version(b)
                .cmp(&sqlite_state_version(a))
                .then_with(|| b.file_name().cmp(&a.file_name()))
        });
        for path in candidates {
            let Ok(conn) = Connection::open_with_flags(
                &path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            ) else {
                continue;
            };
            if sqlite_has_table(&conn, "threads").unwrap_or(false) {
                return vec![path];
            }
        }
    }
    Vec::new()
}

/// Mirrors Codex++ provider sync: visit every current SQLite session database,
/// then the legacy root state database. This is intentionally separate from
/// `sqlite_candidate_paths`, which identifies the single active database used
/// by destructive session deletion verification.
pub(crate) fn sqlite_session_db_paths(codex_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(configured) = configured_sqlite_home(codex_dir) {
        roots.push(configured);
    }
    roots.push(codex_dir.join("sqlite"));

    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        let mut candidates = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .filter(|path| {
                matches!(
                    path.extension().and_then(|extension| extension.to_str()),
                    Some("db" | "sqlite" | "sqlite3")
                )
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|path| {
            (
                path.file_name()
                    .is_none_or(|name| name != std::ffi::OsStr::new("codex-dev.db")),
                path.file_name().map(|name| name.to_os_string()),
            )
        });
        for path in candidates {
            if !seen.insert(path.clone()) {
                continue;
            }
            let Ok(conn) = Connection::open_with_flags(
                &path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            ) else {
                continue;
            };
            let is_session_db = ["threads", "automation_runs", "inbox_items"]
                .iter()
                .any(|table| sqlite_has_table(&conn, table).unwrap_or(false));
            if is_session_db {
                paths.push(path);
            }
        }
    }

    let legacy = codex_dir.join("state_5.sqlite");
    if legacy.exists() && seen.insert(legacy.clone()) {
        paths.push(legacy);
    }
    paths
}

fn sqlite_subagent_thread_ids(
    conn: &Connection,
    thread_cols: &HashSet<String>,
) -> Result<HashSet<String>> {
    let mut edge_child_ids = HashSet::new();

    if sqlite_has_table(conn, "thread_spawn_edges")? {
        let edge_cols = table_column_set(conn, "thread_spawn_edges")?;
        if edge_cols.contains("child_thread_id") {
            let mut stmt = conn
                .prepare(
                    "SELECT DISTINCT e.child_thread_id
                     FROM thread_spawn_edges e
                     INNER JOIN threads t ON t.id = e.child_thread_id",
                )
                .map_err(|e| CodexxError::Database(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| CodexxError::Database(e.to_string()))?;
            for row in rows {
                edge_child_ids.insert(row.map_err(|e| CodexxError::Database(e.to_string()))?);
            }
        }
    }

    let mut ids = edge_child_ids;
    if thread_cols.contains("thread_source") || thread_cols.contains("source") {
        let thread_source_col = sql_select_column(thread_cols, "thread_source", "NULL");
        let source_col = sql_select_column(thread_cols, "source", "NULL");
        let query = format!("SELECT \"id\", {thread_source_col}, {source_col} FROM threads");
        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        for row in rows {
            let (id, thread_source, source) =
                row.map_err(|e| CodexxError::Database(e.to_string()))?;
            if let Some(thread_source) = thread_source
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                if thread_source.eq_ignore_ascii_case("subagent") {
                    ids.insert(id);
                } else {
                    ids.remove(&id);
                }
                continue;
            }

            if let Some(source) = source
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                let source_is_subagent = source.eq_ignore_ascii_case("subagent")
                    || serde_json::from_str::<Value>(source)
                        .ok()
                        .is_some_and(|value| {
                            value
                                .as_object()
                                .is_some_and(|object| object.contains_key("subagent"))
                        });
                if source_is_subagent {
                    ids.insert(id);
                } else {
                    ids.remove(&id);
                }
            }
        }
    }

    Ok(ids)
}

struct SqliteThreadIndexState<'a> {
    thread_id: &'a str,
    provider: Option<&'a str>,
    has_user_event: Option<i64>,
    cwd: Option<&'a str>,
    has_user_event_column: bool,
    cwd_column: bool,
}

fn sqlite_thread_needs_alignment(
    rollouts: &RolloutScan,
    target_provider: &str,
    state: &SqliteThreadIndexState<'_>,
) -> bool {
    if state.provider.map(str::trim).unwrap_or_default() != target_provider {
        return true;
    }
    if state.has_user_event_column
        && rollouts
            .thread_ids_with_user_events
            .contains(state.thread_id)
        && state.has_user_event.unwrap_or_default() != 1
    {
        return true;
    }
    if state.cwd_column {
        if let Some(expected_cwd) = rollouts.cwd_by_thread_id.get(state.thread_id) {
            if state.cwd.and_then(normalize_workspace_path).as_deref()
                != Some(expected_cwd.as_str())
            {
                return true;
            }
        }
    }
    false
}

pub(crate) fn scan_sqlite(
    codex_dir: &Path,
    rollouts: &RolloutScan,
    target_provider: &str,
) -> Result<SqliteScan> {
    let mut scan = SqliteScan::default();
    let mut thread_ids = HashSet::new();
    let mut subagent_ids = HashSet::new();
    let mut mismatched_ids = HashSet::new();
    for path in sqlite_session_db_paths(codex_dir) {
        let conn = match Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(conn) => conn,
            Err(e) => {
                scan.warnings
                    .push(format!("无法读取 SQLite: {} ({e})", path.display()));
                continue;
            }
        };
        if !sqlite_has_table(&conn, "threads")? {
            continue;
        }
        let cols = table_column_set(&conn, "threads")?;
        if !cols.contains("id") || !cols.contains("model_provider") {
            scan.warnings.push(format!(
                "SQLite threads 缺少 id 或 model_provider 字段: {}",
                path.display()
            ));
            continue;
        }
        scan.sqlite_dbs += 1;
        let has_user_event_col = sql_select_column(&cols, "has_user_event", "NULL");
        let cwd_col = sql_select_column(&cols, "cwd", "NULL");
        let query = format!(
            "SELECT \"id\", \"model_provider\", {has_user_event_col}, {cwd_col} FROM threads"
        );
        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        for row in rows {
            let (id, provider, has_user_event, cwd) =
                row.map_err(|e| CodexxError::Database(e.to_string()))?;
            thread_ids.insert(id.clone());
            if sqlite_thread_needs_alignment(
                rollouts,
                target_provider,
                &SqliteThreadIndexState {
                    thread_id: &id,
                    provider: provider.as_deref(),
                    has_user_event,
                    cwd: cwd.as_deref(),
                    has_user_event_column: cols.contains("has_user_event"),
                    cwd_column: cols.contains("cwd"),
                },
            ) {
                mismatched_ids.insert(id);
            }
        }
        subagent_ids.extend(sqlite_subagent_thread_ids(&conn, &cols)?);
    }
    subagent_ids.retain(|id| thread_ids.contains(id));
    scan.sqlite_threads = thread_ids.len();
    scan.subagent_threads = subagent_ids.len();
    scan.top_level_threads = thread_ids.len().saturating_sub(subagent_ids.len());
    scan.mismatched_threads = mismatched_ids.len();
    Ok(scan)
}

pub(crate) fn list_session_previews(
    codex_dir: &Path,
    rollouts: &RolloutScan,
    target_provider: &str,
    limit: usize,
) -> Result<(Vec<SessionPreview>, Vec<String>)> {
    let mut candidates = Vec::new();
    let mut warnings = Vec::new();

    for path in sqlite_session_db_paths(codex_dir) {
        let conn = match Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(conn) => conn,
            Err(e) => {
                warnings.push(format!("无法读取会话数据库: {} ({e})", path.display()));
                continue;
            }
        };
        if !sqlite_has_table(&conn, "threads")? {
            continue;
        }
        let cols = table_column_set(&conn, "threads")?;
        if !cols.contains("id") {
            continue;
        }
        let subagent_thread_ids = sqlite_subagent_thread_ids(&conn, &cols)?;

        let title_col = sql_select_column(&cols, "title", "NULL");
        let first_message_col = sql_select_column(&cols, "first_user_message", "NULL");
        let preview_col = sql_select_column(&cols, "preview", "NULL");
        let provider_col = sql_select_column(&cols, "model_provider", "NULL");
        let model_col = sql_select_column(&cols, "model", "NULL");
        let cwd_col = sql_select_column(&cols, "cwd", "NULL");
        let rollout_col = sql_select_column(&cols, "rollout_path", "NULL");
        let updated_ms_col = sql_select_column(&cols, "updated_at_ms", "NULL");
        let updated_col = sql_select_column(&cols, "updated_at", "NULL");
        let archived_col = sql_select_column(&cols, "archived", "0");
        let has_user_event_col = sql_select_column(&cols, "has_user_event", "0");
        let order_col = if cols.contains("recency_at_ms") {
            "\"recency_at_ms\""
        } else if cols.contains("updated_at_ms") {
            "\"updated_at_ms\""
        } else if cols.contains("updated_at") {
            "\"updated_at\""
        } else {
            "\"id\""
        };

        let query = format!(
            "SELECT \"id\", {title_col}, {first_message_col}, {preview_col}, {provider_col}, {model_col}, {cwd_col}, {rollout_col}, {updated_ms_col}, {updated_col}, {archived_col}, {has_user_event_col} FROM threads ORDER BY {order_col} DESC"
        );
        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let title: Option<String> = row.get(1)?;
                let first_message: Option<String> = row.get(2)?;
                let preview: Option<String> = row.get(3)?;
                let model_provider: Option<String> = row.get(4)?;
                let model: Option<String> = row.get(5)?;
                let cwd: Option<String> = row.get(6)?;
                let rollout_path: Option<String> = row.get(7)?;
                let updated_at_ms: Option<i64> = row.get(8)?;
                let updated_at: Option<i64> = row.get(9)?;
                let archived: i64 = row.get(10)?;
                let has_user_event: i64 = row.get(11)?;
                let clean_title = [title, first_message, preview]
                    .into_iter()
                    .flatten()
                    .map(|v| v.trim().to_string())
                    .find(|v| !v.is_empty())
                    .unwrap_or_else(|| format!("会话 {}", id.chars().take(8).collect::<String>()));
                let normalized_provider = model_provider
                    .as_ref()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty());
                let normalized_cwd = cwd.as_deref().and_then(normalize_workspace_path);
                let normalized_rollout_path = rollout_path
                    .as_ref()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty());
                let needs_sync = sqlite_thread_needs_alignment(
                    rollouts,
                    target_provider,
                    &SqliteThreadIndexState {
                        thread_id: &id,
                        provider: normalized_provider.as_deref(),
                        has_user_event: Some(has_user_event),
                        cwd: normalized_cwd.as_deref(),
                        has_user_event_column: cols.contains("has_user_event"),
                        cwd_column: cols.contains("cwd"),
                    },
                );
                let is_subagent = subagent_thread_ids.contains(&id);
                Ok(SessionPreview {
                    id,
                    title: clean_title,
                    model_provider: normalized_provider.clone(),
                    model: model.and_then(|v| {
                        let v = v.trim().to_string();
                        (!v.is_empty()).then_some(v)
                    }),
                    cwd: normalized_cwd,
                    rollout_path: normalized_rollout_path,
                    updated_at_ms: updated_at_ms.or_else(|| updated_at.map(|v| v * 1000)),
                    archived: archived != 0,
                    has_user_event: has_user_event != 0,
                    is_subagent,
                    needs_sync,
                })
            })
            .map_err(|e| CodexxError::Database(e.to_string()))?;

        for row in rows {
            let session = row.map_err(|e| CodexxError::Database(e.to_string()))?;
            candidates.push(session);
        }
    }

    candidates.sort_by(|a, b| {
        b.updated_at_ms
            .cmp(&a.updated_at_ms)
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut seen = HashSet::new();
    let sessions = candidates
        .into_iter()
        .filter(|session| seen.insert(session.id.clone()))
        .take(limit.max(1))
        .collect();
    Ok((sessions, warnings))
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

fn projectless_thread_ids(path: &Path) -> Result<HashSet<String>> {
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
        fs::write(path, &text).map_err(|error| io_err(path, error))?;
        if let Some(parent) = path.parent() {
            let backup = parent.join(".codex-global-state.json.bak");
            fs::write(&backup, text).map_err(|error| io_err(&backup, error))?;
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

struct SessionMaintenanceLock {
    file: fs::File,
}

impl Drop for SessionMaintenanceLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn acquire_session_maintenance_lock(codex_dir: &Path) -> Result<SessionMaintenanceLock> {
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

fn normalized_session_ids(values: Vec<String>) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    for value in values {
        let id = value.trim();
        let valid = !id.is_empty()
            && id.len() <= 128
            && id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'));
        if !valid {
            return Err(CodexxError::Config(format!("会话 ID 无效: {id}")));
        }
        if seen.insert(id.to_string()) {
            ids.push(id.to_string());
        }
    }
    if ids.is_empty() {
        return Err(CodexxError::Config("请选择至少一个会话".to_string()));
    }
    if ids.len() > 1000 {
        return Err(CodexxError::Config("单次最多删除 1000 个会话".to_string()));
    }
    Ok(ids)
}

fn collect_thread_spawn_edges(codex_dir: &Path) -> Result<Vec<(String, String)>> {
    let mut edges = HashSet::new();
    for path in sqlite_candidate_paths(codex_dir) {
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| CodexxError::Database(format!("读取会话关系失败 {}: {e}", path.display())))?;
        if !sqlite_has_table(&conn, "thread_spawn_edges")? {
            continue;
        }
        let cols = table_column_set(&conn, "thread_spawn_edges")?;
        if !cols.contains("parent_thread_id") || !cols.contains("child_thread_id") {
            continue;
        }
        let mut stmt = conn
            .prepare("SELECT parent_thread_id, child_thread_id FROM thread_spawn_edges")
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        for row in rows {
            edges.insert(row.map_err(|e| CodexxError::Database(e.to_string()))?);
        }
    }
    Ok(edges.into_iter().collect())
}

fn selected_session_roots(codex_dir: &Path, selected: &[String]) -> Result<Vec<String>> {
    let edges = collect_thread_spawn_edges(codex_dir)?;
    let selected_set = selected.iter().cloned().collect::<HashSet<_>>();
    let mut parents = HashMap::<String, Vec<String>>::new();
    for (parent, child) in edges {
        parents.entry(child).or_default().push(parent);
    }
    Ok(selected
        .iter()
        .filter(|id| {
            let mut pending = parents.get(*id).cloned().unwrap_or_default();
            let mut visited = HashSet::new();
            while let Some(parent) = pending.pop() {
                if !visited.insert(parent.clone()) {
                    continue;
                }
                if selected_set.contains(&parent) {
                    return false;
                }
                if let Some(next) = parents.get(&parent) {
                    pending.extend(next.iter().cloned());
                }
            }
            true
        })
        .cloned()
        .collect())
}

fn session_descendants_by_root(
    codex_dir: &Path,
    roots: &[String],
) -> Result<HashMap<String, HashSet<String>>> {
    let edges = collect_thread_spawn_edges(codex_dir)?;
    let mut children = HashMap::<String, Vec<String>>::new();
    for (parent, child) in edges {
        children.entry(parent).or_default().push(child);
    }
    let mut descendants = HashMap::new();
    for root in roots {
        let mut ids = HashSet::from([root.clone()]);
        let mut pending = vec![root.clone()];
        while let Some(parent) = pending.pop() {
            if let Some(next) = children.get(&parent) {
                for child in next {
                    if ids.insert(child.clone()) {
                        pending.push(child.clone());
                    }
                }
            }
        }
        descendants.insert(root.clone(), ids);
    }
    Ok(descendants)
}

pub(crate) fn active_session_ids_present(
    active_database_paths: &[PathBuf],
    session_ids: &HashSet<String>,
) -> Result<HashSet<String>> {
    if active_database_paths.is_empty() {
        return Err(CodexxError::Database(
            "验证删除结果失败，未找到删除前确认的活动会话库".to_string(),
        ));
    }
    let mut present = HashSet::new();
    for path in active_database_paths {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| {
            CodexxError::Database(format!(
                "验证删除结果失败，无法读取 {}: {error}",
                path.display()
            ))
        })?;
        if !sqlite_has_table(&conn, "threads")? {
            return Err(CodexxError::Database(format!(
                "验证删除结果失败，活动会话库缺少 threads 表: {}",
                path.display()
            )));
        }
        if !table_column_set(&conn, "threads")?.contains("id") {
            return Err(CodexxError::Database(format!(
                "验证删除结果失败，活动会话库 threads 表缺少 id 字段: {}",
                path.display()
            )));
        }
        let mut stmt = conn
            .prepare("SELECT 1 FROM threads WHERE id = ?1 LIMIT 1")
            .map_err(|error| CodexxError::Database(error.to_string()))?;
        for id in session_ids {
            if stmt
                .exists([id])
                .map_err(|error| CodexxError::Database(error.to_string()))?
            {
                present.insert(id.clone());
            }
        }
    }
    Ok(present)
}

#[derive(Default)]
struct ActiveSessionStorageSnapshot {
    all_ids: HashSet<String>,
    subagent_ids: HashSet<String>,
    mismatched_ids: HashSet<String>,
}

fn active_session_storage_snapshot(
    codex_dir: &Path,
    active_database_paths: &[PathBuf],
) -> Result<ActiveSessionStorageSnapshot> {
    let mut snapshot = ActiveSessionStorageSnapshot::default();
    let target_provider = current_model_provider(codex_dir, None)?;
    let rollouts = scan_rollouts(codex_dir, &target_provider)?;
    for path in active_database_paths {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| {
            CodexxError::Database(format!(
                "准备删除会话时无法读取活动会话库 {}: {error}",
                path.display()
            ))
        })?;
        if !sqlite_has_table(&conn, "threads")? {
            return Err(CodexxError::Database(format!(
                "活动会话库缺少 threads 表: {}",
                path.display()
            )));
        }
        let cols = table_column_set(&conn, "threads")?;
        if !cols.contains("id") {
            return Err(CodexxError::Database(format!(
                "活动会话库 threads 表缺少 id 字段: {}",
                path.display()
            )));
        }

        let mut ids_stmt = conn
            .prepare("SELECT id FROM threads")
            .map_err(|error| CodexxError::Database(error.to_string()))?;
        let ids = ids_stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| CodexxError::Database(error.to_string()))?;
        for id in ids {
            snapshot
                .all_ids
                .insert(id.map_err(|error| CodexxError::Database(error.to_string()))?);
        }
        snapshot
            .subagent_ids
            .extend(sqlite_subagent_thread_ids(&conn, &cols)?);

        if cols.contains("model_provider") {
            let has_user_event_col = sql_select_column(&cols, "has_user_event", "NULL");
            let cwd_col = sql_select_column(&cols, "cwd", "NULL");
            let query =
                format!("SELECT id, model_provider, {has_user_event_col}, {cwd_col} FROM threads");
            let mut mismatch_stmt = conn
                .prepare(&query)
                .map_err(|error| CodexxError::Database(error.to_string()))?;
            let mismatches = mismatch_stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })
                .map_err(|error| CodexxError::Database(error.to_string()))?;
            for row in mismatches {
                let (id, provider, has_user_event, cwd) =
                    row.map_err(|error| CodexxError::Database(error.to_string()))?;
                if sqlite_thread_needs_alignment(
                    &rollouts,
                    &target_provider,
                    &SqliteThreadIndexState {
                        thread_id: &id,
                        provider: provider.as_deref(),
                        has_user_event,
                        cwd: cwd.as_deref(),
                        has_user_event_column: cols.contains("has_user_event"),
                        cwd_column: cols.contains("cwd"),
                    },
                ) {
                    snapshot.mismatched_ids.insert(id);
                }
            }
        }
    }
    Ok(snapshot)
}

#[cfg(test)]
fn session_ids_with_descendants(codex_dir: &Path, roots: &[String]) -> Result<HashSet<String>> {
    Ok(session_descendants_by_root(codex_dir, roots)?
        .into_values()
        .flatten()
        .collect())
}

fn is_rollout_storage_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.starts_with("rollout-")
                && (name.ends_with(".jsonl") || name.ends_with(".jsonl.zst"))
        })
}

fn collect_rollout_storage_paths(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_rollout_storage_paths(&path, out);
        } else if file_type.is_file() && is_rollout_storage_file(&path) {
            out.push(path);
        }
    }
}

fn rollout_filename_matches_id(path: &Path, id: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.ends_with(&format!("-{id}.jsonl")) || name.ends_with(&format!("-{id}.jsonl.zst"))
        })
}

fn canonical_rollout_storage_roots(codex_dir: &Path) -> Vec<PathBuf> {
    [
        codex_dir.join("sessions"),
        codex_dir.join("archived_sessions"),
    ]
    .into_iter()
    .filter_map(|root| root.canonicalize().ok())
    .collect()
}

fn is_canonical_rollout_storage_path(codex_dir: &Path, path: &Path) -> bool {
    canonical_rollout_storage_roots(codex_dir)
        .iter()
        .any(|root| path.starts_with(root))
}

fn canonical_rollout_path(codex_dir: &Path, value: &str, id: &str) -> Result<Option<PathBuf>> {
    let raw = PathBuf::from(value.trim());
    let path = if raw.is_absolute() {
        raw
    } else {
        codex_dir.join(raw)
    };
    if !path.exists() {
        return Ok(None);
    }
    if !rollout_filename_matches_id(&path, id) {
        return Err(CodexxError::Config(format!(
            "会话文件名与 ID 不匹配，已拒绝删除: {}",
            path.display()
        )));
    }
    let canonical = path.canonicalize().map_err(|e| io_err(&path, e))?;
    if !is_canonical_rollout_storage_path(codex_dir, &canonical) {
        return Err(CodexxError::Config(format!(
            "会话文件超出 Codex 会话目录，已拒绝删除: {}",
            path.display()
        )));
    }
    Ok(Some(canonical))
}

fn selected_rollout_paths(
    codex_dir: &Path,
    session_ids: &HashSet<String>,
) -> Result<HashSet<PathBuf>> {
    let mut paths = HashSet::new();
    for db_path in all_codex_sqlite_paths(codex_dir) {
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| {
            CodexxError::Database(format!("读取 SQLite 失败 {}: {e}", db_path.display()))
        })?;
        if !sqlite_has_table(&conn, "threads")? {
            continue;
        }
        let cols = table_column_set(&conn, "threads")?;
        if !cols.contains("id") || !cols.contains("rollout_path") {
            continue;
        }
        let mut stmt = conn
            .prepare("SELECT rollout_path FROM threads WHERE id = ?1")
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        for id in session_ids {
            let rows = stmt
                .query_map([id], |row| row.get::<_, Option<String>>(0))
                .map_err(|e| CodexxError::Database(e.to_string()))?;
            for row in rows {
                if let Some(value) = row.map_err(|e| CodexxError::Database(e.to_string()))? {
                    if let Some(path) = canonical_rollout_path(codex_dir, &value, id)? {
                        paths.insert(path);
                    }
                }
            }
        }
    }

    let mut discovered = Vec::new();
    for root in [
        codex_dir.join("sessions"),
        codex_dir.join("archived_sessions"),
    ] {
        collect_rollout_storage_paths(&root, &mut discovered);
    }
    for path in discovered {
        if session_ids
            .iter()
            .any(|id| rollout_filename_matches_id(&path, id))
        {
            let canonical = path.canonicalize().map_err(|e| io_err(&path, e))?;
            if !is_canonical_rollout_storage_path(codex_dir, &canonical) {
                return Err(CodexxError::Config(format!(
                    "会话文件超出 Codex 会话目录，已拒绝删除: {}",
                    path.display()
                )));
            }
            paths.insert(canonical);
        }
    }
    Ok(paths)
}

fn remove_jsonl_session_entries(
    path: &Path,
    id_keys: &[&str],
    session_ids: &HashSet<String>,
) -> Result<usize> {
    if !path.exists() {
        return Ok(0);
    }
    let text = fs::read_to_string(path).map_err(|e| io_err(path, e))?;
    let (next, removed) = filter_jsonl_session_entries(&text, id_keys, session_ids);
    if removed > 0 {
        write_text(path, &next)?;
    }
    Ok(removed)
}

fn filter_jsonl_session_entries(
    text: &str,
    id_keys: &[&str],
    session_ids: &HashSet<String>,
) -> (String, usize) {
    let mut next = String::with_capacity(text.len());
    let mut removed = 0usize;
    for segment in text.split_inclusive('\n') {
        let (line, ending) = split_line_ending(segment);
        let matches = serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|value| {
                id_keys.iter().find_map(|key| {
                    value
                        .get(*key)
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
            })
            .is_some_and(|id| session_ids.contains(&id));
        if matches {
            removed += 1;
        } else {
            next.push_str(line);
            next.push_str(ending);
        }
    }
    (next, removed)
}

fn remove_session_index_entries(codex_dir: &Path, session_ids: &HashSet<String>) -> Result<usize> {
    remove_jsonl_session_entries(&codex_dir.join("session_index.jsonl"), &["id"], session_ids)
}

fn remove_session_history_entries(
    codex_dir: &Path,
    session_ids: &HashSet<String>,
) -> Result<usize> {
    let path = codex_dir.join("history.jsonl");
    if !path.exists() {
        return Ok(0);
    }
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| io_err(&path, e))?;
    file.try_lock().map_err(|error| {
        CodexxError::Config(format!(
            "历史记录正在被其他 Codex 进程使用，请关闭相关 Codex 窗口或 CLI 后重试: {error}"
        ))
    })?;
    let result = (|| -> Result<usize> {
        let mut text = String::new();
        file.read_to_string(&mut text)
            .map_err(|e| io_err(&path, e))?;
        let (next, removed) = filter_jsonl_session_entries(&text, &["session_id"], session_ids);
        if removed > 0 {
            file.set_len(0).map_err(|e| io_err(&path, e))?;
            file.seek(SeekFrom::Start(0))
                .map_err(|e| io_err(&path, e))?;
            file.write_all(next.as_bytes())
                .map_err(|e| io_err(&path, e))?;
            file.sync_all().map_err(|e| io_err(&path, e))?;
        }
        Ok(removed)
    })();
    let _ = file.unlock();
    result
}

fn remove_shell_snapshot_files(codex_dir: &Path, session_ids: &HashSet<String>) -> Result<usize> {
    let root = codex_dir.join("shell_snapshots");
    let Ok(entries) = fs::read_dir(&root) else {
        return Ok(0);
    };
    let mut removed = 0usize;
    for entry in entries {
        let entry = entry.map_err(|e| io_err(&root, e))?;
        let file_type = entry.file_type().map_err(|e| io_err(&entry.path(), e))?;
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let matches = session_ids.iter().any(|id| {
            name.strip_prefix(id)
                .is_some_and(|suffix| suffix.starts_with('.'))
        });
        if matches {
            fs::remove_file(&path).map_err(|e| io_err(&path, e))?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn delete_ids_from_table(
    tx: &rusqlite::Transaction<'_>,
    table: &str,
    column: &str,
    session_ids: &HashSet<String>,
) -> Result<usize> {
    if !sqlite_has_table(tx, table)? || !table_column_set(tx, table)?.contains(column) {
        return Ok(0);
    }
    let sql = format!("DELETE FROM \"{table}\" WHERE \"{column}\" = ?1");
    let mut deleted = 0usize;
    for id in session_ids {
        deleted += tx
            .execute(&sql, [id])
            .map_err(|e| CodexxError::Database(e.to_string()))?;
    }
    Ok(deleted)
}

fn purge_session_database_references(
    codex_dir: &Path,
    session_ids: &HashSet<String>,
) -> (usize, usize, Vec<String>) {
    let known_tables = [
        "threads",
        "thread_dynamic_tools",
        "thread_spawn_edges",
        "agent_job_items",
        "logs",
        "stage1_outputs",
        "thread_goals",
        "thread_turns",
        "thread_items",
        "thread_history_projection_state",
        "local_thread_catalog",
        "automation_runs",
        "inbox_items",
    ];
    let mut deleted_threads = 0usize;
    let mut deleted_related = 0usize;
    let mut errors = Vec::new();
    for path in all_codex_sqlite_paths(codex_dir) {
        let result = (|| -> Result<(usize, usize)> {
            let mut conn = Connection::open_with_flags(
                &path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|e| {
                CodexxError::Database(format!("打开 SQLite 失败 {}: {e}", path.display()))
            })?;
            conn.busy_timeout(Duration::from_secs(5))
                .map_err(|e| CodexxError::Database(e.to_string()))?;
            let mut relevant = false;
            for table in known_tables {
                if sqlite_has_table(&conn, table)? {
                    relevant = true;
                    break;
                }
            }
            if !relevant {
                return Ok((0, 0));
            }
            conn.execute_batch("PRAGMA secure_delete = ON; PRAGMA foreign_keys = ON;")
                .map_err(|e| CodexxError::Database(e.to_string()))?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| {
                    CodexxError::Database(format!("锁定 SQLite 失败 {}: {e}", path.display()))
                })?;
            let mut db_threads = 0usize;
            let mut db_related = 0usize;

            db_related +=
                delete_ids_from_table(&tx, "thread_dynamic_tools", "thread_id", session_ids)?;
            for (table, column) in [
                ("logs", "thread_id"),
                ("stage1_outputs", "thread_id"),
                ("thread_goals", "thread_id"),
                ("thread_turns", "thread_id"),
                ("thread_items", "thread_id"),
                ("thread_history_projection_state", "thread_id"),
                ("local_thread_catalog", "thread_id"),
                ("automation_runs", "thread_id"),
                ("inbox_items", "thread_id"),
            ] {
                db_related += delete_ids_from_table(&tx, table, column, session_ids)?;
            }

            if sqlite_has_table(&tx, "thread_spawn_edges")? {
                let cols = table_column_set(&tx, "thread_spawn_edges")?;
                if cols.contains("parent_thread_id") && cols.contains("child_thread_id") {
                    for id in session_ids {
                        db_related += tx
                            .execute(
                                "DELETE FROM thread_spawn_edges WHERE parent_thread_id = ?1 OR child_thread_id = ?1",
                                [id],
                            )
                            .map_err(|e| CodexxError::Database(e.to_string()))?;
                    }
                }
            }
            if sqlite_has_table(&tx, "agent_job_items")?
                && table_column_set(&tx, "agent_job_items")?.contains("assigned_thread_id")
            {
                for id in session_ids {
                    db_related += tx
                        .execute(
                            "UPDATE agent_job_items SET assigned_thread_id = NULL WHERE assigned_thread_id = ?1",
                            [id],
                        )
                        .map_err(|e| CodexxError::Database(e.to_string()))?;
                }
            }
            db_threads += delete_ids_from_table(&tx, "threads", "id", session_ids)?;
            tx.commit()
                .map_err(|e| CodexxError::Database(e.to_string()))?;
            let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
            Ok((db_threads, db_related))
        })();
        match result {
            Ok((db_threads, db_related)) => {
                deleted_threads += db_threads;
                deleted_related += db_related;
            }
            Err(error) => {
                errors.push(format!("SQLite 清理失败 {}: {error}", path.display()));
            }
        }
    }
    (deleted_threads, deleted_related, errors)
}

#[derive(Debug, Default)]
pub(crate) struct LocalSessionDeleteCounts {
    pub(crate) deleted_ids: HashSet<String>,
    pub(crate) deleted_thread_rows: usize,
    pub(crate) deleted_rollout_files: usize,
    pub(crate) deleted_related_rows: usize,
    pub(crate) errors: Vec<String>,
}

fn delete_exact_session_ids_locally(
    codex_dir: &Path,
    session_ids: HashSet<String>,
    rollout_paths: HashSet<PathBuf>,
) -> LocalSessionDeleteCounts {
    let mut deleted_files = 0usize;
    let mut deleted_related_rows = 0usize;
    let mut errors = Vec::new();
    for path in rollout_paths {
        if path.exists() {
            match fs::remove_file(&path) {
                Ok(()) => deleted_files += 1,
                Err(error) => errors.push(io_err(&path, error).to_string()),
            }
        }
    }
    for result in [
        remove_session_index_entries(codex_dir, &session_ids),
        remove_session_history_entries(codex_dir, &session_ids),
        remove_shell_snapshot_files(codex_dir, &session_ids),
    ] {
        match result {
            Ok(removed) => deleted_related_rows += removed,
            Err(error) => errors.push(error.to_string()),
        }
    }
    let (deleted_thread_rows, removed_database_rows, database_errors) =
        purge_session_database_references(codex_dir, &session_ids);
    deleted_related_rows += removed_database_rows;
    errors.extend(database_errors);
    LocalSessionDeleteCounts {
        deleted_ids: session_ids,
        deleted_thread_rows,
        deleted_rollout_files: deleted_files,
        deleted_related_rows,
        errors,
    }
}

#[cfg(test)]
pub(crate) fn hard_delete_sessions_locally(
    codex_dir: &Path,
    roots: &[String],
) -> Result<LocalSessionDeleteCounts> {
    let session_ids = session_ids_with_descendants(codex_dir, roots)?;
    let rollout_paths = selected_rollout_paths(codex_dir, &session_ids)?;
    Ok(delete_exact_session_ids_locally(
        codex_dir,
        session_ids,
        rollout_paths,
    ))
}

#[derive(Debug, Default)]
struct OfficialSessionDeleteOutcome {
    deleted_ids: HashSet<String>,
    completed_roots: HashSet<String>,
    failed_roots: Vec<(String, String)>,
}

#[derive(Debug)]
enum AppServerDeleteAttempt {
    Success(OfficialSessionDeleteOutcome),
    Unsupported(String),
}

fn send_app_server_message(
    stdin: &mut impl Write,
    value: &Value,
) -> std::result::Result<(), String> {
    let mut line = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    line.push(b'\n');
    stdin.write_all(&line).map_err(|error| error.to_string())?;
    stdin.flush().map_err(|error| error.to_string())
}

fn recv_app_server_message(
    receiver: &mpsc::Receiver<std::io::Result<String>>,
    deadline: Instant,
) -> std::result::Result<Value, String> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| "Codex App Server 响应超时".to_string())?;
        let line = receiver
            .recv_timeout(remaining)
            .map_err(|error| format!("Codex App Server 响应失败: {error}"))?
            .map_err(|error| format!("读取 Codex App Server 输出失败: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }
        return serde_json::from_str(&line)
            .map_err(|error| format!("解析 Codex App Server 输出失败: {error}"));
    }
}

fn app_server_error(value: &Value) -> Option<(i64, String)> {
    let error = value.get("error")?;
    Some((
        error
            .get("code")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Codex App Server 返回未知错误")
            .to_string(),
    ))
}

fn stop_app_server_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn run_app_server_delete_attempt(
    mut child: Child,
    codex_dir: &Path,
    roots: &[String],
) -> AppServerDeleteAttempt {
    let Some(mut stdin) = child.stdin.take() else {
        stop_app_server_child(&mut child);
        return AppServerDeleteAttempt::Unsupported("Codex App Server stdin 不可用".to_string());
    };
    let Some(stdout) = child.stdout.take() else {
        stop_app_server_child(&mut child);
        return AppServerDeleteAttempt::Unsupported("Codex App Server stdout 不可用".to_string());
    };
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let result = (|| -> AppServerDeleteAttempt {
        let initialize_id = 1i64;
        let initialize = json!({
            "id": initialize_id,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "codex_x",
                    "title": "Codex-X",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": null
            }
        });
        if let Err(error) = send_app_server_message(&mut stdin, &initialize) {
            return AppServerDeleteAttempt::Unsupported(error);
        }
        let initialize_deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let message = match recv_app_server_message(&receiver, initialize_deadline) {
                Ok(message) => message,
                Err(error) => return AppServerDeleteAttempt::Unsupported(error),
            };
            if message.get("id").and_then(Value::as_i64) != Some(initialize_id) {
                continue;
            }
            if let Some((_, message)) = app_server_error(&message) {
                return AppServerDeleteAttempt::Unsupported(message);
            }
            let Some(server_home) = message
                .get("result")
                .and_then(|value| value.get("codexHome"))
                .and_then(Value::as_str)
            else {
                return AppServerDeleteAttempt::Unsupported(
                    "Codex App Server 未返回 CODEX_HOME".to_string(),
                );
            };
            let requested_home = codex_dir
                .canonicalize()
                .unwrap_or_else(|_| codex_dir.to_path_buf());
            let returned_home = PathBuf::from(server_home)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(server_home));
            if requested_home != returned_home {
                return AppServerDeleteAttempt::Unsupported(format!(
                    "Codex App Server 使用了不同的 CODEX_HOME: {}",
                    returned_home.display()
                ));
            }
            break;
        }
        if let Err(error) = send_app_server_message(&mut stdin, &json!({"method": "initialized"})) {
            return AppServerDeleteAttempt::Unsupported(error);
        }

        let mut outcome = OfficialSessionDeleteOutcome::default();
        'delete_roots: for (index, root) in roots.iter().enumerate() {
            let request_id = 1000 + index as i64;
            if let Err(error) = send_app_server_message(
                &mut stdin,
                &json!({
                    "id": request_id,
                    "method": "thread/delete",
                    "params": { "threadId": root }
                }),
            ) {
                for pending in &roots[index..] {
                    outcome.failed_roots.push((pending.clone(), error.clone()));
                }
                break 'delete_roots;
            }
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                let message = match recv_app_server_message(&receiver, deadline) {
                    Ok(message) => message,
                    Err(error) => {
                        for pending in &roots[index..] {
                            outcome.failed_roots.push((pending.clone(), error.clone()));
                        }
                        break 'delete_roots;
                    }
                };
                if message.get("method").and_then(Value::as_str) == Some("thread/deleted") {
                    if let Some(id) = message
                        .get("params")
                        .and_then(|value| value.get("threadId"))
                        .and_then(Value::as_str)
                    {
                        outcome.deleted_ids.insert(id.to_string());
                    }
                }
                if message.get("id").and_then(Value::as_i64) == Some(request_id) {
                    if let Some((code, error)) = app_server_error(&message) {
                        let lower = error.to_ascii_lowercase();
                        if code == -32601 {
                            return AppServerDeleteAttempt::Unsupported(error);
                        }
                        if code == -32600
                            && (lower.contains("no rollout")
                                || lower.contains("not found")
                                || lower.contains("does not exist"))
                        {
                            outcome.completed_roots.insert(root.clone());
                            break;
                        }
                        outcome.failed_roots.push((root.clone(), error));
                        break;
                    }
                    outcome.completed_roots.insert(root.clone());
                    break;
                }
            }
        }
        AppServerDeleteAttempt::Success(outcome)
    })();

    drop(stdin);
    stop_app_server_child(&mut child);
    let _ = reader.join();
    result
}

fn delete_sessions_via_codex_app_server(
    codex_dir: &Path,
    roots: &[String],
) -> Result<Option<OfficialSessionDeleteOutcome>> {
    let mut unsupported_messages = Vec::new();
    for program in platform::codex_executable_candidates() {
        let is_bare_command = program.components().count() == 1;
        if !is_bare_command && !program.is_file() {
            continue;
        }
        let mut command = platform::program_command(&program, &["app-server", "--stdio"]);
        command
            .env("CODEX_HOME", codex_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                unsupported_messages.push(format!("{}: {error}", program.display()));
                continue;
            }
        };
        match run_app_server_delete_attempt(child, codex_dir, roots) {
            AppServerDeleteAttempt::Success(outcome) => return Ok(Some(outcome)),
            AppServerDeleteAttempt::Unsupported(message) => {
                unsupported_messages.push(format!("{}: {message}", program.display()));
            }
        }
    }
    let _ = unsupported_messages;
    Ok(None)
}

fn merge_delete_counts(target: &mut LocalSessionDeleteCounts, source: LocalSessionDeleteCounts) {
    target.deleted_ids.extend(source.deleted_ids);
    target.deleted_thread_rows += source.deleted_thread_rows;
    target.deleted_rollout_files += source.deleted_rollout_files;
    target.deleted_related_rows += source.deleted_related_rows;
    target.errors.extend(source.errors);
}

pub(crate) fn delete_codex_sessions_inner(
    input: SessionDeleteInput,
) -> Result<SessionDeleteResult> {
    let selected = normalized_session_ids(input.session_ids)?;
    let requested_sessions = selected.len();
    let codex_dir = resolve_codex_dir(input.config_dir)?;
    let _maintenance_lock = acquire_session_maintenance_lock(&codex_dir)?;
    let roots = selected_session_roots(&codex_dir, &selected)?;
    let expected_by_root = session_descendants_by_root(&codex_dir, &roots)?;
    let expected_ids = expected_by_root
        .values()
        .flatten()
        .cloned()
        .collect::<HashSet<_>>();
    let active_database_paths = sqlite_candidate_paths(&codex_dir);
    if active_database_paths.is_empty() {
        return Err(CodexxError::Database(
            "无法确认当前活动会话库，已取消永久删除".to_string(),
        ));
    }
    let verification_ids = expected_ids.clone();
    let status_before = session_sync_status_inner(Some(codex_dir.display().to_string()), None)?;
    let storage_before = active_session_storage_snapshot(&codex_dir, &active_database_paths)?;
    // Validate and collect every filesystem target before the official API can
    // make the deletion irreversible.
    let expected_rollout_paths = selected_rollout_paths(&codex_dir, &expected_ids)?;
    let mut counts = LocalSessionDeleteCounts::default();
    let mut failed_roots = Vec::new();

    match delete_sessions_via_codex_app_server(&codex_dir, &roots)? {
        Some(outcome) => {
            let mut cleanup_ids = outcome.deleted_ids;
            for root in outcome.completed_roots {
                if let Some(ids) = expected_by_root.get(&root) {
                    cleanup_ids.extend(ids.iter().cloned());
                }
            }
            failed_roots = outcome.failed_roots;
            if !cleanup_ids.is_empty() {
                let cleanup_rollout_paths = expected_rollout_paths
                    .into_iter()
                    .filter(|path| {
                        cleanup_ids
                            .iter()
                            .any(|id| rollout_filename_matches_id(path, id))
                    })
                    .collect();
                merge_delete_counts(
                    &mut counts,
                    delete_exact_session_ids_locally(
                        &codex_dir,
                        cleanup_ids,
                        cleanup_rollout_paths,
                    ),
                );
            }
        }
        None => {
            merge_delete_counts(
                &mut counts,
                delete_exact_session_ids_locally(&codex_dir, expected_ids, expected_rollout_paths),
            );
        }
    }

    let remaining_ids = match active_session_ids_present(&active_database_paths, &verification_ids)
    {
        Ok(remaining) => remaining,
        Err(error) => {
            counts.errors.push(error.to_string());
            verification_ids.clone()
        }
    };
    counts.deleted_ids = verification_ids
        .difference(&remaining_ids)
        .cloned()
        .collect();
    let failed_selected = selected
        .iter()
        .filter(|id| remaining_ids.contains(*id))
        .count();
    let status = match session_sync_status_inner(Some(codex_dir.display().to_string()), None) {
        Ok(status) => status,
        Err(error) => {
            let message = format!("删除后刷新会话状态失败: {error}");
            counts.errors.push(message.clone());
            let mut fallback = status_before;
            let deleted_active_ids = counts
                .deleted_ids
                .intersection(&storage_before.all_ids)
                .cloned()
                .collect::<HashSet<_>>();
            let deleted_mismatched = deleted_active_ids
                .intersection(&storage_before.mismatched_ids)
                .count();
            let deleted_subagents = deleted_active_ids
                .intersection(&storage_before.subagent_ids)
                .count();
            let deleted_top_level = deleted_active_ids.len().saturating_sub(deleted_subagents);
            fallback
                .sessions
                .retain(|item| !counts.deleted_ids.contains(&item.id));
            fallback.sqlite_threads = fallback
                .sqlite_threads
                .saturating_sub(deleted_active_ids.len());
            fallback.top_level_threads =
                fallback.top_level_threads.saturating_sub(deleted_top_level);
            fallback.subagent_threads = fallback.subagent_threads.saturating_sub(deleted_subagents);
            fallback.mismatched_threads = fallback
                .mismatched_threads
                .saturating_sub(deleted_mismatched);
            fallback.needs_sync = fallback.mismatched_threads > 0;
            fallback.warnings.push(message);
            fallback
        }
    };
    let failed_sessions = failed_roots.len().max(failed_selected);
    let mut failure_parts = Vec::new();
    if let Some((_, message)) = failed_roots.first() {
        failure_parts.push(format!(
            "{} 个会话未能删除；首个错误: {message}",
            failed_sessions
        ));
    }
    if let Some(message) = counts.errors.first() {
        let prefix = if counts.deleted_ids.is_empty() {
            "本地清理未完成"
        } else {
            "会话删除已执行，但本地残留清理未完成"
        };
        failure_parts.push(format!(
            "{prefix}（{} 项）；首个错误: {message}",
            counts.errors.len()
        ));
    }
    let failure_message = (!failure_parts.is_empty()).then(|| failure_parts.join("；"));
    Ok(SessionDeleteResult {
        status,
        requested_sessions,
        deleted_sessions: counts.deleted_ids.len(),
        failed_sessions,
        failure_message,
        deleted_thread_rows: counts.deleted_thread_rows,
        deleted_rollout_files: counts.deleted_rollout_files,
        deleted_related_rows: counts.deleted_related_rows,
    })
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
