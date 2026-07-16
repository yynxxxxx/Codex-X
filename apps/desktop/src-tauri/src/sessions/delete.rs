use super::app_server::delete_sessions_via_codex_app_server;
use super::storage::{
    current_model_provider, discover_sqlite_databases, ensure_sqlite_discovery_writable,
    scan_rollouts, split_line_ending, sqlite_subagent_thread_ids, sqlite_thread_needs_alignment,
    SqliteDiscovery, SqliteThreadIndexState,
};
use super::sync::{acquire_session_maintenance_lock, session_sync_status_with_discovery};
use super::types::SessionSyncStatus;
use crate::error::{CodexxError, Result};
use crate::file_io::{io_err, write_text};
use crate::resolve_codex_dir;
use crate::sqlite_utils::{sql_select_column, sqlite_has_table, table_column_set};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionDeleteInput {
    pub(crate) config_dir: Option<String>,
    pub(crate) session_ids: Vec<String>,
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

fn relationship_database_sources(
    discovery: &SqliteDiscovery,
    selected: &[String],
) -> Result<HashMap<String, PathBuf>> {
    let mut sources = HashMap::new();
    for path in discovery.active_first_thread_paths() {
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| {
            CodexxError::Database(format!("读取会话关系失败 {}: {error}", path.display()))
        })?;
        if !table_column_set(&conn, "threads")?.contains("id") {
            continue;
        }
        let mut stmt = conn
            .prepare("SELECT 1 FROM threads WHERE id = ?1 LIMIT 1")
            .map_err(|error| CodexxError::Database(error.to_string()))?;
        for id in selected {
            if !sources.contains_key(id)
                && stmt
                    .exists([id])
                    .map_err(|error| CodexxError::Database(error.to_string()))?
            {
                sources.insert(id.clone(), path.clone());
            }
        }
    }
    Ok(sources)
}

fn collect_thread_spawn_edges(path: &Path) -> Result<Vec<(String, String)>> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| CodexxError::Database(format!("读取会话关系失败 {}: {e}", path.display())))?;
    if !sqlite_has_table(&conn, "thread_spawn_edges")? {
        return Ok(Vec::new());
    }
    let cols = table_column_set(&conn, "thread_spawn_edges")?;
    if !cols.contains("parent_thread_id") || !cols.contains("child_thread_id") {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare("SELECT parent_thread_id, child_thread_id FROM thread_spawn_edges")
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let mut edges = Vec::new();
    for row in rows {
        edges.push(row.map_err(|e| CodexxError::Database(e.to_string()))?);
    }
    Ok(edges)
}

fn relationship_edges_by_database(
    sources: &HashMap<String, PathBuf>,
) -> Result<HashMap<PathBuf, Vec<(String, String)>>> {
    let mut edges = HashMap::new();
    for path in sources.values() {
        if !edges.contains_key(path) {
            edges.insert(path.clone(), collect_thread_spawn_edges(path)?);
        }
    }
    Ok(edges)
}

fn selected_session_roots(
    sources: &HashMap<String, PathBuf>,
    selected: &[String],
) -> Result<Vec<String>> {
    let edges_by_database = relationship_edges_by_database(sources)?;
    let selected_set = selected.iter().cloned().collect::<HashSet<_>>();
    Ok(selected
        .iter()
        .filter(|id| {
            let Some(source) = sources.get(*id) else {
                return true;
            };
            let Some(edges) = edges_by_database.get(source) else {
                return true;
            };
            let mut pending = edges
                .iter()
                .filter(|(_, child)| child == *id)
                .map(|(parent, _)| parent.clone())
                .collect::<Vec<_>>();
            let mut visited = HashSet::new();
            while let Some(parent) = pending.pop() {
                if !visited.insert(parent.clone()) {
                    continue;
                }
                if selected_set.contains(&parent) && sources.get(&parent) == Some(source) {
                    return false;
                }
                pending.extend(
                    edges
                        .iter()
                        .filter(|(_, child)| child == &parent)
                        .map(|(next_parent, _)| next_parent.clone()),
                );
            }
            true
        })
        .cloned()
        .collect())
}

fn session_descendants_by_root(
    sources: &HashMap<String, PathBuf>,
    roots: &[String],
) -> Result<HashMap<String, HashSet<String>>> {
    let edges_by_database = relationship_edges_by_database(sources)?;
    let mut descendants = HashMap::new();
    for root in roots {
        let mut ids = HashSet::from([root.clone()]);
        let Some(edges) = sources
            .get(root)
            .and_then(|source| edges_by_database.get(source))
        else {
            descendants.insert(root.clone(), ids);
            continue;
        };
        let mut pending = vec![root.clone()];
        while let Some(parent) = pending.pop() {
            for child in edges
                .iter()
                .filter(|(candidate, _)| candidate == &parent)
                .map(|(_, child)| child)
            {
                if ids.insert(child.clone()) {
                    pending.push(child.clone());
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
            let cwd_col = sql_select_column(&cols, "cwd", "NULL");
            let query = format!("SELECT id, model_provider, {cwd_col} FROM threads");
            let mut mismatch_stmt = conn
                .prepare(&query)
                .map_err(|error| CodexxError::Database(error.to_string()))?;
            let mismatches = mismatch_stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })
                .map_err(|error| CodexxError::Database(error.to_string()))?;
            for row in mismatches {
                let (id, provider, cwd) =
                    row.map_err(|error| CodexxError::Database(error.to_string()))?;
                if sqlite_thread_needs_alignment(
                    &rollouts,
                    &target_provider,
                    &SqliteThreadIndexState {
                        thread_id: &id,
                        provider: provider.as_deref(),
                        cwd: cwd.as_deref(),
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
fn session_ids_with_descendants(
    sources: &HashMap<String, PathBuf>,
    roots: &[String],
) -> Result<HashSet<String>> {
    Ok(session_descendants_by_root(sources, roots)?
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
    thread_database_paths: &[PathBuf],
    session_ids: &HashSet<String>,
) -> Result<HashSet<PathBuf>> {
    let mut paths = HashSet::new();
    for db_path in thread_database_paths {
        let conn = Connection::open_with_flags(
            db_path,
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
    related_database_paths: &[PathBuf],
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
    for path in related_database_paths {
        let result = (|| -> Result<(usize, usize)> {
            let mut conn = Connection::open_with_flags(
                path,
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
    related_database_paths: &[PathBuf],
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
        purge_session_database_references(related_database_paths, &session_ids);
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
    let discovery = discover_sqlite_databases(codex_dir);
    ensure_sqlite_discovery_writable(&discovery)?;
    let relationship_sources = relationship_database_sources(&discovery, roots)?;
    let session_ids = session_ids_with_descendants(&relationship_sources, roots)?;
    let rollout_paths = selected_rollout_paths(codex_dir, &discovery.thread_paths, &session_ids)?;
    Ok(delete_exact_session_ids_locally(
        codex_dir,
        &discovery.related_paths,
        session_ids,
        rollout_paths,
    ))
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
    let discovery = discover_sqlite_databases(&codex_dir);
    ensure_sqlite_discovery_writable(&discovery)?;
    let relationship_sources = relationship_database_sources(&discovery, &selected)?;
    let roots = selected_session_roots(&relationship_sources, &selected)?;
    let expected_by_root = session_descendants_by_root(&relationship_sources, &roots)?;
    let expected_ids = expected_by_root
        .values()
        .flatten()
        .cloned()
        .collect::<HashSet<_>>();
    if discovery.thread_paths.is_empty() {
        return Err(CodexxError::Database(
            "无法确认当前活动会话库，已取消永久删除".to_string(),
        ));
    }
    let verification_ids = expected_ids.clone();
    let target_provider = current_model_provider(&codex_dir, None)?;
    let status_before =
        session_sync_status_with_discovery(&codex_dir, target_provider.clone(), &discovery)?;
    let storage_before = active_session_storage_snapshot(&codex_dir, &discovery.thread_paths)?;
    // Validate and collect every filesystem target before the official API can
    // make the deletion irreversible.
    let expected_rollout_paths =
        selected_rollout_paths(&codex_dir, &discovery.thread_paths, &expected_ids)?;
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
                        &discovery.related_paths,
                        cleanup_ids,
                        cleanup_rollout_paths,
                    ),
                );
            }
        }
        None => {
            merge_delete_counts(
                &mut counts,
                delete_exact_session_ids_locally(
                    &codex_dir,
                    &discovery.related_paths,
                    expected_ids,
                    expected_rollout_paths,
                ),
            );
        }
    }

    let remaining_ids = match active_session_ids_present(&discovery.thread_paths, &verification_ids)
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
    let status = match session_sync_status_with_discovery(&codex_dir, target_provider, &discovery) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_codex_dir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "codex-x-delete-verification-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test codex dir");
        path
    }

    fn create_thread_database(path: &Path, id: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create sqlite parent");
        }
        let conn = Connection::open(path).expect("create thread database");
        conn.execute(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL)",
            [],
        )
        .expect("create threads table");
        conn.execute(
            "INSERT INTO threads (id, model_provider) VALUES (?1, 'openai')",
            [id],
        )
        .expect("insert thread");
    }

    #[test]
    fn deletion_verification_checks_second_visible_database() {
        let codex_dir = temp_codex_dir();
        let active = codex_dir.join("state_10.sqlite");
        let second = codex_dir.join("sqlite/custom.db");
        let id = "019f6000-0000-7000-8000-000000000331";
        create_thread_database(&active, id);
        create_thread_database(&second, id);

        let discovery = discover_sqlite_databases(&codex_dir);
        assert_eq!(discovery.thread_paths.len(), 2);
        Connection::open(&active)
            .expect("open active database")
            .execute("DELETE FROM threads WHERE id = ?1", [id])
            .expect("delete only active copy");

        let ids = HashSet::from([id.to_string()]);
        let remaining = active_session_ids_present(&discovery.thread_paths, &ids)
            .expect("verify all visible databases");
        assert_eq!(remaining, ids);

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn descendant_edges_use_active_database_not_stale_legacy_copy() {
        let codex_dir = temp_codex_dir();
        let active = codex_dir.join("state_10.sqlite");
        let legacy = codex_dir.join("sqlite/state_5.sqlite");
        let parent = "019f6000-0000-7000-8000-000000000341";
        let child = "019f6000-0000-7000-8000-000000000342";
        let keep = "019f6000-0000-7000-8000-000000000343";
        create_thread_database(&active, parent);
        create_thread_database(&legacy, parent);

        for (path, descendant) in [(&active, child), (&legacy, keep)] {
            let conn = Connection::open(path).expect("open relationship database");
            conn.execute(
                "INSERT INTO threads (id, model_provider) VALUES (?1, 'openai')",
                [descendant],
            )
            .expect("insert descendant");
            conn.execute(
                "CREATE TABLE thread_spawn_edges (
                    parent_thread_id TEXT NOT NULL,
                    child_thread_id TEXT NOT NULL
                 )",
                [],
            )
            .expect("create spawn edges");
            conn.execute(
                "INSERT INTO thread_spawn_edges (parent_thread_id, child_thread_id)
                 VALUES (?1, ?2)",
                (parent, descendant),
            )
            .expect("insert spawn edge");
        }

        let discovery = discover_sqlite_databases(&codex_dir);
        let relationship_sources = relationship_database_sources(&discovery, &[parent.to_string()])
            .expect("resolve relationship source");
        assert_eq!(relationship_sources.get(parent), Some(&active));
        let descendants = session_descendants_by_root(&relationship_sources, &[parent.to_string()])
            .expect("resolve descendants");
        assert_eq!(
            descendants.get(parent),
            Some(&HashSet::from([parent.to_string(), child.to_string()]))
        );

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn secondary_only_session_uses_its_own_descendant_edges() {
        let codex_dir = temp_codex_dir();
        let active = codex_dir.join("state_10.sqlite");
        let secondary = codex_dir.join("sqlite/custom.db");
        let active_id = "019f6000-0000-7000-8000-000000000361";
        let parent = "019f6000-0000-7000-8000-000000000362";
        let child = "019f6000-0000-7000-8000-000000000363";
        create_thread_database(&active, active_id);
        create_thread_database(&secondary, parent);
        let conn = Connection::open(&secondary).expect("open secondary database");
        conn.execute(
            "INSERT INTO threads (id, model_provider) VALUES (?1, 'openai')",
            [child],
        )
        .expect("insert secondary child");
        conn.execute(
            "CREATE TABLE thread_spawn_edges (
                parent_thread_id TEXT NOT NULL,
                child_thread_id TEXT NOT NULL
             )",
            [],
        )
        .expect("create secondary edges");
        conn.execute(
            "INSERT INTO thread_spawn_edges (parent_thread_id, child_thread_id)
             VALUES (?1, ?2)",
            (parent, child),
        )
        .expect("insert secondary edge");
        drop(conn);

        let discovery = discover_sqlite_databases(&codex_dir);
        let sources = relationship_database_sources(&discovery, &[parent.to_string()])
            .expect("resolve secondary source");
        assert_eq!(sources.get(parent), Some(&secondary));
        let descendants = session_descendants_by_root(&sources, &[parent.to_string()])
            .expect("resolve secondary descendants");
        assert_eq!(
            descendants.get(parent),
            Some(&HashSet::from([parent.to_string(), child.to_string()]))
        );

        let _ = fs::remove_dir_all(codex_dir);
    }
}
