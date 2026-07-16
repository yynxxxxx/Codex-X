use crate::error::{CodexxError, Result};
use crate::file_io::{ensure_directory, io_err, write_json};
use chrono::Local;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub(super) struct BackupSnapshot {
    live_path: PathBuf,
    backup_path: PathBuf,
    existed: bool,
}

#[derive(Debug)]
pub(super) struct ProviderSyncBackup {
    pub(super) dir: PathBuf,
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

fn copy_file_to_backup(
    codex_dir: &Path,
    backup_dir: &Path,
    source: &Path,
) -> Result<BackupSnapshot> {
    let target = backup_target_path(codex_dir, backup_dir, source)?;
    let existed = source.exists();
    if existed {
        if let Some(parent) = target.parent() {
            ensure_directory(parent)?;
        }
        fs::copy(source, &target).map_err(|e| io_err(&target, e))?;
    }
    Ok(BackupSnapshot {
        live_path: source.to_path_buf(),
        backup_path: target,
        existed,
    })
}

pub(crate) fn backup_sqlite_to_backup(
    codex_dir: &Path,
    backup_dir: &Path,
    source: &Path,
) -> Result<()> {
    use rusqlite::backup::{Backup, StepResult};

    if !source.exists() {
        return Err(CodexxError::Database(format!(
            "SQLite 快照源不存在: {}",
            source.display()
        )));
    }
    let target = backup_target_path(codex_dir, backup_dir, source)?;
    if let Some(parent) = target.parent() {
        ensure_directory(parent)?;
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
    if !target.is_file() {
        return Err(CodexxError::Database(format!(
            "SQLite 快照未生成: {}",
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

pub(super) fn create_provider_sync_backup(
    codex_dir: &Path,
    target_provider: &str,
    changed_rollouts: &[PathBuf],
    sqlite_paths: &[PathBuf],
) -> Result<ProviderSyncBackup> {
    let root = provider_sync_backup_root(codex_dir);
    ensure_directory(&root)?;
    let mut backup_dir = root.join(Local::now().format("%Y%m%d%H%M%S").to_string());
    let mut suffix = 0;
    while backup_dir.exists() {
        suffix += 1;
        backup_dir = root.join(format!("{}-{suffix}", Local::now().format("%Y%m%d%H%M%S")));
    }
    ensure_directory(&backup_dir)?;

    let mut snapshots = Vec::new();
    for path in sqlite_paths {
        backup_sqlite_to_backup(codex_dir, &backup_dir, path)?;
        snapshots.push(BackupSnapshot {
            live_path: path.clone(),
            backup_path: backup_target_path(codex_dir, &backup_dir, path)?,
            existed: path.exists(),
        });
    }
    for name in [
        "config.toml",
        ".codex-global-state.json",
        ".codex-global-state.json.bak",
    ] {
        snapshots.push(copy_file_to_backup(
            codex_dir,
            &backup_dir,
            &codex_dir.join(name),
        )?);
    }
    for path in changed_rollouts {
        snapshots.push(copy_file_to_backup(codex_dir, &backup_dir, path)?);
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
            "snapshots": snapshots.iter().map(|snapshot| json!({
                "livePath": snapshot.live_path.display().to_string(),
                "backupPath": snapshot.backup_path.display().to_string(),
                "existed": snapshot.existed,
            })).collect::<Vec<_>>(),
        }),
    )?;
    Ok(ProviderSyncBackup { dir: backup_dir })
}
