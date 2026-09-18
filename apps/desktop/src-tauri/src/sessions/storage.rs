use super::global_state::normalize_workspace_path;
use super::types::{RolloutScan, SessionFileChange, SessionPreview, SqliteScan};
use crate::error::{CodexxError, Result};
use crate::file_io::{
    atomic_write, io_err, json_err, parse_toml_document, read_to_string_if_exists,
};
use crate::paths::home_dir;
use crate::sqlite_utils::{sql_select_column, sqlite_has_table, table_column_set};
use crate::{config_path, string_value};
use rusqlite::{Connection, OpenFlags};
use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use toml_edit::DocumentMut;

const MAX_ROLLOUT_IN_MEMORY_BYTES: u64 = 32 * 1024 * 1024;

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

pub(super) fn rollout_filename_matches_id(path: &Path, id: &str) -> bool {
    if let Some(thread_id) = rollout_filename_thread_id(path) {
        return thread_id == id;
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.ends_with(&format!("-{id}.jsonl")) || name.ends_with(&format!("-{id}.jsonl.zst"))
        })
}

pub(super) fn canonical_rollout_storage_roots(codex_dir: &Path) -> Vec<PathBuf> {
    [
        codex_dir.join("sessions"),
        codex_dir.join("archived_sessions"),
    ]
    .into_iter()
    .filter_map(|root| root.canonicalize().ok())
    .collect()
}

pub(super) fn is_canonical_rollout_storage_path(codex_dir: &Path, path: &Path) -> bool {
    canonical_rollout_storage_roots(codex_dir)
        .iter()
        .any(|root| path.starts_with(root))
}

fn referenced_rollout_paths(
    codex_dir: &Path,
    rollout_paths_by_thread_id: &HashMap<String, String>,
    include_archived_storage: bool,
    failures: &mut Vec<String>,
) -> HashMap<PathBuf, HashSet<String>> {
    let mut referenced = HashMap::<PathBuf, HashSet<String>>::new();
    for (thread_id, value) in rollout_paths_by_thread_id {
        let raw = PathBuf::from(value.trim());
        let path = if raw.is_absolute() {
            raw
        } else {
            codex_dir.join(raw)
        };
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                failures.push(format!(
                    "活动 SQLite 引用的会话文件不存在: {}",
                    path.display()
                ));
                continue;
            }
            Err(error) => {
                failures.push(format!(
                    "无法检查活动会话文件: {} ({error})",
                    path.display()
                ));
                continue;
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() || !is_rollout_file(&path) {
            failures.push(format!(
                "活动 SQLite 引用了不受支持的会话文件: {}",
                path.display()
            ));
            continue;
        }
        let canonical = match path.canonicalize() {
            Ok(canonical) => canonical,
            Err(error) => {
                failures.push(format!(
                    "无法解析活动会话文件路径: {} ({error})",
                    path.display()
                ));
                continue;
            }
        };
        let within_codex_storage = is_canonical_rollout_storage_path(codex_dir, &canonical);
        let allowed_root = if include_archived_storage {
            within_codex_storage
        } else {
            within_codex_storage
                && codex_dir
                    .join("sessions")
                    .canonicalize()
                    .is_ok_and(|root| canonical.starts_with(root))
        };
        if !allowed_root {
            let reason = if within_codex_storage {
                "活动 SQLite 引用了归档会话文件"
            } else {
                "活动 SQLite 引用的会话文件超出 Codex 会话目录"
            };
            failures.push(format!("{reason}: {}", path.display()));
            continue;
        }
        let expected_thread_ids = referenced.entry(canonical.clone()).or_default();
        expected_thread_ids.insert(thread_id.clone());
        if expected_thread_ids.len() > 1 {
            failures.push(format!(
                "活动 SQLite 的多个线程引用了同一个会话文件: {}",
                canonical.display()
            ));
        }
    }
    referenced
}

fn collect_rollout_paths(root: &Path, out: &mut Vec<PathBuf>, failures: &mut Vec<String>) {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                failures.push(format!("无法读取会话目录: {} ({error})", root.display()));
            }
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                failures.push(format!("无法读取会话目录项: {} ({error})", root.display()));
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                failures.push(format!(
                    "无法读取会话文件类型: {} ({error})",
                    path.display()
                ));
                continue;
            }
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_rollout_paths(&path, out, failures);
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
    scan_rollouts_with_thread_filter(codex_dir, target_provider, None, None, true, None, false)
}

pub(super) fn scan_provider_rollouts(
    codex_dir: &Path,
    target_provider: &str,
    excluded_thread_ids: &HashSet<String>,
    authoritative_rollout_paths: &HashMap<String, String>,
) -> Result<RolloutScan> {
    scan_rollouts_with_thread_filter(
        codex_dir,
        target_provider,
        None,
        Some(authoritative_rollout_paths),
        false,
        Some(excluded_thread_ids),
        true,
    )
}

pub(super) fn scan_rollouts_for_thread_ids(
    codex_dir: &Path,
    target_provider: &str,
    thread_ids: &HashSet<String>,
    rollout_paths_by_thread_id: &HashMap<String, String>,
) -> Result<RolloutScan> {
    scan_rollouts_with_thread_filter(
        codex_dir,
        target_provider,
        Some(thread_ids),
        Some(rollout_paths_by_thread_id),
        false,
        None,
        true,
    )
}

fn scan_rollouts_with_thread_filter(
    codex_dir: &Path,
    target_provider: &str,
    allowed_thread_ids: Option<&HashSet<String>>,
    rollout_paths_by_thread_id: Option<&HashMap<String, String>>,
    include_archived_storage: bool,
    excluded_thread_ids: Option<&HashSet<String>>,
    exclude_source_marked_subagents: bool,
) -> Result<RolloutScan> {
    let mut paths = Vec::new();
    let mut scan = RolloutScan::default();
    let mut referenced = HashMap::new();
    collect_rollout_paths(
        &codex_dir.join("sessions"),
        &mut paths,
        &mut scan.scan_failures,
    );
    if include_archived_storage {
        collect_rollout_paths(
            &codex_dir.join("archived_sessions"),
            &mut paths,
            &mut scan.scan_failures,
        );
    }
    paths.sort();
    scan.discovered_rollout_files = paths.len();
    if let Some(excluded_thread_ids) = excluded_thread_ids {
        paths.retain(|path| {
            !excluded_thread_ids
                .iter()
                .any(|id| rollout_filename_matches_id(path, id))
        });
    }
    if let Some(allowed_thread_ids) = allowed_thread_ids {
        let empty_rollout_paths = HashMap::new();
        let rollout_paths_by_thread_id = rollout_paths_by_thread_id.unwrap_or(&empty_rollout_paths);
        let explicitly_referenced_thread_ids = rollout_paths_by_thread_id
            .keys()
            .filter(|id| allowed_thread_ids.contains(*id))
            .cloned()
            .collect::<HashSet<_>>();
        referenced = referenced_rollout_paths(
            codex_dir,
            rollout_paths_by_thread_id,
            include_archived_storage,
            &mut scan.scan_failures,
        );
        paths.retain(|path| {
            let is_unreferenced_thread_rollout = allowed_thread_ids
                .iter()
                .filter(|id| !explicitly_referenced_thread_ids.contains(*id))
                .any(|id| rollout_filename_matches_id(path, id));
            is_unreferenced_thread_rollout
                || path
                    .canonicalize()
                    .is_ok_and(|canonical| referenced.contains_key(&canonical))
        });
    }
    scan.rollout_files = paths.len();

    for path in paths {
        let expected_thread_ids = path
            .canonicalize()
            .ok()
            .and_then(|canonical| referenced.get(&canonical));
        let identity = match read_rollout_identity(&path) {
            Ok(identity) => identity,
            Err(failure) => {
                scan.scan_failures.push(failure);
                continue;
            }
        };
        let identity_id = identity.payload.id.as_deref().unwrap_or_default().trim();
        if expected_thread_ids
            .is_some_and(|expected| expected.len() != 1 || !expected.contains(identity_id))
        {
            scan.scan_failures.push(format!(
                "活动 SQLite 引用的会话文件与线程 ID 不一致: {}",
                path.display()
            ));
            continue;
        }
        let authoritative_identity = rollout_paths_by_thread_id.is_none_or(|authority| {
            is_authoritative_rollout(codex_dir, &path, identity_id, authority)
        });
        if identity.payload.is_internal() {
            if authoritative_identity {
                scan.internal_thread_ids.insert(identity_id.to_string());
            }
            if exclude_source_marked_subagents {
                continue;
            }
        }
        let rollout_size = match fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                scan.scan_failures.push(format!(
                    "无法读取会话文件大小: {} ({error})",
                    path.display()
                ));
                continue;
            }
        };
        if rollout_size > MAX_ROLLOUT_IN_MEMORY_BYTES {
            if excluded_thread_ids.is_some_and(|excluded| excluded.contains(identity_id))
                || allowed_thread_ids.is_some_and(|allowed| !allowed.contains(identity_id))
            {
                continue;
            }
            scan.session_meta_count += 1;
            scan.thread_ids.insert(identity_id.to_string());
            if let Some(cwd) = identity
                .payload
                .cwd
                .as_deref()
                .and_then(normalize_workspace_path)
            {
                scan.cwd_by_thread_id.insert(identity_id.to_string(), cwd);
            }
            if identity.payload.model_provider.as_deref() == Some(target_provider) {
                scan.provider_candidate_paths.insert(path);
            } else {
                scan.warnings.push(format!(
                    "会话文件过大（{} MiB），为避免内存耗尽已跳过 Provider 改写: {}",
                    rollout_size / (1024 * 1024),
                    path.display()
                ));
            }
            continue;
        }
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                let reason = if is_locked_io_error(&error) {
                    "会话文件被占用或无权限读取"
                } else {
                    "无法读取会话文件"
                };
                scan.scan_failures
                    .push(format!("{reason}: {} ({error})", path.display()));
                continue;
            }
        };
        let mut next_text = String::with_capacity(text.len());
        let mut rewrite_needed = false;
        let mut file_session_meta_count = 0usize;
        let mut file_mismatched_session_meta = 0usize;
        let mut invalid_json_lines = 0usize;
        let mut invalid_session_meta = 0usize;
        let mut thread_id = None;
        let mut cwd = None;
        let mut is_subagent = false;

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
                            if file_session_meta_count == 1 {
                                thread_id = payload
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .map(ToString::to_string);
                                cwd = payload
                                    .get("cwd")
                                    .and_then(Value::as_str)
                                    .and_then(normalize_workspace_path);
                                is_subagent =
                                    payload.get("source").is_some_and(source_value_is_subagent)
                                        || payload
                                            .get("thread_source")
                                            .and_then(Value::as_str)
                                            .is_some_and(thread_source_is_internal);
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
                                file_mismatched_session_meta += 1;
                            }
                        } else {
                            invalid_session_meta += 1;
                        }
                    }
                } else {
                    invalid_json_lines += 1;
                }
            }
            next_text.push_str(&next_line);
            next_text.push_str(line_ending);
        }

        if is_subagent && authoritative_identity {
            if let Some(id) = thread_id.as_ref() {
                if expected_thread_ids.is_none_or(|expected| expected.contains(id)) {
                    scan.internal_thread_ids.insert(id.clone());
                }
            }
        }
        if (exclude_source_marked_subagents && is_subagent)
            || thread_id
                .as_ref()
                .is_some_and(|id| excluded_thread_ids.is_some_and(|excluded| excluded.contains(id)))
        {
            continue;
        }
        if invalid_json_lines > 0 {
            scan.scan_failures.push(format!(
                "会话文件包含 {invalid_json_lines} 行无法解析的 JSON: {}",
                path.display()
            ));
        }
        if invalid_session_meta > 0 {
            scan.scan_failures.push(format!(
                "会话文件包含 {invalid_session_meta} 条无法读取的 session_meta: {}",
                path.display()
            ));
        }
        if invalid_json_lines > 0 || invalid_session_meta > 0 {
            // A broken orphan remains a warning, but must never become a
            // provider/catalog candidate merely because one metadata line parsed.
            continue;
        }

        if file_session_meta_count == 0 {
            if expected_thread_ids.is_some() {
                scan.scan_failures.push(format!(
                    "活动 SQLite 引用的会话文件缺少 session_meta: {}",
                    path.display()
                ));
            }
            continue;
        }
        let Some(thread_id) = thread_id else {
            scan.scan_failures.push(format!(
                "会话文件的 session_meta 缺少 id: {}",
                path.display()
            ));
            continue;
        };
        if let Some(expected_thread_ids) = expected_thread_ids {
            if expected_thread_ids.len() != 1 || !expected_thread_ids.contains(&thread_id) {
                scan.scan_failures.push(format!(
                    "活动 SQLite 引用的会话文件与线程 ID 不一致: {}",
                    path.display()
                ));
                continue;
            }
        }
        if allowed_thread_ids.is_some_and(|allowed| !allowed.contains(&thread_id)) {
            continue;
        }
        scan.session_meta_count += file_session_meta_count;
        scan.provider_candidate_paths.insert(path.clone());
        scan.thread_ids.insert(thread_id.clone());
        if let Some(cwd) = cwd {
            scan.cwd_by_thread_id.insert(thread_id.clone(), cwd);
        }
        if rewrite_needed {
            scan.mismatched_rollouts += 1;
            scan.mismatched_session_meta += file_mismatched_session_meta;
            scan.mismatched_thread_ids.insert(thread_id);
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
    is_active: bool,
    active_priority: usize,
    session_priority: usize,
    allow_custom_names: bool,
}

fn sqlite_storage_roots(codex_dir: &Path) -> Vec<SqliteStorageRoot> {
    let mut roots = Vec::new();
    let configured = configured_sqlite_home(codex_dir);
    if let Some(configured) = configured.as_ref() {
        roots.push(SqliteStorageRoot {
            path: configured.clone(),
            is_active: true,
            active_priority: 0,
            session_priority: 0,
            allow_custom_names: true,
        });
    }
    roots.push(SqliteStorageRoot {
        path: codex_dir.to_path_buf(),
        is_active: configured.is_none(),
        active_priority: 1,
        session_priority: 2,
        allow_custom_names: false,
    });
    roots.push(SqliteStorageRoot {
        path: codex_dir.join("sqlite"),
        is_active: false,
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
    pub(super) active_scan_failures: Vec<String>,
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
    is_active: bool,
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

fn sqlite_table_names(path: &Path, busy_timeout: Option<Duration>) -> Option<HashSet<String>> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    if let Some(timeout) = busy_timeout {
        conn.busy_timeout(timeout).ok()?;
    }
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

fn compare_sqlite_filenames(left: &Path, right: &Path) -> std::cmp::Ordering {
    left.file_name()
        .is_none_or(|name| name != std::ffi::OsStr::new("codex-dev.db"))
        .cmp(
            &right
                .file_name()
                .is_none_or(|name| name != std::ffi::OsStr::new("codex-dev.db")),
        )
        .then_with(|| left.file_name().cmp(&right.file_name()))
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
            .then_with(|| compare_sqlite_filenames(&left.path, &right.path))
    });
    matches
        .into_iter()
        .map(|database| database.path.clone())
        .collect()
}

pub(super) fn discover_sqlite_databases(codex_dir: &Path) -> SqliteDiscovery {
    discover_sqlite_databases_with_busy_timeout(codex_dir, None)
}

fn discover_sqlite_databases_with_busy_timeout(
    codex_dir: &Path,
    busy_timeout: Option<Duration>,
) -> SqliteDiscovery {
    let mut databases = Vec::new();
    let mut seen_paths = HashSet::new();
    let mut unreadable_paths = Vec::new();
    let mut active_scan_failures = Vec::new();

    for root in sqlite_storage_roots(codex_dir) {
        let entries = match fs::read_dir(&root.path) {
            Ok(entries) => entries,
            Err(error) => {
                if root.is_active && error.kind() != std::io::ErrorKind::NotFound {
                    active_scan_failures.push(format!(
                        "无法读取当前会话数据库目录: {} ({error})",
                        root.path.display()
                    ));
                }
                continue;
            }
        };
        let mut paths = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    if root.is_active {
                        active_scan_failures.push(format!(
                            "无法读取当前会话数据库目录项: {} ({error})",
                            root.path.display()
                        ));
                    }
                    continue;
                }
            };
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    if root.is_active {
                        active_scan_failures.push(format!(
                            "无法读取当前会话数据库文件类型: {} ({error})",
                            path.display()
                        ));
                    }
                    continue;
                }
            };
            if file_type.is_file()
                && !file_type.is_symlink()
                && is_sqlite_file(&path)
                && (root.allow_custom_names || is_root_codex_sqlite_file(&path))
            {
                paths.push(path);
            }
        }
        paths.sort();

        for path in paths {
            if !seen_paths.insert(path.clone()) {
                continue;
            }
            let codex_named = is_root_codex_sqlite_file(&path);
            let Some(tables) = sqlite_table_names(&path, busy_timeout) else {
                let header_is_sqlite = has_sqlite_header(&path);
                let read_error = fs::File::open(&path).err();
                if header_is_sqlite || read_error.is_some() {
                    if root.is_active {
                        let detail = read_error
                            .map(|error| format!(" ({error})"))
                            .unwrap_or_default();
                        active_scan_failures.push(format!(
                            "无法读取当前活动会话数据库: {}{detail}",
                            path.display()
                        ));
                    }
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
                is_active: root.is_active,
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
        .filter(|database| database.is_active && database.has_threads)
        .min_by(|left, right| {
            left.active_priority
                .cmp(&right.active_priority)
                .then_with(|| right.state_version.cmp(&left.state_version))
                .then_with(|| compare_sqlite_filenames(&left.path, &right.path))
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
        active_scan_failures,
    }
}

pub(crate) fn sqlite_candidate_paths(codex_dir: &Path) -> Vec<PathBuf> {
    discover_sqlite_databases(codex_dir).active_paths
}

pub(crate) fn sqlite_candidate_paths_with_timeout(
    codex_dir: &Path,
    timeout: Duration,
) -> Vec<PathBuf> {
    discover_sqlite_databases_with_busy_timeout(codex_dir, Some(timeout)).active_paths
}

fn clean_session_title(values: [Option<String>; 3]) -> Option<String> {
    values
        .into_iter()
        .flatten()
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}

pub(crate) fn session_project_title(cwd: &str) -> Option<String> {
    let path = normalize_workspace_path(cwd)?.replace('\\', "/");
    if path.chars().any(char::is_control) || path.contains("://") {
        return None;
    }
    let parts = path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    // A UNC share root is a storage location, rather than a project directory.
    if path.starts_with("//") && parts.len() <= 2 {
        return None;
    }
    let name = parts.last()?.trim();
    if name.is_empty()
        || matches!(name, "." | ".." | "~")
        || (name.len() == 2 && name.as_bytes()[0].is_ascii_alphabetic() && name.ends_with(':'))
    {
        return None;
    }
    Some(name.to_string())
}

/// Best-effort titles for the small recent-usage list. Read only the requested
/// IDs and re-read metadata on each refresh so renamed chats appear immediately.
pub(crate) fn session_titles_by_id(codex_dir: &Path, ids: &[String]) -> HashMap<String, String> {
    let ids = ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .collect::<HashSet<_>>();
    if ids.is_empty() {
        return HashMap::new();
    }
    let timeout = Duration::from_millis(50);
    let discovery = discover_sqlite_databases_with_busy_timeout(codex_dir, Some(timeout));
    let mut titles = HashMap::new();
    let mut projects = HashMap::new();
    for path in discovery.active_first_session_paths() {
        let Ok(conn) = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) else {
            continue;
        };
        if conn.busy_timeout(timeout).is_err()
            || !sqlite_has_table(&conn, "threads").unwrap_or(false)
        {
            continue;
        }
        let Ok(cols) = table_column_set(&conn, "threads") else {
            continue;
        };
        if !cols.contains("id") {
            continue;
        }
        let unresolved = ids
            .iter()
            .copied()
            .filter(|id| !titles.contains_key(*id))
            .collect::<Vec<_>>();
        if unresolved.is_empty() {
            break;
        }
        let title = sql_select_column(&cols, "title", "NULL");
        let first = sql_select_column(&cols, "first_user_message", "NULL");
        let preview = sql_select_column(&cols, "preview", "NULL");
        let cwd = sql_select_column(&cols, "cwd", "NULL");
        // Keep below SQLite's variable limit even if another caller requests more IDs.
        for chunk in unresolved.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let query = format!("SELECT \"id\", {title}, {first}, {preview}, {cwd} FROM threads WHERE \"id\" IN ({placeholders})");
            let Ok(mut statement) = conn.prepare(&query) else {
                continue;
            };
            let Ok(rows) = statement.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                let id = row.get::<_, String>(0)?;
                let text = |index| row.get::<_, Option<String>>(index).ok().flatten();
                Ok((
                    id,
                    clean_session_title([text(1), text(2), text(3)]),
                    text(4),
                ))
            }) else {
                continue;
            };
            for (id, title, cwd) in rows.flatten() {
                if let Some(title) = title {
                    titles.entry(id).or_insert(title);
                } else if let Some(project) = cwd.as_deref().and_then(session_project_title) {
                    projects.entry(id).or_insert(project);
                }
            }
        }
    }
    // Prefer a real chat title from an older database over a project-only fallback.
    for (id, project) in projects {
        titles.entry(id).or_insert(project);
    }
    titles
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
                if thread_source_is_internal(thread_source) {
                    ids.insert(id);
                    continue;
                }
            }

            if let Some(source) = source
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                if source_text_is_subagent(source) {
                    ids.insert(id);
                }
            }
        }
    }

    Ok(ids)
}

pub(crate) fn source_value_is_subagent(source: &Value) -> bool {
    match source {
        Value::String(source) => source_kind_is_internal(source),
        Value::Object(source) => source.contains_key("subagent") || source.contains_key("internal"),
        _ => false,
    }
}

pub(crate) fn source_kind_is_internal(source: &str) -> bool {
    let source = source.trim().to_ascii_lowercase();
    source == "subagent"
        || source == "internal"
        || source.starts_with("subagent_")
        || source.starts_with("internal_")
}

pub(crate) fn thread_source_is_internal(source: &str) -> bool {
    matches!(
        source.trim().to_ascii_lowercase().as_str(),
        "subagent" | "guardian_review" | "memory_consolidation"
    ) || source_text_is_subagent(source)
}

pub(crate) fn source_text_is_subagent(source: &str) -> bool {
    let source = source.trim();
    source_kind_is_internal(source)
        || serde_json::from_str::<Value>(source)
            .ok()
            .as_ref()
            .is_some_and(source_value_is_subagent)
}

// Only retain identity fields from the first rollout record. Instructions and
// other potentially large metadata are streamed past, never stored or logged.
#[derive(Default)]
struct InternalSource(bool);

impl<'de> Deserialize<'de> for InternalSource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct SourceVisitor;
        impl<'de> Visitor<'de> for SourceVisitor {
            type Value = InternalSource;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("session source")
            }

            fn visit_str<E: serde::de::Error>(
                self,
                value: &str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(InternalSource(source_kind_is_internal(value)))
            }

            fn visit_map<M: MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Self::Value, M::Error> {
                let mut internal = false;
                while let Some(key) = map.next_key::<String>()? {
                    internal |= key == "subagent" || key == "internal";
                    map.next_value::<IgnoredAny>()?;
                }
                Ok(InternalSource(internal))
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(InternalSource(false))
            }

            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(InternalSource(false))
            }

            fn visit_bool<E: serde::de::Error>(
                self,
                _: bool,
            ) -> std::result::Result<Self::Value, E> {
                Ok(InternalSource(false))
            }

            fn visit_i64<E: serde::de::Error>(self, _: i64) -> std::result::Result<Self::Value, E> {
                Ok(InternalSource(false))
            }

            fn visit_u64<E: serde::de::Error>(self, _: u64) -> std::result::Result<Self::Value, E> {
                Ok(InternalSource(false))
            }

            fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<Self::Value, E> {
                Ok(InternalSource(false))
            }
        }
        deserializer.deserialize_any(SourceVisitor)
    }
}

#[derive(Default, Deserialize)]
struct RolloutIdentityPayload {
    id: Option<String>,
    cwd: Option<String>,
    model_provider: Option<String>,
    #[serde(default)]
    source: InternalSource,
    thread_source: Option<String>,
}

impl RolloutIdentityPayload {
    fn is_internal(&self) -> bool {
        self.source.0
            || self
                .thread_source
                .as_deref()
                .is_some_and(thread_source_is_internal)
    }
}

#[derive(Deserialize)]
struct RolloutIdentityRecord {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    payload: RolloutIdentityPayload,
}

/// Classify only the rollout's own first metadata record. A user-created fork
/// can replay an internal parent's metadata later without becoming internal.
pub(super) fn rollout_text_is_internal(text: &str) -> bool {
    RolloutIdentityRecord::deserialize(&mut serde_json::Deserializer::from_str(text))
        .is_ok_and(|record| record.kind == "session_meta" && record.payload.is_internal())
}

pub(super) fn rollout_path_has_syncable_identity(path: &Path) -> Result<bool> {
    read_rollout_identity(path)
        .map(|record| !record.payload.is_internal())
        .map_err(CodexxError::Config)
}

fn is_filename_uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

/// Codex thread/revert keeps the thread ID and appends a distinct rollout ID:
/// rollout-<timestamp>-<thread-id>_<rollout-id>.jsonl. The suffix is never the
/// conversation identity. Share this parser with lookup/deletion fallback so
/// neither path can accidentally select a different thread by its rollout ID.
fn rollout_filename_thread_id(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    let stem = name
        .strip_suffix(".jsonl.zst")
        .or_else(|| name.strip_suffix(".jsonl"))?;
    let core = stem
        .rsplit_once('_')
        .and_then(|(prefix, rollout_id)| {
            if !is_filename_uuid(rollout_id) {
                return None;
            }
            let thread_id = prefix.get(prefix.len().checked_sub(36)?..)?;
            let delimiter = prefix.get(prefix.len().checked_sub(37)?..prefix.len() - 36)?;
            (is_filename_uuid(thread_id) && delimiter == "-").then_some(prefix)
        })
        .unwrap_or(stem);
    let thread_id = core.get(core.len().checked_sub(36)?..)?;
    let delimiter = core.get(core.len().checked_sub(37)?..core.len() - 36)?;
    (is_filename_uuid(thread_id) && delimiter == "-").then_some(thread_id)
}

fn read_rollout_identity(path: &Path) -> std::result::Result<RolloutIdentityRecord, String> {
    let file = fs::File::open(path)
        .map_err(|_| format!("无法读取会话文件来源信息: {}", path.display()))?;
    let record = RolloutIdentityRecord::deserialize(&mut serde_json::Deserializer::from_reader(
        BufReader::new(file),
    ))
    .map_err(|_| {
        format!(
            "会话文件包含无法解析的 JSON 或无法读取会话文件来源信息: {}",
            path.display()
        )
    })?;
    if record.kind != "session_meta" {
        return Err(format!("会话文件缺少起始 session_meta: {}", path.display()));
    }
    let Some(id) = record
        .payload
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return Err(format!("会话来源信息缺少线程 ID: {}", path.display()));
    };
    if rollout_filename_thread_id(path).is_some_and(|filename_id| filename_id != id) {
        return Err(format!(
            "会话来源信息与文件线程 ID 不一致: {}",
            path.display()
        ));
    }
    Ok(record)
}

fn is_authoritative_rollout(
    codex_dir: &Path,
    path: &Path,
    id: &str,
    authority: &HashMap<String, String>,
) -> bool {
    let Some(authoritative_path) = authority.get(id) else {
        return true;
    };
    let raw = PathBuf::from(authoritative_path);
    let resolved = if raw.is_absolute() {
        raw
    } else {
        codex_dir.join(raw)
    };
    resolved
        .canonicalize()
        .ok()
        .zip(path.canonicalize().ok())
        .is_some_and(|(expected, actual)| expected == actual)
}

/// Read rollout identities independently of the SQLite provider candidate list.
/// This also protects internal threads whose database source is missing/stale.
/// Any failure is returned as a scan failure so an unknown identity cannot be
/// silently promoted into a visible Desktop catalog entry.
pub(super) fn rollout_internal_thread_ids(
    codex_dir: &Path,
    authoritative_rollout_paths: &HashMap<String, String>,
) -> (HashSet<String>, Vec<String>) {
    let mut paths = Vec::new();
    let mut failures = Vec::new();
    let mut ids = HashSet::new();
    collect_rollout_paths(&codex_dir.join("sessions"), &mut paths, &mut failures);
    for path in paths {
        let record = match read_rollout_identity(&path) {
            Ok(record) => record,
            Err(failure) => {
                failures.push(failure);
                continue;
            }
        };
        let id = record.payload.id.as_deref().unwrap_or_default().trim();
        if !is_authoritative_rollout(codex_dir, &path, id, authoritative_rollout_paths) {
            continue;
        }
        if record.payload.is_internal() {
            ids.insert(id.to_string());
        }
    }
    (ids, failures)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UsageThreadIdentity {
    pub(crate) is_subagent: bool,
    pub(crate) classification_known: bool,
    pub(crate) parent_id: Option<String>,
    pub(crate) parent_conflict: bool,
    /// Only the current database's rollout reference can override stale copies.
    pub(crate) rollout_path: Option<PathBuf>,
}

fn usage_identities_on_connection(
    conn: &Connection,
) -> Result<HashMap<String, (bool, bool, HashSet<String>, Option<String>)>> {
    if !sqlite_has_table(conn, "threads")? {
        return Ok(HashMap::new());
    }
    let cols = table_column_set(conn, "threads")?;
    if !cols.contains("id") {
        return Ok(HashMap::new());
    }
    let subagents = sqlite_subagent_thread_ids(conn, &cols)?;
    let source = sql_select_column(&cols, "source", "NULL");
    let thread_source = sql_select_column(&cols, "thread_source", "NULL");
    let rollout_path = sql_select_column(&cols, "rollout_path", "NULL");
    let mut statement = conn
        .prepare(&format!(
            "SELECT id, {source}, {thread_source}, {rollout_path} FROM threads"
        ))
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1).ok().flatten(),
                row.get::<_, Option<String>>(2).ok().flatten(),
                row.get::<_, Option<String>>(3).ok().flatten(),
            ))
        })
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    let mut identities = HashMap::new();
    for row in rows {
        let (id, source, thread_source, rollout_path) =
            row.map_err(|error| CodexxError::Database(error.to_string()))?;
        if id.trim().is_empty() {
            continue;
        }
        let is_subagent = subagents.contains(&id);
        let classification_known = is_subagent
            || thread_source
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            || source
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty());
        let mut parents = HashSet::new();
        if is_subagent {
            if let Some(parent) = source
                .as_deref()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .and_then(|value| {
                    value
                        .pointer("/subagent/thread_spawn/parent_thread_id")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
            {
                parents.insert(parent);
            }
        }
        identities.insert(
            id,
            (is_subagent, classification_known, parents, rollout_path),
        );
    }
    if sqlite_has_table(conn, "thread_spawn_edges")? {
        let cols = table_column_set(conn, "thread_spawn_edges")?;
        if cols.contains("child_thread_id") && cols.contains("parent_thread_id") {
            let mut statement = conn
                .prepare("SELECT child_thread_id, parent_thread_id FROM thread_spawn_edges")
                .map_err(|error| CodexxError::Database(error.to_string()))?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1).ok().flatten(),
                    ))
                })
                .map_err(|error| CodexxError::Database(error.to_string()))?;
            for row in rows {
                let (child, parent) =
                    row.map_err(|error| CodexxError::Database(error.to_string()))?;
                if let Some((true, _, parents, _)) = identities.get_mut(&child) {
                    if let Some(parent) = parent
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty())
                    {
                        parents.insert(parent);
                    }
                }
            }
        }
    }
    Ok(identities)
}

/// Reuse session-management classification without reading any rollout content.
/// Current database records override stale legacy classification and parentage.
pub(crate) fn usage_thread_identities(codex_dir: &Path) -> HashMap<String, UsageThreadIdentity> {
    let timeout = Duration::from_millis(50);
    let discovery = discover_sqlite_databases_with_busy_timeout(codex_dir, Some(timeout));
    let mut active_ids = HashSet::new();
    let mut combined = HashMap::<String, (bool, bool, HashSet<String>, Option<String>)>::new();
    for path in discovery.active_first_session_paths() {
        let Ok(conn) = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) else {
            continue;
        };
        if conn.busy_timeout(timeout).is_err() {
            continue;
        }
        let Ok(identities) = usage_identities_on_connection(&conn) else {
            continue;
        };
        let is_active = discovery.active_paths.contains(&path);
        for (id, (is_subagent, classification_known, parents, rollout_path)) in identities {
            if is_active {
                active_ids.insert(id.clone());
                combined.insert(
                    id,
                    (is_subagent, classification_known, parents, rollout_path),
                );
            } else if !active_ids.contains(&id) {
                let existing = combined
                    .entry(id)
                    .or_insert_with(|| (false, false, HashSet::new(), None));
                existing.0 |= is_subagent;
                existing.1 |= classification_known;
                existing.2.extend(parents);
            }
        }
    }
    combined
        .into_iter()
        .map(
            |(id, (is_subagent, classification_known, parents, rollout_path))| {
                let parent_conflict = is_subagent && parents.len() > 1;
                let parent_id = (is_subagent && parents.len() == 1)
                    .then(|| parents.into_iter().next().unwrap());
                let rollout_path =
                    rollout_path
                        .filter(|path| !path.trim().is_empty())
                        .map(|path| {
                            let path = PathBuf::from(path);
                            if path.is_absolute() {
                                path
                            } else {
                                codex_dir.join(path)
                            }
                        });
                (
                    id,
                    UsageThreadIdentity {
                        is_subagent,
                        classification_known,
                        parent_id,
                        parent_conflict,
                        rollout_path,
                    },
                )
            },
        )
        .collect()
}

pub(super) struct SqliteThreadIndexState<'a> {
    pub(super) thread_id: &'a str,
    pub(super) provider: Option<&'a str>,
    pub(super) cwd: Option<&'a str>,
    pub(super) cwd_column: bool,
    pub(super) archived: bool,
}

pub(super) fn sqlite_thread_needs_alignment(
    rollouts: &RolloutScan,
    target_provider: &str,
    state: &SqliteThreadIndexState<'_>,
) -> bool {
    if state.archived {
        return false;
    }
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
    let mut syncable_thread_ids = HashSet::new();
    let mut archived_thread_ids = HashSet::new();
    let mut rollout_paths_by_thread_id = HashMap::new();
    let mut subagent_ids = rollouts.internal_thread_ids.clone();
    let mut mismatched_ids = HashSet::new();
    for path in sqlite_paths {
        let conn = match Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(conn) => conn,
            Err(e) => {
                scan.scan_failures
                    .push(format!("无法读取当前活动 SQLite: {} ({e})", path.display()));
                continue;
            }
        };
        if !sqlite_has_table(&conn, "threads")? {
            continue;
        }
        let cols = table_column_set(&conn, "threads")?;
        if !cols.contains("id") || !cols.contains("model_provider") {
            scan.scan_failures.push(format!(
                "当前活动 SQLite 的 threads 表缺少 id 或 model_provider 字段: {}",
                path.display()
            ));
            continue;
        }
        scan.sqlite_dbs += 1;
        let cwd_col = sql_select_column(&cols, "cwd", "NULL");
        let rollout_col = sql_select_column(&cols, "rollout_path", "NULL");
        let archived_col = sql_select_column(&cols, "archived", "0");
        let query = format!(
            "SELECT \"id\", \"model_provider\", {cwd_col}, {rollout_col}, {archived_col} FROM threads"
        );
        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        for row in rows {
            let (id, provider, cwd, rollout_path, archived) =
                row.map_err(|e| CodexxError::Database(e.to_string()))?;
            thread_ids.insert(id.clone());
            let archived = archived != 0;
            if archived {
                archived_thread_ids.insert(id.clone());
            } else {
                syncable_thread_ids.insert(id.clone());
                if let Some(rollout_path) = rollout_path
                    .map(|path| path.trim().to_string())
                    .filter(|path| !path.is_empty())
                {
                    rollout_paths_by_thread_id.insert(id.clone(), rollout_path);
                }
            }
            if sqlite_thread_needs_alignment(
                rollouts,
                target_provider,
                &SqliteThreadIndexState {
                    thread_id: &id,
                    provider: provider.as_deref(),
                    cwd: cwd.as_deref(),
                    cwd_column: cols.contains("cwd"),
                    archived,
                },
            ) {
                mismatched_ids.insert(id);
            }
        }
        subagent_ids.extend(sqlite_subagent_thread_ids(&conn, &cols)?);
    }
    subagent_ids.retain(|id| thread_ids.contains(id));
    syncable_thread_ids.retain(|id| !subagent_ids.contains(id));
    rollout_paths_by_thread_id.retain(|id, _| !subagent_ids.contains(id));
    mismatched_ids.retain(|id| !subagent_ids.contains(id));
    scan.sqlite_threads = thread_ids.len();
    scan.subagent_threads = subagent_ids.len();
    scan.top_level_threads = thread_ids.len().saturating_sub(subagent_ids.len());
    scan.mismatched_threads = mismatched_ids.len();
    scan.thread_ids = thread_ids;
    scan.syncable_thread_ids = syncable_thread_ids;
    scan.archived_thread_ids = archived_thread_ids;
    scan.subagent_thread_ids = subagent_ids;
    scan.rollout_paths_by_thread_id = rollout_paths_by_thread_id;
    scan.mismatched_thread_ids = mismatched_ids;
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
                let clean_title = clean_session_title([title, first_message, preview])
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
                let is_archived = archived != 0;
                let is_subagent =
                    subagent_thread_ids.contains(&id) || rollouts.internal_thread_ids.contains(&id);
                let needs_sync = !is_archived
                    && !is_subagent
                    && (rollouts.mismatched_thread_ids.contains(&id)
                        || sqlite_thread_needs_alignment(
                            rollouts,
                            target_provider,
                            &SqliteThreadIndexState {
                                thread_id: &id,
                                provider: normalized_provider.as_deref(),
                                cwd: normalized_cwd.as_deref(),
                                cwd_column: cols.contains("cwd"),
                                archived: is_archived,
                            },
                        ));
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
                    archived: is_archived,
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
    // Internal tasks can greatly outnumber their parents. Apply the display
    // limit after reserving space for real conversations, otherwise hiding the
    // newest internal rows in the frontend could leave an empty session list.
    let (ordinary, internal): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .filter(|session| seen.insert(session.id.clone()))
        .partition(|session| !session.is_subagent);
    let mut sessions = ordinary
        .into_iter()
        .chain(internal)
        .take(limit.max(1))
        .collect::<Vec<_>>();
    sessions.sort_by(|a, b| {
        b.updated_at_ms
            .cmp(&a.updated_at_ms)
            .then_with(|| a.id.cmp(&b.id))
    });
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

    fn create_title_database(path: &Path) -> Connection {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("CREATE TABLE threads (id TEXT PRIMARY KEY, title, first_user_message, preview, cwd TEXT);").unwrap();
        conn
    }

    fn create_identity_database(path: &Path) -> Connection {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, thread_source TEXT, source TEXT);
            CREATE TABLE thread_spawn_edges (parent_thread_id TEXT, child_thread_id TEXT);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn internal_sources_cover_serialized_and_display_forms_without_title_heuristics() {
        for source in [
            "subagent",
            " subagent_review ",
            "subagent_thread_spawn_parent_d1",
            "subagent_memory_consolidation",
            "internal",
            "internal_guardian",
            "internal_memory_consolidation",
            r#"{"internal":"guardian"}"#,
            r#"{"internal":"memory_consolidation"}"#,
            r#"{"subagent":"review"}"#,
            r#"{"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}}"#,
        ] {
            assert!(source_text_is_subagent(source), "{source}");
        }
        for source in [
            "cli",
            "vscode",
            "exec",
            "mcp",
            "unknown",
            "user",
            "codex-auto-review",
            r#"{"custom":"chatgpt"}"#,
            r#"{"custom":"codex-auto-review"}"#,
        ] {
            assert!(!source_text_is_subagent(source), "{source}");
        }
        for source in ["subagent", "guardian_review", "memory_consolidation"] {
            assert!(thread_source_is_internal(source));
        }
        assert!(!thread_source_is_internal("future_feature"));
    }

    #[test]
    fn sqlite_internal_classification_keeps_edges_and_all_current_internal_sources() {
        let dir = temp_codex_dir("current-internal-sources");
        let conn = create_identity_database(&dir.join("state_10.sqlite"));
        conn.execute_batch(
            r#"INSERT INTO threads VALUES
            ('parent', 'user', 'cli'),
            ('empty-user', NULL, NULL),
            ('user-fork', 'user', 'vscode'),
            ('guardian', 'guardian_review', 'exec'),
            ('memory', 'memory_consolidation', NULL),
            ('internal-source', 'user', '{"internal":"guardian"}'),
            ('edge-cli', NULL, 'cli'),
            ('edge-user', 'user', 'cli'),
            ('edge-future', 'future_feature', 'unknown'),
            ('review-display', NULL, 'subagent_review');
            INSERT INTO thread_spawn_edges VALUES
            ('parent', 'edge-cli'), ('parent', 'edge-user'), ('parent', 'edge-future');"#,
        )
        .unwrap();
        let cols = table_column_set(&conn, "threads").unwrap();
        let internal = sqlite_subagent_thread_ids(&conn, &cols).unwrap();
        assert_eq!(
            internal,
            [
                "guardian",
                "memory",
                "internal-source",
                "edge-cli",
                "edge-user",
                "edge-future",
                "review-display"
            ]
            .into_iter()
            .map(String::from)
            .collect()
        );
        drop(conn);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rollout_identity_streams_large_headers_and_ignores_replayed_parent_metadata() {
        let dir = temp_codex_dir("rollout-internal-identities");
        fs::create_dir_all(dir.join("sessions")).unwrap();
        let large_instructions = "x".repeat(3 * 1024 * 1024);
        let internal = serde_json::json!({"type":"session_meta","payload":{"id":"internal","source":{"internal":"guardian"},"base_instructions":{"text":large_instructions}}}).to_string();
        fs::write(
            dir.join("sessions/rollout-test-internal.jsonl"),
            format!("{internal}\nnot even JSON body\n"),
        )
        .unwrap();
        let fork = serde_json::json!({"type":"session_meta","payload":{"id":"fork","source":"vscode","thread_source":"user","forked_from_id":"internal"}}).to_string();
        let fork_text = format!("{fork}\n{internal}\n");
        fs::write(dir.join("sessions/rollout-test-fork.jsonl"), &fork_text).unwrap();
        assert!(rollout_text_is_internal(&internal));
        assert!(!rollout_text_is_internal(&fork_text));
        let (ids, failures) = rollout_internal_thread_ids(&dir, &HashMap::new());
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(ids, HashSet::from(["internal".to_string()]));
        let scan =
            scan_provider_rollouts(&dir, "custom", &HashSet::new(), &HashMap::new()).unwrap();
        assert_eq!(scan.internal_thread_ids, ids);
        assert!(scan.thread_ids.contains("fork"));
        assert!(!scan.thread_ids.contains("internal"));
        assert_eq!(scan.changes.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn oversized_rollouts_are_classified_without_loading_the_body() {
        let dir = temp_codex_dir("oversized-rollout-scan");
        let sessions = dir.join("sessions");
        fs::create_dir_all(&sessions).unwrap();

        let matching = sessions.join("rollout-large-matching.jsonl");
        fs::write(
            &matching,
            r#"{"type":"session_meta","payload":{"id":"large-matching","cwd":"C:/workspace","model_provider":"openai","source":"vscode"}}
"#,
        )
        .unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&matching)
            .unwrap()
            .set_len(MAX_ROLLOUT_IN_MEMORY_BYTES + 1)
            .unwrap();

        let mismatched = sessions.join("rollout-large-mismatched.jsonl");
        fs::write(
            &mismatched,
            r#"{"type":"session_meta","payload":{"id":"large-mismatched","model_provider":"custom","source":"vscode"}}
"#,
        )
        .unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&mismatched)
            .unwrap()
            .set_len(MAX_ROLLOUT_IN_MEMORY_BYTES + 1)
            .unwrap();

        let scan = scan_provider_rollouts(&dir, "openai", &HashSet::new(), &HashMap::new())
            .expect("scan oversized rollouts");

        assert!(scan.scan_failures.is_empty(), "{:?}", scan.scan_failures);
        assert_eq!(scan.session_meta_count, 2);
        assert!(scan.thread_ids.contains("large-matching"));
        assert!(scan.thread_ids.contains("large-mismatched"));
        assert_eq!(
            scan.cwd_by_thread_id
                .get("large-matching")
                .map(String::as_str),
            Some("C:/workspace")
        );
        assert!(scan.provider_candidate_paths.contains(&matching));
        assert!(!scan.provider_candidate_paths.contains(&mismatched));
        assert!(scan.changes.is_empty());
        assert_eq!(scan.warnings.len(), 1);
        assert!(scan.warnings[0].contains("避免内存耗尽"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reverted_rollout_filenames_match_the_thread_not_the_rollout_id() {
        let thread = "019f6000-0000-7000-8000-000000000901";
        let rollout = "019f6000-0000-7000-8000-000000000902";
        for extension in ["jsonl", "jsonl.zst"] {
            let ordinary =
                PathBuf::from(format!("rollout-2026-09-17T13-10-16-{thread}.{extension}"));
            let reverted = PathBuf::from(format!(
                "rollout-2026-09-17T13-10-16-{thread}_{rollout}.{extension}"
            ));
            let other_thread =
                PathBuf::from(format!("rollout-2026-09-17T13-10-16-{rollout}.{extension}"));
            assert_eq!(rollout_filename_thread_id(&ordinary), Some(thread));
            assert_eq!(rollout_filename_thread_id(&reverted), Some(thread));
            assert!(rollout_filename_matches_id(&ordinary, thread));
            assert!(rollout_filename_matches_id(&reverted, thread));
            assert!(!rollout_filename_matches_id(&reverted, rollout));
            assert!(!rollout_filename_matches_id(
                &reverted,
                &format!("{thread}_{rollout}")
            ));
            assert!(!rollout_filename_matches_id(&other_thread, thread));
            assert!(rollout_filename_matches_id(&other_thread, rollout));
            let legacy_prefix = PathBuf::from(format!("rollout-custom_label-{thread}.{extension}"));
            assert_eq!(rollout_filename_thread_id(&legacy_prefix), Some(thread));
        }
        assert!(rollout_filename_matches_id(
            Path::new("rollout-test-legacy-id.jsonl"),
            "legacy-id"
        ));
    }

    #[test]
    fn reverted_rollout_identity_preserves_strict_metadata_and_internal_checks() {
        let dir = temp_codex_dir("reverted-rollout-identity");
        let thread = "019f6000-0000-7000-8000-000000000911";
        let rollout = "019f6000-0000-7000-8000-000000000912";
        let path = dir.join(format!(
            "rollout-2026-09-17T13-10-16-{thread}_{rollout}.jsonl"
        ));
        let write_meta = |id: &str, source: Value| {
            let record = serde_json::json!({"type":"session_meta", "payload":{
                "id":id, "source":source, "thread_source":"user", "model_provider":"openai"
            }});
            fs::write(&path, format!("{record}\n")).unwrap();
        };
        write_meta(thread, serde_json::json!("vscode"));
        assert_eq!(
            read_rollout_identity(&path).unwrap().payload.id.as_deref(),
            Some(thread)
        );
        assert!(rollout_path_has_syncable_identity(&path).unwrap());
        for wrong_id in [rollout, "019f6000-0000-7000-8000-000000000913"] {
            write_meta(wrong_id, serde_json::json!("vscode"));
            assert!(read_rollout_identity(&path).is_err());
            assert!(rollout_path_has_syncable_identity(&path).is_err());
        }
        write_meta(thread, serde_json::json!({"internal":"guardian"}));
        assert!(read_rollout_identity(&path).unwrap().payload.is_internal());
        assert!(!rollout_path_has_syncable_identity(&path).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rollout_identity_rejects_wrong_thread_id_and_malformed_headers() {
        let dir = temp_codex_dir("rollout-unknown-identities");
        fs::create_dir_all(dir.join("sessions")).unwrap();
        fs::write(dir.join("sessions/rollout-test-019f6000-0000-7000-8000-000000000301.jsonl"), r#"{"type":"session_meta","payload":{"id":"019f6000-0000-7000-8000-000000000302","source":{"internal":"guardian"}}}"#).unwrap();
        fs::write(dir.join("sessions/rollout-test-broken.jsonl"), "{broken").unwrap();
        fs::write(
            dir.join("sessions/rollout-test-missing.jsonl"),
            r#"{"type":"session_meta","payload":{"source":{"internal":"guardian"}}}"#,
        )
        .unwrap();
        let (ids, failures) = rollout_internal_thread_ids(&dir, &HashMap::new());
        assert!(ids.is_empty());
        assert_eq!(failures.len(), 3);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn current_rollout_authority_ignores_a_stale_internal_copy_of_the_same_thread() {
        let dir = temp_codex_dir("rollout-authoritative-classification");
        fs::create_dir_all(dir.join("sessions")).unwrap();
        let current = dir.join("sessions/rollout-current.jsonl");
        let copy = dir.join("sessions/rollout-copy-shared.jsonl");
        let normal_copy = dir.join("sessions/rollout-normal-copy-shared.jsonl");
        let normal = r#"{"type":"session_meta","payload":{"id":"shared","source":"vscode","model_provider":"openai"}}"#;
        fs::write(&current, normal).unwrap();
        fs::write(&normal_copy, normal).unwrap();
        fs::write(&copy, r#"{"type":"session_meta","payload":{"id":"shared","source":{"internal":"guardian"},"model_provider":"openai"}}"#).unwrap();
        let authority = HashMap::from([("shared".to_string(), current.display().to_string())]);
        let (ids, failures) = rollout_internal_thread_ids(&dir, &authority);
        assert!(ids.is_empty());
        assert!(failures.is_empty());
        let scan = scan_provider_rollouts(&dir, "custom", &HashSet::new(), &authority).unwrap();
        assert!(scan.internal_thread_ids.is_empty());
        assert_eq!(scan.thread_ids, HashSet::from(["shared".to_string()]));
        assert_eq!(scan.changes.len(), 2);
        assert_eq!(
            scan.provider_candidate_paths,
            HashSet::from([current, normal_copy])
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rollout_internal_identity_keeps_previews_and_sync_candidates_consistent() {
        let dir = temp_codex_dir("internal-preview-alignment");
        let database = dir.join("state_10.sqlite");
        create_thread_database(&database, "internal", "openai");
        let conn = Connection::open(&database).unwrap();
        conn.execute_batch("INSERT INTO threads VALUES ('parent', 'openai', 'Parent', 3), ('empty-user', 'openai', NULL, 2), ('user-named-review', 'openai', 'codex-auto-review', 1);").unwrap();
        drop(conn);
        let rollouts = RolloutScan {
            internal_thread_ids: HashSet::from(["internal".to_string()]),
            ..RolloutScan::default()
        };
        let paths = [database];
        let sqlite = scan_sqlite_with_paths(&paths, &rollouts, "custom").unwrap();
        assert_eq!(sqlite.subagent_threads, 1);
        assert!(!sqlite.syncable_thread_ids.contains("internal"));
        assert!(!sqlite.mismatched_thread_ids.contains("internal"));
        for id in ["parent", "empty-user", "user-named-review"] {
            assert!(sqlite.syncable_thread_ids.contains(id));
        }
        let (previews, failures) =
            list_session_previews_with_paths(&paths, &rollouts, "custom", 100).unwrap();
        assert!(failures.is_empty());
        for preview in previews {
            assert_eq!(preview.is_subagent, preview.id == "internal");
            assert_eq!(preview.needs_sync, preview.id != "internal");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn internal_tasks_do_not_exhaust_the_preview_limit_before_user_conversations() {
        let dir = temp_codex_dir("preview-limit-keeps-users");
        let database = dir.join("state_10.sqlite");
        create_thread_database(&database, "older-parent", "openai");
        let conn = Connection::open(&database).unwrap();
        conn.execute_batch("ALTER TABLE threads ADD COLUMN source TEXT;
            INSERT INTO threads VALUES ('new-internal-one', 'openai', 'Internal', 100, 'internal_guardian'),
              ('new-internal-two', 'openai', 'Internal', 99, 'subagent_review'),
              ('new-internal-three', 'openai', 'Internal', 98, 'subagent'),
              ('older-user-fork', 'openai', NULL, 2, 'vscode');").unwrap();
        drop(conn);
        let (previews, warnings) =
            list_session_previews_with_paths(&[database], &RolloutScan::default(), "custom", 2)
                .unwrap();
        assert!(warnings.is_empty());
        assert_eq!(
            previews
                .iter()
                .map(|preview| preview.id.as_str())
                .collect::<Vec<_>>(),
            vec!["older-user-fork", "older-parent"]
        );
        assert!(previews
            .iter()
            .all(|preview| !preview.is_subagent && preview.needs_sync));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn usage_thread_identity_preserves_current_internal_evidence_despite_user_label() {
        let dir = temp_codex_dir("usage-identities-source");
        let path = dir.join("state_10.sqlite");
        let conn = create_identity_database(&path);
        conn.execute_batch(r#"INSERT INTO threads VALUES
            ('parent', 'user', NULL),
            ('edge', NULL, NULL),
            ('source', NULL, '{"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}}'),
            ('thread-source', 'subagent', NULL),
            ('source-only', NULL, 'subagent'),
            ('user-override', 'user', '{"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}}');
            INSERT INTO thread_spawn_edges VALUES ('parent', 'edge'), ('parent', 'user-override');"#).unwrap();
        drop(conn);
        let before = fs::read(&path).unwrap();
        let identities = usage_thread_identities(&dir);
        assert_eq!(
            identities["edge"],
            UsageThreadIdentity {
                classification_known: true,
                is_subagent: true,
                parent_id: Some("parent".into()),
                parent_conflict: false,
                rollout_path: None,
            }
        );
        assert_eq!(identities["source"], identities["edge"]);
        assert_eq!(
            identities["thread-source"],
            UsageThreadIdentity {
                classification_known: true,
                is_subagent: true,
                parent_id: None,
                parent_conflict: false,
                rollout_path: None,
            }
        );
        assert_eq!(identities["source-only"], identities["thread-source"]);
        assert_eq!(identities["user-override"], identities["edge"]);
        assert_eq!(
            identities["parent"],
            UsageThreadIdentity {
                classification_known: true,
                is_subagent: false,
                parent_id: None,
                parent_conflict: false,
                rollout_path: None,
            }
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn usage_thread_identity_missing_or_blank_source_is_unknown_not_explicit_user() {
        let dir = temp_codex_dir("usage-identities-unknown");
        let active = create_identity_database(&dir.join("state_10.sqlite"));
        active
            .execute_batch(
                "INSERT INTO threads VALUES
            ('null-source', NULL, NULL), ('blank-source', '  ', '  '),
            ('known-user', 'user', NULL), ('source-user', NULL, 'cli');",
            )
            .unwrap();
        let legacy_path = dir.join("sqlite/state_5.sqlite");
        create_thread_database(&legacy_path, "no-source-columns", "openai");
        let identities = usage_thread_identities(&dir);
        for id in ["null-source", "blank-source", "no-source-columns"] {
            assert_eq!(
                identities[id],
                UsageThreadIdentity {
                    is_subagent: false,
                    classification_known: false,
                    parent_id: None,
                    parent_conflict: false,
                    rollout_path: None,
                }
            );
        }
        for id in ["known-user", "source-user"] {
            assert!(!identities[id].is_subagent);
            assert!(identities[id].classification_known);
        }
        drop(active);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn usage_thread_identity_active_records_override_stale_legacy_subagents_and_parents() {
        let dir = temp_codex_dir("usage-identities-active");
        let active = create_identity_database(&dir.join("state_10.sqlite"));
        let legacy = create_identity_database(&dir.join("sqlite/state_5.sqlite"));
        active
            .execute_batch(
                "INSERT INTO threads VALUES ('user', 'user', NULL), ('child', 'subagent', NULL);
            INSERT INTO thread_spawn_edges VALUES ('current-parent', 'child');",
            )
            .unwrap();
        legacy.execute_batch("INSERT INTO threads VALUES ('user', 'subagent', NULL), ('child', 'subagent', NULL), ('legacy-child', 'subagent', NULL);
            INSERT INTO thread_spawn_edges VALUES ('old-parent', 'user'), ('old-parent', 'child'), ('legacy-parent', 'legacy-child');").unwrap();
        let identities = usage_thread_identities(&dir);
        assert_eq!(
            identities["user"],
            UsageThreadIdentity {
                classification_known: true,
                is_subagent: false,
                parent_id: None,
                parent_conflict: false,
                rollout_path: None,
            }
        );
        assert_eq!(
            identities["child"].parent_id.as_deref(),
            Some("current-parent")
        );
        assert_eq!(
            identities["legacy-child"].parent_id.as_deref(),
            Some("legacy-parent")
        );
        drop(active);
        drop(legacy);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn usage_thread_identity_conflicting_parent_edges_or_sources_remain_unassigned() {
        let dir = temp_codex_dir("usage-identities-conflicts");
        let active = create_identity_database(&dir.join("state_10.sqlite"));
        let first = create_identity_database(&dir.join("sqlite/state_5.sqlite"));
        let second = create_identity_database(&dir.join("sqlite/state_6.sqlite"));
        active.execute_batch(r#"INSERT INTO threads VALUES ('conflict', 'subagent', '{"subagent":{"thread_spawn":{"parent_thread_id":"source-parent"}}}');
            INSERT INTO thread_spawn_edges VALUES ('edge-parent', 'conflict');"#).unwrap();
        first.execute_batch("INSERT INTO threads VALUES ('legacy', 'subagent', NULL); INSERT INTO thread_spawn_edges VALUES ('first-parent', 'legacy');").unwrap();
        second.execute_batch("INSERT INTO threads VALUES ('legacy', 'subagent', NULL); INSERT INTO thread_spawn_edges VALUES ('second-parent', 'legacy');").unwrap();
        let identities = usage_thread_identities(&dir);
        assert_eq!(
            identities["conflict"],
            UsageThreadIdentity {
                classification_known: true,
                is_subagent: true,
                parent_id: None,
                parent_conflict: true,
                rollout_path: None,
            }
        );
        assert_eq!(identities["legacy"], identities["conflict"]);
        drop(active);
        drop(first);
        drop(second);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn usage_thread_identity_missing_corrupt_and_locked_databases_are_best_effort() {
        let dir = temp_codex_dir("usage-identities-unavailable");
        assert!(usage_thread_identities(&dir.join("missing")).is_empty());
        assert!(!dir.join("missing").exists());
        fs::write(
            dir.join("state_12.sqlite"),
            b"SQLite format 3\0broken fixture",
        )
        .unwrap();
        assert!(usage_thread_identities(&dir).is_empty());
        let locked = create_identity_database(&dir.join("state_10.sqlite"));
        locked
            .execute_batch("INSERT INTO threads VALUES ('user', 'user', NULL); BEGIN EXCLUSIVE;")
            .unwrap();
        let start = std::time::Instant::now();
        assert!(usage_thread_identities(&dir).is_empty());
        assert!(start.elapsed() < Duration::from_secs(2));
        locked.execute_batch("ROLLBACK;").unwrap();
        assert!(!usage_thread_identities(&dir)["user"].is_subagent);
        drop(locked);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn usage_session_titles_follow_chat_title_fallbacks_and_only_requested_ids() {
        let codex_dir = temp_codex_dir("usage-title-fields");
        let conn = create_title_database(&codex_dir.join("state_10.sqlite"));
        conn.execute_batch(
            "INSERT INTO threads VALUES
             ('named', '  真实聊天标题  ', 'First message', 'Preview', '/projects/unused'),
             ('first', '   ', ' 首条用户消息 ', 'Preview', NULL),
             ('preview', NULL, ' ', '  会话摘要  ', NULL),
             ('project', NULL, NULL, NULL, 'C:\\work\\Codex-X\\'),
             ('invalid-title', X'FF', ' 可读取的首条消息 ', NULL, NULL),
             ('invalid-values', 42, 3.14, X'FF', NULL),
             ('root', NULL, NULL, NULL, '/'),
             ('excluded', 'This row was not requested', NULL, NULL, NULL);",
        )
        .unwrap();
        let ids = [
            "named",
            "first",
            "preview",
            "project",
            "invalid-title",
            "invalid-values",
            "root",
            "x' OR 1=1 --",
        ]
        .map(ToString::to_string);
        let titles = session_titles_by_id(&codex_dir, &ids);
        assert_eq!(titles.len(), 5);
        assert_eq!(titles["named"], "真实聊天标题");
        assert_eq!(titles["first"], "首条用户消息");
        assert_eq!(titles["preview"], "会话摘要");
        assert_eq!(titles["project"], "Codex-X");
        assert_eq!(titles["invalid-title"], "可读取的首条消息");
        assert!(!titles.contains_key("excluded"));
        assert!(!titles.contains_key("invalid-values"));
        assert!(!titles.contains_key("root"));
        drop(conn);
        fs::remove_dir_all(codex_dir).unwrap();
    }

    #[test]
    fn usage_session_titles_prefer_active_database_and_reread_renames() {
        let codex_dir = temp_codex_dir("usage-title-priority");
        let active = create_title_database(&codex_dir.join("state_10.sqlite"));
        let legacy = create_title_database(&codex_dir.join("sqlite/state_5.sqlite"));
        active
            .execute_batch(
                "INSERT INTO threads VALUES
            ('shared', 'Current title', NULL, NULL, NULL),
            ('fallback', ' ', NULL, NULL, '/projects/active-project'),
            ('project', NULL, NULL, NULL, '/projects/active-project');",
            )
            .unwrap();
        legacy
            .execute_batch(
                "INSERT INTO threads VALUES
            ('shared', 'Old title', NULL, NULL, NULL),
            ('fallback', 'Recovered chat title', NULL, NULL, '/projects/old-project'),
            ('project', NULL, NULL, NULL, '/projects/old-project');",
            )
            .unwrap();
        let ids = ["shared", "fallback", "project"].map(ToString::to_string);
        let titles = session_titles_by_id(&codex_dir, &ids);
        assert_eq!(titles["shared"], "Current title");
        assert_eq!(titles["fallback"], "Recovered chat title");
        assert_eq!(titles["project"], "active-project");
        active
            .execute(
                "UPDATE threads SET title = 'Renamed chat title' WHERE id = 'shared'",
                [],
            )
            .unwrap();
        assert_eq!(
            session_titles_by_id(&codex_dir, &ids)["shared"],
            "Renamed chat title"
        );
        drop(active);
        drop(legacy);
        fs::remove_dir_all(codex_dir).unwrap();
    }

    #[test]
    fn usage_session_titles_are_read_only_and_tolerate_missing_corrupt_or_locked_databases() {
        let codex_dir = temp_codex_dir("usage-title-read-only");
        let missing = codex_dir.join("missing");
        let ids = ["session".to_string()];
        assert!(session_titles_by_id(&missing, &ids).is_empty());
        assert!(!missing.exists());
        let path = codex_dir.join("state_10.sqlite");
        create_thread_database(&path, "session", "openai");
        fs::write(
            codex_dir.join("state_12.sqlite"),
            b"SQLite format 3\0broken fixture",
        )
        .unwrap();
        let before = fs::read(&path).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let original_permissions = fs::metadata(&path).unwrap().permissions();
        let mut permissions = original_permissions.clone();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions.clone()).unwrap();
        assert_eq!(
            session_titles_by_id(&codex_dir, &ids)["session"],
            "test session"
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
        assert!(!codex_dir.join("state_10.sqlite-journal").exists());
        fs::set_permissions(&path, original_permissions).unwrap();
        let locked = Connection::open(&path).unwrap();
        locked.execute_batch("BEGIN EXCLUSIVE;").unwrap();
        let start = std::time::Instant::now();
        assert!(session_titles_by_id(&codex_dir, &ids).is_empty());
        assert!(start.elapsed() < Duration::from_secs(2));
        locked.execute_batch("ROLLBACK;").unwrap();
        drop(locked);
        fs::remove_dir_all(codex_dir).unwrap();
    }

    #[test]
    fn session_project_titles_support_platform_paths_and_reject_roots() {
        for (path, expected) in [
            ("/Users/test/projects/Codex-X/", Some("Codex-X")),
            (r"C:\work\Codex-X\", Some("Codex-X")),
            (r"\\?\C:\work\Codex-X", Some("Codex-X")),
            (r"\\server\share\project", Some("project")),
            ("relative/project", Some("project")),
            ("", None),
            ("  ", None),
            ("/", None),
            (r"C:\", None),
            ("C:", None),
            (r"\\server\share\", None),
            (".", None),
            ("..", None),
        ] {
            assert_eq!(session_project_title(path).as_deref(), expected, "{path}");
        }
    }

    #[test]
    fn root_state_10_honors_an_explicit_provider_target() {
        let codex_dir = temp_codex_dir("root-state-10");
        let database = codex_dir.join("state_10.sqlite");
        let id = "019f6000-0000-7000-8000-000000000301";
        fs::write(
            codex_dir.join("config.toml"),
            "model_provider = \"custom\"\n",
        )
        .expect("write shared provider config");
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

        let status = crate::sessions::sync::session_sync_status_inner(
            Some(codex_dir.display().to_string()),
            Some("openai".to_string()),
        )
        .expect("read explicit provider status");
        assert_eq!(status.target_provider, "openai");
        assert!(!status.needs_sync);

        let result = crate::sessions::sync::sync_sessions_provider_inner(
            Some(codex_dir.display().to_string()),
            Some("openai".to_string()),
        )
        .expect("sync root state_10");
        assert_eq!(result.status.target_provider, "openai");
        assert_eq!(result.updated_threads, 0);
        let provider: String = Connection::open(&database)
            .expect("reopen state_10")
            .query_row(
                "SELECT model_provider FROM threads WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .expect("read updated provider");
        assert_eq!(provider, "openai");

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn root_codex_dev_database_is_active_without_state_database() {
        let codex_dir = temp_codex_dir("root-codex-dev");
        let database = codex_dir.join("codex-dev.db");
        create_thread_database(&database, "019f6000-0000-7000-8000-000000000302", "openai");

        let discovery = discover_sqlite_databases(&codex_dir);
        assert_eq!(discovery.active_paths, vec![database.clone()]);
        assert_eq!(discovery.thread_paths, vec![database]);

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn configured_sqlite_home_custom_database_is_active() {
        let codex_dir = temp_codex_dir("configured-custom-active");
        fs::write(
            codex_dir.join("config.toml"),
            "sqlite_home = \"session-store\"\n",
        )
        .expect("write sqlite_home config");
        let database = codex_dir.join("session-store/custom-name.db");
        let later_database = codex_dir.join("session-store/later-name.sqlite3");
        create_thread_database(&database, "019f6000-0000-7000-8000-000000000303", "openai");
        create_thread_database(
            &later_database,
            "019f6000-0000-7000-8000-000000000304",
            "openai",
        );

        let discovery = discover_sqlite_databases(&codex_dir);
        assert_eq!(discovery.active_paths, vec![database.clone()]);
        assert_eq!(discovery.thread_paths, vec![database, later_database]);

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn configured_sqlite_home_prefers_codex_dev_before_custom_database() {
        let codex_dir = temp_codex_dir("configured-codex-dev-precedence");
        fs::write(
            codex_dir.join("config.toml"),
            "sqlite_home = \"session-store\"\n",
        )
        .expect("write sqlite_home config");
        let storage = codex_dir.join("session-store");
        let codex_dev = storage.join("codex-dev.db");
        let custom = storage.join("custom-name.db");
        create_thread_database(&codex_dev, "019f6000-0000-7000-8000-000000000305", "openai");
        create_thread_database(&custom, "019f6000-0000-7000-8000-000000000306", "openai");

        let discovery = discover_sqlite_databases(&codex_dir);
        assert_eq!(discovery.active_paths, vec![codex_dev]);

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn configured_sqlite_home_prefers_latest_state_database() {
        let codex_dir = temp_codex_dir("configured-state-precedence");
        fs::write(
            codex_dir.join("config.toml"),
            "sqlite_home = \"session-store\"\n",
        )
        .expect("write sqlite_home config");
        let storage = codex_dir.join("session-store");
        let custom = storage.join("custom-name.db");
        let codex_dev = storage.join("codex-dev.db");
        let state_5 = storage.join("state_5.sqlite");
        let state_10 = storage.join("state_10.sqlite");
        for (database, id) in [
            (&custom, "019f6000-0000-7000-8000-000000000307"),
            (&codex_dev, "019f6000-0000-7000-8000-000000000308"),
            (&state_5, "019f6000-0000-7000-8000-000000000309"),
            (&state_10, "019f6000-0000-7000-8000-000000000310"),
        ] {
            create_thread_database(database, id, "openai");
        }

        let discovery = discover_sqlite_databases(&codex_dir);
        assert_eq!(discovery.active_paths, vec![state_10]);

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
        create_thread_database(&unrelated, "019f6000-0000-7000-8000-000000000341", "openai");

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
