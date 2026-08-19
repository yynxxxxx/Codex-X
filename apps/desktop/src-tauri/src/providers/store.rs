use super::open_store as open_db;
use crate::error::{CodexxError, Result};
use crate::{now_rfc3339, sanitize_id};
use rusqlite::{params, Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use toml_edit::{value, DocumentMut};

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
    pub(crate) requires_openai_auth: bool,
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
    Unauthenticated {
        base_url: String,
        name: String,
        model: String,
        wire_api: String,
        requires_openai_auth: bool,
    },
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
    Merged,
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

fn normalized_provider_model(provider: &SavedProvider) -> String {
    provider.model.trim().to_string()
}

fn normalized_provider_wire_api(provider: &SavedProvider) -> String {
    provider.wire_api.trim().to_ascii_lowercase()
}

fn same_provider_profile_fields(left: &SavedProvider, right: &SavedProvider) -> bool {
    normalized_provider_model(left) == normalized_provider_model(right)
        && normalized_provider_wire_api(left) == normalized_provider_wire_api(right)
        && left.requires_openai_auth == right.requires_openai_auth
}

pub(crate) fn provider_identity(provider: &SavedProvider) -> Option<ProviderIdentity> {
    use sha2::{Digest, Sha256};

    let base_url = canonical_provider_base_url(&provider.base_url);
    if base_url.is_empty() {
        return None;
    }
    let model = normalized_provider_model(provider);
    let wire_api = normalized_provider_wire_api(provider);
    if let Some(api_key) = effective_provider_api_key(provider) {
        // Hash the complete profile so the credential cannot be recovered or
        // correlated independently of its endpoint and runtime settings.
        let mut hasher = Sha256::new();
        hasher.update(b"codex-x/provider-profile-identity/v2\0");
        for part in [
            base_url.as_str(),
            api_key.as_str(),
            model.as_str(),
            wire_api.as_str(),
        ] {
            hasher.update(part.as_bytes());
            hasher.update(b"\0");
        }
        hasher.update([u8::from(provider.requires_openai_auth)]);
        let digest: [u8; 32] = hasher.finalize().into();
        return Some(ProviderIdentity::Credential(digest));
    }

    let name = normalized_provider_name(&provider.provider_name);
    (!name.is_empty()).then_some(ProviderIdentity::Unauthenticated {
        base_url,
        name,
        model,
        wire_api,
        requires_openai_auth: provider.requires_openai_auth,
    })
}

pub(crate) fn provider_template_from_document(
    doc: &DocumentMut,
    provider_id: &str,
    model: &str,
) -> Result<String> {
    let provider_exists = doc
        .get("model_providers")
        .and_then(|item| item.as_table())
        .and_then(|providers| providers.get(provider_id))
        .and_then(|item| item.as_table())
        .is_some();
    if !provider_exists {
        return Err(CodexxError::Config(format!(
            "供应商 TOML 缺少 [model_providers.{provider_id}]"
        )));
    }

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

fn compatible_provider_match(left: &SavedProvider, right: &SavedProvider) -> bool {
    if !same_provider_endpoint(left, right) || !same_provider_profile_fields(left, right) {
        return false;
    }
    match (
        effective_provider_api_key(left),
        effective_provider_api_key(right),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => {
            normalized_provider_name(&left.provider_name)
                == normalized_provider_name(&right.provider_name)
        }
    }
}

fn unique_compatible_provider<'a>(
    stored: &'a [StoredProvider],
    incoming: &SavedProvider,
) -> Option<&'a StoredProvider> {
    let mut matches = stored
        .iter()
        .filter(|candidate| compatible_provider_match(&candidate.provider, incoming));
    let candidate = matches.next()?;
    matches.next().is_none().then_some(candidate)
}

pub(crate) fn matching_saved_provider_ids_for_live(
    live: &SavedProvider,
    providers: &[SavedProvider],
) -> Vec<String> {
    let stable = providers
        .iter()
        .filter(|candidate| !is_historical_custom_provider_id(&candidate.id))
        .collect::<Vec<_>>();
    if let Some(identity) = provider_identity(live) {
        let exact = stable
            .iter()
            .copied()
            .filter(|candidate| provider_identity(candidate).as_ref() == Some(&identity))
            .collect::<Vec<_>>();
        if !exact.is_empty() {
            return exact
                .into_iter()
                .map(|candidate| candidate.id.clone())
                .collect();
        }
    }

    let compatible = stable
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
    stable
        .into_iter()
        .filter(|candidate| {
            same_provider_endpoint_and_name(live, candidate)
                && same_provider_profile_fields(live, candidate)
        })
        .map(|candidate| candidate.id.clone())
        .collect()
}

pub(crate) fn unique_saved_provider_id_for_live(
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
        requires_openai_auth: row.get::<_, i64>(7)? != 0,
    })
}

fn stored_providers_on_connection(conn: &Connection) -> Result<Vec<StoredProvider>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, provider_name, base_url, model, api_key, toml_config, wire_api,
                    requires_openai_auth, created_at, updated_at, source, source_id
             FROM providers
             ORDER BY created_at ASC, updated_at ASC, id ASC",
        )
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(StoredProvider {
                provider: saved_provider_from_row(row)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                source: row.get(10)?,
                source_id: row.get(11)?,
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

pub(crate) fn provider_by_id_on_connection(
    conn: &Connection,
    id: &str,
) -> Result<Option<SavedProvider>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, provider_name, base_url, model, api_key, toml_config, wire_api,
                    requires_openai_auth
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
    let (source, source_id) = origin
        .map(|(source, source_id)| (source, Some(source_id)))
        .unwrap_or((MANUAL_PROVIDER_SOURCE, None));
    conn.execute(
        "INSERT INTO providers
            (id, provider_name, base_url, model, api_key, toml_config, wire_api,
             requires_openai_auth, source, source_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
         ON CONFLICT(id) DO UPDATE SET
            provider_name = excluded.provider_name,
            base_url = excluded.base_url,
            model = excluded.model,
            api_key = excluded.api_key,
            toml_config = excluded.toml_config,
            wire_api = excluded.wire_api,
            requires_openai_auth = excluded.requires_openai_auth,
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
    incoming
}

fn source_matches(row: &StoredProvider, origin: (&str, &str)) -> bool {
    let source_matches = row.source == origin.0
        || (origin.0 == CCSWITCH_PROVIDER_SOURCE && row.source == CCSWITCH_LOCAL_PROVIDER_SOURCE);
    source_matches && row.source_id.as_deref() == Some(origin.1)
}

fn source_can_merge(row: &StoredProvider, origin: Option<(&str, &str)>) -> bool {
    match origin {
        None => true,
        Some(origin) => row.source == MANUAL_PROVIDER_SOURCE || source_matches(row, origin),
    }
}

fn conflicting_provider_ids(
    stored: &[StoredProvider],
    provider: &SavedProvider,
    origin: Option<(&str, &str)>,
) -> Vec<String> {
    let identity = provider_identity(provider);
    stored
        .iter()
        .filter(|candidate| candidate.provider.id != provider.id)
        .filter(|candidate| source_can_merge(candidate, origin))
        .filter(|candidate| {
            identity.as_ref().is_some_and(|identity| {
                provider_identity(&candidate.provider).as_ref() == Some(identity)
            }) || (is_historical_custom_provider_id(&candidate.provider.id)
                && !is_historical_custom_provider_id(&provider.id)
                && same_provider_endpoint_and_name(&candidate.provider, provider)
                && same_provider_profile_fields(&candidate.provider, provider))
        })
        .map(|candidate| candidate.provider.id.clone())
        .collect()
}

fn upsert_provider_in_savepoint(
    conn: &Connection,
    mut provider: SavedProvider,
    mode: ProviderUpsertMode,
    origin: Option<(&str, &str)>,
) -> Result<ProviderUpsertResult> {
    let requested_id = provider.id.clone();
    let identity = provider_identity(&provider);
    let stored = stored_providers_on_connection(conn)?;
    let source_match = origin.and_then(|origin| {
        stored
            .iter()
            .find(|candidate| source_matches(candidate, origin))
    });
    let merge_candidates = stored
        .iter()
        .filter(|candidate| source_can_merge(candidate, origin))
        .cloned()
        .collect::<Vec<_>>();
    let identity_match = identity.as_ref().and_then(|identity| {
        merge_candidates
            .iter()
            .find(|candidate| provider_identity(&candidate.provider).as_ref() == Some(identity))
    });
    let manual_candidates = stored
        .iter()
        .filter(|candidate| candidate.source == MANUAL_PROVIDER_SOURCE)
        .cloned()
        .collect::<Vec<_>>();
    let manual_identity_match = identity.as_ref().and_then(|identity| {
        manual_candidates
            .iter()
            .find(|candidate| provider_identity(&candidate.provider).as_ref() == Some(identity))
    });
    let manual_compatible_match = unique_compatible_provider(&manual_candidates, &provider);
    let exact_id_match = stored
        .iter()
        .find(|candidate| candidate.provider.id == requested_id);
    let compatible_match = unique_compatible_provider(&merge_candidates, &provider);
    let import_rehome_target = source_match.and_then(|source| {
        manual_identity_match
            .or(manual_compatible_match)
            .filter(|candidate| candidate.provider.id != source.provider.id)
    });

    let target = match mode {
        ProviderUpsertMode::Manual => exact_id_match.or(identity_match).or(compatible_match),
        ProviderUpsertMode::Imported => import_rehome_target
            .or(source_match)
            .or(identity_match)
            .or(compatible_match),
    };
    let preserves_local_id = mode == ProviderUpsertMode::Imported
        && target.is_some_and(|candidate| {
            candidate.source == MANUAL_PROVIDER_SOURCE
                || candidate.source == CCSWITCH_LOCAL_PROVIDER_SOURCE
        });
    let kind = if let Some(target) = target {
        let existing = &target.provider;
        let same_id = existing.id == requested_id;
        provider.id = existing.id.clone();
        if mode == ProviderUpsertMode::Imported {
            provider = merge_authoritative_import(provider, existing);
        }
        if import_rehome_target.is_some() {
            ProviderUpsertKind::Merged
        } else if source_match.is_some() || same_id {
            ProviderUpsertKind::Updated
        } else {
            ProviderUpsertKind::Merged
        }
    } else {
        if exact_id_match.is_some() {
            provider.id = unique_provider_id_on_connection(conn, &provider.id)?;
        }
        ProviderUpsertKind::Added
    };

    if let (Some(source), Some(target)) = (source_match, import_rehome_target) {
        if source.provider.id != target.provider.id {
            conn.execute("DELETE FROM providers WHERE id = ?1", [&source.provider.id])
                .map_err(|e| CodexxError::Database(e.to_string()))?;
        }
    }
    let write_origin = match origin {
        Some((CCSWITCH_PROVIDER_SOURCE, source_id)) if preserves_local_id => {
            Some((CCSWITCH_LOCAL_PROVIDER_SOURCE, source_id))
        }
        _ => origin,
    };
    write_provider_with_origin(conn, &provider, write_origin)?;
    for duplicate_id in conflicting_provider_ids(&stored, &provider, origin) {
        conn.execute("DELETE FROM providers WHERE id = ?1", [&duplicate_id])
            .map_err(|e| CodexxError::Database(e.to_string()))?;
    }
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
    conn: &Connection,
) -> Result<usize> {
    const SAVEPOINT: &str = "codex_x_provider_cleanup";
    conn.execute_batch(&format!("SAVEPOINT {SAVEPOINT}"))
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let result = (|| -> Result<usize> {
        let rows = stored_providers_on_connection(conn)?;
        let mut groups: HashMap<ProviderIdentity, Vec<StoredProvider>> = HashMap::new();
        for row in rows {
            if let Some(identity @ ProviderIdentity::Credential(_)) =
                provider_identity(&row.provider)
            {
                groups.entry(identity).or_default().push(row);
            }
        }
        let duplicate_groups = groups
            .into_values()
            .filter(|group| group.len() > 1)
            .collect::<Vec<_>>();
        let mut merged = 0usize;
        for mut group in duplicate_groups {
            let preserves_local_profile = group.iter().any(|row| {
                row.source == MANUAL_PROVIDER_SOURCE || row.source == CCSWITCH_LOCAL_PROVIDER_SOURCE
            });
            let mut origins = group
                .iter()
                .filter_map(|row| {
                    row.source_id.as_ref().map(|source_id| {
                        let source = if row.source == CCSWITCH_LOCAL_PROVIDER_SOURCE {
                            CCSWITCH_PROVIDER_SOURCE.to_string()
                        } else {
                            row.source.clone()
                        };
                        (source, source_id.clone())
                    })
                })
                .collect::<Vec<_>>();
            origins.sort();
            origins.dedup();
            if origins.len() > 1 {
                continue;
            }
            group.sort_by(|left, right| {
                let left_is_local = left.source == MANUAL_PROVIDER_SOURCE
                    || left.source == CCSWITCH_LOCAL_PROVIDER_SOURCE;
                let right_is_local = right.source == MANUAL_PROVIDER_SOURCE
                    || right.source == CCSWITCH_LOCAL_PROVIDER_SOURCE;
                right_is_local
                    .cmp(&left_is_local)
                    .then_with(|| {
                        is_historical_custom_provider_id(&left.provider.id)
                            .cmp(&is_historical_custom_provider_id(&right.provider.id))
                    })
                    .then_with(|| right.updated_at.cmp(&left.updated_at))
                    .then_with(|| right.created_at.cmp(&left.created_at))
                    .then_with(|| left.provider.id.cmp(&right.provider.id))
            });
            let mut survivor = group[0].clone();
            for duplicate in group.iter().skip(1) {
                if survivor.provider.provider_name.trim().is_empty()
                    && !duplicate.provider.provider_name.trim().is_empty()
                {
                    survivor.provider.provider_name = duplicate.provider.provider_name.clone();
                }
                if survivor.provider.model.trim().is_empty()
                    && !duplicate.provider.model.trim().is_empty()
                {
                    survivor.provider.model = duplicate.provider.model.clone();
                }
                if survivor
                    .provider
                    .toml_config
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty())
                    && duplicate
                        .provider
                        .toml_config
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
                {
                    survivor.provider.toml_config = duplicate.provider.toml_config.clone();
                }
                if survivor.provider.api_key.is_none() && duplicate.provider.api_key.is_some() {
                    survivor.provider.api_key = duplicate.provider.api_key.clone();
                }
            }
            survivor.provider.base_url = canonical_provider_base_url(&survivor.provider.base_url);
            survivor.provider.api_key = survivor
                .provider
                .api_key
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
            for duplicate in group.iter().skip(1) {
                conn.execute(
                    "DELETE FROM providers WHERE id = ?1",
                    [&duplicate.provider.id],
                )
                .map_err(|e| CodexxError::Database(e.to_string()))?;
                merged += 1;
            }
            let origin = origins.first().map(|(source, source_id)| {
                let source = if preserves_local_profile && source == CCSWITCH_PROVIDER_SOURCE {
                    CCSWITCH_LOCAL_PROVIDER_SOURCE
                } else {
                    source.as_str()
                };
                (source, source_id.as_str())
            });
            write_provider_with_origin(conn, &survivor.provider, origin)?;
        }

        loop {
            let rows = stored_providers_on_connection(conn)?;
            let ghost_id = rows.iter().find_map(|ghost| {
                if !is_historical_custom_provider_id(&ghost.provider.id) {
                    return None;
                }
                if ghost.source != MANUAL_PROVIDER_SOURCE || ghost.source_id.is_some() {
                    return None;
                }
                let mut candidates = rows.iter().filter(|candidate| {
                    !is_historical_custom_provider_id(&candidate.provider.id)
                        && same_provider_endpoint_and_name(&ghost.provider, &candidate.provider)
                        && same_provider_profile_fields(&ghost.provider, &candidate.provider)
                });
                candidates.next()?;
                candidates
                    .next()
                    .is_none()
                    .then(|| ghost.provider.id.clone())
            });
            let Some(ghost_id) = ghost_id else {
                break;
            };
            conn.execute("DELETE FROM providers WHERE id = ?1", [&ghost_id])
                .map_err(|e| CodexxError::Database(e.to_string()))?;
            merged += 1;
        }
        Ok(merged)
    })();

    match result {
        Ok(merged) => {
            conn.execute_batch(&format!("RELEASE SAVEPOINT {SAVEPOINT}"))
                .map_err(|e| CodexxError::Database(e.to_string()))?;
            Ok(merged)
        }
        Err(error) => {
            let rollback = conn.execute_batch(&format!(
                "ROLLBACK TO SAVEPOINT {SAVEPOINT}; RELEASE SAVEPOINT {SAVEPOINT}"
            ));
            match rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(CodexxError::Database(format!(
                    "{error}; provider cleanup rollback failed: {rollback_error}"
                ))),
            }
        }
    }
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

pub(crate) fn normalize_saved_provider(provider: SavedProvider) -> Result<SavedProvider> {
    let raw_id = provider.id.trim();
    if raw_id.is_empty() {
        return Err(CodexxError::Config("provider id 不能为空".to_string()));
    }
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
        requires_openai_auth: provider.requires_openai_auth,
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
    sync_provider_toml_from_fields(&mut normalized)?;
    Ok(normalized)
}

pub(crate) fn save_manual_provider_on_connection(
    conn: &Connection,
    provider: SavedProvider,
) -> Result<SavedProvider> {
    let requested_id = provider.id.trim().to_string();
    let provider = normalize_saved_provider(provider)?;
    if requested_id != provider.id && provider_by_id_on_connection(conn, &provider.id)?.is_some() {
        return Err(CodexxError::Config(format!(
            "供应商 ID {} 规范化后与现有供应商冲突，请更换名称或 ID",
            requested_id
        )));
    }
    Ok(upsert_provider_on_connection(conn, provider, ProviderUpsertMode::Manual)?.provider)
}

pub(crate) fn save_provider_inner(provider: SavedProvider) -> Result<SavedProvider> {
    let conn = open_db()?;
    save_manual_provider_on_connection(&conn, provider)
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

fn insert_stored_provider(conn: &Connection, stored: &StoredProvider) -> Result<()> {
    let provider = &stored.provider;
    conn.execute(
        "INSERT INTO providers
            (id, provider_name, base_url, model, api_key, toml_config, wire_api,
             requires_openai_auth, source, source_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            provider.id,
            provider.provider_name,
            provider.base_url,
            provider.model,
            provider.api_key,
            provider.toml_config,
            provider.wire_api,
            if provider.requires_openai_auth { 1 } else { 0 },
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
    let conn = open_db()?;
    conn.execute("DELETE FROM providers WHERE id = ?1", params![id])
        .map_err(|e| CodexxError::Database(e.to_string()))?;
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

    fn test_connection() -> Connection {
        let conn = Connection::open_in_memory().expect("open provider test database");
        conn.execute_batch(
            "CREATE TABLE providers (
                id TEXT PRIMARY KEY, provider_name TEXT NOT NULL, base_url TEXT NOT NULL,
                model TEXT NOT NULL, api_key TEXT, toml_config TEXT,
                wire_api TEXT NOT NULL DEFAULT 'responses',
                requires_openai_auth INTEGER NOT NULL DEFAULT 1,
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
        }
    }

    fn provider_count(conn: &Connection) -> usize {
        list_saved_providers_on_connection(conn).unwrap().len()
    }

    #[test]
    fn manual_profiles_keep_same_endpoint_and_key_with_different_models() {
        let conn = test_connection();
        let mut gpt = provider("waibi-gpt", "Waibi GPT", Some("test-key-shared"));
        gpt.model = "gpt-5.6".to_string();
        let mut deepseek = provider("waibi-deepseek", "Waibi DeepSeek", Some("test-key-shared"));
        deepseek.model = "deepseek-v3".to_string();

        upsert_provider_on_connection(&conn, gpt, ProviderUpsertMode::Manual).unwrap();
        upsert_provider_on_connection(&conn, deepseek, ProviderUpsertMode::Manual).unwrap();

        assert_eq!(provider_count(&conn), 2);
    }

    #[test]
    fn manual_profiles_deduplicate_an_identical_profile() {
        let conn = test_connection();
        let first = provider("profile-first", "First Name", Some("test-key-shared"));
        let second = provider("profile-second", "Renamed Profile", Some("test-key-shared"));

        let added =
            upsert_provider_on_connection(&conn, first, ProviderUpsertMode::Manual).unwrap();
        let merged =
            upsert_provider_on_connection(&conn, second, ProviderUpsertMode::Manual).unwrap();

        assert_eq!(added.kind, ProviderUpsertKind::Added);
        assert_eq!(merged.kind, ProviderUpsertKind::Merged);
        assert_eq!(merged.provider.id, "profile-first");
        assert_eq!(provider_count(&conn), 1);
    }

    #[test]
    fn manual_profiles_keep_different_keys_for_the_same_model() {
        let conn = test_connection();
        let first = provider("profile-first", "Same API", Some("test-key-first"));
        let second = provider("profile-second", "Same API", Some("test-key-second"));

        upsert_provider_on_connection(&conn, first, ProviderUpsertMode::Manual).unwrap();
        upsert_provider_on_connection(&conn, second, ProviderUpsertMode::Manual).unwrap();

        assert_eq!(provider_count(&conn), 2);
    }

    #[test]
    fn manual_profiles_keep_different_wire_apis() {
        let conn = test_connection();
        let first = provider("responses-profile", "Responses", Some("test-key-shared"));
        let mut second = provider("chat-profile", "Chat", Some("test-key-shared"));
        second.wire_api = "chat".to_string();

        upsert_provider_on_connection(&conn, first, ProviderUpsertMode::Manual).unwrap();
        upsert_provider_on_connection(&conn, second, ProviderUpsertMode::Manual).unwrap();

        assert_eq!(provider_count(&conn), 2);
    }

    #[test]
    fn manual_profiles_keep_different_auth_requirements() {
        let conn = test_connection();
        let first = provider("auth-profile", "Auth", Some("test-key-shared"));
        let mut second = provider("no-auth-profile", "No Auth", Some("test-key-shared"));
        second.requires_openai_auth = false;

        upsert_provider_on_connection(&conn, first, ProviderUpsertMode::Manual).unwrap();
        upsert_provider_on_connection(&conn, second, ProviderUpsertMode::Manual).unwrap();

        assert_eq!(provider_count(&conn), 2);
    }

    #[test]
    fn live_provider_profile_matches_only_its_model() {
        let mut gpt = provider("waibi-gpt", "Waibi GPT", Some("test-key-shared"));
        gpt.model = "gpt-5.6".to_string();
        let mut deepseek = provider("waibi-deepseek", "Waibi DeepSeek", Some("test-key-shared"));
        deepseek.model = "deepseek-v3".to_string();

        let mut live_gpt = gpt.clone();
        live_gpt.id = "custom".to_string();
        live_gpt.provider_name = "Waibi GPT".to_string();
        assert_eq!(
            unique_saved_provider_id_for_live(&live_gpt, &[gpt.clone(), deepseek.clone()]),
            Some("waibi-gpt".to_string())
        );

        let mut live_deepseek = deepseek.clone();
        live_deepseek.id = "custom".to_string();
        live_deepseek.provider_name = "Waibi DeepSeek".to_string();
        assert_eq!(
            unique_saved_provider_id_for_live(&live_deepseek, &[gpt, deepseek]),
            Some("waibi-deepseek".to_string())
        );
    }

    #[test]
    fn legacy_cleanup_keeps_same_credentials_with_different_models() {
        let conn = test_connection();
        let mut gpt = provider("waibi-gpt", "Waibi", Some("test-key-shared"));
        gpt.model = "gpt-5.6".to_string();
        let mut deepseek = provider("waibi-deepseek", "Waibi", Some("test-key-shared"));
        deepseek.model = "deepseek-v3".to_string();
        write_provider_on_connection(&conn, &gpt).unwrap();
        write_provider_on_connection(&conn, &deepseek).unwrap();

        assert_eq!(
            consolidate_legacy_provider_duplicates_on_connection(&conn).unwrap(),
            0
        );
        assert_eq!(provider_count(&conn), 2);
    }

    #[test]
    fn editing_model_updates_the_requested_provider_id() {
        let conn = test_connection();
        let original = provider("waibi-gpt", "Waibi GPT", Some("test-key-shared"));
        upsert_provider_on_connection(&conn, original, ProviderUpsertMode::Manual).unwrap();

        let mut edited = provider("waibi-gpt", "Waibi GPT", Some("test-key-shared"));
        edited.model = "gpt-5.6".to_string();
        let result =
            upsert_provider_on_connection(&conn, edited, ProviderUpsertMode::Manual).unwrap();

        assert_eq!(result.kind, ProviderUpsertKind::Updated);
        assert_eq!(result.provider.id, "waibi-gpt");
        assert_eq!(result.provider.model, "gpt-5.6");
        assert_eq!(provider_count(&conn), 1);
    }

    #[test]
    fn repeated_ccswitch_source_updates_in_place_after_profile_change() {
        let conn = test_connection();
        let initial = provider("cc-row", "CC GPT", Some("test-key-shared"));
        upsert_ccswitch_provider_on_connection(&conn, initial, "stable-source").unwrap();

        let mut changed = provider("cc-row", "CC DeepSeek", Some("test-key-shared"));
        changed.model = "deepseek-v3".to_string();
        changed.wire_api = "chat".to_string();
        let result =
            upsert_ccswitch_provider_on_connection(&conn, changed, "stable-source").unwrap();

        assert_eq!(result.kind, ProviderUpsertKind::Updated);
        assert_eq!(result.provider.id, "cc-row");
        assert_eq!(result.provider.model, "deepseek-v3");
        assert_eq!(result.provider.wire_api, "chat");
        assert_eq!(provider_count(&conn), 1);
    }

    #[test]
    fn provider_upsert_deduplicates_safe_matches_and_keeps_distinct_credentials() {
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
        let merged = upsert_ccswitch_provider_on_connection(
            &compatible,
            provider("cc-import", " same   api ", Some("sk-imported")),
            "compatible-row",
        )
        .unwrap();
        assert_eq!(merged.kind, ProviderUpsertKind::Merged);
        assert_eq!(merged.provider.id, "local");
        assert_eq!(merged.provider.api_key.as_deref(), Some("sk-imported"));
        assert_eq!(provider_count(&compatible), 1);

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
    fn stable_import_collision_keeps_manual_id_and_applies_source_values() {
        let conn = test_connection();
        let mut imported = provider("cc-old", "Imported old", Some("sk-old"));
        imported.base_url = "https://old.example.com/v1".to_string();
        upsert_ccswitch_provider_on_connection(&conn, imported, "cc-row")
            .expect("seed stable imported provider");

        let mut manual = provider("manual-local", "Locally edited", Some("sk-shared"));
        manual.base_url = "https://shared.example.com/v1".to_string();
        manual.model = "local-model".to_string();
        manual.toml_config = Some("model = \"local-model\"".to_string());
        upsert_provider_on_connection(&conn, manual, ProviderUpsertMode::Manual)
            .expect("seed manual provider");

        let mut changed = provider("cc-old", "Imported changed", Some("sk-shared"));
        changed.base_url = "https://shared.example.com/v1".to_string();
        changed.model = "local-model".to_string();
        let result = upsert_ccswitch_provider_on_connection(&conn, changed, "cc-row")
            .expect("merge source update into manual record");

        assert_eq!(result.kind, ProviderUpsertKind::Merged);
        assert_eq!(result.provider.id, "manual-local");
        assert_eq!(result.provider.provider_name, "Imported changed");
        assert_eq!(result.provider.model, "local-model");
        assert_eq!(
            result.provider.toml_config.as_deref(),
            Some("model = \"local-model\"")
        );
        let rows = stored_providers_on_connection(&conn).expect("read merged providers");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, CCSWITCH_LOCAL_PROVIDER_SOURCE);
        assert_eq!(rows[0].source_id.as_deref(), Some("cc-row"));
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
        assert_eq!(rows[0].source, CCSWITCH_LOCAL_PROVIDER_SOURCE);
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
    fn historical_custom_ghost_cleanup_requires_one_stable_endpoint_match() {
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
            1
        );
        let rows = list_saved_providers_on_connection(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "local");
        assert_eq!(rows[0].api_key.as_deref(), Some("sk-current"));

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
    }

    #[test]
    fn legacy_consolidation_keeps_local_id_but_next_import_is_authoritative() {
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
            1
        );
        let rows = stored_providers_on_connection(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider.id, "manual-local");
        assert_eq!(rows[0].provider.provider_name, "Edited locally");
        assert_eq!(rows[0].provider.model, "latest-model");
        assert_eq!(
            rows[0].provider.toml_config.as_deref(),
            Some("model = \"latest-model\"")
        );
        assert_eq!(rows[0].source, CCSWITCH_LOCAL_PROVIDER_SOURCE);
        assert_eq!(rows[0].source_id.as_deref(), Some("cc-row"));

        let mut repeated = provider("imported-old", "Remote replacement", Some("sk-same"));
        repeated.model = "remote-model".to_string();
        repeated.toml_config = Some("model = \"remote-model\"".to_string());
        upsert_ccswitch_provider_on_connection(&conn, repeated, "cc-row")
            .expect("repeat import after legacy consolidation");

        let rows = stored_providers_on_connection(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider.id, "manual-local");
        assert_eq!(rows[0].provider.provider_name, "Remote replacement");
        assert_eq!(rows[0].provider.model, "remote-model");
        assert_eq!(
            rows[0].provider.toml_config.as_deref(),
            Some("model = \"remote-model\"")
        );
        assert_eq!(rows[0].source, CCSWITCH_LOCAL_PROVIDER_SOURCE);
        assert_eq!(rows[0].source_id.as_deref(), Some("cc-row"));
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
    fn live_custom_matches_only_one_stable_provider_without_trusting_a_stale_key() {
        let stable = provider("stable", "Same API", Some("sk-current"));
        let persisted_ghost = provider("custom", "Same API", Some("sk-old-live"));
        let live = provider("custom", " same   api ", Some("sk-old-live"));
        assert_eq!(
            unique_saved_provider_id_for_live(&live, &[stable.clone(), persisted_ghost]),
            Some("stable".to_string())
        );

        let second = provider("second", "Same API", Some("sk-second"));
        assert_eq!(
            unique_saved_provider_id_for_live(&live, &[stable, second]),
            None
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
        assert_eq!(normalized.wire_api, "chat_completions");
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
            Some("chat_completions")
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
