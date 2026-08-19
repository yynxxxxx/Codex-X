use super::ccswitch::codex_section_from_table;
use super::official_accounts::{
    account_by_id, account_by_id_mut, create_official_account, load_official_account_store,
    official_account_draft, official_account_summaries, official_accounts_path,
    save_official_account_store, OfficialAccountDraft, OfficialAccountStore,
    OfficialAccountSummary, OfficialAccountUpdateInput,
};
use super::official_auth::{
    auth_value_has_material, build_official_config_text,
    capture_live_official_config_before_provider_switch, document_is_official, is_chatgpt_auth,
    live_config_is_official, mark_official_config_deleted_reset, mark_official_config_reset,
    official_config_candidate, official_history_restore_blocked, official_snapshot_path,
    save_official_config_snapshot, validate_official_config_text,
};
use super::{
    custom_provider_id, delete_provider_inner, experimental_bearer_token_from_doc,
    is_placeholder_provider, list_saved_providers_inner, list_saved_providers_on_connection,
    matching_saved_provider_ids_for_live, normalize_saved_provider, open_store,
    provider_template_from_document, reserved_codex_provider_id, rollback_provider_store_inner,
    save_provider_with_rollback_inner, strip_provider_bearer_tokens,
    unique_saved_provider_id_for_live, ProviderStoreRollback, SavedProvider,
};
use crate::backups::create_backup;
use crate::config_migration::migrate_legacy_prompt_config_locked;
use crate::error::{CodexxError, Result};
use crate::file_io::{ensure_directory, json_err, parse_toml_document, read_to_string_if_exists};
use crate::live_config::{
    acquire_live_config_lock, atomic_write_if_unchanged, ensure_file_snapshot_unchanged,
    read_file_snapshot, remove_file_if_unchanged, restore_file_snapshot_if_unchanged,
    text_from_snapshot,
};
use crate::state::{build_state_after_migration, ActionResult};
use crate::toml_utils::ensure_table;
use crate::{auth_path, config_path, resolve_codex_dir, string_value};
use chrono::Local;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use toml_edit::{value, DocumentMut, Item};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderInput {
    pub(crate) config_dir: Option<String>,
    #[serde(rename = "providerId")]
    pub(crate) provider_id: Option<String>,
    pub(crate) provider_name: String,
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) api_key: Option<String>,
    pub(crate) wire_api: Option<String>,
    pub(crate) requires_openai_auth: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderTomlInput {
    pub(crate) config_dir: Option<String>,
    pub(crate) config_text: String,
    pub(crate) api_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficialConfigInput {
    pub(crate) config_dir: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) auth_json: Option<String>,
    pub(crate) config_text: Option<String>,
}

enum LiveAuthAction {
    Keep,
    Replace(Value),
    Remove,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiveWriteOrder {
    AuthFirst,
    ConfigFirst,
}

fn config_snapshot_is_official(config: Option<&[u8]>) -> Option<bool> {
    let Some(config) = config else {
        return Some(true);
    };
    let text = std::str::from_utf8(config).ok()?;
    let doc = text.parse::<DocumentMut>().ok()?;
    Some(document_is_official(&doc))
}

fn replacement_write_order(
    current_config: Option<&[u8]>,
    target_config: Option<&[u8]>,
) -> LiveWriteOrder {
    // Never publish an official credential while a third-party endpoint is
    // still active. Other replacements keep CC Switch's auth-first ordering.
    if config_snapshot_is_official(current_config) == Some(false)
        && config_snapshot_is_official(target_config) == Some(true)
    {
        LiveWriteOrder::ConfigFirst
    } else {
        LiveWriteOrder::AuthFirst
    }
}

fn removal_write_order(target_config: Option<&[u8]>) -> LiveWriteOrder {
    // Removing auth must not expose an official credential to a third-party
    // endpoint, while an official route should be published before credentials
    // are removed.
    if config_snapshot_is_official(target_config) == Some(false) {
        LiveWriteOrder::AuthFirst
    } else {
        LiveWriteOrder::ConfigFirst
    }
}

fn json_bytes(path: &Path, value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| json_err(path, error))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn provider_auth_action(api_key: Option<&str>) -> LiveAuthAction {
    let Some(api_key) = api_key.map(str::trim).filter(|key| !key.is_empty()) else {
        return LiveAuthAction::Remove;
    };
    let mut auth = serde_json::Map::new();
    auth.insert(
        "OPENAI_API_KEY".to_string(),
        Value::String(api_key.to_string()),
    );
    LiveAuthAction::Replace(Value::Object(auth))
}

fn live_auth_api_key(codex_dir: &Path) -> Result<Option<String>> {
    let path = auth_path(codex_dir);
    let Some(bytes) = read_file_snapshot(&path)? else {
        return Ok(None);
    };
    // A stale or partially-written auth file must not make the whole manager
    // unusable. Provider switches replace this file atomically, so treat an
    // invalid JSON payload as having no reusable key and let the switch repair
    // it. I/O failures still propagate.
    let auth: Value = match serde_json::from_slice(&bytes) {
        Ok(auth) => auth,
        Err(_) => return Ok(None),
    };
    Ok(auth
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(ToString::to_string))
}

#[derive(Debug)]
struct AppliedLiveFiles {
    config_path: PathBuf,
    auth_path: PathBuf,
    old_config: Option<Vec<u8>>,
    old_auth: Option<Vec<u8>>,
    new_config: Vec<u8>,
    new_auth: Option<Option<Vec<u8>>>,
}

impl AppliedLiveFiles {
    fn rollback(&self) -> Result<()> {
        ensure_file_snapshot_unchanged(&self.config_path, Some(self.new_config.as_slice()))?;
        if let Some(new_auth) = &self.new_auth {
            ensure_file_snapshot_unchanged(&self.auth_path, new_auth.as_deref())?;
        }

        match &self.new_auth {
            Some(Some(new_auth)) => {
                let order = replacement_write_order(
                    Some(self.new_config.as_slice()),
                    self.old_config.as_deref(),
                );
                let restore_auth = || {
                    restore_file_snapshot_if_unchanged(
                        &self.auth_path,
                        Some(new_auth.as_slice()),
                        self.old_auth.as_deref(),
                    )
                };
                let restore_config = || {
                    restore_file_snapshot_if_unchanged(
                        &self.config_path,
                        Some(self.new_config.as_slice()),
                        self.old_config.as_deref(),
                    )
                };
                match order {
                    LiveWriteOrder::AuthFirst => {
                        restore_auth()
                            .map_err(|error| CodexxError::Config(format!("auth.json: {error}")))?;
                        restore_config().map_err(|error| {
                            CodexxError::Config(format!("config.toml: {error}"))
                        })?;
                    }
                    LiveWriteOrder::ConfigFirst => {
                        restore_config().map_err(|error| {
                            CodexxError::Config(format!("config.toml: {error}"))
                        })?;
                        restore_auth()
                            .map_err(|error| CodexxError::Config(format!("auth.json: {error}")))?;
                    }
                }
            }
            Some(None) => {
                let restore_config = || {
                    restore_file_snapshot_if_unchanged(
                        &self.config_path,
                        Some(self.new_config.as_slice()),
                        self.old_config.as_deref(),
                    )
                };
                if let Some(old_auth) = self.old_auth.as_deref() {
                    let restore_auth = || {
                        restore_file_snapshot_if_unchanged(&self.auth_path, None, Some(old_auth))
                    };
                    match replacement_write_order(
                        Some(self.new_config.as_slice()),
                        self.old_config.as_deref(),
                    ) {
                        LiveWriteOrder::AuthFirst => {
                            restore_auth().map_err(|error| {
                                CodexxError::Config(format!("auth.json: {error}"))
                            })?;
                            restore_config().map_err(|error| {
                                CodexxError::Config(format!("config.toml: {error}"))
                            })?;
                        }
                        LiveWriteOrder::ConfigFirst => {
                            restore_config().map_err(|error| {
                                CodexxError::Config(format!("config.toml: {error}"))
                            })?;
                            restore_auth().map_err(|error| {
                                CodexxError::Config(format!("auth.json: {error}"))
                            })?;
                        }
                    }
                } else {
                    restore_config()
                        .map_err(|error| CodexxError::Config(format!("config.toml: {error}")))?;
                }
            }
            None => {
                restore_file_snapshot_if_unchanged(
                    &self.config_path,
                    Some(self.new_config.as_slice()),
                    self.old_config.as_deref(),
                )
                .map_err(|error| CodexxError::Config(format!("config.toml: {error}")))?;
            }
        }
        Ok(())
    }
}

struct AppliedSnapshot {
    path: PathBuf,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
}

impl AppliedSnapshot {
    fn rollback(&self) -> Result<()> {
        restore_file_snapshot_if_unchanged(
            &self.path,
            self.after.as_deref(),
            self.before.as_deref(),
        )
    }
}

#[derive(Default)]
struct AppliedOfficialState {
    changes: Vec<AppliedSnapshot>,
}

impl AppliedOfficialState {
    fn push(&mut self, change: AppliedSnapshot) {
        self.changes.push(change);
    }

    fn rollback(&self) -> Result<()> {
        for change in &self.changes {
            ensure_file_snapshot_unchanged(&change.path, change.after.as_deref())?;
        }
        let mut failures = Vec::new();
        for change in self.changes.iter().rev() {
            if let Err(error) = change.rollback() {
                failures.push(error.to_string());
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(CodexxError::Config(failures.join("；")))
        }
    }
}

fn update_official_snapshot<F>(codex_dir: &Path, update: F) -> Result<AppliedSnapshot>
where
    F: FnOnce() -> Result<()>,
{
    let path = official_snapshot_path(codex_dir)?;
    let before = read_file_snapshot(&path)?;
    update()?;
    let after = read_file_snapshot(&path)?;
    Ok(AppliedSnapshot {
        path,
        before,
        after,
    })
}

fn apply_official_account_store(
    codex_dir: &Path,
    before: Option<Vec<u8>>,
    store: &OfficialAccountStore,
) -> Result<AppliedSnapshot> {
    let path = official_accounts_path(codex_dir)?;
    ensure_file_snapshot_unchanged(&path, before.as_deref())?;
    save_official_account_store(codex_dir, store)?;
    let after = read_file_snapshot(&path)?;
    Ok(AppliedSnapshot {
        path,
        before,
        after,
    })
}

fn live_official_account_values(
    codex_dir: &Path,
    config_snapshot: Option<&[u8]>,
    auth_snapshot: Option<&[u8]>,
) -> Result<Option<(String, Option<String>, Value)>> {
    if config_snapshot_is_official(config_snapshot) != Some(true) {
        return Ok(None);
    }
    let Some(auth_bytes) = auth_snapshot else {
        return Ok(None);
    };
    let auth: Value = match serde_json::from_slice::<Value>(auth_bytes) {
        Ok(value) if value.is_object() && is_chatgpt_auth(&value) => value,
        _ => return Ok(None),
    };
    let config = match config_snapshot {
        Some(bytes) => String::from_utf8(bytes.to_vec())
            .map_err(|_| CodexxError::Config("config.toml 不是有效的 UTF-8 文本".to_string()))?,
        None => build_official_config_text(codex_dir, None, false)?,
    };
    let (config, model) = validate_official_config_text(codex_dir, &config, None)?;
    Ok(Some((config, model, auth)))
}

fn capture_selected_account_in_store(
    codex_dir: &Path,
    store: &mut OfficialAccountStore,
    config_snapshot: Option<&[u8]>,
    auth_snapshot: Option<&[u8]>,
) -> Result<bool> {
    let Some(selected_id) = store.selected_account_id.clone() else {
        return Ok(false);
    };
    let Some((config, model, auth)) =
        live_official_account_values(codex_dir, config_snapshot, auth_snapshot)?
    else {
        return Ok(false);
    };
    let account = account_by_id_mut(store, &selected_id)?;
    let now = Local::now().to_rfc3339();
    account.config = config;
    account.model = model;
    account.auth = auth;
    account.updated_at = now.clone();
    account.last_used_at = Some(now);
    Ok(true)
}

fn capture_live_official_state(codex_dir: &Path) -> Result<Option<AppliedOfficialState>> {
    let accounts_path = official_accounts_path(codex_dir)?;
    let accounts_before = read_file_snapshot(&accounts_path)?;
    let mut store = load_official_account_store(codex_dir)?;
    let config_snapshot = read_file_snapshot(&config_path(codex_dir))?;
    let auth_snapshot = read_file_snapshot(&auth_path(codex_dir))?;
    let account_changed = capture_selected_account_in_store(
        codex_dir,
        &mut store,
        config_snapshot.as_deref(),
        auth_snapshot.as_deref(),
    )?;

    let snapshot_path = official_snapshot_path(codex_dir)?;
    let snapshot_before = read_file_snapshot(&snapshot_path)?;
    let snapshot_changed = capture_live_official_config_before_provider_switch(codex_dir)?;
    let mut applied = AppliedOfficialState::default();
    if snapshot_changed {
        applied.push(AppliedSnapshot {
            path: snapshot_path,
            before: snapshot_before,
            after: read_file_snapshot(&official_snapshot_path(codex_dir)?)?,
        });
    }
    if account_changed {
        match apply_official_account_store(codex_dir, accounts_before, &store) {
            Ok(change) => applied.push(change),
            Err(error) => {
                return match applied.rollback() {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(CodexxError::Config(format!(
                        "{error}；官方认证捕获回滚失败：{rollback_error}"
                    ))),
                };
            }
        }
    }
    Ok((!applied.changes.is_empty()).then_some(applied))
}

fn rollback_after_failure<T>(
    error: CodexxError,
    live: Option<&AppliedLiveFiles>,
    snapshot: Option<&AppliedSnapshot>,
) -> Result<T> {
    let mut failures = Vec::new();
    let mut live_rollback_succeeded = true;
    if let Some(live) = live {
        if let Err(rollback_error) = live.rollback() {
            live_rollback_succeeded = false;
            failures.push(format!("live 配置: {rollback_error}"));
        }
    }
    // Keep the newly captured official snapshot as a recovery point when live
    // rollback is blocked by an external writer.
    if live_rollback_succeeded {
        if let Some(snapshot) = snapshot {
            if let Err(rollback_error) = snapshot.rollback() {
                failures.push(format!("官方快照: {rollback_error}"));
            }
        }
    }
    if failures.is_empty() {
        Err(error)
    } else {
        Err(CodexxError::Config(format!(
            "{error}；回滚失败：{}",
            failures.join("；")
        )))
    }
}

fn rollback_after_official_state_failure<T>(
    error: CodexxError,
    live: Option<&AppliedLiveFiles>,
    state: Option<&AppliedOfficialState>,
) -> Result<T> {
    let mut failures = Vec::new();
    let mut live_rollback_succeeded = true;
    if let Some(live) = live {
        if let Err(rollback_error) = live.rollback() {
            live_rollback_succeeded = false;
            failures.push(format!("live 配置: {rollback_error}"));
        }
    }
    if live_rollback_succeeded {
        if let Some(state) = state {
            if let Err(rollback_error) = state.rollback() {
                failures.push(format!("官方账号状态: {rollback_error}"));
            }
        }
    }
    if failures.is_empty() {
        Err(error)
    } else {
        Err(CodexxError::Config(format!(
            "{error}；回滚失败：{}",
            failures.join("；")
        )))
    }
}

fn rollback_persisted_provider<T>(
    result: Result<T>,
    rollback: Option<ProviderStoreRollback>,
) -> Result<T> {
    match (result, rollback) {
        (Ok(value), _) => Ok(value),
        (Err(error), None) => Err(error),
        (Err(error), Some(rollback)) => match rollback_provider_store_inner(rollback) {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(CodexxError::Database(format!(
                "{error}；供应商配置回滚失败: {rollback_error}"
            ))),
        },
    }
}

fn write_live_files(
    codex_dir: &Path,
    old_config: Option<Vec<u8>>,
    old_auth: Option<Vec<u8>>,
    config_text: &str,
    auth_action: &LiveAuthAction,
) -> Result<AppliedLiveFiles> {
    write_live_files_with_between_writes(
        codex_dir,
        old_config,
        old_auth,
        config_text,
        auth_action,
        || Ok(()),
    )
}

fn write_live_files_with_between_writes<F>(
    codex_dir: &Path,
    old_config: Option<Vec<u8>>,
    old_auth: Option<Vec<u8>>,
    config_text: &str,
    auth_action: &LiveAuthAction,
    between_writes: F,
) -> Result<AppliedLiveFiles>
where
    F: FnOnce() -> Result<()>,
{
    let cfg = config_path(codex_dir);
    let auth = auth_path(codex_dir);
    let new_config = config_text.as_bytes().to_vec();
    let new_auth = match auth_action {
        LiveAuthAction::Keep => None,
        LiveAuthAction::Replace(value) => Some(Some(json_bytes(&auth, value)?)),
        LiveAuthAction::Remove => Some(None),
    };

    match &new_auth {
        Some(Some(bytes)) => {
            match replacement_write_order(old_config.as_deref(), Some(new_config.as_slice())) {
                LiveWriteOrder::AuthFirst => {
                    atomic_write_if_unchanged(&auth, old_auth.as_deref(), bytes)?;
                    let write_result = between_writes()
                        .and_then(|()| {
                            ensure_file_snapshot_unchanged(&auth, Some(bytes.as_slice()))
                        })
                        .and_then(|()| {
                            atomic_write_if_unchanged(&cfg, old_config.as_deref(), &new_config)
                        });
                    if let Err(error) = write_result {
                        let rollback = restore_file_snapshot_if_unchanged(
                            &auth,
                            Some(bytes.as_slice()),
                            old_auth.as_deref(),
                        );
                        return match rollback {
                            Ok(()) => Err(error),
                            Err(rollback_error) => Err(CodexxError::Config(format!(
                                "写入 Codex live 配置失败：{error}；auth.json 回滚也失败：{rollback_error}"
                            ))),
                        };
                    }
                }
                LiveWriteOrder::ConfigFirst => {
                    atomic_write_if_unchanged(&cfg, old_config.as_deref(), &new_config)?;
                    let write_result = between_writes()
                        .and_then(|()| {
                            ensure_file_snapshot_unchanged(&cfg, Some(new_config.as_slice()))
                        })
                        .and_then(|()| {
                            atomic_write_if_unchanged(&auth, old_auth.as_deref(), bytes)
                        });
                    if let Err(error) = write_result {
                        let rollback = restore_file_snapshot_if_unchanged(
                            &cfg,
                            Some(new_config.as_slice()),
                            old_config.as_deref(),
                        );
                        return match rollback {
                            Ok(()) => Err(error),
                            Err(rollback_error) => Err(CodexxError::Config(format!(
                                "写入 Codex live 配置失败：{error}；config.toml 回滚也失败：{rollback_error}"
                            ))),
                        };
                    }
                }
            }
        }
        Some(None) => {
            let order = removal_write_order(Some(new_config.as_slice()));
            match order {
                LiveWriteOrder::AuthFirst => {
                    remove_file_if_unchanged(&auth, old_auth.as_deref())?;
                    let write_result = between_writes()
                        .and_then(|()| ensure_file_snapshot_unchanged(&auth, None))
                        .and_then(|()| {
                            atomic_write_if_unchanged(&cfg, old_config.as_deref(), &new_config)
                        });
                    if let Err(error) = write_result {
                        let rollback =
                            restore_file_snapshot_if_unchanged(&auth, None, old_auth.as_deref());
                        return match rollback {
                            Ok(()) => Err(error),
                            Err(rollback_error) => Err(CodexxError::Config(format!(
                                "写入 Codex live 配置失败：{error}；auth.json 回滚也失败：{rollback_error}"
                            ))),
                        };
                    }
                }
                LiveWriteOrder::ConfigFirst => {
                    atomic_write_if_unchanged(&cfg, old_config.as_deref(), &new_config)?;
                    let write_result = between_writes()
                        .and_then(|()| {
                            ensure_file_snapshot_unchanged(&cfg, Some(new_config.as_slice()))
                        })
                        .and_then(|()| remove_file_if_unchanged(&auth, old_auth.as_deref()));
                    if let Err(error) = write_result {
                        let rollback = restore_file_snapshot_if_unchanged(
                            &cfg,
                            Some(new_config.as_slice()),
                            old_config.as_deref(),
                        );
                        return match rollback {
                            Ok(()) => Err(error),
                            Err(rollback_error) => Err(CodexxError::Config(format!(
                                "写入 Codex live 配置失败：{error}；config.toml 回滚也失败：{rollback_error}"
                            ))),
                        };
                    }
                }
            }
        }
        None => {
            atomic_write_if_unchanged(&cfg, old_config.as_deref(), &new_config)?;
        }
    }

    Ok(AppliedLiveFiles {
        config_path: cfg,
        auth_path: auth,
        old_config,
        old_auth,
        new_config,
        new_auth,
    })
}

pub(crate) fn detected_live_custom_provider(codex_dir: &Path) -> Result<Option<SavedProvider>> {
    let cfg = config_path(codex_dir);
    let text = read_to_string_if_exists(&cfg)?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    let doc = parse_toml_document(&cfg, &text)?;
    let Some(provider_id) = string_value(&doc, "model_provider") else {
        return Ok(None);
    };
    if document_is_official(&doc)
        || (provider_id != "custom" && reserved_codex_provider_id(&provider_id))
    {
        return Ok(None);
    }
    let Some(model) = string_value(&doc, "model") else {
        return Ok(None);
    };
    let Some(provider_table) = doc
        .get("model_providers")
        .and_then(|item| item.as_table())
        .and_then(|providers| providers.get(provider_id.as_str()))
        .and_then(|item| item.as_table())
    else {
        return Ok(None);
    };
    let Some(section) = codex_section_from_table(&provider_id, provider_table, Some(model.clone()))
    else {
        return Ok(None);
    };

    // Older switchers stored a third-party key in auth.json. Read it only after
    // this document has been proven to be a third-party route; it is used for
    // matching/migration and is never promoted to an official auth snapshot.
    let api_key = match experimental_bearer_token_from_doc(&doc, Some(&provider_id)) {
        Some(api_key) => Some(api_key),
        None => live_auth_api_key(codex_dir)?,
    };
    let provider_name = section
        .name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| provider_id.clone());
    let toml_config = provider_template_from_document(&doc, &provider_id, &model)?;

    Ok(Some(SavedProvider {
        id: provider_id,
        provider_name,
        base_url: section.base_url,
        model,
        api_key,
        toml_config: (!toml_config.is_empty()).then_some(toml_config),
        wire_api: section.wire_api,
        requires_openai_auth: section.requires_openai_auth,
    }))
}

fn persist_detected_live_custom_provider(
    codex_dir: &Path,
) -> Result<Option<ProviderStoreRollback>> {
    let Some(mut live) = detected_live_custom_provider(codex_dir)? else {
        return Ok(None);
    };
    let saved = list_saved_providers_inner()?;
    let Some(saved_id) = unique_saved_provider_id_for_live(&live, &saved) else {
        return Ok(None);
    };
    if live.api_key.is_none() {
        live.api_key = saved
            .iter()
            .find(|provider| provider.id == saved_id)
            .and_then(|provider| provider.api_key.clone());
    }
    live.id = saved_id;
    let (_, rollback) = save_provider_with_rollback_inner(live)?;
    Ok(Some(rollback))
}

pub(crate) fn build_provider_toml_draft_inner(
    mut provider: SavedProvider,
    config_dir: Option<String>,
) -> Result<String> {
    if provider.id.trim().is_empty() {
        provider.id = custom_provider_id(&provider.provider_name);
    }
    let codex_dir = resolve_codex_dir(config_dir)?;
    let cfg = config_path(&codex_dir);
    let saved_template = provider
        .toml_config
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let base_text = match saved_template {
        Some(text) => text.to_string(),
        None => read_to_string_if_exists(&cfg)?,
    };
    let mut doc = parse_toml_document(&cfg, &base_text)?;
    strip_provider_bearer_tokens(&mut doc);

    let provider_id = saved_template
        .and_then(|_| string_value(&doc, "model_provider"))
        .filter(|id| {
            doc.get("model_providers")
                .and_then(|item| item.as_table())
                .and_then(|providers| providers.get(id))
                .and_then(|item| item.as_table())
                .is_some()
        })
        .unwrap_or_else(|| "custom".to_string());
    doc["model_provider"] = value(provider_id.clone());
    doc["model"] = value(provider.model.trim());
    let providers = ensure_table(doc.as_table_mut(), "model_providers")?;
    let table = ensure_table(providers, &provider_id)?;
    table["name"] = value(provider.provider_name.trim());
    table["base_url"] = value(provider.base_url.trim().trim_end_matches('/'));
    table["wire_api"] = value(provider.wire_api.trim());
    table["requires_openai_auth"] = value(provider.requires_openai_auth);
    provider.toml_config = Some(doc.to_string().trim_end().to_string());

    normalize_saved_provider(provider)?
        .toml_config
        .ok_or_else(|| CodexxError::Config("无法生成供应商 TOML".to_string()))
}

fn apply_official_config_locked(
    codex_dir: &Path,
    config_text: Option<&str>,
    model: Option<&str>,
    clear_model_if_none: bool,
    auth_action: LiveAuthAction,
    action: &str,
) -> Result<(Option<String>, AppliedLiveFiles)> {
    let cfg = config_path(codex_dir);
    let auth = auth_path(codex_dir);
    let old_config = read_file_snapshot(&cfg)?;
    let old_auth = read_file_snapshot(&auth)?;
    let backup_id = create_backup(codex_dir, action)?;

    let config_text = match config_text.map(str::trim).filter(|text| !text.is_empty()) {
        Some(config_text) => validate_official_config_text(codex_dir, config_text, model)?.0,
        None => {
            let config = build_official_config_text(codex_dir, model, clear_model_if_none)?;
            validate_official_config_text(codex_dir, &config, model)?.0
        }
    };

    let applied = write_live_files(codex_dir, old_config, old_auth, &config_text, &auth_action)?;
    Ok((backup_id, applied))
}

fn applied_config_text(live: &AppliedLiveFiles) -> Result<String> {
    String::from_utf8(live.new_config.clone())
        .map_err(|error| CodexxError::Config(format!("config.toml 不是有效 UTF-8: {error}")))
}

fn finish_live_action(
    codex_dir: &Path,
    message: String,
    backup_id: Option<String>,
    live: &AppliedLiveFiles,
    snapshot: Option<&AppliedSnapshot>,
) -> Result<ActionResult> {
    match build_state_after_migration(codex_dir.to_path_buf()) {
        Ok(state) => Ok(ActionResult {
            ok: true,
            message,
            backup_id,
            state,
        }),
        Err(error) => rollback_after_failure(error, Some(live), snapshot),
    }
}

fn finish_live_action_with_official_state(
    codex_dir: &Path,
    message: String,
    backup_id: Option<String>,
    live: &AppliedLiveFiles,
    state: Option<&AppliedOfficialState>,
) -> Result<ActionResult> {
    match build_state_after_migration(codex_dir.to_path_buf()) {
        Ok(state) => Ok(ActionResult {
            ok: true,
            message,
            backup_id,
            state,
        }),
        Err(error) => rollback_after_official_state_failure(error, Some(live), state),
    }
}

pub(crate) fn switch_official_provider_with_pre_persist<F>(
    config_dir: Option<String>,
    pre_persist: F,
) -> Result<ActionResult>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let codex_dir = resolve_codex_dir(config_dir)?;
    ensure_directory(&codex_dir)?;
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    let was_official = live_config_is_official(&codex_dir)?;
    let history_restore_blocked = official_history_restore_blocked(&codex_dir)?;
    pre_persist(&codex_dir)?;
    let candidate = official_config_candidate(&codex_dir, false)?;
    let model = candidate
        .as_ref()
        .and_then(|candidate| candidate.model.clone());
    let config_text = candidate
        .as_ref()
        .and_then(|candidate| candidate.config_text.clone());
    let candidate_auth = candidate
        .as_ref()
        .and_then(|candidate| candidate.auth.clone());
    let auth_action = candidate_auth
        .clone()
        .map(LiveAuthAction::Replace)
        .unwrap_or(if candidate.is_some() {
            LiveAuthAction::Remove
        } else if was_official {
            // A user may legitimately use the built-in OpenAI provider with an
            // API key. It is safe to keep only while the live route is already
            // official; an API key seen under a proxy route remains ambiguous.
            LiveAuthAction::Keep
        } else {
            LiveAuthAction::Remove
        });
    let message = if candidate_auth.is_some() {
        "已切换到 OpenAI Official".to_string()
    } else {
        "已切换到 OpenAI Official，请在 Codex 中完成登录".to_string()
    };
    let (backup_id, live) = apply_official_config_locked(
        &codex_dir,
        config_text.as_deref(),
        model.as_deref(),
        !was_official && model.is_none(),
        auth_action,
        "switch-official",
    )?;
    let snapshot = if let Some(candidate) = candidate {
        let applied_config = applied_config_text(&live)?;
        match update_official_snapshot(&codex_dir, || {
            if let Some(auth) = candidate.auth.as_ref() {
                save_official_config_snapshot(&codex_dir, Some(applied_config), model, auth)
            } else if history_restore_blocked {
                mark_official_config_deleted_reset(&codex_dir, Some(applied_config), model)
            } else {
                mark_official_config_reset(&codex_dir, Some(applied_config), model)
            }
        }) {
            Ok(snapshot) => Some(snapshot),
            Err(error) => return rollback_after_failure(error, Some(&live), None),
        }
    } else {
        None
    };
    finish_live_action(&codex_dir, message, backup_id, &live, snapshot.as_ref())
}

pub(crate) fn switch_official_provider_inner(config_dir: Option<String>) -> Result<ActionResult> {
    let mut provider_rollback = None;
    let result = switch_official_provider_with_pre_persist(config_dir, |codex_dir| {
        provider_rollback = persist_detected_live_custom_provider(codex_dir)?;
        Ok(())
    });
    rollback_persisted_provider(result, provider_rollback)
}

pub(crate) fn save_official_config_inner(
    config_dir: Option<String>,
    model: Option<String>,
    auth_json: Option<String>,
    config_text: Option<String>,
) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    ensure_directory(&codex_dir)?;
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    let auth = auth_path(&codex_dir);
    let model = model
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let existing = official_config_candidate(&codex_dir, false)?;
    let parsed_auth = if let Some(auth_json) = auth_json
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        let parsed: Value = serde_json::from_str(&auth_json).map_err(|e| json_err(&auth, e))?;
        if !parsed.is_object() || !auth_value_has_material(&parsed) {
            return Err(CodexxError::Config(
                "官方 auth.json 必须是包含有效认证信息的 JSON object".to_string(),
            ));
        }
        parsed
    } else {
        existing
            .as_ref()
            .and_then(|candidate| candidate.auth.clone())
            .ok_or_else(|| {
                CodexxError::Config("没有可保存的官方认证，请先完成官方登录".to_string())
            })?
    };
    let requested_config = config_text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            existing
                .as_ref()
                .and_then(|candidate| candidate.config_text.clone())
        });
    let (official_config, effective_model) = match requested_config {
        Some(config) => validate_official_config_text(&codex_dir, &config, model.as_deref())?,
        None => {
            let config = build_official_config_text(&codex_dir, model.as_deref(), false)?;
            validate_official_config_text(&codex_dir, &config, model.as_deref())?
        }
    };

    if live_config_is_official(&codex_dir)? {
        let (backup_id, live) = apply_official_config_locked(
            &codex_dir,
            Some(&official_config),
            effective_model.as_deref(),
            false,
            LiveAuthAction::Replace(parsed_auth.clone()),
            "save-official",
        )?;
        let applied_config = applied_config_text(&live)?;
        let snapshot = match update_official_snapshot(&codex_dir, || {
            save_official_config_snapshot(
                &codex_dir,
                Some(applied_config),
                effective_model,
                &parsed_auth,
            )
        }) {
            Ok(snapshot) => snapshot,
            Err(error) => return rollback_after_failure(error, Some(&live), None),
        };
        return finish_live_action(
            &codex_dir,
            "已保存并更新当前 OpenAI Official 配置".to_string(),
            backup_id,
            &live,
            Some(&snapshot),
        );
    }

    let backup_id = create_backup(&codex_dir, "save-official-snapshot")?;
    let snapshot = update_official_snapshot(&codex_dir, || {
        save_official_config_snapshot(
            &codex_dir,
            Some(official_config),
            effective_model,
            &parsed_auth,
        )
    })?;
    match build_state_after_migration(codex_dir.clone()) {
        Ok(state) => Ok(ActionResult {
            ok: true,
            message: "已保存 OpenAI Official 配置".to_string(),
            backup_id,
            state,
        }),
        Err(error) => rollback_after_failure(error, None, Some(&snapshot)),
    }
}

fn finish_official_state_action(
    codex_dir: &Path,
    message: String,
    backup_id: Option<String>,
    state_change: &AppliedOfficialState,
) -> Result<ActionResult> {
    match build_state_after_migration(codex_dir.to_path_buf()) {
        Ok(state) => Ok(ActionResult {
            ok: true,
            message,
            backup_id,
            state,
        }),
        Err(error) => rollback_after_official_state_failure(error, None, Some(state_change)),
    }
}

fn parse_trusted_official_auth(path: &Path, auth_json: &str) -> Result<Value> {
    let auth: Value = serde_json::from_str(auth_json).map_err(|error| json_err(path, error))?;
    if !auth.is_object() || !is_chatgpt_auth(&auth) {
        return Err(CodexxError::Config(
            "官方账号 auth.json 不是可信的 OpenAI Official 认证".to_string(),
        ));
    }
    Ok(auth)
}

fn apply_legacy_account_snapshot(
    codex_dir: &Path,
    config: String,
    model: Option<String>,
    auth: &Value,
) -> Result<AppliedSnapshot> {
    update_official_snapshot(codex_dir, || {
        save_official_config_snapshot(codex_dir, Some(config), model, auth)
    })
}

pub(crate) fn list_official_accounts_inner(
    config_dir: Option<String>,
) -> Result<Vec<OfficialAccountSummary>> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    let store = load_official_account_store(&codex_dir)?;
    Ok(official_account_summaries(&store))
}

pub(crate) fn get_official_account_inner(
    config_dir: Option<String>,
    account_id: String,
) -> Result<OfficialAccountDraft> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    let store = load_official_account_store(&codex_dir)?;
    official_account_draft(account_by_id(&store, account_id.trim())?)
}

pub(crate) fn capture_current_official_account_inner(
    config_dir: Option<String>,
    name: String,
) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    ensure_directory(&codex_dir)?;
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    let config_before = read_file_snapshot(&config_path(&codex_dir))?;
    let auth_before = read_file_snapshot(&auth_path(&codex_dir))?;
    let Some((config, model, auth)) =
        live_official_account_values(&codex_dir, config_before.as_deref(), auth_before.as_deref())?
    else {
        return Err(CodexxError::Config(
            "未检测到有效 OpenAI Official 登录，请先在 Codex 中完成登录".to_string(),
        ));
    };

    let accounts_path = official_accounts_path(&codex_dir)?;
    let accounts_before = read_file_snapshot(&accounts_path)?;
    let mut store = load_official_account_store(&codex_dir)?;
    if store.selected_account_id.is_some() {
        return Err(CodexxError::Config(
            "当前 OpenAI Official 登录已经保存，请先新增官方账号".to_string(),
        ));
    }
    let account =
        create_official_account(&store, &name, model.clone(), config.clone(), auth.clone())?;
    let account_id = account.id.clone();
    store.accounts.push(account);
    store.selected_account_id = Some(account_id);
    let backup_id = create_backup(&codex_dir, "capture-official-account")?;

    let mut applied = AppliedOfficialState::default();
    applied.push(apply_official_account_store(
        &codex_dir,
        accounts_before,
        &store,
    )?);
    match apply_legacy_account_snapshot(&codex_dir, config, model, &auth) {
        Ok(change) => applied.push(change),
        Err(error) => return rollback_after_official_state_failure(error, None, Some(&applied)),
    }
    finish_official_state_action(
        &codex_dir,
        "已保存当前 OpenAI Official 登录".to_string(),
        backup_id,
        &applied,
    )
}

pub(crate) fn update_official_account_inner(
    input: OfficialAccountUpdateInput,
) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(input.config_dir)?;
    ensure_directory(&codex_dir)?;
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    let name = input.name.trim();
    if name.is_empty() {
        return Err(CodexxError::Config("官方账号名称不能为空".to_string()));
    }
    let requested_model = input
        .model
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty());
    let (config, model) = validate_official_config_text(
        &codex_dir,
        input.config_text.trim(),
        requested_model.as_deref(),
    )?;
    let auth = parse_trusted_official_auth(&auth_path(&codex_dir), &input.auth_json)?;
    let account_id = input.account_id.trim();

    let accounts_path = official_accounts_path(&codex_dir)?;
    let accounts_before = read_file_snapshot(&accounts_path)?;
    let mut store = load_official_account_store(&codex_dir)?;
    let selected = store.selected_account_id.as_deref() == Some(account_id);
    let now = Local::now().to_rfc3339();
    let account = account_by_id_mut(&mut store, account_id)?;
    account.name = name.to_string();
    account.model = model.clone();
    account.config = config.clone();
    account.auth = auth.clone();
    account.updated_at = now;

    let applies_live = selected && live_config_is_official(&codex_dir)?;
    let (backup_id, live) = if applies_live {
        let (backup_id, live) = apply_official_config_locked(
            &codex_dir,
            Some(&config),
            model.as_deref(),
            false,
            LiveAuthAction::Replace(auth.clone()),
            "update-official-account",
        )?;
        (backup_id, Some(live))
    } else {
        (None, None)
    };

    let mut applied = AppliedOfficialState::default();
    match apply_official_account_store(&codex_dir, accounts_before, &store) {
        Ok(change) => applied.push(change),
        Err(error) => {
            return rollback_after_official_state_failure(error, live.as_ref(), None);
        }
    }
    if selected {
        match apply_legacy_account_snapshot(&codex_dir, config, model, &auth) {
            Ok(change) => applied.push(change),
            Err(error) => {
                return rollback_after_official_state_failure(error, live.as_ref(), Some(&applied));
            }
        }
    }

    match live.as_ref() {
        Some(live) => finish_live_action_with_official_state(
            &codex_dir,
            "官方账号已保存并更新当前登录".to_string(),
            backup_id,
            live,
            Some(&applied),
        ),
        None => finish_official_state_action(
            &codex_dir,
            "官方账号已保存".to_string(),
            backup_id,
            &applied,
        ),
    }
}

fn switch_official_account_with_pre_persist<F>(
    config_dir: Option<String>,
    account_id: String,
    pre_persist: F,
) -> Result<ActionResult>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let codex_dir = resolve_codex_dir(config_dir)?;
    ensure_directory(&codex_dir)?;
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    let account_id = account_id.trim();
    let accounts_path = official_accounts_path(&codex_dir)?;
    let accounts_before = read_file_snapshot(&accounts_path)?;
    let mut store = load_official_account_store(&codex_dir)?;
    account_by_id(&store, account_id)?;

    let old_config = read_file_snapshot(&config_path(&codex_dir))?;
    let old_auth = read_file_snapshot(&auth_path(&codex_dir))?;
    let was_official = config_snapshot_is_official(old_config.as_deref()) == Some(true);
    if was_official {
        capture_selected_account_in_store(
            &codex_dir,
            &mut store,
            old_config.as_deref(),
            old_auth.as_deref(),
        )?;
    } else {
        pre_persist(&codex_dir)?;
    }

    let target = account_by_id(&store, account_id)?.clone();
    let (config, model) =
        validate_official_config_text(&codex_dir, &target.config, target.model.as_deref())?;
    let (backup_id, live) = apply_official_config_locked(
        &codex_dir,
        Some(&config),
        model.as_deref(),
        false,
        LiveAuthAction::Replace(target.auth.clone()),
        "switch-official-account",
    )?;

    let now = Local::now().to_rfc3339();
    store.selected_account_id = Some(account_id.to_string());
    account_by_id_mut(&mut store, account_id)?.last_used_at = Some(now);
    let mut applied = AppliedOfficialState::default();
    match apply_official_account_store(&codex_dir, accounts_before, &store) {
        Ok(change) => applied.push(change),
        Err(error) => return rollback_after_official_state_failure(error, Some(&live), None),
    }
    match apply_legacy_account_snapshot(&codex_dir, config, model, &target.auth) {
        Ok(change) => applied.push(change),
        Err(error) => {
            return rollback_after_official_state_failure(error, Some(&live), Some(&applied));
        }
    }
    finish_live_action_with_official_state(
        &codex_dir,
        format!("已切换到 OpenAI Official · {}", target.name),
        backup_id,
        &live,
        Some(&applied),
    )
}

pub(crate) fn switch_official_account_inner(
    config_dir: Option<String>,
    account_id: String,
) -> Result<ActionResult> {
    let mut provider_rollback = None;
    let result = switch_official_account_with_pre_persist(config_dir, account_id, |codex_dir| {
        provider_rollback = persist_detected_live_custom_provider(codex_dir)?;
        Ok(())
    });
    rollback_persisted_provider(result, provider_rollback)
}

fn prepare_new_official_account_with_pre_persist<F>(
    config_dir: Option<String>,
    pre_persist: F,
) -> Result<ActionResult>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let codex_dir = resolve_codex_dir(config_dir)?;
    ensure_directory(&codex_dir)?;
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    let accounts_path = official_accounts_path(&codex_dir)?;
    let accounts_before = read_file_snapshot(&accounts_path)?;
    let mut store = load_official_account_store(&codex_dir)?;
    let old_config = read_file_snapshot(&config_path(&codex_dir))?;
    let old_auth = read_file_snapshot(&auth_path(&codex_dir))?;
    let was_official = config_snapshot_is_official(old_config.as_deref()) == Some(true);
    if was_official {
        capture_selected_account_in_store(
            &codex_dir,
            &mut store,
            old_config.as_deref(),
            old_auth.as_deref(),
        )?;
    } else {
        pre_persist(&codex_dir)?;
    }

    let requested_config = if was_official {
        old_config
            .as_deref()
            .map(|bytes| text_from_snapshot(&config_path(&codex_dir), Some(bytes)))
            .transpose()?
    } else {
        store
            .selected_account_id
            .as_deref()
            .and_then(|id| account_by_id(&store, id).ok())
            .map(|account| account.config.clone())
            .or_else(|| {
                official_config_candidate(&codex_dir, false)
                    .ok()
                    .flatten()
                    .and_then(|candidate| candidate.config_text)
            })
    };
    let (backup_id, live) = apply_official_config_locked(
        &codex_dir,
        requested_config.as_deref(),
        None,
        false,
        LiveAuthAction::Remove,
        "prepare-new-official-account",
    )?;
    let applied_config = applied_config_text(&live)?;
    let applied_model = validate_official_config_text(&codex_dir, &applied_config, None)?.1;
    store.selected_account_id = None;

    let mut applied = AppliedOfficialState::default();
    match apply_official_account_store(&codex_dir, accounts_before, &store) {
        Ok(change) => applied.push(change),
        Err(error) => return rollback_after_official_state_failure(error, Some(&live), None),
    }
    match update_official_snapshot(&codex_dir, || {
        mark_official_config_reset(
            &codex_dir,
            Some(applied_config.clone()),
            applied_model.clone(),
        )
    }) {
        Ok(change) => applied.push(change),
        Err(error) => {
            return rollback_after_official_state_failure(error, Some(&live), Some(&applied));
        }
    }
    finish_live_action_with_official_state(
        &codex_dir,
        "已准备新的 OpenAI Official 登录，请在 Codex 中完成登录".to_string(),
        backup_id,
        &live,
        Some(&applied),
    )
}

pub(crate) fn prepare_new_official_account_inner(
    config_dir: Option<String>,
) -> Result<ActionResult> {
    let mut provider_rollback = None;
    let result = prepare_new_official_account_with_pre_persist(config_dir, |codex_dir| {
        provider_rollback = persist_detected_live_custom_provider(codex_dir)?;
        Ok(())
    });
    rollback_persisted_provider(result, provider_rollback)
}

pub(crate) fn delete_official_account_inner(
    config_dir: Option<String>,
    account_id: String,
) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    ensure_directory(&codex_dir)?;
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    let account_id = account_id.trim();
    let accounts_path = official_accounts_path(&codex_dir)?;
    let accounts_before = read_file_snapshot(&accounts_path)?;
    let mut store = load_official_account_store(&codex_dir)?;
    let deleted = account_by_id(&store, account_id)?.clone();
    let selected = store.selected_account_id.as_deref() == Some(account_id);
    let current_official = selected && live_config_is_official(&codex_dir)?;

    let (backup_id, live, reset_config, reset_model) = if current_official {
        let current_config = read_file_snapshot(&config_path(&codex_dir))?;
        let current_config = current_config
            .as_deref()
            .map(|bytes| text_from_snapshot(&config_path(&codex_dir), Some(bytes)))
            .transpose()?
            .unwrap_or_else(|| deleted.config.clone());
        let (config, model) = validate_official_config_text(&codex_dir, &current_config, None)?;
        let (backup_id, live) = apply_official_config_locked(
            &codex_dir,
            Some(&config),
            model.as_deref(),
            false,
            LiveAuthAction::Remove,
            "delete-current-official-account",
        )?;
        (backup_id, Some(live), config, model)
    } else {
        let (config, model) =
            validate_official_config_text(&codex_dir, &deleted.config, deleted.model.as_deref())?;
        (None, None, config, model)
    };

    store.accounts.retain(|account| account.id != account_id);
    if selected {
        store.selected_account_id = None;
    }
    let mut applied = AppliedOfficialState::default();
    match apply_official_account_store(&codex_dir, accounts_before, &store) {
        Ok(change) => applied.push(change),
        Err(error) => {
            return rollback_after_official_state_failure(error, live.as_ref(), None);
        }
    }
    if selected {
        match update_official_snapshot(&codex_dir, || {
            mark_official_config_deleted_reset(
                &codex_dir,
                Some(reset_config.clone()),
                reset_model.clone(),
            )
        }) {
            Ok(change) => applied.push(change),
            Err(error) => {
                return rollback_after_official_state_failure(error, live.as_ref(), Some(&applied));
            }
        }
    }

    match live.as_ref() {
        Some(live) => finish_live_action_with_official_state(
            &codex_dir,
            "当前官方账号已删除，OpenAI 登录状态已清除".to_string(),
            backup_id,
            live,
            Some(&applied),
        ),
        None => finish_official_state_action(
            &codex_dir,
            "官方账号已删除".to_string(),
            backup_id,
            &applied,
        ),
    }
}

pub(crate) fn restore_official_provider_inner(config_dir: Option<String>) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    ensure_directory(&codex_dir)?;
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    if official_history_restore_blocked(&codex_dir)? {
        return Err(CodexxError::Config(
            "已删除的官方账号不能从旧快照或历史备份恢复".to_string(),
        ));
    }
    let candidate = official_config_candidate(&codex_dir, true)?.ok_or_else(|| {
        CodexxError::Config(
            "未找到可信的官方认证快照或官方模式历史备份，请新建官方配置后重新登录".to_string(),
        )
    })?;
    let model = candidate.model.clone();
    let config_text = candidate.config_text.clone();
    let message = "已还原 OpenAI Official 配置".to_string();
    let snapshot = update_official_snapshot(&codex_dir, || {
        if let Some(auth) = candidate.auth.as_ref() {
            save_official_config_snapshot(&codex_dir, config_text, model, auth)
        } else {
            mark_official_config_reset(&codex_dir, config_text, model)
        }
    })?;
    match build_state_after_migration(codex_dir.clone()) {
        Ok(state) => Ok(ActionResult {
            ok: true,
            message,
            backup_id: None,
            state,
        }),
        Err(error) => rollback_after_failure(error, None, Some(&snapshot)),
    }
}

fn reset_official_provider_with_pre_persist<F>(
    config_dir: Option<String>,
    model: Option<String>,
    config_text: Option<String>,
    pre_persist: F,
) -> Result<ActionResult>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let codex_dir = resolve_codex_dir(config_dir)?;
    ensure_directory(&codex_dir)?;
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    pre_persist(&codex_dir)?;
    let model = model
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let config_text = match config_text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        Some(config_text) => {
            validate_official_config_text(&codex_dir, config_text, model.as_deref())?.0
        }
        None => build_official_config_text(&codex_dir, model.as_deref(), model.is_none())?,
    };
    let (backup_id, live) = apply_official_config_locked(
        &codex_dir,
        Some(&config_text),
        model.as_deref(),
        model.is_none(),
        LiveAuthAction::Remove,
        "reset-official",
    )?;
    let applied_config = applied_config_text(&live)?;
    let applied_model = validate_official_config_text(&codex_dir, &applied_config, None)?.1;
    let snapshot = match update_official_snapshot(&codex_dir, || {
        mark_official_config_reset(&codex_dir, Some(applied_config), applied_model)
    }) {
        Ok(snapshot) => snapshot,
        Err(error) => return rollback_after_failure(error, Some(&live), None),
    };
    finish_live_action(
        &codex_dir,
        "已新建 OpenAI Official 配置，请在 Codex 中重新登录".to_string(),
        backup_id,
        &live,
        Some(&snapshot),
    )
}

pub(crate) fn reset_official_provider_inner(
    config_dir: Option<String>,
    model: Option<String>,
    config_text: Option<String>,
) -> Result<ActionResult> {
    let mut provider_rollback = None;
    let result =
        reset_official_provider_with_pre_persist(config_dir, model, config_text, |codex_dir| {
            provider_rollback = persist_detected_live_custom_provider(codex_dir)?;
            Ok(())
        });
    rollback_persisted_provider(result, provider_rollback)
}

fn merge_provider_toml_into_live(
    cfg: &Path,
    current_text: &str,
    provider_text: &str,
    explicit_api_key: Option<String>,
) -> Result<(DocumentMut, Option<String>)> {
    let source = parse_toml_document(cfg, provider_text)?;
    let model = string_value(&source, "model")
        .ok_or_else(|| CodexxError::Config("config.toml 必须包含 model".to_string()))?;
    let source_provider_id = string_value(&source, "model_provider")
        .ok_or_else(|| CodexxError::Config("config.toml 必须包含 model_provider".to_string()))?;
    let mut source_provider = source
        .get("model_providers")
        .and_then(|item| item.as_table())
        .and_then(|providers| providers.get(source_provider_id.as_str()))
        .and_then(|item| item.as_table())
        .cloned()
        .ok_or_else(|| {
            CodexxError::Config(format!(
                "config.toml 缺少 [model_providers.{source_provider_id}]"
            ))
        })?;
    let source_name = source_provider
        .get("name")
        .and_then(|item| item.as_str())
        .unwrap_or_default();
    if source_provider
        .get("base_url")
        .and_then(|item| item.as_str())
        .is_none_or(|value| value.trim().is_empty())
    {
        return Err(CodexxError::Config(
            "供应商配置必须包含非空 base_url".to_string(),
        ));
    }
    if is_placeholder_provider(
        source_name,
        source_provider
            .get("base_url")
            .and_then(|item| item.as_str())
            .unwrap_or_default(),
    ) {
        return Err(CodexxError::Config(
            "供应商名称和 base_url 不能使用示例占位值，请填写实际配置".to_string(),
        ));
    }

    let api_key = explicit_api_key
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| experimental_bearer_token_from_doc(&source, Some(source_provider_id.as_str())));
    let requires_openai_auth = source_provider
        .get("requires_openai_auth")
        .and_then(|item| item.as_bool())
        .unwrap_or(false);
    if requires_openai_auth && api_key.is_none() {
        return Err(CodexxError::Config(
            "该供应商需要 API Key，未切换且未修改 auth.json".to_string(),
        ));
    }
    source_provider.remove("experimental_bearer_token");

    // New and cc-switch imports carry a complete provider config. Treat it as
    // authoritative so provider-specific desktop/features/plugin settings make
    // the round trip. Older Codex-X records contained only the route/model;
    // those sparse templates inherit the current common settings for backward
    // compatibility.
    let has_multiple_provider_tables = source
        .get("model_providers")
        .and_then(|item| item.as_table())
        .is_some_and(|providers| providers.len() > 1);
    let has_complete_config = has_multiple_provider_tables
        || source
            .as_table()
            .iter()
            .any(|(key, _)| !matches!(key, "model_provider" | "model" | "model_providers"));
    let mut live = if has_complete_config {
        source
    } else {
        parse_toml_document(cfg, current_text)?
    };
    strip_provider_bearer_tokens(&mut live);
    live["model_provider"] = value("custom");
    live["model"] = value(model);
    let providers = ensure_table(live.as_table_mut(), "model_providers")?;
    providers.remove("custom");
    if source_provider_id != "custom" {
        providers.remove(&source_provider_id);
    }
    providers.insert("custom", Item::Table(source_provider));
    Ok((live, api_key))
}

fn save_provider_toml_config_locked<F>(
    codex_dir: &Path,
    input: ProviderTomlInput,
    old_config: Option<Vec<u8>>,
    pre_persist: F,
) -> Result<ActionResult>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let cfg = config_path(codex_dir);
    let old_auth = read_file_snapshot(&auth_path(codex_dir))?;
    let official_state = capture_live_official_state(codex_dir)?;
    let prepared = (|| -> Result<(Option<String>, String, String, LiveAuthAction)> {
        pre_persist(codex_dir)?;
        let backup_id = create_backup(codex_dir, "save-provider-toml")?;
        let current_text = text_from_snapshot(&cfg, old_config.as_deref())?;
        let (doc, api_key) = merge_provider_toml_into_live(
            &cfg,
            &current_text,
            input.config_text.trim_end(),
            input.api_key,
        )?;
        let provider_name = doc
            .get("model_providers")
            .and_then(|item| item.as_table())
            .and_then(|providers| providers.get("custom"))
            .and_then(|item| item.as_table())
            .and_then(|table| table.get("name"))
            .and_then(|item| item.as_str())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or("供应商");
        let message = format!("已切换到 {provider_name}");
        let auth_action = provider_auth_action(api_key.as_deref());
        let replacement = doc.to_string().trim_end().to_string() + "\n";
        Ok((backup_id, replacement, message, auth_action))
    })();
    let (backup_id, replacement, message, auth_action) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            return rollback_after_official_state_failure(error, None, official_state.as_ref())
        }
    };
    let live = match write_live_files(codex_dir, old_config, old_auth, &replacement, &auth_action) {
        Ok(live) => live,
        Err(error) => {
            return rollback_after_official_state_failure(error, None, official_state.as_ref())
        }
    };
    finish_live_action_with_official_state(
        codex_dir,
        message,
        backup_id,
        &live,
        official_state.as_ref(),
    )
}

pub(crate) fn save_provider_toml_config_with_pre_persist<F>(
    input: ProviderTomlInput,
    pre_persist: F,
) -> Result<ActionResult>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let codex_dir = resolve_codex_dir(input.config_dir.clone())?;
    ensure_directory(&codex_dir)?;
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    let old_config = read_file_snapshot(&config_path(&codex_dir))?;
    save_provider_toml_config_locked(&codex_dir, input, old_config, pre_persist)
}

pub(crate) fn save_provider_toml_config_inner(input: ProviderTomlInput) -> Result<ActionResult> {
    let mut provider_rollback = None;
    let result = save_provider_toml_config_with_pre_persist(input, |codex_dir| {
        provider_rollback = persist_detected_live_custom_provider(codex_dir)?;
        Ok(())
    });
    rollback_persisted_provider(result, provider_rollback)
}

fn switch_provider_locked<F>(
    codex_dir: &Path,
    input: ProviderInput,
    old_config: Option<Vec<u8>>,
    pre_persist: F,
) -> Result<ActionResult>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let provider_name = input.provider_name.trim();
    if input
        .provider_id
        .as_deref()
        .is_some_and(|provider_id| provider_id.trim().is_empty())
    {
        return Err(CodexxError::Config("供应商 ID 不能为空".to_string()));
    }
    // CC Switch uses a stable live key for all third-party providers. The
    // logical saved id remains in Codex-X storage and is matched by backend.
    let live_provider_key = "custom";
    let base_url = input.base_url.trim().trim_end_matches('/');
    let model = input.model.trim();
    if provider_name.is_empty() {
        return Err(CodexxError::Config("供应商名称不能为空".to_string()));
    }
    if base_url.is_empty() {
        return Err(CodexxError::Config("base_url 不能为空".to_string()));
    }
    if model.is_empty() {
        return Err(CodexxError::Config("model 不能为空".to_string()));
    }
    if is_placeholder_provider(provider_name, base_url) {
        return Err(CodexxError::Config(
            "供应商名称和 base_url 不能使用示例占位值，请填写实际配置".to_string(),
        ));
    }

    let cfg = config_path(codex_dir);
    let old_auth = read_file_snapshot(&auth_path(codex_dir))?;
    let official_state = capture_live_official_state(codex_dir)?;
    let prepared = (|| -> Result<(Option<String>, String, LiveAuthAction)> {
        pre_persist(codex_dir)?;
        let backup_id = create_backup(codex_dir, "switch-provider")?;
        let text = text_from_snapshot(&cfg, old_config.as_deref())?;
        let mut doc = parse_toml_document(&cfg, &text)?;
        strip_provider_bearer_tokens(&mut doc);
        doc["model_provider"] = value(live_provider_key);
        doc["model"] = value(model);
        let root = doc.as_table_mut();
        let providers = ensure_table(root, "model_providers")?;
        providers.remove(live_provider_key);
        let provider_table = ensure_table(providers, live_provider_key)?;
        provider_table["name"] = value(provider_name);
        provider_table["base_url"] = value(base_url);
        provider_table["wire_api"] =
            value(input.wire_api.unwrap_or_else(|| "responses".to_string()));
        let requires_openai_auth = input.requires_openai_auth.unwrap_or(true);
        provider_table["requires_openai_auth"] = value(requires_openai_auth);

        let api_key = input
            .api_key
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if requires_openai_auth && api_key.is_none() {
            return Err(CodexxError::Config(
                "该供应商需要 API Key，未切换且未修改 auth.json".to_string(),
            ));
        }
        let auth_action = provider_auth_action(api_key.as_deref());
        Ok((backup_id, doc.to_string(), auth_action))
    })();
    let (backup_id, replacement, auth_action) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            return rollback_after_official_state_failure(error, None, official_state.as_ref())
        }
    };
    let live = match write_live_files(codex_dir, old_config, old_auth, &replacement, &auth_action) {
        Ok(live) => live,
        Err(error) => {
            return rollback_after_official_state_failure(error, None, official_state.as_ref())
        }
    };
    finish_live_action_with_official_state(
        codex_dir,
        format!("已切换到 {provider_name}"),
        backup_id,
        &live,
        official_state.as_ref(),
    )
}

pub(crate) fn switch_provider_with_pre_persist<F>(
    input: ProviderInput,
    pre_persist: F,
) -> Result<ActionResult>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let codex_dir = resolve_codex_dir(input.config_dir.clone())?;
    ensure_directory(&codex_dir)?;
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    let old_config = read_file_snapshot(&config_path(&codex_dir))?;
    switch_provider_locked(&codex_dir, input, old_config, pre_persist)
}

pub(crate) fn switch_provider_inner(input: ProviderInput) -> Result<ActionResult> {
    let mut provider_rollback = None;
    let result = switch_provider_with_pre_persist(input, |codex_dir| {
        provider_rollback = persist_detected_live_custom_provider(codex_dir)?;
        Ok(())
    });
    rollback_persisted_provider(result, provider_rollback)
}

fn save_active_provider_with_apply<F>(
    provider: SavedProvider,
    config_dir: Option<String>,
    apply: F,
) -> Result<ActionResult>
where
    F: FnOnce(&SavedProvider, &Path, Option<Vec<u8>>) -> Result<ActionResult>,
{
    let provider = normalize_saved_provider(provider)?;
    let codex_dir = resolve_codex_dir(config_dir)?;
    ensure_directory(&codex_dir)?;
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    let active_config = read_file_snapshot(&config_path(&codex_dir))?;
    let conn = open_store()?;
    let saved_before = list_saved_providers_on_connection(&conn)?;
    let live = detected_live_custom_provider(&codex_dir)?.ok_or_else(|| {
        CodexxError::Config("当前不是可编辑的第三方供应商，未修改保存记录".to_string())
    })?;
    let matches = matching_saved_provider_ids_for_live(&live, &saved_before);
    match matches.as_slice() {
        [] => {
            if saved_before
                .iter()
                .any(|candidate| candidate.id == provider.id)
            {
                return Err(CodexxError::Config(format!(
                    "供应商 ID {} 已被另一条配置使用，请更换名称后再保存",
                    provider.id
                )));
            }
        }
        [active_id] if active_id == &provider.id => {}
        [active_id] => {
            return Err(CodexxError::Config(format!(
                "当前启用的是 {active_id}，不能把 {} 作为活动配置保存",
                provider.id
            )));
        }
        _ => {
            return Err(CodexxError::Config(
                "当前 live 供应商匹配到多条保存记录，请先清理重复配置".to_string(),
            ));
        }
    }

    let (saved, rollback) = save_provider_with_rollback_inner(provider)?;
    match apply(&saved, &codex_dir, active_config) {
        Ok(mut result) => {
            result.message = "供应商配置已保存并热更新".to_string();
            Ok(result)
        }
        Err(error) => match rollback_provider_store_inner(rollback) {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(CodexxError::Database(format!(
                "热更新供应商失败: {error}；数据库回滚也失败: {rollback_error}"
            ))),
        },
    }
}

pub(crate) fn save_active_provider_inner(
    provider: SavedProvider,
    config_dir: Option<String>,
) -> Result<ActionResult> {
    save_active_provider_with_apply(provider, config_dir, |saved, codex_dir, active_config| {
        if let Some(config_text) = saved.toml_config.clone() {
            save_provider_toml_config_locked(
                codex_dir,
                ProviderTomlInput {
                    config_dir: None,
                    config_text,
                    api_key: saved.api_key.clone(),
                },
                active_config,
                |_| Ok(()),
            )
        } else {
            switch_provider_locked(
                codex_dir,
                ProviderInput {
                    config_dir: None,
                    provider_id: Some(saved.id.clone()),
                    provider_name: saved.provider_name.clone(),
                    base_url: saved.base_url.clone(),
                    model: saved.model.clone(),
                    api_key: saved.api_key.clone(),
                    wire_api: Some(saved.wire_api.clone()),
                    requires_openai_auth: Some(saved.requires_openai_auth),
                },
                active_config,
                |_| Ok(()),
            )
        }
    })
}

pub(crate) fn delete_saved_provider_inner(id: &str, config_dir: Option<String>) -> Result<()> {
    let id = id.trim();
    if id.is_empty() {
        return Err(CodexxError::Config("供应商 ID 不能为空".to_string()));
    }
    let codex_dir = resolve_codex_dir(config_dir)?;
    let providers = list_saved_providers_inner()?;
    if let Some(live) = detected_live_custom_provider(&codex_dir)? {
        let active_ids = matching_saved_provider_ids_for_live(&live, &providers);
        if live.id == id || active_ids.iter().any(|active_id| active_id == id) {
            return Err(CodexxError::Config(
                "不能直接删除当前启用的供应商，请先切换到官方配置或其他供应商".to_string(),
            ));
        }
    }
    delete_provider_inner(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_io::{write_json, write_text};
    use crate::providers::delete_provider_inner;
    use crate::providers::{list_saved_providers_inner, save_provider_inner};
    use serde_json::json;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn active_provider_fixture(
        tag: u64,
        id: &str,
        name: &str,
        model: &str,
        key: &str,
    ) -> SavedProvider {
        let base_url = format!("https://active-{tag}.example.com/v1");
        SavedProvider {
            id: id.to_string(),
            provider_name: name.to_string(),
            base_url: base_url.clone(),
            model: model.to_string(),
            api_key: Some(key.to_string()),
            toml_config: Some(format!(
                r#"model_provider = "custom"
model = "{model}"

[model_providers.custom]
name = "{name}"
base_url = "{base_url}"
wire_api = "responses"
requires_openai_auth = false
"#
            )),
            wire_api: "responses".to_string(),
            requires_openai_auth: false,
        }
    }

    fn active_provider_test_dir(label: &str, tag: u64) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "codex-x-active-provider-{label}-{}-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create active provider test directory");
        path
    }

    #[test]
    fn removing_auth_uses_the_target_route_write_order() {
        let official = b"model_provider = \"openai\"\nmodel = \"official-model\"\n";
        let proxy = b"model_provider = \"custom\"\nmodel = \"proxy-model\"\n";

        assert_eq!(
            removal_write_order(Some(official)),
            LiveWriteOrder::ConfigFirst
        );
        assert_eq!(removal_write_order(Some(proxy)), LiveWriteOrder::AuthFirst);
    }

    #[test]
    fn replacing_auth_publishes_official_route_before_credential() {
        let codex_dir = active_provider_test_dir("official-before-official-auth", 29_996);
        let old_config = "model_provider = \"custom\"\nmodel = \"proxy-model\"\n";
        let new_config = "model_provider = \"openai\"\nmodel = \"official-model\"\n";
        let old_auth = json!({"OPENAI_API_KEY": "proxy-key"});
        let new_auth = json!({
            "auth_mode": "chatgpt",
            "tokens": {"access_token": "official-access"}
        });
        write_text(&config_path(&codex_dir), old_config).expect("write old config");
        write_json(&auth_path(&codex_dir), &old_auth).expect("write old auth");

        let old_config_snapshot =
            read_file_snapshot(&config_path(&codex_dir)).expect("snapshot old config");
        let old_auth_snapshot =
            read_file_snapshot(&auth_path(&codex_dir)).expect("snapshot old auth");
        write_live_files_with_between_writes(
            &codex_dir,
            old_config_snapshot,
            old_auth_snapshot,
            new_config,
            &LiveAuthAction::Replace(new_auth.clone()),
            || {
                assert_eq!(
                    fs::read_to_string(config_path(&codex_dir)).expect("read route between writes"),
                    new_config
                );
                assert_eq!(
                    serde_json::from_slice::<Value>(
                        &fs::read(auth_path(&codex_dir)).expect("read auth between writes")
                    )
                    .expect("parse auth between writes"),
                    old_auth
                );
                Ok(())
            },
        )
        .expect("replace live files");

        assert_eq!(
            fs::read_to_string(config_path(&codex_dir)).expect("read final config"),
            new_config
        );
        assert_eq!(
            serde_json::from_slice::<Value>(
                &fs::read(auth_path(&codex_dir)).expect("read final auth")
            )
            .expect("parse final auth"),
            new_auth
        );
        fs::remove_dir_all(codex_dir).expect("remove official-before-official-auth test directory");
    }

    #[test]
    fn replacing_auth_publishes_proxy_credential_before_route() {
        let codex_dir = active_provider_test_dir("proxy-before-global-auth", 29_997);
        let old_config = "model_provider = \"openai\"\nmodel = \"official-model\"\n";
        let new_config = r#"model_provider = "custom"
model = "proxy-model"

[model_providers.custom]
base_url = "https://proxy.example.com/v1"
"#;
        let old_auth = json!({"OPENAI_API_KEY": "official-key"});
        let new_auth = json!({"OPENAI_API_KEY": "proxy-key"});
        write_text(&config_path(&codex_dir), old_config).expect("write old config");
        write_json(&auth_path(&codex_dir), &old_auth).expect("write old auth");

        let old_config_snapshot =
            read_file_snapshot(&config_path(&codex_dir)).expect("snapshot old config");
        let old_auth_snapshot =
            read_file_snapshot(&auth_path(&codex_dir)).expect("snapshot old auth");
        write_live_files_with_between_writes(
            &codex_dir,
            old_config_snapshot,
            old_auth_snapshot,
            new_config,
            &LiveAuthAction::Replace(new_auth.clone()),
            || {
                assert_eq!(
                    fs::read_to_string(config_path(&codex_dir)).expect("read route between writes"),
                    old_config
                );
                assert_eq!(
                    serde_json::from_slice::<Value>(
                        &fs::read(auth_path(&codex_dir)).expect("read auth between writes")
                    )
                    .expect("parse auth between writes"),
                    new_auth
                );
                Ok(())
            },
        )
        .expect("replace live files");

        assert_eq!(
            serde_json::from_slice::<Value>(
                &fs::read(auth_path(&codex_dir)).expect("read final auth")
            )
            .expect("parse final auth"),
            new_auth
        );
        fs::remove_dir_all(codex_dir).expect("remove proxy-before-global-auth test directory");
    }

    #[test]
    fn removing_auth_publishes_official_route_before_deleting_credential() {
        let codex_dir = active_provider_test_dir("official-before-auth-remove", 29_998);
        let old_config = "model_provider = \"custom\"\nmodel = \"proxy-model\"\n";
        let new_config = "model_provider = \"openai\"\nmodel = \"official-model\"\n";
        let old_auth = json!({"OPENAI_API_KEY": "proxy-key"});
        write_text(&config_path(&codex_dir), old_config).expect("write old config");
        write_json(&auth_path(&codex_dir), &old_auth).expect("write old auth");

        let old_config_snapshot =
            read_file_snapshot(&config_path(&codex_dir)).expect("snapshot old config");
        let old_auth_snapshot =
            read_file_snapshot(&auth_path(&codex_dir)).expect("snapshot old auth");
        let applied = write_live_files_with_between_writes(
            &codex_dir,
            old_config_snapshot,
            old_auth_snapshot,
            new_config,
            &LiveAuthAction::Remove,
            || {
                assert_eq!(
                    fs::read_to_string(config_path(&codex_dir)).expect("read route between writes"),
                    new_config
                );
                assert_eq!(
                    serde_json::from_slice::<Value>(
                        &fs::read(auth_path(&codex_dir)).expect("read auth between writes")
                    )
                    .expect("parse auth between writes"),
                    old_auth
                );
                Ok(())
            },
        )
        .expect("remove live auth");

        assert!(!auth_path(&codex_dir).exists());
        applied.rollback().expect("roll back removed auth");
        assert_eq!(
            fs::read_to_string(config_path(&codex_dir)).expect("read rolled back proxy config"),
            old_config
        );
        assert_eq!(
            serde_json::from_slice::<Value>(
                &fs::read(auth_path(&codex_dir)).expect("read rolled back proxy auth")
            )
            .expect("parse rolled back proxy auth"),
            old_auth
        );
        fs::remove_dir_all(codex_dir).expect("remove official-before-auth-remove test directory");
    }

    #[test]
    fn removing_auth_deletes_official_credential_before_publishing_proxy_route() {
        let codex_dir = active_provider_test_dir("auth-remove-before-proxy", 30_008);
        let old_config = "model_provider = \"openai\"\nmodel = \"official-model\"\n";
        let new_config = r#"model_provider = "custom"
model = "proxy-model"

[model_providers.custom]
base_url = "https://proxy.example.com/v1"
"#;
        let old_auth = json!({
            "auth_mode": "chatgpt",
            "tokens": {"access_token": "official-access"}
        });
        write_text(&config_path(&codex_dir), old_config).expect("write old config");
        write_json(&auth_path(&codex_dir), &old_auth).expect("write old auth");

        let old_config_snapshot =
            read_file_snapshot(&config_path(&codex_dir)).expect("snapshot old config");
        let old_auth_snapshot =
            read_file_snapshot(&auth_path(&codex_dir)).expect("snapshot old auth");
        let applied = write_live_files_with_between_writes(
            &codex_dir,
            old_config_snapshot,
            old_auth_snapshot,
            new_config,
            &LiveAuthAction::Remove,
            || {
                assert_eq!(
                    fs::read_to_string(config_path(&codex_dir)).expect("read route between writes"),
                    old_config
                );
                assert!(!auth_path(&codex_dir).exists());
                Ok(())
            },
        )
        .expect("remove live auth before proxy route");

        assert_eq!(
            fs::read_to_string(config_path(&codex_dir)).expect("read final proxy config"),
            new_config
        );
        assert!(!auth_path(&codex_dir).exists());

        applied.rollback().expect("roll back proxy switch");
        assert_eq!(
            fs::read_to_string(config_path(&codex_dir)).expect("read rolled back official config"),
            old_config
        );
        assert_eq!(
            serde_json::from_slice::<Value>(
                &fs::read(auth_path(&codex_dir)).expect("read rolled back official auth")
            )
            .expect("parse rolled back official auth"),
            old_auth
        );
        fs::remove_dir_all(codex_dir).expect("remove auth-remove-before-proxy test directory");
    }

    #[test]
    fn concurrent_auth_change_blocks_config_and_preserves_external_auth() {
        let codex_dir = active_provider_test_dir("config-first-rollback", 29_999);
        let old_config = "model_provider = \"openai\"\nmodel = \"official-model\"\n";
        let old_auth = json!({
            "auth_mode": "chatgpt",
            "tokens": {"access_token": "official-access"}
        });
        let external_auth = json!({"OPENAI_API_KEY": "external-key"});
        write_text(&config_path(&codex_dir), old_config).expect("write old config");
        write_json(&auth_path(&codex_dir), &old_auth).expect("write old auth");

        let old_config_snapshot =
            read_file_snapshot(&config_path(&codex_dir)).expect("snapshot old config");
        let old_auth_snapshot =
            read_file_snapshot(&auth_path(&codex_dir)).expect("snapshot old auth");
        let error = write_live_files_with_between_writes(
            &codex_dir,
            old_config_snapshot,
            old_auth_snapshot,
            "model_provider = \"custom\"\nmodel = \"proxy-model\"\n",
            &LiveAuthAction::Replace(json!({"OPENAI_API_KEY": "proxy-key"})),
            || write_json(&auth_path(&codex_dir), &external_auth),
        )
        .expect_err("stale auth must fail after config write");

        assert!(error.to_string().contains("已被其他程序修改"));
        assert_eq!(
            fs::read_to_string(config_path(&codex_dir)).expect("read rolled back config"),
            old_config
        );
        assert_eq!(
            serde_json::from_slice::<Value>(
                &fs::read(auth_path(&codex_dir)).expect("read rolled back auth")
            )
            .expect("parse external auth"),
            external_auth
        );
        fs::remove_dir_all(codex_dir).expect("remove config-first-rollback test directory");
    }

    #[test]
    fn config_failure_rolls_back_auth_first_write() {
        let codex_dir = active_provider_test_dir("auth-first-rollback", 30_007);
        let old_config = "model_provider = \"openai\"\nmodel = \"official-model\"\n";
        let old_auth = json!({
            "auth_mode": "chatgpt",
            "tokens": {"access_token": "official-access"}
        });
        let external_config = "model_provider = \"external\"\nmodel = \"external-model\"\n";
        write_text(&config_path(&codex_dir), old_config).expect("write old config");
        write_json(&auth_path(&codex_dir), &old_auth).expect("write old auth");

        let old_config_snapshot =
            read_file_snapshot(&config_path(&codex_dir)).expect("snapshot old config");
        let old_auth_snapshot =
            read_file_snapshot(&auth_path(&codex_dir)).expect("snapshot old auth");
        let error = write_live_files_with_between_writes(
            &codex_dir,
            old_config_snapshot,
            old_auth_snapshot,
            "model_provider = \"custom\"\nmodel = \"proxy-model\"\n",
            &LiveAuthAction::Replace(json!({"OPENAI_API_KEY": "proxy-key"})),
            || write_text(&config_path(&codex_dir), external_config),
        )
        .expect_err("stale config must fail after auth write");

        assert!(error.to_string().contains("已被其他程序修改"));
        assert_eq!(
            fs::read_to_string(config_path(&codex_dir)).expect("read external config"),
            external_config
        );
        assert_eq!(
            serde_json::from_slice::<Value>(
                &fs::read(auth_path(&codex_dir)).expect("read rolled back auth")
            )
            .expect("parse rolled back auth"),
            old_auth
        );
        fs::remove_dir_all(codex_dir).expect("remove auth-first rollback test directory");
    }

    #[test]
    fn direct_switch_rejects_placeholder_provider_values() {
        let codex_dir = active_provider_test_dir("placeholder", 30_000);
        let error = switch_provider_inner(ProviderInput {
            config_dir: Some(codex_dir.display().to_string()),
            provider_id: Some("placeholder".to_string()),
            provider_name: "your-provider".to_string(),
            base_url: "https://example.com/v1".to_string(),
            model: "gpt-5.5".to_string(),
            api_key: None,
            wire_api: Some("responses".to_string()),
            requires_openai_auth: Some(false),
        })
        .expect_err("placeholder provider must be rejected");
        assert!(error.to_string().contains("示例占位值"));
        assert!(!config_path(&codex_dir).exists());
        fs::remove_dir_all(codex_dir).expect("remove placeholder test directory");
    }

    #[test]
    fn complete_provider_template_replaces_live_common_config() {
        let current = r#"# keep-live-comment
model_provider = "custom"
model = "model-a"
approval_policy = "never"
service_tier = "priority"

[model_providers.custom]
name = "Provider A"
base_url = "https://a.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "sk-a"

[features]
js_repl = false

[mcp_servers.live]
command = "live-server"

[projects."/live/project"]
trust_level = "trusted"
"#;
        let historical_template = r#"model_provider = "saved-provider"
model = "model-b"
approval_policy = "on-request"
service_tier = "flex"

[model_providers.saved-provider]
name = "Provider B"
base_url = "https://b.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "sk-stale-template"
request_max_retries = 9

[features]
js_repl = true

[mcp_servers.stale]
command = "stale-server"

[projects."/stale/project"]
trust_level = "untrusted"
"#;

        let (merged, api_key) = merge_provider_toml_into_live(
            Path::new("config.toml"),
            current,
            historical_template,
            Some("sk-b".to_string()),
        )
        .expect("merge provider template");
        let text = merged.to_string();

        assert!(!text.contains("# keep-live-comment"));
        assert_eq!(merged["model_provider"].as_str(), Some("custom"));
        assert_eq!(merged["model"].as_str(), Some("model-b"));
        assert_eq!(merged["approval_policy"].as_str(), Some("on-request"));
        assert_eq!(merged["service_tier"].as_str(), Some("flex"));
        assert!(merged.get("model_reasoning_effort").is_none());
        assert!(merged.get("disable_response_storage").is_none());
        assert_eq!(merged["features"]["js_repl"].as_bool(), Some(true));
        assert_eq!(
            merged["mcp_servers"]["stale"]["command"].as_str(),
            Some("stale-server")
        );
        assert!(merged["mcp_servers"].get("live").is_none());
        assert_eq!(
            merged["projects"]["/stale/project"]["trust_level"].as_str(),
            Some("untrusted")
        );
        assert!(merged["projects"].get("/live/project").is_none());
        assert_eq!(
            merged["model_providers"]["custom"]["name"].as_str(),
            Some("Provider B")
        );
        assert_eq!(
            merged["model_providers"]["custom"]["request_max_retries"].as_integer(),
            Some(9)
        );
        assert!(merged["model_providers"]["custom"]
            .get("experimental_bearer_token")
            .is_none());
        assert_eq!(api_key.as_deref(), Some("sk-b"));
        assert!(!text.contains("sk-a"));
        assert!(!text.contains("sk-stale-template"));
    }

    #[test]
    fn provider_toml_draft_preserves_full_config_without_writing_live_files() {
        let codex_dir = active_provider_test_dir("full-draft", 40_001);
        let current = r#"# keep-draft-comment
model_provider = "custom"
model = "model-a"
model_reasoning_effort = "xhigh"
experimental_bearer_token = "sk-top-level"

[model_providers.custom]
name = "Provider A"
base_url = "https://a.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "sk-a"
request_max_retries = 5

[model_providers.other]
name = "Other"
base_url = "https://other.example.com/v1"
experimental_bearer_token = "sk-other"

[projects."/work/project"]
trust_level = "trusted"

[plugins."browser@openai-bundled"]
enabled = true

[features]
js_repl = false

[mcp_servers.docs]
command = "docs-server"
"#;
        let auth = br#"{"auth_mode":"chatgpt","tokens":{"access_token":"official-token"}}"#;
        write_text(&config_path(&codex_dir), current).expect("write full live config");
        fs::write(auth_path(&codex_dir), auth).expect("write live auth");
        let config_before = fs::read(config_path(&codex_dir)).expect("snapshot config");
        let auth_before = fs::read(auth_path(&codex_dir)).expect("snapshot auth");

        let draft = build_provider_toml_draft_inner(
            SavedProvider {
                id: "provider-b".to_string(),
                provider_name: "Provider B".to_string(),
                base_url: "https://b.example.com/v1/".to_string(),
                model: "model-b".to_string(),
                api_key: Some("sk-b".to_string()),
                toml_config: None,
                wire_api: "responses".to_string(),
                requires_openai_auth: false,
            },
            Some(codex_dir.display().to_string()),
        )
        .expect("build full provider TOML draft");
        let doc = draft
            .parse::<DocumentMut>()
            .expect("parse full provider TOML draft");

        assert_eq!(
            fs::read(config_path(&codex_dir)).expect("read unchanged config"),
            config_before
        );
        assert_eq!(
            fs::read(auth_path(&codex_dir)).expect("read unchanged auth"),
            auth_before
        );
        assert!(draft.contains("# keep-draft-comment"));
        assert_eq!(doc["model"].as_str(), Some("model-b"));
        assert_eq!(doc["model_reasoning_effort"].as_str(), Some("xhigh"));
        assert_eq!(
            doc["model_providers"]["custom"]["name"].as_str(),
            Some("Provider B")
        );
        assert_eq!(
            doc["model_providers"]["custom"]["base_url"].as_str(),
            Some("https://b.example.com/v1")
        );
        assert_eq!(
            doc["model_providers"]["custom"]["request_max_retries"].as_integer(),
            Some(5)
        );
        assert_eq!(
            doc["projects"]["/work/project"]["trust_level"].as_str(),
            Some("trusted")
        );
        assert_eq!(
            doc["plugins"]["browser@openai-bundled"]["enabled"].as_bool(),
            Some(true)
        );
        assert_eq!(doc["features"]["js_repl"].as_bool(), Some(false));
        assert_eq!(
            doc["mcp_servers"]["docs"]["command"].as_str(),
            Some("docs-server")
        );
        assert!(doc.get("experimental_bearer_token").is_none());
        assert!(doc["model_providers"]
            .as_table()
            .expect("model providers table")
            .iter()
            .all(|(_, item)| item
                .as_table()
                .is_none_or(|table| table.get("experimental_bearer_token").is_none())));
        assert!(!draft.contains("sk-b"));

        fs::remove_dir_all(codex_dir).expect("remove draft test directory");
    }

    fn write_active_provider_files(codex_dir: &Path, provider: &SavedProvider) -> Value {
        let token = provider.api_key.as_deref().expect("provider key");
        let mut doc = provider
            .toml_config
            .as_deref()
            .expect("provider TOML")
            .parse::<DocumentMut>()
            .expect("parse live config");
        doc["approval_policy"] = value("never");
        let mcp_servers =
            ensure_table(doc.as_table_mut(), "mcp_servers").expect("create MCP provider table");
        let docs = ensure_table(mcp_servers, "docs").expect("create MCP docs table");
        docs["command"] = value("docs-server");
        write_text(&config_path(codex_dir), &doc.to_string()).expect("write live config");
        let auth = json!({"OPENAI_API_KEY": token});
        write_json(&auth_path(codex_dir), &auth).expect("write live auth");
        auth
    }

    fn saved_provider(id: &str) -> SavedProvider {
        list_saved_providers_inner()
            .expect("list saved providers")
            .into_iter()
            .find(|provider| provider.id == id)
            .expect("saved provider")
    }

    #[test]
    fn active_provider_save_updates_one_record_and_hot_applies() {
        let _db_guard = crate::app_db::test_db_guard();
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let tag = COUNTER.fetch_add(1, Ordering::Relaxed) + 10_000;
        let id = format!("active-save-{tag}");
        let codex_dir = active_provider_test_dir("success", tag);
        let original = active_provider_fixture(tag, &id, "Before", "model-before", "sk-before");
        save_provider_inner(original.clone()).expect("save original provider");
        write_active_provider_files(&codex_dir, &original);

        let updated = active_provider_fixture(tag, &id, "After", "model-after", "sk-after");
        let result = save_active_provider_inner(updated, Some(codex_dir.display().to_string()))
            .expect("save active provider");

        assert!(!result.state.is_official_provider);
        let state = serde_json::to_value(&result.state).expect("serialize updated state");
        assert_eq!(state["activeSavedProviderId"].as_str(), Some(id.as_str()),);
        let live = fs::read_to_string(config_path(&codex_dir)).expect("read updated live config");
        let doc = live
            .parse::<DocumentMut>()
            .expect("parse updated live config");
        assert_eq!(doc["model"].as_str(), Some("model-after"));
        assert_eq!(doc["approval_policy"].as_str(), Some("never"));
        assert_eq!(
            doc["mcp_servers"]["docs"]["command"].as_str(),
            Some("docs-server")
        );
        assert!(doc["model_providers"]["custom"]
            .get("experimental_bearer_token")
            .is_none());
        assert_eq!(saved_provider(&id).provider_name, "After");
        let auth_after: Value = serde_json::from_str(
            &fs::read_to_string(auth_path(&codex_dir)).expect("read official auth"),
        )
        .expect("parse official auth");
        assert_eq!(auth_after, json!({"OPENAI_API_KEY": "sk-after"}));
        let delete_error = delete_saved_provider_inner(&id, Some(codex_dir.display().to_string()))
            .expect_err("active provider deletion must be blocked");
        assert!(delete_error.to_string().contains("不能直接删除当前启用"));

        delete_provider_inner(&id).expect("delete test provider");
        fs::remove_dir_all(codex_dir).expect("remove active provider test directory");
    }

    #[test]
    fn active_provider_save_rolls_back_record_when_apply_fails_before_writing() {
        let _db_guard = crate::app_db::test_db_guard();
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let tag = COUNTER.fetch_add(1, Ordering::Relaxed) + 20_000;
        let id = format!("active-rollback-{tag}");
        let codex_dir = active_provider_test_dir("rollback", tag);
        let original = active_provider_fixture(tag, &id, "Before", "model-before", "sk-before");
        save_provider_inner(original.clone()).expect("save original provider");
        write_active_provider_files(&codex_dir, &original);
        let config_before = fs::read(config_path(&codex_dir)).expect("snapshot config");
        let auth_before = fs::read(auth_path(&codex_dir)).expect("snapshot auth");

        let updated =
            active_provider_fixture(tag, &id, "Must Roll Back", "model-after", "sk-after");
        let error = save_active_provider_with_apply(
            updated,
            Some(codex_dir.display().to_string()),
            |_, _, _| Err(CodexxError::Config("injected apply failure".to_string())),
        )
        .expect_err("injected apply failure must roll back");

        assert!(error.to_string().contains("injected apply failure"));
        assert_eq!(saved_provider(&id).provider_name, "Before");
        assert_eq!(
            fs::read(config_path(&codex_dir)).expect("read rolled back config"),
            config_before
        );
        assert_eq!(
            fs::read(auth_path(&codex_dir)).expect("read rolled back auth"),
            auth_before
        );

        delete_provider_inner(&id).expect("delete test provider");
        fs::remove_dir_all(codex_dir).expect("remove active provider test directory");
    }

    #[test]
    fn provider_switch_rolls_back_config_when_auth_changes_concurrently() {
        let tag = 30_004;
        let codex_dir = active_provider_test_dir("post-write-state-failure", tag);
        let original =
            active_provider_fixture(tag, "custom", "Before", "model-before", "sk-before");
        write_active_provider_files(&codex_dir, &original);
        let config_before = fs::read(config_path(&codex_dir)).expect("snapshot live config");
        let external_auth = json!({"OPENAI_API_KEY": "external-change"});

        let error = switch_provider_with_pre_persist(
            ProviderInput {
                config_dir: Some(codex_dir.display().to_string()),
                provider_id: Some("next".to_string()),
                provider_name: "Next".to_string(),
                base_url: "https://next.example.com/v1".to_string(),
                model: "model-next".to_string(),
                api_key: Some("sk-next".to_string()),
                wire_api: Some("responses".to_string()),
                requires_openai_auth: Some(false),
            },
            |dir| write_json(&auth_path(dir), &external_auth),
        )
        .expect_err("stale auth write must roll config back");

        assert!(error.to_string().contains("已被其他程序修改"));
        assert_eq!(
            fs::read(config_path(&codex_dir)).expect("read rolled back live config"),
            config_before
        );
        assert_eq!(
            serde_json::from_slice::<Value>(
                &fs::read(auth_path(&codex_dir)).expect("read external auth")
            )
            .expect("parse external auth"),
            external_auth
        );

        fs::remove_dir_all(codex_dir).expect("remove state failure test directory");
    }

    #[test]
    fn active_provider_edit_repairs_malformed_auth_and_updates_database() {
        let _db_guard = crate::app_db::test_db_guard();
        let tag = 30_005;
        let id = format!("active-post-write-rollback-{tag}");
        let codex_dir = active_provider_test_dir("active-post-write-state-failure", tag);
        let original = active_provider_fixture(tag, &id, "Before", "model-before", "sk-before");
        save_provider_inner(original.clone()).expect("save original provider");
        write_active_provider_files(&codex_dir, &original);
        fs::write(auth_path(&codex_dir), b"{malformed-auth").expect("write malformed live auth");

        let updated = active_provider_fixture(tag, &id, "After", "model-after", "sk-after");
        let result = save_active_provider_inner(updated, Some(codex_dir.display().to_string()))
            .expect("provider save should replace malformed auth");

        assert_eq!(result.state.model.as_deref(), Some("model-after"));
        let saved = saved_provider(&id);
        assert_eq!(saved.provider_name, "After");
        assert_eq!(saved.model, "model-after");
        assert_eq!(saved.api_key.as_deref(), Some("sk-after"));
        assert!(fs::read_to_string(config_path(&codex_dir))
            .expect("read updated live config")
            .contains("model = \"model-after\""));
        assert_eq!(
            serde_json::from_slice::<Value>(
                &fs::read(auth_path(&codex_dir)).expect("read repaired auth")
            )
            .expect("parse repaired auth"),
            json!({"OPENAI_API_KEY": "sk-after"})
        );

        delete_provider_inner(&id).expect("delete test provider");
        fs::remove_dir_all(codex_dir).expect("remove state failure test directory");
    }

    #[test]
    fn live_config_lock_rejects_a_second_writer() {
        let codex_dir = active_provider_test_dir("lock", 30_001);
        ensure_directory(&codex_dir).expect("create test Codex directory");

        let first = acquire_live_config_lock(&codex_dir).expect("acquire first live lock");
        let error = acquire_live_config_lock(&codex_dir)
            .err()
            .expect("second live lock must fail");
        assert!(error.to_string().contains("另一个 Codex-X"));
        drop(first);
        acquire_live_config_lock(&codex_dir).expect("lock is released on drop");

        fs::remove_dir_all(codex_dir).expect("remove live lock test directory");
    }

    #[test]
    fn provider_switch_rejects_a_stale_read_and_preserves_external_toml() {
        let codex_dir = active_provider_test_dir("stale-config", 30_002);
        ensure_directory(&codex_dir).expect("create test Codex directory");
        let cfg = config_path(&codex_dir);
        write_text(
            &cfg,
            "model_provider = \"custom\"\nmodel = \"before\"\n\n[model_providers.custom]\nname = \"Before\"\nbase_url = \"https://before.example/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = false\n",
        )
        .expect("write initial config");
        let external = "model = \"external-change\"\napproval_policy = \"never\"\n";

        let error = switch_provider_with_pre_persist(
            ProviderInput {
                config_dir: Some(codex_dir.display().to_string()),
                provider_id: Some("next".to_string()),
                provider_name: "Next".to_string(),
                base_url: "https://next.example/v1".to_string(),
                model: "next-model".to_string(),
                api_key: Some("sk-next".to_string()),
                wire_api: Some("responses".to_string()),
                requires_openai_auth: Some(false),
            },
            |dir| write_text(&config_path(dir), external),
        )
        .expect_err("stale config write must be rejected");

        assert!(error.to_string().contains("已被其他程序修改"));
        assert_eq!(
            fs::read_to_string(&cfg).expect("read externally changed config"),
            external
        );
        fs::remove_dir_all(codex_dir).expect("remove stale config test directory");
    }

    #[test]
    fn active_provider_edit_rejects_live_change_after_detection_and_restores_record() {
        let _db_guard = crate::app_db::test_db_guard();
        let tag = 30_006;
        let id = format!("active-stale-edit-{tag}");
        let codex_dir = active_provider_test_dir("active-stale-edit", tag);
        let original = active_provider_fixture(tag, &id, "Before", "model-before", "sk-before");
        save_provider_inner(original.clone()).expect("save original provider");
        write_active_provider_files(&codex_dir, &original);
        let external = r#"model_provider = "custom"
model = "external-model"

[model_providers.custom]
name = "External"
base_url = "https://external.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "sk-external"
"#;
        let updated = active_provider_fixture(tag, &id, "After", "model-after", "sk-after");

        let error = save_active_provider_with_apply(
            updated,
            Some(codex_dir.display().to_string()),
            |saved, codex_dir, active_config| {
                write_text(&config_path(codex_dir), external)?;
                save_provider_toml_config_locked(
                    codex_dir,
                    ProviderTomlInput {
                        config_dir: None,
                        config_text: saved.toml_config.clone().expect("provider TOML"),
                        api_key: saved.api_key.clone(),
                    },
                    active_config,
                    |_| Ok(()),
                )
            },
        )
        .expect_err("live provider change must reject stale active edit");

        assert!(error.to_string().contains("已被其他程序修改"));
        assert_eq!(
            fs::read_to_string(config_path(&codex_dir)).expect("read external live config"),
            external
        );
        assert_eq!(saved_provider(&id).provider_name, original.provider_name);

        delete_provider_inner(&id).expect("delete test provider");
        fs::remove_dir_all(codex_dir).expect("remove stale active edit test directory");
    }

    #[test]
    fn detected_live_provider_can_be_adopted_saved_and_hot_applied() {
        let _db_guard = crate::app_db::test_db_guard();
        let tag = 30_003;
        let id = format!("detected-adopt-{tag}");
        let codex_dir = active_provider_test_dir("detected-adopt", tag);
        let live = active_provider_fixture(tag, "custom", "Detected", "before", "sk-before");
        write_active_provider_files(&codex_dir, &live);

        let adopted = active_provider_fixture(tag, &id, "Adopted", "after", "sk-after");
        let result = save_active_provider_inner(adopted, Some(codex_dir.display().to_string()))
            .expect("adopt detected live provider");
        assert_eq!(result.state.model.as_deref(), Some("after"));
        assert_eq!(saved_provider(&id).provider_name, "Adopted");

        delete_provider_inner(&id).expect("delete adopted provider");
        fs::remove_dir_all(codex_dir).expect("remove detected provider test directory");
    }

    fn official_account_test_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let tag = COUNTER.fetch_add(1, Ordering::Relaxed);
        active_provider_test_dir(label, 70_000 + tag)
    }

    fn official_account_config(model: &str) -> String {
        format!("model_provider = \"openai\"\nmodel = \"{model}\"\n")
    }

    fn official_account_auth(access_token: &str, refresh_token: &str) -> Value {
        json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": access_token,
                "refresh_token": refresh_token,
                "id_token": format!("test-id-{access_token}")
            }
        })
    }

    fn write_official_account_live(codex_dir: &Path, model: &str, access_token: &str) {
        write_text(&config_path(codex_dir), &official_account_config(model))
            .expect("write official account config");
        write_json(
            &auth_path(codex_dir),
            &official_account_auth(access_token, &format!("test-refresh-{access_token}")),
        )
        .expect("write official account auth");
    }

    fn capture_test_account(codex_dir: &Path, name: &str) -> String {
        capture_current_official_account_inner(
            Some(codex_dir.display().to_string()),
            name.to_string(),
        )
        .expect("capture official account");
        list_official_accounts_inner(Some(codex_dir.display().to_string()))
            .expect("list official accounts")
            .into_iter()
            .find(|account| account.name == name)
            .expect("captured account")
            .id
    }

    fn prepare_and_capture_test_account(
        codex_dir: &Path,
        name: &str,
        model: &str,
        access_token: &str,
    ) -> String {
        prepare_new_official_account_inner(Some(codex_dir.display().to_string()))
            .expect("prepare another official account");
        write_official_account_live(codex_dir, model, access_token);
        capture_test_account(codex_dir, name)
    }

    fn read_live_auth(codex_dir: &Path) -> Value {
        serde_json::from_slice(&fs::read(auth_path(codex_dir)).expect("read live auth"))
            .expect("parse live auth")
    }

    fn remove_official_account_test_files(codex_dir: &Path) {
        if let Ok(path) = official_accounts_path(codex_dir) {
            let _ = fs::remove_file(path);
        }
        if let Ok(path) = official_snapshot_path(codex_dir) {
            let _ = fs::remove_file(path);
        }
        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn official_accounts_are_isolated_by_codex_home() {
        let first = official_account_test_dir("official-isolation-a");
        let second = official_account_test_dir("official-isolation-b");
        write_official_account_live(&first, "model-a", "test-access-a1");
        write_official_account_live(&second, "model-b", "test-access-b1");
        capture_test_account(&first, "Account A");
        capture_test_account(&second, "Account B");

        let first_list = list_official_accounts_inner(Some(first.display().to_string())).unwrap();
        let second_list = list_official_accounts_inner(Some(second.display().to_string())).unwrap();
        assert_eq!(first_list.len(), 1);
        assert_eq!(first_list[0].name, "Account A");
        assert_eq!(second_list.len(), 1);
        assert_eq!(second_list[0].name, "Account B");
        assert_ne!(
            official_accounts_path(&first).unwrap(),
            official_accounts_path(&second).unwrap()
        );
        remove_official_account_test_files(&first);
        remove_official_account_test_files(&second);
    }

    #[test]
    fn creating_two_official_accounts_lists_both_without_auth() {
        let dir = official_account_test_dir("official-list-two");
        write_official_account_live(&dir, "model-a", "test-access-a1");
        capture_test_account(&dir, "Account A");
        prepare_and_capture_test_account(&dir, "Account B", "model-b", "test-access-b1");

        let summaries = list_official_accounts_inner(Some(dir.display().to_string())).unwrap();
        assert_eq!(summaries.len(), 2);
        let serialized = serde_json::to_string(&summaries).unwrap();
        assert!(!serialized.contains("test-access"));
        assert!(!serialized.contains("refresh_token"));
        remove_official_account_test_files(&dir);
    }

    #[test]
    fn renaming_official_account_preserves_stable_id() {
        let dir = official_account_test_dir("official-rename");
        write_official_account_live(&dir, "model-a", "test-access-a1");
        let id = capture_test_account(&dir, "Before");
        let draft =
            get_official_account_inner(Some(dir.display().to_string()), id.clone()).unwrap();
        update_official_account_inner(OfficialAccountUpdateInput {
            config_dir: Some(dir.display().to_string()),
            account_id: id.clone(),
            name: "After".to_string(),
            model: draft.model,
            config_text: draft.config_text,
            auth_json: draft.auth_json,
        })
        .expect("rename account");
        let summaries = list_official_accounts_inner(Some(dir.display().to_string())).unwrap();
        assert_eq!(summaries[0].id, id);
        assert_eq!(summaries[0].name, "After");
        remove_official_account_test_files(&dir);
    }

    #[test]
    fn deleting_account_b_does_not_change_account_a() {
        let dir = official_account_test_dir("official-delete-b");
        write_official_account_live(&dir, "model-a", "test-access-a1");
        let account_a = capture_test_account(&dir, "Account A");
        let account_b =
            prepare_and_capture_test_account(&dir, "Account B", "model-b", "test-access-b1");
        let before_a =
            get_official_account_inner(Some(dir.display().to_string()), account_a.clone()).unwrap();
        delete_official_account_inner(Some(dir.display().to_string()), account_b)
            .expect("delete account B");
        let after_a =
            get_official_account_inner(Some(dir.display().to_string()), account_a).unwrap();
        assert_eq!(after_a.auth_json, before_a.auth_json);
        assert_eq!(after_a.config_text, before_a.config_text);
        remove_official_account_test_files(&dir);
    }

    #[test]
    fn switching_a_to_b_replaces_live_auth_and_config() {
        let dir = official_account_test_dir("official-switch-a-b");
        write_official_account_live(&dir, "model-a", "test-access-a1");
        let account_a = capture_test_account(&dir, "Account A");
        let account_b =
            prepare_and_capture_test_account(&dir, "Account B", "model-b", "test-access-b1");
        switch_official_account_inner(Some(dir.display().to_string()), account_a)
            .expect("switch to A first");
        switch_official_account_inner(Some(dir.display().to_string()), account_b)
            .expect("switch A to B");
        assert_eq!(
            read_live_auth(&dir)["tokens"]["access_token"].as_str(),
            Some("test-access-b1")
        );
        assert!(fs::read_to_string(config_path(&dir))
            .unwrap()
            .contains("model-b"));
        remove_official_account_test_files(&dir);
    }

    #[test]
    fn refreshed_oauth_token_is_captured_before_account_switch() {
        let dir = official_account_test_dir("official-refresh-round-trip");
        write_official_account_live(&dir, "model-a", "test-access-a1");
        let account_a = capture_test_account(&dir, "Account A");
        let account_b =
            prepare_and_capture_test_account(&dir, "Account B", "model-b", "test-access-b1");
        switch_official_account_inner(Some(dir.display().to_string()), account_a.clone())
            .expect("switch to A");
        write_json(
            &auth_path(&dir),
            &official_account_auth("test-access-a2", "test-refresh-a2"),
        )
        .expect("simulate Codex token refresh");
        switch_official_account_inner(Some(dir.display().to_string()), account_b)
            .expect("switch refreshed A to B");
        switch_official_account_inner(Some(dir.display().to_string()), account_a)
            .expect("switch back to A");
        assert_eq!(
            read_live_auth(&dir)["tokens"]["access_token"].as_str(),
            Some("test-access-a2")
        );
        remove_official_account_test_files(&dir);
    }

    #[test]
    fn third_party_to_account_publishes_official_route_before_auth() {
        let dir = official_account_test_dir("official-route-before-auth");
        let proxy_config = r#"model_provider = "custom"
model = "proxy-model"

[model_providers.custom]
name = "Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
"#;
        let old_auth = json!({"OPENAI_API_KEY": "test-proxy-key"});
        let target_auth = official_account_auth("test-access-a1", "test-refresh-a");
        write_text(&config_path(&dir), proxy_config).unwrap();
        write_json(&auth_path(&dir), &old_auth).unwrap();
        let old_config = read_file_snapshot(&config_path(&dir)).unwrap();
        let old_auth_bytes = read_file_snapshot(&auth_path(&dir)).unwrap();
        let target_config = official_account_config("model-a");
        write_live_files_with_between_writes(
            &dir,
            old_config,
            old_auth_bytes,
            &target_config,
            &LiveAuthAction::Replace(target_auth),
            || {
                assert!(live_config_is_official(&dir)?);
                assert_eq!(read_live_auth(&dir), old_auth);
                Ok(())
            },
        )
        .expect("publish official account safely");
        remove_official_account_test_files(&dir);
    }

    #[test]
    fn deleting_noncurrent_a_keeps_current_b_live_auth() {
        let dir = official_account_test_dir("official-delete-noncurrent");
        write_official_account_live(&dir, "model-a", "test-access-a1");
        let account_a = capture_test_account(&dir, "Account A");
        prepare_and_capture_test_account(&dir, "Account B", "model-b", "test-access-b1");
        let auth_before = fs::read(auth_path(&dir)).unwrap();
        delete_official_account_inner(Some(dir.display().to_string()), account_a)
            .expect("delete noncurrent A");
        assert_eq!(fs::read(auth_path(&dir)).unwrap(), auth_before);
        remove_official_account_test_files(&dir);
    }

    #[test]
    fn deleting_current_account_clears_auth_and_blocks_legacy_restore() {
        let dir = official_account_test_dir("official-delete-current");
        write_official_account_live(&dir, "model-a", "test-access-a1");
        let account_a = capture_test_account(&dir, "Account A");
        delete_official_account_inner(Some(dir.display().to_string()), account_a)
            .expect("delete current A");
        assert!(!auth_path(&dir).exists());
        assert!(load_official_account_store(&dir)
            .unwrap()
            .selected_account_id
            .is_none());
        switch_official_provider_inner(Some(dir.display().to_string()))
            .expect("legacy switch after deletion");
        assert!(!auth_path(&dir).exists());
        let restore_error = restore_official_provider_inner(Some(dir.display().to_string()))
            .expect_err("explicit restore must keep deleted account blocked");
        assert!(restore_error.to_string().contains("已删除的官方账号"));
        switch_official_provider_inner(Some(dir.display().to_string()))
            .expect("switch after blocked explicit restore");
        assert!(!auth_path(&dir).exists());
        remove_official_account_test_files(&dir);
    }

    #[test]
    fn deleting_selected_account_while_third_party_keeps_live_files() {
        let dir = official_account_test_dir("official-delete-selected-proxy");
        write_official_account_live(&dir, "model-a", "test-access-a1");
        let account_a = capture_test_account(&dir, "Account A");
        let proxy_config = r#"model_provider = "custom"
model = "proxy-model"

[model_providers.custom]
name = "Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
"#;
        write_text(&config_path(&dir), proxy_config).unwrap();
        write_json(
            &auth_path(&dir),
            &json!({"OPENAI_API_KEY": "test-proxy-key"}),
        )
        .unwrap();
        let config_before = fs::read(config_path(&dir)).unwrap();
        let auth_before = fs::read(auth_path(&dir)).unwrap();
        delete_official_account_inner(Some(dir.display().to_string()), account_a)
            .expect("delete selected account under proxy");
        assert_eq!(fs::read(config_path(&dir)).unwrap(), config_before);
        assert_eq!(fs::read(auth_path(&dir)).unwrap(), auth_before);
        remove_official_account_test_files(&dir);
    }

    #[test]
    fn prepare_new_account_keeps_old_account_and_clears_selection_and_live_auth() {
        let dir = official_account_test_dir("official-prepare-new");
        write_official_account_live(&dir, "model-a", "test-access-a1");
        let account_a = capture_test_account(&dir, "Account A");
        prepare_new_official_account_inner(Some(dir.display().to_string()))
            .expect("prepare new official login");
        let store = load_official_account_store(&dir).unwrap();
        assert!(store.accounts.iter().any(|account| account.id == account_a));
        assert!(store.selected_account_id.is_none());
        assert!(!auth_path(&dir).exists());
        remove_official_account_test_files(&dir);
    }

    #[test]
    fn capture_without_valid_login_does_not_create_account() {
        let dir = official_account_test_dir("official-capture-invalid");
        write_text(&config_path(&dir), &official_account_config("model-a")).unwrap();
        let error = capture_current_official_account_inner(
            Some(dir.display().to_string()),
            "Empty".to_string(),
        )
        .expect_err("missing auth must fail");
        assert!(error
            .to_string()
            .contains("未检测到有效 OpenAI Official 登录"));
        assert!(
            list_official_accounts_inner(Some(dir.display().to_string()))
                .unwrap()
                .is_empty()
        );
        remove_official_account_test_files(&dir);
    }

    #[test]
    fn legacy_snapshot_still_switches_without_multi_account_store() {
        let dir = official_account_test_dir("official-legacy-compatible");
        write_official_account_live(&dir, "legacy-model", "test-access-legacy");
        assert!(capture_live_official_config_before_provider_switch(&dir).unwrap());
        let account_path = official_accounts_path(&dir).unwrap();
        assert!(!account_path.exists());
        switch_provider_with_pre_persist(
            ProviderInput {
                config_dir: Some(dir.display().to_string()),
                provider_id: Some("proxy".to_string()),
                provider_name: "Proxy".to_string(),
                base_url: "https://proxy.example.com/v1".to_string(),
                model: "proxy-model".to_string(),
                api_key: Some("test-proxy-key".to_string()),
                wire_api: Some("responses".to_string()),
                requires_openai_auth: Some(true),
            },
            |_| Ok(()),
        )
        .expect("switch legacy official to proxy");
        switch_official_provider_with_pre_persist(Some(dir.display().to_string()), |_| Ok(()))
            .expect("switch legacy snapshot back to official");
        assert_eq!(
            read_live_auth(&dir)["tokens"]["access_token"].as_str(),
            Some("test-access-legacy")
        );
        assert!(!account_path.exists());
        remove_official_account_test_files(&dir);
    }

    #[test]
    fn corrupt_official_account_store_is_never_overwritten() {
        let dir = official_account_test_dir("official-corrupt-store");
        write_official_account_live(&dir, "model-a", "test-access-a1");
        let path = official_accounts_path(&dir).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let corrupt = b"{corrupt-official-account-store";
        fs::write(&path, corrupt).unwrap();
        let error = capture_current_official_account_inner(
            Some(dir.display().to_string()),
            "Account A".to_string(),
        )
        .expect_err("corrupt store must block capture");
        assert!(error.to_string().contains("拒绝覆盖"));
        assert_eq!(fs::read(&path).unwrap(), corrupt);
        remove_official_account_test_files(&dir);
    }
}
