use super::app_server::delete_sessions_via_codex_app_server;
use super::rollout_stream::{
    for_each_jsonl_record, record_matches_top_level_strings, resolved_rollout_path,
};
use super::storage::{
    current_model_provider, discover_sqlite_databases, ensure_sqlite_discovery_writable,
    is_canonical_rollout_storage_path, rollout_filename_matches_id, scan_rollouts,
    sqlite_subagent_thread_ids, sqlite_thread_needs_alignment, SqliteDiscovery,
    SqliteThreadIndexState,
};
use super::sync::{acquire_session_maintenance_lock, session_sync_status_with_discovery};
use super::types::SessionSyncStatus;
use crate::error::{CodexxError, Result};
use crate::file_io::io_err;
use crate::resolve_codex_dir;
use crate::sqlite_utils::{sql_select_column, sqlite_has_table, table_column_set};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tempfile::NamedTempFile;

#[cfg(test)]
use super::storage::hash_rollout_file;

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
            let archived_col = sql_select_column(&cols, "archived", "0");
            let query =
                format!("SELECT id, model_provider, {cwd_col}, {archived_col} FROM threads");
            let mut mismatch_stmt = conn
                .prepare(&query)
                .map_err(|error| CodexxError::Database(error.to_string()))?;
            let mismatches = mismatch_stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })
                .map_err(|error| CodexxError::Database(error.to_string()))?;
            for row in mismatches {
                let (id, provider, cwd, archived) =
                    row.map_err(|error| CodexxError::Database(error.to_string()))?;
                if sqlite_thread_needs_alignment(
                    &rollouts,
                    &target_provider,
                    &SqliteThreadIndexState {
                        thread_id: &id,
                        provider: provider.as_deref(),
                        cwd: cwd.as_deref(),
                        cwd_column: cols.contains("cwd"),
                        archived: archived != 0,
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

fn canonical_rollout_path(codex_dir: &Path, value: &str, id: &str) -> Result<Option<PathBuf>> {
    let raw = PathBuf::from(value.trim());
    let path = if raw.is_absolute() {
        raw
    } else {
        codex_dir.join(raw)
    };
    let path = match resolved_rollout_path(&path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_err(&path, error)),
    };
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

const CLEANUP_IO_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct CleanupFileStamp {
    len: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    unix_identity: Option<(u64, u64)>,
}

impl CleanupFileStamp {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        let unix_identity = {
            use std::os::unix::fs::MetadataExt;
            Some((metadata.dev(), metadata.ino()))
        };
        #[cfg(not(unix))]
        let unix_identity = None;
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
            unix_identity,
        }
    }
}

struct CleanupHashingReader<'a> {
    source: &'a mut fs::File,
    backup: Option<&'a mut fs::File>,
    digest: &'a mut Sha256,
    consumed: u64,
}

impl Read for CleanupHashingReader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let count = self.source.read(bytes)?;
        if let Some(backup) = self.backup.as_mut() {
            backup.write_all(&bytes[..count])?;
        }
        self.digest.update(&bytes[..count]);
        self.consumed += count as u64;
        Ok(count)
    }
}

fn copy_cleanup_bytes(reader: &mut impl Read, writer: &mut impl Write) -> std::io::Result<u64> {
    let mut bytes = [0u8; CLEANUP_IO_BUFFER_BYTES];
    let mut copied = 0u64;
    loop {
        let count = reader.read(&mut bytes)?;
        if count == 0 {
            return Ok(copied);
        }
        writer.write_all(&bytes[..count])?;
        copied += count as u64;
    }
}

fn filter_cleanup_records<R: Read, W: Write>(
    reader: R,
    writer: &mut W,
    path: &Path,
    id_keys: &[&str],
    session_ids: &HashSet<String>,
) -> Result<usize> {
    let mut removed = 0usize;
    for_each_jsonl_record(reader, |record, _| {
        record
            .seek(SeekFrom::Start(0))
            .map_err(|error| io_err(path, error))?;
        let matches = if session_ids.is_empty() {
            false
        } else {
            match record_matches_top_level_strings(record, id_keys, session_ids, 128) {
                Ok(matches) => matches,
                Err(error @ CodexxError::Io { .. }) => return Err(error),
                // Invalid JSON is preserved exactly as in the previous filter.
                Err(_) => false,
            }
        };
        if matches {
            removed += 1;
        } else {
            record
                .seek(SeekFrom::Start(0))
                .map_err(|error| io_err(path, error))?;
            copy_cleanup_bytes(record, writer).map_err(|error| io_err(path, error))?;
        }
        Ok(())
    })?;
    Ok(removed)
}

struct PreparedJsonlCleanup {
    output: NamedTempFile,
    original_backup: Option<NamedTempFile>,
    original_stamp: CleanupFileStamp,
    original_hash: [u8; 32],
    removed: usize,
}

fn cleanup_source_changed(path: &Path) -> CodexxError {
    CodexxError::Config(format!(
        "会话索引或历史记录在清理期间发生变化，已取消替换: {}",
        path.display()
    ))
}

fn hash_open_cleanup_file(path: &Path, source: &fs::File) -> Result<[u8; 32]> {
    // Read the existing handle: an exclusive history lock can reject a second
    // handle's read on Windows, even when it is opened by this same process.
    let mut reader = source;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| io_err(path, error))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; CLEANUP_IO_BUFFER_BYTES];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| io_err(path, error))?;
        if count == 0 {
            return Ok(digest.finalize().into());
        }
        digest.update(&buffer[..count]);
    }
}

fn ensure_cleanup_source_unchanged(
    path: &Path,
    source: &fs::File,
    stamp: &CleanupFileStamp,
    hash: &[u8; 32],
) -> Result<()> {
    let source_stamp = || -> Result<CleanupFileStamp> {
        source
            .metadata()
            .map(|metadata| CleanupFileStamp::from_metadata(&metadata))
            .map_err(|error| io_err(path, error))
    };
    let path_stamp = || -> Result<CleanupFileStamp> {
        fs::metadata(path)
            .map(|metadata| CleanupFileStamp::from_metadata(&metadata))
            .map_err(|error| io_err(path, error))
    };
    if &source_stamp()? != stamp || &path_stamp()? != stamp {
        return Err(cleanup_source_changed(path));
    }
    if &hash_open_cleanup_file(path, source)? != hash
        || &source_stamp()? != stamp
        || &path_stamp()? != stamp
    {
        return Err(cleanup_source_changed(path));
    }
    Ok(())
}

fn prepare_jsonl_cleanup(
    path: &Path,
    source: &mut fs::File,
    id_keys: &[&str],
    session_ids: &HashSet<String>,
    retain_backup: bool,
) -> Result<PreparedJsonlCleanup> {
    let metadata = source.metadata().map_err(|error| io_err(path, error))?;
    if !metadata.is_file() {
        return Err(CodexxError::Config(format!(
            "会话日志不是普通文件: {}",
            path.display()
        )));
    }
    let original_stamp = CleanupFileStamp::from_metadata(&metadata);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut output = NamedTempFile::new_in(parent).map_err(|error| io_err(path, error))?;
    let mut original_backup = if retain_backup {
        Some(NamedTempFile::new_in(parent).map_err(|error| io_err(path, error))?)
    } else {
        None
    };
    source
        .seek(SeekFrom::Start(0))
        .map_err(|error| io_err(path, error))?;
    let mut digest = Sha256::new();
    let mut reader = CleanupHashingReader {
        source: &mut *source,
        backup: original_backup.as_mut().map(NamedTempFile::as_file_mut),
        digest: &mut digest,
        consumed: 0,
    };
    let removed = filter_cleanup_records(
        &mut reader,
        output.as_file_mut(),
        path,
        id_keys,
        session_ids,
    )?;
    let consumed = reader.consumed;
    drop(reader);
    if consumed != original_stamp.len
        || CleanupFileStamp::from_metadata(&source.metadata().map_err(|error| io_err(path, error))?)
            != original_stamp
    {
        return Err(cleanup_source_changed(path));
    }
    output
        .as_file()
        .sync_all()
        .map_err(|error| io_err(path, error))?;
    if let Some(backup) = original_backup.as_ref() {
        backup
            .as_file()
            .sync_all()
            .map_err(|error| io_err(path, error))?;
    }
    let original_hash = digest.finalize().into();
    ensure_cleanup_source_unchanged(path, source, &original_stamp, &original_hash)?;
    Ok(PreparedJsonlCleanup {
        output,
        original_backup,
        original_stamp,
        original_hash,
        removed,
    })
}

fn preflight_session_jsonl_cleanup(codex_dir: &Path) -> Result<()> {
    // Framing and staging use fixed memory even for a very large valid history.
    // Read/write failures still occur before the irreversible deletion API.
    for filename in ["session_index.jsonl", "history.jsonl"] {
        let path = codex_dir.join(filename);
        let mut source = match fs::File::open(&path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io_err(&path, error)),
        };
        prepare_jsonl_cleanup(&path, &mut source, &[], &HashSet::new(), false)?;
    }
    Ok(())
}

fn commit_index_cleanup(
    path: &Path,
    source: fs::File,
    prepared: PreparedJsonlCleanup,
) -> Result<usize> {
    if prepared.removed == 0 {
        return Ok(0);
    }
    ensure_cleanup_source_unchanged(
        path,
        &source,
        &prepared.original_stamp,
        &prepared.original_hash,
    )?;
    let permissions = source
        .metadata()
        .map_err(|error| io_err(path, error))?
        .permissions();
    prepared
        .output
        .as_file()
        .set_permissions(permissions)
        .map_err(|error| {
            io_err(
                path,
                std::io::Error::new(error.kind(), format!("设置临时会话索引权限失败: {error}")),
            )
        })?;
    // tempfile's Windows persist uses SetFileAttributesW followed by MoveFileExW.
    // Close both our destination and staging handles before that replacement;
    // the index has no history-style inode/append-lock requirement.
    drop(source);
    let output = prepared.output.into_temp_path();
    let verifier = fs::File::open(path).map_err(|error| {
        io_err(
            path,
            std::io::Error::new(error.kind(), format!("最终复核会话索引失败: {error}")),
        )
    })?;
    let unchanged = ensure_cleanup_source_unchanged(
        path,
        &verifier,
        &prepared.original_stamp,
        &prepared.original_hash,
    );
    drop(verifier);
    unchanged?;
    output.persist(path).map_err(|error| {
        io_err(
            path,
            std::io::Error::new(
                error.error.kind(),
                format!("原子替换会话索引失败: {}", error.error),
            ),
        )
    })?;
    Ok(prepared.removed)
}

fn remove_jsonl_session_entries(
    path: &Path,
    id_keys: &[&str],
    session_ids: &HashSet<String>,
) -> Result<usize> {
    let mut source = match fs::File::open(path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(io_err(path, error)),
    };
    let prepared = prepare_jsonl_cleanup(path, &mut source, id_keys, session_ids, false)?;
    commit_index_cleanup(path, source, prepared)
}

fn remove_session_index_entries(codex_dir: &Path, session_ids: &HashSet<String>) -> Result<usize> {
    remove_jsonl_session_entries(&codex_dir.join("session_index.jsonl"), &["id"], session_ids)
}

fn restore_history_backup(
    path: &Path,
    source: &mut fs::File,
    backup: &mut fs::File,
    original_len: u64,
    original_hash: &[u8; 32],
) -> Result<()> {
    backup
        .seek(SeekFrom::Start(0))
        .map_err(|error| io_err(path, error))?;
    source
        .seek(SeekFrom::Start(0))
        .map_err(|error| io_err(path, error))?;
    if copy_cleanup_bytes(backup, source).map_err(|error| io_err(path, error))? != original_len {
        return Err(CodexxError::Config(
            "历史记录备份不完整，无法安全回滚".into(),
        ));
    }
    // Filtering never grows the file. Replaying the complete backup restores its
    // original length after a failed truncate, and does not truncate a later
    // append beyond the original EOF from a writer that ignored the lock.
    source.sync_all().map_err(|error| io_err(path, error))?;
    source
        .seek(SeekFrom::Start(0))
        .map_err(|error| io_err(path, error))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; CLEANUP_IO_BUFFER_BYTES];
    let mut remaining = original_len;
    while remaining > 0 {
        let allowed = buffer.len().min(remaining as usize);
        let count = source
            .read(&mut buffer[..allowed])
            .map_err(|error| io_err(path, error))?;
        if count == 0 {
            return Err(CodexxError::Config("历史记录回滚后长度不足".into()));
        }
        digest.update(&buffer[..count]);
        remaining -= count as u64;
    }
    let restored: [u8; 32] = digest.finalize().into();
    if &restored != original_hash {
        return Err(CodexxError::Config("历史记录回滚校验失败".into()));
    }
    Ok(())
}

fn commit_history_cleanup_with_writer<F>(
    path: &Path,
    source: &mut fs::File,
    mut prepared: PreparedJsonlCleanup,
    write_output: F,
) -> Result<usize>
where
    F: FnOnce(&mut fs::File, &mut fs::File, u64, u64) -> Result<()>,
{
    if prepared.removed == 0 {
        return Ok(0);
    }
    ensure_cleanup_source_unchanged(
        path,
        source,
        &prepared.original_stamp,
        &prepared.original_hash,
    )?;
    let output_len = prepared
        .output
        .as_file()
        .metadata()
        .map_err(|error| io_err(path, error))?
        .len();
    if output_len > prepared.original_stamp.len {
        return Err(CodexxError::Config(
            "历史记录过滤结果异常，未写入原文件".into(),
        ));
    }
    let mut backup = prepared
        .original_backup
        .take()
        .ok_or_else(|| CodexxError::Config("历史记录缺少完整磁盘备份，未写入原文件".into()))?;
    if let Err(error) = write_output(
        source,
        prepared.output.as_file_mut(),
        output_len,
        prepared.original_stamp.len,
    ) {
        if let Err(restore_error) = restore_history_backup(
            path,
            source,
            backup.as_file_mut(),
            prepared.original_stamp.len,
            &prepared.original_hash,
        ) {
            return match backup.keep() {
                Ok((_, retained)) => Err(CodexxError::Config(format!(
                    "{error}；历史记录回滚失败：{restore_error}；已保留私有备份：{}",
                    retained.display()
                ))),
                Err(keep_error) => Err(CodexxError::Config(format!(
                    "{error}；历史记录回滚失败：{restore_error}；保留备份失败：{keep_error}"
                ))),
            };
        }
        return Err(error);
    }
    Ok(prepared.removed)
}

fn remove_session_history_entries(
    codex_dir: &Path,
    session_ids: &HashSet<String>,
) -> Result<usize> {
    let path = codex_dir.join("history.jsonl");
    let mut file = match fs::OpenOptions::new().read(true).write(true).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(io_err(&path, error)),
    };
    file.try_lock().map_err(|error| {
        CodexxError::Config(format!(
            "历史记录正在被其他 Codex 进程使用，请关闭相关 Codex 窗口或 CLI 后重试: {error}"
        ))
    })?;
    let result = (|| -> Result<usize> {
        let prepared = prepare_jsonl_cleanup(&path, &mut file, &["session_id"], session_ids, true)?;
        commit_history_cleanup_with_writer(
            &path,
            &mut file,
            prepared,
            |source, output, output_len, original_len| {
                output
                    .seek(SeekFrom::Start(0))
                    .map_err(|error| io_err(&path, error))?;
                source
                    .seek(SeekFrom::Start(0))
                    .map_err(|error| io_err(&path, error))?;
                copy_cleanup_bytes(output, source).map_err(|error| io_err(&path, error))?;
                if source
                    .metadata()
                    .map_err(|error| io_err(&path, error))?
                    .len()
                    != original_len
                {
                    return Err(cleanup_source_changed(&path));
                }
                source
                    .set_len(output_len)
                    .map_err(|error| io_err(&path, error))?;
                source.sync_all().map_err(|error| io_err(&path, error))?;
                Ok(())
            },
        )
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
    preflight_session_jsonl_cleanup(codex_dir)?;
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
    super::wsl::ensure_writable(&codex_dir)?;
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
    preflight_session_jsonl_cleanup(&codex_dir)?;
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
            let deleted_mismatched_sessions = fallback
                .sessions
                .iter()
                .filter(|item| item.needs_sync && deleted_active_ids.contains(&item.id))
                .count();
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
            fallback.mismatched_sessions = fallback
                .mismatched_sessions
                .saturating_sub(deleted_mismatched_sessions);
            fallback.needs_sync = fallback.mismatched_sessions > 0;
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
    fn deletion_target_resolves_compressed_logical_alias_without_selecting_another_history() {
        let codex_dir = temp_codex_dir();
        let id = "01a0e854-0000-7000-8000-000000000001";
        let logical = codex_dir.join(format!("sessions/rollout-current-{id}.jsonl"));
        fs::create_dir_all(logical.parent().unwrap()).unwrap();
        let compressed = logical.with_extension("jsonl.zst");
        fs::write(
            &compressed,
            zstd::stream::encode_all(&b"{}\n"[..], 1).unwrap(),
        )
        .unwrap();
        let found = canonical_rollout_path(&codex_dir, &logical.display().to_string(), id).unwrap();
        assert_eq!(found, Some(compressed.canonicalize().unwrap()));
        fs::write(&logical, b"{}\n").unwrap();
        let found = canonical_rollout_path(&codex_dir, &logical.display().to_string(), id).unwrap();
        assert_eq!(found, Some(logical.canonicalize().unwrap()));
        fs::remove_dir_all(codex_dir).unwrap();
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

    #[test]
    fn unreadable_cleanup_logs_cancel_native_and_local_deletion_before_any_mutation() {
        for filename in ["session_index.jsonl", "history.jsonl"] {
            let codex_dir = temp_codex_dir();
            let id = "019f6000-0000-7000-8000-000000000371";
            let database = codex_dir.join("state_10.sqlite");
            create_thread_database(&database, id);
            let rollout = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
            fs::create_dir_all(rollout.parent().unwrap()).unwrap();
            let rollout_text = format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"model_provider\":\"openai\"}}}}\n"
            );
            fs::write(&rollout, &rollout_text).unwrap();
            for (name, key) in [
                ("session_index.jsonl", "id"),
                ("history.jsonl", "session_id"),
            ] {
                fs::write(codex_dir.join(name), format!("{{\"{key}\":\"{id}\"}}\n")).unwrap();
            }
            let unreadable = codex_dir.join(filename);
            fs::remove_file(&unreadable).unwrap();
            fs::create_dir(&unreadable).unwrap();
            let database_before = fs::read(&database).unwrap();
            let untouched_name = if filename == "history.jsonl" {
                "session_index.jsonl"
            } else {
                "history.jsonl"
            };
            let untouched_path = codex_dir.join(untouched_name);
            let untouched_before = fs::read(&untouched_path).unwrap();

            let local_error = hard_delete_sessions_locally(&codex_dir, &[id.to_string()])
                .expect_err("local deletion must reject an unreadable cleanup log");
            assert!(local_error.to_string().contains(filename));
            let native_error = delete_codex_sessions_inner(SessionDeleteInput {
                config_dir: Some(codex_dir.display().to_string()),
                session_ids: vec![id.to_string()],
            })
            .expect_err("native deletion must stop at preflight before starting Codex");
            assert!(native_error.to_string().contains(filename));

            assert_eq!(fs::read(&rollout).unwrap(), rollout_text.as_bytes());
            assert_eq!(fs::read(&database).unwrap(), database_before);
            assert_eq!(fs::read(&untouched_path).unwrap(), untouched_before);
            assert!(unreadable.is_dir());
            let present =
                active_session_ids_present(&[database], &HashSet::from([id.to_string()])).unwrap();
            assert!(present.contains(id));
            fs::remove_dir_all(codex_dir).unwrap();
        }
    }

    #[test]
    fn large_cleanup_logs_stream_without_a_total_size_limit() {
        for filename in ["session_index.jsonl", "history.jsonl"] {
            let codex_dir = temp_codex_dir();
            let id = "019f6000-0000-7000-8000-000000000381";
            let database = codex_dir.join("state_10.sqlite");
            create_thread_database(&database, id);
            let rollout = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
            fs::create_dir_all(rollout.parent().unwrap()).unwrap();
            fs::write(&rollout, format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"model_provider\":\"openai\"}}}}\n")).unwrap();
            let path = codex_dir.join(filename);
            let key = if filename == "history.jsonl" {
                "session_id"
            } else {
                "id"
            };
            let remove_large_record = filename == "history.jsonl";
            let mut file = fs::File::create(&path).unwrap();
            let large_id = if remove_large_record { id } else { "kept" };
            let prefix = format!("{{\"{key}\":\"{large_id}\",\"text\":\"");
            let mut expected = Sha256::new();
            file.write_all(prefix.as_bytes()).unwrap();
            if !remove_large_record {
                expected.update(prefix.as_bytes());
            }
            let bytes = [b'x'; CLEANUP_IO_BUFFER_BYTES];
            for _ in 0..(33 * 1024 * 1024 / CLEANUP_IO_BUFFER_BYTES) {
                file.write_all(&bytes).unwrap();
                if !remove_large_record {
                    expected.update(bytes);
                }
            }
            file.write_all(b"\"}\r\n").unwrap();
            if !remove_large_record {
                expected.update(b"\"}\r\n");
                file.write_all(format!("{{\"{key}\":\"{id}\"}}\n").as_bytes())
                    .unwrap();
            }
            let tail = format!("not-json\r\n{{\"{key}\":\"{id}\",\"{key}\":\"kept-last\"}}\n{{\"{key}\":\"kept-final\"}}");
            file.write_all(tail.as_bytes()).unwrap();
            expected.update(tail.as_bytes());
            drop(file);
            assert!(fs::metadata(&path).unwrap().len() > 32 * 1024 * 1024);
            let other_name = if filename == "history.jsonl" {
                "session_index.jsonl"
            } else {
                "history.jsonl"
            };
            let other_key = if key == "id" { "session_id" } else { "id" };
            fs::write(
                codex_dir.join(other_name),
                format!("{{\"{other_key}\":\"{id}\"}}\r\n{{\"{other_key}\":\"untouched\"}}"),
            )
            .unwrap();
            preflight_session_jsonl_cleanup(&codex_dir)
                .expect("large valid logs must pass streaming preflight");
            let result = hard_delete_sessions_locally(&codex_dir, &[id.to_string()]).unwrap();
            assert!(result.errors.is_empty(), "{:?}", result.errors);
            assert!(!rollout.exists());
            assert!(
                !active_session_ids_present(&[database], &HashSet::from([id.to_string()]))
                    .unwrap()
                    .contains(id)
            );
            let expected_hash: [u8; 32] = expected.finalize().into();
            assert_eq!(hash_rollout_file(&path).unwrap(), expected_hash);
            assert_eq!(
                fs::read(codex_dir.join(other_name)).unwrap(),
                format!("{{\"{other_key}\":\"untouched\"}}").as_bytes()
            );
            fs::remove_dir_all(codex_dir).unwrap();
        }
    }

    #[test]
    fn streaming_cleanup_preserves_missing_files_and_normal_jsonl_filtering() {
        let codex_dir = temp_codex_dir();
        preflight_session_jsonl_cleanup(&codex_dir).unwrap();
        assert!(!codex_dir.join("session_index.jsonl").exists());
        assert!(!codex_dir.join("history.jsonl").exists());
        let ids = HashSet::from(["selected".to_string()]);
        for (filename, key) in [
            ("session_index.jsonl", "id"),
            ("history.jsonl", "session_id"),
        ] {
            fs::write(
                codex_dir.join(filename),
                format!("{{\"{key}\":\"selected\"}}\r\nnot-json\r\n{{\"{key}\":\"kept\"}}"),
            )
            .unwrap();
        }
        preflight_session_jsonl_cleanup(&codex_dir).unwrap();
        assert_eq!(remove_session_index_entries(&codex_dir, &ids).unwrap(), 1);
        assert_eq!(remove_session_history_entries(&codex_dir, &ids).unwrap(), 1);
        for (filename, key) in [
            ("session_index.jsonl", "id"),
            ("history.jsonl", "session_id"),
        ] {
            assert_eq!(
                fs::read_to_string(codex_dir.join(filename)).unwrap(),
                format!("not-json\r\n{{\"{key}\":\"kept\"}}")
            );
        }
        fs::remove_dir_all(codex_dir).unwrap();
    }

    #[test]
    fn streaming_cleanup_only_matches_valid_top_level_ids_and_preserves_other_bytes() {
        let codex_dir = temp_codex_dir();
        let path = codex_dir.join("session_index.jsonl");
        let kept = b"not-json\r\n{\"nested\":{\"id\":\"selected\"},\"id\":\"kept\"}\n{\"id\":\"selected\",\"id\":null}\r\n\xff\n";
        let mut original = kept.to_vec();
        original.extend_from_slice(b"{\"id\":\"selected\"}\r\n");
        original.extend_from_slice(b"{\"id\":\"last-kept\"}");
        fs::write(&path, original).unwrap();
        assert_eq!(
            remove_session_index_entries(&codex_dir, &HashSet::from(["selected".to_string()]))
                .unwrap(),
            1
        );
        let mut expected = kept.to_vec();
        expected.extend_from_slice(b"{\"id\":\"last-kept\"}");
        assert_eq!(fs::read(path).unwrap(), expected);
        fs::remove_dir_all(codex_dir).unwrap();
    }

    struct FailingCleanupRead<R> {
        inner: R,
        remaining: usize,
    }
    impl<R: Read> Read for FailingCleanupRead<R> {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                return Err(std::io::Error::other("injected cleanup read failure"));
            }
            let allowed = bytes.len().min(self.remaining);
            let count = self.inner.read(&mut bytes[..allowed])?;
            self.remaining -= count;
            Ok(count)
        }
    }

    struct FailingCleanupWrite<W> {
        inner: W,
        remaining: usize,
    }
    impl<W: Write> Write for FailingCleanupWrite<W> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                return Err(std::io::Error::other("injected cleanup write failure"));
            }
            let count = self
                .inner
                .write(&bytes[..bytes.len().min(self.remaining)])?;
            self.remaining -= count;
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }

    #[test]
    fn streaming_cleanup_read_or_staging_write_failure_preserves_the_source() {
        let codex_dir = temp_codex_dir();
        let path = codex_dir.join("session_index.jsonl");
        let original = b"{\"id\":\"kept\",\"text\":\"retained\"}\r\n{\"id\":\"selected\"}\n";
        fs::write(&path, original).unwrap();
        let ids = HashSet::from(["selected".to_string()]);
        let mut target = NamedTempFile::new_in(&codex_dir).unwrap();
        let error = filter_cleanup_records(
            FailingCleanupRead {
                inner: fs::File::open(&path).unwrap(),
                remaining: 7,
            },
            target.as_file_mut(),
            &path,
            &["id"],
            &ids,
        )
        .expect_err("read errors must propagate, not become a malformed record");
        assert!(matches!(error, CodexxError::Io { .. }));
        assert_eq!(fs::read(&path).unwrap(), original);
        let mut writer = FailingCleanupWrite {
            inner: target.as_file_mut(),
            remaining: 3,
        };
        let error = filter_cleanup_records(
            fs::File::open(&path).unwrap(),
            &mut writer,
            &path,
            &["id"],
            &ids,
        )
        .expect_err("a failed private output must never be installed");
        assert!(matches!(error, CodexxError::Io { .. }));
        assert_eq!(fs::read(&path).unwrap(), original);
        drop(target);
        fs::remove_dir_all(codex_dir).unwrap();
    }

    #[test]
    fn staged_cleanup_detects_growth_before_index_or_history_commit() {
        for history in [false, true] {
            let codex_dir = temp_codex_dir();
            let path = codex_dir.join(if history {
                "history.jsonl"
            } else {
                "session_index.jsonl"
            });
            let key = if history { "session_id" } else { "id" };
            fs::write(
                &path,
                format!("{{\"{key}\":\"selected\"}}\n{{\"{key}\":\"kept\"}}\n"),
            )
            .unwrap();
            let mut source = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            let prepared = prepare_jsonl_cleanup(
                &path,
                &mut source,
                &[key],
                &HashSet::from(["selected".to_string()]),
                history,
            )
            .unwrap();
            let mut external = fs::OpenOptions::new().append(true).open(&path).unwrap();
            external
                .write_all(format!("{{\"{key}\":\"new-append\"}}\n").as_bytes())
                .unwrap();
            external.sync_all().unwrap();
            drop(external);
            let grown = fs::read(&path).unwrap();
            let result = if history {
                let result = commit_history_cleanup_with_writer(
                    &path,
                    &mut source,
                    prepared,
                    |_, _, _, _| panic!("growth must be rejected before writing history"),
                );
                drop(source);
                result
            } else {
                commit_index_cleanup(&path, source, prepared)
            };
            assert!(result
                .expect_err("a stale staged result must not replace appended data")
                .to_string()
                .contains("发生变化"));
            assert_eq!(fs::read(&path).unwrap(), grown);
            fs::remove_dir_all(codex_dir).unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn index_replacement_closes_a_delete_denying_source_handle_and_preserves_all_kept_bytes() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

        let codex_dir = temp_codex_dir();
        let path = codex_dir.join("session_index.jsonl");
        fs::write(
            &path,
            b"{\"id\":\"selected\"}\r\nnot-json\r\n{\"id\":\"kept\"}",
        )
        .unwrap();
        let mut initial = fs::File::open(&path).unwrap();
        let prepared = prepare_jsonl_cleanup(
            &path,
            &mut initial,
            &["id"],
            &HashSet::from(["selected".to_string()]),
            false,
        )
        .unwrap();
        drop(initial);
        // Allow all verification reads while denying DELETE sharing, which is
        // specifically required for Windows rename/replacement operations.
        let source = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&path)
            .unwrap();
        assert_eq!(commit_index_cleanup(&path, source, prepared).unwrap(), 1);
        assert_eq!(fs::read(&path).unwrap(), b"not-json\r\n{\"id\":\"kept\"}");
        fs::remove_dir_all(codex_dir).unwrap();
    }

    #[test]
    fn index_replacement_rejects_same_length_modified_bytes_even_with_restored_mtime() {
        let codex_dir = temp_codex_dir();
        let path = codex_dir.join("session_index.jsonl");
        let original = b"{\"id\":\"selected\"}\n{\"id\":\"kept\"}\n";
        fs::write(&path, original).unwrap();
        let mut source = fs::File::open(&path).unwrap();
        let prepared = prepare_jsonl_cleanup(
            &path,
            &mut source,
            &["id"],
            &HashSet::from(["selected".to_string()]),
            false,
        )
        .unwrap();
        let mtime = prepared.original_stamp.modified.unwrap();
        let mut changed = original.to_vec();
        let last = changed.iter().rposition(|byte| *byte == b't').unwrap();
        changed[last] = b'x';
        let mut external = fs::OpenOptions::new().write(true).open(&path).unwrap();
        external.write_all(&changed).unwrap();
        external.sync_all().unwrap();
        drop(external);
        // Set the timestamp through a fresh handle after closing the data writer;
        // Windows may otherwise finalize a pending write time when it closes.
        let timestamp = fs::OpenOptions::new().write(true).open(&path).unwrap();
        timestamp
            .set_times(fs::FileTimes::new().set_modified(mtime))
            .unwrap();
        drop(timestamp);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), mtime);
        assert_eq!(fs::metadata(&path).unwrap().len(), original.len() as u64);
        let error = commit_index_cleanup(&path, source, prepared)
            .expect_err("a matching length and timestamp must not bypass the source digest");
        assert!(error.to_string().contains("发生变化"));
        assert_eq!(fs::read(&path).unwrap(), changed);
        fs::remove_dir_all(codex_dir).unwrap();
    }

    #[test]
    fn history_commit_write_failure_restores_backup_and_keeps_its_lock_and_inode() {
        for fail_after_truncate in [false, true] {
            let codex_dir = temp_codex_dir();
            let path = codex_dir.join("history.jsonl");
            let original = b"{\"session_id\":\"selected\",\"text\":\"private fixture\"}\r\n{\"session_id\":\"kept\"}";
            fs::write(&path, original).unwrap();
            let before = CleanupFileStamp::from_metadata(&fs::metadata(&path).unwrap());
            let mut source = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            source.try_lock().unwrap();
            let probe = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            assert!(probe.try_lock().is_err());
            let prepared = prepare_jsonl_cleanup(
                &path,
                &mut source,
                &["session_id"],
                &HashSet::from(["selected".to_string()]),
                true,
            )
            .unwrap();
            let error = commit_history_cleanup_with_writer(
                &path,
                &mut source,
                prepared,
                |source, output, output_len, _| {
                    source.seek(SeekFrom::Start(0)).unwrap();
                    output.seek(SeekFrom::Start(0)).unwrap();
                    if fail_after_truncate {
                        copy_cleanup_bytes(output, source).unwrap();
                        source.set_len(output_len).unwrap();
                    } else {
                        source.write_all(b"partial failed write").unwrap();
                    }
                    Err(io_err(
                        &path,
                        std::io::Error::other("injected commit write/sync failure"),
                    ))
                },
            )
            .expect_err("failed commit must report the failure after restoring the disk backup");
            assert!(error.to_string().contains("injected commit"));
            let expected_hash: [u8; 32] = Sha256::digest(original).into();
            assert_eq!(
                hash_open_cleanup_file(&path, &source).unwrap(),
                expected_hash
            );
            assert_eq!(source.metadata().unwrap().len(), original.len() as u64);
            assert_eq!(
                CleanupFileStamp::from_metadata(&source.metadata().unwrap()).unix_identity,
                before.unix_identity
            );
            assert!(
                probe.try_lock().is_err(),
                "rollback must not release the live history lock"
            );
            source.unlock().unwrap();
            assert!(probe.try_lock().is_ok());
            probe.unlock().unwrap();
            drop(probe);
            drop(source);
            assert_eq!(fs::read(&path).unwrap(), original);
            fs::remove_dir_all(codex_dir).unwrap();
        }
    }

    #[test]
    fn successful_history_cleanup_keeps_an_already_open_append_handle_usable() {
        let codex_dir = temp_codex_dir();
        let path = codex_dir.join("history.jsonl");
        fs::write(
            &path,
            b"{\"session_id\":\"selected\"}\n{\"session_id\":\"kept\"}\n",
        )
        .unwrap();
        let before = CleanupFileStamp::from_metadata(&fs::metadata(&path).unwrap());
        let mut old_append_handle = fs::OpenOptions::new()
            .read(true)
            .append(true)
            .open(&path)
            .unwrap();
        assert_eq!(
            remove_session_history_entries(&codex_dir, &HashSet::from(["selected".to_string()]))
                .unwrap(),
            1
        );
        old_append_handle.try_lock().unwrap();
        old_append_handle
            .write_all(b"{\"session_id\":\"later\"}\r\n")
            .unwrap();
        old_append_handle.unlock().unwrap();
        drop(old_append_handle);
        let after = CleanupFileStamp::from_metadata(&fs::metadata(&path).unwrap());
        assert_eq!(after.unix_identity, before.unix_identity);
        assert_eq!(
            fs::read(&path).unwrap(),
            b"{\"session_id\":\"kept\"}\n{\"session_id\":\"later\"}\r\n"
        );
        fs::remove_dir_all(codex_dir).unwrap();
    }
}
