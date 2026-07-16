use super::global_state::{normalize_workspace_path, projectless_thread_ids};
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
use std::io::Read;
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

#[derive(Debug)]
struct SqliteStorageRoot {
    path: PathBuf,
    active_priority: usize,
    session_priority: usize,
    allow_custom_names: bool,
}

fn sqlite_storage_roots(codex_dir: &Path) -> Vec<SqliteStorageRoot> {
    let mut roots = Vec::new();
    if let Some(configured) = configured_sqlite_home(codex_dir) {
        roots.push(SqliteStorageRoot {
            path: configured,
            active_priority: 0,
            session_priority: 0,
            allow_custom_names: true,
        });
    }
    roots.push(SqliteStorageRoot {
        path: codex_dir.to_path_buf(),
        active_priority: 1,
        session_priority: 2,
        allow_custom_names: false,
    });
    roots.push(SqliteStorageRoot {
        path: codex_dir.join("sqlite"),
        active_priority: 2,
        session_priority: 1,
        allow_custom_names: true,
    });

    let mut seen = HashSet::new();
    roots.retain(|root| seen.insert(root.path.clone()));
    roots
}

const SESSION_TABLES: &[&str] = &["threads", "automation_runs", "inbox_items"];
const RELATED_TABLES: &[&str] = &[
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

#[derive(Debug, Clone, Default)]
pub(super) struct SqliteDiscovery {
    pub(super) active_paths: Vec<PathBuf>,
    pub(super) thread_paths: Vec<PathBuf>,
    pub(super) session_paths: Vec<PathBuf>,
    pub(super) related_paths: Vec<PathBuf>,
    pub(super) unreadable_paths: Vec<PathBuf>,
}

impl SqliteDiscovery {
    pub(super) fn active_first_thread_paths(&self) -> Vec<PathBuf> {
        primary_paths_first(&self.active_paths, &self.thread_paths)
    }

    pub(super) fn active_first_session_paths(&self) -> Vec<PathBuf> {
        primary_paths_first(&self.active_paths, &self.session_paths)
    }
}

#[derive(Debug)]
struct DiscoveredSqlite {
    path: PathBuf,
    active_priority: usize,
    session_priority: usize,
    state_version: Option<u64>,
    has_threads: bool,
    has_session_tables: bool,
    has_related_tables: bool,
}

fn is_sqlite_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "db" | "sqlite" | "sqlite3"
            )
        })
}

fn is_root_codex_sqlite_file(path: &Path) -> bool {
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

fn sqlite_state_version(path: &Path) -> Option<u64> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .and_then(|stem| stem.strip_prefix("state_"))
        .and_then(|value| value.parse::<u64>().ok())
}

fn sqlite_table_names(path: &Path) -> Option<HashSet<String>> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .ok()?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0)).ok()?;
    let mut tables = HashSet::new();
    for row in rows {
        tables.insert(row.ok()?);
    }
    Some(tables)
}

fn has_sqlite_header(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut header = [0u8; 16];
    file.read_exact(&mut header).is_ok() && &header == b"SQLite format 3\0"
}

fn primary_paths_first(primary: &[PathBuf], paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut ordered = Vec::with_capacity(paths.len());
    let mut seen = HashSet::new();
    for path in primary.iter().chain(paths) {
        if seen.insert(path.clone()) {
            ordered.push(path.clone());
        }
    }
    ordered
}

pub(super) fn ensure_sqlite_discovery_writable(discovery: &SqliteDiscovery) -> Result<()> {
    if discovery.unreadable_paths.is_empty() {
        Ok(())
    } else {
        Err(CodexxError::Config(
            "无法读取会话数据库，请关闭 Codex 后重试。".to_string(),
        ))
    }
}

fn ordered_database_paths(
    databases: &[DiscoveredSqlite],
    include: impl Fn(&DiscoveredSqlite) -> bool,
    priority: impl Fn(&DiscoveredSqlite) -> usize,
) -> Vec<PathBuf> {
    let mut matches = databases
        .iter()
        .filter(|database| include(database))
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        priority(left)
            .cmp(&priority(right))
            .then_with(|| {
                left.path
                    .file_name()
                    .is_none_or(|name| name != std::ffi::OsStr::new("codex-dev.db"))
                    .cmp(
                        &right
                            .path
                            .file_name()
                            .is_none_or(|name| name != std::ffi::OsStr::new("codex-dev.db")),
                    )
            })
            .then_with(|| left.path.file_name().cmp(&right.path.file_name()))
    });
    matches
        .into_iter()
        .map(|database| database.path.clone())
        .collect()
}

pub(super) fn discover_sqlite_databases(codex_dir: &Path) -> SqliteDiscovery {
    let mut databases = Vec::new();
    let mut seen_paths = HashSet::new();
    let mut unreadable_paths = Vec::new();

    for root in sqlite_storage_roots(codex_dir) {
        let Ok(entries) = fs::read_dir(&root.path) else {
            continue;
        };
        let mut paths = entries
            .flatten()
            .filter_map(|entry| {
                let file_type = entry.file_type().ok()?;
                (file_type.is_file() && !file_type.is_symlink()).then(|| entry.path())
            })
            .filter(|path| is_sqlite_file(path))
            .filter(|path| root.allow_custom_names || is_root_codex_sqlite_file(path))
            .collect::<Vec<_>>();
        paths.sort();

        for path in paths {
            if !seen_paths.insert(path.clone()) {
                continue;
            }
            let codex_named = is_root_codex_sqlite_file(&path);
            let Some(tables) = sqlite_table_names(&path) else {
                if has_sqlite_header(&path) {
                    unreadable_paths.push(path);
                }
                continue;
            };
            let has_threads = tables.contains("threads");
            let has_session_tables = SESSION_TABLES.iter().any(|table| tables.contains(*table));
            let has_related_tables = RELATED_TABLES.iter().any(|table| tables.contains(*table))
                && (has_session_tables || codex_named);
            if !has_session_tables && !has_related_tables {
                continue;
            }
            databases.push(DiscoveredSqlite {
                state_version: sqlite_state_version(&path),
                path,
                active_priority: root.active_priority,
                session_priority: root.session_priority,
                has_threads,
                has_session_tables,
                has_related_tables,
            });
        }
    }

    let active_path = databases
        .iter()
        .filter(|database| database.has_threads && database.state_version.is_some())
        .min_by(|left, right| {
            left.active_priority
                .cmp(&right.active_priority)
                .then_with(|| right.state_version.cmp(&left.state_version))
                .then_with(|| right.path.file_name().cmp(&left.path.file_name()))
        })
        .map(|database| database.path.clone());

    SqliteDiscovery {
        active_paths: active_path.into_iter().collect(),
        thread_paths: ordered_database_paths(
            &databases,
            |database| database.has_threads,
            |database| database.session_priority,
        ),
        session_paths: ordered_database_paths(
            &databases,
            |database| database.has_session_tables,
            |database| database.session_priority,
        ),
        related_paths: ordered_database_paths(
            &databases,
            |database| database.has_related_tables,
            |database| database.active_priority,
        ),
        unreadable_paths,
    }
}

pub(crate) fn sqlite_candidate_paths(codex_dir: &Path) -> Vec<PathBuf> {
    discover_sqlite_databases(codex_dir).active_paths
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn sqlite_session_db_paths(codex_dir: &Path) -> Vec<PathBuf> {
    discover_sqlite_databases(codex_dir).session_paths
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
    pub(super) cwd: Option<&'a str>,
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

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn scan_sqlite(
    codex_dir: &Path,
    rollouts: &RolloutScan,
    target_provider: &str,
) -> Result<SqliteScan> {
    let discovery = discover_sqlite_databases(codex_dir);
    scan_sqlite_with_paths(&discovery.session_paths, rollouts, target_provider)
}

pub(super) fn scan_sqlite_with_paths(
    sqlite_paths: &[PathBuf],
    rollouts: &RolloutScan,
    target_provider: &str,
) -> Result<SqliteScan> {
    let mut scan = SqliteScan::default();
    let mut thread_ids = HashSet::new();
    let mut subagent_ids = HashSet::new();
    let mut mismatched_ids = HashSet::new();
    for path in sqlite_paths {
        let conn = match Connection::open_with_flags(
            path,
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
        let cwd_col = sql_select_column(&cols, "cwd", "NULL");
        let query = format!("SELECT \"id\", \"model_provider\", {cwd_col} FROM threads");
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
            let (id, provider, cwd) = row.map_err(|e| CodexxError::Database(e.to_string()))?;
            thread_ids.insert(id.clone());
            if sqlite_thread_needs_alignment(
                rollouts,
                target_provider,
                &SqliteThreadIndexState {
                    thread_id: &id,
                    provider: provider.as_deref(),
                    cwd: cwd.as_deref(),
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

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn list_session_previews(
    codex_dir: &Path,
    rollouts: &RolloutScan,
    target_provider: &str,
    limit: usize,
) -> Result<(Vec<SessionPreview>, Vec<String>)> {
    let discovery = discover_sqlite_databases(codex_dir);
    list_session_previews_with_paths(
        &discovery.active_first_session_paths(),
        rollouts,
        target_provider,
        limit,
    )
}

pub(super) fn list_session_previews_with_paths(
    sqlite_paths: &[PathBuf],
    rollouts: &RolloutScan,
    target_provider: &str,
    limit: usize,
) -> Result<(Vec<SessionPreview>, Vec<String>)> {
    let mut candidates = Vec::new();
    let mut warnings = Vec::new();

    for path in sqlite_paths {
        let conn = match Connection::open_with_flags(
            path,
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
                        cwd: normalized_cwd.as_deref(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_codex_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "codex-x-storage-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create test codex dir");
        path
    }

    fn create_thread_database(path: &Path, id: &str, provider: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create sqlite parent");
        }
        let conn = Connection::open(path).expect("create thread database");
        conn.execute_batch(
            "CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                model_provider TEXT NOT NULL,
                title TEXT,
                updated_at_ms INTEGER
             );",
        )
        .expect("create threads table");
        conn.execute(
            "INSERT INTO threads (id, model_provider, title, updated_at_ms)
             VALUES (?1, ?2, 'test session', 1)",
            (id, provider),
        )
        .expect("insert thread");
    }

    #[test]
    fn root_state_10_is_listed_and_synchronized() {
        let codex_dir = temp_codex_dir("root-state-10");
        let database = codex_dir.join("state_10.sqlite");
        let id = "019f6000-0000-7000-8000-000000000301";
        create_thread_database(&database, id, "openai");

        let discovery = discover_sqlite_databases(&codex_dir);
        assert_eq!(discovery.active_paths, vec![database.clone()]);
        assert_eq!(discovery.thread_paths, vec![database.clone()]);
        let (sessions, warnings) = list_session_previews_with_paths(
            &discovery.session_paths,
            &RolloutScan::default(),
            "custom",
            50,
        )
        .expect("list state_10 session");
        assert!(warnings.is_empty());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, id);

        let result = crate::sessions::sync::sync_sessions_provider_inner(
            Some(codex_dir.display().to_string()),
            Some("custom".to_string()),
        )
        .expect("sync root state_10");
        assert_eq!(result.updated_threads, 1);
        let provider: String = Connection::open(&database)
            .expect("reopen state_10")
            .query_row(
                "SELECT model_provider FROM threads WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .expect("read updated provider");
        assert_eq!(provider, "custom");

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn custom_db_and_sqlite3_share_the_same_discovery() {
        let codex_dir = temp_codex_dir("custom-extensions");
        let custom_db = codex_dir.join("sqlite/custom.db");
        let custom_sqlite3 = codex_dir.join("sqlite/custom.sqlite3");
        create_thread_database(&custom_db, "019f6000-0000-7000-8000-000000000311", "openai");
        create_thread_database(
            &custom_sqlite3,
            "019f6000-0000-7000-8000-000000000312",
            "openai",
        );

        let discovery = discover_sqlite_databases(&codex_dir);
        let expected = HashSet::from([custom_db, custom_sqlite3]);
        assert_eq!(
            discovery.thread_paths.into_iter().collect::<HashSet<_>>(),
            expected
        );
        assert_eq!(
            discovery.session_paths.into_iter().collect::<HashSet<_>>(),
            expected
        );
        assert_eq!(
            discovery.related_paths.into_iter().collect::<HashSet<_>>(),
            expected
        );

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn invalid_state_database_is_ignored() {
        let codex_dir = temp_codex_dir("invalid-state");
        let invalid = codex_dir.join("state_5.sqlite");
        let valid = codex_dir.join("state_10.sqlite");
        fs::write(&invalid, b"not a sqlite database").expect("write invalid sqlite");
        create_thread_database(&valid, "019f6000-0000-7000-8000-000000000321", "openai");

        let discovery = discover_sqlite_databases(&codex_dir);
        assert_eq!(discovery.active_paths, vec![valid.clone()]);
        assert_eq!(discovery.thread_paths, vec![valid]);
        assert!(!discovery.session_paths.contains(&invalid));
        assert!(!discovery.related_paths.contains(&invalid));
        assert!(discovery.unreadable_paths.is_empty());

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn sqlite_header_with_unreadable_schema_blocks_mutation() {
        let codex_dir = temp_codex_dir("unreadable-schema");
        let unreadable = codex_dir.join("state_5.sqlite");
        fs::write(&unreadable, b"SQLite format 3\0").expect("write truncated sqlite");

        let discovery = discover_sqlite_databases(&codex_dir);
        assert_eq!(discovery.unreadable_paths, vec![unreadable]);
        let error = ensure_sqlite_discovery_writable(&discovery)
            .expect_err("unreadable sqlite must block mutation");
        assert_eq!(
            error.to_string(),
            "配置错误: 无法读取会话数据库，请关闭 Codex 后重试。"
        );

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn unrelated_root_sqlite_is_not_classified_as_codex_storage() {
        let codex_dir = temp_codex_dir("unrelated-root");
        let unrelated = codex_dir.join("unrelated.sqlite");
        let conn = Connection::open(&unrelated).expect("create unrelated sqlite");
        conn.execute("CREATE TABLE logs (thread_id TEXT)", [])
            .expect("create unrelated logs table");
        drop(conn);

        let discovery = discover_sqlite_databases(&codex_dir);
        assert!(!discovery.related_paths.contains(&unrelated));
        assert!(!discovery.session_paths.contains(&unrelated));
        assert!(!discovery.thread_paths.contains(&unrelated));

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn unrelated_custom_database_with_only_related_table_is_not_cleaned() {
        let codex_dir = temp_codex_dir("unrelated-custom");
        let unrelated = codex_dir.join("sqlite/unrelated.db");
        fs::create_dir_all(unrelated.parent().expect("sqlite parent"))
            .expect("create sqlite directory");
        let conn = Connection::open(&unrelated).expect("create unrelated custom sqlite");
        conn.execute("CREATE TABLE logs (thread_id TEXT)", [])
            .expect("create unrelated logs table");
        drop(conn);

        let discovery = discover_sqlite_databases(&codex_dir);
        assert!(!discovery.related_paths.contains(&unrelated));
        assert!(!discovery.session_paths.contains(&unrelated));

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn duplicate_preview_prefers_active_database_at_same_timestamp() {
        let codex_dir = temp_codex_dir("active-preview-priority");
        let active = codex_dir.join("state_10.sqlite");
        let legacy = codex_dir.join("sqlite/state_5.sqlite");
        let id = "019f6000-0000-7000-8000-000000000351";
        create_thread_database(&active, id, "openai");
        create_thread_database(&legacy, id, "openai");
        for (path, title) in [(&active, "active title"), (&legacy, "legacy title")] {
            Connection::open(path)
                .expect("open duplicate database")
                .execute(
                    "UPDATE threads SET title = ?1, updated_at_ms = 100 WHERE id = ?2",
                    (title, id),
                )
                .expect("update duplicate title");
        }

        let discovery = discover_sqlite_databases(&codex_dir);
        assert_eq!(discovery.session_paths, vec![legacy, active.clone()]);
        let previews = list_session_previews_with_paths(
            &discovery.active_first_session_paths(),
            &RolloutScan::default(),
            "openai",
            50,
        )
        .expect("list duplicate previews")
        .0;
        assert_eq!(previews.len(), 1);
        assert_eq!(previews[0].title, "active title");

        let _ = fs::remove_dir_all(codex_dir);
    }
}
