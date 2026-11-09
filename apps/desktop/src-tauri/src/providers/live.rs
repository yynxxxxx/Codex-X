use super::ccswitch::codex_section_from_table;
#[cfg(test)]
use super::official_auth::mark_official_config_reset;
use super::official_auth::{
    auth_value_has_material, build_official_config_text,
    capture_live_official_config_before_provider_switch, document_is_official,
    live_config_is_official, official_config_candidate, official_snapshot_path,
    save_official_config_snapshot, saved_official_profile_candidate, validate_official_config_text,
};
use super::{
    custom_provider_id, delete_provider_inner, experimental_bearer_token_from_doc,
    is_placeholder_provider, list_saved_providers_on_connection,
    matching_saved_provider_ids_for_live_on_connection, normalize_saved_provider,
    normalize_saved_provider_for_save, open_store, provider_template_from_document,
    reconcile_active_provider_on_connection, reserved_codex_provider_id,
    rollback_provider_store_inner, save_detected_provider_with_rollback_inner,
    save_provider_with_rollback_inner, strip_provider_bearer_tokens, ProviderStoreRollback,
    SavedProvider,
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

pub(super) enum LiveAuthAction {
    #[cfg(test)]
    Keep,
    Replace(Value),
    Remove,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LiveWriteOrder {
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

pub(crate) fn replacement_write_order(
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

fn configure_live_provider_auth(
    table: &mut toml_edit::Table,
    api_key: Option<&str>,
    requires_openai_auth: bool,
) {
    table.remove("experimental_bearer_token");
    // Codex ignores auth.json for providers that do not use OpenAI auth.
    // Supply the selected provider's key directly, not a stale environment or
    // command-backed credential inherited from a previous configuration.
    if !requires_openai_auth {
        if let Some(key) = api_key.map(str::trim).filter(|key| !key.is_empty()) {
            table.remove("env_key");
            table.remove("env_key_instructions");
            table.remove("auth");
            for headers in ["http_headers", "env_http_headers"] {
                if let Some(headers) = table.get_mut(headers).and_then(Item::as_table_like_mut) {
                    let authorization = headers
                        .iter()
                        .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                        .map(|(name, _)| name.to_string())
                        .collect::<Vec<_>>();
                    for name in authorization {
                        headers.remove(&name);
                    }
                }
            }
            table["experimental_bearer_token"] = value(key);
        }
    }
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
pub(super) struct AppliedLiveFiles {
    config_path: PathBuf,
    auth_path: PathBuf,
    old_config: Option<Vec<u8>>,
    old_auth: Option<Vec<u8>>,
    new_config: Vec<u8>,
    new_auth: Option<Option<Vec<u8>>>,
}

impl AppliedLiveFiles {
    pub(super) fn rollback(&self) -> Result<()> {
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

fn capture_live_official_snapshot(codex_dir: &Path) -> Result<Option<AppliedSnapshot>> {
    let path = official_snapshot_path(codex_dir)?;
    let before = read_file_snapshot(&path)?;
    if !capture_live_official_config_before_provider_switch(codex_dir)? {
        return Ok(None);
    }
    let after = read_file_snapshot(&path)?;
    Ok(Some(AppliedSnapshot {
        path,
        before,
        after,
    }))
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
        #[cfg(test)]
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
    let doc = crate::failover::direct_document(codex_dir, &doc)?;
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
        upstream_api: None,
        model_mappings: Vec::new(),
    }))
}

pub(super) fn persist_detected_live_custom_provider(
    codex_dir: &Path,
) -> Result<Option<ProviderStoreRollback>> {
    let Some(live) = detected_live_custom_provider(codex_dir)? else {
        return Ok(None);
    };
    save_detected_provider_with_rollback_inner(codex_dir, live)
}

// These keys belong to a provider/model selection, rather than shared Codex
// integrations. Keep this explicit: model_instructions_file, for example, is shared.
const PROVIDER_MODEL_ROOTS: &[&str] = &[
    "model",
    "review_model",
    "model_reasoning_effort",
    "model_reasoning_summary",
    "model_verbosity",
    "model_context_window",
    "model_auto_compact_token_limit",
    "model_supports_reasoning_summaries",
    "model_catalog_json",
    "service_tier",
    "disable_response_storage",
];

fn is_provider_only_document(doc: &DocumentMut) -> bool {
    doc.as_table().iter().all(|(key, _)| {
        PROVIDER_MODEL_ROOTS.contains(&key)
            || matches!(
                key,
                "model_provider" | "model_providers" | "experimental_bearer_token"
            )
    })
}

fn common_config_handled(codex_dir: &Path) -> Result<bool> {
    open_store()?
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM provider_common_config_state WHERE codex_dir = ?1)",
            [crate::paths::normalized_path_scope(codex_dir)],
            |row| row.get(0),
        )
        .map_err(|error| CodexxError::Database(error.to_string()))
}

fn mark_common_config_handled(codex_dir: &Path) -> Result<()> {
    open_store()?.execute(
        "INSERT OR IGNORE INTO provider_common_config_state (codex_dir, handled_at) VALUES (?1, ?2)",
        (crate::paths::normalized_path_scope(codex_dir), crate::now_rfc3339()),
    ).map_err(|error| CodexxError::Database(error.to_string()))?;
    Ok(())
}

fn provider_config_base_document(codex_dir: &Path) -> Result<DocumentMut> {
    let cfg = config_path(codex_dir);
    let text = read_to_string_if_exists(&cfg)?;
    // A broken live file must be surfaced, not silently replaced with a snapshot.
    let current = parse_toml_document(&cfg, &text)?;
    let mut current = crate::failover::direct_document(codex_dir, &current)?;
    if is_provider_only_document(&current) && !common_config_handled(codex_dir)? {
        let default_profile = super::official_profiles::default_profile_id(codex_dir)?;
        let snapshot = saved_official_profile_candidate(codex_dir, &default_profile)?;
        if let Some(text) = snapshot.and_then(|snapshot| snapshot.config_text) {
            let official = parse_toml_document(&cfg, &text)?;
            // Only recover shared integration tables from the trusted same-home
            // snapshot. Never recover old auth, routing, execution or model defaults.
            for key in [
                "mcp_servers",
                "desktop",
                "marketplaces",
                "plugins",
                "projects",
            ] {
                if current.get(key).is_none() {
                    if let Some(item) = official
                        .get(key)
                        .filter(|item| item.as_table_like().is_some())
                    {
                        current.as_table_mut().insert(key, item.clone());
                    }
                }
            }
        }
    }
    strip_provider_bearer_tokens(&mut current);
    Ok(current)
}

pub(crate) fn get_provider_config_base_inner(config_dir: Option<String>) -> Result<String> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    Ok(provider_config_base_document(&codex_dir)?
        .to_string()
        .trim_end()
        .to_string())
}

fn overlay_provider_template(base: &mut DocumentMut, template: &DocumentMut) -> Result<()> {
    for (key, item) in template.as_table().iter() {
        if key == "model_providers" {
            let providers = ensure_table(base.as_table_mut(), key)?;
            if let Some(tables) = item.as_table() {
                for (id, table) in tables.iter() {
                    providers.insert(id, table.clone());
                }
            }
        } else {
            base.as_table_mut().insert(key, item.clone());
        }
    }
    Ok(())
}

fn clear_foreign_provider_credentials(
    table: &mut toml_edit::Table,
    base_url: &str,
    own_template: bool,
) {
    let same_endpoint = table
        .get("base_url")
        .and_then(Item::as_str)
        .is_some_and(|old| {
            super::store::canonical_provider_base_url(old)
                == super::store::canonical_provider_base_url(base_url)
        });
    if !own_template || !same_endpoint {
        for key in [
            "experimental_bearer_token",
            "env_key",
            "env_key_instructions",
            "auth",
            "http_headers",
            "env_http_headers",
            "query_params",
        ] {
            table.remove(key);
        }
    }
}

pub(crate) fn build_provider_toml_draft_inner(
    provider: SavedProvider,
    config_dir: Option<String>,
) -> Result<String> {
    build_provider_toml_draft_with_origin_inner(provider, config_dir, false)
}

pub(crate) fn build_provider_toml_draft_with_origin_inner(
    mut provider: SavedProvider,
    config_dir: Option<String>,
    new_provider: bool,
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
    let mut doc = match saved_template {
        Some(text) => {
            let template = parse_toml_document(&cfg, text)?;
            let template = crate::failover::direct_document(&codex_dir, &template)?;
            if is_provider_only_document(&template) {
                let mut base = provider_config_base_document(&codex_dir)?;
                overlay_provider_template(&mut base, &template)?;
                base
            } else {
                template
            }
        }
        None => provider_config_base_document(&codex_dir)?,
    };
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
    clear_foreign_provider_credentials(
        table,
        &provider.base_url,
        saved_template.is_some() && !new_provider,
    );
    super::transport::configure_third_party_transport(
        table,
        &provider.base_url,
        saved_template.is_some() && !new_provider,
    );
    table["name"] = value(provider.provider_name.trim());
    table["base_url"] = value(provider.base_url.trim().trim_end_matches('/'));
    table["wire_api"] = value(provider.wire_api.trim());
    table["requires_openai_auth"] = value(provider.requires_openai_auth);
    provider.toml_config = Some(doc.to_string().trim_end().to_string());

    normalize_saved_provider(provider)?
        .toml_config
        .ok_or_else(|| CodexxError::Config("无法生成供应商 TOML".to_string()))
}

pub(super) fn apply_official_config_locked(
    codex_dir: &Path,
    config_text: Option<&str>,
    model: Option<&str>,
    clear_model_if_none: bool,
    auth_action: LiveAuthAction,
    action: &str,
) -> Result<(Option<String>, AppliedLiveFiles)> {
    apply_official_config_with_snapshot_locked(
        codex_dir,
        config_text,
        model,
        clear_model_if_none,
        auth_action,
        action,
        read_live_file_snapshot(codex_dir)?,
    )
}

pub(super) struct LiveFileSnapshot {
    config: Option<Vec<u8>>,
    auth: Option<Vec<u8>>,
}

pub(super) fn read_live_file_snapshot(codex_dir: &Path) -> Result<LiveFileSnapshot> {
    Ok(LiveFileSnapshot {
        config: read_file_snapshot(&config_path(codex_dir))?,
        auth: read_file_snapshot(&auth_path(codex_dir))?,
    })
}

pub(super) fn apply_official_config_with_snapshot_locked(
    codex_dir: &Path,
    config_text: Option<&str>,
    model: Option<&str>,
    clear_model_if_none: bool,
    auth_action: LiveAuthAction,
    action: &str,
    before: LiveFileSnapshot,
) -> Result<(Option<String>, AppliedLiveFiles)> {
    let backup_id = create_backup(codex_dir, action)?;

    let config_text = match config_text.map(str::trim).filter(|text| !text.is_empty()) {
        Some(config_text) => validate_official_config_text(codex_dir, config_text, model)?.0,
        None => {
            let config = build_official_config_text(codex_dir, model, clear_model_if_none)?;
            validate_official_config_text(codex_dir, &config, model)?.0
        }
    };

    let applied = write_live_files(
        codex_dir,
        before.config,
        before.auth,
        &config_text,
        &auth_action,
    )?;
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
pub(crate) fn restore_official_provider_inner(config_dir: Option<String>) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    ensure_directory(&codex_dir)?;
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
fn merge_provider_toml_into_live(
    cfg: &Path,
    current_text: &str,
    provider_text: &str,
    explicit_api_key: Option<String>,
) -> Result<(DocumentMut, Option<String>)> {
    merge_provider_toml_into_live_with_policy(
        cfg,
        current_text,
        provider_text,
        explicit_api_key,
        false,
    )
}

fn merge_provider_toml_into_live_with_policy(
    cfg: &Path,
    current_text: &str,
    provider_text: &str,
    explicit_api_key: Option<String>,
    explicit_full_config: bool,
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
    super::store::validate_provider_header_table(&source_provider)?;
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
    let source_base_url = source_provider["base_url"].as_str().unwrap().to_string();
    super::transport::configure_third_party_transport(&mut source_provider, &source_base_url, true);
    let requires_openai_auth = source_provider
        .get("requires_openai_auth")
        .and_then(|item| item.as_bool())
        .unwrap_or(false);
    let has_env_key = source_provider
        .get("env_key")
        .and_then(Item::as_str)
        .is_some_and(|name| !name.trim().is_empty());
    if requires_openai_auth && api_key.is_none() && !has_env_key {
        return Err(CodexxError::Config(
            "该供应商需要 API Key，未切换且未修改 auth.json".to_string(),
        ));
    }
    configure_live_provider_auth(
        &mut source_provider,
        api_key.as_deref(),
        requires_openai_auth,
    );

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
    let mut live = if explicit_full_config || has_complete_config {
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
    save_provider_toml_config_with_catalog_locked(
        codex_dir,
        input,
        old_config,
        pre_persist,
        None,
        false,
    )
}

fn save_provider_toml_config_with_catalog_locked<F>(
    codex_dir: &Path,
    input: ProviderTomlInput,
    old_config: Option<Vec<u8>>,
    pre_persist: F,
    saved: Option<&SavedProvider>,
    explicit_full_config: bool,
) -> Result<ActionResult>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let cfg = config_path(codex_dir);
    let old_auth = read_file_snapshot(&auth_path(codex_dir))?;
    let snapshot = capture_live_official_snapshot(codex_dir)?;
    let prepared = (|| -> Result<(Option<String>, String, String, LiveAuthAction)> {
        pre_persist(codex_dir)?;
        let backup_id = create_backup(codex_dir, "save-provider-toml")?;
        let current_text = text_from_snapshot(&cfg, old_config.as_deref())?;
        let (mut doc, api_key) = merge_provider_toml_into_live_with_policy(
            &cfg,
            &current_text,
            input.config_text.trim_end(),
            input.api_key,
            explicit_full_config,
        )?;
        if let Some(saved) = saved {
            // Resolve the menu after sparse legacy templates inherit common
            // settings, so a previous provider's managed menu cannot leak back.
            super::model_catalog::prepare_model_catalog(
                codex_dir,
                &saved.id,
                &saved.model_mappings,
                &saved.model,
                &mut doc,
            )?;
        }
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
        Err(error) => return rollback_after_failure(error, None, snapshot.as_ref()),
    };
    let live = match write_live_files(codex_dir, old_config, old_auth, &replacement, &auth_action) {
        Ok(live) => live,
        Err(error) => return rollback_after_failure(error, None, snapshot.as_ref()),
    };
    let result = finish_live_action(codex_dir, message, backup_id, &live, snapshot.as_ref())?;
    // After a validated application, an empty shared configuration may be a
    // deliberate edit. Do not resurrect an older official snapshot next time.
    if let Err(error) = mark_common_config_handled(codex_dir) {
        return rollback_after_failure(error, Some(&live), snapshot.as_ref());
    }
    Ok(result)
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
    let snapshot = capture_live_official_snapshot(codex_dir)?;
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
        super::transport::configure_third_party_transport(provider_table, base_url, false);
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
        configure_live_provider_auth(provider_table, api_key.as_deref(), requires_openai_auth);
        let auth_action = provider_auth_action(api_key.as_deref());
        Ok((backup_id, doc.to_string(), auth_action))
    })();
    let (backup_id, replacement, auth_action) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return rollback_after_failure(error, None, snapshot.as_ref()),
    };
    let live = match write_live_files(codex_dir, old_config, old_auth, &replacement, &auth_action) {
        Ok(live) => live,
        Err(error) => return rollback_after_failure(error, None, snapshot.as_ref()),
    };
    finish_live_action(
        codex_dir,
        format!("已切换到 {provider_name}"),
        backup_id,
        &live,
        snapshot.as_ref(),
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
    let codex_dir = resolve_codex_dir(config_dir)?;
    ensure_directory(&codex_dir)?;
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    let active_config = read_file_snapshot(&config_path(&codex_dir))?;
    let conn = open_store()?;
    let provider = normalize_saved_provider_for_save(&conn, provider)?;
    let saved_before = list_saved_providers_on_connection(&conn)?;
    let live = detected_live_custom_provider(&codex_dir)?.ok_or_else(|| {
        CodexxError::Config("当前不是可编辑的第三方供应商，未修改保存记录".to_string())
    })?;
    let matches = matching_saved_provider_ids_for_live_on_connection(
        &conn,
        &codex_dir,
        &live,
        &saved_before,
    )?;
    if matches.is_empty() {
        if saved_before
            .iter()
            .any(|candidate| candidate.id == provider.id)
        {
            return Err(CodexxError::Config(format!(
                "供应商 ID {} 已被另一条配置使用，请更换名称后再保存",
                provider.id
            )));
        }
    } else if !matches.iter().any(|active_id| active_id == &provider.id) {
        return Err(CodexxError::Config(format!(
            "当前 live 配置不匹配供应商 {}，不能作为活动配置保存",
            provider.id
        )));
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
    save_active_provider_with_apply(provider, config_dir, apply_saved_provider_locked)
}

pub(crate) fn save_active_provider_with_common_config_inner(
    provider: SavedProvider,
    config_dir: Option<String>,
) -> Result<ActionResult> {
    save_active_provider_with_apply(provider, config_dir, |saved, dir, before| {
        apply_saved_provider_with_policy_locked(saved, dir, before, true)
    })
}

fn apply_saved_provider_locked(
    saved: &SavedProvider,
    codex_dir: &Path,
    active_config: Option<Vec<u8>>,
) -> Result<ActionResult> {
    apply_saved_provider_with_policy_locked(saved, codex_dir, active_config, false)
}

fn provider_activation_document(saved: &SavedProvider, codex_dir: &Path) -> Result<DocumentMut> {
    let draft =
        build_provider_toml_draft_inner(saved.clone(), Some(codex_dir.display().to_string()))?;
    let target = parse_toml_document(&config_path(codex_dir), &draft)?;
    let preferences = saved
        .toml_config
        .as_deref()
        .map(|text| parse_toml_document(&config_path(codex_dir), text))
        .transpose()?
        .unwrap_or_default();
    let mut current = provider_config_base_document(codex_dir)?;
    for key in PROVIDER_MODEL_ROOTS {
        // Privacy is shared, even though legacy thin templates included it.
        if *key == "disable_response_storage" {
            continue;
        }
        match preferences.get(key) {
            Some(item) => {
                current.as_table_mut().insert(key, item.clone());
            }
            None if *key != "model_catalog_json" => {
                current.as_table_mut().remove(key);
            }
            None => {}
        }
    }
    // Model catalogs are finalized by prepare_model_catalog. Its existing rule
    // removes app-owned catalogs while retaining an explicitly supplied user file.
    current["model"] = value(saved.model.clone());
    let provider_id = string_value(&target, "model_provider")
        .ok_or_else(|| CodexxError::Config("供应商配置缺少 model_provider".to_string()))?;
    current["model_provider"] = value(provider_id.clone());
    let table = target
        .get("model_providers")
        .and_then(Item::as_table)
        .and_then(|providers| providers.get(&provider_id))
        .cloned()
        .ok_or_else(|| CodexxError::Config("供应商配置缺少模型接口设置".to_string()))?;
    ensure_table(current.as_table_mut(), "model_providers")?.insert(&provider_id, table);
    strip_provider_bearer_tokens(&mut current);
    Ok(current)
}

pub(super) fn official_activation_config_text(
    codex_dir: &Path,
    target_text: &str,
) -> Result<String> {
    let target = parse_toml_document(&config_path(codex_dir), target_text)?;
    let mut current = provider_config_base_document(codex_dir)?;
    for key in PROVIDER_MODEL_ROOTS {
        if *key == "disable_response_storage" {
            continue;
        }
        match target.get(key) {
            Some(item) => {
                current.as_table_mut().insert(key, item.clone());
            }
            None if *key != "model_catalog_json" => {
                current.as_table_mut().remove(key);
            }
            None => {}
        }
    }
    // Official routing and account constraints come only from the selected
    // official configuration. Never inherit a third-party endpoint or credential.
    for key in [
        "model_provider",
        "model_providers",
        "base_url",
        "experimental_bearer_token",
        "auth",
        "auth_mode",
        "tokens",
        "openai_api_key",
        "api_key",
        "api_base",
        "chatgpt_base_url",
        "forced_login_method",
        "forced_chatgpt_workspace_id",
        "env_key",
        "env_key_instructions",
        "http_headers",
        "env_http_headers",
        "query_params",
    ] {
        current.as_table_mut().remove(key);
        if let Some(item) = target.get(key) {
            current.as_table_mut().insert(key, item.clone());
        }
    }
    let model = string_value(&target, "model");
    Ok(validate_official_config_text(codex_dir, &current.to_string(), model.as_deref())?.0)
}

fn apply_saved_provider_with_policy_locked(
    saved: &SavedProvider,
    codex_dir: &Path,
    active_config: Option<Vec<u8>>,
    apply_common_config: bool,
) -> Result<ActionResult> {
    if !saved.model_mappings.is_empty() && saved.wire_api != "responses" {
        return Err(CodexxError::Config(
            "模型映射需要供应商提供 Responses 兼容接口，请检查 Wire API 设置".into(),
        ));
    }
    let config_text = if apply_common_config {
        match saved.toml_config.as_deref() {
            Some(text) => {
                let doc = parse_toml_document(&config_path(codex_dir), text)?;
                crate::failover::direct_document(codex_dir, &doc)?.to_string()
            }
            None => build_provider_toml_draft_inner(
                saved.clone(),
                Some(codex_dir.display().to_string()),
            )?,
        }
    } else {
        provider_activation_document(saved, codex_dir)?.to_string()
    };
    save_provider_toml_config_with_catalog_locked(
        codex_dir,
        ProviderTomlInput {
            config_dir: None,
            config_text,
            api_key: saved.api_key.clone(),
        },
        active_config,
        |_| Ok(()),
        Some(saved),
        true,
    )
}

pub(crate) fn activate_saved_provider_inner(
    config_dir: Option<String>,
    provider_id: String,
) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    ensure_directory(&codex_dir)?;
    let _lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    let saved = super::store::provider_by_id_on_connection(&open_store()?, provider_id.trim())?
        .ok_or_else(|| CodexxError::Config("供应商已不存在，请刷新列表后重试".into()))?;
    let active_config = read_file_snapshot(&config_path(&codex_dir))?;
    let rollback = persist_detected_live_custom_provider(&codex_dir)?;
    let result = apply_saved_provider_locked(&saved, &codex_dir, active_config);
    rollback_persisted_provider(result, rollback)
}

pub(crate) fn delete_saved_provider_inner(id: &str, config_dir: Option<String>) -> Result<()> {
    let id = id.trim();
    if id.is_empty() {
        return Err(CodexxError::Config("供应商 ID 不能为空".to_string()));
    }
    let codex_dir = resolve_codex_dir(config_dir)?;
    let conn = open_store()?;
    let providers = list_saved_providers_on_connection(&conn)?;
    if let Some(live) = detected_live_custom_provider(&codex_dir)? {
        let active_ids = matching_saved_provider_ids_for_live_on_connection(
            &conn, &codex_dir, &live, &providers,
        )?;
        let active_id = reconcile_active_provider_on_connection(&conn, &codex_dir, &active_ids)?;
        if live.id == id || active_id.as_deref() == Some(id) {
            return Err(CodexxError::Config(
                "不能直接删除当前启用的供应商，请先切换到官方配置或其他供应商".to_string(),
            ));
        }
    }
    drop(conn);
    delete_provider_inner(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_io::{write_json, write_text};
    use crate::providers::{delete_provider_inner, remember_active_provider_on_connection};
    use crate::providers::{list_saved_providers_inner, save_provider_inner};
    use serde_json::json;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn header_values_survive_direct_toml_activation_and_invalid_values_are_rejected() {
        let config = "model_provider='custom'\nmodel='gpt-test'\n[model_providers.custom]\nname='Header fixture'\nbase_url='https://headers.example.test/v1'\nwire_api='responses'\nrequires_openai_auth=false\nhttp_headers={User-Agent='Fixture agent'}\nenv_http_headers={X-Project='PROJECT_ID'}\n";
        let (doc, _) =
            merge_provider_toml_into_live(Path::new("fixture.toml"), "", config, None).unwrap();
        assert_eq!(
            doc["model_providers"]["custom"]["http_headers"]["User-Agent"].as_str(),
            Some("Fixture agent")
        );
        assert_eq!(
            doc["model_providers"]["custom"]["env_http_headers"]["X-Project"].as_str(),
            Some("PROJECT_ID")
        );
        let invalid = config.replace(
            "User-Agent='Fixture agent'",
            "User-Agent=\"secret\\r\\ninjected\"",
        );
        let error = merge_provider_toml_into_live(Path::new("fixture.toml"), "", &invalid, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Header"));
        assert!(!error.contains("secret"));
    }

    #[test]
    fn explicit_api_key_removes_stale_authorization_headers_from_both_sources() {
        let mut doc = "[custom]\nhttp_headers={Authorization='Bearer stale',X-Trace='keep'}\nenv_http_headers={aUtHoRiZaTiOn='STALE_AUTH',X-Project='PROJECT_ID'}\n".parse::<DocumentMut>().unwrap();
        let table = doc["custom"].as_table_mut().unwrap();
        configure_live_provider_auth(table, Some("fixture-key"), false);
        assert!(table["http_headers"]
            .as_table_like()
            .unwrap()
            .get("Authorization")
            .is_none());
        assert!(table["env_http_headers"]
            .as_table_like()
            .unwrap()
            .get("aUtHoRiZaTiOn")
            .is_none());
        assert_eq!(table["http_headers"]["X-Trace"].as_str(), Some("keep"));
        assert_eq!(
            table["env_http_headers"]["X-Project"].as_str(),
            Some("PROJECT_ID")
        );
    }

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
            upstream_api: None,
            model_mappings: Vec::new(),
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
        assert_eq!(
            merged["model_providers"]["custom"]["experimental_bearer_token"].as_str(),
            Some("sk-b")
        );
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
                upstream_api: None,
                model_mappings: Vec::new(),
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

    #[test]
    fn provider_draft_does_not_inherit_official_websockets() {
        let dir = active_provider_test_dir("transport-official", 40_021);
        let official = build_official_config_text(&dir, Some("official-model"), false).unwrap();
        assert_eq!(
            official.parse::<DocumentMut>().unwrap()["model_providers"]["custom"]
                ["supports_websockets"]
                .as_bool(),
            Some(true)
        );
        fs::write(config_path(&dir), &official).unwrap();
        let mut target =
            active_provider_fixture(40_021, "proxy", "My proxy", "model", "fixture-key");
        target.toml_config = None;
        let draft =
            build_provider_toml_draft_inner(target, Some(dir.display().to_string())).unwrap();
        let draft = draft.parse::<DocumentMut>().unwrap();
        assert_eq!(
            draft["model_providers"]["custom"]["supports_websockets"].as_bool(),
            Some(false)
        );
        assert_eq!(fs::read_to_string(config_path(&dir)).unwrap(), official);
        fs::write(config_path(&dir), draft.to_string()).unwrap();
        let restored = build_official_config_text(&dir, Some("official-model"), false).unwrap();
        assert_eq!(
            restored.parse::<DocumentMut>().unwrap()["model_providers"]["custom"]
                ["supports_websockets"]
                .as_bool(),
            Some(true)
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn provider_draft_preserves_own_websockets_only_until_endpoint_changes() {
        let dir = active_provider_test_dir("transport-own", 40_022);
        let mut target =
            active_provider_fixture(40_022, "proxy", "My proxy", "model", "fixture-key");
        target
            .toml_config
            .as_mut()
            .unwrap()
            .push_str("supports_websockets = true\nrequest_max_retries = 7\n");
        let render = |provider| {
            build_provider_toml_draft_inner(provider, Some(dir.display().to_string()))
                .unwrap()
                .parse::<DocumentMut>()
                .unwrap()
        };
        let unchanged = render(target.clone());
        assert_eq!(
            unchanged["model_providers"]["custom"]["supports_websockets"].as_bool(),
            Some(true)
        );
        target.base_url = "https://new-endpoint.example/v1".into();
        let changed = render(target);
        assert_eq!(
            changed["model_providers"]["custom"]["supports_websockets"].as_bool(),
            Some(false)
        );
        assert_eq!(
            changed["model_providers"]["custom"]["request_max_retries"].as_integer(),
            Some(7)
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn new_provider_template_keeps_common_settings_without_reusing_websocket_capability() {
        let dir = active_provider_test_dir("transport-new-origin", 40_024);
        let mut target =
            active_provider_fixture(40_024, "proxy", "My proxy", "model", "fixture-key");
        target.toml_config.as_mut().unwrap().push_str("supports_websockets = true\nrequest_max_retries = 7\n[mcp_servers.docs]\ncommand = 'fixture-mcp'\n");
        for (new_provider, expected) in [(true, false), (false, true)] {
            let draft = build_provider_toml_draft_with_origin_inner(
                target.clone(),
                Some(dir.display().to_string()),
                new_provider,
            )
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
            assert_eq!(
                draft["model_providers"]["custom"]["supports_websockets"].as_bool(),
                Some(expected)
            );
            assert_eq!(
                draft["model_providers"]["custom"]["request_max_retries"].as_integer(),
                Some(7)
            );
            assert_eq!(
                draft["mcp_servers"]["docs"]["command"].as_str(),
                Some("fixture-mcp")
            );
        }
        assert!(!config_path(&dir).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_deepseek_template_activates_without_websocket_retries() {
        let _guard = crate::app_db::test_db_guard();
        let dir = active_provider_test_dir("transport-legacy", 40_023);
        let mut target = active_provider_fixture(
            40_023,
            "old-deepseek",
            "DeepSeek",
            "deepseek-chat",
            "fixture-key",
        );
        target.base_url = "https://api.deepseek.com".into();
        target.toml_config = Some("model_provider='custom'\nmodel='deepseek-chat'\n[model_providers.custom]\nname='DeepSeek'\nbase_url='https://api.deepseek.com'\nwire_api='responses'\nrequires_openai_auth=false\nsupports_websockets=true\nrequest_max_retries=7\n".into());
        // Feed a pre-fix record directly through the activation path, bypassing
        // save normalization that would already correct this historical value.
        apply_saved_provider_locked(&target, &dir, None).unwrap();
        let doc = fs::read_to_string(config_path(&dir))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(
            doc["model_providers"]["custom"]["supports_websockets"].as_bool(),
            Some(false)
        );
        assert_eq!(
            doc["model_providers"]["custom"]["request_max_retries"].as_integer(),
            Some(7)
        );
        assert_eq!(
            doc["model_providers"]["custom"]["base_url"].as_str(),
            Some("https://api.deepseek.com")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn direct_toml_activation_preserves_proxy_ws_but_corrects_deepseek_ws() {
        for (endpoint, expected) in [
            ("https://proxy.example/v1", true),
            ("https://api.deepseek.com/v1", false),
        ] {
            let source = format!("model_provider='saved'\nmodel='some-model'\n[model_providers.saved]\nname='My API'\nbase_url='{endpoint}'\nsupports_websockets=true\nrequest_max_retries=7\n");
            let (doc, _) =
                merge_provider_toml_into_live(Path::new("config.toml"), "", &source, None).unwrap();
            assert_eq!(
                doc["model_providers"]["custom"]["supports_websockets"].as_bool(),
                Some(expected)
            );
            assert_eq!(
                doc["model_providers"]["custom"]["request_max_retries"].as_integer(),
                Some(7)
            );
        }
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
    fn active_provider_identity_survives_external_model_and_name_changes() {
        let _guard = crate::app_db::test_db_guard();
        let tag = 50_001;
        let id = format!("runtime-edit-{tag}");
        let dir = active_provider_test_dir("runtime-identity", tag);
        let original = active_provider_fixture(tag, &id, "Original", "saved-model", "fixture-key");
        save_provider_inner(original.clone()).unwrap();
        remember_active_provider_on_connection(&open_store().unwrap(), &dir, &id).unwrap();
        let external = active_provider_fixture(
            tag,
            "custom",
            "Codex renamed",
            "runtime-model",
            "fixture-key",
        );
        write_active_provider_files(&dir, &external);
        assert_eq!(
            build_state_after_migration(dir.clone())
                .unwrap()
                .active_saved_provider_id
                .as_deref(),
            Some(id.as_str())
        );
        let updated = active_provider_fixture(tag, &id, "User renamed", "new-model", "fixture-key");
        let result = save_active_provider_inner(updated, Some(dir.display().to_string())).unwrap();
        assert_eq!(
            result.state.active_saved_provider_id.as_deref(),
            Some(id.as_str())
        );
        assert_eq!(saved_provider(&id).provider_name, "User renamed");
        assert_eq!(saved_provider(&id).model, "new-model");
        assert_eq!(
            list_saved_providers_inner()
                .unwrap()
                .iter()
                .filter(|provider| provider.base_url == original.base_url)
                .count(),
            1
        );
        delete_provider_inner(&id).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn selected_record_wins_when_runtime_model_matches_a_same_api_sibling() {
        let _guard = crate::app_db::test_db_guard();
        let tag = 50_002;
        let first_id = format!("runtime-first-{tag}");
        let second_id = format!("runtime-second-{tag}");
        let dir = active_provider_test_dir("runtime-sibling", tag);
        let first = save_provider_inner(active_provider_fixture(
            tag,
            &first_id,
            "First",
            "first-model",
            "shared-fixture-key",
        ))
        .unwrap();
        let second = save_provider_inner(active_provider_fixture(
            tag,
            &second_id,
            "Second",
            "second-model",
            "shared-fixture-key",
        ))
        .unwrap();
        remember_active_provider_on_connection(&open_store().unwrap(), &dir, &first_id).unwrap();
        let external = active_provider_fixture(
            tag,
            "custom",
            "Second",
            "second-model",
            "shared-fixture-key",
        );
        write_active_provider_files(&dir, &external);
        assert_eq!(
            build_state_after_migration(dir.clone())
                .unwrap()
                .active_saved_provider_id
                .as_deref(),
            Some(first_id.as_str())
        );
        let before_config = fs::read(config_path(&dir)).unwrap();
        let before_auth = fs::read(auth_path(&dir)).unwrap();
        assert!(
            save_active_provider_inner(second.clone(), Some(dir.display().to_string())).is_err()
        );
        assert_eq!(saved_provider(&first_id), first);
        assert_eq!(saved_provider(&second_id), second);
        assert_eq!(fs::read(config_path(&dir)).unwrap(), before_config);
        assert_eq!(fs::read(auth_path(&dir)).unwrap(), before_auth);
        let updated = active_provider_fixture(
            tag,
            &first_id,
            "Edited first",
            "edited-model",
            "shared-fixture-key",
        );
        save_active_provider_inner(updated, Some(dir.display().to_string())).unwrap();
        assert_eq!(saved_provider(&second_id), second);
        assert_eq!(saved_provider(&first_id).provider_name, "Edited first");
        delete_provider_inner(&first_id).unwrap();
        delete_provider_inner(&second_id).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unique_route_recovers_an_older_missing_selection_after_model_change() {
        let _guard = crate::app_db::test_db_guard();
        let tag = 50_003;
        let id = format!("runtime-recover-{tag}");
        let dir = active_provider_test_dir("runtime-recover", tag);
        save_provider_inner(active_provider_fixture(
            tag,
            &id,
            "Saved",
            "before",
            "fixture-key",
        ))
        .unwrap();
        write_active_provider_files(
            &dir,
            &active_provider_fixture(tag, "custom", "External", "after", "fixture-key"),
        );
        let updated = active_provider_fixture(tag, &id, "Renamed", "saved-again", "fixture-key");
        let result = save_active_provider_inner(updated, Some(dir.display().to_string())).unwrap();
        assert_eq!(
            result.state.active_saved_provider_id.as_deref(),
            Some(id.as_str())
        );
        assert_eq!(saved_provider(&id).provider_name, "Renamed");
        delete_provider_inner(&id).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn switching_after_cancelled_runtime_edit_updates_original_without_a_duplicate() {
        let _guard = crate::app_db::test_db_guard();
        let tag = 50_004;
        let id = format!("runtime-cancel-{tag}");
        let dir = active_provider_test_dir("runtime-cancel", tag);
        let mut mapped = active_provider_fixture(tag, &id, "Original", "before", "fixture-key");
        mapped.model_mappings = vec![
            super::super::model_catalog::ProviderModelMapping {
                model: "before".into(),
                display_name: "Saved model".into(),
                context_window: Some(128000),
            },
            super::super::model_catalog::ProviderModelMapping {
                model: "external-model".into(),
                display_name: "Other supported model".into(),
                context_window: Some(256000),
            },
        ];
        let original = save_provider_inner(mapped).unwrap();
        remember_active_provider_on_connection(&open_store().unwrap(), &dir, &id).unwrap();
        let external = active_provider_fixture(tag, "custom", "D", "external-model", "fixture-key");
        write_active_provider_files(&dir, &external);
        // Opening/cancelling an editor only reads state; it must not persist a second row.
        let current = build_state_after_migration(dir.clone()).unwrap();
        assert_eq!(
            current.active_saved_provider_id.as_deref(),
            Some(id.as_str())
        );
        assert_eq!(saved_provider(&id), original);
        switch_official_provider_inner(Some(dir.display().to_string())).unwrap();
        let after = list_saved_providers_inner()
            .unwrap()
            .into_iter()
            .filter(|provider| provider.base_url == original.base_url)
            .collect::<Vec<_>>();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].id, id);
        assert_eq!(after[0].provider_name, "D");
        assert_eq!(after[0].model, "external-model");
        assert_eq!(after[0].model_mappings, original.model_mappings);
        delete_provider_inner(&id).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_runtime_edit_restores_original_record_and_selection() {
        let _guard = crate::app_db::test_db_guard();
        let tag = 50_005;
        let id = format!("runtime-failure-{tag}");
        let dir = active_provider_test_dir("runtime-failure", tag);
        let original = save_provider_inner(active_provider_fixture(
            tag,
            &id,
            "Original",
            "before",
            "fixture-key",
        ))
        .unwrap();
        remember_active_provider_on_connection(&open_store().unwrap(), &dir, &id).unwrap();
        write_active_provider_files(
            &dir,
            &active_provider_fixture(tag, "custom", "D", "external-model", "fixture-key"),
        );
        let before_config = fs::read(config_path(&dir)).unwrap();
        let before_auth = fs::read(auth_path(&dir)).unwrap();
        let error = save_active_provider_with_apply(
            active_provider_fixture(tag, &id, "Must roll back", "requested", "new-fixture-key"),
            Some(dir.display().to_string()),
            |_, _, _| Err(CodexxError::Config("fixture apply failure".to_string())),
        )
        .unwrap_err();
        assert!(error.to_string().contains("fixture apply failure"));
        assert_eq!(saved_provider(&id), original);
        assert_eq!(fs::read(config_path(&dir)).unwrap(), before_config);
        assert_eq!(fs::read(auth_path(&dir)).unwrap(), before_auth);
        assert_eq!(
            build_state_after_migration(dir.clone())
                .unwrap()
                .active_saved_provider_id
                .as_deref(),
            Some(id.as_str())
        );
        switch_official_provider_inner(Some(dir.display().to_string())).unwrap();
        assert_eq!(
            list_saved_providers_inner()
                .unwrap()
                .iter()
                .filter(|provider| provider.base_url == original.base_url)
                .count(),
            1
        );
        delete_provider_inner(&id).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ambiguous_runtime_route_does_not_create_or_overwrite_a_saved_profile_on_switch() {
        let _guard = crate::app_db::test_db_guard();
        let tag = 50_006;
        let dir = active_provider_test_dir("runtime-ambiguous", tag);
        let first = save_provider_inner(active_provider_fixture(
            tag,
            &format!("runtime-a-{tag}"),
            "A",
            "model-a",
            "fixture-key",
        ))
        .unwrap();
        let second = save_provider_inner(active_provider_fixture(
            tag,
            &format!("runtime-b-{tag}"),
            "B",
            "model-b",
            "fixture-key",
        ))
        .unwrap();
        write_active_provider_files(
            &dir,
            &active_provider_fixture(tag, "custom", "D", "external-model", "fixture-key"),
        );
        assert!(build_state_after_migration(dir.clone())
            .unwrap()
            .active_saved_provider_id
            .is_none());
        switch_official_provider_inner(Some(dir.display().to_string())).unwrap();
        assert_eq!(saved_provider(&first.id), first);
        assert_eq!(saved_provider(&second.id), second);
        assert_eq!(
            list_saved_providers_inner()
                .unwrap()
                .iter()
                .filter(|provider| provider.base_url == first.base_url)
                .count(),
            2
        );
        delete_provider_inner(&first.id).unwrap();
        delete_provider_inner(&second.id).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn active_provider_edit_rejects_a_different_live_credential_without_side_effects() {
        let _guard = crate::app_db::test_db_guard();
        let tag = 50_007;
        let id = format!("runtime-foreign-{tag}");
        let dir = active_provider_test_dir("runtime-foreign", tag);
        let original = save_provider_inner(active_provider_fixture(
            tag,
            &id,
            "Original",
            "model",
            "stored-fixture-key",
        ))
        .unwrap();
        remember_active_provider_on_connection(&open_store().unwrap(), &dir, &id).unwrap();
        write_active_provider_files(
            &dir,
            &active_provider_fixture(tag, "custom", "Original", "model", "foreign-fixture-key"),
        );
        let before_config = fs::read(config_path(&dir)).unwrap();
        let before_auth = fs::read(auth_path(&dir)).unwrap();
        assert!(
            save_active_provider_inner(original.clone(), Some(dir.display().to_string())).is_err()
        );
        assert_eq!(saved_provider(&id), original);
        assert_eq!(fs::read(config_path(&dir)).unwrap(), before_config);
        assert_eq!(fs::read(auth_path(&dir)).unwrap(), before_auth);
        assert_eq!(
            super::super::selection::selected_provider_id_on_connection(
                &open_store().unwrap(),
                &dir
            )
            .unwrap()
            .as_deref(),
            Some(id.as_str())
        );
        delete_provider_inner(&id).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn editor_env_auth_survives_active_save_and_switching_away_and_back() {
        let _guard = crate::app_db::test_db_guard();
        let tag = 60_010;
        let dir = active_provider_test_dir("editor-env-auth", tag);
        let original = save_provider_inner(active_provider_fixture(
            tag,
            &format!("editor-env-{tag}"),
            "Environment fixture",
            "fixture-model",
            "old-form-fixture-key",
        ))
        .unwrap();
        activate_saved_provider_inner(Some(dir.display().to_string()), original.id.clone())
            .unwrap();
        let mut edited = original.clone();
        edited.toml_config.as_mut().unwrap().push_str(
            "\nenv_key = 'CODEXX_FIXTURE_ENV_KEY'\nenv_key_instructions = 'Set the variable before launching Codex'\n",
        );
        let edited = crate::providers::provider_with_editor_auth(edited, true).unwrap();
        assert!(edited.api_key.is_none());
        save_active_provider_with_common_config_inner(edited, Some(dir.display().to_string()))
            .unwrap();
        assert!(
            !auth_path(&dir).exists(),
            "the previous direct key must not survive"
        );

        let alternate = save_provider_inner(active_provider_fixture(
            tag + 1,
            &format!("editor-env-alternate-{tag}"),
            "Alternate fixture",
            "other-model",
            "alternate-fixture-key",
        ))
        .unwrap();
        activate_saved_provider_inner(Some(dir.display().to_string()), alternate.id.clone())
            .unwrap();
        activate_saved_provider_inner(Some(dir.display().to_string()), original.id.clone())
            .unwrap();
        let doc = fs::read_to_string(config_path(&dir))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        let route = doc["model_providers"]["custom"].as_table().unwrap();
        assert_eq!(route["env_key"].as_str(), Some("CODEXX_FIXTURE_ENV_KEY"));
        assert_eq!(
            route["env_key_instructions"].as_str(),
            Some("Set the variable before launching Codex")
        );
        assert!(route.get("experimental_bearer_token").is_none());
        assert!(!auth_path(&dir).exists());
        let saved = saved_provider(&original.id);
        assert!(saved.api_key.is_none());
        assert!(saved
            .toml_config
            .unwrap()
            .contains("CODEXX_FIXTURE_ENV_KEY"));
        delete_provider_inner(&original.id).unwrap();
        delete_provider_inner(&alternate.id).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn editor_env_auth_is_saved_for_inactive_providers_and_validated() {
        let mut provider = active_provider_fixture(
            60_011,
            "editor-env-inactive",
            "Environment fixture",
            "fixture-model",
            "old-form-fixture-key",
        );
        provider.toml_config.as_mut().unwrap().push_str(
            "env_key = 'CODEXX_FIXTURE_ENV_KEY'\nexperimental_bearer_token = 'old-toml-fixture-key'\n",
        );
        // API-key form edits retain the existing explicit-key precedence.
        assert_eq!(
            crate::providers::provider_with_editor_auth(provider.clone(), false)
                .unwrap()
                .api_key
                .as_deref(),
            Some("old-form-fixture-key")
        );
        let prepared = crate::providers::provider_with_editor_auth(provider.clone(), true).unwrap();
        let normalized = normalize_saved_provider(prepared).unwrap();
        assert!(normalized.api_key.is_none());
        let text = normalized.toml_config.unwrap();
        assert!(text.contains("CODEXX_FIXTURE_ENV_KEY"));
        assert!(!text.contains("old-toml-fixture-key"));
        for invalid in ["env_key = ''", "env_key = 42"] {
            let mut invalid_provider = provider.clone();
            invalid_provider.toml_config = Some(
                provider
                    .toml_config
                    .as_ref()
                    .unwrap()
                    .replace("env_key = 'CODEXX_FIXTURE_ENV_KEY'", invalid),
            );
            assert!(crate::providers::provider_with_editor_auth(invalid_provider, true).is_err());
        }
        let (doc, key) = merge_provider_toml_into_live(
            Path::new("fixture.toml"),
            "",
            &text.replace(
                "requires_openai_auth = false",
                "requires_openai_auth = true",
            ),
            None,
        )
        .unwrap();
        assert!(key.is_none());
        assert_eq!(
            doc["model_providers"]["custom"]["env_key"].as_str(),
            Some("CODEXX_FIXTURE_ENV_KEY")
        );
    }

    #[test]
    fn saved_provider_activation_supplies_its_key_and_real_model_catalog() {
        let _guard = crate::app_db::test_db_guard();
        let tag = 60_001;
        let dir = active_provider_test_dir("catalog-auth", tag);
        let id = format!("catalog-auth-{tag}");
        let mut provider =
            active_provider_fixture(tag, &id, "DeepSeek", "deepseek-chat", "fixture-current-key");
        provider.toml_config.as_mut().unwrap().push_str(
            r#"env_key = "STALE_ENV_KEY"
env_key_instructions = "Stale environment instructions"
auth = { command = "fixture-never-executed" }
http_headers = { "X-Trace" = "keep-static" }
env_http_headers = { aUtHoRiZaTiOn = "STALE_AUTH", "X-Project" = "PROJECT_ENV" }
"#,
        );
        provider.model_mappings = vec![
            super::super::model_catalog::ProviderModelMapping {
                model: "deepseek-chat".into(),
                display_name: "DeepSeek V3".into(),
                context_window: Some(128000),
            },
            super::super::model_catalog::ProviderModelMapping {
                model: "deepseek-reasoner".into(),
                display_name: "DeepSeek R1".into(),
                context_window: Some(256000),
            },
        ];
        save_provider_inner(provider.clone()).unwrap();
        let result =
            activate_saved_provider_inner(Some(dir.display().to_string()), id.clone()).unwrap();
        assert_eq!(result.state.model.as_deref(), Some("deepseek-chat"));
        let doc = fs::read_to_string(config_path(&dir))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        let route = doc["model_providers"]["custom"].as_table().unwrap();
        assert_eq!(
            route["experimental_bearer_token"].as_str(),
            Some("fixture-current-key")
        );
        for key in ["env_key", "env_key_instructions", "auth"] {
            assert!(route.get(key).is_none());
        }
        assert!(route["http_headers"]
            .as_table_like()
            .unwrap()
            .get("Authorization")
            .is_none());
        assert!(route["env_http_headers"]
            .as_table_like()
            .unwrap()
            .get("aUtHoRiZaTiOn")
            .is_none());
        assert_eq!(
            route["http_headers"]["X-Trace"].as_str(),
            Some("keep-static")
        );
        assert_eq!(
            route["env_http_headers"]["X-Project"].as_str(),
            Some("PROJECT_ENV")
        );
        let pointer = PathBuf::from(doc["model_catalog_json"].as_str().unwrap());
        assert!(pointer.is_file());
        let catalog: Value = serde_json::from_slice(&fs::read(&pointer).unwrap()).unwrap();
        let models = catalog["models"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0]["slug"], "deepseek-chat");
        assert_eq!(models[0]["display_name"], "DeepSeek V3");
        assert_eq!(models[1]["slug"], "deepseek-reasoner");
        assert_eq!(models[1]["context_window"], 256000);
        for model in models {
            assert_eq!(
                model["input_modalities"],
                serde_json::json!(["text", "image"])
            );
            assert_eq!(
                model["supported_reasoning_levels"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()["effort"],
                "ultra"
            );
        }
        assert!(!fs::read_to_string(&pointer)
            .unwrap()
            .contains("fixture-current-key"));
        assert!(!saved_provider(&id)
            .toml_config
            .unwrap()
            .contains("fixture-current-key"));
        delete_provider_inner(&id).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn switching_to_unmapped_or_official_provider_removes_only_owned_catalog_pointer() {
        let _guard = crate::app_db::test_db_guard();
        let tag = 60_002;
        let dir = active_provider_test_dir("catalog-switch", tag);
        let mut mapped = active_provider_fixture(
            tag,
            &format!("catalog-mapped-{tag}"),
            "Mapped",
            "deepseek-chat",
            "fixture-mapped-key",
        );
        mapped.model_mappings = vec![super::super::model_catalog::ProviderModelMapping {
            model: "deepseek-chat".into(),
            display_name: "DeepSeek".into(),
            context_window: None,
        }];
        let mapped = save_provider_inner(mapped).unwrap();
        let plain = save_provider_inner(active_provider_fixture(
            tag + 1,
            &format!("catalog-plain-{tag}"),
            "Plain",
            "plain-model",
            "fixture-plain-key",
        ))
        .unwrap();
        activate_saved_provider_inner(Some(dir.display().to_string()), mapped.id.clone()).unwrap();
        let doc = fs::read_to_string(config_path(&dir))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        let generated = PathBuf::from(doc["model_catalog_json"].as_str().unwrap());
        activate_saved_provider_inner(Some(dir.display().to_string()), plain.id.clone()).unwrap();
        let plain_live = fs::read_to_string(config_path(&dir))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert!(
            plain_live.get("model_catalog_json").is_none(),
            "a sparse unmapped provider must not inherit the previous generated model catalog"
        );
        assert!(
            generated.is_file(),
            "catalog snapshots may remain for saved configurations and rollback"
        );
        activate_saved_provider_inner(Some(dir.display().to_string()), mapped.id.clone()).unwrap();
        switch_official_provider_inner(Some(dir.display().to_string())).unwrap();
        let official = fs::read_to_string(config_path(&dir))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert!(official.get("model_catalog_json").is_none());
        // Use a fresh CODEX_HOME so an unrelated older official snapshot cannot
        // replace the user-supplied catalog before the cleanup logic sees it.
        let custom_dir = active_provider_test_dir("catalog-user-pointer", 60_004);
        let custom_catalog = custom_dir.join("my-custom-models.json");
        fs::write(&custom_catalog, "{\"models\":[]}").unwrap();
        let mut custom = plain.clone();
        let mut template = custom
            .toml_config
            .as_ref()
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        template["model_catalog_json"] = value(custom_catalog.display().to_string());
        custom.toml_config = Some(template.to_string());
        save_provider_inner(custom).unwrap();
        activate_saved_provider_inner(Some(custom_dir.display().to_string()), plain.id.clone())
            .unwrap();
        switch_official_provider_inner(Some(custom_dir.display().to_string())).unwrap();
        let official_custom = fs::read_to_string(config_path(&custom_dir))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(
            official_custom["model_catalog_json"].as_str(),
            Some(custom_catalog.to_str().unwrap())
        );
        assert!(custom_catalog.is_file());
        delete_provider_inner(&mapped.id).unwrap();
        delete_provider_inner(&plain.id).unwrap();
        fs::remove_dir_all(custom_dir).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn model_catalog_failure_rolls_back_activation_adoption_and_active_edits() {
        let _guard = crate::app_db::test_db_guard();
        let tag = 60_003;
        let dir = active_provider_test_dir("catalog-failure", tag);
        let original = save_provider_inner(active_provider_fixture(
            tag,
            &format!("catalog-original-{tag}"),
            "Original",
            "before",
            "fixture-original-key",
        ))
        .unwrap();
        remember_active_provider_on_connection(&open_store().unwrap(), &dir, &original.id).unwrap();
        let external = active_provider_fixture(
            tag,
            "custom",
            "Codex rename",
            "external-model",
            "fixture-original-key",
        );
        write_active_provider_files(&dir, &external);
        let before_config = fs::read(config_path(&dir)).unwrap();
        let before_auth = fs::read(auth_path(&dir)).unwrap();
        let mut target = active_provider_fixture(
            tag + 1,
            &format!("catalog-target-{tag}"),
            "Target",
            "deepseek-chat",
            "fixture-target-key",
        );
        target.model_mappings = vec![super::super::model_catalog::ProviderModelMapping {
            model: "deepseek-chat".into(),
            display_name: "DeepSeek".into(),
            context_window: None,
        }];
        let target = save_provider_inner(target).unwrap();
        fs::write(dir.join(".codex-x"), "fixture blocks generated directory").unwrap();
        assert!(
            activate_saved_provider_inner(Some(dir.display().to_string()), target.id.clone())
                .is_err()
        );
        assert_eq!(saved_provider(&original.id), original);
        assert_eq!(saved_provider(&target.id), target);
        let mut updated = original.clone();
        updated.provider_name = "Must roll back".into();
        updated.model_mappings = vec![super::super::model_catalog::ProviderModelMapping {
            model: "before".into(),
            display_name: "Mapped before".into(),
            context_window: None,
        }];
        assert!(save_active_provider_inner(updated, Some(dir.display().to_string())).is_err());
        assert_eq!(saved_provider(&original.id), original);
        assert_eq!(fs::read(config_path(&dir)).unwrap(), before_config);
        assert_eq!(fs::read(auth_path(&dir)).unwrap(), before_auth);
        assert_eq!(
            super::super::selection::selected_provider_id_on_connection(
                &open_store().unwrap(),
                &dir
            )
            .unwrap()
            .as_deref(),
            Some(original.id.as_str())
        );
        delete_provider_inner(&original.id).unwrap();
        delete_provider_inner(&target.id).unwrap();
        fs::remove_dir_all(dir).unwrap();
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
        assert_eq!(
            doc["model_providers"]["custom"]["experimental_bearer_token"].as_str(),
            Some("sk-after")
        );
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
    fn active_provider_save_uses_explicit_id_when_live_profile_has_duplicates() {
        let _db_guard = crate::app_db::test_db_guard();
        let tag = 50_001;
        let first_id = format!("active-duplicate-first-{tag}");
        let target_id = format!("active-duplicate-target-{tag}");
        let codex_dir = active_provider_test_dir("duplicate-explicit-edit", tag);
        let first =
            active_provider_fixture(tag, &first_id, "Shared Before", "model-before", "sk-before");
        let target = active_provider_fixture(
            tag,
            &target_id,
            "Shared Before",
            "model-before",
            "sk-before",
        );
        save_provider_inner(first.clone()).expect("save first duplicate provider");
        save_provider_inner(target).expect("save target duplicate provider");
        write_active_provider_files(&codex_dir, &first);

        let updated = active_provider_fixture(
            tag,
            &target_id,
            "Explicit Target",
            "model-after",
            "sk-after",
        );
        let result = save_active_provider_inner(updated, Some(codex_dir.display().to_string()))
            .expect("edit the explicitly selected active duplicate");

        let untouched = saved_provider(&first_id);
        assert_eq!(untouched.provider_name, "Shared Before");
        assert_eq!(untouched.model, "model-before");
        assert_eq!(untouched.api_key.as_deref(), Some("sk-before"));
        let saved_target = saved_provider(&target_id);
        assert_eq!(saved_target.provider_name, "Explicit Target");
        assert_eq!(saved_target.model, "model-after");
        assert_eq!(saved_target.api_key.as_deref(), Some("sk-after"));
        let state = serde_json::to_value(&result.state).expect("serialize updated state");
        assert_eq!(
            state["activeSavedProviderId"].as_str(),
            Some(target_id.as_str())
        );

        delete_provider_inner(&first_id).expect("delete first duplicate provider");
        delete_provider_inner(&target_id).expect("delete target duplicate provider");
        fs::remove_dir_all(codex_dir).expect("remove duplicate edit test directory");
    }

    #[test]
    fn explicit_current_id_protects_only_the_active_duplicate_from_deletion() {
        let _db_guard = crate::app_db::test_db_guard();
        let tag = 50_002;
        let first_id = format!("active-delete-duplicate-first-{tag}");
        let remaining_id = format!("active-delete-duplicate-remaining-{tag}");
        let codex_dir = active_provider_test_dir("duplicate-delete-guard", tag);
        let first = active_provider_fixture(
            tag,
            &first_id,
            "Shared Provider",
            "shared-model",
            "sk-shared",
        );
        let remaining = active_provider_fixture(
            tag,
            &remaining_id,
            "Shared Provider",
            "shared-model",
            "sk-shared",
        );
        save_provider_inner(first.clone()).expect("save first duplicate provider");
        save_provider_inner(remaining).expect("save remaining duplicate provider");
        write_active_provider_files(&codex_dir, &first);
        let conn = open_store().expect("open provider store");
        remember_active_provider_on_connection(&conn, &codex_dir, &first_id)
            .expect("remember the explicit current provider");
        drop(conn);

        delete_saved_provider_inner(&remaining_id, Some(codex_dir.display().to_string()))
            .expect("delete the inactive duplicate");
        assert!(list_saved_providers_inner()
            .expect("list providers after duplicate deletion")
            .iter()
            .all(|provider| provider.id != remaining_id));
        let delete_error =
            delete_saved_provider_inner(&first_id, Some(codex_dir.display().to_string()))
                .expect_err("the explicitly selected provider must be protected");
        assert!(delete_error.to_string().contains("不能直接删除当前启用"));
        assert_eq!(saved_provider(&first_id).provider_name, "Shared Provider");

        delete_provider_inner(&first_id).expect("delete active duplicate during cleanup");
        fs::remove_dir_all(codex_dir).expect("remove duplicate delete test directory");
    }

    #[test]
    fn active_provider_save_preserves_a_legacy_custom_record_id() {
        let _db_guard = crate::app_db::test_db_guard();
        let tag = 50_003;
        let temporary_id = format!("legacy-custom-seed-{tag}");
        let codex_dir = active_provider_test_dir("legacy-custom-edit", tag);
        let temporary = active_provider_fixture(
            tag,
            &temporary_id,
            "Legacy Before",
            "model-before",
            "sk-before",
        );
        save_provider_inner(temporary.clone()).expect("seed temporary legacy provider");
        let conn = open_store().expect("open provider store");
        conn.execute(
            "UPDATE providers SET id = 'custom' WHERE id = ?1",
            [&temporary_id],
        )
        .expect("restore historical custom ID");
        drop(conn);

        let mut live = temporary;
        live.id = "custom".to_string();
        write_active_provider_files(&codex_dir, &live);
        let updated =
            active_provider_fixture(tag, "custom", "Legacy After", "model-after", "sk-after");
        let result = save_active_provider_inner(updated, Some(codex_dir.display().to_string()))
            .expect("edit active provider with a legacy custom ID");

        assert_eq!(saved_provider("custom").provider_name, "Legacy After");
        assert!(list_saved_providers_inner()
            .expect("list providers after legacy edit")
            .iter()
            .all(|provider| provider.id != "custom-custom"));
        let state = serde_json::to_value(&result.state).expect("serialize updated state");
        assert_eq!(state["activeSavedProviderId"].as_str(), Some("custom"));

        delete_provider_inner("custom").expect("delete legacy custom provider");
        fs::remove_dir_all(codex_dir).expect("remove legacy custom test directory");
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

    #[test]
    fn detected_live_provider_survives_activation_and_later_provider_switches() {
        let _db_guard = crate::app_db::test_db_guard();
        for activate_first in [false, true] {
            let tag = if activate_first { 40_001 } else { 40_002 };
            let codex_dir = active_provider_test_dir("detected-retained", tag);
            let config_dir = Some(codex_dir.display().to_string());
            let name = format!("Detected {tag}");
            let live = active_provider_fixture(tag, "custom", &name, "live-model", "sk-live");
            write_active_provider_files(&codex_dir, &live);
            let original_config = fs::read_to_string(config_path(&codex_dir)).unwrap();
            let original_auth = fs::read(auth_path(&codex_dir)).unwrap();
            let state = crate::get_codex_state_inner(config_dir.clone()).unwrap();
            assert!(state.active_saved_provider_id.is_none());
            assert!(list_saved_providers_inner()
                .unwrap()
                .iter()
                .all(|provider| provider.base_url != live.base_url));
            assert_eq!(
                fs::read_to_string(config_path(&codex_dir)).unwrap(),
                original_config
            );
            assert_eq!(fs::read(auth_path(&codex_dir)).unwrap(), original_auth);

            if activate_first {
                let activated = switch_provider_inner(ProviderInput {
                    config_dir: config_dir.clone(),
                    provider_id: Some(custom_provider_id(&live.provider_name)),
                    provider_name: live.provider_name.clone(),
                    base_url: live.base_url.clone(),
                    model: live.model.clone(),
                    api_key: live.api_key.clone(),
                    wire_api: Some(live.wire_api.clone()),
                    requires_openai_auth: Some(live.requires_openai_auth),
                })
                .expect("activate the detected CC Switch live configuration");
                let adopted_id = activated.state.active_saved_provider_id.clone();
                assert!(adopted_id.is_some());
                let selected = crate::finish_provider_selection(
                    activated,
                    crate::ActiveProviderSelectionUpdate::Set(format!("provisional-{tag}")),
                );
                assert_eq!(selected.state.active_saved_provider_id, adopted_id);
                assert!(!selected.message.contains("当前供应商状态记录失败"));
            }
            switch_official_provider_inner(config_dir.clone()).expect("switch to official");
            let retained = list_saved_providers_inner()
                .unwrap()
                .into_iter()
                .filter(|provider| provider.base_url == live.base_url)
                .collect::<Vec<_>>();
            assert_eq!(retained.len(), 1);
            let retained = &retained[0];
            assert_eq!(retained.api_key, live.api_key);
            assert_eq!(retained.provider_name, name);
            let retained_config = retained.toml_config.clone().unwrap();
            assert!(retained_config.contains("approval_policy = \"never\""));
            assert!(retained_config.contains("docs-server"));

            save_provider_toml_config_inner(ProviderTomlInput {
                config_dir: config_dir.clone(),
                config_text: retained_config,
                api_key: retained.api_key.clone(),
            })
            .expect("restore the retained provider");
            assert_eq!(fs::read(auth_path(&codex_dir)).unwrap(), original_auth);

            switch_provider_inner(ProviderInput {
                config_dir,
                provider_id: Some(format!("other-{tag}")),
                provider_name: "Other".to_string(),
                base_url: format!("https://other-{tag}.example.com/v1"),
                model: "other-model".to_string(),
                api_key: Some("sk-other".to_string()),
                wire_api: Some("responses".to_string()),
                requires_openai_auth: Some(true),
            })
            .expect("switch from the detected provider to another third party");
            let final_rows = list_saved_providers_inner().unwrap();
            assert_eq!(
                final_rows
                    .iter()
                    .filter(|provider| provider.base_url == live.base_url)
                    .count(),
                1
            );
            assert_eq!(saved_provider(&retained.id).api_key, live.api_key);

            delete_provider_inner(&retained.id).unwrap();
            let _ = fs::remove_file(official_snapshot_path(&codex_dir).unwrap());
            fs::remove_dir_all(codex_dir).unwrap();
        }
    }

    #[test]
    fn detected_live_provider_adoption_preserves_an_unrelated_colliding_id() {
        let _db_guard = crate::app_db::test_db_guard();
        let tag = 40_003;
        let codex_dir = active_provider_test_dir("detected-collision", tag);
        let live = active_provider_fixture(tag, "custom", "Detected Collision", "live", "sk-live");
        let existing = save_provider_inner(active_provider_fixture(
            tag + 1,
            &custom_provider_id(&live.provider_name),
            "Existing",
            "existing-model",
            "sk-existing",
        ))
        .unwrap();
        write_active_provider_files(&codex_dir, &live);

        let activated = save_provider_toml_config_inner(ProviderTomlInput {
            config_dir: Some(codex_dir.display().to_string()),
            config_text: fs::read_to_string(config_path(&codex_dir)).unwrap(),
            api_key: live.api_key.clone(),
        })
        .unwrap();

        assert_eq!(saved_provider(&existing.id), existing);
        let retained = list_saved_providers_inner()
            .unwrap()
            .into_iter()
            .find(|provider| provider.base_url == live.base_url)
            .expect("retain detected provider beside the colliding saved ID");
        assert_ne!(retained.id, existing.id);
        assert_eq!(retained.api_key, live.api_key);
        assert_eq!(
            activated.state.active_saved_provider_id.as_deref(),
            Some(retained.id.as_str())
        );
        let selected = crate::finish_provider_selection(
            activated,
            crate::ActiveProviderSelectionUpdate::Set(existing.id.clone()),
        );
        assert_eq!(
            selected.state.active_saved_provider_id.as_deref(),
            Some(retained.id.as_str())
        );
        assert!(!selected.message.contains("当前供应商状态记录失败"));
        switch_official_provider_inner(Some(codex_dir.display().to_string())).unwrap();
        assert_eq!(saved_provider(&existing.id), existing);
        delete_provider_inner(&existing.id).unwrap();
        delete_provider_inner(&retained.id).unwrap();
        fs::remove_dir_all(codex_dir).unwrap();
    }

    #[test]
    fn failed_switch_rolls_back_new_detected_provider_adoption() {
        let _db_guard = crate::app_db::test_db_guard();
        let tag = 40_005;
        let codex_dir = active_provider_test_dir("detected-rollback", tag);
        let live = active_provider_fixture(tag, "custom", "Detected Rollback", "live", "sk-live");
        write_active_provider_files(&codex_dir, &live);
        let config_before = fs::read(config_path(&codex_dir)).unwrap();
        let auth_before = fs::read(auth_path(&codex_dir)).unwrap();

        let error = switch_provider_inner(ProviderInput {
            config_dir: Some(codex_dir.display().to_string()),
            provider_id: Some("missing-key".to_string()),
            provider_name: "Missing Key".to_string(),
            base_url: "https://missing-key.example.com/v1".to_string(),
            model: "target-model".to_string(),
            api_key: None,
            wire_api: Some("responses".to_string()),
            requires_openai_auth: Some(true),
        })
        .expect_err("reject target credentials after the pre-switch adoption");

        assert!(error.to_string().contains("需要 API Key"));
        assert!(list_saved_providers_inner()
            .unwrap()
            .iter()
            .all(|provider| provider.base_url != live.base_url));
        assert_eq!(fs::read(config_path(&codex_dir)).unwrap(), config_before);
        assert_eq!(fs::read(auth_path(&codex_dir)).unwrap(), auth_before);
        fs::remove_dir_all(codex_dir).unwrap();
    }

    #[test]
    fn detected_live_provider_only_captures_runtime_config_after_explicit_copy_selection() {
        let _db_guard = crate::app_db::test_db_guard();
        let tag = 40_006;
        let codex_dir = active_provider_test_dir("detected-ambiguous", tag);
        let live = active_provider_fixture(tag, "custom", "Duplicate", "live", "sk-live");
        let mut first = live.clone();
        first.id = "detected-ambiguous-first".to_string();
        let first = save_provider_inner(first).unwrap();
        let mut second = live.clone();
        second.id = "detected-ambiguous-second".to_string();
        let second = save_provider_inner(second).unwrap();
        write_active_provider_files(&codex_dir, &live);

        let activated = save_provider_toml_config_inner(ProviderTomlInput {
            config_dir: Some(codex_dir.display().to_string()),
            config_text: fs::read_to_string(config_path(&codex_dir)).unwrap(),
            api_key: live.api_key.clone(),
        })
        .unwrap();
        assert!(activated.state.active_saved_provider_id.is_none());
        assert_eq!(saved_provider(&first.id), first);
        assert_eq!(saved_provider(&second.id), second);
        let selected = crate::finish_provider_selection(
            activated,
            crate::ActiveProviderSelectionUpdate::Set(second.id.clone()),
        );
        assert_eq!(
            selected.state.active_saved_provider_id.as_deref(),
            Some(second.id.as_str())
        );

        switch_official_provider_inner(Some(codex_dir.display().to_string())).unwrap();

        assert_eq!(saved_provider(&first.id), first);
        let retained = saved_provider(&second.id);
        assert_eq!(retained.id, second.id);
        assert_eq!(retained.provider_name, second.provider_name);
        assert_eq!(retained.base_url, second.base_url);
        assert_eq!(retained.model, second.model);
        assert_eq!(retained.api_key, second.api_key);
        assert_eq!(retained.model_mappings, second.model_mappings);
        let config = retained
            .toml_config
            .as_ref()
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(config["approval_policy"].as_str(), Some("never"));
        assert_eq!(
            config["mcp_servers"]["docs"]["command"].as_str(),
            Some("docs-server")
        );
        assert!(config["model_providers"]["custom"]
            .get("experimental_bearer_token")
            .is_none());
        assert_eq!(
            list_saved_providers_inner()
                .unwrap()
                .iter()
                .filter(|provider| provider.base_url == live.base_url)
                .count(),
            2
        );
        delete_provider_inner(&first.id).unwrap();
        delete_provider_inner(&second.id).unwrap();
        fs::remove_dir_all(codex_dir).unwrap();
    }
    const INHERITED_COMMON_FIXTURE: &str = r#"
[desktop]
notifications = false
[marketplaces.fixture]
source = "current-local-source"
[plugins."fixture@local"]
enabled = true
[projects."/fixture/project"]
trust_level = "trusted"
[mcp_servers.current]
command = "current-mcp"
[mcp_servers.current.env]
MCP_KEY = "current-mcp-key"
[future_common]
mode = "current-future-mode"
"#;

    #[test]
    fn thin_provider_draft_inherits_live_common_and_keeps_explicit_model_preferences() {
        let dir = active_provider_test_dir("thin-draft-inheritance", 70_001);
        let current = active_provider_fixture(70_001, "current", "Current", "old-model", "old-key");
        let text = format!(
            "{}{}",
            current.toml_config.as_ref().unwrap(),
            INHERITED_COMMON_FIXTURE
        );
        fs::write(config_path(&dir), &text).unwrap();
        fs::write(auth_path(&dir), b"{\"OPENAI_API_KEY\":\"old-auth\"}").unwrap();
        let auth = fs::read(auth_path(&dir)).unwrap();
        let mut target =
            active_provider_fixture(70_002, "target", "Target", "new-model", "new-key");
        let mut template = target
            .toml_config
            .as_deref()
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        template["model_reasoning_effort"] = value("low");
        template["disable_response_storage"] = value(true);
        template["model_context_window"] = value(128000);
        target.toml_config = Some(template.to_string());
        for new_provider in [true, false] {
            let draft = build_provider_toml_draft_with_origin_inner(
                target.clone(),
                Some(dir.display().to_string()),
                new_provider,
            )
            .unwrap();
            let draft = draft.parse::<DocumentMut>().unwrap();
            assert_eq!(
                draft["mcp_servers"]["current"]["env"]["MCP_KEY"].as_str(),
                Some("current-mcp-key")
            );
            assert_eq!(draft["desktop"]["notifications"].as_bool(), Some(false));
            assert_eq!(
                draft["marketplaces"]["fixture"]["source"].as_str(),
                Some("current-local-source")
            );
            assert_eq!(
                draft["future_common"]["mode"].as_str(),
                Some("current-future-mode")
            );
            assert_eq!(draft["model"].as_str(), Some("new-model"));
            assert_eq!(draft["model_reasoning_effort"].as_str(), Some("low"));
            assert_eq!(draft["model_context_window"].as_integer(), Some(128000));
        }
        assert_eq!(fs::read_to_string(config_path(&dir)).unwrap(), text);
        assert_eq!(fs::read(auth_path(&dir)).unwrap(), auth);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn thin_live_draft_recovers_only_integration_tables_from_same_home_official_snapshot() {
        let dir = active_provider_test_dir("thin-live-snapshot", 70_003);
        let current =
            active_provider_fixture(70_003, "current", "Current", "current-model", "current-key");
        let mut text = current
            .toml_config
            .as_deref()
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        text["model_reasoning_effort"] = value("low");
        text["disable_response_storage"] = value(true);
        text["model_providers"]["custom"]["experimental_bearer_token"] = value("current-bearer");
        let text = text.to_string();
        fs::write(config_path(&dir), &text).unwrap();
        fs::write(auth_path(&dir), b"{\"OPENAI_API_KEY\":\"current-auth\"}").unwrap();
        let auth = fs::read(auth_path(&dir)).unwrap();
        let official = format!("model_provider = 'openai'\nmodel = 'official-model'\nmodel_reasoning_effort = 'xhigh'\nmodel_context_window = 1000000\nmodel_catalog_json = '/official-only-models.json'\nsandbox_mode = 'danger-full-access'\napproval_policy = 'never'\nservice_tier = 'priority'\nnotify = ['never-copy-this-command']\nexperimental_bearer_token = 'official-only-key'\n{INHERITED_COMMON_FIXTURE}");
        save_official_config_snapshot(
            &dir,
            Some(official),
            Some("official-model".into()),
            &json!({"auth_mode":"chatgpt","tokens":{"access_token":"official-private-token"}}),
        )
        .unwrap();
        let snapshot_path = super::super::official_auth::official_snapshot_path_for_profile(
            &dir,
            super::super::official_profiles::DEFAULT_OFFICIAL_PROFILE_ID,
        )
        .unwrap();
        let snapshot = fs::read(&snapshot_path).unwrap();
        let base = get_provider_config_base_inner(Some(dir.display().to_string())).unwrap();
        let doc = base.parse::<DocumentMut>().unwrap();
        for key in [
            "mcp_servers",
            "desktop",
            "marketplaces",
            "plugins",
            "projects",
        ] {
            assert!(doc.get(key).is_some(), "missing inherited {key}");
        }
        for key in [
            "sandbox_mode",
            "approval_policy",
            "service_tier",
            "notify",
            "future_common",
            "model_context_window",
            "model_catalog_json",
            "experimental_bearer_token",
        ] {
            assert!(doc.get(key).is_none(), "unexpected snapshot key {key}");
        }
        assert_eq!(doc["model"].as_str(), Some("current-model"));
        assert_eq!(doc["model_reasoning_effort"].as_str(), Some("low"));
        assert_eq!(doc["model_provider"].as_str(), Some("custom"));
        assert!(!base.contains("official-only"));
        assert!(!base.contains("official-private-token"));
        assert!(!base.contains("current-bearer"));
        assert_eq!(fs::read_to_string(config_path(&dir)).unwrap(), text);
        assert_eq!(fs::read(auth_path(&dir)).unwrap(), auth);
        assert_eq!(fs::read(&snapshot_path).unwrap(), snapshot);
        // A copied snapshot from a different CODEX_HOME is not a trusted fallback.
        let mut wrong_scope: Value = serde_json::from_slice(&snapshot).unwrap();
        wrong_scope["codexDir"] = json!(dir.join("different-home").display().to_string());
        fs::write(&snapshot_path, serde_json::to_vec(&wrong_scope).unwrap()).unwrap();
        let isolated = get_provider_config_base_inner(Some(dir.display().to_string()))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert!(isolated.get("mcp_servers").is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn normal_saved_activation_preserves_current_common_for_full_and_thin_templates() {
        let _guard = crate::app_db::test_db_guard();
        for full_template in [false, true] {
            let tag = if full_template { 70_005 } else { 70_004 };
            let dir = active_provider_test_dir("activation-common", tag);
            let current = active_provider_fixture(
                tag,
                &format!("inherit-current-{tag}"),
                "Current",
                "old-model",
                "old-key",
            );
            let current = save_provider_inner(current).unwrap();
            let mut live = current
                .toml_config
                .as_deref()
                .unwrap()
                .parse::<DocumentMut>()
                .unwrap();
            live["disable_response_storage"] = value(true);
            live["model_context_window"] = value(1000000);
            live["model_auto_compact_token_limit"] = value(900000);
            let live = format!("{}{}", live, INHERITED_COMMON_FIXTURE);
            fs::write(config_path(&dir), &live).unwrap();
            remember_active_provider_on_connection(&open_store().unwrap(), &dir, &current.id)
                .unwrap();
            let mut target = active_provider_fixture(
                tag + 20,
                &format!("inherit-target-{tag}"),
                "Target",
                "target-model",
                "target-key",
            );
            let mut template = target
                .toml_config
                .as_deref()
                .unwrap()
                .parse::<DocumentMut>()
                .unwrap();
            template["model_reasoning_effort"] = value("high");
            if full_template {
                template["disable_response_storage"] = value(false);
                template["model_context_window"] = value(128000);
            }
            target.toml_config = Some(template.to_string());
            if full_template {
                target
                    .toml_config
                    .as_mut()
                    .unwrap()
                    .push_str(&INHERITED_COMMON_FIXTURE.replace("current-", "stale-"));
            }
            let target = save_provider_inner(target).unwrap();
            activate_saved_provider_inner(Some(dir.display().to_string()), target.id.clone())
                .unwrap();
            let applied = fs::read_to_string(config_path(&dir))
                .unwrap()
                .parse::<DocumentMut>()
                .unwrap();
            assert_eq!(
                applied["mcp_servers"]["current"]["env"]["MCP_KEY"].as_str(),
                Some("current-mcp-key")
            );
            assert_eq!(applied["desktop"]["notifications"].as_bool(), Some(false));
            assert_eq!(
                applied["marketplaces"]["fixture"]["source"].as_str(),
                Some("current-local-source")
            );
            assert_eq!(
                applied["future_common"]["mode"].as_str(),
                Some("current-future-mode")
            );
            assert_eq!(applied["model"].as_str(), Some("target-model"));
            assert_eq!(applied["model_reasoning_effort"].as_str(), Some("high"));
            assert_eq!(applied["disable_response_storage"].as_bool(), Some(true));
            if full_template {
                assert_eq!(applied["model_context_window"].as_integer(), Some(128000));
            } else {
                assert!(applied.get("model_context_window").is_none());
            }
            assert!(applied.get("model_auto_compact_token_limit").is_none());
            let before = live.parse::<DocumentMut>().unwrap();
            for key in [
                "mcp_servers",
                "desktop",
                "marketplaces",
                "plugins",
                "projects",
                "future_common",
            ] {
                assert_eq!(
                    applied[key].to_string(),
                    before[key].to_string(),
                    "shared table changed: {key}"
                );
            }
            assert_eq!(
                applied["model_providers"]["custom"]["experimental_bearer_token"].as_str(),
                Some("target-key")
            );
            assert!(!applied.to_string().contains("stale-"));
            delete_provider_inner(&current.id).unwrap();
            delete_provider_inner(&target.id).unwrap();
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn active_edits_preserve_common_by_default_and_apply_explicit_manual_removal() {
        let _guard = crate::app_db::test_db_guard();
        let dir = active_provider_test_dir("active-common-policy", 70_006);
        let provider = save_provider_inner(active_provider_fixture(
            70_006,
            "active-common-policy-70006",
            "Active",
            "model",
            "active-key",
        ))
        .unwrap();
        fs::write(
            config_path(&dir),
            format!(
                "{}{}",
                provider.toml_config.as_ref().unwrap(),
                INHERITED_COMMON_FIXTURE
            ),
        )
        .unwrap();
        remember_active_provider_on_connection(&open_store().unwrap(), &dir, &provider.id).unwrap();
        let mut edited = provider.clone();
        edited.provider_name = "Renamed".into();
        edited.model = "new-model".into();
        save_active_provider_inner(edited, Some(dir.display().to_string())).unwrap();
        let after_normal = fs::read_to_string(config_path(&dir))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(
            after_normal["mcp_servers"]["current"]["command"].as_str(),
            Some("current-mcp")
        );
        let edited = saved_provider(&provider.id);
        // The explicit editor supplied only provider settings: deleted common
        // tables must stay deleted rather than being silently inherited again.
        save_active_provider_with_common_config_inner(edited, Some(dir.display().to_string()))
            .unwrap();
        let after_explicit = fs::read_to_string(config_path(&dir))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert!(after_explicit.get("mcp_servers").is_none());
        assert!(after_explicit.get("desktop").is_none());
        assert!(after_explicit.get("future_common").is_none());
        assert_eq!(after_explicit["model"].as_str(), Some("new-model"));
        delete_provider_inner(&provider.id).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn provider_base_and_activation_do_not_hide_a_malformed_live_file() {
        let _guard = crate::app_db::test_db_guard();
        let dir = active_provider_test_dir("malformed-base", 70_007);
        let malformed = "model = [\n[mcp_servers\n";
        fs::write(config_path(&dir), malformed).unwrap();
        let mut target = active_provider_fixture(
            70_007,
            "malformed-base-70007",
            "Target",
            "model",
            "fixture-key",
        );
        assert!(get_provider_config_base_inner(Some(dir.display().to_string())).is_err());
        assert!(
            build_provider_toml_draft_inner(target.clone(), Some(dir.display().to_string()))
                .is_err()
        );
        target
            .toml_config
            .as_mut()
            .unwrap()
            .push_str(INHERITED_COMMON_FIXTURE);
        let target = save_provider_inner(target).unwrap();
        assert!(
            activate_saved_provider_inner(Some(dir.display().to_string()), target.id.clone())
                .is_err()
        );
        assert_eq!(fs::read_to_string(config_path(&dir)).unwrap(), malformed);
        assert!(!auth_path(&dir).exists());
        delete_provider_inner(&target.id).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn new_or_changed_provider_drafts_drop_vendor_credentials_but_keep_mcp_env() {
        let dir = active_provider_test_dir("credential-inheritance", 70_008);
        let mut source =
            active_provider_fixture(70_008, "credential-source", "Source", "model", "source-key");
        source.toml_config.as_mut().unwrap().push_str("env_key = 'OLD_API_KEY'\nenv_key_instructions = 'old instructions'\nauth = { command = 'old-auth-command' }\nhttp_headers = { 'X-API-Key' = 'old-header-key' }\nenv_http_headers = { 'X-Project' = 'OLD_PROJECT' }\nquery_params = { api_key = 'old-query-key' }\n");
        source
            .toml_config
            .as_mut()
            .unwrap()
            .push_str(INHERITED_COMMON_FIXTURE);
        for (new_provider, change_endpoint) in [(true, false), (false, true), (false, false)] {
            let mut target = source.clone();
            if change_endpoint {
                target.base_url = "https://different.example.test/v1".into();
            }
            let doc = build_provider_toml_draft_with_origin_inner(
                target,
                Some(dir.display().to_string()),
                new_provider,
            )
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
            let route = doc["model_providers"]["custom"].as_table().unwrap();
            for key in [
                "env_key",
                "env_key_instructions",
                "auth",
                "http_headers",
                "env_http_headers",
                "query_params",
            ] {
                assert_eq!(
                    route.contains_key(key),
                    !new_provider && !change_endpoint,
                    "unexpected inherited {key}"
                );
            }
            assert_eq!(
                doc["mcp_servers"]["current"]["env"]["MCP_KEY"].as_str(),
                Some("current-mcp-key")
            );
        }
        assert!(!config_path(&dir).exists());
        assert!(!auth_path(&dir).exists());
        fs::remove_dir_all(dir).unwrap();
    }
    fn write_common_recovery_snapshot(dir: &Path) {
        let official = format!(
            "model_provider = 'openai'\nmodel = 'official-model'\n{INHERITED_COMMON_FIXTURE}"
        );
        save_official_config_snapshot(
            dir,
            Some(official),
            Some("official-model".into()),
            &json!({"auth_mode":"chatgpt","tokens":{"access_token":"snapshot-fixture-token"}}),
        )
        .unwrap();
    }

    #[test]
    fn explicit_empty_common_config_does_not_resurrect_on_later_drafts_or_activation() {
        let _guard = crate::app_db::test_db_guard();
        let dir = active_provider_test_dir("intentional-empty-common", 70_009);
        let current = save_provider_inner(active_provider_fixture(
            70_009,
            "intentional-empty-current",
            "Current",
            "current-model",
            "current-key",
        ))
        .unwrap();
        fs::write(config_path(&dir), current.toml_config.as_ref().unwrap()).unwrap();
        remember_active_provider_on_connection(&open_store().unwrap(), &dir, &current.id).unwrap();
        write_common_recovery_snapshot(&dir);
        let preview = get_provider_config_base_inner(Some(dir.display().to_string())).unwrap();
        assert!(preview.contains("[mcp_servers.current]"));
        assert!(
            !common_config_handled(&dir).unwrap(),
            "preview must not consume recovery"
        );
        let mut target = active_provider_fixture(
            70_010,
            "intentional-empty-target",
            "Target",
            "target-model",
            "target-key",
        );
        target
            .toml_config
            .as_mut()
            .unwrap()
            .push_str(INHERITED_COMMON_FIXTURE);
        let target = save_provider_inner(target).unwrap();
        assert!(
            !common_config_handled(&dir).unwrap(),
            "inactive save must not consume recovery"
        );
        // Explicitly applying the edited minimal document intentionally removes
        // all integration tables even though an older official snapshot has them.
        save_active_provider_with_common_config_inner(
            current.clone(),
            Some(dir.display().to_string()),
        )
        .unwrap();
        assert!(common_config_handled(&dir).unwrap());
        let base = get_provider_config_base_inner(Some(dir.display().to_string()))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert!(base.get("mcp_servers").is_none());
        let new_target =
            active_provider_fixture(70_011, "new-empty-draft", "New", "new-model", "new-key");
        let draft = build_provider_toml_draft_with_origin_inner(
            new_target,
            Some(dir.display().to_string()),
            true,
        )
        .unwrap()
        .parse::<DocumentMut>()
        .unwrap();
        assert!(draft.get("mcp_servers").is_none());
        activate_saved_provider_inner(Some(dir.display().to_string()), target.id.clone()).unwrap();
        let applied = fs::read_to_string(config_path(&dir))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        for key in [
            "mcp_servers",
            "desktop",
            "marketplaces",
            "plugins",
            "projects",
        ] {
            assert!(
                applied.get(key).is_none(),
                "intentionally removed {key} was resurrected"
            );
        }
        // Completion in one CODEX_HOME cannot suppress recovery in another.
        let other = active_provider_test_dir("unhandled-other-home", 70_012);
        fs::write(config_path(&other), current.toml_config.as_ref().unwrap()).unwrap();
        write_common_recovery_snapshot(&other);
        assert!(
            get_provider_config_base_inner(Some(other.display().to_string()))
                .unwrap()
                .contains("[mcp_servers.current]")
        );
        assert!(!common_config_handled(&other).unwrap());
        delete_provider_inner(&current.id).unwrap();
        delete_provider_inner(&target.id).unwrap();
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(other).unwrap();
    }

    #[test]
    fn failed_recovery_marker_write_rolls_back_live_files_and_keeps_recovery_available() {
        let _guard = crate::app_db::test_db_guard();
        let dir = active_provider_test_dir("common-marker-failure", 70_013);
        let current = save_provider_inner(active_provider_fixture(
            70_013,
            "common-marker-current",
            "Current",
            "current-model",
            "current-key",
        ))
        .unwrap();
        fs::write(config_path(&dir), current.toml_config.as_ref().unwrap()).unwrap();
        fs::write(auth_path(&dir), b"{\"OPENAI_API_KEY\":\"current-key\"}").unwrap();
        remember_active_provider_on_connection(&open_store().unwrap(), &dir, &current.id).unwrap();
        write_common_recovery_snapshot(&dir);
        let target = save_provider_inner(active_provider_fixture(
            70_014,
            "common-marker-target",
            "Target",
            "target-model",
            "target-key",
        ))
        .unwrap();
        let config_before = fs::read(config_path(&dir)).unwrap();
        let auth_before = fs::read(auth_path(&dir)).unwrap();
        let scope = crate::paths::normalized_path_scope(&dir).replace('\'', "''");
        open_store().unwrap().execute_batch(&format!("CREATE TRIGGER reject_common_marker BEFORE INSERT ON provider_common_config_state WHEN NEW.codex_dir = '{scope}' BEGIN SELECT RAISE(ABORT, 'marker-blocked'); END;")).unwrap();
        let error =
            activate_saved_provider_inner(Some(dir.display().to_string()), target.id.clone())
                .expect_err("marker write must not report success");
        open_store()
            .unwrap()
            .execute_batch("DROP TRIGGER reject_common_marker")
            .unwrap();
        assert!(error.to_string().contains("marker-blocked"), "{error}");
        assert_eq!(fs::read(config_path(&dir)).unwrap(), config_before);
        assert_eq!(fs::read(auth_path(&dir)).unwrap(), auth_before);
        assert!(!common_config_handled(&dir).unwrap());
        assert!(
            get_provider_config_base_inner(Some(dir.display().to_string()))
                .unwrap()
                .contains("[mcp_servers.current]")
        );
        activate_saved_provider_inner(Some(dir.display().to_string()), target.id.clone()).unwrap();
        assert!(common_config_handled(&dir).unwrap());
        assert!(fs::read_to_string(config_path(&dir))
            .unwrap()
            .contains("[mcp_servers.current]"));
        delete_provider_inner(&current.id).unwrap();
        delete_provider_inner(&target.id).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn recovered_common_survives_official_third_party_round_trip_with_auth_isolation() {
        let _guard = crate::app_db::test_db_guard();
        let dir = active_provider_test_dir("official-common-roundtrip", 70_015);
        fs::write(config_path(&dir), "model_provider = 'openai'\nmodel = 'official-selected'\ndisable_response_storage = true\n").unwrap();
        let official_auth = json!({"auth_mode":"chatgpt","tokens":{"access_token":"roundtrip-official-token","account_id":"roundtrip-account"}});
        write_json(&auth_path(&dir), &official_auth).unwrap();
        write_common_recovery_snapshot(&dir);
        let third = save_provider_inner(active_provider_fixture(
            70_015,
            "common-roundtrip-third",
            "Third",
            "third-model",
            "third-key",
        ))
        .unwrap();
        let assert_common = || {
            let doc = fs::read_to_string(config_path(&dir))
                .unwrap()
                .parse::<DocumentMut>()
                .unwrap();
            for key in [
                "mcp_servers",
                "desktop",
                "marketplaces",
                "plugins",
                "projects",
            ] {
                assert!(doc.get(key).is_some(), "roundtrip lost {key}");
            }
            assert_eq!(
                doc["mcp_servers"]["current"]["env"]["MCP_KEY"].as_str(),
                Some("current-mcp-key")
            );
            doc
        };
        activate_saved_provider_inner(Some(dir.display().to_string()), third.id.clone()).unwrap();
        assert_eq!(assert_common()["model"].as_str(), Some("third-model"));
        assert!(common_config_handled(&dir).unwrap());
        super::super::official_profiles::switch_official_profile_inner(
            Some(dir.display().to_string()),
            super::super::official_profiles::DEFAULT_OFFICIAL_PROFILE_ID.to_string(),
        )
        .unwrap();
        let official = assert_common();
        assert_eq!(official["model"].as_str(), Some("official-selected"));
        assert!(document_is_official(&official));
        assert!(!official.to_string().contains("third-key"));
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(auth_path(&dir)).unwrap()).unwrap(),
            official_auth
        );
        activate_saved_provider_inner(Some(dir.display().to_string()), third.id.clone()).unwrap();
        let third_doc = assert_common();
        assert_eq!(third_doc["model"].as_str(), Some("third-model"));
        assert_eq!(
            third_doc["model_providers"]["custom"]["experimental_bearer_token"].as_str(),
            Some("third-key")
        );
        assert!(!third_doc.to_string().contains("roundtrip-official-token"));
        assert!(!fs::read_to_string(auth_path(&dir))
            .unwrap_or_default()
            .contains("roundtrip-official-token"));
        delete_provider_inner(&third.id).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
}
