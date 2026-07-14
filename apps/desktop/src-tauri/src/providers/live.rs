use super::{
    custom_provider_id, experimental_bearer_token_from_doc, reserved_codex_provider_id,
    save_detected_provider_inner, SavedProvider,
};
use crate::backups::create_backup;
use crate::error::{CodexxError, Result};
use crate::file_io::{
    io_err, json_err, parse_toml_document, read_to_string_if_exists, write_json, write_text,
};
use crate::state::{build_state, ActionResult};
use crate::toml_utils::ensure_table;
use crate::{auth_path, codex_section_from_table, config_path, resolve_codex_dir, string_value};
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use toml_edit::{value, DocumentMut};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderInput {
    pub(crate) config_dir: Option<String>,
    #[serde(rename = "providerId")]
    pub(crate) _provider_id: Option<String>,
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
}

fn live_auth_api_key(codex_dir: &Path) -> Result<Option<String>> {
    let path = auth_path(codex_dir);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|e| io_err(&path, e))?;
    let auth: Value = serde_json::from_str(&text).map_err(|e| json_err(&path, e))?;
    Ok(auth
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(ToString::to_string))
}

fn strip_provider_bearer_tokens(doc: &mut DocumentMut) {
    doc.as_table_mut().remove("experimental_bearer_token");
    if let Some(providers) = doc
        .get_mut("model_providers")
        .and_then(|item| item.as_table_mut())
    {
        for (_, item) in providers.iter_mut() {
            if let Some(table) = item.as_table_mut() {
                table.remove("experimental_bearer_token");
            }
        }
    }
}

pub(crate) fn detected_live_custom_provider(codex_dir: &Path) -> Result<Option<SavedProvider>> {
    let cfg = config_path(codex_dir);
    let text = read_to_string_if_exists(&cfg)?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    let mut doc = parse_toml_document(&cfg, &text)?;
    let Some(provider_id) = string_value(&doc, "model_provider") else {
        return Ok(None);
    };
    if reserved_codex_provider_id(&provider_id) {
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

    let api_key = match experimental_bearer_token_from_doc(&doc, Some(&provider_id)) {
        Some(api_key) => Some(api_key),
        None => live_auth_api_key(codex_dir)?,
    };
    strip_provider_bearer_tokens(&mut doc);
    let provider_name = section
        .name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| provider_id.clone());
    let toml_config = doc.to_string().trim_end().to_string();

    Ok(Some(SavedProvider {
        id: custom_provider_id(&provider_id),
        provider_name,
        base_url: section.base_url,
        model,
        api_key,
        toml_config: (!toml_config.is_empty()).then_some(toml_config),
        wire_api: section.wire_api,
        requires_openai_auth: section.requires_openai_auth,
    }))
}

fn persist_detected_live_custom_provider(codex_dir: &Path) -> Result<()> {
    if let Some(provider) = detected_live_custom_provider(codex_dir)? {
        save_detected_provider_inner(provider)?;
    }
    Ok(())
}

fn set_top_level_defaults(doc: &mut DocumentMut) {
    if doc.get("model_reasoning_effort").is_none() {
        doc["model_reasoning_effort"] = value("high");
    }
    if doc.get("disable_response_storage").is_none() {
        doc["disable_response_storage"] = value(true);
    }
}

fn set_provider_bearer_token(doc: &mut DocumentMut, token: &str) {
    let token = token.trim();
    if token.is_empty() {
        return;
    }
    let provider_id = string_value(doc, "model_provider");
    if let Some(provider_id) = provider_id {
        if let Some(provider_table) = doc
            .get_mut("model_providers")
            .and_then(|item| item.as_table_mut())
            .and_then(|providers| providers.get_mut(provider_id.as_str()))
            .and_then(|item| item.as_table_mut())
        {
            provider_table["experimental_bearer_token"] = value(token);
            return;
        }
    }
    doc["experimental_bearer_token"] = value(token);
}

fn apply_official_config(
    config_dir: Option<String>,
    model: Option<String>,
    auth_json: Option<String>,
    action: &str,
    message: &str,
) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    fs::create_dir_all(&codex_dir).map_err(|e| io_err(&codex_dir, e))?;
    let cfg = config_path(&codex_dir);
    let auth = auth_path(&codex_dir);
    let backup_id = create_backup(&codex_dir, action)?;

    let text = read_to_string_if_exists(&cfg)?;
    let mut doc = parse_toml_document(&cfg, &text)?;

    // 官方模式显式指向 Codex 内置 OpenAI provider，避免从第三方 custom
    // 切回官方时仍被旧版 Codex/缓存误判为自定义路由。
    doc["model_provider"] = value("openai");
    let mut remove_model_providers = false;
    if let Some(providers) = doc
        .as_table_mut()
        .get_mut("model_providers")
        .and_then(|item| item.as_table_mut())
    {
        providers.remove("custom");
        remove_model_providers = providers.is_empty();
    }
    if remove_model_providers {
        doc.as_table_mut().remove("model_providers");
    }

    if let Some(model) = model
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        doc["model"] = value(model);
    }

    write_text(&cfg, &doc.to_string())?;

    if let Some(auth_json) = auth_json
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        let parsed: Value = serde_json::from_str(&auth_json).map_err(|e| json_err(&auth, e))?;
        if !parsed.is_object() {
            return Err(CodexxError::Config(
                "auth.json 必须是 JSON object".to_string(),
            ));
        }
        write_json(&auth, &parsed)?;
    }

    let state = build_state(codex_dir)?;
    Ok(ActionResult {
        ok: true,
        message: message.to_string(),
        backup_id,
        state,
    })
}

pub(crate) fn switch_official_provider_with_pre_persist<F>(
    config_dir: Option<String>,
    pre_persist: F,
) -> Result<ActionResult>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let codex_dir = resolve_codex_dir(config_dir.clone())?;
    pre_persist(&codex_dir)?;
    // Switching to official must not overwrite auth.json with a stale cc-switch
    // ChatGPT token. Codex desktop/CLI owns the live official login flow; after
    // the user logs in, Codex-X should simply refresh and display ~/.codex/auth.json.
    apply_official_config(
        config_dir,
        None,
        None,
        "switch-official",
        "已切换到 OpenAI Official（auth.json 保持当前 live 状态）",
    )
}

pub(crate) fn switch_official_provider_inner(config_dir: Option<String>) -> Result<ActionResult> {
    switch_official_provider_with_pre_persist(config_dir, persist_detected_live_custom_provider)
}

pub(crate) fn save_official_config_inner(
    config_dir: Option<String>,
    model: Option<String>,
    auth_json: Option<String>,
) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    fs::create_dir_all(&codex_dir).map_err(|e| io_err(&codex_dir, e))?;
    let cfg = config_path(&codex_dir);
    let auth = auth_path(&codex_dir);
    let backup_id = create_backup(&codex_dir, "save-official")?;

    let text = read_to_string_if_exists(&cfg)?;
    let mut doc = parse_toml_document(&cfg, &text)?;
    if let Some(model) = model
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        doc["model"] = value(model);
        write_text(&cfg, &doc.to_string())?;
    }

    if let Some(auth_json) = auth_json
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        let parsed: Value = serde_json::from_str(&auth_json).map_err(|e| json_err(&auth, e))?;
        if !parsed.is_object() {
            return Err(CodexxError::Config(
                "auth.json 必须是 JSON object".to_string(),
            ));
        }
        write_json(&auth, &parsed)?;
    }

    let state = build_state(codex_dir)?;
    Ok(ActionResult {
        ok: true,
        message: "已保存 OpenAI Official 配置（未切换启用）".to_string(),
        backup_id,
        state,
    })
}

pub(crate) fn save_provider_toml_config_with_pre_persist<F>(
    input: ProviderTomlInput,
    pre_persist: F,
) -> Result<ActionResult>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let codex_dir = resolve_codex_dir(input.config_dir.clone())?;
    fs::create_dir_all(&codex_dir).map_err(|e| io_err(&codex_dir, e))?;
    pre_persist(&codex_dir)?;
    let cfg = config_path(&codex_dir);
    let auth = auth_path(&codex_dir);
    let backup_id = create_backup(&codex_dir, "save-provider-toml")?;

    let config_text = input.config_text.trim_end().to_string();
    let mut doc = parse_toml_document(&cfg, &config_text)?;
    if string_value(&doc, "model").is_none() {
        return Err(CodexxError::Config(
            "config.toml 必须包含 model".to_string(),
        ));
    }
    let api_key = input
        .api_key
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(api_key) = api_key.as_deref() {
        set_provider_bearer_token(&mut doc, api_key);
    }
    write_text(&cfg, &(doc.to_string().trim_end().to_string() + "\n"))?;

    if let Some(api_key) = api_key {
        let mut auth_value = if auth.exists() {
            let text = fs::read_to_string(&auth).map_err(|e| io_err(&auth, e))?;
            serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!({}))
        } else {
            json!({})
        };
        if !auth_value.is_object() {
            auth_value = json!({});
        }
        auth_value["OPENAI_API_KEY"] = Value::String(api_key);
        auth_value["auth_mode"] = Value::String("apikey".to_string());
        write_json(&auth, &auth_value)?;
    }

    let state = build_state(codex_dir)?;
    Ok(ActionResult {
        ok: true,
        message: "已保存供应商 TOML 配置".to_string(),
        backup_id,
        state,
    })
}

pub(crate) fn save_provider_toml_config_inner(input: ProviderTomlInput) -> Result<ActionResult> {
    save_provider_toml_config_with_pre_persist(input, persist_detected_live_custom_provider)
}

pub(crate) fn switch_provider_with_pre_persist<F>(
    input: ProviderInput,
    pre_persist: F,
) -> Result<ActionResult>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let codex_dir = resolve_codex_dir(input.config_dir.clone())?;
    fs::create_dir_all(&codex_dir).map_err(|e| io_err(&codex_dir, e))?;
    pre_persist(&codex_dir)?;
    let cfg = config_path(&codex_dir);
    let auth = auth_path(&codex_dir);
    let backup_id = create_backup(&codex_dir, "switch-provider")?;

    let provider_name = input.provider_name.trim();
    // Keep the saved provider id only for Codex-X/cc-switch bookkeeping.
    // cc-switch writes third-party Codex providers to the live config as
    // `model_provider = "custom"` + `[model_providers.custom]`; mirroring
    // that behavior avoids Codex CLI/App versions that ignore arbitrary live
    // provider ids or keep resolving the previous custom provider.
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

    let text = read_to_string_if_exists(&cfg)?;
    let mut doc = parse_toml_document(&cfg, &text)?;
    doc["model_provider"] = value(live_provider_key);
    doc["model"] = value(model);
    set_top_level_defaults(&mut doc);

    let root = doc.as_table_mut();
    let providers = ensure_table(root, "model_providers")?;
    providers.remove(live_provider_key);
    let provider_table = ensure_table(providers, live_provider_key)?;
    provider_table["name"] = value(provider_name);
    provider_table["base_url"] = value(base_url);
    provider_table["wire_api"] = value(input.wire_api.unwrap_or_else(|| "responses".to_string()));
    provider_table["requires_openai_auth"] = value(input.requires_openai_auth.unwrap_or(true));

    let api_key = input
        .api_key
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(api_key) = api_key.as_deref() {
        // New threads reload config.toml, while a running app-server may retain
        // the auth.json credential it loaded at startup. The provider-scoped
        // token makes provider switches effective without restarting Codex.
        set_provider_bearer_token(&mut doc, api_key);
    }
    write_text(&cfg, &doc.to_string())?;

    if let Some(api_key) = api_key {
        let auth_value = json!({
            "OPENAI_API_KEY": api_key,
            "auth_mode": "apikey",
        });
        write_json(&auth, &auth_value)?;
    }

    let state = build_state(codex_dir)?;
    Ok(ActionResult {
        ok: true,
        message: format!("已切换到 {provider_name} / {model}"),
        backup_id,
        state,
    })
}

pub(crate) fn switch_provider_inner(input: ProviderInput) -> Result<ActionResult> {
    switch_provider_with_pre_persist(input, persist_detected_live_custom_provider)
}
