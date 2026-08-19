use super::official_auth::{
    canonical_codex_identity, is_chatgpt_auth, validate_official_config_text,
};
use crate::error::{CodexxError, Result};
use crate::file_io::{ensure_directory, io_err, write_private_json};
use crate::paths::app_home;
use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const OFFICIAL_ACCOUNT_STORE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficialAccountStore {
    pub(crate) version: u32,
    pub(crate) codex_dir: String,
    pub(crate) selected_account_id: Option<String>,
    pub(crate) accounts: Vec<OfficialAccount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficialAccount {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) model: Option<String>,
    pub(crate) config: String,
    pub(crate) auth: Value,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) last_used_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficialAccountSummary {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) model: Option<String>,
    pub(crate) selected: bool,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) last_used_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficialAccountDraft {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) model: Option<String>,
    pub(crate) config_text: String,
    pub(crate) auth_json: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) last_used_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficialAccountUpdateInput {
    pub(crate) config_dir: Option<String>,
    pub(crate) account_id: String,
    pub(crate) name: String,
    pub(crate) model: Option<String>,
    pub(crate) config_text: String,
    pub(crate) auth_json: String,
}

pub(crate) fn official_accounts_path(codex_dir: &Path) -> Result<PathBuf> {
    let identity = canonical_codex_identity(codex_dir);
    let digest = Sha256::digest(identity.as_bytes());
    Ok(app_home()?
        .join("official-accounts")
        .join(format!("{digest:x}.json")))
}

pub(crate) fn empty_official_account_store(codex_dir: &Path) -> OfficialAccountStore {
    OfficialAccountStore {
        version: OFFICIAL_ACCOUNT_STORE_VERSION,
        codex_dir: canonical_codex_identity(codex_dir),
        selected_account_id: None,
        accounts: Vec::new(),
    }
}

fn invalid_store(path: &Path, message: &str) -> CodexxError {
    CodexxError::Config(format!(
        "OpenAI Official 账号存储无效（{message}），为避免凭据丢失已拒绝覆盖: {}",
        path.display()
    ))
}

pub(crate) fn validate_official_account(codex_dir: &Path, account: &OfficialAccount) -> Result<()> {
    if account.id.trim().is_empty() {
        return Err(CodexxError::Config("官方账号 ID 不能为空".to_string()));
    }
    if account.name.trim().is_empty() {
        return Err(CodexxError::Config("官方账号名称不能为空".to_string()));
    }
    let (sanitized_config, _) =
        validate_official_config_text(codex_dir, &account.config, account.model.as_deref())?;
    if sanitized_config != account.config {
        return Err(CodexxError::Config(
            "官方账号 config.toml 包含不允许保存的认证字段".to_string(),
        ));
    }
    if !account.auth.is_object() || !is_chatgpt_auth(&account.auth) {
        return Err(CodexxError::Config(
            "官方账号 auth.json 不是可信的 OpenAI Official 认证".to_string(),
        ));
    }
    Ok(())
}

fn validate_store(codex_dir: &Path, path: &Path, store: &OfficialAccountStore) -> Result<()> {
    if store.version != OFFICIAL_ACCOUNT_STORE_VERSION {
        return Err(invalid_store(path, "不支持的版本"));
    }
    if store.codex_dir != canonical_codex_identity(codex_dir) {
        return Err(invalid_store(path, "CODEX_HOME 不匹配"));
    }
    let mut ids = HashSet::new();
    for account in &store.accounts {
        if !ids.insert(account.id.as_str()) {
            return Err(invalid_store(path, "包含重复账号 ID"));
        }
        validate_official_account(codex_dir, account)
            .map_err(|_| invalid_store(path, "包含无效账号记录"))?;
    }
    if store
        .selected_account_id
        .as_deref()
        .is_some_and(|selected| !ids.contains(selected))
    {
        return Err(invalid_store(path, "当前账号 ID 不存在"));
    }
    Ok(())
}

pub(crate) fn load_official_account_store(codex_dir: &Path) -> Result<OfficialAccountStore> {
    let path = official_accounts_path(codex_dir)?;
    if !path.is_file() {
        return Ok(empty_official_account_store(codex_dir));
    }
    let text = fs::read_to_string(&path).map_err(|error| io_err(&path, error))?;
    let store: OfficialAccountStore =
        serde_json::from_str(&text).map_err(|_| invalid_store(&path, "JSON 已损坏"))?;
    validate_store(codex_dir, &path, &store)?;
    Ok(store)
}

pub(crate) fn save_official_account_store(
    codex_dir: &Path,
    store: &OfficialAccountStore,
) -> Result<()> {
    let path = official_accounts_path(codex_dir)?;
    validate_store(codex_dir, &path, store)?;
    if let Some(parent) = path.parent() {
        ensure_directory(parent)?;
    }
    let value = serde_json::to_value(store)
        .map_err(|error| CodexxError::Config(format!("序列化官方账号存储失败: {error}")))?;
    write_private_json(&path, &value)
}

pub(crate) fn official_account_summaries(
    store: &OfficialAccountStore,
) -> Vec<OfficialAccountSummary> {
    store
        .accounts
        .iter()
        .map(|account| OfficialAccountSummary {
            id: account.id.clone(),
            name: account.name.clone(),
            model: account.model.clone(),
            selected: store.selected_account_id.as_deref() == Some(account.id.as_str()),
            created_at: account.created_at.clone(),
            updated_at: account.updated_at.clone(),
            last_used_at: account.last_used_at.clone(),
        })
        .collect()
}

pub(crate) fn official_account_draft(account: &OfficialAccount) -> Result<OfficialAccountDraft> {
    let auth_json = serde_json::to_string_pretty(&account.auth)
        .map_err(|error| CodexxError::Config(format!("格式化官方账号认证失败: {error}")))?;
    Ok(OfficialAccountDraft {
        id: account.id.clone(),
        name: account.name.clone(),
        model: account.model.clone(),
        config_text: account.config.clone(),
        auth_json,
        created_at: account.created_at.clone(),
        updated_at: account.updated_at.clone(),
        last_used_at: account.last_used_at.clone(),
    })
}

pub(crate) fn account_by_id<'a>(
    store: &'a OfficialAccountStore,
    account_id: &str,
) -> Result<&'a OfficialAccount> {
    store
        .accounts
        .iter()
        .find(|account| account.id == account_id)
        .ok_or_else(|| CodexxError::Config("未找到指定的 OpenAI Official 账号".to_string()))
}

pub(crate) fn account_by_id_mut<'a>(
    store: &'a mut OfficialAccountStore,
    account_id: &str,
) -> Result<&'a mut OfficialAccount> {
    store
        .accounts
        .iter_mut()
        .find(|account| account.id == account_id)
        .ok_or_else(|| CodexxError::Config("未找到指定的 OpenAI Official 账号".to_string()))
}

fn next_account_id(store: &OfficialAccountStore) -> String {
    static ACCOUNT_COUNTER: AtomicU64 = AtomicU64::new(0);
    loop {
        let id = format!(
            "official-{:x}-{:x}-{:x}",
            Local::now().timestamp_millis(),
            std::process::id(),
            ACCOUNT_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        if store.accounts.iter().all(|account| account.id != id) {
            return id;
        }
    }
}

pub(crate) fn create_official_account(
    store: &OfficialAccountStore,
    name: &str,
    model: Option<String>,
    config: String,
    auth: Value,
) -> Result<OfficialAccount> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CodexxError::Config("官方账号名称不能为空".to_string()));
    }
    let now = Local::now().to_rfc3339();
    Ok(OfficialAccount {
        id: next_account_id(store),
        name: name.to_string(),
        model,
        config,
        auth,
        created_at: now.clone(),
        updated_at: now.clone(),
        last_used_at: Some(now),
    })
}
