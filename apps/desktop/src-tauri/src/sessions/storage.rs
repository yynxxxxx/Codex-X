use super::sync::projectless_thread_ids;
use super::types::{RolloutScan, SessionFileChange, SessionPreview, SqliteScan};
use crate::error::{CodexxError, Result};
use crate::file_io::{
    atomic_write, io_err, json_err, parse_toml_document, read_to_string_if_exists,
};
use crate::paths::home_dir;
use crate::sqlite_utils::{sql_select_column, sqlite_has_table, table_column_set};
use crate::{config_path, string_value};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use toml_edit::DocumentMut;

pub(super) fn current_model_provider(codex_dir: &Path, explicit: Option<String>) -> Result<String> {
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

pub(super) fn split_line_ending(segment: &str) -> (&str, &str) {
    if let Some(line) = segment.strip_suffix("\r\n") {
        (line, "\r\n")
    } else if let Some(line) = segment.strip_suffix('\n') {
        (line, "\n")
    } else {
        (segment, "")
    }
}

pub(super) fn normalize_workspace_path(value: &str) -> Option<String> {
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
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
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

pub(super) fn all_codex_sqlite_paths(codex_dir: &Path) -> Vec<PathBuf> {
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

pub(super) fn sqlite_subagent_thread_ids(
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

pub(super) struct SqliteThreadIndexState<'a> {
    pub(super) thread_id: &'a str,
    pub(super) provider: Option<&'a str>,
    pub(super) has_user_event: Option<i64>,
    pub(super) cwd: Option<&'a str>,
    pub(super) has_user_event_column: bool,
    pub(super) cwd_column: bool,
}

pub(super) fn sqlite_thread_needs_alignment(
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
