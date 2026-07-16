use crate::constants::AGENTS_FILENAME;
use crate::error::Result;
use crate::file_io::{ensure_directory, io_err, write_json};
use crate::paths::app_home;
use crate::prompts::agents_path;
use crate::{auth_path, config_path};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BackupMeta {
    pub(crate) id: String,
    pub(crate) action: String,
    pub(crate) created_at: String,
    pub(crate) codex_dir: String,
    pub(crate) config_path: String,
    pub(crate) auth_path: String,
    pub(crate) had_config: bool,
    pub(crate) had_auth: bool,
    #[serde(default)]
    pub(crate) agents_path: String,
    #[serde(default)]
    pub(crate) had_agents: bool,
    #[serde(default)]
    pub(crate) tracks_agents: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BackupEntry {
    id: String,
    action: String,
    created_at: String,
    path: String,
    had_config: bool,
    had_auth: bool,
    had_agents: bool,
}

fn backup_root() -> Result<PathBuf> {
    Ok(app_home()?.join("backups"))
}

pub(crate) fn action_backup_root(codex_dir: &Path) -> Result<PathBuf> {
    #[cfg(test)]
    {
        Ok(codex_dir.join(".codexx-test-backups"))
    }
    #[cfg(not(test))]
    {
        let _ = codex_dir;
        backup_root()
    }
}

pub(crate) fn create_backup(codex_dir: &Path, action: &str) -> Result<Option<String>> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static BACKUP_COUNTER: AtomicU64 = AtomicU64::new(0);
    let cfg = config_path(codex_dir);
    let auth = auth_path(codex_dir);
    let agents = agents_path(codex_dir);
    let had_config = cfg.exists();
    let had_auth = auth.exists();
    let had_agents = agents.exists();

    let id = format!(
        "{}-{}-{}",
        Local::now().format("%Y%m%d-%H%M%S-%3f"),
        BACKUP_COUNTER.fetch_add(1, Ordering::Relaxed),
        action
    );
    let dir = action_backup_root(codex_dir)?.join(&id);
    ensure_directory(&dir)?;

    if had_config {
        fs::copy(&cfg, dir.join("config.toml")).map_err(|e| io_err(&cfg, e))?;
    }
    if had_auth {
        fs::copy(&auth, dir.join("auth.json")).map_err(|e| io_err(&auth, e))?;
    }
    if had_agents {
        fs::copy(&agents, dir.join(AGENTS_FILENAME)).map_err(|e| io_err(&agents, e))?;
    }

    let meta = BackupMeta {
        id: id.clone(),
        action: action.to_string(),
        created_at: Local::now().to_rfc3339(),
        codex_dir: codex_dir.display().to_string(),
        config_path: cfg.display().to_string(),
        auth_path: auth.display().to_string(),
        had_config,
        had_auth,
        agents_path: agents.display().to_string(),
        had_agents,
        tracks_agents: true,
    };
    write_json(
        &dir.join("meta.json"),
        &serde_json::to_value(meta).expect("meta serialize"),
    )?;
    Ok(Some(id))
}

fn read_backup_entry(dir: &Path) -> Option<BackupEntry> {
    let meta_path = dir.join("meta.json");
    let text = fs::read_to_string(&meta_path).ok()?;
    let meta: BackupMeta = serde_json::from_str(&text).ok()?;
    Some(BackupEntry {
        id: meta.id,
        action: meta.action,
        created_at: meta.created_at,
        path: dir.display().to_string(),
        had_config: meta.had_config,
        had_auth: meta.had_auth,
        had_agents: meta.had_agents,
    })
}

pub(crate) fn backups() -> Result<Vec<BackupEntry>> {
    let root = backup_root()?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(&root).map_err(|e| io_err(&root, e))? {
        let entry = entry.map_err(|e| io_err(&root, e))?;
        let path = entry.path();
        if path.is_dir() {
            if let Some(backup) = read_backup_entry(&path) {
                entries.push(backup);
            }
        }
    }
    entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(entries)
}

pub(crate) fn latest_backup() -> Result<Option<BackupEntry>> {
    Ok(backups()?.into_iter().next())
}
