use super::{clear_provider_selections_on_connection, open_store as open_db};
use crate::error::{CodexxError, Result};
use crate::paths::normalized_path_scope;
use crate::{now_rfc3339, resolve_codex_dir, sanitize_id};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use toml_edit::{value, DocumentMut, Item, Table};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ProviderHeaderSource {
    Static,
    Env,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderHeader {
    pub(crate) name: String,
    pub(crate) value: String,
    pub(crate) source: ProviderHeaderSource,
}

fn provider_headers_from_table(table: &Table) -> Result<Vec<ProviderHeader>> {
    let mut result = Vec::new();
    for (field, source) in [
        ("http_headers", ProviderHeaderSource::Static),
        ("env_http_headers", ProviderHeaderSource::Env),
    ] {
        let Some(item) = table.get(field) else {
            continue;
        };
        let headers = item
            .as_table_like()
            .ok_or_else(|| CodexxError::Config(format!("供应商 {field} 必须是键值表")))?;
        for (name, item) in headers.iter() {
            let text = item
                .as_str()
                .ok_or_else(|| CodexxError::Config(format!("供应商 {field} 的值必须是文本")))?;
            result.push(ProviderHeader {
                name: name.to_string(),
                value: text.to_string(),
                source,
            });
        }
    }
    Ok(result)
}

fn validate_provider_headers(headers: &[ProviderHeader]) -> Result<()> {
    let mut names = HashSet::new();
    for (index, header) in headers.iter().enumerate() {
        let row = index + 1;
        reqwest::header::HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|_| CodexxError::Config(format!("第 {row} 个供应商 Header 名称无效")))?;
        if !names.insert(header.name.to_ascii_lowercase()) {
            return Err(CodexxError::Config(format!(
                "第 {row} 个供应商 Header 名称重复（不区分大小写）"
            )));
        }
        match header.source {
            ProviderHeaderSource::Static => {
                reqwest::header::HeaderValue::from_str(&header.value).map_err(|_| {
                    CodexxError::Config(format!(
                        "第 {row} 个供应商 Header ({}) 值无效，不能包含换行或控制字符",
                        header.name
                    ))
                })?;
            }
            ProviderHeaderSource::Env => {
                let mut bytes = header.value.bytes();
                let valid_first = bytes
                    .next()
                    .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_');
                if !valid_first || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
                    return Err(CodexxError::Config(format!(
                        "第 {row} 个供应商 Header ({}) 环境变量名无效，请使用字母、数字和下划线，且不要以数字开头", header.name
                    )));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn validate_provider_header_table(table: &Table) -> Result<()> {
    validate_provider_headers(&provider_headers_from_table(table)?)
}

fn selected_provider_id(doc: &DocumentMut) -> Result<String> {
    doc.get("model_provider")
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| CodexxError::Config("供应商 TOML 缺少 model_provider".to_string()))
}

/// Read only the selected provider's headers. Invalid names and values remain
/// visible to the editor so users can repair older configurations.
pub(crate) fn read_provider_headers_inner(config_text: String) -> Result<Vec<ProviderHeader>> {
    let doc = config_text
        .parse::<DocumentMut>()
        .map_err(|_| CodexxError::Config("供应商 TOML 无效，请先修正配置".to_string()))?;
    let provider_id = selected_provider_id(&doc)?;
    let table = doc
        .get("model_providers")
        .and_then(Item::as_table)
        .and_then(|providers| providers.get(&provider_id))
        .and_then(Item::as_table)
        .ok_or_else(|| CodexxError::Config("供应商 TOML 缺少当前供应商配置表".to_string()))?;
    provider_headers_from_table(table)
}

/// Patch Codex's native TOML fields without changing credentials or unrelated
/// provider/shared configuration. An empty list removes both header fields.
pub(crate) fn update_provider_headers_inner(
    config_text: String,
    mut headers: Vec<ProviderHeader>,
) -> Result<String> {
    for header in &mut headers {
        header.name = header.name.trim().to_string();
    }
    validate_provider_headers(&headers)?;
    let mut doc = config_text
        .parse::<DocumentMut>()
        .map_err(|_| CodexxError::Config("供应商 TOML 无效，请先修正配置".to_string()))?;
    let provider_id = selected_provider_id(&doc)?;
    let table = doc
        .get_mut("model_providers")
        .and_then(Item::as_table_mut)
        .and_then(|providers| providers.get_mut(&provider_id))
        .and_then(Item::as_table_mut)
        .ok_or_else(|| CodexxError::Config("供应商 TOML 缺少当前供应商配置表".to_string()))?;
    for (field, source) in [
        ("http_headers", ProviderHeaderSource::Static),
        ("env_http_headers", ProviderHeaderSource::Env),
    ] {
        let rows = headers
            .iter()
            .filter(|header| header.source == source)
            .collect::<Vec<_>>();
        if rows.is_empty() {
            table.remove(field);
            continue;
        }
        // Retain table style, comments and ordering for existing header names.
        if table.get(field).and_then(Item::as_table_like).is_none() {
            table.insert(field, Item::Table(Table::new()));
        }
        let values = table
            .get_mut(field)
            .and_then(Item::as_table_like_mut)
            .unwrap();
        let removed = values
            .iter()
            .filter(|(name, _)| !rows.iter().any(|header| header.name == *name))
            .map(|(name, _)| name.to_string())
            .collect::<Vec<_>>();
        for name in removed {
            values.remove(&name);
        }
        for header in rows {
            if values.get(&header.name).and_then(Item::as_str) != Some(header.value.as_str()) {
                values.insert(&header.name, value(header.value.clone()));
            }
        }
    }
    Ok(doc.to_string().trim_end().to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SavedProvider {
    pub(crate) id: String,
    pub(crate) provider_name: String,
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) api_key: Option<String>,
    pub(crate) toml_config: Option<String>,
    pub(crate) wire_api: String,
    /// Upstream format used by the app's local bridge; Codex still speaks Responses.
    #[serde(default)]
    pub(crate) upstream_api: Option<String>,
    pub(crate) requires_openai_auth: bool,
    #[serde(default)]
    pub(crate) model_mappings: Vec<super::model_catalog::ProviderModelMapping>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredProvider {
    provider: SavedProvider,
    created_at: String,
    updated_at: String,
    source: String,
    source_id: Option<String>,
}

pub(crate) struct ProviderStoreRollback {
    before: Vec<StoredProvider>,
    after: Vec<StoredProvider>,
}

const MANUAL_PROVIDER_SOURCE: &str = "manual";
pub(crate) const CCSWITCH_PROVIDER_SOURCE: &str = "cc-switch";
const CCSWITCH_LOCAL_PROVIDER_SOURCE: &str = "cc-switch-local";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ProviderIdentity {
    Credential([u8; 32]),
    Unauthenticated { base_url: String, name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProviderProfileIdentity {
    // Matching helper only; manually saved rows are always addressed by ID.
    provider: ProviderIdentity,
    model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderUpsertMode {
    Manual,
    Imported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderUpsertKind {
    Added,
    Updated,
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderUpsertResult {
    pub(crate) provider: SavedProvider,
    pub(crate) kind: ProviderUpsertKind,
}

pub(crate) fn canonical_provider_base_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if let Ok(parsed) = ureq::get(trimmed).request_url() {
        let url = parsed.as_url();
        if let Some(host) = url.host_str() {
            let mut canonical = format!("{}://", url.scheme().to_ascii_lowercase());
            if !url.username().is_empty() {
                canonical.push_str(url.username());
                if let Some(password) = url.password() {
                    canonical.push(':');
                    canonical.push_str(password);
                }
                canonical.push('@');
            }
            if host.contains(':') && !host.starts_with('[') {
                canonical.push('[');
                canonical.push_str(&host.to_ascii_lowercase());
                canonical.push(']');
            } else {
                canonical.push_str(&host.to_ascii_lowercase());
            }
            if let Some(port) = url.port() {
                canonical.push(':');
                canonical.push_str(&port.to_string());
            }
            let path = url.path().trim_end_matches('/');
            if !path.is_empty() {
                canonical.push_str(path);
            }
            if let Some(query) = url.query() {
                canonical.push('?');
                canonical.push_str(query);
            }
            return canonical;
        }
    }

    trimmed.trim_end_matches('/').to_string()
}

fn normalized_provider_name(input: &str) -> String {
    input
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub(crate) fn is_placeholder_provider(provider_name: &str, base_url: &str) -> bool {
    normalized_provider_name(provider_name) == "your-provider"
        && canonical_provider_base_url(base_url) == "https://example.com/v1"
}

fn effective_provider_api_key(provider: &SavedProvider) -> Option<String> {
    provider
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            let text = provider.toml_config.as_deref()?;
            let doc = text.parse::<DocumentMut>().ok()?;
            let provider_id = doc
                .get("model_provider")
                .and_then(|item| item.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty());
            experimental_bearer_token_from_doc(&doc, provider_id)
        })
}

pub(crate) fn provider_identity(provider: &SavedProvider) -> Option<ProviderIdentity> {
    use sha2::{Digest, Sha256};

    let base_url = canonical_provider_base_url(&provider.base_url);
    if base_url.is_empty() {
        return None;
    }
    if let Some(api_key) = effective_provider_api_key(provider) {
        // Hash the complete endpoint/credential tuple so neither the key nor a
        // reusable key-only fingerprint is persisted, logged, or sent to the UI.
        let mut hasher = Sha256::new();
        hasher.update(b"codex-x/provider-identity/v1\0");
        hasher.update(base_url.as_bytes());
        hasher.update(b"\0");
        hasher.update(api_key.as_bytes());
        let digest: [u8; 32] = hasher.finalize().into();
        return Some(ProviderIdentity::Credential(digest));
    }

    let name = normalized_provider_name(&provider.provider_name);
    (!name.is_empty()).then_some(ProviderIdentity::Unauthenticated { base_url, name })
}

fn provider_profile_identity(provider: &SavedProvider) -> Option<ProviderProfileIdentity> {
    provider_identity(provider).map(|identity| ProviderProfileIdentity {
        provider: identity,
        model: provider.model.trim().to_string(),
    })
}

pub(crate) fn provider_template_from_document(
    doc: &DocumentMut,
    provider_id: &str,
    model: &str,
) -> Result<String> {
    let provider_table = doc
        .get("model_providers")
        .and_then(|item| item.as_table())
        .and_then(|providers| providers.get(provider_id))
        .and_then(|item| item.as_table())
        .ok_or_else(|| {
            CodexxError::Config(format!("供应商 TOML 缺少 [model_providers.{provider_id}]"))
        })?;
    validate_provider_header_table(provider_table)?;

    let mut template = doc.clone();
    template["model_provider"] = value(provider_id);
    template["model"] = value(model);
    strip_provider_bearer_tokens(&mut template);
    Ok(template.to_string().trim_end().to_string())
}

pub(crate) fn strip_provider_bearer_tokens(doc: &mut DocumentMut) {
    doc.as_table_mut().remove("experimental_bearer_token");
    let Some(providers) = doc
        .get_mut("model_providers")
        .and_then(|item| item.as_table_mut())
    else {
        return;
    };
    for (_, item) in providers.iter_mut() {
        if let Some(table) = item.as_table_mut() {
            table.remove("experimental_bearer_token");
        }
    }
}

fn same_provider_endpoint(left: &SavedProvider, right: &SavedProvider) -> bool {
    let left = canonical_provider_base_url(&left.base_url);
    !left.is_empty() && left == canonical_provider_base_url(&right.base_url)
}

fn same_provider_endpoint_and_name(left: &SavedProvider, right: &SavedProvider) -> bool {
    same_provider_endpoint(left, right)
        && normalized_provider_name(&left.provider_name)
            == normalized_provider_name(&right.provider_name)
}

fn same_provider_profile_fallback(left: &SavedProvider, right: &SavedProvider) -> bool {
    same_provider_endpoint_and_name(left, right) && left.model.trim() == right.model.trim()
}

fn compatible_provider_match(left: &SavedProvider, right: &SavedProvider) -> bool {
    if !same_provider_profile_fallback(left, right) {
        return false;
    }
    match (
        effective_provider_api_key(left),
        effective_provider_api_key(right),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    }
}

pub(crate) fn matching_saved_provider_ids_for_live(
    live: &SavedProvider,
    providers: &[SavedProvider],
) -> Vec<String> {
    let candidates = providers.iter().collect::<Vec<_>>();
    if let Some(identity) = provider_profile_identity(live) {
        let exact = candidates
            .iter()
            .copied()
            .filter(|candidate| provider_profile_identity(candidate).as_ref() == Some(&identity))
            .collect::<Vec<_>>();
        if !exact.is_empty() {
            return exact
                .into_iter()
                .map(|candidate| candidate.id.clone())
                .collect();
        }
    }

    let compatible = candidates
        .iter()
        .copied()
        .filter(|candidate| compatible_provider_match(live, candidate))
        .collect::<Vec<_>>();
    if !compatible.is_empty() {
        return compatible
            .into_iter()
            .map(|candidate| candidate.id.clone())
            .collect();
    }

    if !is_historical_custom_provider_id(&live.id) {
        return Vec::new();
    }
    candidates
        .into_iter()
        .filter(|candidate| same_provider_profile_fallback(live, candidate))
        .map(|candidate| candidate.id.clone())
        .collect()
}

fn same_live_provider_route(live: &SavedProvider, saved: &SavedProvider) -> bool {
    if !same_provider_endpoint(live, saved) {
        return false;
    }
    match (
        effective_provider_api_key(live),
        effective_provider_api_key(saved),
    ) {
        (Some(live_key), Some(saved_key)) => live_key == saved_key,
        // Missing live authentication is tolerated for legacy providers, but
        // without credentials a changed name is not enough evidence to adopt it.
        _ => {
            normalized_provider_name(&live.provider_name)
                == normalized_provider_name(&saved.provider_name)
        }
    }
}

/// A saved record keeps its identity when Codex changes the runtime model or
/// display name. Prefer the remembered record only while its route/credential
/// still matches; otherwise fall back to the existing profile matching rules.
/// This lookup never changes a selection or writes a provider.
pub(crate) fn matching_saved_provider_ids_for_live_on_connection(
    conn: &Connection,
    codex_dir: &std::path::Path,
    live: &SavedProvider,
    providers: &[SavedProvider],
) -> Result<Vec<String>> {
    if let Some(selected) = super::selection::selected_provider_id_on_connection(conn, codex_dir)? {
        if providers
            .iter()
            .any(|provider| provider.id == selected && same_live_provider_route(live, provider))
        {
            return Ok(vec![selected]);
        }
    }
    let matches = matching_saved_provider_ids_for_live(live, providers)
        .into_iter()
        .filter(|id| {
            providers
                .iter()
                .any(|provider| &provider.id == id && same_live_provider_route(live, provider))
        })
        .collect::<Vec<_>>();
    if !matches.is_empty() {
        return Ok(matches);
    }
    // Several same-API records remain ambiguous without a remembered ID. Return
    // all candidates so callers can leave them alone rather than create a copy.
    Ok(providers
        .iter()
        .filter(|provider| same_live_provider_route(live, provider))
        .map(|provider| provider.id.clone())
        .collect())
}

#[cfg(test)]
fn unique_saved_provider_id_for_live(
    live: &SavedProvider,
    providers: &[SavedProvider],
) -> Option<String> {
    let matches = matching_saved_provider_ids_for_live(live, providers);
    (matches.len() == 1).then(|| matches[0].clone())
}

fn saved_provider_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SavedProvider> {
    Ok(SavedProvider {
        id: row.get(0)?,
        provider_name: row.get(1)?,
        base_url: row.get(2)?,
        model: row.get(3)?,
        api_key: row.get(4)?,
        toml_config: row.get(5)?,
        wire_api: row.get(6)?,
        upstream_api: row.get(9)?,
        requires_openai_auth: row.get::<_, i64>(7)? != 0,
        model_mappings: serde_json::from_str(&row.get::<_, String>(8)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                8,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
    })
}

fn stored_providers_on_connection(conn: &Connection) -> Result<Vec<StoredProvider>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, provider_name, base_url, model, api_key, toml_config, wire_api,
                    requires_openai_auth, model_mappings_json, upstream_api, created_at, updated_at, source, source_id
             FROM providers
             ORDER BY created_at ASC, rowid ASC",
        )
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(StoredProvider {
                provider: saved_provider_from_row(row)?,
                created_at: row.get(10)?,
                updated_at: row.get(11)?,
                source: row.get(12)?,
                source_id: row.get(13)?,
            })
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;

    let mut providers = Vec::new();
    for row in rows {
        let mut stored = row.map_err(|e| CodexxError::Database(e.to_string()))?;
        normalize_stored_provider_from_toml(&mut stored.provider);
        providers.push(stored);
    }
    Ok(providers)
}

pub(crate) fn list_saved_providers_on_connection(conn: &Connection) -> Result<Vec<SavedProvider>> {
    Ok(stored_providers_on_connection(conn)?
        .into_iter()
        .map(|stored| stored.provider)
        .collect())
}

pub(crate) fn list_saved_providers_inner() -> Result<Vec<SavedProvider>> {
    let conn = open_db()?;
    list_saved_providers_on_connection(&conn)
}

fn validate_provider_card_order(order: &[String]) -> Result<()> {
    if order.len() > 4096 {
        return Err(CodexxError::Config("供应商排序项过多".to_string()));
    }
    let mut seen = HashSet::with_capacity(order.len());
    for key in order {
        let valid_key = key.split_once(':').is_some_and(|(source, id)| {
            matches!(source, "official" | "local" | "detected")
                && !id.trim().is_empty()
                && id.trim() == id
                && key.len() <= 512
                && !key.chars().any(char::is_control)
        });
        if !valid_key {
            return Err(CodexxError::Config(
                "供应商排序包含无效卡片标识".to_string(),
            ));
        }
        if !seen.insert(key) {
            return Err(CodexxError::Config("供应商排序包含重复卡片".to_string()));
        }
    }
    Ok(())
}

fn get_provider_order_on_connection(
    conn: &Connection,
    codex_dir: &std::path::Path,
) -> Result<Vec<String>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT order_json FROM provider_card_orders WHERE codex_dir = ?1",
            [normalized_path_scope(codex_dir)],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    json.map(|json| {
        serde_json::from_str(&json)
            .map_err(|error| CodexxError::Database(format!("读取供应商排序失败: {error}")))
    })
    .transpose()
    .map(Option::unwrap_or_default)
}

pub(crate) fn get_provider_order_inner(config_dir: Option<String>) -> Result<Vec<String>> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    get_provider_order_on_connection(&open_db()?, &codex_dir)
}

fn save_provider_order_on_connection(
    conn: &mut Connection,
    codex_dir: &std::path::Path,
    order: Vec<String>,
) -> Result<Vec<String>> {
    validate_provider_card_order(&order)?;
    let json = serde_json::to_string(&order)
        .map_err(|error| CodexxError::Config(format!("序列化供应商排序失败: {error}")))?;
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    // Keep the preference independent of provider records. A refresh, import,
    // edit, or temporary detected row must not change the user's chosen order;
    // the UI ignores absent keys and appends newly added cards.
    transaction
        .execute(
            "INSERT INTO provider_card_orders (codex_dir, order_json, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(codex_dir) DO UPDATE SET
                order_json = excluded.order_json,
                updated_at = excluded.updated_at",
            params![normalized_path_scope(codex_dir), json, now_rfc3339()],
        )
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    transaction
        .commit()
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    Ok(order)
}

pub(crate) fn save_provider_order_inner(
    config_dir: Option<String>,
    order: Vec<String>,
) -> Result<Vec<String>> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    save_provider_order_on_connection(&mut open_db()?, &codex_dir, order)
}

pub(crate) fn provider_by_id_on_connection(
    conn: &Connection,
    id: &str,
) -> Result<Option<SavedProvider>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, provider_name, base_url, model, api_key, toml_config, wire_api,
                    requires_openai_auth, model_mappings_json, upstream_api
             FROM providers WHERE id = ?1 LIMIT 1",
        )
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let provider = stmt
        .query_row([id], saved_provider_from_row)
        .map(Some)
        .or_else(|error| {
            if matches!(error, rusqlite::Error::QueryReturnedNoRows) {
                Ok(None)
            } else {
                Err(error)
            }
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    Ok(provider.map(|mut provider| {
        normalize_stored_provider_from_toml(&mut provider);
        provider
    }))
}

fn write_provider_with_origin(
    conn: &Connection,
    provider: &SavedProvider,
    origin: Option<(&str, &str)>,
) -> Result<()> {
    let now = now_rfc3339();
    let model_mappings_json = serde_json::to_string(&provider.model_mappings)
        .map_err(|error| CodexxError::Config(format!("序列化模型映射失败: {error}")))?;
    let (source, source_id) = origin
        .map(|(source, source_id)| (source, Some(source_id)))
        .unwrap_or((MANUAL_PROVIDER_SOURCE, None));
    conn.execute(
        "INSERT INTO providers
            (id, provider_name, base_url, model, api_key, toml_config, wire_api,
             requires_openai_auth, model_mappings_json, upstream_api, source, source_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)
         ON CONFLICT(id) DO UPDATE SET
            provider_name = excluded.provider_name,
            base_url = excluded.base_url,
            model = excluded.model,
            api_key = excluded.api_key,
            toml_config = excluded.toml_config,
            wire_api = excluded.wire_api,
            requires_openai_auth = excluded.requires_openai_auth,
            model_mappings_json = excluded.model_mappings_json,
            upstream_api = excluded.upstream_api,
            source = CASE
                WHEN excluded.source_id IS NULL THEN providers.source
                ELSE excluded.source
            END,
            source_id = CASE
                WHEN excluded.source_id IS NULL THEN providers.source_id
                ELSE excluded.source_id
            END,
            updated_at = excluded.updated_at",
        params![
            provider.id,
            provider.provider_name,
            provider.base_url,
            provider.model,
            provider.api_key,
            provider.toml_config,
            provider.wire_api,
            if provider.requires_openai_auth { 1 } else { 0 },
            model_mappings_json,
            provider.upstream_api,
            source,
            source_id,
            now,
        ],
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
fn write_provider_on_connection(conn: &Connection, provider: &SavedProvider) -> Result<()> {
    write_provider_with_origin(conn, provider, None)
}

fn unique_provider_id_on_connection(conn: &Connection, preferred: &str) -> Result<String> {
    if provider_by_id_on_connection(conn, preferred)?.is_none() {
        return Ok(preferred.to_string());
    }
    let mut index = 2usize;
    loop {
        let candidate = format!("{preferred}-{index}");
        if provider_by_id_on_connection(conn, &candidate)?.is_none() {
            return Ok(candidate);
        }
        index += 1;
    }
}

fn merge_authoritative_import(
    mut incoming: SavedProvider,
    existing: &SavedProvider,
) -> SavedProvider {
    incoming.id = existing.id.clone();
    if incoming.provider_name.trim().is_empty() {
        incoming.provider_name = existing.provider_name.clone();
    }
    if incoming.base_url.trim().is_empty() {
        incoming.base_url = existing.base_url.clone();
    }
    if incoming.model.trim().is_empty() {
        incoming.model = existing.model.clone();
    }
    if incoming
        .api_key
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        incoming.api_key = existing.api_key.clone();
    }
    if incoming
        .toml_config
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        incoming.toml_config = existing.toml_config.clone();
    }
    if incoming.wire_api.trim().is_empty() {
        incoming.wire_api = existing.wire_api.clone();
    }
    if incoming.model_mappings.is_empty() {
        incoming.model_mappings = existing.model_mappings.clone();
    }
    // CC Switch can refresh native TOML without knowing the local bridge's
    // format selection. Keep the app metadata unless the import supplies one.
    if incoming.upstream_api.is_none() {
        incoming.upstream_api = existing.upstream_api.clone();
    }
    incoming
}

fn source_matches(row: &StoredProvider, origin: (&str, &str)) -> bool {
    let source_matches = row.source == origin.0
        || (origin.0 == CCSWITCH_PROVIDER_SOURCE && row.source == CCSWITCH_LOCAL_PROVIDER_SOURCE);
    source_matches && row.source_id.as_deref() == Some(origin.1)
}

fn upsert_provider_in_savepoint(
    conn: &Connection,
    mut provider: SavedProvider,
    mode: ProviderUpsertMode,
    origin: Option<(&str, &str)>,
) -> Result<ProviderUpsertResult> {
    let requested_id = provider.id.clone();
    let stored = stored_providers_on_connection(conn)?;
    let source_match = origin.and_then(|origin| {
        stored
            .iter()
            .find(|candidate| source_matches(candidate, origin))
    });
    let exact_id_match = stored
        .iter()
        .find(|candidate| candidate.provider.id == requested_id);

    let target = match mode {
        ProviderUpsertMode::Manual => exact_id_match,
        ProviderUpsertMode::Imported => source_match,
    };
    let kind = if let Some(target) = target {
        let existing = &target.provider;
        provider.id = existing.id.clone();
        if mode == ProviderUpsertMode::Imported {
            provider = merge_authoritative_import(provider, existing);
        }
        ProviderUpsertKind::Updated
    } else {
        if exact_id_match.is_some() {
            provider.id = unique_provider_id_on_connection(conn, &provider.id)?;
        }
        ProviderUpsertKind::Added
    };

    write_provider_with_origin(conn, &provider, origin)?;
    let provider = provider_by_id_on_connection(conn, &provider.id)?
        .ok_or_else(|| CodexxError::Database("provider saved but not found".to_string()))?;
    Ok(ProviderUpsertResult { provider, kind })
}

fn upsert_provider_with_origin(
    conn: &Connection,
    provider: SavedProvider,
    mode: ProviderUpsertMode,
    origin: Option<(&str, &str)>,
) -> Result<ProviderUpsertResult> {
    const SAVEPOINT: &str = "codex_x_provider_upsert";
    conn.execute_batch(&format!("SAVEPOINT {SAVEPOINT}"))
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    match upsert_provider_in_savepoint(conn, provider, mode, origin) {
        Ok(result) => {
            conn.execute_batch(&format!("RELEASE SAVEPOINT {SAVEPOINT}"))
                .map_err(|e| CodexxError::Database(e.to_string()))?;
            Ok(result)
        }
        Err(error) => {
            let rollback = conn.execute_batch(&format!(
                "ROLLBACK TO SAVEPOINT {SAVEPOINT}; RELEASE SAVEPOINT {SAVEPOINT}"
            ));
            match rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(CodexxError::Database(format!(
                    "{error}; provider upsert rollback failed: {rollback_error}"
                ))),
            }
        }
    }
}

pub(crate) fn upsert_provider_on_connection(
    conn: &Connection,
    provider: SavedProvider,
    mode: ProviderUpsertMode,
) -> Result<ProviderUpsertResult> {
    upsert_provider_with_origin(conn, provider, mode, None)
}

pub(crate) fn upsert_ccswitch_provider_on_connection(
    conn: &Connection,
    provider: SavedProvider,
    source_id: &str,
) -> Result<ProviderUpsertResult> {
    let source_id = source_id.trim();
    if source_id.is_empty() {
        return Err(CodexxError::Config(
            "cc-switch 供应商缺少稳定来源 ID".to_string(),
        ));
    }
    upsert_provider_with_origin(
        conn,
        provider,
        ProviderUpsertMode::Imported,
        Some((CCSWITCH_PROVIDER_SOURCE, source_id)),
    )
}

fn is_historical_custom_provider_id(id: &str) -> bool {
    let id = id.trim();
    id == "custom"
        || id.strip_prefix("custom-").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

pub(crate) fn consolidate_legacy_provider_duplicates_on_connection(
    _conn: &Connection,
) -> Result<usize> {
    // Distinct IDs are independent records, even when every configuration
    // field matches. Keep the legacy hook for import-result compatibility.
    Ok(0)
}

fn apply_provider_toml_authority(provider: &mut SavedProvider) -> Result<()> {
    let Some(text) = provider.toml_config.as_deref() else {
        return Ok(());
    };
    let mut doc = text
        .parse::<DocumentMut>()
        .map_err(|error| CodexxError::Config(format!("供应商 TOML 无效: {error}")))?;
    let provider_id = doc
        .get("model_provider")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| CodexxError::Config("供应商 TOML 缺少 model_provider".to_string()))?;

    let table = doc
        .get("model_providers")
        .and_then(|item| item.as_table())
        .and_then(|providers| providers.get(&provider_id))
        .and_then(|item| item.as_table())
        .ok_or_else(|| {
            CodexxError::Config(format!("供应商 TOML 缺少 [model_providers.{provider_id}]"))
        })?;

    let model = doc
        .get("model")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let provider_name = table
        .get("name")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let base_url = table
        .get("base_url")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let wire_api = table
        .get("wire_api")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let requires_openai_auth = table
        .get("requires_openai_auth")
        .and_then(|item| item.as_bool());
    let toml_api_key = experimental_bearer_token_from_doc(&doc, Some(&provider_id));

    if let Some(model) = model {
        provider.model = model;
    }
    if let Some(provider_name) = provider_name {
        provider.provider_name = provider_name;
    }
    if let Some(base_url) = base_url {
        provider.base_url = canonical_provider_base_url(&base_url);
    }
    if let Some(wire_api) = wire_api {
        provider.wire_api = wire_api;
    }
    if let Some(requires_openai_auth) = requires_openai_auth {
        provider.requires_openai_auth = requires_openai_auth;
    }
    if provider.api_key.is_none() {
        provider.api_key = toml_api_key;
    }

    strip_provider_bearer_tokens(&mut doc);
    provider.toml_config = Some(doc.to_string().trim_end().to_string());
    Ok(())
}

fn normalize_stored_provider_from_toml(provider: &mut SavedProvider) {
    // Old releases could persist stale scalar columns beside a newer complete
    // TOML template. Reads must trust a valid template without making one bad
    // legacy row prevent the provider list from loading.
    let _ = apply_provider_toml_authority(provider);
    if normalize_provider_upstream_api(provider).is_ok() && provider.upstream_api.is_some() {
        // Migrate legacy native Chat templates in memory, preserving comments
        // and the independent bridge format instead of putting metadata in TOML.
        if let Some(mut doc) = provider
            .toml_config
            .as_deref()
            .and_then(|text| text.parse::<DocumentMut>().ok())
        {
            if let Ok(id) = selected_provider_id(&doc) {
                if let Some(table) = doc
                    .get_mut("model_providers")
                    .and_then(Item::as_table_mut)
                    .and_then(|providers| providers.get_mut(&id))
                    .and_then(Item::as_table_mut)
                {
                    if table.get("wire_api").and_then(Item::as_str) != Some("responses") {
                        table["wire_api"] = value("responses");
                    }
                    provider.toml_config = Some(doc.to_string().trim_end().to_string());
                }
            }
        }
    }
}

fn normalize_provider_upstream_api(provider: &mut SavedProvider) -> Result<()> {
    if !matches!(
        provider.wire_api.trim(),
        "" | "responses" | "chat" | "chat_completions"
    ) {
        return Err(CodexxError::Config(
            "供应商 wire_api 不受支持；Codex 客户端仅支持 Responses，请通过上游协议选择 Chat Completions、Anthropic Messages 或 Gemini".to_string(),
        ));
    }
    let selected = provider.upstream_api.as_deref().map(str::trim);
    provider.upstream_api =
        match selected {
            Some("responses" | "chat_completions" | "anthropic_messages" | "gemini") => {
                selected.map(ToString::to_string)
            }
            Some(_) => return Err(CodexxError::Config(
                "供应商上游协议必须是 responses、chat_completions、anthropic_messages 或 gemini"
                    .to_string(),
            )),
            None if matches!(provider.wire_api.trim(), "chat" | "chat_completions") => {
                Some("chat_completions".to_string())
            }
            None => None,
        };
    if provider.upstream_api.is_some() {
        provider.wire_api = "responses".to_string();
    }
    Ok(())
}

fn sync_provider_toml_from_fields(provider: &mut SavedProvider) -> Result<()> {
    let Some(text) = provider.toml_config.as_deref() else {
        return Ok(());
    };
    let mut doc = text
        .parse::<DocumentMut>()
        .map_err(|error| CodexxError::Config(format!("供应商 TOML 无效: {error}")))?;
    let provider_id = doc
        .get("model_provider")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| CodexxError::Config("供应商 TOML 缺少 model_provider".to_string()))?;

    if provider.api_key.is_none() {
        provider.api_key = experimental_bearer_token_from_doc(&doc, Some(&provider_id));
    }
    strip_provider_bearer_tokens(&mut doc);
    doc["model"] = value(provider.model.clone());

    let table = doc
        .get_mut("model_providers")
        .and_then(|item| item.as_table_mut())
        .and_then(|providers| providers.get_mut(&provider_id))
        .and_then(|item| item.as_table_mut())
        .ok_or_else(|| {
            CodexxError::Config(format!("供应商 TOML 缺少 [model_providers.{provider_id}]"))
        })?;
    table["name"] = value(provider.provider_name.clone());
    super::transport::configure_third_party_transport(table, &provider.base_url, true);
    table["base_url"] = value(provider.base_url.clone());
    table["wire_api"] = value(provider.wire_api.clone());
    table["requires_openai_auth"] = value(provider.requires_openai_auth);
    provider.toml_config = Some(provider_template_from_document(
        &doc,
        &provider_id,
        &provider.model,
    )?);
    Ok(())
}

pub(crate) fn provider_with_editor_auth(
    mut provider: SavedProvider,
    prefer_toml_auth: bool,
) -> Result<SavedProvider> {
    if !prefer_toml_auth {
        return Ok(provider);
    }
    let Some(text) = provider.toml_config.as_deref() else {
        return Ok(provider);
    };
    let mut doc = text
        .parse::<DocumentMut>()
        .map_err(|error| CodexxError::Config(format!("供应商 TOML 无效: {error}")))?;
    let id = selected_provider_id(&doc)?;
    let env_key = doc
        .get("model_providers")
        .and_then(Item::as_table)
        .and_then(|providers| providers.get(&id))
        .and_then(Item::as_table)
        .and_then(|table| table.get("env_key"));
    let Some(env_key) = env_key else {
        return Ok(provider);
    };
    if env_key.as_str().is_none_or(|name| name.trim().is_empty()) {
        return Err(CodexxError::Config(
            "供应商 env_key 必须是非空的环境变量名称".into(),
        ));
    }
    // A manual TOML edit selects environment-based auth. The form still holds
    // the old saved key; do not let that fallback erase the user's env_key on
    // activation or persist it beside a different credential source.
    provider.api_key = None;
    strip_provider_bearer_tokens(&mut doc);
    provider.toml_config = Some(doc.to_string().trim_end().to_string());
    Ok(provider)
}

pub(crate) fn normalize_saved_provider(provider: SavedProvider) -> Result<SavedProvider> {
    let raw_id = provider.id.trim();
    if raw_id.is_empty() {
        return Err(CodexxError::Config("provider id 不能为空".to_string()));
    }
    let model_mappings =
        super::model_catalog::normalize_mappings(&provider.model_mappings, &provider.model)?;
    let mut normalized = SavedProvider {
        id: custom_provider_id(raw_id),
        provider_name: provider.provider_name.trim().to_string(),
        base_url: canonical_provider_base_url(&provider.base_url),
        model: provider.model.trim().to_string(),
        api_key: provider
            .api_key
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        toml_config: provider
            .toml_config
            .map(|value| value.trim_end().to_string())
            .filter(|value| !value.trim().is_empty()),
        wire_api: if provider.wire_api.trim().is_empty() {
            "responses".to_string()
        } else {
            provider.wire_api.trim().to_string()
        },
        upstream_api: provider.upstream_api,
        requires_openai_auth: provider.requires_openai_auth,
        model_mappings,
    };
    if normalized.provider_name.is_empty() {
        return Err(CodexxError::Config("供应商名称不能为空".to_string()));
    }
    if normalized.base_url.is_empty() {
        return Err(CodexxError::Config("base_url 不能为空".to_string()));
    }
    if normalized.model.is_empty() {
        return Err(CodexxError::Config("model 不能为空".to_string()));
    }
    if is_placeholder_provider(&normalized.provider_name, &normalized.base_url) {
        return Err(CodexxError::Config(
            "供应商名称和 base_url 不能使用示例占位值，请填写实际配置".to_string(),
        ));
    }
    // Imported/read records are hydrated from their complete TOML before they
    // reach this path. For an explicit user edit, the latest form fields win
    // while the full TOML (comments, MCP, projects, desktop settings, etc.) is
    // retained verbatim apart from the standard provider fields.
    normalize_provider_upstream_api(&mut normalized)?;
    sync_provider_toml_from_fields(&mut normalized)?;
    Ok(normalized)
}

pub(crate) fn normalize_saved_provider_for_save(
    conn: &Connection,
    mut provider: SavedProvider,
) -> Result<SavedProvider> {
    let requested_id = provider.id.trim().to_string();
    let existing = provider_by_id_on_connection(conn, &requested_id)?;
    if provider.upstream_api.is_none()
        && !matches!(provider.wire_api.trim(), "chat" | "chat_completions")
    {
        // Older clients omit this new app-only field. Preserve their existing
        // route format; selecting native Responses explicitly uses Some(responses).
        provider.upstream_api = existing
            .as_ref()
            .and_then(|saved| saved.upstream_api.clone());
    }
    let mut normalized = normalize_saved_provider(provider)?;
    if existing.is_some() {
        // Existing IDs are record identities. Preserve legacy reserved IDs such
        // as `custom` instead of treating an edit as a new normalized record.
        normalized.id = requested_id;
    } else if requested_id != normalized.id
        && provider_by_id_on_connection(conn, &normalized.id)?.is_some()
    {
        return Err(CodexxError::Config(format!(
            "供应商 ID {} 规范化后与现有供应商冲突，请更换名称或 ID",
            requested_id
        )));
    }
    Ok(normalized)
}

pub(crate) fn save_manual_provider_on_connection(
    conn: &Connection,
    provider: SavedProvider,
) -> Result<SavedProvider> {
    let provider = normalize_saved_provider_for_save(conn, provider)?;
    Ok(upsert_provider_on_connection(conn, provider, ProviderUpsertMode::Manual)?.provider)
}

pub(crate) fn save_provider_inner(provider: SavedProvider) -> Result<SavedProvider> {
    let conn = open_db()?;
    save_manual_provider_on_connection(&conn, provider)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DuplicateProviderResult {
    pub(crate) provider: SavedProvider,
    pub(crate) providers: Vec<SavedProvider>,
    pub(crate) active_provider_id: Option<String>,
}

pub(crate) fn duplicate_provider_inner(
    config_dir: Option<String>,
    provider_id: Option<String>,
    provider_name: Option<String>,
) -> Result<DuplicateProviderResult> {
    let codex_dir = crate::resolve_codex_dir(config_dir)?;
    let _lock = crate::live_config::acquire_live_config_lock(&codex_dir)?;
    let live = super::live::detected_live_custom_provider(&codex_dir)?;
    duplicate_provider_on_connection(
        &mut open_db()?,
        &codex_dir,
        live,
        provider_id.as_deref(),
        provider_name.as_deref(),
    )
}

fn duplicate_provider_on_connection(
    conn: &mut Connection,
    codex_dir: &std::path::Path,
    live: Option<SavedProvider>,
    provider_id: Option<&str>,
    provider_name: Option<&str>,
) -> Result<DuplicateProviderResult> {
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    let mut saved = list_saved_providers_on_connection(&transaction)?;
    let source = if let Some(id) = provider_id {
        provider_by_id_on_connection(&transaction, id)?
            .ok_or_else(|| CodexxError::Config("供应商已不存在，请刷新列表后重试".to_string()))?
    } else {
        let mut detected = live.clone().ok_or_else(|| {
            CodexxError::Config("当前第三方配置已变更，请刷新列表后重试".to_string())
        })?;
        // Adopt an unsaved detected original before adding its copy. Otherwise
        // the copy would become the only matching row and appear to be enabled.
        let detected_matches = matching_saved_provider_ids_for_live_on_connection(
            &transaction,
            codex_dir,
            &detected,
            &saved,
        )?;
        if let [saved_id] = detected_matches.as_slice() {
            if let Some(original) = saved.iter().find(|provider| &provider.id == saved_id) {
                detected.id = original.id.clone();
                detected.model_mappings = original.model_mappings.clone();
                detected.upstream_api = original.upstream_api.clone();
                if detected.api_key.is_none() {
                    detected.api_key = original.api_key.clone();
                }
            }
        } else if detected_matches.is_empty() {
            detected.id = unique_provider_id_on_connection(
                &transaction,
                &custom_provider_id(&detected.provider_name),
            )?;
            detected = save_manual_provider_on_connection(&transaction, detected)?;
            saved.push(detected.clone());
        }
        detected
    };
    let active_provider_id = if let Some(live) = live.as_ref() {
        super::reconcile_active_provider_on_connection(
            &transaction,
            codex_dir,
            &matching_saved_provider_ids_for_live_on_connection(
                &transaction,
                codex_dir,
                live,
                &saved,
            )?,
        )?
    } else {
        None
    };
    let base_name = provider_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("{} 副本", source.provider_name));
    let mut copy = source.clone();
    copy.id = unique_provider_id_on_connection(&transaction, &format!("{}-copy", source.id))?;
    copy.provider_name = base_name.clone();
    let mut suffix = 2;
    while saved
        .iter()
        .any(|provider| provider.provider_name == copy.provider_name)
    {
        copy.provider_name = format!("{base_name} {suffix}");
        suffix += 1;
    }
    let provider = save_manual_provider_on_connection(&transaction, copy)?;
    let providers = list_saved_providers_on_connection(&transaction)?;
    transaction
        .commit()
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    Ok(DuplicateProviderResult {
        provider,
        providers,
        active_provider_id,
    })
}

pub(crate) fn save_provider_with_rollback_inner(
    provider: SavedProvider,
) -> Result<(SavedProvider, ProviderStoreRollback)> {
    let mut conn = open_db()?;
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    let before = stored_providers_on_connection(&transaction)?;
    let saved = save_manual_provider_on_connection(&transaction, provider)?;
    let after = stored_providers_on_connection(&transaction)?;
    transaction
        .commit()
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    Ok((saved, ProviderStoreRollback { before, after }))
}

pub(crate) fn save_detected_provider_with_rollback_inner(
    codex_dir: &std::path::Path,
    mut live: SavedProvider,
) -> Result<Option<ProviderStoreRollback>> {
    let mut conn = open_db()?;
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    let before = stored_providers_on_connection(&transaction)?;
    let saved = before
        .iter()
        .map(|stored| stored.provider.clone())
        .collect::<Vec<_>>();
    let matches =
        matching_saved_provider_ids_for_live_on_connection(&transaction, codex_dir, &live, &saved)?;
    match matches.as_slice() {
        [saved_id] => {
            if let Some(saved) = saved.iter().find(|provider| &provider.id == saved_id) {
                live.model_mappings = saved.model_mappings.clone();
                live.upstream_api = saved.upstream_api.clone();
            }
            if live.api_key.is_none() {
                live.api_key = saved
                    .iter()
                    .find(|provider| &provider.id == saved_id)
                    .and_then(|provider| provider.api_key.clone());
            }
            live.id = saved_id.clone();
        }
        [] => {
            // A detected live route is only a temporary UI row until a write
            // action adopts it. Preserve it before replacing config/auth, and
            // allocate its ID under the same transaction to avoid overwriting
            // an unrelated manual profile or a concurrent adoption.
            live.id = unique_provider_id_on_connection(
                &transaction,
                &custom_provider_id(&live.provider_name),
            )?;
        }
        _ => return Ok(None),
    }
    save_manual_provider_on_connection(&transaction, live)?;
    let after = stored_providers_on_connection(&transaction)?;
    transaction
        .commit()
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    Ok(Some(ProviderStoreRollback { before, after }))
}

fn insert_stored_provider(conn: &Connection, stored: &StoredProvider) -> Result<()> {
    let provider = &stored.provider;
    let model_mappings_json = serde_json::to_string(&provider.model_mappings)
        .map_err(|error| CodexxError::Config(format!("序列化模型映射失败: {error}")))?;
    conn.execute(
        "INSERT INTO providers
            (id, provider_name, base_url, model, api_key, toml_config, wire_api,
             requires_openai_auth, model_mappings_json, upstream_api, source, source_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            provider.id,
            provider.provider_name,
            provider.base_url,
            provider.model,
            provider.api_key,
            provider.toml_config,
            provider.wire_api,
            if provider.requires_openai_auth { 1 } else { 0 },
            model_mappings_json,
            provider.upstream_api,
            stored.source,
            stored.source_id,
            stored.created_at,
            stored.updated_at,
        ],
    )
    .map_err(|error| CodexxError::Database(error.to_string()))?;
    Ok(())
}

pub(crate) fn rollback_provider_store_inner(rollback: ProviderStoreRollback) -> Result<()> {
    let mut conn = open_db()?;
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    let current = stored_providers_on_connection(&transaction)?;
    let ids = rollback
        .before
        .iter()
        .chain(&rollback.after)
        .map(|stored| stored.provider.id.clone())
        .collect::<HashSet<_>>();
    let changed_ids = ids
        .into_iter()
        .filter(|id| {
            rollback
                .before
                .iter()
                .find(|stored| stored.provider.id == *id)
                != rollback
                    .after
                    .iter()
                    .find(|stored| stored.provider.id == *id)
        })
        .collect::<Vec<_>>();
    for id in &changed_ids {
        let actual = current.iter().find(|stored| stored.provider.id == *id);
        let expected = rollback
            .after
            .iter()
            .find(|stored| stored.provider.id == *id);
        if actual != expected {
            return Err(CodexxError::Database(format!(
                "供应商 {id} 已被其他操作修改，拒绝覆盖并发变更"
            )));
        }
    }
    for id in &changed_ids {
        transaction
            .execute("DELETE FROM providers WHERE id = ?1", [id])
            .map_err(|error| CodexxError::Database(error.to_string()))?;
    }
    for stored in rollback
        .before
        .iter()
        .filter(|stored| changed_ids.contains(&stored.provider.id))
    {
        insert_stored_provider(&transaction, stored)?;
    }
    transaction
        .commit()
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    Ok(())
}

pub(crate) fn delete_provider_inner(id: &str) -> Result<()> {
    let mut conn = open_db()?;
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    clear_provider_selections_on_connection(&transaction, id)?;
    transaction
        .execute("DELETE FROM providers WHERE id = ?1", params![id])
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    transaction
        .commit()
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    Ok(())
}

pub(crate) fn reserved_codex_provider_id(id: &str) -> bool {
    matches!(
        id.trim().to_ascii_lowercase().as_str(),
        "openai" | "custom" | "amazon-bedrock" | "ollama" | "lmstudio" | "oss"
    )
}

pub(crate) fn custom_provider_id(input: &str) -> String {
    let id = sanitize_id(input);
    if reserved_codex_provider_id(&id) {
        format!("{id}-custom")
    } else {
        id
    }
}

pub(crate) fn experimental_bearer_token_from_doc(
    doc: &DocumentMut,
    provider_id: Option<&str>,
) -> Option<String> {
    let token_from_table = provider_id.and_then(|id| {
        doc.get("model_providers")
            .and_then(|item| item.as_table())
            .and_then(|providers| providers.get(id))
            .and_then(|item| item.as_table())
            .and_then(|table| table.get("experimental_bearer_token"))
            .and_then(|item| item.as_str())
    });

    token_from_table
        .or_else(|| {
            doc.get("experimental_bearer_token")
                .and_then(|item| item.as_str())
        })
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER_CONFIG: &str = r#"# keep the shared configuration
model_provider = "custom"
model = "gpt-test"

[model_providers.custom]
name = "Header provider"
base_url = "https://header.example.test/v1"
wire_api = "responses"
http_headers = { "X-Title" = "Codex-X" }
env_http_headers = { "X-Project" = "PROJECT_ID" }

[model_providers.other.http_headers]
X-Other = "keep-other-provider"

[mcp_servers.keep]
command = "fixture-server"
"#;

    fn header(name: &str, text: &str, source: ProviderHeaderSource) -> ProviderHeader {
        ProviderHeader {
            name: name.to_string(),
            value: text.to_string(),
            source,
        }
    }

    #[test]
    fn headers_read_patch_and_remove_only_touch_the_selected_provider() {
        let rows = read_provider_headers_inner(HEADER_CONFIG.to_string()).unwrap();
        assert_eq!(
            rows,
            vec![
                header("X-Title", "Codex-X", ProviderHeaderSource::Static),
                header("X-Project", "PROJECT_ID", ProviderHeaderSource::Env),
            ]
        );
        let next = update_provider_headers_inner(
            HEADER_CONFIG.to_string(),
            vec![
                header(
                    " User-Agent ",
                    "  custom agent  ",
                    ProviderHeaderSource::Static,
                ),
                header("X-Project", "PROJECT_ID", ProviderHeaderSource::Env),
            ],
        )
        .unwrap();
        let doc = next.parse::<DocumentMut>().unwrap();
        assert!(next.contains("# keep the shared configuration"));
        assert_eq!(
            doc["mcp_servers"]["keep"]["command"].as_str(),
            Some("fixture-server")
        );
        assert_eq!(
            doc["model_providers"]["other"]["http_headers"]["X-Other"].as_str(),
            Some("keep-other-provider")
        );
        assert_eq!(
            doc["model_providers"]["custom"]["http_headers"]["User-Agent"].as_str(),
            Some("  custom agent  ")
        );
        assert!(doc["model_providers"]["custom"]["http_headers"]
            .as_table_like()
            .unwrap()
            .get("X-Title")
            .is_none());
        let removed = update_provider_headers_inner(next, Vec::new()).unwrap();
        assert!(read_provider_headers_inner(removed.clone())
            .unwrap()
            .is_empty());
        let removed = removed.parse::<DocumentMut>().unwrap();
        assert!(removed["model_providers"]["custom"]
            .as_table()
            .unwrap()
            .get("http_headers")
            .is_none());
        assert!(removed["model_providers"]["custom"]
            .as_table()
            .unwrap()
            .get("env_http_headers")
            .is_none());
    }

    #[test]
    fn headers_patch_accepts_table_syntax_and_preserves_unchanged_comments() {
        let text = HEADER_CONFIG.replace("http_headers = { \"X-Title\" = \"Codex-X\" }\nenv_http_headers = { \"X-Project\" = \"PROJECT_ID\" }", "[model_providers.custom.http_headers]\n# keep header comment\nX-Title = \"Codex-X\" # keep value comment\n[model_providers.custom.env_http_headers]\nX-Project = \"PROJECT_ID\"");
        let rows = read_provider_headers_inner(text.clone()).unwrap();
        let patched = update_provider_headers_inner(text.clone(), rows).unwrap();
        assert_eq!(patched, text.trim_end());
    }

    #[test]
    fn headers_reject_duplicate_names_and_injection_without_echoing_values() {
        for rows in [
            vec![
                header("X-Test", "secret", ProviderHeaderSource::Static),
                header("x-test", "PROJECT_ID", ProviderHeaderSource::Env),
            ],
            vec![header("bad:name", "secret", ProviderHeaderSource::Static)],
            vec![header(
                "X-Test",
                "secret\r\nInjected: yes",
                ProviderHeaderSource::Static,
            )],
            vec![header("X-Test", "secret\0", ProviderHeaderSource::Static)],
            vec![header(
                "X-Test",
                "PROJECT=secret",
                ProviderHeaderSource::Env,
            )],
        ] {
            let error = update_provider_headers_inner(HEADER_CONFIG.to_string(), rows)
                .unwrap_err()
                .to_string();
            assert!(!error.contains("secret"));
            assert!(error.contains("供应商 Header"));
        }
    }

    #[test]
    fn invalid_legacy_headers_can_be_read_and_repaired_but_cannot_be_saved() {
        let text = HEADER_CONFIG.replace("\"Codex-X\"", "\"secret\\r\\ninjection\"");
        let rows = read_provider_headers_inner(text.clone()).unwrap();
        assert_eq!(rows[0].value, "secret\r\ninjection");
        let doc = text.parse::<DocumentMut>().unwrap();
        assert!(provider_template_from_document(&doc, "custom", "gpt-test").is_err());
        let repaired = update_provider_headers_inner(
            text,
            vec![header("X-Title", "safe", ProviderHeaderSource::Static)],
        )
        .unwrap();
        assert!(provider_template_from_document(
            &repaired.parse::<DocumentMut>().unwrap(),
            "custom",
            "gpt-test"
        )
        .is_ok());
    }

    #[test]
    fn headers_reject_malformed_tables_and_non_text_values() {
        for replacement in ["http_headers = 4", "http_headers = { X-Test = 4 }"] {
            let text =
                HEADER_CONFIG.replace("http_headers = { \"X-Title\" = \"Codex-X\" }", replacement);
            assert!(read_provider_headers_inner(text).is_err());
        }
    }

    fn test_connection() -> Connection {
        let conn = Connection::open_in_memory().expect("open provider test database");
        conn.execute_batch(
            "CREATE TABLE providers (
                id TEXT PRIMARY KEY, provider_name TEXT NOT NULL, base_url TEXT NOT NULL,
                model TEXT NOT NULL, api_key TEXT, toml_config TEXT,
                wire_api TEXT NOT NULL DEFAULT 'responses',
                requires_openai_auth INTEGER NOT NULL DEFAULT 1,
                model_mappings_json TEXT NOT NULL DEFAULT '[]',
                upstream_api TEXT,
                source TEXT NOT NULL DEFAULT 'manual',
                source_id TEXT,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL);",
        )
        .expect("create providers table");
        conn
    }

    fn provider(id: &str, name: &str, api_key: Option<&str>) -> SavedProvider {
        SavedProvider {
            id: id.to_string(),
            provider_name: name.to_string(),
            base_url: "https://example.com/v1".to_string(),
            model: "gpt-5.5".to_string(),
            api_key: api_key.map(ToString::to_string),
            toml_config: None,
            wire_api: "responses".to_string(),
            requires_openai_auth: true,
            upstream_api: None,
            model_mappings: Vec::new(),
        }
    }

    #[test]
    fn bridge_protocol_metadata_defaults_for_old_payloads_and_validates_on_save() {
        let old = serde_json::json!({
            "id":"old", "providerName":"Old", "baseUrl":"https://upstream.example.test/v1",
            "model":"gpt-test", "apiKey":null, "tomlConfig":null,
            "wireApi":"responses", "requiresOpenaiAuth":false,
        });
        let parsed = serde_json::from_value::<SavedProvider>(old).unwrap();
        assert!(parsed.upstream_api.is_none());
        for protocol in [
            "responses",
            "chat_completions",
            "anthropic_messages",
            "gemini",
        ] {
            let mut selected = provider("bridge", "Bridge", None);
            selected.upstream_api = Some(format!(" {protocol} "));
            selected.toml_config = Some(HEADER_CONFIG.to_string());
            let normalized = normalize_saved_provider(selected).unwrap();
            assert_eq!(normalized.upstream_api.as_deref(), Some(protocol));
            assert_eq!(normalized.wire_api, "responses");
            let doc = normalized
                .toml_config
                .unwrap()
                .parse::<DocumentMut>()
                .unwrap();
            assert_eq!(
                doc["model_providers"]["custom"]["wire_api"].as_str(),
                Some("responses")
            );
            assert!(doc["model_providers"]["custom"]
                .as_table()
                .unwrap()
                .get("upstream_api")
                .is_none());
        }
        let mut invalid = provider("bad-protocol", "Bad protocol", None);
        invalid.upstream_api = Some("unknown".into());
        assert!(normalize_saved_provider(invalid)
            .unwrap_err()
            .to_string()
            .contains("上游协议"));
    }

    #[test]
    fn legacy_chat_wire_api_migrates_to_independent_bridge_metadata() {
        for legacy in ["chat", "chat_completions"] {
            let conn = test_connection();
            let mut old = provider("legacy-chat", "Legacy chat", None);
            old.toml_config = Some(HEADER_CONFIG.replace(
                "wire_api = \"responses\"",
                &format!("wire_api = \"{legacy}\""),
            ));
            // The TOML remains authoritative when old DB scalar columns disagree.
            write_provider_on_connection(&conn, &old).unwrap();
            let migrated = provider_by_id_on_connection(&conn, &old.id)
                .unwrap()
                .unwrap();
            assert_eq!(migrated.wire_api, "responses");
            assert_eq!(migrated.upstream_api.as_deref(), Some("chat_completions"));
            let text = migrated.toml_config.as_ref().unwrap();
            assert!(text.contains("# keep the shared configuration"));
            assert_eq!(
                text.parse::<DocumentMut>().unwrap()["model_providers"]["custom"]["wire_api"]
                    .as_str(),
                Some("responses")
            );
            let saved = save_manual_provider_on_connection(&conn, migrated).unwrap();
            assert_eq!(saved.upstream_api.as_deref(), Some("chat_completions"));
            assert_eq!(
                conn.query_row(
                    "SELECT wire_api FROM providers WHERE id=?1",
                    [&saved.id],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
                "responses"
            );
        }
    }

    #[test]
    fn unknown_legacy_wire_api_remains_readable_and_is_rejected_on_save() {
        let conn = test_connection();
        let mut unknown = provider("unknown-wire", "Unknown wire", None);
        unknown.wire_api = "unrecognized-wire-format".into();
        write_provider_on_connection(&conn, &unknown).unwrap();
        let read = provider_by_id_on_connection(&conn, &unknown.id)
            .unwrap()
            .unwrap();
        assert_eq!(read.wire_api, "unrecognized-wire-format");
        assert!(read.upstream_api.is_none());
        assert!(save_manual_provider_on_connection(&conn, read)
            .unwrap_err()
            .to_string()
            .contains("wire_api 不受支持"));
    }

    #[test]
    fn edits_from_older_clients_keep_bridge_metadata_until_explicitly_reset() {
        let conn = test_connection();
        let mut configured = provider("configured", "Configured", None);
        configured.upstream_api = Some("anthropic_messages".into());
        let saved = save_manual_provider_on_connection(&conn, configured).unwrap();
        let mut older_edit = saved.clone();
        older_edit.upstream_api = None;
        older_edit.provider_name = "Renamed".into();
        let preserved = save_manual_provider_on_connection(&conn, older_edit).unwrap();
        assert_eq!(preserved.upstream_api, saved.upstream_api);
        let mut native = preserved;
        native.upstream_api = Some("responses".into());
        let reset = save_manual_provider_on_connection(&conn, native).unwrap();
        assert_eq!(reset.upstream_api.as_deref(), Some("responses"));
    }

    #[test]
    fn upstream_metadata_and_headers_survive_store_copy_import_refresh_and_snapshot_restore() {
        let mut conn = test_connection();
        let mut original = provider("gemini-bridge", "Gemini bridge", Some("fixture-key"));
        original.upstream_api = Some("gemini".into());
        original.toml_config = Some(HEADER_CONFIG.to_string());
        let original = normalize_saved_provider(original).unwrap();
        let saved =
            upsert_ccswitch_provider_on_connection(&conn, original.clone(), "ccswitch-fixture")
                .unwrap()
                .provider;
        assert_eq!(saved.upstream_api.as_deref(), Some("gemini"));
        let copied = duplicate_provider_on_connection(
            &mut conn,
            std::path::Path::new("/fixture/upstream-copy"),
            None,
            Some(&saved.id),
            None,
        )
        .unwrap()
        .provider;
        assert_eq!(copied.upstream_api, saved.upstream_api);
        assert_eq!(
            read_provider_headers_inner(copied.toml_config.unwrap()).unwrap(),
            read_provider_headers_inner(saved.toml_config.clone().unwrap()).unwrap()
        );
        let mut incoming = original;
        incoming.upstream_api = None;
        let refreshed = upsert_ccswitch_provider_on_connection(&conn, incoming, "ccswitch-fixture")
            .unwrap()
            .provider;
        assert_eq!(refreshed.upstream_api.as_deref(), Some("gemini"));
        let snapshots = stored_providers_on_connection(&conn).unwrap();
        let restored = test_connection();
        for snapshot in &snapshots {
            insert_stored_provider(&restored, snapshot).unwrap();
        }
        assert_eq!(
            stored_providers_on_connection(&restored).unwrap(),
            snapshots
        );
    }

    fn provider_count(conn: &Connection) -> usize {
        list_saved_providers_on_connection(conn).unwrap().len()
    }

    fn copy_test_connection() -> Connection {
        let conn = test_connection();
        conn.execute_batch("CREATE TABLE active_provider_selections (codex_dir TEXT PRIMARY KEY, provider_id TEXT NOT NULL, updated_at TEXT NOT NULL);").unwrap();
        conn
    }

    fn provider_order_test_connection() -> Connection {
        let conn = test_connection();
        conn.execute_batch(
            "CREATE TABLE provider_card_orders (
                codex_dir TEXT PRIMARY KEY, order_json TEXT NOT NULL, updated_at TEXT NOT NULL);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn provider_card_order_persists_across_connections_and_keeps_config_scopes_independent() {
        let database_file = tempfile::NamedTempFile::new().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let other_directory = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(database_file.path()).unwrap();
        conn.execute_batch(
            "CREATE TABLE provider_card_orders (
                codex_dir TEXT PRIMARY KEY, order_json TEXT NOT NULL, updated_at TEXT NOT NULL);",
        )
        .unwrap();
        assert!(get_provider_order_on_connection(&conn, directory.path())
            .unwrap()
            .is_empty());
        let chosen = vec![
            "local:shared-id".to_string(),
            "official:shared-id".to_string(),
            "detected:detected-自定义".to_string(),
            "official:openai-official".to_string(),
        ];
        assert_eq!(
            save_provider_order_on_connection(&mut conn, directory.path(), chosen.clone()).unwrap(),
            chosen
        );
        let second = vec!["official:another-account".to_string()];
        save_provider_order_on_connection(&mut conn, other_directory.path(), second.clone())
            .unwrap();
        drop(conn);
        let reopened = Connection::open(database_file.path()).unwrap();
        assert_eq!(
            get_provider_order_on_connection(&reopened, directory.path()).unwrap(),
            chosen
        );
        assert_eq!(
            get_provider_order_on_connection(&reopened, other_directory.path()).unwrap(),
            second
        );
    }

    #[cfg(unix)]
    #[test]
    fn provider_card_order_resolves_linked_config_directory_to_the_same_scope() {
        let directory = tempfile::tempdir().unwrap();
        let alias_root = tempfile::tempdir().unwrap();
        let alias = alias_root.path().join("linked-config");
        std::os::unix::fs::symlink(directory.path(), &alias).unwrap();
        let mut conn = provider_order_test_connection();
        let order = vec!["local:linked-provider".to_string()];
        save_provider_order_on_connection(&mut conn, &alias, order.clone()).unwrap();
        assert_eq!(
            get_provider_order_on_connection(&conn, directory.path()).unwrap(),
            order
        );
    }

    #[test]
    fn provider_card_order_survives_provider_refresh_edits_additions_and_deletions() {
        let mut conn = provider_order_test_connection();
        let directory = std::path::Path::new("/fixture/provider-order-lifecycle");
        let original = provider("ordered-provider", "Imported", Some("fixture-key"));
        upsert_ccswitch_provider_on_connection(&conn, original.clone(), "ordered-source").unwrap();
        let order = vec![
            "local:ordered-provider".to_string(),
            "official:openai-official".to_string(),
            "detected:detected-temporary".to_string(),
        ];
        save_provider_order_on_connection(&mut conn, directory, order.clone()).unwrap();
        assert_eq!(
            provider_by_id_on_connection(&conn, &original.id).unwrap(),
            Some(original)
        );

        let fresh = provider("external-id", "Refreshed import", Some("fixture-key"));
        let refreshed = upsert_ccswitch_provider_on_connection(&conn, fresh, "ordered-source")
            .unwrap()
            .provider;
        assert_eq!(refreshed.id, "ordered-provider");
        let mut edited = refreshed;
        edited.model = "other-model".to_string();
        save_manual_provider_on_connection(&conn, edited).unwrap();
        save_manual_provider_on_connection(
            &conn,
            provider("new-provider", "New provider", Some("new-fixture-key")),
        )
        .unwrap();
        conn.execute("DELETE FROM providers WHERE id = 'ordered-provider'", [])
            .unwrap();
        assert_eq!(
            get_provider_order_on_connection(&conn, directory).unwrap(),
            order
        );
        save_provider_order_on_connection(&mut conn, directory, Vec::new()).unwrap();
        assert!(get_provider_order_on_connection(&conn, directory)
            .unwrap()
            .is_empty());
        assert!(provider_by_id_on_connection(&conn, "new-provider")
            .unwrap()
            .is_some());
    }

    #[test]
    fn provider_card_order_rejects_invalid_or_duplicate_keys_without_overwriting_saved_order() {
        let mut conn = provider_order_test_connection();
        let directory = std::path::Path::new("/fixture/provider-order-validation");
        let order = vec!["local:kept-provider".to_string()];
        save_provider_order_on_connection(&mut conn, directory, order.clone()).unwrap();
        for invalid in [
            vec![
                "local:kept-provider".to_string(),
                "local:kept-provider".to_string(),
            ],
            vec!["unknown:provider".to_string()],
            vec!["local:".to_string()],
            vec!["official: account".to_string()],
            vec!["detected:invalid\nprovider".to_string()],
            vec![format!("local:{}", "x".repeat(512))],
            (0..4097).map(|id| format!("local:{id}")).collect(),
        ] {
            assert!(save_provider_order_on_connection(&mut conn, directory, invalid).is_err());
            assert_eq!(
                get_provider_order_on_connection(&conn, directory).unwrap(),
                order
            );
        }
    }

    #[test]
    fn provider_card_order_database_failure_rolls_back_the_entire_order() {
        let mut conn = provider_order_test_connection();
        let directory = std::path::Path::new("/fixture/provider-order-rollback");
        let before = vec![
            "official:openai-official".to_string(),
            "local:kept".to_string(),
        ];
        save_provider_order_on_connection(&mut conn, directory, before.clone()).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_provider_card_order BEFORE UPDATE ON provider_card_orders
             BEGIN SELECT RAISE(ABORT, 'fixture order persistence failure'); END;",
        )
        .unwrap();
        assert!(save_provider_order_on_connection(
            &mut conn,
            directory,
            vec![
                "local:kept".to_string(),
                "official:openai-official".to_string()
            ],
        )
        .is_err());
        assert_eq!(
            get_provider_order_on_connection(&conn, directory).unwrap(),
            before
        );
        assert!(
            conn.is_autocommit(),
            "failed save must release its transaction"
        );
    }

    #[test]
    fn model_mappings_roundtrip_and_copy_preserve_independent_records() {
        let mut conn = copy_test_connection();
        let mut original = provider("mapping-original", "Original", Some("fixture-key"));
        original.model_mappings = vec![
            super::super::model_catalog::ProviderModelMapping {
                model: " gpt-5.5 ".into(),
                display_name: "GPT".into(),
                context_window: None,
            },
            super::super::model_catalog::ProviderModelMapping {
                model: "deepseek-chat".into(),
                display_name: "DeepSeek".into(),
                context_window: Some(128000),
            },
        ];
        let original = save_manual_provider_on_connection(&conn, original).unwrap();
        assert_eq!(original.model_mappings[0].model, "gpt-5.5");
        assert_eq!(
            provider_by_id_on_connection(&conn, &original.id)
                .unwrap()
                .unwrap()
                .model_mappings,
            original.model_mappings
        );
        let copied = duplicate_provider_on_connection(
            &mut conn,
            std::path::Path::new("/fixture/model-copy"),
            Some(original.clone()),
            Some(&original.id),
            None,
        )
        .unwrap();
        assert_ne!(copied.provider.id, original.id);
        assert_eq!(copied.provider.model_mappings, original.model_mappings);
        let mut detected = original.clone();
        detected.id = "custom".into();
        detected.model = "deepseek-chat".into();
        detected.model_mappings.clear();
        let detected_copy = duplicate_provider_on_connection(
            &mut conn,
            std::path::Path::new("/fixture/model-copy"),
            Some(detected),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            detected_copy.provider.model_mappings,
            original.model_mappings
        );
        let mut edited = copied.provider;
        edited.model_mappings[1].display_name = "Independent copy".into();
        save_manual_provider_on_connection(&conn, edited).unwrap();
        assert_eq!(
            provider_by_id_on_connection(&conn, &original.id)
                .unwrap()
                .unwrap()
                .model_mappings,
            original.model_mappings
        );
        let serialized = serde_json::to_value(&original).unwrap();
        assert_eq!(serialized["modelMappings"][1]["contextWindow"], 128000);
        let mut old_payload = serialized;
        old_payload.as_object_mut().unwrap().remove("modelMappings");
        assert!(serde_json::from_value::<SavedProvider>(old_payload)
            .unwrap()
            .model_mappings
            .is_empty());
    }

    #[test]
    fn cc_switch_refresh_preserves_locally_saved_model_mappings() {
        let conn = copy_test_connection();
        let mut original = provider("mapping-import", "Imported", Some("fixture-key"));
        original.model_mappings = vec![super::super::model_catalog::ProviderModelMapping {
            model: "gpt-5.5".into(),
            display_name: "Local label".into(),
            context_window: Some(256000),
        }];
        upsert_ccswitch_provider_on_connection(&conn, original.clone(), "mapping-source").unwrap();
        let incoming = provider("external-id", "Fresh import", Some("fixture-key"));
        let refreshed =
            upsert_ccswitch_provider_on_connection(&conn, incoming, "mapping-source").unwrap();
        assert_eq!(refreshed.provider.id, original.id);
        assert_eq!(refreshed.provider.provider_name, "Fresh import");
        assert_eq!(refreshed.provider.model_mappings, original.model_mappings);
    }

    #[test]
    fn model_mapping_rollback_restores_mappings_and_origin_metadata() {
        let _guard = crate::app_db::test_db_guard();
        let mut original = provider("mapping-rollback-record", "Original", Some("fixture-key"));
        original.model_mappings = vec![super::super::model_catalog::ProviderModelMapping {
            model: "gpt-5.5".into(),
            display_name: "Original model".into(),
            context_window: Some(128000),
        }];
        let conn = open_db().unwrap();
        upsert_ccswitch_provider_on_connection(&conn, original.clone(), "mapping-rollback-source")
            .unwrap();
        let metadata = |conn: &Connection| {
            conn.query_row(
            "SELECT source, source_id, created_at, updated_at FROM providers WHERE id = 'mapping-rollback-record'", [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
        ).unwrap()
        };
        let before = metadata(&conn);
        let mut changed = original.clone();
        changed.model_mappings[0].display_name = "Changed".into();
        changed.model_mappings[0].context_window = Some(1000000);
        let (_, rollback) = save_provider_with_rollback_inner(changed).unwrap();
        rollback_provider_store_inner(rollback).unwrap();
        assert_eq!(
            provider_by_id_on_connection(&conn, &original.id)
                .unwrap()
                .unwrap()
                .model_mappings,
            original.model_mappings
        );
        assert_eq!(metadata(&conn), before);
        drop(conn);
        delete_provider_inner(&original.id).unwrap();
    }

    #[test]
    fn immediate_copy_preserves_original_selection_and_complete_template() {
        let mut conn = copy_test_connection();
        let dir = std::path::Path::new("/fixture/copy-selection");
        let mut original = provider("original", "Original", Some("fixture-key"));
        original.toml_config = Some("model = \"gpt-5.5\"\nmodel_provider = \"custom\"\nmodel_context_window = 1000000\n[model_providers.custom]\nname = \"Original\"\nbase_url = \"https://example.com/v1\"\nwire_api = \"responses\"\n[mcp_servers.keep]\ncommand = \"fixture\"\n".to_string());
        let original = save_manual_provider_on_connection(&conn, original).unwrap();
        let first = duplicate_provider_on_connection(
            &mut conn,
            dir,
            Some(original.clone()),
            Some(&original.id),
            None,
        )
        .unwrap();
        assert_eq!(
            first.active_provider_id.as_deref(),
            Some(original.id.as_str())
        );
        assert_ne!(first.provider.id, original.id);
        assert_eq!(first.provider.api_key, original.api_key);
        assert_eq!(first.provider.provider_name, "Original 副本");
        let config = first
            .provider
            .toml_config
            .as_ref()
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(config["model_context_window"].as_integer(), Some(1_000_000));
        assert_eq!(
            config["mcp_servers"]["keep"]["command"].as_str(),
            Some("fixture")
        );
        assert_eq!(
            provider_by_id_on_connection(&conn, &original.id)
                .unwrap()
                .unwrap(),
            original
        );
        let second = duplicate_provider_on_connection(
            &mut conn,
            dir,
            Some(original.clone()),
            Some(&original.id),
            None,
        )
        .unwrap();
        assert_eq!(second.provider.provider_name, "Original 副本 2");
        assert_eq!(
            second.active_provider_id.as_deref(),
            Some(original.id.as_str())
        );
        assert_ne!(second.provider.id, first.provider.id);
    }

    #[test]
    fn immediate_copy_adopts_detected_original_without_enabling_the_copy() {
        let mut conn = copy_test_connection();
        let dir = std::path::Path::new("/fixture/copy-detected");
        let live = provider("custom", "Detected", Some("fixture-key"));
        let result =
            duplicate_provider_on_connection(&mut conn, dir, Some(live.clone()), None, None)
                .unwrap();
        assert_eq!(result.providers.len(), 2);
        let original = result
            .providers
            .iter()
            .find(|provider| provider.id != result.provider.id)
            .unwrap();
        assert_eq!(original.provider_name, live.provider_name);
        assert_eq!(
            result.active_provider_id.as_deref(),
            Some(original.id.as_str())
        );
        assert_ne!(
            result.active_provider_id.as_deref(),
            Some(result.provider.id.as_str())
        );
    }

    #[test]
    fn failed_immediate_copy_rolls_back_detected_adoption_and_selection() {
        let mut conn = copy_test_connection();
        conn.execute_batch("CREATE TRIGGER fail_copy BEFORE INSERT ON providers WHEN NEW.provider_name LIKE '%副本%' BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;").unwrap();
        let result = duplicate_provider_on_connection(
            &mut conn,
            std::path::Path::new("/fixture/copy-failure"),
            Some(provider("custom", "Detected", Some("fixture-key"))),
            None,
            None,
        );
        assert!(result.is_err());
        assert_eq!(provider_count(&conn), 0);
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM active_provider_selections",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn provider_upsert_uses_record_ids_and_stable_import_sources() {
        let stable = test_connection();
        let mut existing = provider("cc-stable-id", "Old name", Some("sk-old"));
        existing.toml_config = Some("model = \"locally-preserved\"".to_string());
        upsert_ccswitch_provider_on_connection(&stable, existing, "source-row").unwrap();
        let mut imported = provider("cc-stable-id", "Current CCS name", Some("sk-new"));
        imported.model = "gpt-5.6".to_string();
        let updated =
            upsert_ccswitch_provider_on_connection(&stable, imported.clone(), "source-row")
                .unwrap();
        assert_eq!(updated.kind, ProviderUpsertKind::Updated);
        assert_eq!(updated.provider.api_key.as_deref(), Some("sk-new"));
        assert_eq!(updated.provider.provider_name, "Current CCS name");
        assert_eq!(updated.provider.model, "gpt-5.6");
        assert!(updated.provider.toml_config.is_some());
        upsert_ccswitch_provider_on_connection(&stable, imported, "source-row").unwrap();
        assert_eq!(provider_count(&stable), 1);

        let mut moved = provider("cc-stable-id", "Moved CCS provider", Some("sk-moved"));
        moved.base_url = "https://moved.example.com/v1".to_string();
        let moved = upsert_ccswitch_provider_on_connection(&stable, moved, "source-row")
            .expect("update stable CCS source after endpoint change");
        assert_eq!(moved.kind, ProviderUpsertKind::Updated);
        assert_eq!(moved.provider.base_url, "https://moved.example.com/v1");
        assert_eq!(moved.provider.api_key.as_deref(), Some("sk-moved"));
        assert_eq!(provider_count(&stable), 1);

        let id_collision = test_connection();
        let manual = provider("shared-id", "Manual", Some("sk-manual"));
        upsert_provider_on_connection(&id_collision, manual, ProviderUpsertMode::Manual).unwrap();
        let mut external = provider("shared-id", "Imported", Some("sk-imported"));
        external.base_url = "https://imported.example.com/v1".to_string();
        let imported = upsert_ccswitch_provider_on_connection(&id_collision, external, "shared-id")
            .expect("import id collision without overwriting manual record");
        assert_eq!(imported.kind, ProviderUpsertKind::Added);
        assert_eq!(imported.provider.id, "shared-id-2");
        assert_eq!(
            provider_by_id_on_connection(&id_collision, "shared-id")
                .unwrap()
                .unwrap()
                .api_key
                .as_deref(),
            Some("sk-manual")
        );
        let mut changed = provider("shared-id", "Imported changed", Some("sk-next"));
        changed.base_url = "https://next.example.com/v1".to_string();
        let changed = upsert_ccswitch_provider_on_connection(&id_collision, changed, "shared-id")
            .expect("update imported row by source identity");
        assert_eq!(changed.provider.id, "shared-id-2");
        assert_eq!(changed.provider.base_url, "https://next.example.com/v1");
        assert_eq!(provider_count(&id_collision), 2);

        let compatible = test_connection();
        upsert_provider_on_connection(
            &compatible,
            provider("local", "Same API", None),
            ProviderUpsertMode::Manual,
        )
        .unwrap();
        let added = upsert_ccswitch_provider_on_connection(
            &compatible,
            provider("cc-import", " same   api ", Some("sk-imported")),
            "compatible-row",
        )
        .unwrap();
        assert_eq!(added.kind, ProviderUpsertKind::Added);
        assert_eq!(added.provider.id, "cc-import");
        assert_eq!(added.provider.api_key.as_deref(), Some("sk-imported"));
        assert_eq!(provider_count(&compatible), 2);

        let distinct = test_connection();
        upsert_provider_on_connection(
            &distinct,
            provider("local", "Same API", Some("sk-first")),
            ProviderUpsertMode::Manual,
        )
        .unwrap();
        let added = upsert_ccswitch_provider_on_connection(
            &distinct,
            provider("cc-import", "Same API", Some("sk-second")),
            "distinct-row",
        )
        .unwrap();
        assert_eq!(added.kind, ProviderUpsertKind::Added);
        assert_eq!(provider_count(&distinct), 2);
    }

    #[test]
    fn provider_upsert_keeps_manual_profiles_with_distinct_ids() {
        let conn = test_connection();
        let mut gpt = provider("gpt-profile", "Same API", Some("sk-shared"));
        gpt.model = "gpt-5.6".to_string();
        let added = upsert_provider_on_connection(&conn, gpt.clone(), ProviderUpsertMode::Manual)
            .expect("save GPT profile");
        assert_eq!(added.kind, ProviderUpsertKind::Added);

        let exact_copy = SavedProvider {
            id: "gpt-copy".to_string(),
            ..gpt.clone()
        };
        let added = upsert_provider_on_connection(&conn, exact_copy, ProviderUpsertMode::Manual)
            .expect("save an identical profile under a different ID");
        assert_eq!(added.kind, ProviderUpsertKind::Added);
        assert_eq!(added.provider.id, "gpt-copy");

        let renamed_copy = SavedProvider {
            id: "gpt-renamed".to_string(),
            provider_name: "Renamed API".to_string(),
            ..gpt.clone()
        };
        let added = upsert_provider_on_connection(&conn, renamed_copy, ProviderUpsertMode::Manual)
            .expect("save a renamed profile under a different ID");
        assert_eq!(added.kind, ProviderUpsertKind::Added);

        let edited = SavedProvider {
            provider_name: "Edited GPT".to_string(),
            ..gpt
        };
        let updated = upsert_provider_on_connection(&conn, edited, ProviderUpsertMode::Manual)
            .expect("update the profile with the same ID");
        assert_eq!(updated.kind, ProviderUpsertKind::Updated);
        assert_eq!(updated.provider.id, "gpt-profile");
        assert_eq!(updated.provider.provider_name, "Edited GPT");

        let mut deepseek = provider("deepseek-profile", "Renamed API", Some("sk-shared"));
        deepseek.model = " deepseek-v3 ".to_string();
        let added = upsert_provider_on_connection(&conn, deepseek, ProviderUpsertMode::Manual)
            .expect("save DeepSeek profile with shared credentials");
        assert_eq!(added.kind, ProviderUpsertKind::Added);
        assert_eq!(provider_count(&conn), 4);
    }

    #[test]
    fn provider_listing_preserves_insertion_order_when_timestamps_match() {
        let conn = test_connection();
        for id in ["z-original", "m-sibling", "a-copy"] {
            write_provider_on_connection(&conn, &provider(id, id, Some("sk-shared")))
                .expect("insert provider");
        }
        conn.execute(
            "UPDATE providers
             SET created_at = '2026-01-01T00:00:00Z',
                 updated_at = CASE id
                     WHEN 'z-original' THEN '2026-03-01T00:00:00Z'
                     WHEN 'm-sibling' THEN '2026-02-01T00:00:00Z'
                     ELSE '2026-01-01T00:00:00Z'
                 END",
            [],
        )
        .expect("align creation timestamps");

        let listed_ids = list_saved_providers_on_connection(&conn)
            .expect("list providers")
            .into_iter()
            .map(|provider| provider.id)
            .collect::<Vec<_>>();
        assert_eq!(listed_ids, ["z-original", "m-sibling", "a-copy"]);

        let mut edited = provider("z-original", "Edited original", Some("sk-shared"));
        edited.model = "gpt-5.6".to_string();
        upsert_provider_on_connection(&conn, edited, ProviderUpsertMode::Manual)
            .expect("update first provider");
        let listed_ids = list_saved_providers_on_connection(&conn)
            .expect("list providers after edit")
            .into_iter()
            .map(|provider| provider.id)
            .collect::<Vec<_>>();
        assert_eq!(listed_ids, ["z-original", "m-sibling", "a-copy"]);
    }

    #[test]
    fn manual_save_preserves_an_existing_legacy_reserved_id() {
        let conn = test_connection();
        let legacy = provider("custom", "Legacy Provider", Some("sk-legacy"));
        write_provider_on_connection(&conn, &legacy).expect("seed legacy custom record");

        let edited = SavedProvider {
            provider_name: "Edited Legacy Provider".to_string(),
            ..legacy
        };
        let saved = save_manual_provider_on_connection(&conn, edited)
            .expect("edit the existing legacy custom record");

        assert_eq!(saved.id, "custom");
        assert_eq!(saved.provider_name, "Edited Legacy Provider");
        assert_eq!(provider_count(&conn), 1);
        assert!(provider_by_id_on_connection(&conn, "custom-custom")
            .expect("look up normalized duplicate")
            .is_none());
    }

    #[test]
    fn ccswitch_import_keeps_ambiguous_manual_profiles_and_updates_by_source_id() {
        let conn = test_connection();
        let first = provider("manual-one", "Same API", Some("sk-shared"));
        let second = SavedProvider {
            id: "manual-two".to_string(),
            ..first.clone()
        };
        upsert_provider_on_connection(&conn, first, ProviderUpsertMode::Manual)
            .expect("save first manual profile");
        upsert_provider_on_connection(&conn, second, ProviderUpsertMode::Manual)
            .expect("save second manual profile");

        let imported = provider("cc-profile", "Same API", Some("sk-shared"));
        let added = upsert_ccswitch_provider_on_connection(&conn, imported.clone(), "cc-source-id")
            .expect("import alongside ambiguous manual profiles");
        assert_eq!(added.kind, ProviderUpsertKind::Added);
        assert_eq!(added.provider.id, "cc-profile");
        assert_eq!(provider_count(&conn), 3);

        let repeated = SavedProvider {
            provider_name: "Updated from CC Switch".to_string(),
            ..imported
        };
        let updated = upsert_ccswitch_provider_on_connection(&conn, repeated, "cc-source-id")
            .expect("update the same imported source row");
        assert_eq!(updated.kind, ProviderUpsertKind::Updated);
        assert_eq!(updated.provider.id, "cc-profile");
        assert_eq!(updated.provider.provider_name, "Updated from CC Switch");
        assert_eq!(provider_count(&conn), 3);
    }

    #[test]
    fn ccswitch_import_keeps_a_manual_copy_of_its_current_profile() {
        let conn = test_connection();
        let imported = provider("cc-profile", "Same API", Some("sk-shared"));
        upsert_ccswitch_provider_on_connection(&conn, imported.clone(), "cc-source-id")
            .expect("seed imported profile");
        let manual_copy = SavedProvider {
            id: "manual-copy".to_string(),
            ..imported.clone()
        };
        upsert_provider_on_connection(&conn, manual_copy, ProviderUpsertMode::Manual)
            .expect("save manual copy");

        let updated = upsert_ccswitch_provider_on_connection(&conn, imported, "cc-source-id")
            .expect("refresh imported profile by source ID");
        assert_eq!(updated.kind, ProviderUpsertKind::Updated);
        assert_eq!(updated.provider.id, "cc-profile");
        assert_eq!(provider_count(&conn), 2);
        assert!(provider_by_id_on_connection(&conn, "manual-copy")
            .expect("read manual copy")
            .is_some());
    }

    #[test]
    fn stable_import_updates_its_source_without_claiming_a_manual_id() {
        let conn = test_connection();
        let mut imported = provider("cc-old", "Imported old", Some("sk-old"));
        imported.base_url = "https://old.example.com/v1".to_string();
        upsert_ccswitch_provider_on_connection(&conn, imported, "cc-row")
            .expect("seed stable imported provider");

        let mut manual = provider("manual-local", "Locally edited", Some("sk-shared"));
        manual.base_url = "https://shared.example.com/v1".to_string();
        manual.model = "remote-model".to_string();
        manual.toml_config = Some("model = \"remote-model\"".to_string());
        upsert_provider_on_connection(&conn, manual, ProviderUpsertMode::Manual)
            .expect("seed manual provider");

        let mut changed = provider("cc-old", "Imported changed", Some("sk-shared"));
        changed.base_url = "https://shared.example.com/v1".to_string();
        changed.model = "remote-model".to_string();
        let result = upsert_ccswitch_provider_on_connection(&conn, changed, "cc-row")
            .expect("update the imported source record");

        assert_eq!(result.kind, ProviderUpsertKind::Updated);
        assert_eq!(result.provider.id, "cc-old");
        assert_eq!(result.provider.provider_name, "Imported changed");
        assert_eq!(result.provider.model, "remote-model");
        let rows = stored_providers_on_connection(&conn).expect("read independent providers");
        assert_eq!(rows.len(), 2);
        let manual = rows
            .iter()
            .find(|row| row.provider.id == "manual-local")
            .expect("manual provider remains");
        assert_eq!(manual.provider.provider_name, "Locally edited");
        assert_eq!(manual.source, MANUAL_PROVIDER_SOURCE);
        let imported = rows
            .iter()
            .find(|row| row.provider.id == "cc-old")
            .expect("imported provider remains");
        assert_eq!(imported.source, CCSWITCH_PROVIDER_SOURCE);
        assert_eq!(imported.source_id.as_deref(), Some("cc-row"));
    }

    #[test]
    fn ccswitch_import_replaces_stale_local_values_with_complete_source() {
        let conn = test_connection();
        let mut stale = provider("legacy-local", "Local name", Some("sk-local"));
        stale.base_url = "https://old.example.com/v1".to_string();
        stale.model = "local-model".to_string();
        stale.toml_config = Some(
            r#"model_provider = "custom"
model = "local-model"

[model_providers.custom]
name = "Local name"
base_url = "https://old.example.com/v1"
wire_api = "chat"
requires_openai_auth = true
request_max_retries = 3
"#
            .to_string(),
        );
        stale.wire_api = "chat".to_string();
        write_provider_with_origin(
            &conn,
            &stale,
            Some((CCSWITCH_LOCAL_PROVIDER_SOURCE, "cc-row")),
        )
        .expect("seed stale local import");

        let mut imported = provider("cc-row", "CC name", Some("sk-imported"));
        imported.base_url = "https://new.example.com/v1".to_string();
        imported.model = "remote-model".to_string();
        imported.requires_openai_auth = false;
        imported.toml_config = Some(
            r#"# complete cc-switch template
model_provider = "custom"
model = "remote-model"
service_tier = "priority"

[model_providers.custom]
name = "CC name"
base_url = "https://new.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
request_max_retries = 7

[projects."/work/project"]
trust_level = "trusted"

[plugins."browser@openai-bundled"]
enabled = true
"#
            .to_string(),
        );
        let result = upsert_ccswitch_provider_on_connection(&conn, imported, "cc-row")
            .expect("replace stale local values from the authoritative cc-switch row");

        assert_eq!(result.kind, ProviderUpsertKind::Updated);
        assert_eq!(result.provider.id, "legacy-local");
        assert_eq!(result.provider.provider_name, "CC name");
        assert_eq!(result.provider.base_url, "https://new.example.com/v1");
        assert_eq!(result.provider.model, "remote-model");
        assert_eq!(result.provider.api_key.as_deref(), Some("sk-imported"));
        assert_eq!(result.provider.wire_api, "responses");
        assert!(!result.provider.requires_openai_auth);
        let template = result
            .provider
            .toml_config
            .expect("complete cc-switch template");
        let doc = template
            .parse::<DocumentMut>()
            .expect("parse imported provider template");
        assert!(template.contains("# complete cc-switch template"));
        assert_eq!(doc["service_tier"].as_str(), Some("priority"));
        assert_eq!(doc["model"].as_str(), Some("remote-model"));
        assert_eq!(
            doc["model_providers"]["custom"]["name"].as_str(),
            Some("CC name")
        );
        assert_eq!(
            doc["model_providers"]["custom"]["request_max_retries"].as_integer(),
            Some(7)
        );
        assert_eq!(
            doc["projects"]["/work/project"]["trust_level"].as_str(),
            Some("trusted")
        );
        assert_eq!(
            doc["plugins"]["browser@openai-bundled"]["enabled"].as_bool(),
            Some(true)
        );
        let rows = stored_providers_on_connection(&conn).expect("read imported row");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, CCSWITCH_PROVIDER_SOURCE);
        assert_eq!(rows[0].source_id.as_deref(), Some("cc-row"));
    }

    #[test]
    fn ccswitch_import_falls_back_only_when_source_fields_are_missing() {
        let conn = test_connection();
        let mut existing = provider("local-id", "Existing name", Some("sk-existing"));
        existing.base_url = "https://existing.example.com/v1".to_string();
        existing.model = "existing-model".to_string();
        existing.toml_config = Some("model = \"existing-model\"".to_string());
        existing.wire_api = "responses".to_string();
        write_provider_with_origin(
            &conn,
            &existing,
            Some((CCSWITCH_LOCAL_PROVIDER_SOURCE, "cc-row")),
        )
        .expect("seed existing source row");

        let missing = SavedProvider {
            id: "remote-id".to_string(),
            provider_name: " ".to_string(),
            base_url: String::new(),
            model: "\t".to_string(),
            api_key: Some(" ".to_string()),
            toml_config: Some("\n".to_string()),
            wire_api: String::new(),
            requires_openai_auth: false,
            upstream_api: None,
            model_mappings: Vec::new(),
        };
        let result = upsert_ccswitch_provider_on_connection(&conn, missing, "cc-row")
            .expect("fill missing import fields from the existing source row");

        assert_eq!(result.provider.id, "local-id");
        assert_eq!(result.provider.provider_name, "Existing name");
        assert_eq!(result.provider.base_url, "https://existing.example.com/v1");
        assert_eq!(result.provider.model, "existing-model");
        assert_eq!(result.provider.api_key.as_deref(), Some("sk-existing"));
        assert_eq!(
            result.provider.toml_config.as_deref(),
            Some("model = \"existing-model\"")
        );
        assert_eq!(result.provider.wire_api, "responses");
        assert!(!result.provider.requires_openai_auth);
    }

    #[test]
    fn legacy_cleanup_preserves_historical_custom_ids() {
        let conn = test_connection();
        write_provider_on_connection(&conn, &provider("local", "Same API", Some("sk-current")))
            .unwrap();
        write_provider_on_connection(
            &conn,
            &provider("custom", " same   api ", Some("sk-old-live")),
        )
        .unwrap();

        assert_eq!(
            consolidate_legacy_provider_duplicates_on_connection(&conn).unwrap(),
            0
        );
        let rows = list_saved_providers_on_connection(&conn).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.id == "local"));
        assert!(rows.iter().any(|row| row.id == "custom"));

        let ambiguous = test_connection();
        write_provider_on_connection(
            &ambiguous,
            &provider("local-a", "Same API", Some("sk-first")),
        )
        .unwrap();
        write_provider_on_connection(
            &ambiguous,
            &provider("local-b", "Same API", Some("sk-second")),
        )
        .unwrap();
        write_provider_on_connection(
            &ambiguous,
            &provider("custom-2", "Same API", Some("sk-old-live")),
        )
        .unwrap();

        assert_eq!(
            consolidate_legacy_provider_duplicates_on_connection(&ambiguous).unwrap(),
            0
        );
        assert_eq!(provider_count(&ambiguous), 3);

        let different_models = test_connection();
        let mut stable = provider("local", "Same API", Some("sk-current"));
        stable.model = "deepseek-v3".to_string();
        write_provider_on_connection(&different_models, &stable).unwrap();
        let mut ghost = provider("custom", " same   api ", Some("sk-old-live"));
        ghost.model = "gpt-5.6".to_string();
        write_provider_on_connection(&different_models, &ghost).unwrap();

        assert_eq!(
            consolidate_legacy_provider_duplicates_on_connection(&different_models).unwrap(),
            0
        );
        assert_eq!(provider_count(&different_models), 2);
    }

    #[test]
    fn legacy_consolidation_keeps_distinct_models_for_shared_credentials() {
        let conn = test_connection();
        let mut gpt = provider("gpt-profile", "Same API", Some("sk-shared"));
        gpt.model = "gpt-5.6".to_string();
        write_provider_on_connection(&conn, &gpt).unwrap();
        let mut deepseek = provider("deepseek-profile", "Same API", Some("sk-shared"));
        deepseek.model = "deepseek-v3".to_string();
        write_provider_on_connection(&conn, &deepseek).unwrap();

        assert_eq!(
            consolidate_legacy_provider_duplicates_on_connection(&conn).unwrap(),
            0
        );
        assert_eq!(provider_count(&conn), 2);
    }

    #[test]
    fn legacy_consolidation_preserves_manual_and_imported_ids() {
        let conn = test_connection();
        let mut imported = provider("imported-old", "Imported old", Some("sk-same"));
        imported.model = "latest-model".to_string();
        write_provider_with_origin(&conn, &imported, Some((CCSWITCH_PROVIDER_SOURCE, "cc-row")))
            .unwrap();
        let mut manual = provider("manual-local", "Edited locally", Some("sk-same"));
        manual.model = "latest-model".to_string();
        manual.toml_config = Some("model = \"latest-model\"".to_string());
        write_provider_on_connection(&conn, &manual).unwrap();
        conn.execute(
            "UPDATE providers SET updated_at = CASE id
                WHEN 'imported-old' THEN '2026-02-01T00:00:00Z'
                ELSE '2026-01-01T00:00:00Z' END",
            [],
        )
        .unwrap();

        assert_eq!(
            consolidate_legacy_provider_duplicates_on_connection(&conn).unwrap(),
            0
        );
        let rows = stored_providers_on_connection(&conn).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.provider.id == "manual-local"));
        assert!(rows.iter().any(|row| row.provider.id == "imported-old"));

        let mut repeated = provider("imported-old", "Remote replacement", Some("sk-same"));
        repeated.model = "remote-model".to_string();
        repeated.toml_config = Some("model = \"remote-model\"".to_string());
        upsert_ccswitch_provider_on_connection(&conn, repeated, "cc-row")
            .expect("repeat import after legacy consolidation");

        let rows = stored_providers_on_connection(&conn).unwrap();
        assert_eq!(rows.len(), 2);
        let manual = rows
            .iter()
            .find(|row| row.provider.id == "manual-local")
            .expect("manual provider remains");
        assert_eq!(manual.provider.provider_name, "Edited locally");
        assert_eq!(
            manual.provider.toml_config.as_deref(),
            Some("model = \"latest-model\"")
        );
        let imported = rows
            .iter()
            .find(|row| row.provider.id == "imported-old")
            .expect("imported provider remains");
        assert_eq!(imported.provider.provider_name, "Remote replacement");
        assert_eq!(imported.provider.model, "remote-model");
        assert_eq!(imported.source, CCSWITCH_PROVIDER_SOURCE);
        assert_eq!(imported.source_id.as_deref(), Some("cc-row"));
    }

    #[test]
    fn legacy_consolidation_does_not_merge_different_external_sources() {
        let conn = test_connection();
        write_provider_with_origin(
            &conn,
            &provider("cc-one", "Same API", Some("sk-same")),
            Some((CCSWITCH_PROVIDER_SOURCE, "row-one")),
        )
        .unwrap();
        write_provider_with_origin(
            &conn,
            &provider("cc-two", "Same API", Some("sk-same")),
            Some((CCSWITCH_PROVIDER_SOURCE, "row-two")),
        )
        .unwrap();

        assert_eq!(
            consolidate_legacy_provider_duplicates_on_connection(&conn).unwrap(),
            0
        );
        assert_eq!(provider_count(&conn), 2);
    }

    #[test]
    fn live_custom_matches_a_preserved_historical_record_by_its_profile() {
        let stable = provider("stable", "Same API", Some("sk-current"));
        let historical = provider("custom", "Same API", Some("sk-old-live"));
        let live = provider("custom", " same   api ", Some("sk-old-live"));
        assert_eq!(
            unique_saved_provider_id_for_live(&live, &[stable.clone(), historical]),
            Some("custom".to_string())
        );

        let second = provider("second", "Same API", Some("sk-second"));
        assert_eq!(
            unique_saved_provider_id_for_live(&live, &[stable, second]),
            None
        );
    }

    #[test]
    fn live_custom_matches_the_saved_profile_with_the_same_model() {
        let mut gpt = provider("gpt-profile", "Same API", Some("sk-shared"));
        gpt.model = "gpt-5.6".to_string();
        let mut deepseek = provider("deepseek-profile", "Same API", Some("sk-shared"));
        deepseek.model = "deepseek-v3".to_string();
        let mut live = provider("custom", "Same API", Some("sk-shared"));
        live.model = " deepseek-v3 ".to_string();

        assert_eq!(
            unique_saved_provider_id_for_live(&live, &[gpt, deepseek]),
            Some("deepseek-profile".to_string())
        );
    }

    #[test]
    fn provider_toml_keeps_extensions_and_syncs_explicit_field_edits() {
        let mut item = provider("saved", "Edited Name", Some("sk-explicit"));
        item.base_url = "https://edited.example.com/v1/".to_string();
        item.model = "edited-model".to_string();
        item.wire_api = "chat_completions".to_string();
        item.requires_openai_auth = false;
        item.toml_config = Some(
            r#"model_provider = "proxy"
model = "stale-model"
approval_policy = "never"
experimental_bearer_token = "sk-top-level"

[model_providers.proxy]
name = "Stale Name"
base_url = "https://stale.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
experimental_bearer_token = "sk-stale"
request_max_retries = 7

[model_providers.proxy.http_headers]
X-Route = "keep-me"

[model_providers.unrelated]
name = "Keep this provider"
base_url = "https://unrelated.example.com/v1"
experimental_bearer_token = "sk-unrelated"

[mcp_servers.docs]
command = "keep-this-command"
"#
            .to_string(),
        );

        let normalized = normalize_saved_provider(item).expect("normalize provider TOML");
        let text = normalized.toml_config.as_deref().unwrap();
        let doc = text.parse::<DocumentMut>().expect("parse normalized TOML");
        assert_eq!(normalized.provider_name, "Edited Name");
        assert_eq!(normalized.base_url, "https://edited.example.com/v1");
        assert_eq!(normalized.model, "edited-model");
        assert_eq!(normalized.wire_api, "responses");
        assert_eq!(normalized.upstream_api.as_deref(), Some("chat_completions"));
        assert!(!normalized.requires_openai_auth);
        assert_eq!(doc["model"].as_str(), Some("edited-model"));
        assert_eq!(
            doc["model_providers"]["proxy"]["name"].as_str(),
            Some("Edited Name")
        );
        assert_eq!(
            doc["model_providers"]["proxy"]["base_url"].as_str(),
            Some("https://edited.example.com/v1")
        );
        assert_eq!(
            doc["model_providers"]["proxy"]["wire_api"].as_str(),
            Some("responses")
        );
        assert_eq!(
            doc["model_providers"]["proxy"]["requires_openai_auth"].as_bool(),
            Some(false)
        );
        assert_eq!(
            doc["model_providers"]["proxy"]["request_max_retries"].as_integer(),
            Some(7)
        );
        assert_eq!(
            doc["model_providers"]["proxy"]["http_headers"]["X-Route"].as_str(),
            Some("keep-me")
        );
        assert_eq!(doc["model_provider"].as_str(), Some("proxy"));
        assert_eq!(doc["approval_policy"].as_str(), Some("never"));
        assert_eq!(
            doc["model_providers"]["unrelated"]["name"].as_str(),
            Some("Keep this provider")
        );
        assert_eq!(
            doc["mcp_servers"]["docs"]["command"].as_str(),
            Some("keep-this-command")
        );
        assert!(doc.get("experimental_bearer_token").is_none());
        assert!(doc["model_providers"]
            .as_table()
            .expect("model providers table")
            .iter()
            .all(|(_, item)| item
                .as_table()
                .is_none_or(|table| table.get("experimental_bearer_token").is_none())));
        assert!(!text.contains("experimental_bearer_token"));
        assert_eq!(normalized.api_key.as_deref(), Some("sk-explicit"));
    }

    #[test]
    fn legacy_rows_are_repaired_from_complete_toml_on_read() {
        let conn = test_connection();
        let mut legacy = provider("legacy", "Wrong database name", None);
        legacy.base_url = "https://wrong.example.com/v1".to_string();
        legacy.model = "wrong-model".to_string();
        legacy.wire_api = "chat_completions".to_string();
        legacy.requires_openai_auth = true;
        legacy.toml_config = Some(
            r#"# authoritative saved template
model_provider = "proxy"
model = "gpt-5.6-sol"
service_tier = "priority"

[model_providers.proxy]
name = "Authoritative Name"
base_url = "https://RIGHT.example.com/v1/"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "sk-from-template"
request_max_retries = 9
"#
            .to_string(),
        );
        write_provider_on_connection(&conn, &legacy).expect("seed legacy mismatch");

        let listed = list_saved_providers_on_connection(&conn).expect("read providers");
        assert_eq!(listed.len(), 1);
        let repaired = &listed[0];
        assert_eq!(repaired.provider_name, "Authoritative Name");
        assert_eq!(repaired.base_url, "https://right.example.com/v1");
        assert_eq!(repaired.model, "gpt-5.6-sol");
        assert_eq!(repaired.wire_api, "responses");
        assert!(!repaired.requires_openai_auth);
        assert_eq!(repaired.api_key.as_deref(), Some("sk-from-template"));
        let template = repaired.toml_config.as_deref().expect("saved template");
        assert!(template.contains("# authoritative saved template"));
        assert!(template.contains("request_max_retries = 9"));
        assert!(!template.contains("experimental_bearer_token"));

        let by_id = provider_by_id_on_connection(&conn, "legacy")
            .expect("read provider by id")
            .expect("legacy provider");
        assert_eq!(by_id, *repaired);

        let raw_model: String = conn
            .query_row(
                "SELECT model FROM providers WHERE id = 'legacy'",
                [],
                |row| row.get(0),
            )
            .expect("read raw legacy scalar");
        assert_eq!(raw_model, "wrong-model");
    }

    #[test]
    fn malformed_legacy_toml_does_not_break_provider_reads() {
        let conn = test_connection();
        let mut legacy = provider("legacy", "Fallback Name", Some("sk-fallback"));
        legacy.base_url = "https://fallback.example.com/v1".to_string();
        legacy.model = "fallback-model".to_string();
        legacy.toml_config = Some("model = [".to_string());
        write_provider_on_connection(&conn, &legacy).expect("seed malformed legacy row");

        let listed = list_saved_providers_on_connection(&conn).expect("read malformed legacy row");
        assert_eq!(listed, vec![legacy]);
    }

    #[test]
    fn placeholder_provider_values_are_rejected() {
        let mut item = provider("placeholder", "your-provider", None);
        item.model = "gpt-5.5".to_string();
        let error = normalize_saved_provider(item).expect_err("placeholder must be rejected");
        assert!(error.to_string().contains("示例占位值"));
    }
}
