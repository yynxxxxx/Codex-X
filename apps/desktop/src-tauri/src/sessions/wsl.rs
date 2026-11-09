//! WSL SQLite must be read by Linux, where its byte-range/WAL locks work.
//! Only browsing/export use these disposable native SQLite backups. Provider
//! sync and deletion must never write a copied database back over the live one.
use super::types::{RolloutScan, SessionPreview, SessionSyncStatus};
use crate::error::{CodexxError, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

const SNAPSHOT_SCRIPT: &str = include_str!("wsl_snapshot.py");
const READ_ONLY_REASON: &str = "WSL 会话当前以只读方式显示，可查看和导出。Windows 无法安全取得 WSL 会话数据库的锁；同步和永久删除请在对应 WSL 发行版中操作。";

#[derive(Debug, PartialEq, Eq)]
struct WslTarget {
    unc_server: String,
    distribution: String,
    linux_home: String,
}

fn normalized_unc_path(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    if normalized.to_ascii_lowercase().starts_with("//?/unc/") {
        format!("//{}", &normalized[8..])
    } else {
        normalized
    }
}

// Guard writes by server identity alone. A malformed or aliased target must
// fail the strict preview parser rather than fall back to Windows SQLite.
fn has_wsl_server(path: &Path) -> bool {
    let normalized = normalized_unc_path(path);
    normalized
        .strip_prefix("//")
        .and_then(|unc| unc.split('/').next())
        .is_some_and(|server| {
            server.eq_ignore_ascii_case("wsl.localhost") || server.eq_ignore_ascii_case("wsl$")
        })
}

fn wsl_target(path: &Path) -> Option<WslTarget> {
    let normalized = normalized_unc_path(path);
    let mut parts = normalized.strip_prefix("//")?.split('/');
    let server = parts.next()?;
    if !server.eq_ignore_ascii_case("wsl.localhost") && !server.eq_ignore_ascii_case("wsl$") {
        return None;
    }
    let distribution = parts.next()?;
    if distribution.is_empty()
        || matches!(distribution, "." | "..")
        || distribution.starts_with('-')
        || distribution
            .chars()
            .any(|character| character.is_control() || character == ':')
    {
        return None;
    }
    let components = parts.filter(|part| !part.is_empty()).collect::<Vec<_>>();
    if components.is_empty()
        || components
            .iter()
            .any(|part| matches!(*part, "." | "..") || part.chars().any(char::is_control))
    {
        return None;
    }
    Some(WslTarget {
        unc_server: server.to_string(),
        distribution: distribution.to_string(),
        linux_home: format!("/{}", components.join("/")),
    })
}

pub(super) fn is_wsl_path(path: &Path) -> bool {
    cfg!(target_os = "windows") && selected_wsl_directory(path).is_some()
}

fn selected_wsl_directory(path: &Path) -> Option<PathBuf> {
    if has_wsl_server(path) {
        return Some(path.to_path_buf());
    }
    // A parent directory can be a junction even when CODEX_HOME itself is a
    // normal directory. Resolve the whole existing path, not just its last item.
    #[cfg(target_os = "windows")]
    {
        path.canonicalize()
            .ok()
            .filter(|canonical| has_wsl_server(canonical))
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

pub(super) fn ensure_writable(codex_dir: &Path) -> Result<()> {
    if is_wsl_path(codex_dir) {
        return Err(CodexxError::Config(READ_ONLY_REASON.into()));
    }
    Ok(())
}

#[derive(Deserialize)]
struct SnapshotManifest {
    databases: Vec<String>,
    active: Option<String>,
}

fn snapshot_path(directory: &Path, name: &str) -> Result<PathBuf> {
    let valid_name = name
        .strip_prefix("snapshot-")
        .and_then(|name| name.strip_suffix(".sqlite"))
        .is_some_and(|index| !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit()));
    if !valid_name || Path::new(name).components().count() != 1 || name.contains(['/', '\\']) {
        return Err(CodexxError::Config("WSL 返回了无效的会话快照路径".into()));
    }
    let path = directory.join(name);
    let metadata =
        std::fs::symlink_metadata(&path).map_err(|error| crate::file_io::io_err(&path, error))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(CodexxError::Config("WSL 会话快照不是普通文件".into()));
    }
    Ok(path)
}

fn linux_path_for_windows(target: &WslTarget, value: &str) -> String {
    if value.starts_with('/') && !value.starts_with("//") {
        format!(
            "\\\\{}\\{}{}",
            target.unc_server,
            target.distribution,
            value.replace('/', "\\")
        )
    } else {
        value.to_string()
    }
}

pub(super) struct WslSessions {
    pub(super) sessions: Vec<SessionPreview>,
    sqlite_dbs: usize,
    sqlite_threads: usize,
    subagent_threads: usize,
}

pub(super) fn session_previews(
    codex_dir: &Path,
    target_provider: &str,
    limit: usize,
) -> Result<WslSessions> {
    let selected_directory = selected_wsl_directory(codex_dir)
        .ok_or_else(|| CodexxError::Config("WSL 数据目录格式无效".into()))?;
    let target = wsl_target(&selected_directory)
        .ok_or_else(|| CodexxError::Config("WSL 数据目录格式无效".into()))?;
    let temporary = tempfile::Builder::new()
        .prefix("codex-x-wsl-read-")
        .tempdir()
        .map_err(|error| crate::file_io::io_err(codex_dir, error))?;
    let output_directory = temporary.path().to_string_lossy().into_owned();
    let mut arguments = vec![
        "--distribution",
        target.distribution.as_str(),
        "--exec",
        "python3",
        "-I",
        "-c",
        SNAPSHOT_SCRIPT,
        "--home",
        target.linux_home.as_str(),
        "--output",
        output_directory.as_str(),
        "--windows-output",
    ];
    let configured = std::fs::read_to_string(codex_dir.join("config.toml"))
        .ok()
        .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
        .and_then(|document| crate::string_value(&document, "sqlite_home"));
    if let Some(configured) = configured.as_deref() {
        arguments.extend(["--sqlite-home", configured]);
    }
    let output = crate::platform::desktop_command_output(
        Path::new("wsl.exe"),
        &arguments,
        Duration::from_secs(20),
    )
    .map_err(|_| {
        CodexxError::Config("读取 WSL 会话超时。请确认发行版已启动，然后重新检查会话。".into())
    })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(CodexxError::Config(format!("无法在 WSL 中读取会话。请在 {} 中确认 python3 可用（Ubuntu/Debian 可运行 sudo apt install python3），然后重试。{}", target.distribution, detail.trim().chars().take(600).collect::<String>())));
    }
    let manifest: SnapshotManifest = serde_json::from_slice(&output.stdout)
        .map_err(|_| CodexxError::Config("无法解析 WSL 会话快照结果".into()))?;
    let databases = manifest
        .databases
        .iter()
        .map(|name| snapshot_path(temporary.path(), name))
        .collect::<Result<Vec<_>>>()?;
    let active = match manifest.active {
        Some(name) if manifest.databases.contains(&name) => {
            vec![snapshot_path(temporary.path(), &name)?]
        }
        Some(_) => {
            return Err(CodexxError::Config(
                "WSL 活动会话快照不在已验证列表中".into(),
            ))
        }
        None => Vec::new(),
    };
    let (mut sessions, warnings) = super::storage::list_session_previews_with_paths(
        &active,
        &RolloutScan::default(),
        target_provider,
        limit,
    )?;
    if !warnings.is_empty() {
        return Err(CodexxError::Config(warnings.join("\n")));
    }
    super::names::apply_display_names(codex_dir, &databases, &mut sessions);
    let (sqlite_threads, subagent_threads) = if let Some(path) = active.first() {
        let connection =
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|error| CodexxError::Database(error.to_string()))?;
        let total = connection
            .query_row("SELECT COUNT(*) FROM threads", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|error| CodexxError::Database(error.to_string()))?
            as usize;
        let columns = crate::sqlite_utils::table_column_set(&connection, "threads")?;
        (
            total,
            super::storage::sqlite_subagent_thread_ids(&connection, &columns)?.len(),
        )
    } else {
        (0, 0)
    };
    for session in &mut sessions {
        session.needs_sync = false;
        if let Some(path) = session.rollout_path.as_mut() {
            *path = linux_path_for_windows(&target, path);
        }
    }
    Ok(WslSessions {
        sessions,
        sqlite_dbs: active.len(),
        sqlite_threads,
        subagent_threads,
    })
}

pub(super) fn session_status(
    codex_dir: &Path,
    target_provider: String,
) -> Result<SessionSyncStatus> {
    let WslSessions {
        sessions,
        sqlite_dbs,
        sqlite_threads,
        subagent_threads,
    } = session_previews(codex_dir, &target_provider, 1000)?;
    let top_level_threads = sqlite_threads.saturating_sub(subagent_threads);
    let (directory_identities, pin_scope_key) =
        super::directory_identity::session_directory_identities(codex_dir, &sessions);
    Ok(SessionSyncStatus {
        codex_dir: codex_dir.display().to_string(),
        directory_identities,
        pin_scope_key: Some(pin_scope_key),
        target_provider,
        rollout_files: sessions
            .iter()
            .filter(|session| session.rollout_path.is_some())
            .count(),
        session_meta_count: 0,
        mismatched_rollouts: 0,
        mismatched_session_meta: 0,
        sqlite_dbs,
        sqlite_threads,
        top_level_threads,
        subagent_threads,
        mismatched_threads: 0,
        mismatched_sessions: 0,
        needs_sync: false,
        scan_complete: true,
        read_only_reason: Some(READ_ONLY_REASON.into()),
        scan_failures: Vec::new(),
        backup_dir: None,
        warnings: vec![READ_ONLY_REASON.into()],
        sessions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_unc_paths_are_scoped_to_one_distribution_and_preserve_case_and_spaces() {
        for path in [
            r"\\wsl.localhost\Ubuntu-24.04\home\Some User\.codex",
            r"\\wsl$\Ubuntu-24.04\home\Some User\.codex",
            r"\\?\UNC\wsl.localhost\Ubuntu-24.04\home\Some User\.codex",
        ] {
            let target = wsl_target(Path::new(path)).unwrap();
            assert_eq!(target.distribution, "Ubuntu-24.04");
            assert_eq!(target.linux_home, "/home/Some User/.codex");
            assert_eq!(
                linux_path_for_windows(&target, "/home/User/.codex/sessions/thread.jsonl"),
                format!(
                    r"\\{}\Ubuntu-24.04\home\User\.codex\sessions\thread.jsonl",
                    target.unc_server
                )
            );
        }
        for path in [
            r"D:\Data\.codex",
            r"\\server\Ubuntu\home\User\.codex",
            r"\\wsl$\Ubuntu\home\..\.codex",
            r"\\wsl$\--exec\home\.codex",
            r"\\wsl$\Ubuntu",
        ] {
            assert!(wsl_target(Path::new(path)).is_none(), "{path}");
        }
    }

    #[test]
    fn snapshot_manifest_cannot_escape_the_private_directory() {
        let directory = tempfile::tempdir().unwrap();
        for name in [
            "../snapshot-0.sqlite",
            r"..\snapshot-0.sqlite",
            "D:/snapshot-0.sqlite",
            "snapshot-nested/file.sqlite",
            "snapshot-0.sqlite:stream.sqlite",
        ] {
            assert!(snapshot_path(directory.path(), name).is_err());
        }
        std::fs::write(directory.path().join("snapshot-0.sqlite"), b"fixture").unwrap();
        assert!(snapshot_path(directory.path(), "snapshot-0.sqlite").is_ok());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn wsl_mutations_fail_before_creating_locks_or_opening_databases() {
        for root in [
            r"\\wsl.localhost\Nonexistent-Fixture\home\fixture\.codex",
            r"\\wsl.localhost\Nonexistent-Fixture\home\fixture\.\.codex",
            r"\\wsl$\Nonexistent-Fixture\home\fixture\parent\..\.codex",
            r"\\?\UNC\wsl.localhost\Nonexistent-Fixture\home\fixture\..\.codex",
        ] {
            let root = Path::new(root);
            assert!(ensure_writable(root)
                .unwrap_err()
                .to_string()
                .contains("只读"));
            assert!(super::super::sync::acquire_session_maintenance_lock(root)
                .err()
                .unwrap()
                .to_string()
                .contains("只读"));
        }
    }

    #[test]
    fn invalid_wsl_targets_stay_identified_as_wsl_for_write_guards() {
        for path in [
            r"\\wsl.localhost\Ubuntu\home\fixture\.\.codex",
            r"\\wsl$\Ubuntu\home\fixture\parent\..\.codex",
            r"\\?\UNC\WSL.LOCALHOST\Ubuntu\home\fixture\..\.codex",
            r"\\wsl$\--exec\home\.codex",
            r"\\wsl$\Ubuntu",
        ] {
            assert!(has_wsl_server(Path::new(path)), "{path}");
            assert!(wsl_target(Path::new(path)).is_none(), "{path}");
        }
        assert!(!has_wsl_server(Path::new(
            r"\\wsl.localhost.example\share\.codex"
        )));
    }
}
