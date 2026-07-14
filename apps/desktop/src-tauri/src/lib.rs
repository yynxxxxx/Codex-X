use chrono::Local;
use rusqlite::{params, Connection, OpenFlags, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};
use thiserror::Error;

mod constants;
mod platform;

use constants::*;
use toml_edit::{value, DocumentMut, Item, Table};

#[derive(Debug, Error)]
enum CodexxError {
    #[error("无法获取用户主目录")]
    NoHomeDir,
    #[error("IO error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("TOML parse error at {path}: {message}")]
    Toml { path: String, message: String },
    #[error("JSON error at {path}: {source}")]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("配置错误: {0}")]
    Config(String),
    #[error("SQLite error: {0}")]
    Database(String),
}

type Result<T> = std::result::Result<T, CodexxError>;

static BUILTIN_PROMPT_CACHE_LOCK: Mutex<()> = Mutex::new(());

impl serde::Serialize for CodexxError {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderSummary {
    id: String,
    name: Option<String>,
    base_url: Option<String>,
    wire_api: Option<String>,
    requires_openai_auth: Option<bool>,
    is_current: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexState {
    codex_dir: String,
    config_path: String,
    auth_path: String,
    config_exists: bool,
    auth_exists: bool,
    official_auth_available: bool,
    model: Option<String>,
    model_provider: Option<String>,
    instruction_file: Option<String>,
    instruction_enabled: bool,
    instruction_injection_mode: Option<String>,
    instruction_template_key: Option<String>,
    agents_path: String,
    active_saved_provider_id: Option<String>,
    providers: Vec<ProviderSummary>,
    config_text: String,
    auth_preview: Option<Value>,
    auth_text: String,
    last_backup: Option<BackupEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackupMeta {
    id: String,
    action: String,
    created_at: String,
    codex_dir: String,
    config_path: String,
    auth_path: String,
    had_config: bool,
    had_auth: bool,
    #[serde(default)]
    agents_path: String,
    #[serde(default)]
    had_agents: bool,
    #[serde(default)]
    tracks_agents: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupEntry {
    id: String,
    action: String,
    created_at: String,
    path: String,
    had_config: bool,
    had_auth: bool,
    had_agents: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderInput {
    config_dir: Option<String>,
    #[serde(rename = "providerId")]
    _provider_id: Option<String>,
    provider_name: String,
    base_url: String,
    model: String,
    api_key: Option<String>,
    wire_api: Option<String>,
    requires_openai_auth: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderTomlInput {
    config_dir: Option<String>,
    config_text: String,
    api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderConnectionResult {
    ok: bool,
    status: Option<u16>,
    message: String,
    duration_ms: u128,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OfficialConfigInput {
    config_dir: Option<String>,
    model: Option<String>,
    auth_json: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedProvider {
    id: String,
    provider_name: String,
    base_url: String,
    model: String,
    api_key: Option<String>,
    toml_config: Option<String>,
    wire_api: String,
    requires_openai_auth: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedPrompt {
    id: String,
    title: String,
    filename: String,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BuiltinPromptStatus {
    id: String,
    filename: String,
    title: String,
    subtitle: String,
    badge: String,
    source_url: String,
    cached: bool,
    updated: bool,
    content_source: String,
    checked_at: Option<String>,
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptInjectionMode {
    Replace,
    Append,
}

impl PromptInjectionMode {
    fn parse(value: Option<&str>) -> Result<Self> {
        match value
            .unwrap_or("replace")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "replace" | "model" => Ok(Self::Replace),
            "append" | "agents" => Ok(Self::Append),
            other => Err(CodexxError::Config(format!("未知提示词注入模式: {other}"))),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportResult {
    imported: usize,
    added: usize,
    updated: usize,
    merged: usize,
    skipped: usize,
    warnings: Vec<String>,
    providers: Vec<SavedProvider>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OfficialAuthCandidate {
    auth_json: String,
    model: Option<String>,
    source: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ActionResult {
    ok: bool,
    message: String,
    backup_id: Option<String>,
    state: CodexState,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AboutInfo {
    app_version: String,
    codex_version: Option<String>,
    codex_dir: String,
    project_url: String,
    github_repo: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticItem {
    key: String,
    label: String,
    path: Option<String>,
    status: String,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StartupDiagnostics {
    codex_dir: String,
    needs_manual_select: bool,
    summary: String,
    items: Vec<DiagnosticItem>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelectedSessionSyncInput {
    config_dir: Option<String>,
    target_provider: Option<String>,
    session_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionDeleteInput {
    config_dir: Option<String>,
    session_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionPreview {
    id: String,
    title: String,
    model_provider: Option<String>,
    model: Option<String>,
    cwd: Option<String>,
    rollout_path: Option<String>,
    updated_at_ms: Option<i64>,
    archived: bool,
    has_user_event: bool,
    needs_sync: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionSyncStatus {
    codex_dir: String,
    target_provider: String,
    rollout_files: usize,
    session_meta_count: usize,
    mismatched_rollouts: usize,
    mismatched_session_meta: usize,
    sqlite_dbs: usize,
    sqlite_threads: usize,
    top_level_threads: usize,
    subagent_threads: usize,
    mismatched_threads: usize,
    needs_sync: bool,
    backup_dir: Option<String>,
    warnings: Vec<String>,
    sessions: Vec<SessionPreview>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionSyncResult {
    status: SessionSyncStatus,
    updated_rollouts: usize,
    updated_threads: usize,
    backup_dir: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionDeleteResult {
    status: SessionSyncStatus,
    requested_sessions: usize,
    deleted_sessions: usize,
    failed_sessions: usize,
    failure_message: Option<String>,
    deleted_thread_rows: usize,
    deleted_rollout_files: usize,
    deleted_related_rows: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManagedMcpServer {
    id: String,
    name: String,
    transport: String,
    enabled: bool,
    source: String,
    summary: String,
    command: Option<String>,
    url: Option<String>,
    config_json: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManagedSkill {
    id: String,
    name: String,
    description: Option<String>,
    directory: String,
    enabled: bool,
    source: String,
    path: String,
    content_hash: Option<String>,
    update_status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillsMcpState {
    codex_dir: String,
    codex_skills_dir: String,
    disabled_skills_dir: String,
    skills: Vec<ManagedSkill>,
    mcp_servers: Vec<ManagedMcpServer>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillsMcpActionResult {
    imported_skills: usize,
    imported_mcp: usize,
    message: String,
    state: SkillsMcpState,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillsMcpImportPreview {
    skills: Vec<ManagedSkill>,
    mcp_servers: Vec<ManagedMcpServer>,
    warnings: Vec<String>,
}

#[derive(Debug, Default)]
struct RolloutScan {
    rollout_files: usize,
    session_meta_count: usize,
    mismatched_rollouts: usize,
    mismatched_session_meta: usize,
    changed_files: Vec<(PathBuf, String)>,
    thread_ids_with_user_events: HashSet<String>,
    cwd_by_thread_id: HashMap<String, String>,
    warnings: Vec<String>,
}

#[derive(Debug, Default)]
struct SqliteScan {
    sqlite_dbs: usize,
    sqlite_threads: usize,
    top_level_threads: usize,
    subagent_threads: usize,
    mismatched_threads: usize,
    warnings: Vec<String>,
}

fn sql_select_column(cols: &HashSet<String>, name: &str, fallback: &str) -> String {
    if cols.contains(name) {
        format!("\"{}\"", name.replace('"', "\"\""))
    } else {
        fallback.to_string()
    }
}

fn io_err(path: &Path, source: std::io::Error) -> CodexxError {
    CodexxError::Io {
        path: path.display().to_string(),
        source,
    }
}

fn json_err(path: &Path, source: serde_json::Error) -> CodexxError {
    CodexxError::Json {
        path: path.display().to_string(),
        source,
    }
}

fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().ok_or(CodexxError::NoHomeDir)
}

fn app_home() -> Result<PathBuf> {
    #[cfg(test)]
    {
        use std::sync::OnceLock;
        static TEST_APP_HOME: OnceLock<PathBuf> = OnceLock::new();
        Ok(TEST_APP_HOME
            .get_or_init(|| {
                std::env::temp_dir().join(format!(
                    "codex-x-test-home-{}-{}",
                    std::process::id(),
                    Local::now().timestamp_nanos_opt().unwrap_or_default()
                ))
            })
            .clone())
    }
    #[cfg(not(test))]
    {
        if let Ok(value) = std::env::var("CODEXX_HOME") {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Ok(PathBuf::from(trimmed));
            }
        }
        Ok(home_dir()?.join(".codexx"))
    }
}

fn db_path() -> Result<PathBuf> {
    Ok(app_home()?.join("codexx.db"))
}

fn ensure_sqlite_column(
    conn: &Connection,
    table: &str,
    column: &str,
    alter_sql: &str,
) -> Result<()> {
    let cols = table_column_set(conn, table)?;
    if cols.contains(column) {
        return Ok(());
    }
    match conn.execute(alter_sql, []) {
        Ok(_) => Ok(()),
        Err(e) => {
            let message = e.to_string().to_ascii_lowercase();
            if message.contains("duplicate column") || message.contains("duplicate column name") {
                // Another running Codex-X process may have applied the same
                // lightweight migration between our PRAGMA check and ALTER.
                Ok(())
            } else {
                Err(CodexxError::Database(e.to_string()))
            }
        }
    }
}

fn open_db() -> Result<Connection> {
    let path = db_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    }
    let mut conn = Connection::open(&path).map_err(|e| CodexxError::Database(e.to_string()))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS providers (
            id TEXT PRIMARY KEY,
            provider_name TEXT NOT NULL,
            base_url TEXT NOT NULL,
            model TEXT NOT NULL,
            api_key TEXT,
            toml_config TEXT,
            wire_api TEXT NOT NULL DEFAULT 'responses',
            requires_openai_auth INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_providers_updated_at ON providers(updated_at DESC);
        CREATE TABLE IF NOT EXISTS prompts (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            filename TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_prompts_updated_at ON prompts(updated_at DESC);
        CREATE TABLE IF NOT EXISTS builtin_prompt_cache (
            id TEXT PRIMARY KEY,
            filename TEXT NOT NULL,
            source_url TEXT NOT NULL,
            content TEXT NOT NULL,
            checked_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS managed_mcp_servers (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            server_config TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS managed_skills (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT,
            directory TEXT NOT NULL,
            source_path TEXT,
            content_hash TEXT,
            enabled INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT NOT NULL
        );",
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    ensure_sqlite_column(
        &conn,
        "providers",
        "toml_config",
        "ALTER TABLE providers ADD COLUMN toml_config TEXT",
    )?;
    conn.execute(
        "DELETE FROM prompts
         WHERE id LIKE 'external-%'
           AND EXISTS (
             SELECT 1 FROM prompts AS kept
             WHERE lower(kept.filename) = lower(prompts.filename)
               AND kept.id NOT LIKE 'external-%'
           )",
        [],
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    conn.execute(
        "DELETE FROM prompts
         WHERE id LIKE 'external-%'
           AND EXISTS (
             SELECT 1 FROM prompts AS kept
             WHERE kept.content = prompts.content
               AND kept.id NOT LIKE 'external-%'
           )",
        [],
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    conn.execute(
        "DELETE FROM prompts
         WHERE id LIKE 'external-%'
           AND EXISTS (
             SELECT 1 FROM prompts AS kept
             WHERE kept.content = prompts.content
               AND kept.id LIKE 'external-%'
               AND kept.rowid <> prompts.rowid
               AND (kept.updated_at > prompts.updated_at OR (kept.updated_at = prompts.updated_at AND kept.rowid > prompts.rowid))
           )",
        [],
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    merge_duplicate_provider_identities(&mut conn)?;
    Ok(conn)
}

fn now_rfc3339() -> String {
    Local::now().to_rfc3339()
}

#[derive(Debug, Clone)]
struct StoredProvider {
    provider: SavedProvider,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ProviderIdentity {
    Credential([u8; 32]),
    Unauthenticated { base_url: String, name: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderUpsertMode {
    Manual,
    Imported,
    Detected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderUpsertKind {
    Added,
    Updated,
    Merged,
}

#[derive(Debug, Clone)]
struct ProviderUpsertResult {
    provider: SavedProvider,
    kind: ProviderUpsertKind,
}

fn canonical_provider_base_url(input: &str) -> String {
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

fn provider_identity(provider: &SavedProvider) -> Option<ProviderIdentity> {
    use sha2::{Digest, Sha256};

    let base_url = canonical_provider_base_url(&provider.base_url);
    if base_url.is_empty() {
        return None;
    }
    let explicit_api_key = provider
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let toml_api_key = provider.toml_config.as_deref().and_then(|text| {
        let doc = text.parse::<DocumentMut>().ok()?;
        let provider_id = doc
            .get("model_provider")
            .and_then(|item| item.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty());
        experimental_bearer_token_from_doc(&doc, provider_id)
    });
    if let Some(api_key) = explicit_api_key.or(toml_api_key.as_deref()) {
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

    let name = provider
        .provider_name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    (!name.is_empty()).then_some(ProviderIdentity::Unauthenticated { base_url, name })
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
                    requires_openai_auth, created_at, updated_at
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
            })
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;

    let mut providers = Vec::new();
    for row in rows {
        providers.push(row.map_err(|e| CodexxError::Database(e.to_string()))?);
    }
    Ok(providers)
}

fn list_saved_providers_on_connection(conn: &Connection) -> Result<Vec<SavedProvider>> {
    Ok(stored_providers_on_connection(conn)?
        .into_iter()
        .map(|stored| stored.provider)
        .collect())
}

fn list_saved_providers_inner() -> Result<Vec<SavedProvider>> {
    let conn = open_db()?;
    list_saved_providers_on_connection(&conn)
}

fn provider_by_id_on_connection(conn: &Connection, id: &str) -> Result<Option<SavedProvider>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, provider_name, base_url, model, api_key, toml_config, wire_api,
                    requires_openai_auth
             FROM providers WHERE id = ?1 LIMIT 1",
        )
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    stmt.query_row([id], saved_provider_from_row)
        .map(Some)
        .or_else(|error| {
            if matches!(error, rusqlite::Error::QueryReturnedNoRows) {
                Ok(None)
            } else {
                Err(error)
            }
        })
        .map_err(|e| CodexxError::Database(e.to_string()))
}

fn write_provider_on_connection(conn: &Connection, provider: &SavedProvider) -> Result<()> {
    let now = now_rfc3339();
    conn.execute(
        "INSERT INTO providers
            (id, provider_name, base_url, model, api_key, toml_config, wire_api, requires_openai_auth, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
         ON CONFLICT(id) DO UPDATE SET
            provider_name = excluded.provider_name,
            base_url = excluded.base_url,
            model = excluded.model,
            api_key = excluded.api_key,
            toml_config = excluded.toml_config,
            wire_api = excluded.wire_api,
            requires_openai_auth = excluded.requires_openai_auth,
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
            now,
        ],
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    Ok(())
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

fn preserve_existing_provider_config(
    mut incoming: SavedProvider,
    existing: &SavedProvider,
) -> SavedProvider {
    incoming.id = existing.id.clone();
    if !existing.provider_name.trim().is_empty() {
        incoming.provider_name = existing.provider_name.clone();
    }
    if !existing.base_url.trim().is_empty() {
        incoming.base_url = existing.base_url.clone();
    }
    if !existing.model.trim().is_empty() {
        incoming.model = existing.model.clone();
    }
    if existing
        .api_key
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        incoming.api_key = existing.api_key.clone();
    }
    if existing
        .toml_config
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        incoming.toml_config = existing.toml_config.clone();
    }
    if !existing.wire_api.trim().is_empty() {
        incoming.wire_api = existing.wire_api.clone();
    }
    incoming.requires_openai_auth = existing.requires_openai_auth;
    incoming
}

fn upsert_provider_on_connection(
    conn: &Connection,
    mut provider: SavedProvider,
    mode: ProviderUpsertMode,
) -> Result<ProviderUpsertResult> {
    let requested_id = provider.id.clone();
    let identity = provider_identity(&provider);
    let stored = stored_providers_on_connection(conn)?;
    let identity_match = identity.as_ref().and_then(|identity| {
        stored
            .iter()
            .find(|candidate| provider_identity(&candidate.provider).as_ref() == Some(identity))
    });
    let exact_id_match = stored
        .iter()
        .find(|candidate| candidate.provider.id == requested_id);

    let target = match mode {
        ProviderUpsertMode::Manual => exact_id_match.or(identity_match),
        ProviderUpsertMode::Imported | ProviderUpsertMode::Detected => identity_match,
    };
    let kind = if let Some(target) = target {
        let existing = &target.provider;
        let same_id = existing.id == requested_id;
        provider.id = existing.id.clone();
        if mode != ProviderUpsertMode::Manual {
            provider = preserve_existing_provider_config(provider, existing);
        }
        if same_id {
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

    write_provider_on_connection(conn, &provider)?;
    let provider = provider_by_id_on_connection(conn, &provider.id)?
        .ok_or_else(|| CodexxError::Database("provider saved but not found".to_string()))?;
    Ok(ProviderUpsertResult { provider, kind })
}

fn merge_duplicate_provider_identities(conn: &mut Connection) -> Result<usize> {
    let rows = stored_providers_on_connection(conn)?;
    let mut groups: HashMap<ProviderIdentity, Vec<StoredProvider>> = HashMap::new();
    for row in rows {
        if let Some(identity @ ProviderIdentity::Credential(_)) = provider_identity(&row.provider) {
            groups.entry(identity).or_default().push(row);
        }
    }
    let duplicate_groups = groups
        .into_values()
        .filter(|group| group.len() > 1)
        .collect::<Vec<_>>();
    if duplicate_groups.is_empty() {
        return Ok(0);
    }

    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let mut merged = 0usize;
    for mut group in duplicate_groups {
        group.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
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
            if duplicate.updated_at > survivor.updated_at {
                survivor.updated_at = duplicate.updated_at.clone();
            }
        }
        survivor.provider.base_url = canonical_provider_base_url(&survivor.provider.base_url);
        survivor.provider.api_key = survivor
            .provider
            .api_key
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        transaction
            .execute(
                "UPDATE providers SET provider_name = ?2, base_url = ?3, model = ?4,
                        api_key = ?5, toml_config = ?6, wire_api = ?7,
                        requires_openai_auth = ?8, updated_at = ?9
                 WHERE id = ?1",
                params![
                    survivor.provider.id,
                    survivor.provider.provider_name,
                    survivor.provider.base_url,
                    survivor.provider.model,
                    survivor.provider.api_key,
                    survivor.provider.toml_config,
                    survivor.provider.wire_api,
                    if survivor.provider.requires_openai_auth {
                        1
                    } else {
                        0
                    },
                    survivor.updated_at,
                ],
            )
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        for duplicate in group.iter().skip(1) {
            transaction
                .execute(
                    "DELETE FROM providers WHERE id = ?1",
                    [&duplicate.provider.id],
                )
                .map_err(|e| CodexxError::Database(e.to_string()))?;
            merged += 1;
        }
    }
    transaction
        .commit()
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    Ok(merged)
}

fn normalize_saved_provider(provider: SavedProvider) -> Result<SavedProvider> {
    let raw_id = provider.id.trim();
    if raw_id.is_empty() {
        return Err(CodexxError::Config("provider id 不能为空".to_string()));
    }
    let normalized = SavedProvider {
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
    Ok(normalized)
}

fn save_manual_provider_on_connection(
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

fn save_provider_inner(provider: SavedProvider) -> Result<SavedProvider> {
    let conn = open_db()?;
    save_manual_provider_on_connection(&conn, provider)
}

fn save_detected_provider_inner(provider: SavedProvider) -> Result<SavedProvider> {
    let provider = normalize_saved_provider(provider)?;
    let conn = open_db()?;
    Ok(upsert_provider_on_connection(&conn, provider, ProviderUpsertMode::Detected)?.provider)
}

fn delete_provider_on_connection(conn: &Connection, id: &str) -> Result<()> {
    let id = id.trim();
    if id.is_empty() {
        return Err(CodexxError::Config("供应商 ID 不能为空".to_string()));
    }
    let deleted = conn
        .execute("DELETE FROM providers WHERE id = ?1", params![id])
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    if deleted == 0 {
        return Err(CodexxError::Config(format!("供应商不存在或已删除: {id}")));
    }
    Ok(())
}

fn delete_provider_inner(id: &str) -> Result<()> {
    let conn = open_db()?;
    delete_provider_on_connection(&conn, id)
}

fn normalize_prompt_filename(input: &str, fallback: &str) -> String {
    let raw = input.trim().trim_end_matches(".md");
    let base = if raw.is_empty() { fallback } else { raw };
    let mut out = String::new();
    let mut last_dash = false;
    for ch in base.to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let out = out.trim_matches('-');
    format!("{}.md", if out.is_empty() { "custom-prompt" } else { out })
}

fn canonical_prompt_content(input: &str) -> String {
    input
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_string()
}

fn list_saved_prompts_inner() -> Result<Vec<SavedPrompt>> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare("SELECT id, title, filename, content FROM prompts ORDER BY updated_at DESC, created_at DESC")
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SavedPrompt {
                id: row.get(0)?,
                title: row.get(1)?,
                filename: row.get(2)?,
                content: row.get(3)?,
            })
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let mut prompts = Vec::new();
    for row in rows {
        let prompt = row.map_err(|e| CodexxError::Database(e.to_string()))?;
        let filename_key = prompt.filename.to_ascii_lowercase();
        let content_key = canonical_prompt_content(&prompt.content);
        let duplicate_index = prompts.iter().position(|existing: &SavedPrompt| {
            existing.filename.to_ascii_lowercase() == filename_key
                || (canonical_prompt_content(&existing.content) == content_key
                    && (existing.id.starts_with("external-") || prompt.id.starts_with("external-")))
        });
        if let Some(index) = duplicate_index {
            let existing_is_external = prompts[index].id.starts_with("external-");
            let prompt_is_external = prompt.id.starts_with("external-");
            if existing_is_external && !prompt_is_external {
                prompts[index] = prompt;
            }
            continue;
        }
        prompts.push(prompt);
    }
    Ok(prompts)
}

fn save_prompt_inner(prompt: SavedPrompt) -> Result<SavedPrompt> {
    let conn = open_db()?;
    let now = now_rfc3339();
    conn.execute(
        "INSERT INTO prompts (id, title, filename, content, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)
         ON CONFLICT(id) DO UPDATE SET
            title = excluded.title,
            filename = excluded.filename,
            content = excluded.content,
            updated_at = excluded.updated_at",
        params![
            prompt.id,
            prompt.title,
            prompt.filename,
            prompt.content,
            now
        ],
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    list_saved_prompts_inner()?
        .into_iter()
        .find(|p| p.id == prompt.id)
        .ok_or_else(|| CodexxError::Database("prompt saved but not found".to_string()))
}

fn get_saved_prompt_inner(id: &str) -> Result<SavedPrompt> {
    list_saved_prompts_inner()?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| CodexxError::Config(format!("提示词不存在: {id}")))
}

fn delete_prompt_inner(id: &str) -> Result<()> {
    let conn = open_db()?;
    conn.execute("DELETE FROM prompts WHERE id = ?1", params![id])
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    Ok(())
}

fn find_saved_prompt_by_content(content: &str) -> Result<Option<SavedPrompt>> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare("SELECT id, title, filename, content FROM prompts ORDER BY CASE WHEN id LIKE 'external-%' THEN 1 ELSE 0 END, updated_at DESC, created_at DESC")
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SavedPrompt {
                id: row.get(0)?,
                title: row.get(1)?,
                filename: row.get(2)?,
                content: row.get(3)?,
            })
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let target = canonical_prompt_content(content);
    for row in rows {
        let prompt = row.map_err(|e| CodexxError::Database(e.to_string()))?;
        if canonical_prompt_content(&prompt.content) == target {
            return Ok(Some(prompt));
        }
    }
    Ok(None)
}

fn find_saved_prompt_by_current_file(filename: &str, content: &str) -> Result<Option<SavedPrompt>> {
    if let Some(prompt) = find_saved_prompt_by_content(content)? {
        return Ok(Some(prompt));
    }
    let normalized_filename = normalize_prompt_filename(filename, "external-prompt");
    let conn = open_db()?;
    let mut stmt = conn
        .prepare(
            "SELECT id, title, filename, content FROM prompts
             WHERE lower(filename) = lower(?1)
             ORDER BY CASE WHEN id LIKE 'external-%' THEN 1 ELSE 0 END, updated_at DESC, created_at DESC
             LIMIT 1",
        )
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    match stmt.query_row([normalized_filename], |row| {
        Ok(SavedPrompt {
            id: row.get(0)?,
            title: row.get(1)?,
            filename: row.get(2)?,
            content: row.get(3)?,
        })
    }) {
        Ok(mut prompt) => {
            if canonical_prompt_content(&prompt.content) != canonical_prompt_content(content) {
                prompt.content = content.to_string();
            }
            Ok(Some(prompt))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(CodexxError::Database(e.to_string())),
    }
}

#[derive(Debug, Clone, Copy)]
struct BundledPromptMeta {
    id: &'static str,
    filename: &'static str,
    title: &'static str,
    subtitle: &'static str,
    badge: &'static str,
    content: &'static str,
}

#[derive(Debug, Clone)]
struct CachedBuiltinPrompt {
    id: String,
    filename: String,
    source_url: String,
    content: String,
    checked_at: String,
}

#[derive(Debug, Deserialize)]
struct GithubContentEntry {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    download_url: Option<String>,
}

fn bundled_prompt_metas() -> [BundledPromptMeta; 3] {
    [
        BundledPromptMeta {
            id: "gpt5.5-unrestricted",
            filename: INSTRUCTION_FILENAME,
            title: "gpt-5.5 unrestricted 破甲",
            subtitle: "方法：先让ai分析项目，分析完之后发【不直白的逆向】命令",
            badge: "推荐",
            content: INSTRUCTION_CONTENT,
        },
        BundledPromptMeta {
            id: "gpt5.4-unrestricted",
            filename: INSTRUCTION_54_FILENAME,
            title: "gpt-5.4 unrestricted 破甲",
            subtitle: "方法：先让ai分析项目，分析完之后发【不直白的逆向】命令",
            badge: "兼容",
            content: INSTRUCTION_54_CONTENT,
        },
        BundledPromptMeta {
            id: "gpt5.5-jeli",
            filename: INSTRUCTION_JELI_FILENAME,
            title: "gpt5.5-jeli.md",
            subtitle: "gpt5.5 大白话（80%场景）破甲",
            badge: "通用",
            content: INSTRUCTION_JELI_CONTENT,
        },
    ]
}

fn bundled_prompt_meta(template_id: &str) -> Option<BundledPromptMeta> {
    let id = if template_id.trim().is_empty() {
        "gpt5.5-unrestricted"
    } else {
        template_id.trim()
    };
    bundled_prompt_metas()
        .into_iter()
        .find(|item| item.id == id)
}

fn stable_remote_prompt_id(filename: &str) -> String {
    if let Some(meta) = bundled_prompt_metas()
        .into_iter()
        .find(|item| item.filename.eq_ignore_ascii_case(filename))
    {
        return meta.id.to_string();
    }
    use sha2::{Digest, Sha256};
    let normalized_filename = filename.to_ascii_lowercase();
    let stem = normalized_filename
        .strip_suffix(".md")
        .unwrap_or(&normalized_filename);
    let slug = normalize_prompt_filename(stem, "remote-prompt")
        .trim_end_matches(".md")
        .to_string();
    let digest = Sha256::digest(normalized_filename.as_bytes());
    let suffix = digest[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("github-{slug}-{suffix}")
}

fn prompt_display_meta(filename: &str) -> (String, String, String) {
    if filename.eq_ignore_ascii_case("海鸥3.0破甲.md") {
        return (
            "海鸥3.0破甲.md".to_string(),
            "测试生效：海鸥在线，你要整点薯条吗？".to_string(),
            "远程".to_string(),
        );
    }
    if let Some(meta) = bundled_prompt_metas()
        .into_iter()
        .find(|item| item.filename.eq_ignore_ascii_case(filename))
    {
        return (
            meta.title.to_string(),
            meta.subtitle.to_string(),
            meta.badge.to_string(),
        );
    }
    (
        filename.to_string(),
        "来自 GitHub examples 的远程提示词模板".to_string(),
        "远程".to_string(),
    )
}

fn builtin_prompt_source_url(filename: &str) -> String {
    format!("https://raw.githubusercontent.com/yynxxxxx/Codex-X/main/examples/{filename}")
}

fn cached_builtin_prompt(id: &str) -> Result<Option<CachedBuiltinPrompt>> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare(
            "SELECT id, filename, source_url, content, checked_at
             FROM builtin_prompt_cache WHERE id = ?1",
        )
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    match stmt.query_row([id], |row| {
        Ok(CachedBuiltinPrompt {
            id: row.get(0)?,
            filename: row.get(1)?,
            source_url: row.get(2)?,
            content: row.get(3)?,
            checked_at: row.get(4)?,
        })
    }) {
        Ok(value) => Ok(Some(value)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(CodexxError::Database(e.to_string())),
    }
}

fn cached_builtin_prompts_from_connection(conn: &Connection) -> Result<Vec<CachedBuiltinPrompt>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, filename, source_url, content, checked_at
             FROM builtin_prompt_cache ORDER BY filename COLLATE NOCASE ASC",
        )
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(CachedBuiltinPrompt {
                id: row.get(0)?,
                filename: row.get(1)?,
                source_url: row.get(2)?,
                content: row.get(3)?,
                checked_at: row.get(4)?,
            })
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    rows.map(|row| row.map_err(|e| CodexxError::Database(e.to_string())))
        .collect()
}

fn cached_builtin_prompts() -> Result<Vec<CachedBuiltinPrompt>> {
    let conn = open_db()?;
    cached_builtin_prompts_from_connection(&conn)
}

fn stale_cached_prompt_ids(
    caches: &[CachedBuiltinPrompt],
    active_ids: &HashSet<String>,
) -> Vec<String> {
    caches
        .iter()
        .filter(|cache| !active_ids.contains(&cache.id))
        .map(|cache| cache.id.clone())
        .collect()
}

#[cfg(test)]
fn delete_cached_prompt_ids(conn: &mut Connection, stale_ids: &[String]) -> Result<usize> {
    if stale_ids.is_empty() {
        return Ok(0);
    }

    let transaction = conn
        .transaction()
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let mut deleted = 0;
    for id in stale_ids {
        deleted += transaction
            .execute("DELETE FROM builtin_prompt_cache WHERE id = ?1", [id])
            .map_err(|e| CodexxError::Database(e.to_string()))?;
    }
    transaction
        .commit()
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    Ok(deleted)
}

fn prune_builtin_prompt_cache(active_ids: &HashSet<String>) -> Result<usize> {
    let mut conn = open_db()?;
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let stale_ids = stale_cached_prompt_ids(
        &cached_builtin_prompts_from_connection(&transaction)?,
        active_ids,
    );
    let mut deleted = 0;
    for id in stale_ids {
        deleted += transaction
            .execute("DELETE FROM builtin_prompt_cache WHERE id = ?1", [id])
            .map_err(|e| CodexxError::Database(e.to_string()))?;
    }
    transaction
        .commit()
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    Ok(deleted)
}

fn save_builtin_prompt_cache(
    id: &str,
    filename: &str,
    source_url: &str,
    content: &str,
) -> Result<()> {
    let conn = open_db()?;
    let now = now_rfc3339();
    conn.execute(
        "INSERT INTO builtin_prompt_cache (id, filename, source_url, content, checked_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)
         ON CONFLICT(id) DO UPDATE SET
           filename = excluded.filename,
           source_url = excluded.source_url,
           content = excluded.content,
           checked_at = excluded.checked_at,
           updated_at = CASE WHEN builtin_prompt_cache.content <> excluded.content THEN excluded.updated_at ELSE builtin_prompt_cache.updated_at END",
        params![id, filename, source_url, content, now],
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    Ok(())
}

fn fetch_remote_prompt(source_url: &str) -> Result<String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(5))
        .build();
    let response = agent
        .get(source_url)
        .set("User-Agent", "Codex-X")
        .call()
        .map_err(|e| CodexxError::Config(format!("GitHub 提示词更新失败: {e}")))?;
    let text = response
        .into_string()
        .map_err(|e| CodexxError::Config(format!("读取 GitHub 提示词失败: {e}")))?;
    if text.trim().is_empty() {
        return Err(CodexxError::Config("GitHub 提示词内容为空".to_string()));
    }
    Ok(text)
}

fn fetch_github_prompt_catalog() -> Result<Vec<(String, String, String)>> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(5))
        .build();
    let response = agent
        .get(GITHUB_EXAMPLES_API)
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", "Codex-X")
        .call()
        .map_err(|e| CodexxError::Config(format!("读取 GitHub examples 目录失败: {e}")))?;
    let body = response
        .into_string()
        .map_err(|e| CodexxError::Config(format!("读取 GitHub examples 目录失败: {e}")))?;
    let entries: Vec<GithubContentEntry> = serde_json::from_str(&body)
        .map_err(|e| CodexxError::Config(format!("解析 GitHub examples 目录失败: {e}")))?;
    github_prompt_catalog_from_entries(entries)
}

fn github_prompt_catalog_from_entries(
    entries: Vec<GithubContentEntry>,
) -> Result<Vec<(String, String, String)>> {
    let mut prompts = Vec::new();
    for entry in entries {
        if entry.kind != "file"
            || !entry.name.to_ascii_lowercase().ends_with(".md")
            || entry.name.contains('/')
            || entry.name.contains('\\')
        {
            continue;
        }
        let source_url = entry.download_url.ok_or_else(|| {
            CodexxError::Config(format!("GitHub 模板缺少下载地址: {}", entry.name))
        })?;
        let id = stable_remote_prompt_id(&entry.name);
        prompts.push((id, entry.name, source_url));
    }
    prompts.sort_by(|a, b| a.1.to_ascii_lowercase().cmp(&b.1.to_ascii_lowercase()));
    let mut seen_ids = HashSet::new();
    let mut seen_filenames = HashSet::new();
    prompts.retain(|(id, filename, _)| {
        seen_ids.insert(id.clone()) && seen_filenames.insert(filename.to_ascii_lowercase())
    });
    Ok(prompts)
}

fn prompt_status_from_cache(cache: CachedBuiltinPrompt, message: &str) -> BuiltinPromptStatus {
    let (title, subtitle, badge) = prompt_display_meta(&cache.filename);
    BuiltinPromptStatus {
        id: cache.id,
        filename: cache.filename,
        title,
        subtitle,
        badge,
        source_url: cache.source_url,
        cached: true,
        updated: false,
        content_source: "cache".to_string(),
        checked_at: Some(cache.checked_at),
        message: message.to_string(),
    }
}

fn refresh_builtin_prompt_from_source(
    id: &str,
    filename: &str,
    source_url: &str,
    bundled: Option<&str>,
) -> Result<BuiltinPromptStatus> {
    let cached_before = cached_builtin_prompt(id)?;
    let (title, subtitle, badge) = prompt_display_meta(filename);
    match fetch_remote_prompt(&source_url) {
        Ok(remote) => {
            let updated = cached_before
                .as_ref()
                .map(|cached| cached.content != remote)
                .unwrap_or_else(|| bundled.is_none_or(|content| remote != content));
            save_builtin_prompt_cache(id, filename, &source_url, &remote)?;
            let checked_at = cached_builtin_prompt(id)?.map(|cached| cached.checked_at);
            Ok(BuiltinPromptStatus {
                id: id.to_string(),
                filename: filename.to_string(),
                title,
                subtitle,
                badge,
                source_url: source_url.to_string(),
                cached: true,
                updated,
                content_source: "github".to_string(),
                checked_at,
                message: if updated {
                    "已更新到 GitHub 最新提示词"
                } else {
                    "已是 GitHub 最新提示词"
                }
                .to_string(),
            })
        }
        Err(e) => {
            let cached = cached_before.is_some();
            Ok(BuiltinPromptStatus {
                id: id.to_string(),
                filename: filename.to_string(),
                title,
                subtitle,
                badge,
                source_url: source_url.to_string(),
                cached,
                updated: false,
                content_source: if cached {
                    "cache"
                } else if bundled.is_some() {
                    "bundled"
                } else {
                    "unavailable"
                }
                .to_string(),
                checked_at: cached_before.map(|item| item.checked_at),
                message: format!(
                    "无法连接 GitHub，已使用{}：{}",
                    if cached {
                        "本地缓存"
                    } else if bundled.is_some() {
                        "打包内置版本"
                    } else {
                        "不可用状态"
                    },
                    e
                ),
            })
        }
    }
}

fn bundled_prompt_status(meta: BundledPromptMeta, message: &str) -> BuiltinPromptStatus {
    BuiltinPromptStatus {
        id: meta.id.to_string(),
        filename: meta.filename.to_string(),
        title: meta.title.to_string(),
        subtitle: meta.subtitle.to_string(),
        badge: meta.badge.to_string(),
        source_url: builtin_prompt_source_url(meta.filename),
        cached: false,
        updated: false,
        content_source: "bundled".to_string(),
        checked_at: None,
        message: message.to_string(),
    }
}

fn cached_prompt_fallback_statuses(caches: Vec<CachedBuiltinPrompt>) -> Vec<BuiltinPromptStatus> {
    let cache_map = caches
        .iter()
        .cloned()
        .map(|cache| (cache.id.clone(), cache))
        .collect::<HashMap<_, _>>();
    let mut seen_ids = HashSet::new();
    let mut seen_filenames = HashSet::new();
    let mut statuses = Vec::new();

    for meta in bundled_prompt_metas() {
        let status = cache_map
            .get(meta.id)
            .cloned()
            .map(|cache| prompt_status_from_cache(cache, "使用上次成功同步的 GitHub 缓存"))
            .unwrap_or_else(|| bundled_prompt_status(meta, "使用打包内置版本"));
        seen_ids.insert(status.id.to_ascii_lowercase());
        seen_filenames.insert(status.filename.to_ascii_lowercase());
        statuses.push(status);
    }

    let mut extra_caches = caches;
    extra_caches.sort_by_key(|cache| cache.id != stable_remote_prompt_id(&cache.filename));
    for cache in extra_caches {
        let id = cache.id.to_ascii_lowercase();
        let filename = cache.filename.to_ascii_lowercase();
        if seen_ids.contains(&id) || seen_filenames.contains(&filename) {
            continue;
        }
        seen_ids.insert(id);
        seen_filenames.insert(filename);
        statuses.push(prompt_status_from_cache(
            cache,
            "使用上次成功同步的 GitHub 缓存",
        ));
    }
    statuses
}

fn builtin_prompt_status_inner() -> Result<Vec<BuiltinPromptStatus>> {
    Ok(cached_prompt_fallback_statuses(cached_builtin_prompts()?))
}

fn active_remote_builtin_prompt_id(config_dir: Option<String>) -> Option<String> {
    let codex_dir = resolve_codex_dir(config_dir).ok()?;
    let state = build_state(codex_dir).ok()?;
    let template_key = state.instruction_template_key.as_deref()?;
    let id = template_key.strip_prefix("builtin:")?.trim();
    if id.is_empty() || bundled_prompt_meta(id).is_some() {
        return None;
    }
    Some(id.to_string())
}

fn refresh_builtin_prompts_inner(config_dir: Option<String>) -> Result<Vec<BuiltinPromptStatus>> {
    let _cache_guard = BUILTIN_PROMPT_CACHE_LOCK
        .lock()
        .map_err(|_| CodexxError::Database("提示词缓存锁已损坏".to_string()))?;
    let catalog = match fetch_github_prompt_catalog() {
        Ok(catalog) => catalog,
        Err(error) => {
            let mut statuses = builtin_prompt_status_inner()?;
            for status in &mut statuses {
                status.message = format!("无法读取 GitHub 模板目录，已使用本地内容：{error}");
            }
            return Ok(statuses);
        }
    };
    let remote_ids = catalog
        .iter()
        .map(|(id, _, _)| id.clone())
        .collect::<HashSet<_>>();
    let mut statuses = Vec::new();
    for (id, filename, source_url) in catalog {
        let bundled = bundled_prompt_meta(&id).map(|meta| meta.content);
        statuses.push(refresh_builtin_prompt_from_source(
            &id,
            &filename,
            &source_url,
            bundled,
        )?);
    }
    for meta in bundled_prompt_metas() {
        if !remote_ids.contains(meta.id) {
            statuses.push(bundled_prompt_status(
                meta,
                "GitHub 中未找到该模板，继续使用打包内置版本",
            ));
        }
    }
    let mut retained_ids = remote_ids.clone();
    if let Some(active_id) = active_remote_builtin_prompt_id(config_dir) {
        if !remote_ids.contains(&active_id) {
            if let Some(cache) = cached_builtin_prompt(&active_id)? {
                let mut status = prompt_status_from_cache(
                    cache,
                    "该模板已从 GitHub 目录移除，但当前配置仍在使用",
                );
                status.content_source = "removed".to_string();
                statuses.push(status);
            }
            retained_ids.insert(active_id);
        }
    }
    prune_builtin_prompt_cache(&retained_ids)?;
    let order = bundled_prompt_metas()
        .into_iter()
        .enumerate()
        .map(|(index, meta)| (meta.id, index))
        .collect::<HashMap<_, _>>();
    statuses.sort_by(|a, b| {
        let a_order = order.get(a.id.as_str()).copied().unwrap_or(usize::MAX);
        let b_order = order.get(b.id.as_str()).copied().unwrap_or(usize::MAX);
        a_order.cmp(&b_order).then_with(|| {
            a.filename
                .to_ascii_lowercase()
                .cmp(&b.filename.to_ascii_lowercase())
        })
    });
    Ok(statuses)
}

fn builtin_prompt_content(template_id: &str) -> Result<(String, String, String, String)> {
    let _cache_guard = BUILTIN_PROMPT_CACHE_LOCK
        .lock()
        .map_err(|_| CodexxError::Database("提示词缓存锁已损坏".to_string()))?;
    let id = if template_id.trim().is_empty() {
        "gpt5.5-unrestricted"
    } else {
        template_id.trim()
    };
    let bundled = bundled_prompt_meta(id);
    let cached = cached_builtin_prompt(id)?;
    let filename = cached
        .as_ref()
        .map(|item| item.filename.clone())
        .or_else(|| bundled.map(|item| item.filename.to_string()))
        .ok_or_else(|| CodexxError::Config(format!("提示词模板不存在或尚未同步: {id}")))?;
    let source_url = cached
        .as_ref()
        .map(|item| item.source_url.clone())
        .unwrap_or_else(|| builtin_prompt_source_url(&filename));

    if let Ok(remote) = fetch_remote_prompt(&source_url) {
        save_builtin_prompt_cache(id, &filename, &source_url, &remote)?;
        return Ok((
            filename.clone(),
            format!("./{filename}"),
            remote,
            "GitHub 最新".to_string(),
        ));
    }
    if let Some(cache) = cached {
        return Ok((
            cache.filename.clone(),
            format!("./{}", cache.filename),
            cache.content,
            "本地缓存".to_string(),
        ));
    }
    let bundled = bundled.ok_or_else(|| {
        CodexxError::Config(format!("无法下载提示词且没有可用缓存: {template_id}"))
    })?;
    Ok((
        bundled.filename.to_string(),
        format!("./{}", bundled.filename),
        bundled.content.to_string(),
        "打包内置".to_string(),
    ))
}

fn builtin_prompt_id_for_filename(filename: &str) -> Result<Option<String>> {
    if let Some(meta) = bundled_prompt_metas()
        .into_iter()
        .find(|item| item.filename.eq_ignore_ascii_case(filename))
    {
        return Ok(Some(meta.id.to_string()));
    }
    Ok(cached_builtin_prompts()?
        .into_iter()
        .find(|item| item.filename.eq_ignore_ascii_case(filename))
        .map(|item| item.id))
}

fn saved_prompt_id_for_filename(filename: &str) -> Result<Option<String>> {
    Ok(list_saved_prompts_inner()?
        .into_iter()
        .find(|item| item.filename.eq_ignore_ascii_case(filename))
        .map(|item| item.id))
}

fn prompt_template_key_for_instruction(value: &str) -> Result<Option<String>> {
    let normalized = value.replace('\\', "/");
    let filename = normalized.rsplit('/').next().unwrap_or(&normalized);
    if let Some(id) = builtin_prompt_id_for_filename(filename)? {
        return Ok(Some(format!("builtin:{id}")));
    }
    Ok(saved_prompt_id_for_filename(filename)?.map(|id| format!("saved:{id}")))
}

fn resolve_instruction_path(codex_dir: &Path, value: &str) -> PathBuf {
    let trimmed = value.trim();
    let expanded = if trimmed == "~" {
        home_dir().unwrap_or_else(|_| codex_dir.to_path_buf())
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|_| PathBuf::from(trimmed))
    } else {
        PathBuf::from(trimmed)
    };
    if expanded.is_absolute() {
        expanded
    } else {
        codex_dir.join(expanded)
    }
}

fn remember_current_instruction_prompt(codex_dir: &Path) -> Result<Option<SavedPrompt>> {
    let cfg = config_path(codex_dir);
    let text = read_to_string_if_exists(&cfg)?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    let doc = parse_toml_document(&cfg, &text)?;
    let Some(current) = string_value(&doc, "model_instructions_file") else {
        return Ok(None);
    };
    if prompt_template_key_for_instruction(&current)?.is_some() {
        return Ok(None);
    }
    let path = resolve_instruction_path(codex_dir, &current);
    if !path.is_file() {
        return Ok(None);
    }
    let content = read_to_string_if_exists(&path)?;
    if content.trim().is_empty() {
        return Ok(None);
    }
    let file_name = path
        .file_name()
        .and_then(|v| v.to_str())
        .map(ToString::to_string)
        .unwrap_or_else(|| "external-prompt.md".to_string());
    let stem = path
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or("external-prompt");
    let normalized_filename = normalize_prompt_filename(&file_name, "external-prompt");
    let existing = find_saved_prompt_by_current_file(&file_name, &content)?;
    let (id, title, filename) = existing
        .map(|prompt| (prompt.id, prompt.title, prompt.filename))
        .unwrap_or_else(|| {
            (
                format!("external-{}", sanitize_id(stem)),
                format!("外部提示词 · {stem}"),
                normalized_filename,
            )
        });
    save_prompt_inner(SavedPrompt {
        id,
        title,
        filename,
        content,
    })
    .map(Some)
}

fn codex_skills_dir(codex_dir: &Path) -> PathBuf {
    codex_dir.join("skills")
}

fn disabled_skills_dir() -> Result<PathBuf> {
    Ok(app_home()?.join("disabled-skills"))
}

fn sanitize_dir_name(input: &str, fallback: &str) -> String {
    let raw = input.trim().trim_matches('/').trim_matches('\\');
    let base = if raw.is_empty() { fallback } else { raw };
    let mut out = String::new();
    let mut last_dash = false;
    for ch in base.to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let out = out.trim_matches('-');
    if out.is_empty() {
        fallback.to_string()
    } else {
        out.to_string()
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).map_err(|e| io_err(dst, e))?;
    for entry in fs::read_dir(src).map_err(|e| io_err(src, e))? {
        let entry = entry.map_err(|e| io_err(src, e))?;
        let path = entry.path();
        let file_name = entry.file_name();
        let target = dst.join(file_name);
        let meta = fs::symlink_metadata(&path).map_err(|e| io_err(&path, e))?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else if meta.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
            }
            fs::copy(&path, &target).map_err(|e| io_err(&target, e))?;
        }
    }
    Ok(())
}

fn compute_dir_hash(dir: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    fn collect(base: &Path, current: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(current).map_err(|e| io_err(current, e))? {
            let entry = entry.map_err(|e| io_err(current, e))?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let meta = fs::symlink_metadata(&path).map_err(|e| io_err(&path, e))?;
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                collect(base, &path, out)?;
            } else if meta.is_file() {
                out.push(path);
            }
        }
        let _ = base;
        Ok(())
    }
    let mut files = Vec::new();
    collect(dir, dir, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for path in files {
        let rel = path
            .strip_prefix(dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        hasher.update(rel.as_bytes());
        hasher.update(b"\0");
        let bytes = fs::read(&path).map_err(|e| io_err(&path, e))?;
        hasher.update(&bytes);
        hasher.update(b"\0");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn clean_skill_metadata_value(value: &str) -> String {
    value
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string()
}

fn read_skill_metadata(skill_dir: &Path, fallback: &str) -> (String, Option<String>) {
    let skill_md = skill_dir.join("SKILL.md");
    let text = read_to_string_if_exists(&skill_md).unwrap_or_default();
    let mut frontmatter_name: Option<String> = None;
    let mut frontmatter_desc: Option<String> = None;
    let mut heading_title: Option<String> = None;
    let mut body_desc: Option<String> = None;
    let mut in_frontmatter = false;
    let mut frontmatter_seen = false;

    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if index == 0 && trimmed == "---" {
            in_frontmatter = true;
            frontmatter_seen = true;
            continue;
        }
        if in_frontmatter {
            if trimmed == "---" {
                in_frontmatter = false;
                continue;
            }
            if let Some((key, value)) = trimmed.split_once(':') {
                let key = key.trim().to_ascii_lowercase();
                let value = clean_skill_metadata_value(value);
                if key == "name" && frontmatter_name.is_none() && !value.is_empty() {
                    frontmatter_name = Some(value);
                } else if key == "description" && frontmatter_desc.is_none() && !value.is_empty() {
                    frontmatter_desc = Some(value);
                }
            }
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        if heading_title.is_none() && trimmed.starts_with('#') {
            heading_title = Some(trimmed.trim_start_matches('#').trim().to_string());
            continue;
        }
        if body_desc.is_none()
            && !trimmed.starts_with('#')
            && !trimmed.starts_with("---")
            && !frontmatter_seen
        {
            body_desc = Some(clean_skill_metadata_value(trimmed));
            break;
        }
    }

    let title = frontmatter_name
        .or(heading_title)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback.to_string());
    let desc = frontmatter_desc.or(body_desc).filter(|s| !s.is_empty());
    (title, desc)
}

fn normalize_legacy_zip_skill_dirs(base: &Path) -> Result<()> {
    if !base.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(base).map_err(|e| io_err(base, e))? {
        let entry = entry.map_err(|e| io_err(base, e))?;
        let path = entry.path();
        if !path.is_dir() || !path.join("SKILL.md").is_file() {
            continue;
        }
        let directory = entry.file_name().to_string_lossy().to_string();
        if !directory.starts_with("skill-zip-") {
            continue;
        }
        let (name, _) = read_skill_metadata(&path, &directory);
        let dst_name = sanitize_dir_name(&name, "skill");
        if dst_name == directory || dst_name.starts_with("skill-zip-") {
            continue;
        }
        let dst = base.join(dst_name);
        if dst.exists() {
            continue;
        }
        fs::rename(&path, &dst).map_err(|e| io_err(&dst, e))?;
    }
    Ok(())
}

fn save_managed_skill(skill: &ManagedSkill) -> Result<()> {
    let conn = open_db()?;
    conn.execute(
        "INSERT INTO managed_skills (id, name, description, directory, source_path, content_hash, enabled, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
           name = excluded.name,
           description = excluded.description,
           directory = excluded.directory,
           source_path = excluded.source_path,
           content_hash = excluded.content_hash,
           enabled = excluded.enabled,
           updated_at = excluded.updated_at",
        params![
            skill.id,
            skill.name,
            skill.description,
            skill.directory,
            skill.path,
            skill.content_hash,
            skill.enabled,
            now_rfc3339()
        ],
    ).map_err(|e| CodexxError::Database(e.to_string()))?;
    Ok(())
}

fn scan_skill_dir(
    base: &Path,
    enabled: bool,
    source: &str,
    out: &mut Vec<ManagedSkill>,
    seen: &mut HashSet<String>,
) -> Result<()> {
    if !base.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(base).map_err(|e| io_err(base, e))? {
        let entry = entry.map_err(|e| io_err(base, e))?;
        let path = entry.path();
        if !path.is_dir() || !path.join("SKILL.md").is_file() {
            continue;
        }
        let directory = entry.file_name().to_string_lossy().to_string();
        let id = sanitize_dir_name(&directory, "skill");
        if seen.contains(&id) {
            continue;
        }
        let (name, description) = read_skill_metadata(&path, &directory);
        let hash = compute_dir_hash(&path).ok();
        out.push(ManagedSkill {
            id: id.clone(),
            name,
            description,
            directory,
            enabled,
            source: source.to_string(),
            path: path.display().to_string(),
            content_hash: hash,
            update_status: "未检查".to_string(),
        });
        seen.insert(id);
    }
    Ok(())
}

fn toml_value_to_json(value: &toml_edit::Value) -> Value {
    if let Some(s) = value.as_str() {
        return json!(s);
    }
    if let Some(i) = value.as_integer() {
        return json!(i);
    }
    if let Some(f) = value.as_float() {
        return json!(f);
    }
    if let Some(b) = value.as_bool() {
        return json!(b);
    }
    if let Some(arr) = value.as_array() {
        return Value::Array(arr.iter().map(toml_value_to_json).collect());
    }
    Value::String(value.to_string())
}

fn toml_item_to_json(item: &Item) -> Value {
    if let Some(v) = item.as_value() {
        return toml_value_to_json(v);
    }
    if let Some(tbl) = item.as_table() {
        let mut obj = serde_json::Map::new();
        for (k, v) in tbl.iter() {
            obj.insert(k.to_string(), toml_item_to_json(v));
        }
        return Value::Object(obj);
    }
    Value::Null
}

fn json_to_toml_item(value_json: &Value) -> Item {
    match value_json {
        Value::String(s) => value(s.clone()),
        Value::Bool(b) => value(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                value(i)
            } else if let Some(f) = n.as_f64() {
                value(f)
            } else {
                value(n.to_string())
            }
        }
        Value::Array(arr) => {
            let mut toml_arr = toml_edit::Array::default();
            for item in arr {
                match item {
                    Value::String(s) => {
                        toml_arr.push(s.as_str());
                    }
                    Value::Bool(b) => {
                        toml_arr.push(*b);
                    }
                    Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            toml_arr.push(i);
                        } else if let Some(f) = n.as_f64() {
                            toml_arr.push(f);
                        }
                    }
                    _ => {}
                }
            }
            value(toml_arr)
        }
        Value::Object(obj) => {
            let mut table = Table::new();
            for (k, v) in obj {
                table.insert(k, json_to_toml_item(v));
            }
            Item::Table(table)
        }
        Value::Null => value(""),
    }
}

fn mcp_summary(config: &Value) -> (String, Option<String>, Option<String>, String) {
    let transport = config
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            if config.get("url").is_some() {
                "http"
            } else {
                "stdio"
            }
        })
        .to_string();
    let command = config
        .get("command")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let url = config
        .get("url")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let args = config
        .get("args")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let summary = if let Some(cmd) = &command {
        if args.is_empty() {
            cmd.clone()
        } else {
            format!("{cmd} {args}")
        }
    } else if let Some(url) = &url {
        url.clone()
    } else {
        transport.clone()
    };
    (transport, command, url, summary)
}

fn save_managed_mcp(id: &str, name: &str, config: &Value, enabled: bool) -> Result<()> {
    let conn = open_db()?;
    conn.execute(
        "INSERT INTO managed_mcp_servers (id, name, server_config, enabled, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET
           name = excluded.name,
           server_config = excluded.server_config,
           enabled = excluded.enabled,
           updated_at = excluded.updated_at",
        params![
            id,
            name,
            serde_json::to_string(config).unwrap_or_default(),
            enabled,
            now_rfc3339()
        ],
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    Ok(())
}

fn db_managed_mcp() -> Result<Vec<(String, String, Value, bool)>> {
    let conn = open_db()?;
    let mut stmt = conn.prepare("SELECT id, name, server_config, enabled FROM managed_mcp_servers ORDER BY name ASC, id ASC")
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            let text: String = row.get(2)?;
            let config = serde_json::from_str(&text).unwrap_or(Value::Object(Default::default()));
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                config,
                row.get::<_, bool>(3)?,
            ))
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| CodexxError::Database(e.to_string()))?);
    }
    Ok(out)
}

fn list_mcp_from_config(codex_dir: &Path) -> Result<Vec<ManagedMcpServer>> {
    let cfg = config_path(codex_dir);
    let text = read_to_string_if_exists(&cfg)?;
    if text.trim().is_empty() {
        return Ok(vec![]);
    }
    let doc = parse_toml_document(&cfg, &text)?;
    let Some(mcp_item) = doc.get("mcp_servers") else {
        return Ok(vec![]);
    };
    let Some(mcp_tbl) = mcp_item.as_table() else {
        return Ok(vec![]);
    };
    let mut out = Vec::new();
    for (id, item) in mcp_tbl.iter() {
        if !item.is_table() {
            continue;
        }
        let config = toml_item_to_json(item);
        let (transport, command, url, summary) = mcp_summary(&config);
        out.push(ManagedMcpServer {
            id: id.to_string(),
            name: id.to_string(),
            transport,
            enabled: true,
            source: "config.toml".to_string(),
            summary,
            command,
            url,
            config_json: config,
        });
    }
    Ok(out)
}

fn sort_managed_skills(skills: &mut [ManagedSkill]) {
    skills.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
}

fn sort_managed_mcp_servers(servers: &mut [ManagedMcpServer]) {
    servers.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
}

fn build_skills_mcp_state_inner(config_dir: Option<String>) -> Result<SkillsMcpState> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    let skills_dir = codex_skills_dir(&codex_dir);
    let disabled_dir = disabled_skills_dir()?;
    let mut warnings = Vec::new();
    let mut skills = Vec::new();
    let mut seen = HashSet::new();
    if let Err(e) = normalize_legacy_zip_skill_dirs(&skills_dir) {
        warnings.push(format!("修正 ZIP Skill 目录名失败: {e}"));
    }
    if let Err(e) = normalize_legacy_zip_skill_dirs(&disabled_dir) {
        warnings.push(format!("修正已禁用 ZIP Skill 目录名失败: {e}"));
    }
    if let Err(e) = scan_skill_dir(&skills_dir, true, "Codex", &mut skills, &mut seen) {
        warnings.push(e.to_string());
    }
    if let Err(e) = scan_skill_dir(
        &disabled_dir,
        false,
        "Codex-X 已禁用",
        &mut skills,
        &mut seen,
    ) {
        warnings.push(e.to_string());
    }

    let mut mcp_servers = list_mcp_from_config(&codex_dir)?;
    let enabled_ids: HashSet<String> = mcp_servers.iter().map(|s| s.id.clone()).collect();
    for (id, name, config, enabled) in db_managed_mcp()? {
        if enabled_ids.contains(&id) {
            continue;
        }
        let (transport, command, url, summary) = mcp_summary(&config);
        mcp_servers.push(ManagedMcpServer {
            id,
            name,
            transport,
            enabled,
            source: "Codex-X".to_string(),
            summary,
            command,
            url,
            config_json: config,
        });
    }
    sort_managed_mcp_servers(&mut mcp_servers);
    sort_managed_skills(&mut skills);
    Ok(SkillsMcpState {
        codex_dir: codex_dir.display().to_string(),
        codex_skills_dir: skills_dir.display().to_string(),
        disabled_skills_dir: disabled_dir.display().to_string(),
        skills,
        mcp_servers,
        warnings,
    })
}

fn import_ccswitch_mcp_servers_for_codex(
    codex_dir: &Path,
    imported_ids: &mut HashSet<String>,
) -> Result<usize> {
    let db = default_ccswitch_db_path()?;
    if !db.exists() {
        return Ok(0);
    }
    let conn = Connection::open_with_flags(
        &db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| {
        CodexxError::Database(format!(
            "打开 cc-switch MCP 数据库失败 {}: {e}",
            db.display()
        ))
    })?;
    let mut stmt = match conn
        .prepare("SELECT id, name, server_config, enabled_codex FROM mcp_servers ORDER BY name ASC, id ASC")
        .or_else(|_| {
            conn.prepare("SELECT id, name, server_config, 0 AS enabled_codex FROM mcp_servers ORDER BY name ASC, id ASC")
        }) {
        Ok(stmt) => stmt,
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if message.to_lowercase().contains("no such table") =>
        {
            return Ok(0);
        }
        Err(e) => return Err(CodexxError::Database(e.to_string())),
    };
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
            ))
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;

    let cfg = config_path(codex_dir);
    let text = read_to_string_if_exists(&cfg)?;
    let mut doc = parse_toml_document(&cfg, &text)?;
    let live_enabled = list_mcp_from_config(codex_dir)?
        .into_iter()
        .map(|server| server.id)
        .collect::<HashSet<_>>();
    let mut imported = 0usize;
    let mut changed_config = false;
    for row in rows {
        let (id, name, config_text, enabled_codex) =
            row.map_err(|e| CodexxError::Database(e.to_string()))?;
        let config: Value =
            serde_json::from_str(&config_text).unwrap_or(Value::Object(Default::default()));
        let enabled = enabled_codex || live_enabled.contains(&id);
        save_managed_mcp(&id, &name, &config, enabled)?;
        if enabled_codex && !live_enabled.contains(&id) {
            ensure_table(doc.as_table_mut(), "mcp_servers")?
                .insert(&id, json_to_toml_item(&config));
            changed_config = true;
        }
        if imported_ids.insert(id) {
            imported += 1;
        }
    }
    if changed_config {
        write_text(&cfg, &doc.to_string())?;
    }
    Ok(imported)
}

fn import_existing_skills_mcp_inner(config_dir: Option<String>) -> Result<SkillsMcpActionResult> {
    let codex_dir = resolve_codex_dir(config_dir.clone())?;
    let skills_dir = codex_skills_dir(&codex_dir);
    fs::create_dir_all(&skills_dir).map_err(|e| io_err(&skills_dir, e))?;
    let mut imported_skills = 0usize;
    let candidates = vec![
        home_dir()?.join(".agents").join("skills"),
        home_dir()?.join(".cc-switch").join("skills"),
    ];
    for base in candidates {
        if !base.exists() {
            continue;
        }
        for entry in fs::read_dir(&base).map_err(|e| io_err(&base, e))? {
            let entry = entry.map_err(|e| io_err(&base, e))?;
            let src = entry.path();
            if !src.is_dir() || !src.join("SKILL.md").is_file() {
                continue;
            }
            let directory = sanitize_dir_name(&entry.file_name().to_string_lossy(), "skill");
            let dst = skills_dir.join(&directory);
            if !dst.exists() {
                copy_dir_recursive(&src, &dst)?;
                imported_skills += 1;
            }
        }
    }

    let mut imported_mcp = 0usize;
    let mut imported_mcp_ids = HashSet::new();
    for server in list_mcp_from_config(&codex_dir)? {
        save_managed_mcp(&server.id, &server.name, &server.config_json, true)?;
        if imported_mcp_ids.insert(server.id.clone()) {
            imported_mcp += 1;
        }
    }
    imported_mcp += import_ccswitch_mcp_servers_for_codex(&codex_dir, &mut imported_mcp_ids)?;
    let state = build_skills_mcp_state_inner(config_dir)?;
    Ok(SkillsMcpActionResult {
        imported_skills,
        imported_mcp,
        message: format!("已导入 {imported_skills} 个 Skills，纳管 {imported_mcp} 个 MCP"),
        state,
    })
}

fn preview_ccswitch_mcp_servers_for_codex(codex_dir: &Path) -> Result<Vec<ManagedMcpServer>> {
    let db = default_ccswitch_db_path()?;
    if !db.exists() {
        return Ok(vec![]);
    }
    let conn = Connection::open_with_flags(
        &db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| {
        CodexxError::Database(format!(
            "打开 cc-switch MCP 数据库失败 {}: {e}",
            db.display()
        ))
    })?;
    let mut stmt = match conn
        .prepare("SELECT id, name, server_config, enabled_codex FROM mcp_servers ORDER BY name ASC, id ASC")
        .or_else(|_| {
            conn.prepare("SELECT id, name, server_config, 0 AS enabled_codex FROM mcp_servers ORDER BY name ASC, id ASC")
        }) {
        Ok(stmt) => stmt,
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if message.to_lowercase().contains("no such table") =>
        {
            return Ok(vec![]);
        }
        Err(e) => return Err(CodexxError::Database(e.to_string())),
    };
    let live_enabled = list_mcp_from_config(codex_dir)?
        .into_iter()
        .map(|server| server.id)
        .collect::<HashSet<_>>();
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
            ))
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let mut out = Vec::new();
    for row in rows {
        let (id, name, config_text, enabled_codex) =
            row.map_err(|e| CodexxError::Database(e.to_string()))?;
        let config: Value =
            serde_json::from_str(&config_text).unwrap_or(Value::Object(Default::default()));
        let (transport, command, url, summary) = mcp_summary(&config);
        out.push(ManagedMcpServer {
            id: id.clone(),
            name,
            transport,
            enabled: enabled_codex || live_enabled.contains(&id),
            source: "cc-switch".to_string(),
            summary,
            command,
            url,
            config_json: config,
        });
    }
    Ok(out)
}

fn preview_existing_skills_mcp_inner(config_dir: Option<String>) -> Result<SkillsMcpImportPreview> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    let skills_dir = codex_skills_dir(&codex_dir);
    let mut warnings = Vec::new();
    let mut skills = Vec::new();
    let mut seen = HashSet::new();
    let candidates = vec![
        home_dir()?.join(".agents").join("skills"),
        home_dir()?.join(".cc-switch").join("skills"),
    ];
    for base in candidates {
        if !base.exists() {
            continue;
        }
        let source = base
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "外部目录".to_string());
        let before = skills.len();
        if let Err(e) = scan_skill_dir(&base, false, &source, &mut skills, &mut seen) {
            warnings.push(e.to_string());
        }
        for skill in &mut skills[before..] {
            if skills_dir.join(&skill.directory).exists() {
                skill.update_status = "已存在，将跳过".to_string();
            } else {
                skill.update_status = "可导入".to_string();
            }
        }
    }
    skills.retain(|skill| skill.update_status != "已存在，将跳过");

    let mut mcp_servers = list_mcp_from_config(&codex_dir)?;
    for server in &mut mcp_servers {
        server.source = "config.toml".to_string();
    }
    let mut seen_mcp = mcp_servers
        .iter()
        .map(|server| server.id.clone())
        .collect::<HashSet<_>>();
    for server in preview_ccswitch_mcp_servers_for_codex(&codex_dir)? {
        if seen_mcp.insert(server.id.clone()) {
            mcp_servers.push(server);
        }
    }
    sort_managed_skills(&mut skills);
    sort_managed_mcp_servers(&mut mcp_servers);
    Ok(SkillsMcpImportPreview {
        skills,
        mcp_servers,
        warnings,
    })
}

fn toggle_codex_mcp_inner(
    config_dir: Option<String>,
    id: String,
    enabled: bool,
) -> Result<SkillsMcpState> {
    let codex_dir = resolve_codex_dir(config_dir.clone())?;
    fs::create_dir_all(&codex_dir).map_err(|e| io_err(&codex_dir, e))?;
    let cfg = config_path(&codex_dir);
    let text = read_to_string_if_exists(&cfg)?;
    let mut doc = parse_toml_document(&cfg, &text)?;
    if enabled {
        let db = db_managed_mcp()?
            .into_iter()
            .find(|(sid, _, _, _)| sid == &id)
            .ok_or_else(|| CodexxError::Config(format!("未找到 MCP: {id}")))?;
        ensure_table(doc.as_table_mut(), "mcp_servers")?.insert(&id, json_to_toml_item(&db.2));
        save_managed_mcp(&id, &db.1, &db.2, true)?;
    } else {
        if let Some(item) = doc
            .get("mcp_servers")
            .and_then(|m| m.as_table())
            .and_then(|tbl| tbl.get(&id))
        {
            let config = toml_item_to_json(item);
            save_managed_mcp(&id, &id, &config, false)?;
        }
        if let Some(tbl) = doc.get_mut("mcp_servers").and_then(|m| m.as_table_mut()) {
            tbl.remove(&id);
        }
    }
    write_text(&cfg, &doc.to_string())?;
    build_skills_mcp_state_inner(config_dir)
}

fn move_dir_replace(src: &Path, dst: &Path) -> Result<()> {
    if !src.exists() {
        return Ok(());
    }
    if dst.exists() {
        fs::remove_dir_all(dst).map_err(|e| io_err(dst, e))?;
    }
    fs::rename(src, dst)
        .or_else(|_| {
            copy_dir_recursive(src, dst).map(|_| {
                let _ = fs::remove_dir_all(src);
            })
        })
        .map_err(|e| {
            CodexxError::Config(format!(
                "移动目录失败 {} -> {}: {e}",
                src.display(),
                dst.display()
            ))
        })
}

fn toggle_codex_skill_inner(
    config_dir: Option<String>,
    id: String,
    enabled: bool,
) -> Result<SkillsMcpState> {
    let codex_dir = resolve_codex_dir(config_dir.clone())?;
    let skills_dir = codex_skills_dir(&codex_dir);
    let disabled_dir = disabled_skills_dir()?;
    fs::create_dir_all(&skills_dir).map_err(|e| io_err(&skills_dir, e))?;
    fs::create_dir_all(&disabled_dir).map_err(|e| io_err(&disabled_dir, e))?;
    let current_state = build_skills_mcp_state_inner(config_dir.clone())?;
    let name = current_state
        .skills
        .iter()
        .find(|skill| skill.id == id)
        .map(|skill| skill.directory.clone())
        .unwrap_or_else(|| sanitize_dir_name(&id, "skill"));
    let enabled_path = skills_dir.join(&name);
    let disabled_path = disabled_dir.join(&name);
    if enabled {
        if disabled_path.exists() {
            move_dir_replace(&disabled_path, &enabled_path)
                .map_err(|e| CodexxError::Config(format!("启用 Skill 失败: {e}")))?;
        }
    } else if enabled_path.exists() {
        move_dir_replace(&enabled_path, &disabled_path)
            .map_err(|e| CodexxError::Config(format!("禁用 Skill 失败: {e}")))?;
    }
    build_skills_mcp_state_inner(config_dir)
}

fn install_skill_zip_inner(
    config_dir: Option<String>,
    file_name: String,
    bytes: Vec<u8>,
) -> Result<SkillsMcpActionResult> {
    let codex_dir = resolve_codex_dir(config_dir.clone())?;
    let skills_dir = codex_skills_dir(&codex_dir);
    fs::create_dir_all(&skills_dir).map_err(|e| io_err(&skills_dir, e))?;
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| CodexxError::Config(format!("读取 ZIP 失败: {e}")))?;
    let tmp = app_home()?
        .join("tmp")
        .join(format!("skill-zip-{}", Local::now().timestamp_millis()));
    fs::create_dir_all(&tmp).map_err(|e| io_err(&tmp, e))?;
    let install_result = (|| -> Result<usize> {
        let mut total_size = 0u64;
        for i in 0..archive.len() {
            let mut file = archive
                .by_index(i)
                .map_err(|e| CodexxError::Config(format!("读取 ZIP 条目失败: {e}")))?;
            let Some(path) = file.enclosed_name().map(|p| p.to_path_buf()) else {
                continue;
            };
            total_size += file.size();
            if total_size > MAX_SKILL_ZIP_BYTES {
                return Err(CodexxError::Config("ZIP 解压后超过 20MB".to_string()));
            }
            let out = tmp.join(path);
            if file.name().ends_with('/') {
                fs::create_dir_all(&out).map_err(|e| io_err(&out, e))?;
            } else {
                if let Some(parent) = out.parent() {
                    fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
                }
                let mut outfile = fs::File::create(&out).map_err(|e| io_err(&out, e))?;
                std::io::copy(&mut file, &mut outfile).map_err(|e| io_err(&out, e))?;
            }
        }

        let mut skill_dirs = Vec::new();
        fn find_skill_dirs(current: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
            if current.join("SKILL.md").is_file() {
                out.push(current.to_path_buf());
                return Ok(());
            }
            for entry in fs::read_dir(current).map_err(|e| io_err(current, e))? {
                let entry = entry.map_err(|e| io_err(current, e))?;
                let path = entry.path();
                if path.is_dir() {
                    find_skill_dirs(&path, out)?;
                }
            }
            Ok(())
        }
        find_skill_dirs(&tmp, &mut skill_dirs)?;
        if skill_dirs.is_empty() {
            return Err(CodexxError::Config("ZIP 中没有找到 SKILL.md".to_string()));
        }
        let mut imported_skills = 0usize;
        for src in skill_dirs {
            let fallback = file_name.trim_end_matches(".zip");
            let dir_name = src.file_name().and_then(|v| v.to_str()).unwrap_or(fallback);
            let (skill_name, _) = read_skill_metadata(&src, dir_name);
            let dst_name = sanitize_dir_name(&skill_name, "skill");
            let dst = skills_dir.join(dst_name);
            if dst.exists() {
                fs::remove_dir_all(&dst).map_err(|e| io_err(&dst, e))?;
            }
            copy_dir_recursive(&src, &dst)?;
            imported_skills += 1;
        }
        Ok(imported_skills)
    })();
    let _ = fs::remove_dir_all(&tmp);
    let imported_skills = install_result?;
    let state = build_skills_mcp_state_inner(config_dir)?;
    Ok(SkillsMcpActionResult {
        imported_skills,
        imported_mcp: 0,
        message: format!("已从 ZIP 安装 {imported_skills} 个 Skill"),
        state,
    })
}

#[derive(Debug, Clone)]
struct CcSwitchSkillMeta {
    repo_owner: String,
    repo_name: String,
    repo_branch: String,
    content_hash: Option<String>,
}

fn ccswitch_skill_meta_by_directory() -> Result<HashMap<String, CcSwitchSkillMeta>> {
    let db = default_ccswitch_db_path()?;
    if !db.exists() {
        return Ok(HashMap::new());
    }
    let conn = Connection::open_with_flags(
        &db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| {
        CodexxError::Database(format!(
            "打开 cc-switch Skills 数据库失败 {}: {e}",
            db.display()
        ))
    })?;
    let mut stmt = match conn.prepare(
        "SELECT directory, repo_owner, repo_name, repo_branch, content_hash FROM skills
         WHERE repo_owner IS NOT NULL AND repo_name IS NOT NULL",
    ) {
        Ok(stmt) => stmt,
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if message.to_lowercase().contains("no such table") =>
        {
            return Ok(HashMap::new());
        }
        Err(e) => return Err(CodexxError::Database(e.to_string())),
    };
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;

    let mut out = HashMap::new();
    for row in rows {
        let (directory, owner, repo, branch, content_hash) =
            row.map_err(|e| CodexxError::Database(e.to_string()))?;
        let (Some(repo_owner), Some(repo_name)) = (owner, repo) else {
            continue;
        };
        out.insert(
            directory.to_ascii_lowercase(),
            CcSwitchSkillMeta {
                repo_owner,
                repo_name,
                repo_branch: branch.unwrap_or_else(|| "main".to_string()),
                content_hash,
            },
        );
    }
    Ok(out)
}

fn download_repo_skill_hashes(
    owner: &str,
    repo: &str,
    branch: &str,
) -> std::result::Result<HashMap<String, String>, String> {
    use sha2::{Digest, Sha256};
    const MAX_ZIP_BYTES: u64 = 100 * 1024 * 1024;
    let url = format!("https://github.com/{owner}/{repo}/archive/refs/heads/{branch}.zip");
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(18))
        .build();
    let response = agent
        .get(&url)
        .set("User-Agent", "Codex-X")
        .call()
        .map_err(|e| format!("下载 {owner}/{repo}@{branch} 失败: {e}"))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_ZIP_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("读取 {owner}/{repo}@{branch} ZIP 失败: {e}"))?;
    if bytes.len() as u64 > MAX_ZIP_BYTES {
        return Err(format!("{owner}/{repo}@{branch} ZIP 超过 100MB"));
    }

    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| format!("解析 {owner}/{repo}@{branch} ZIP 失败: {e}"))?;
    let mut files = Vec::<(String, Vec<u8>)>::new();
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("读取 ZIP 条目失败: {e}"))?;
        if file.is_dir() {
            continue;
        }
        let Some(path) = file.enclosed_name().map(|p| p.to_path_buf()) else {
            continue;
        };
        let normalized = path.to_string_lossy().replace('\\', "/");
        if normalized
            .split('/')
            .any(|part| part.starts_with('.') && part != ".")
        {
            continue;
        }
        let mut data = Vec::new();
        file.read_to_end(&mut data)
            .map_err(|e| format!("读取 ZIP 文件失败: {e}"))?;
        files.push((normalized, data));
    }

    let mut prefixes = HashMap::<String, String>::new();
    for (path, _) in &files {
        if !path.ends_with("/SKILL.md") && path != "SKILL.md" {
            continue;
        }
        let Some(prefix) = path.strip_suffix("/SKILL.md") else {
            continue;
        };
        let Some(name) = prefix.rsplit('/').next() else {
            continue;
        };
        prefixes.insert(name.to_ascii_lowercase(), prefix.to_string());
    }

    let mut hashes = HashMap::new();
    for (skill_name, prefix) in prefixes {
        let prefix_with_slash = format!("{prefix}/");
        let mut scoped = files
            .iter()
            .filter_map(|(path, data)| {
                path.strip_prefix(&prefix_with_slash)
                    .map(|rel| (rel.to_string(), data.as_slice()))
            })
            .collect::<Vec<_>>();
        scoped.sort_by(|a, b| a.0.cmp(&b.0));
        let mut hasher = Sha256::new();
        for (rel, data) in scoped {
            hasher.update(rel.as_bytes());
            hasher.update(b"\0");
            hasher.update(data);
            hasher.update(b"\0");
        }
        hashes.insert(skill_name, format!("{:x}", hasher.finalize()));
    }
    Ok(hashes)
}

fn check_skill_updates_inner(config_dir: Option<String>) -> Result<SkillsMcpState> {
    let mut next = build_skills_mcp_state_inner(config_dir)?;
    let conn = open_db()?;
    let ccswitch_meta = ccswitch_skill_meta_by_directory().unwrap_or_default();
    let mut remote_hash_cache = HashMap::<
        (String, String, String),
        std::result::Result<HashMap<String, String>, String>,
    >::new();
    for skill in &mut next.skills {
        let old: Option<String> = conn
            .query_row(
                "SELECT content_hash FROM managed_skills WHERE id = ?1",
                [&skill.id],
                |row| row.get(0),
            )
            .ok();
        let local_status = match (&old, &skill.content_hash) {
            (Some(a), Some(b)) if a != b => "本地有变化".to_string(),
            (Some(_), Some(_)) => "已是最新".to_string(),
            _ => "已记录".to_string(),
        };
        let meta = ccswitch_meta.get(&skill.directory.to_ascii_lowercase());
        skill.update_status = if let Some(meta) = meta {
            let key = (
                meta.repo_owner.clone(),
                meta.repo_name.clone(),
                meta.repo_branch.clone(),
            );
            let remote = remote_hash_cache
                .entry(key.clone())
                .or_insert_with(|| download_repo_skill_hashes(&key.0, &key.1, &key.2));
            match remote {
                Ok(remote_hashes) => {
                    let remote_hash = remote_hashes.get(&skill.directory.to_ascii_lowercase());
                    let local_hash = skill.content_hash.as_ref().or(meta.content_hash.as_ref());
                    match (local_hash, remote_hash) {
                        (Some(local), Some(remote)) if local != remote => "有新版本".to_string(),
                        (Some(_), Some(_)) => "已是最新".to_string(),
                        (_, Some(_)) => "已记录远程".to_string(),
                        _ => "未找到远程目录".to_string(),
                    }
                }
                Err(e) => format!("远程检查失败：{e}"),
            }
        } else {
            local_status
        };
        save_managed_skill(skill)?;
    }
    Ok(next)
}

fn sanitize_id(input: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in input.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        format!("provider-{}", Local::now().timestamp_millis())
    } else {
        out
    }
}

fn reserved_codex_provider_id(id: &str) -> bool {
    matches!(
        id.trim().to_ascii_lowercase().as_str(),
        "openai" | "amazon-bedrock" | "ollama" | "lmstudio" | "oss"
    )
}

fn custom_provider_id(input: &str) -> String {
    let id = sanitize_id(input);
    if reserved_codex_provider_id(&id) {
        format!("{id}-custom")
    } else {
        id
    }
}

fn experimental_bearer_token_from_doc(
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

#[derive(Debug, Clone)]
struct CcSwitchCodexRow {
    id: String,
    name: String,
    settings_config: String,
    category: Option<String>,
}

fn is_official_ccswitch_row(row: &CcSwitchCodexRow) -> bool {
    row.id.trim().eq_ignore_ascii_case("codex-official")
        || row
            .category
            .as_deref()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("official"))
}

fn read_ccswitch_codex_rows(conn: &Connection) -> Result<Vec<CcSwitchCodexRow>> {
    let provider_columns = table_column_set(conn, "providers")?;
    let category_column = if provider_columns.contains("category") {
        "category"
    } else {
        "NULL"
    };
    let provider_query = format!(
        "SELECT id, name, settings_config, {category_column} FROM providers
         WHERE app_type = 'codex' ORDER BY sort_index ASC, created_at ASC"
    );
    let mut stmt = conn
        .prepare(&provider_query)
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(CcSwitchCodexRow {
                id: row.get::<_, String>(0)?,
                name: row.get::<_, String>(1)?,
                settings_config: row.get::<_, String>(2)?,
                category: row.get::<_, Option<String>>(3)?,
            })
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(|e| CodexxError::Database(e.to_string()))?);
    }
    Ok(result)
}

#[derive(Debug, Clone)]
struct CcSwitchCodexSection {
    id: String,
    name: Option<String>,
    base_url: String,
    model: Option<String>,
    wire_api: String,
    requires_openai_auth: bool,
    experimental_bearer_token: Option<String>,
}

fn table_string(table: &Table, key: &str) -> Option<String> {
    table
        .get(key)
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

fn ccswitch_auth_api_key(settings: &Value) -> Option<String> {
    settings
        .get("auth")
        .and_then(|v| v.get("OPENAI_API_KEY"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

fn codex_section_from_table(
    id: &str,
    table: &Table,
    model: Option<String>,
) -> Option<CcSwitchCodexSection> {
    let base_url = table_string(table, "base_url")?
        .trim_end_matches('/')
        .to_string();
    if base_url.is_empty() {
        return None;
    }
    Some(CcSwitchCodexSection {
        id: id.to_string(),
        name: table_string(table, "name"),
        base_url,
        model,
        wire_api: table_string(table, "wire_api").unwrap_or_else(|| "responses".to_string()),
        requires_openai_auth: table
            .get("requires_openai_auth")
            .and_then(|item| item.as_bool())
            .unwrap_or(false),
        experimental_bearer_token: table_string(table, "experimental_bearer_token"),
    })
}

fn codex_sections_from_config(config_text: &str) -> Vec<CcSwitchCodexSection> {
    let Ok(doc) = config_text.parse::<DocumentMut>() else {
        return Vec::new();
    };
    let model = string_value(&doc, "model");
    let Some(providers) = doc.get("model_providers").and_then(|item| item.as_table()) else {
        return Vec::new();
    };
    providers
        .iter()
        .filter_map(|(id, item)| {
            item.as_table()
                .and_then(|table| codex_section_from_table(id, table, model.clone()))
        })
        .collect()
}

fn select_ccswitch_section_for_row(
    row: &CcSwitchCodexRow,
    settings: &Value,
    global_sections: &HashMap<String, CcSwitchCodexSection>,
) -> Option<CcSwitchCodexSection> {
    let provider_id = custom_provider_id(&row.id);
    if let Some(section) = global_sections.get(&provider_id) {
        return Some(section.clone());
    }
    if let Some(section) = global_sections.get(row.id.trim()) {
        return Some(section.clone());
    }

    let config_text = settings.get("config").and_then(Value::as_str).unwrap_or("");
    let doc = config_text.parse::<DocumentMut>().ok()?;
    let model = string_value(&doc, "model");
    let active_provider = string_value(&doc, "model_provider");
    let providers = doc.get("model_providers").and_then(|item| item.as_table());

    if let Some(providers) = providers {
        for exact_id in [provider_id.as_str(), row.id.trim()] {
            if let Some(section) = providers
                .get(exact_id)
                .and_then(|item| item.as_table())
                .and_then(|table| codex_section_from_table(exact_id, table, model.clone()))
            {
                return Some(section);
            }
        }

        if active_provider.as_deref() == Some(row.id.trim())
            || active_provider.as_deref() == Some(provider_id.as_str())
        {
            if let Some(active) = active_provider.as_deref() {
                if let Some(section) = providers
                    .get(active)
                    .and_then(|item| item.as_table())
                    .and_then(|table| codex_section_from_table(active, table, model.clone()))
                {
                    return Some(section);
                }
            }
        }

        // Legacy cc-switch/custom templates often store every third-party provider
        // under `[model_providers.custom]`. Only use it when the row's own config
        // explicitly activates custom or contains no other provider identity.
        if active_provider
            .as_deref()
            .is_none_or(|active| active == "custom")
        {
            if let Some(section) = providers
                .get("custom")
                .and_then(|item| item.as_table())
                .and_then(|table| codex_section_from_table("custom", table, model.clone()))
            {
                return Some(section);
            }
        }
    }

    doc.get("base_url")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|base_url| CcSwitchCodexSection {
            id: provider_id,
            name: None,
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            wire_api: "responses".to_string(),
            requires_openai_auth: false,
            experimental_bearer_token: experimental_bearer_token_from_doc(
                &doc,
                active_provider.as_deref(),
            ),
        })
}

fn build_ccswitch_codex_provider(
    row: &CcSwitchCodexRow,
    global_sections: &HashMap<String, CcSwitchCodexSection>,
) -> Option<SavedProvider> {
    let settings: Value = serde_json::from_str(&row.settings_config).ok()?;
    let section = select_ccswitch_section_for_row(row, &settings, global_sections)?;
    let api_key = ccswitch_auth_api_key(&settings).or(section.experimental_bearer_token.clone());
    Some(SavedProvider {
        id: custom_provider_id(&row.id),
        provider_name: if row.name.trim().is_empty() {
            section.name.unwrap_or_else(|| row.id.clone())
        } else {
            row.name.trim().to_string()
        },
        base_url: section.base_url,
        model: section.model.unwrap_or_else(|| "gpt-5.5".to_string()),
        api_key,
        toml_config: None,
        wire_api: section.wire_api,
        requires_openai_auth: section.requires_openai_auth,
    })
}

fn push_existing_candidate(candidates: &mut Vec<PathBuf>, candidate: Option<PathBuf>) {
    let Some(path) = candidate else {
        return;
    };
    if !candidates.iter().any(|item| item == &path) {
        candidates.push(path);
    }
}

fn ccswitch_db_candidates() -> Result<Vec<PathBuf>> {
    let mut candidates = Vec::new();

    if let Ok(value) = std::env::var("CC_SWITCH_HOME") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            push_existing_candidate(
                &mut candidates,
                Some(PathBuf::from(trimmed).join("cc-switch.db")),
            );
        }
    }

    let home = home_dir()?;
    // cc-switch 当前主要使用这个位置，macOS/Windows/Linux 都适用。
    push_existing_candidate(
        &mut candidates,
        Some(home.join(".cc-switch").join("cc-switch.db")),
    );

    // 兼容 Tauri/AppData 风格位置，防止未来或不同发行版变更数据目录。
    if let Some(data_dir) = dirs::data_dir() {
        push_existing_candidate(
            &mut candidates,
            Some(data_dir.join("com.ccswitch.desktop").join("cc-switch.db")),
        );
        push_existing_candidate(
            &mut candidates,
            Some(data_dir.join("cc-switch").join("cc-switch.db")),
        );
        push_existing_candidate(
            &mut candidates,
            Some(data_dir.join("CC Switch").join("cc-switch.db")),
        );
    }
    if let Some(data_local_dir) = dirs::data_local_dir() {
        push_existing_candidate(
            &mut candidates,
            Some(
                data_local_dir
                    .join("com.ccswitch.desktop")
                    .join("cc-switch.db"),
            ),
        );
        push_existing_candidate(
            &mut candidates,
            Some(data_local_dir.join("cc-switch").join("cc-switch.db")),
        );
        push_existing_candidate(
            &mut candidates,
            Some(data_local_dir.join("CC Switch").join("cc-switch.db")),
        );
    }

    #[cfg(target_os = "macos")]
    {
        push_existing_candidate(
            &mut candidates,
            Some(
                home.join("Library")
                    .join("Application Support")
                    .join("com.ccswitch.desktop")
                    .join("cc-switch.db"),
            ),
        );
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            push_existing_candidate(
                &mut candidates,
                Some(
                    PathBuf::from(appdata)
                        .join("com.ccswitch.desktop")
                        .join("cc-switch.db"),
                ),
            );
        }
        if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
            push_existing_candidate(
                &mut candidates,
                Some(
                    PathBuf::from(localappdata)
                        .join("com.ccswitch.desktop")
                        .join("cc-switch.db"),
                ),
            );
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg_data_home) = std::env::var("XDG_DATA_HOME") {
            push_existing_candidate(
                &mut candidates,
                Some(
                    PathBuf::from(xdg_data_home)
                        .join("com.ccswitch.desktop")
                        .join("cc-switch.db"),
                ),
            );
        }
        push_existing_candidate(
            &mut candidates,
            Some(
                home.join(".local")
                    .join("share")
                    .join("com.ccswitch.desktop")
                    .join("cc-switch.db"),
            ),
        );
    }

    Ok(candidates)
}

fn default_ccswitch_db_path() -> Result<PathBuf> {
    let candidates = ccswitch_db_candidates()?;
    candidates
        .iter()
        .find(|path| path.exists())
        .cloned()
        .or_else(|| candidates.into_iter().next())
        .ok_or_else(|| CodexxError::Config("无法生成 cc-switch 数据库候选路径".to_string()))
}

fn import_ccswitch_codex_providers_inner(path: Option<String>) -> Result<ImportResult> {
    let db = path
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or(default_ccswitch_db_path()?);

    if !db.exists() {
        let candidates = ccswitch_db_candidates()?
            .into_iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n- ");
        return Err(CodexxError::Config(format!(
            "cc-switch 数据库不存在: {}\n已检查候选路径:\n- {}",
            db.display(),
            candidates
        )));
    }

    let conn = Connection::open_with_flags(
        &db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| {
        CodexxError::Database(format!("打开 cc-switch 数据库失败 {}: {e}", db.display()))
    })?;

    let rows_vec = read_ccswitch_codex_rows(&conn)?;

    let mut global_sections: HashMap<String, CcSwitchCodexSection> = HashMap::new();
    for row in &rows_vec {
        if is_official_ccswitch_row(row) {
            continue;
        }
        let Ok(settings) = serde_json::from_str::<Value>(&row.settings_config) else {
            continue;
        };
        let Some(config_text) = settings.get("config").and_then(Value::as_str) else {
            continue;
        };
        for section in codex_sections_from_config(config_text) {
            if !global_sections.contains_key(&section.id) {
                global_sections.insert(section.id.clone(), section);
            }
        }
    }

    let mut imported = 0usize;
    let mut added = 0usize;
    let mut updated = 0usize;
    let mut merged = 0usize;
    let mut skipped = 0usize;
    let mut warnings = Vec::new();
    let mut local_conn = open_db()?;
    let transaction = local_conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| CodexxError::Database(e.to_string()))?;

    for row in rows_vec {
        if is_official_ccswitch_row(&row) {
            skipped += 1;
            warnings.push(format!(
                "跳过 {} ({})：官方认证不作为第三方供应商导入",
                row.name, row.id
            ));
            continue;
        }
        match build_ccswitch_codex_provider(&row, &global_sections) {
            Some(provider) => {
                let provider = normalize_saved_provider(provider)?;
                let result = upsert_provider_on_connection(
                    &transaction,
                    provider,
                    ProviderUpsertMode::Imported,
                )?;
                match result.kind {
                    ProviderUpsertKind::Added => added += 1,
                    ProviderUpsertKind::Updated => updated += 1,
                    ProviderUpsertKind::Merged => merged += 1,
                }
                imported += 1;
            }
            None => {
                skipped += 1;
                warnings.push(format!(
                    "跳过 {} ({})：未找到可用 config/base_url，可能是官方登录或空模板",
                    row.name, row.id
                ));
            }
        }
    }
    transaction
        .commit()
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let providers = list_saved_providers_on_connection(&local_conn)?;

    Ok(ImportResult {
        imported,
        added,
        updated,
        merged,
        skipped,
        warnings,
        providers,
    })
}

fn default_codex_dir() -> Result<PathBuf> {
    if let Ok(value) = std::env::var("CODEX_HOME") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    Ok(home_dir()?.join(".codex"))
}

fn resolve_codex_dir(config_dir: Option<String>) -> Result<PathBuf> {
    match config_dir
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(path) => Ok(PathBuf::from(path)),
        None => default_codex_dir(),
    }
}

fn config_path(codex_dir: &Path) -> PathBuf {
    codex_dir.join("config.toml")
}

fn auth_path(codex_dir: &Path) -> PathBuf {
    codex_dir.join("auth.json")
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

fn detected_live_custom_provider(codex_dir: &Path) -> Result<Option<SavedProvider>> {
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

fn agents_path(codex_dir: &Path) -> PathBuf {
    codex_dir.join(AGENTS_FILENAME)
}

fn read_ccswitch_official_auth_inner(
    path: Option<String>,
) -> Result<Option<OfficialAuthCandidate>> {
    let db = path
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or(default_ccswitch_db_path()?);

    if !db.exists() {
        return Ok(None);
    }

    let conn = Connection::open_with_flags(
        &db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| {
        CodexxError::Database(format!("打开 cc-switch 数据库失败 {}: {e}", db.display()))
    })?;

    let mut stmt = conn
        .prepare(
            "SELECT id, name, settings_config FROM providers
             WHERE app_type = 'codex' AND (id = 'codex-official' OR category = 'official')
             ORDER BY CASE WHEN id = 'codex-official' THEN 0 ELSE 1 END
             LIMIT 1",
        )
        .map_err(|e| CodexxError::Database(e.to_string()))?;

    let mut rows = stmt
        .query([])
        .map_err(|e| CodexxError::Database(e.to_string()))?;

    let Some(row) = rows
        .next()
        .map_err(|e| CodexxError::Database(e.to_string()))?
    else {
        return Ok(None);
    };

    let id: String = row
        .get(0)
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let name: String = row
        .get(1)
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let settings_config: String = row
        .get(2)
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let settings: Value = serde_json::from_str(&settings_config).map_err(|e| {
        CodexxError::Database(format!("cc-switch official settings JSON 解析失败: {e}"))
    })?;

    let auth = settings
        .get("auth")
        .cloned()
        .filter(|value| value.is_object())
        .ok_or_else(|| {
            CodexxError::Database("cc-switch official provider 缺少 auth object".to_string())
        })?;

    let model = settings
        .get("config")
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<DocumentMut>().ok())
        .and_then(|doc| string_value(&doc, "model"));

    let auth_json = serde_json::to_string_pretty(&auth)
        .map_err(|e| CodexxError::Database(format!("官方 auth JSON 格式化失败: {e}")))?;

    Ok(Some(OfficialAuthCandidate {
        auth_json,
        model,
        source: format!("cc-switch:{name}:{id}"),
    }))
}

fn read_to_string_if_exists(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(path).map_err(|e| io_err(path, e))
}

fn parse_toml_document(path: &Path, text: &str) -> Result<DocumentMut> {
    if text.trim().is_empty() {
        return Ok(DocumentMut::new());
    }
    text.parse::<DocumentMut>().map_err(|e| CodexxError::Toml {
        path: path.display().to_string(),
        message: e.to_string(),
    })
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    }
    let tmp = path.with_extension(format!(
        "tmp.{}.{}.{}",
        std::process::id(),
        Local::now().timestamp_nanos_opt().unwrap_or_default(),
        WRITE_COUNTER.fetch_add(1, Ordering::Relaxed),
    ));
    {
        let mut file = fs::File::create(&tmp).map_err(|e| io_err(&tmp, e))?;
        file.write_all(bytes).map_err(|e| io_err(&tmp, e))?;
        file.sync_all().map_err(|e| io_err(&tmp, e))?;
    }
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path).map_err(|e| io_err(path, e))?;
    }
    fs::rename(&tmp, path).map_err(|e| io_err(path, e))?;
    Ok(())
}

fn write_text(path: &Path, text: &str) -> Result<()> {
    atomic_write(path, text.as_bytes())
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let text = serde_json::to_string_pretty(value).map_err(|e| json_err(path, e))?;
    write_text(path, &(text + "\n"))
}

fn managed_agents_bounds(content: &str) -> Result<Option<(usize, usize)>> {
    let begins = content
        .match_indices(AGENTS_MANAGED_BEGIN)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let ends = content
        .match_indices(AGENTS_MANAGED_END)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();

    if begins.is_empty() && ends.is_empty() {
        return Ok(None);
    }
    if begins.len() != 1 || ends.len() != 1 || begins[0] >= ends[0] {
        return Err(CodexxError::Config(
            "AGENTS.md 中的 Codex-X 受管区块标记不完整或重复，请先修复 BEGIN/END 标记".to_string(),
        ));
    }
    Ok(Some((begins[0], ends[0] + AGENTS_MANAGED_END.len())))
}

fn remove_managed_agents_block(content: &str) -> Result<(String, bool)> {
    let Some((start, end)) = managed_agents_bounds(content)? else {
        return Ok((content.to_string(), false));
    };
    let before = content[..start].trim_end();
    let after = content[end..].trim_start();
    let merged = match (before.is_empty(), after.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("{}\n", before),
        (true, false) => format!("{}\n", after.trim_end()),
        (false, false) => format!("{}\n\n{}\n", before, after.trim_end()),
    };
    Ok((merged, true))
}

fn managed_agents_template_key_from_content(content: &str) -> Option<String> {
    let (start, end) = managed_agents_bounds(content).ok().flatten()?;
    content[start..end].lines().find_map(|line| {
        line.trim()
            .strip_prefix(AGENTS_TEMPLATE_PREFIX)
            .and_then(|value| value.strip_suffix("-->"))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn managed_agents_template_key(codex_dir: &Path) -> Result<Option<String>> {
    let path = agents_path(codex_dir);
    let content = read_to_string_if_exists(&path)?;
    Ok(managed_agents_template_key_from_content(&content))
}

fn install_managed_agents_block(codex_dir: &Path, template_key: &str, content: &str) -> Result<()> {
    let path = agents_path(codex_dir);
    let existing = read_to_string_if_exists(&path)?;
    let (base, _) = remove_managed_agents_block(&existing)?;
    let managed = format!(
        "{AGENTS_MANAGED_BEGIN}\n{AGENTS_TEMPLATE_PREFIX} {template_key} -->\n{}\n{AGENTS_MANAGED_END}",
        content.trim()
    );
    let next = if base.trim().is_empty() {
        format!("{managed}\n")
    } else {
        format!("{}\n\n{managed}\n", base.trim_end())
    };
    write_text(&path, &next)
}

fn uninstall_managed_agents_block(codex_dir: &Path) -> Result<bool> {
    let path = agents_path(codex_dir);
    if !path.exists() {
        return Ok(false);
    }
    let existing = read_to_string_if_exists(&path)?;
    let (next, removed) = remove_managed_agents_block(&existing)?;
    if !removed {
        return Ok(false);
    }
    if next.trim().is_empty() {
        fs::remove_file(&path).map_err(|e| io_err(&path, e))?;
    } else {
        write_text(&path, &next)?;
    }
    Ok(true)
}

fn backup_root() -> Result<PathBuf> {
    Ok(app_home()?.join("backups"))
}

fn action_backup_root(codex_dir: &Path) -> Result<PathBuf> {
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

fn create_backup(codex_dir: &Path, action: &str) -> Result<Option<String>> {
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
    fs::create_dir_all(&dir).map_err(|e| io_err(&dir, e))?;

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

fn backups() -> Result<Vec<BackupEntry>> {
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

fn latest_backup() -> Result<Option<BackupEntry>> {
    Ok(backups()?.into_iter().next())
}

fn redacted_auth_preview(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).map_err(|e| io_err(path, e))?;
    let mut value: Value = serde_json::from_str(&text).map_err(|e| json_err(path, e))?;
    if let Some(obj) = value.as_object_mut() {
        for (key, val) in obj.iter_mut() {
            let lower = key.to_ascii_lowercase();
            if lower.contains("key")
                || lower.contains("token")
                || lower.contains("secret")
                || lower.contains("password")
            {
                if val.as_str().is_some_and(|s| !s.trim().is_empty()) {
                    *val = Value::String("••••••••".to_string());
                }
            }
        }
    }
    Ok(Some(value))
}

fn auth_has_material(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let text = fs::read_to_string(path).map_err(|e| io_err(path, e))?;
    let value: Value = serde_json::from_str(&text).map_err(|e| json_err(path, e))?;
    let Some(obj) = value.as_object() else {
        return Ok(false);
    };
    Ok(obj.iter().any(|(key, value)| {
        if key == "auth_mode" {
            return false;
        }
        match value {
            Value::Null => false,
            Value::String(s) => !s.trim().is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
            _ => true,
        }
    }))
}

fn string_value(doc: &DocumentMut, key: &str) -> Option<String> {
    doc.get(key)
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

fn bool_from_item(item: Option<&Item>) -> Option<bool> {
    item.and_then(|i| i.as_bool())
}

fn extract_providers(doc: &DocumentMut, current: Option<&str>) -> Vec<ProviderSummary> {
    let Some(providers) = doc.get("model_providers").and_then(|i| i.as_table()) else {
        return Vec::new();
    };

    providers
        .iter()
        .filter_map(|(id, item)| {
            let table = item.as_table()?;
            Some(ProviderSummary {
                id: id.to_string(),
                name: table
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string),
                base_url: table
                    .get("base_url")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string),
                wire_api: table
                    .get("wire_api")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string),
                requires_openai_auth: bool_from_item(table.get("requires_openai_auth")),
                is_current: current.is_some_and(|c| c == id),
            })
        })
        .collect()
}

fn normalized_provider_toml_for_match(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .filter(|line| !line.trim_start().starts_with("experimental_bearer_token"))
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn active_saved_provider_id_from_config(
    config_text: &str,
    providers: &[SavedProvider],
) -> Option<String> {
    let live = normalized_provider_toml_for_match(config_text);
    if live.is_empty() {
        return None;
    }
    let matches = providers
        .iter()
        .filter(|provider| {
            provider
                .toml_config
                .as_deref()
                .is_some_and(|toml| normalized_provider_toml_for_match(toml) == live)
        })
        .collect::<Vec<_>>();
    (matches.len() == 1).then(|| matches[0].id.clone())
}

fn build_state(codex_dir: PathBuf) -> Result<CodexState> {
    let cfg = config_path(&codex_dir);
    let auth = auth_path(&codex_dir);
    let text = read_to_string_if_exists(&cfg)?;
    let doc = parse_toml_document(&cfg, &text)?;
    let model = string_value(&doc, "model");
    let model_provider = string_value(&doc, "model_provider");
    let instruction_file = string_value(&doc, "model_instructions_file");
    let model_template_key = instruction_file
        .as_deref()
        .map(prompt_template_key_for_instruction)
        .transpose()?
        .flatten();
    let agents_template_key = managed_agents_template_key(&codex_dir)?;
    let (instruction_injection_mode, instruction_template_key) =
        if let Some(key) = agents_template_key {
            (Some("append".to_string()), Some(key))
        } else if let Some(key) = model_template_key {
            (Some("replace".to_string()), Some(key))
        } else {
            (None, None)
        };
    let instruction_enabled = instruction_template_key.is_some();
    let providers = extract_providers(&doc, model_provider.as_deref());
    let active_saved_provider_id = if model_provider.as_deref() == Some("openai") {
        None
    } else {
        active_saved_provider_id_from_config(&text, &list_saved_providers_inner()?)
    };

    Ok(CodexState {
        codex_dir: codex_dir.display().to_string(),
        config_path: cfg.display().to_string(),
        auth_path: auth.display().to_string(),
        config_exists: cfg.exists(),
        auth_exists: auth.exists(),
        official_auth_available: auth_has_material(&auth)?,
        model,
        model_provider,
        instruction_file,
        instruction_enabled,
        instruction_injection_mode,
        instruction_template_key,
        agents_path: agents_path(&codex_dir).display().to_string(),
        active_saved_provider_id,
        providers,
        config_text: text,
        auth_preview: redacted_auth_preview(&auth)?,
        auth_text: read_to_string_if_exists(&auth)?,
        last_backup: latest_backup()?,
    })
}

fn current_model_provider(codex_dir: &Path, explicit: Option<String>) -> Result<String> {
    if let Some(provider) = explicit
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Ok(provider);
    }
    let cfg = config_path(codex_dir);
    let text = read_to_string_if_exists(&cfg)?;
    let doc = parse_toml_document(&cfg, &text)?;
    Ok(string_value(&doc, "model_provider").unwrap_or_else(|| "openai".to_string()))
}

fn is_rollout_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
}

fn collect_rollout_paths(root: &Path, out: &mut Vec<PathBuf>, warnings: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(root) else {
        if root.exists() {
            warnings.push(format!("无法读取目录: {}", root.display()));
        }
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if path.is_dir() {
            collect_rollout_paths(&path, out, warnings);
        } else if is_rollout_file(&path) {
            out.push(path);
        }
    }
}

fn split_line_ending(segment: &str) -> (&str, &str) {
    if let Some(line) = segment.strip_suffix("\r\n") {
        (line, "\r\n")
    } else if let Some(line) = segment.strip_suffix('\n') {
        (line, "\n")
    } else {
        (segment, "")
    }
}

fn normalize_workspace_path(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with(r"\\?\unc\") {
        return Some(format!(r"\\{}", trimmed[8..].replace('/', r"\")));
    }
    if trimmed.starts_with(r"\\?\") {
        return Some(trimmed[4..].replace('\\', "/"));
    }
    Some(trimmed.to_string())
}

fn scan_rollouts(
    codex_dir: &Path,
    target_provider: &str,
    rewrite: bool,
    only_session_ids: Option<&HashSet<String>>,
) -> Result<RolloutScan> {
    let mut paths = Vec::new();
    let mut scan = RolloutScan::default();
    for dir in ["sessions", "archived_sessions"] {
        collect_rollout_paths(&codex_dir.join(dir), &mut paths, &mut scan.warnings);
    }
    paths.sort();
    scan.rollout_files = paths.len();

    for path in paths {
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e)
                if matches!(e.kind(), std::io::ErrorKind::PermissionDenied)
                    || matches!(e.raw_os_error(), Some(32 | 33)) =>
            {
                scan.warnings
                    .push(format!("跳过被占用/无权限会话文件: {}", path.display()));
                continue;
            }
            Err(e) => return Err(io_err(&path, e)),
        };
        let mut next = String::with_capacity(text.len());
        let mut file_has_meta = false;
        let mut file_changed = false;
        let has_user_event = text.contains("\"user_message\"") || text.contains("\"user_input\"");
        let mut first_thread_id: Option<String> = None;
        let mut first_cwd: Option<String> = None;

        for segment in text.split_inclusive('\n') {
            let (line, ending) = split_line_ending(segment);
            let mut next_line = line.to_string();
            if !line.trim().is_empty() {
                if let Ok(mut record) = serde_json::from_str::<Value>(line) {
                    if record.get("type").and_then(Value::as_str) == Some("session_meta") {
                        file_has_meta = true;
                        scan.session_meta_count += 1;
                        if let Some(payload) =
                            record.get_mut("payload").and_then(Value::as_object_mut)
                        {
                            if first_thread_id.is_none() {
                                first_thread_id = payload
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .map(ToString::to_string);
                            }
                            let selected_for_rewrite = only_session_ids
                                .map(|ids| {
                                    payload
                                        .get("id")
                                        .and_then(Value::as_str)
                                        .map(|id| ids.contains(id))
                                        .unwrap_or(false)
                                })
                                .unwrap_or(true);
                            if first_cwd.is_none() {
                                first_cwd = payload
                                    .get("cwd")
                                    .and_then(Value::as_str)
                                    .and_then(normalize_workspace_path);
                            }
                            if payload.get("model_provider").and_then(Value::as_str)
                                != Some(target_provider)
                            {
                                scan.mismatched_session_meta += 1;
                                if selected_for_rewrite {
                                    file_changed = true;
                                }
                                if rewrite && selected_for_rewrite {
                                    payload.insert(
                                        "model_provider".to_string(),
                                        json!(target_provider),
                                    );
                                    next_line = serde_json::to_string(&record)
                                        .map_err(|e| json_err(&path, e))?;
                                }
                            }
                        }
                    }
                }
            }
            next.push_str(&next_line);
            next.push_str(ending);
        }
        if file_has_meta {
            if has_user_event {
                if let Some(id) = &first_thread_id {
                    scan.thread_ids_with_user_events.insert(id.clone());
                }
            }
            if let (Some(id), Some(cwd)) = (&first_thread_id, &first_cwd) {
                scan.cwd_by_thread_id.insert(id.clone(), cwd.clone());
            }
        }
        if file_changed {
            scan.mismatched_rollouts += 1;
            if rewrite {
                scan.changed_files.push((path, next));
            }
        }
    }
    Ok(scan)
}

fn expand_sqlite_home(codex_dir: &Path, value: &str) -> PathBuf {
    let trimmed = value.trim();
    if trimmed == "~" {
        return home_dir().unwrap_or_else(|_| codex_dir.to_path_buf());
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|_| PathBuf::from(trimmed));
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        path
    } else {
        codex_dir.join(path)
    }
}

fn configured_sqlite_home(codex_dir: &Path) -> Option<PathBuf> {
    let config = config_path(codex_dir);
    let configured = fs::read_to_string(&config)
        .ok()
        .and_then(|text| text.parse::<DocumentMut>().ok())
        .and_then(|doc| string_value(&doc, "sqlite_home"));
    #[cfg(test)]
    let environment = None;
    #[cfg(not(test))]
    let environment = std::env::var("CODEX_SQLITE_HOME").ok();
    configured
        .or(environment)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| expand_sqlite_home(codex_dir, &value))
}

fn sqlite_storage_roots(codex_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(configured) = configured_sqlite_home(codex_dir) {
        roots.push(configured);
    }
    roots.push(codex_dir.to_path_buf());
    roots.push(codex_dir.join("sqlite"));
    let mut seen = HashSet::new();
    roots.retain(|path| seen.insert(path.to_string_lossy().to_string()));
    roots
}

fn is_codex_sqlite_storage_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    if name == "codex-dev.db" {
        return true;
    }
    let Some(stem) = name.strip_suffix(".sqlite") else {
        return false;
    };
    let Some((kind, version)) = stem.rsplit_once('_') else {
        return false;
    };
    !version.is_empty()
        && version.chars().all(|ch| ch.is_ascii_digit())
        && matches!(
            kind,
            "state" | "logs" | "memories" | "goals" | "thread_history"
        )
}

fn all_codex_sqlite_paths(codex_dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for root in sqlite_storage_roots(codex_dir) {
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        let mut root_paths = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && is_codex_sqlite_storage_file(path))
            .collect::<Vec<_>>();
        root_paths.sort();
        for path in root_paths {
            if seen.insert(path.to_string_lossy().to_string()) {
                paths.push(path);
            }
        }
    }
    paths
}

fn sqlite_state_version(path: &Path) -> Option<u64> {
    path.file_name()
        .and_then(|value| value.to_str())
        .and_then(|name| name.strip_prefix("state_"))
        .and_then(|value| value.strip_suffix(".sqlite"))
        .and_then(|value| value.parse::<u64>().ok())
}

fn sqlite_candidate_paths(codex_dir: &Path) -> Vec<PathBuf> {
    for root in sqlite_storage_roots(codex_dir) {
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        let mut candidates = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && sqlite_state_version(path).is_some())
            .collect::<Vec<_>>();
        candidates.sort_by(|a, b| {
            sqlite_state_version(b)
                .cmp(&sqlite_state_version(a))
                .then_with(|| b.file_name().cmp(&a.file_name()))
        });
        for path in candidates {
            let Ok(conn) = Connection::open_with_flags(
                &path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            ) else {
                continue;
            };
            if sqlite_has_table(&conn, "threads").unwrap_or(false) {
                return vec![path];
            }
        }
    }
    Vec::new()
}

fn sqlite_has_table(conn: &Connection, table: &str) -> Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
        [table],
        |_| Ok(()),
    )
    .map(|_| true)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(false),
        other => Err(CodexxError::Database(other.to_string())),
    })
}

fn table_column_set(conn: &Connection, table: &str) -> Result<HashSet<String>> {
    let mut stmt = conn
        .prepare(&format!(
            "PRAGMA table_info(\"{}\")",
            table.replace('"', "\"\"")
        ))
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let mut cols = HashSet::new();
    for row in rows {
        cols.insert(row.map_err(|e| CodexxError::Database(e.to_string()))?);
    }
    Ok(cols)
}

fn sqlite_subagent_thread_ids(
    conn: &Connection,
    thread_cols: &HashSet<String>,
) -> Result<HashSet<String>> {
    let mut edge_child_ids = HashSet::new();

    if sqlite_has_table(conn, "thread_spawn_edges")? {
        let edge_cols = table_column_set(conn, "thread_spawn_edges")?;
        if edge_cols.contains("child_thread_id") {
            let mut stmt = conn
                .prepare(
                    "SELECT DISTINCT e.child_thread_id
                     FROM thread_spawn_edges e
                     INNER JOIN threads t ON t.id = e.child_thread_id",
                )
                .map_err(|e| CodexxError::Database(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| CodexxError::Database(e.to_string()))?;
            for row in rows {
                edge_child_ids.insert(row.map_err(|e| CodexxError::Database(e.to_string()))?);
            }
        }
    }

    let mut ids = edge_child_ids;
    if thread_cols.contains("thread_source") || thread_cols.contains("source") {
        let thread_source_col = sql_select_column(thread_cols, "thread_source", "NULL");
        let source_col = sql_select_column(thread_cols, "source", "NULL");
        let query = format!("SELECT \"id\", {thread_source_col}, {source_col} FROM threads");
        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        for row in rows {
            let (id, thread_source, source) =
                row.map_err(|e| CodexxError::Database(e.to_string()))?;
            if let Some(thread_source) = thread_source
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                if thread_source.eq_ignore_ascii_case("subagent") {
                    ids.insert(id);
                } else {
                    ids.remove(&id);
                }
                continue;
            }

            if let Some(source) = source
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                let source_is_subagent = source.eq_ignore_ascii_case("subagent")
                    || serde_json::from_str::<Value>(source)
                        .ok()
                        .is_some_and(|value| {
                            value
                                .as_object()
                                .is_some_and(|object| object.contains_key("subagent"))
                        });
                if source_is_subagent {
                    ids.insert(id);
                } else {
                    ids.remove(&id);
                }
            }
        }
    }

    Ok(ids)
}

fn scan_sqlite(codex_dir: &Path, target_provider: &str) -> Result<SqliteScan> {
    let mut scan = SqliteScan::default();
    for path in sqlite_candidate_paths(codex_dir) {
        let conn = match Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(conn) => conn,
            Err(e) => {
                scan.warnings
                    .push(format!("无法读取 SQLite: {} ({e})", path.display()));
                continue;
            }
        };
        if !sqlite_has_table(&conn, "threads")? {
            continue;
        }
        let cols = table_column_set(&conn, "threads")?;
        if !cols.contains("model_provider") {
            scan.warnings.push(format!(
                "SQLite threads 缺少 model_provider 字段: {}",
                path.display()
            ));
            continue;
        }
        scan.sqlite_dbs += 1;
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM threads", [], |row| row.get(0))
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        let mismatch: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM threads WHERE COALESCE(model_provider, '') <> ?1",
                [target_provider],
                |row| row.get(0),
            )
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        let subagent_threads = if cols.contains("id") {
            sqlite_subagent_thread_ids(&conn, &cols)?
                .len()
                .min(total.max(0) as usize)
        } else {
            0
        };
        scan.sqlite_threads += total.max(0) as usize;
        scan.top_level_threads += (total.max(0) as usize).saturating_sub(subagent_threads);
        scan.subagent_threads += subagent_threads;
        scan.mismatched_threads += mismatch.max(0) as usize;
    }
    Ok(scan)
}

fn list_session_previews(
    codex_dir: &Path,
    target_provider: &str,
    limit: usize,
) -> Result<(Vec<SessionPreview>, Vec<String>)> {
    let mut sessions = Vec::new();
    let mut warnings = Vec::new();
    let mut seen = HashSet::new();

    for path in sqlite_candidate_paths(codex_dir) {
        let conn = match Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(conn) => conn,
            Err(e) => {
                warnings.push(format!("无法读取会话数据库: {} ({e})", path.display()));
                continue;
            }
        };
        if !sqlite_has_table(&conn, "threads")? {
            continue;
        }
        let cols = table_column_set(&conn, "threads")?;
        if !cols.contains("id") {
            continue;
        }
        let subagent_thread_ids = sqlite_subagent_thread_ids(&conn, &cols)?;

        let title_col = sql_select_column(&cols, "title", "NULL");
        let first_message_col = sql_select_column(&cols, "first_user_message", "NULL");
        let preview_col = sql_select_column(&cols, "preview", "NULL");
        let provider_col = sql_select_column(&cols, "model_provider", "NULL");
        let model_col = sql_select_column(&cols, "model", "NULL");
        let cwd_col = sql_select_column(&cols, "cwd", "NULL");
        let rollout_col = sql_select_column(&cols, "rollout_path", "NULL");
        let updated_ms_col = sql_select_column(&cols, "updated_at_ms", "NULL");
        let updated_col = sql_select_column(&cols, "updated_at", "NULL");
        let archived_col = sql_select_column(&cols, "archived", "0");
        let has_user_event_col = sql_select_column(&cols, "has_user_event", "0");
        let order_col = if cols.contains("recency_at_ms") {
            "\"recency_at_ms\""
        } else if cols.contains("updated_at_ms") {
            "\"updated_at_ms\""
        } else if cols.contains("updated_at") {
            "\"updated_at\""
        } else {
            "\"id\""
        };

        let query = format!(
            "SELECT \"id\", {title_col}, {first_message_col}, {preview_col}, {provider_col}, {model_col}, {cwd_col}, {rollout_col}, {updated_ms_col}, {updated_col}, {archived_col}, {has_user_event_col} FROM threads ORDER BY {order_col} DESC"
        );
        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let title: Option<String> = row.get(1)?;
                let first_message: Option<String> = row.get(2)?;
                let preview: Option<String> = row.get(3)?;
                let model_provider: Option<String> = row.get(4)?;
                let model: Option<String> = row.get(5)?;
                let cwd: Option<String> = row.get(6)?;
                let rollout_path: Option<String> = row.get(7)?;
                let updated_at_ms: Option<i64> = row.get(8)?;
                let updated_at: Option<i64> = row.get(9)?;
                let archived: i64 = row.get(10)?;
                let has_user_event: i64 = row.get(11)?;
                let clean_title = [title, first_message, preview]
                    .into_iter()
                    .flatten()
                    .map(|v| v.trim().to_string())
                    .find(|v| !v.is_empty())
                    .unwrap_or_else(|| format!("会话 {}", id.chars().take(8).collect::<String>()));
                let normalized_provider = model_provider
                    .as_ref()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty());
                Ok(SessionPreview {
                    id,
                    title: clean_title,
                    model_provider: normalized_provider.clone(),
                    model: model.and_then(|v| {
                        let v = v.trim().to_string();
                        (!v.is_empty()).then_some(v)
                    }),
                    cwd: cwd.and_then(|v| {
                        let v = v.trim().to_string();
                        (!v.is_empty()).then_some(v)
                    }),
                    rollout_path: rollout_path.and_then(|v| {
                        let v = v.trim().to_string();
                        (!v.is_empty()).then_some(v)
                    }),
                    updated_at_ms: updated_at_ms.or_else(|| updated_at.map(|v| v * 1000)),
                    archived: archived != 0,
                    has_user_event: has_user_event != 0,
                    needs_sync: normalized_provider.as_deref() != Some(target_provider),
                })
            })
            .map_err(|e| CodexxError::Database(e.to_string()))?;

        for row in rows {
            let session = row.map_err(|e| CodexxError::Database(e.to_string()))?;
            if subagent_thread_ids.contains(&session.id) {
                continue;
            }
            if seen.insert(session.id.clone()) {
                sessions.push(session);
                if sessions.len() >= limit.max(1) {
                    break;
                }
            }
        }
    }

    sessions.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms));
    sessions.truncate(limit);
    Ok((sessions, warnings))
}

fn provider_sync_backup_root(codex_dir: &Path) -> PathBuf {
    codex_dir.join("backups_state").join("provider-sync")
}

fn copy_file_to_backup(codex_dir: &Path, backup_dir: &Path, source: &Path) -> Result<()> {
    if !source.exists() {
        return Ok(());
    }
    let relative = source.strip_prefix(codex_dir).unwrap_or(source);
    let target = backup_dir.join(relative);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    }
    fs::copy(source, &target).map_err(|e| io_err(source, e))?;
    Ok(())
}

fn prune_provider_sync_backups(codex_dir: &Path) -> Result<()> {
    let root = provider_sync_backup_root(codex_dir);
    if !root.exists() {
        return Ok(());
    }
    let mut dirs = Vec::new();
    for entry in fs::read_dir(&root).map_err(|e| io_err(&root, e))? {
        let entry = entry.map_err(|e| io_err(&root, e))?;
        let path = entry.path();
        if path.is_dir() && path.join("metadata.json").exists() {
            dirs.push(path);
        }
    }
    dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    for path in dirs.into_iter().skip(5) {
        let _ = fs::remove_dir_all(path);
    }
    Ok(())
}

fn create_provider_sync_backup(
    codex_dir: &Path,
    target_provider: &str,
    changed_rollouts: &[PathBuf],
) -> Result<PathBuf> {
    let root = provider_sync_backup_root(codex_dir);
    fs::create_dir_all(&root).map_err(|e| io_err(&root, e))?;
    let mut backup_dir = root.join(Local::now().format("%Y%m%d%H%M%S").to_string());
    let mut suffix = 0;
    while backup_dir.exists() {
        suffix += 1;
        backup_dir = root.join(format!("{}-{suffix}", Local::now().format("%Y%m%d%H%M%S")));
    }
    fs::create_dir_all(&backup_dir).map_err(|e| io_err(&backup_dir, e))?;

    for name in [
        "config.toml",
        ".codex-global-state.json",
        ".codex-global-state.json.bak",
    ] {
        copy_file_to_backup(codex_dir, &backup_dir, &codex_dir.join(name))?;
    }
    for path in sqlite_candidate_paths(codex_dir) {
        for candidate in [
            path.clone(),
            PathBuf::from(format!("{}-wal", path.display())),
            PathBuf::from(format!("{}-shm", path.display())),
        ] {
            copy_file_to_backup(codex_dir, &backup_dir, &candidate)?;
        }
    }
    for path in changed_rollouts {
        copy_file_to_backup(codex_dir, &backup_dir, path)?;
    }
    write_json(
        &backup_dir.join("metadata.json"),
        &json!({
            "version": 1,
            "namespace": "provider-sync",
            "managedBy": "Codex-X session manager",
            "codexHome": codex_dir.display().to_string(),
            "targetProvider": target_provider,
            "createdAt": Local::now().to_rfc3339(),
            "changedRolloutFiles": changed_rollouts.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        }),
    )?;
    prune_provider_sync_backups(codex_dir)?;
    Ok(backup_dir)
}

fn update_selected_thread_provider(
    tx: &rusqlite::Transaction<'_>,
    cols: &HashSet<String>,
    target_provider: &str,
    selected_ids: &HashSet<String>,
    thread_ids_with_user_events: &HashSet<String>,
    cwd_by_thread_id: &HashMap<String, String>,
) -> Result<usize> {
    let mut updated = 0usize;
    for id in selected_ids {
        updated += tx
            .execute(
                "UPDATE threads SET model_provider = ?1 WHERE id = ?2 AND COALESCE(model_provider, '') <> ?1",
                (target_provider, id),
            )
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        if cols.contains("has_user_event") && thread_ids_with_user_events.contains(id) {
            updated += tx
                .execute(
                    "UPDATE threads SET has_user_event = 1 WHERE id = ?1 AND COALESCE(has_user_event, 0) <> 1",
                    [id],
                )
                .map_err(|e| CodexxError::Database(e.to_string()))?;
        }
        if cols.contains("cwd") {
            if let Some(cwd) = cwd_by_thread_id.get(id) {
                updated += tx
                    .execute(
                        "UPDATE threads SET cwd = ?1 WHERE id = ?2 AND COALESCE(cwd, '') <> ?1",
                        (cwd, id),
                    )
                    .map_err(|e| CodexxError::Database(e.to_string()))?;
            }
        }
    }
    Ok(updated)
}

fn apply_sqlite_provider_sync_selected(
    codex_dir: &Path,
    target_provider: &str,
    selected_ids: &HashSet<String>,
    thread_ids_with_user_events: &HashSet<String>,
    cwd_by_thread_id: &HashMap<String, String>,
) -> Result<usize> {
    let mut updated = 0usize;
    if selected_ids.is_empty() {
        return Ok(0);
    }
    for path in sqlite_candidate_paths(codex_dir) {
        let mut conn = match Connection::open(&path) {
            Ok(conn) => conn,
            Err(e) => {
                return Err(CodexxError::Database(format!(
                    "打开 SQLite 失败 {}: {e}",
                    path.display()
                )))
            }
        };
        if !sqlite_has_table(&conn, "threads")? {
            continue;
        }
        let cols = table_column_set(&conn, "threads")?;
        if !cols.contains("model_provider") {
            continue;
        }
        let tx = conn
            .transaction()
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        updated += update_selected_thread_provider(
            &tx,
            &cols,
            target_provider,
            selected_ids,
            thread_ids_with_user_events,
            cwd_by_thread_id,
        )?;
        tx.commit()
            .map_err(|e| CodexxError::Database(e.to_string()))?;
    }
    Ok(updated)
}

fn apply_sqlite_provider_sync(
    codex_dir: &Path,
    target_provider: &str,
    thread_ids_with_user_events: &HashSet<String>,
    cwd_by_thread_id: &HashMap<String, String>,
) -> Result<usize> {
    let mut updated = 0usize;
    for path in sqlite_candidate_paths(codex_dir) {
        let mut conn = match Connection::open(&path) {
            Ok(conn) => conn,
            Err(e) => {
                return Err(CodexxError::Database(format!(
                    "打开 SQLite 失败 {}: {e}",
                    path.display()
                )))
            }
        };
        if !sqlite_has_table(&conn, "threads")? {
            continue;
        }
        let cols = table_column_set(&conn, "threads")?;
        if !cols.contains("model_provider") {
            continue;
        }
        let tx = conn
            .transaction()
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        updated += tx
            .execute(
                "UPDATE threads SET model_provider = ?1 WHERE COALESCE(model_provider, '') <> ?1",
                [target_provider],
            )
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        if cols.contains("has_user_event") {
            for id in thread_ids_with_user_events {
                updated += tx
                    .execute("UPDATE threads SET has_user_event = 1 WHERE id = ?1 AND COALESCE(has_user_event, 0) <> 1", [id])
                    .map_err(|e| CodexxError::Database(e.to_string()))?;
            }
        }
        if cols.contains("cwd") {
            for (id, cwd) in cwd_by_thread_id {
                updated += tx
                    .execute(
                        "UPDATE threads SET cwd = ?1 WHERE id = ?2 AND COALESCE(cwd, '') <> ?1",
                        (cwd, id),
                    )
                    .map_err(|e| CodexxError::Database(e.to_string()))?;
            }
        }
        tx.commit()
            .map_err(|e| CodexxError::Database(e.to_string()))?;
    }
    Ok(updated)
}

fn diagnostic_item(
    key: &str,
    label: &str,
    path: Option<&Path>,
    ok: bool,
    manual_when_missing: bool,
) -> DiagnosticItem {
    let status = if ok {
        "ok"
    } else if manual_when_missing {
        "manual"
    } else {
        "missing"
    };
    let message = match status {
        "ok" => "检测通过",
        "manual" => "需要手动选择",
        _ => "未找到",
    };
    DiagnosticItem {
        key: key.to_string(),
        label: label.to_string(),
        path: path.map(|p| p.display().to_string()),
        status: status.to_string(),
        message: message.to_string(),
    }
}

fn startup_diagnostics_inner(config_dir: Option<String>) -> Result<StartupDiagnostics> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    let config = config_path(&codex_dir);
    let auth = auth_path(&codex_dir);
    let sqlite_paths = sqlite_candidate_paths(&codex_dir);
    let codex_dir_ok = codex_dir.is_dir();
    let config_ok = config.is_file();
    let auth_ok = auth.is_file() && auth_has_material(&auth).unwrap_or(false);
    let sqlite_ok = !sqlite_paths.is_empty();

    let mut items = Vec::new();
    items.push(diagnostic_item(
        "codexHome",
        "CODEX_HOME",
        Some(&codex_dir),
        codex_dir_ok,
        true,
    ));
    items.push(diagnostic_item(
        "config",
        "config.toml",
        Some(&config),
        config_ok,
        false,
    ));
    items.push(diagnostic_item(
        "auth",
        "auth.json",
        Some(&auth),
        auth_ok,
        false,
    ));
    items.push(DiagnosticItem {
        key: "sqlite".to_string(),
        label: "SQLite 会话库".to_string(),
        path: sqlite_paths.first().map(|p| {
            if sqlite_paths.len() > 1 {
                format!("{} 等 {} 个", p.display(), sqlite_paths.len())
            } else {
                p.display().to_string()
            }
        }),
        status: if sqlite_ok { "ok" } else { "missing" }.to_string(),
        message: if sqlite_ok {
            "检测通过"
        } else {
            "未找到"
        }
        .to_string(),
    });

    let ok_count = items.iter().filter(|item| item.status == "ok").count();
    let needs_manual_select = !codex_dir_ok;
    let summary = if ok_count == items.len() {
        "Codex 环境检测通过".to_string()
    } else if needs_manual_select {
        "未找到 CODEX_HOME，需要手动选择 Codex 配置目录".to_string()
    } else {
        format!(
            "已检测到 {ok_count}/{} 项，缺失项不影响部分功能使用",
            items.len()
        )
    };

    Ok(StartupDiagnostics {
        codex_dir: codex_dir.display().to_string(),
        needs_manual_select,
        summary,
        items,
    })
}

fn session_sync_status_inner(
    config_dir: Option<String>,
    target_provider: Option<String>,
) -> Result<SessionSyncStatus> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    let target = current_model_provider(&codex_dir, target_provider)?;
    let rollouts = scan_rollouts(&codex_dir, &target, false, None)?;
    let sqlite = scan_sqlite(&codex_dir, &target)?;
    let session_limit = sqlite.top_level_threads.max(50).min(1000);
    let (sessions, session_warnings) = list_session_previews(&codex_dir, &target, session_limit)?;
    let mut warnings = rollouts.warnings;
    warnings.extend(sqlite.warnings);
    warnings.extend(session_warnings);
    Ok(SessionSyncStatus {
        codex_dir: codex_dir.display().to_string(),
        target_provider: target,
        rollout_files: rollouts.rollout_files,
        session_meta_count: rollouts.session_meta_count,
        mismatched_rollouts: rollouts.mismatched_rollouts,
        mismatched_session_meta: rollouts.mismatched_session_meta,
        sqlite_dbs: sqlite.sqlite_dbs,
        sqlite_threads: sqlite.sqlite_threads,
        top_level_threads: sqlite.top_level_threads,
        subagent_threads: sqlite.subagent_threads,
        mismatched_threads: sqlite.mismatched_threads,
        needs_sync: rollouts.mismatched_session_meta > 0 || sqlite.mismatched_threads > 0,
        backup_dir: None,
        warnings,
        sessions,
    })
}

struct SessionMaintenanceLock {
    file: fs::File,
}

impl Drop for SessionMaintenanceLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn acquire_session_maintenance_lock(codex_dir: &Path) -> Result<SessionMaintenanceLock> {
    let tmp_dir = codex_dir.join("tmp");
    fs::create_dir_all(&tmp_dir).map_err(|e| io_err(&tmp_dir, e))?;
    let legacy_lock = tmp_dir.join("provider-sync.lock");
    if legacy_lock.exists() {
        return Err(CodexxError::Config(format!(
            "会话维护正在进行: {}",
            legacy_lock.display()
        )));
    }
    let path = tmp_dir.join("session-maintenance.lock");
    if path.is_dir() {
        return Err(CodexxError::Config(format!(
            "检测到旧版会话维护锁，请确认没有其他 Codex-X 正在维护会话后删除: {}",
            path.display()
        )));
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| io_err(&path, e))?;
    file.try_lock()
        .map_err(|_| CodexxError::Config(format!("会话维护正在进行: {}", path.display())))?;
    file.set_len(0).map_err(|e| io_err(&path, e))?;
    write!(file, "pid={}\n", std::process::id()).map_err(|e| io_err(&path, e))?;
    file.sync_all().map_err(|e| io_err(&path, e))?;
    Ok(SessionMaintenanceLock { file })
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

fn collect_thread_spawn_edges(codex_dir: &Path) -> Result<Vec<(String, String)>> {
    let mut edges = HashSet::new();
    for path in sqlite_candidate_paths(codex_dir) {
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| CodexxError::Database(format!("读取会话关系失败 {}: {e}", path.display())))?;
        if !sqlite_has_table(&conn, "thread_spawn_edges")? {
            continue;
        }
        let cols = table_column_set(&conn, "thread_spawn_edges")?;
        if !cols.contains("parent_thread_id") || !cols.contains("child_thread_id") {
            continue;
        }
        let mut stmt = conn
            .prepare("SELECT parent_thread_id, child_thread_id FROM thread_spawn_edges")
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| CodexxError::Database(e.to_string()))?;
        for row in rows {
            edges.insert(row.map_err(|e| CodexxError::Database(e.to_string()))?);
        }
    }
    Ok(edges.into_iter().collect())
}

fn selected_session_roots(codex_dir: &Path, selected: &[String]) -> Result<Vec<String>> {
    let edges = collect_thread_spawn_edges(codex_dir)?;
    let selected_set = selected.iter().cloned().collect::<HashSet<_>>();
    let mut parents = HashMap::<String, Vec<String>>::new();
    for (parent, child) in edges {
        parents.entry(child).or_default().push(parent);
    }
    Ok(selected
        .iter()
        .filter(|id| {
            let mut pending = parents.get(*id).cloned().unwrap_or_default();
            let mut visited = HashSet::new();
            while let Some(parent) = pending.pop() {
                if !visited.insert(parent.clone()) {
                    continue;
                }
                if selected_set.contains(&parent) {
                    return false;
                }
                if let Some(next) = parents.get(&parent) {
                    pending.extend(next.iter().cloned());
                }
            }
            true
        })
        .cloned()
        .collect())
}

fn session_descendants_by_root(
    codex_dir: &Path,
    roots: &[String],
) -> Result<HashMap<String, HashSet<String>>> {
    let edges = collect_thread_spawn_edges(codex_dir)?;
    let mut children = HashMap::<String, Vec<String>>::new();
    for (parent, child) in edges {
        children.entry(parent).or_default().push(child);
    }
    let mut descendants = HashMap::new();
    for root in roots {
        let mut ids = HashSet::from([root.clone()]);
        let mut pending = vec![root.clone()];
        while let Some(parent) = pending.pop() {
            if let Some(next) = children.get(&parent) {
                for child in next {
                    if ids.insert(child.clone()) {
                        pending.push(child.clone());
                    }
                }
            }
        }
        descendants.insert(root.clone(), ids);
    }
    Ok(descendants)
}

fn active_session_ids_present(
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
    active_database_paths: &[PathBuf],
    target_provider: &str,
) -> Result<ActiveSessionStorageSnapshot> {
    let mut snapshot = ActiveSessionStorageSnapshot::default();
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
            let mut mismatch_stmt = conn
                .prepare("SELECT id FROM threads WHERE COALESCE(model_provider, '') <> ?1")
                .map_err(|error| CodexxError::Database(error.to_string()))?;
            let mismatches = mismatch_stmt
                .query_map([target_provider], |row| row.get::<_, String>(0))
                .map_err(|error| CodexxError::Database(error.to_string()))?;
            for id in mismatches {
                snapshot
                    .mismatched_ids
                    .insert(id.map_err(|error| CodexxError::Database(error.to_string()))?);
            }
        }
    }
    Ok(snapshot)
}

#[cfg(test)]
fn session_ids_with_descendants(codex_dir: &Path, roots: &[String]) -> Result<HashSet<String>> {
    Ok(session_descendants_by_root(codex_dir, roots)?
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

fn rollout_filename_matches_id(path: &Path, id: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.ends_with(&format!("-{id}.jsonl")) || name.ends_with(&format!("-{id}.jsonl.zst"))
        })
}

fn canonical_rollout_storage_roots(codex_dir: &Path) -> Vec<PathBuf> {
    [
        codex_dir.join("sessions"),
        codex_dir.join("archived_sessions"),
    ]
    .into_iter()
    .filter_map(|root| root.canonicalize().ok())
    .collect()
}

fn is_canonical_rollout_storage_path(codex_dir: &Path, path: &Path) -> bool {
    canonical_rollout_storage_roots(codex_dir)
        .iter()
        .any(|root| path.starts_with(root))
}

fn canonical_rollout_path(codex_dir: &Path, value: &str, id: &str) -> Result<Option<PathBuf>> {
    let raw = PathBuf::from(value.trim());
    let path = if raw.is_absolute() {
        raw
    } else {
        codex_dir.join(raw)
    };
    if !path.exists() {
        return Ok(None);
    }
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
    session_ids: &HashSet<String>,
) -> Result<HashSet<PathBuf>> {
    let mut paths = HashSet::new();
    for db_path in all_codex_sqlite_paths(codex_dir) {
        let conn = Connection::open_with_flags(
            &db_path,
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

fn remove_jsonl_session_entries(
    path: &Path,
    id_keys: &[&str],
    session_ids: &HashSet<String>,
) -> Result<usize> {
    if !path.exists() {
        return Ok(0);
    }
    let text = fs::read_to_string(path).map_err(|e| io_err(path, e))?;
    let (next, removed) = filter_jsonl_session_entries(&text, id_keys, session_ids);
    if removed > 0 {
        write_text(path, &next)?;
    }
    Ok(removed)
}

fn filter_jsonl_session_entries(
    text: &str,
    id_keys: &[&str],
    session_ids: &HashSet<String>,
) -> (String, usize) {
    let mut next = String::with_capacity(text.len());
    let mut removed = 0usize;
    for segment in text.split_inclusive('\n') {
        let (line, ending) = split_line_ending(segment);
        let matches = serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|value| {
                id_keys.iter().find_map(|key| {
                    value
                        .get(*key)
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
            })
            .is_some_and(|id| session_ids.contains(&id));
        if matches {
            removed += 1;
        } else {
            next.push_str(line);
            next.push_str(ending);
        }
    }
    (next, removed)
}

fn remove_session_index_entries(codex_dir: &Path, session_ids: &HashSet<String>) -> Result<usize> {
    remove_jsonl_session_entries(&codex_dir.join("session_index.jsonl"), &["id"], session_ids)
}

fn remove_session_history_entries(
    codex_dir: &Path,
    session_ids: &HashSet<String>,
) -> Result<usize> {
    let path = codex_dir.join("history.jsonl");
    if !path.exists() {
        return Ok(0);
    }
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| io_err(&path, e))?;
    file.try_lock().map_err(|error| {
        CodexxError::Config(format!(
            "历史记录正在被其他 Codex 进程使用，请关闭相关 Codex 窗口或 CLI 后重试: {error}"
        ))
    })?;
    let result = (|| -> Result<usize> {
        let mut text = String::new();
        file.read_to_string(&mut text)
            .map_err(|e| io_err(&path, e))?;
        let (next, removed) = filter_jsonl_session_entries(&text, &["session_id"], session_ids);
        if removed > 0 {
            file.set_len(0).map_err(|e| io_err(&path, e))?;
            file.seek(SeekFrom::Start(0))
                .map_err(|e| io_err(&path, e))?;
            file.write_all(next.as_bytes())
                .map_err(|e| io_err(&path, e))?;
            file.sync_all().map_err(|e| io_err(&path, e))?;
        }
        Ok(removed)
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
    codex_dir: &Path,
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
    for path in all_codex_sqlite_paths(codex_dir) {
        let result = (|| -> Result<(usize, usize)> {
            let mut conn = Connection::open_with_flags(
                &path,
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
struct LocalSessionDeleteCounts {
    deleted_ids: HashSet<String>,
    deleted_thread_rows: usize,
    deleted_rollout_files: usize,
    deleted_related_rows: usize,
    errors: Vec<String>,
}

fn delete_exact_session_ids_locally(
    codex_dir: &Path,
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
        purge_session_database_references(codex_dir, &session_ids);
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
fn hard_delete_sessions_locally(
    codex_dir: &Path,
    roots: &[String],
) -> Result<LocalSessionDeleteCounts> {
    let session_ids = session_ids_with_descendants(codex_dir, roots)?;
    let rollout_paths = selected_rollout_paths(codex_dir, &session_ids)?;
    Ok(delete_exact_session_ids_locally(
        codex_dir,
        session_ids,
        rollout_paths,
    ))
}

#[derive(Debug, Default)]
struct OfficialSessionDeleteOutcome {
    deleted_ids: HashSet<String>,
    completed_roots: HashSet<String>,
    failed_roots: Vec<(String, String)>,
}

#[derive(Debug)]
enum AppServerDeleteAttempt {
    Success(OfficialSessionDeleteOutcome),
    Unsupported(String),
}

fn send_app_server_message(
    stdin: &mut impl Write,
    value: &Value,
) -> std::result::Result<(), String> {
    let mut line = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    line.push(b'\n');
    stdin.write_all(&line).map_err(|error| error.to_string())?;
    stdin.flush().map_err(|error| error.to_string())
}

fn recv_app_server_message(
    receiver: &mpsc::Receiver<std::io::Result<String>>,
    deadline: Instant,
) -> std::result::Result<Value, String> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| "Codex App Server 响应超时".to_string())?;
        let line = receiver
            .recv_timeout(remaining)
            .map_err(|error| format!("Codex App Server 响应失败: {error}"))?
            .map_err(|error| format!("读取 Codex App Server 输出失败: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }
        return serde_json::from_str(&line)
            .map_err(|error| format!("解析 Codex App Server 输出失败: {error}"));
    }
}

fn app_server_error(value: &Value) -> Option<(i64, String)> {
    let error = value.get("error")?;
    Some((
        error
            .get("code")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Codex App Server 返回未知错误")
            .to_string(),
    ))
}

fn stop_app_server_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn run_app_server_delete_attempt(
    mut child: Child,
    codex_dir: &Path,
    roots: &[String],
) -> AppServerDeleteAttempt {
    let Some(mut stdin) = child.stdin.take() else {
        stop_app_server_child(&mut child);
        return AppServerDeleteAttempt::Unsupported("Codex App Server stdin 不可用".to_string());
    };
    let Some(stdout) = child.stdout.take() else {
        stop_app_server_child(&mut child);
        return AppServerDeleteAttempt::Unsupported("Codex App Server stdout 不可用".to_string());
    };
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let result = (|| -> AppServerDeleteAttempt {
        let initialize_id = 1i64;
        let initialize = json!({
            "id": initialize_id,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "codex_x",
                    "title": "Codex-X",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": null
            }
        });
        if let Err(error) = send_app_server_message(&mut stdin, &initialize) {
            return AppServerDeleteAttempt::Unsupported(error);
        }
        let initialize_deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let message = match recv_app_server_message(&receiver, initialize_deadline) {
                Ok(message) => message,
                Err(error) => return AppServerDeleteAttempt::Unsupported(error),
            };
            if message.get("id").and_then(Value::as_i64) != Some(initialize_id) {
                continue;
            }
            if let Some((_, message)) = app_server_error(&message) {
                return AppServerDeleteAttempt::Unsupported(message);
            }
            let Some(server_home) = message
                .get("result")
                .and_then(|value| value.get("codexHome"))
                .and_then(Value::as_str)
            else {
                return AppServerDeleteAttempt::Unsupported(
                    "Codex App Server 未返回 CODEX_HOME".to_string(),
                );
            };
            let requested_home = codex_dir
                .canonicalize()
                .unwrap_or_else(|_| codex_dir.to_path_buf());
            let returned_home = PathBuf::from(server_home)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(server_home));
            if requested_home != returned_home {
                return AppServerDeleteAttempt::Unsupported(format!(
                    "Codex App Server 使用了不同的 CODEX_HOME: {}",
                    returned_home.display()
                ));
            }
            break;
        }
        if let Err(error) = send_app_server_message(&mut stdin, &json!({"method": "initialized"})) {
            return AppServerDeleteAttempt::Unsupported(error);
        }

        let mut outcome = OfficialSessionDeleteOutcome::default();
        'delete_roots: for (index, root) in roots.iter().enumerate() {
            let request_id = 1000 + index as i64;
            if let Err(error) = send_app_server_message(
                &mut stdin,
                &json!({
                    "id": request_id,
                    "method": "thread/delete",
                    "params": { "threadId": root }
                }),
            ) {
                for pending in &roots[index..] {
                    outcome.failed_roots.push((pending.clone(), error.clone()));
                }
                break 'delete_roots;
            }
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                let message = match recv_app_server_message(&receiver, deadline) {
                    Ok(message) => message,
                    Err(error) => {
                        for pending in &roots[index..] {
                            outcome.failed_roots.push((pending.clone(), error.clone()));
                        }
                        break 'delete_roots;
                    }
                };
                if message.get("method").and_then(Value::as_str) == Some("thread/deleted") {
                    if let Some(id) = message
                        .get("params")
                        .and_then(|value| value.get("threadId"))
                        .and_then(Value::as_str)
                    {
                        outcome.deleted_ids.insert(id.to_string());
                    }
                }
                if message.get("id").and_then(Value::as_i64) == Some(request_id) {
                    if let Some((code, error)) = app_server_error(&message) {
                        let lower = error.to_ascii_lowercase();
                        if code == -32601 {
                            return AppServerDeleteAttempt::Unsupported(error);
                        }
                        if code == -32600
                            && (lower.contains("no rollout")
                                || lower.contains("not found")
                                || lower.contains("does not exist"))
                        {
                            outcome.completed_roots.insert(root.clone());
                            break;
                        }
                        outcome.failed_roots.push((root.clone(), error));
                        break;
                    }
                    outcome.completed_roots.insert(root.clone());
                    break;
                }
            }
        }
        AppServerDeleteAttempt::Success(outcome)
    })();

    drop(stdin);
    stop_app_server_child(&mut child);
    let _ = reader.join();
    result
}

fn delete_sessions_via_codex_app_server(
    codex_dir: &Path,
    roots: &[String],
) -> Result<Option<OfficialSessionDeleteOutcome>> {
    let mut unsupported_messages = Vec::new();
    for program in platform::codex_executable_candidates() {
        let is_bare_command = program.components().count() == 1;
        if !is_bare_command && !program.is_file() {
            continue;
        }
        let mut command = platform::program_command(&program, &["app-server", "--stdio"]);
        command
            .env("CODEX_HOME", codex_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                unsupported_messages.push(format!("{}: {error}", program.display()));
                continue;
            }
        };
        match run_app_server_delete_attempt(child, codex_dir, roots) {
            AppServerDeleteAttempt::Success(outcome) => return Ok(Some(outcome)),
            AppServerDeleteAttempt::Unsupported(message) => {
                unsupported_messages.push(format!("{}: {message}", program.display()));
            }
        }
    }
    let _ = unsupported_messages;
    Ok(None)
}

fn merge_delete_counts(target: &mut LocalSessionDeleteCounts, source: LocalSessionDeleteCounts) {
    target.deleted_ids.extend(source.deleted_ids);
    target.deleted_thread_rows += source.deleted_thread_rows;
    target.deleted_rollout_files += source.deleted_rollout_files;
    target.deleted_related_rows += source.deleted_related_rows;
    target.errors.extend(source.errors);
}

fn delete_codex_sessions_inner(input: SessionDeleteInput) -> Result<SessionDeleteResult> {
    let selected = normalized_session_ids(input.session_ids)?;
    let requested_sessions = selected.len();
    let codex_dir = resolve_codex_dir(input.config_dir)?;
    let _maintenance_lock = acquire_session_maintenance_lock(&codex_dir)?;
    let roots = selected_session_roots(&codex_dir, &selected)?;
    let expected_by_root = session_descendants_by_root(&codex_dir, &roots)?;
    let expected_ids = expected_by_root
        .values()
        .flatten()
        .cloned()
        .collect::<HashSet<_>>();
    let active_database_paths = sqlite_candidate_paths(&codex_dir);
    if active_database_paths.is_empty() {
        return Err(CodexxError::Database(
            "无法确认当前活动会话库，已取消永久删除".to_string(),
        ));
    }
    let verification_ids = expected_ids.clone();
    let status_before = session_sync_status_inner(Some(codex_dir.display().to_string()), None)?;
    let storage_before =
        active_session_storage_snapshot(&active_database_paths, &status_before.target_provider)?;
    // Validate and collect every filesystem target before the official API can
    // make the deletion irreversible.
    let expected_rollout_paths = selected_rollout_paths(&codex_dir, &expected_ids)?;
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
                        cleanup_ids,
                        cleanup_rollout_paths,
                    ),
                );
            }
        }
        None => {
            merge_delete_counts(
                &mut counts,
                delete_exact_session_ids_locally(&codex_dir, expected_ids, expected_rollout_paths),
            );
        }
    }

    let remaining_ids = match active_session_ids_present(&active_database_paths, &verification_ids)
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
    let status = match session_sync_status_inner(Some(codex_dir.display().to_string()), None) {
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
            fallback.needs_sync =
                fallback.mismatched_session_meta > 0 || fallback.mismatched_threads > 0;
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

fn sync_selected_sessions_provider_inner(
    input: SelectedSessionSyncInput,
) -> Result<SessionSyncResult> {
    let selected_ids = input
        .session_ids
        .into_iter()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect::<HashSet<_>>();
    if selected_ids.is_empty() {
        return Err(CodexxError::Config("请选择至少一个会话".to_string()));
    }

    let codex_dir = resolve_codex_dir(input.config_dir)?;
    fs::create_dir_all(&codex_dir).map_err(|e| io_err(&codex_dir, e))?;
    let target = current_model_provider(&codex_dir, input.target_provider)?;
    let _maintenance_lock = acquire_session_maintenance_lock(&codex_dir)?;
    let rollouts = scan_rollouts(&codex_dir, &target, true, Some(&selected_ids))?;
    let changed_paths = rollouts
        .changed_files
        .iter()
        .map(|(p, _)| p.clone())
        .collect::<Vec<_>>();
    let backup_dir = create_provider_sync_backup(&codex_dir, &target, &changed_paths)?;

    let mut updated_rollouts = 0usize;
    for (path, text) in &rollouts.changed_files {
        write_text(path, text)?;
        updated_rollouts += 1;
    }
    let updated_threads = apply_sqlite_provider_sync_selected(
        &codex_dir,
        &target,
        &selected_ids,
        &rollouts.thread_ids_with_user_events,
        &rollouts.cwd_by_thread_id,
    )?;
    let mut status =
        session_sync_status_inner(Some(codex_dir.display().to_string()), Some(target.clone()))?;
    status.backup_dir = Some(backup_dir.display().to_string());
    Ok(SessionSyncResult {
        status,
        updated_rollouts,
        updated_threads,
        backup_dir: backup_dir.display().to_string(),
    })
}

fn sync_sessions_provider_inner(
    config_dir: Option<String>,
    target_provider: Option<String>,
) -> Result<SessionSyncResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    fs::create_dir_all(&codex_dir).map_err(|e| io_err(&codex_dir, e))?;
    let target = current_model_provider(&codex_dir, target_provider)?;
    let _maintenance_lock = acquire_session_maintenance_lock(&codex_dir)?;
    let rollouts = scan_rollouts(&codex_dir, &target, true, None)?;
    let sqlite_before = scan_sqlite(&codex_dir, &target)?;
    let changed_paths = rollouts
        .changed_files
        .iter()
        .map(|(p, _)| p.clone())
        .collect::<Vec<_>>();
    let backup_dir = create_provider_sync_backup(&codex_dir, &target, &changed_paths)?;

    let mut updated_rollouts = 0usize;
    for (path, text) in &rollouts.changed_files {
        write_text(path, text)?;
        updated_rollouts += 1;
    }
    let updated_threads = apply_sqlite_provider_sync(
        &codex_dir,
        &target,
        &rollouts.thread_ids_with_user_events,
        &rollouts.cwd_by_thread_id,
    )?;
    let mut status =
        session_sync_status_inner(Some(codex_dir.display().to_string()), Some(target.clone()))?;
    status.backup_dir = Some(backup_dir.display().to_string());
    if rollouts.changed_files.is_empty() && sqlite_before.mismatched_threads == 0 {
        status
            .warnings
            .push("没有发现需要修复的会话；已保留一次安全备份。".to_string());
    }
    Ok(SessionSyncResult {
        status,
        updated_rollouts,
        updated_threads,
        backup_dir: backup_dir.display().to_string(),
    })
}

fn set_top_level_defaults(doc: &mut DocumentMut) {
    if doc.get("model_reasoning_effort").is_none() {
        doc["model_reasoning_effort"] = value("high");
    }
    if doc.get("disable_response_storage").is_none() {
        doc["disable_response_storage"] = value(true);
    }
}

fn ensure_table<'a>(parent: &'a mut Table, key: &str) -> Result<&'a mut Table> {
    if !parent.contains_key(key) {
        parent[key] = Item::Table(Table::new());
    }
    parent
        .get_mut(key)
        .and_then(|item| item.as_table_mut())
        .ok_or_else(|| CodexxError::Config(format!("{key} 不是 TOML table")))
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

#[tauri::command]
async fn get_skills_mcp_state(config_dir: Option<String>) -> Result<SkillsMcpState> {
    tauri::async_runtime::spawn_blocking(move || build_skills_mcp_state_inner(config_dir))
        .await
        .map_err(|e| CodexxError::Config(format!("读取 Skills/MCP 失败: {e}")))?
}

#[tauri::command]
async fn import_existing_skills_mcp(config_dir: Option<String>) -> Result<SkillsMcpActionResult> {
    tauri::async_runtime::spawn_blocking(move || import_existing_skills_mcp_inner(config_dir))
        .await
        .map_err(|e| CodexxError::Config(format!("导入已有 Skills/MCP 失败: {e}")))?
}

#[tauri::command]
async fn preview_existing_skills_mcp(config_dir: Option<String>) -> Result<SkillsMcpImportPreview> {
    tauri::async_runtime::spawn_blocking(move || preview_existing_skills_mcp_inner(config_dir))
        .await
        .map_err(|e| CodexxError::Config(format!("预览已有 Skills/MCP 失败: {e}")))?
}

#[tauri::command]
async fn toggle_codex_skill(
    config_dir: Option<String>,
    id: String,
    enabled: bool,
) -> Result<SkillsMcpState> {
    tauri::async_runtime::spawn_blocking(move || toggle_codex_skill_inner(config_dir, id, enabled))
        .await
        .map_err(|e| CodexxError::Config(format!("切换 Skill 失败: {e}")))?
}

#[tauri::command]
async fn toggle_codex_mcp(
    config_dir: Option<String>,
    id: String,
    enabled: bool,
) -> Result<SkillsMcpState> {
    tauri::async_runtime::spawn_blocking(move || toggle_codex_mcp_inner(config_dir, id, enabled))
        .await
        .map_err(|e| CodexxError::Config(format!("切换 MCP 失败: {e}")))?
}

#[tauri::command]
async fn install_skill_zip(
    config_dir: Option<String>,
    file_name: String,
    bytes: Vec<u8>,
) -> Result<SkillsMcpActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        install_skill_zip_inner(config_dir, file_name, bytes)
    })
    .await
    .map_err(|e| CodexxError::Config(format!("ZIP 安装 Skill 失败: {e}")))?
}

#[tauri::command]
async fn check_skill_updates(config_dir: Option<String>) -> Result<SkillsMcpState> {
    tauri::async_runtime::spawn_blocking(move || check_skill_updates_inner(config_dir))
        .await
        .map_err(|e| CodexxError::Config(format!("检查 Skill 更新失败: {e}")))?
}

#[tauri::command]
async fn get_startup_diagnostics(config_dir: Option<String>) -> Result<StartupDiagnostics> {
    tauri::async_runtime::spawn_blocking(move || startup_diagnostics_inner(config_dir))
        .await
        .map_err(|e| CodexxError::Config(format!("启动检测失败: {e}")))?
}

#[tauri::command]
async fn get_session_sync_status(
    config_dir: Option<String>,
    target_provider: Option<String>,
) -> Result<SessionSyncStatus> {
    tauri::async_runtime::spawn_blocking(move || {
        session_sync_status_inner(config_dir, target_provider)
    })
    .await
    .map_err(|e| CodexxError::Config(format!("读取会话状态失败: {e}")))?
}

#[tauri::command]
async fn sync_sessions_provider(
    config_dir: Option<String>,
    target_provider: Option<String>,
) -> Result<SessionSyncResult> {
    tauri::async_runtime::spawn_blocking(move || {
        sync_sessions_provider_inner(config_dir, target_provider)
    })
    .await
    .map_err(|e| CodexxError::Config(format!("同步会话失败: {e}")))?
}

#[tauri::command]
async fn sync_selected_sessions_provider(
    input: SelectedSessionSyncInput,
) -> Result<SessionSyncResult> {
    tauri::async_runtime::spawn_blocking(move || sync_selected_sessions_provider_inner(input))
        .await
        .map_err(|e| CodexxError::Config(format!("同步选中会话失败: {e}")))?
}

#[tauri::command]
async fn delete_codex_sessions(input: SessionDeleteInput) -> Result<SessionDeleteResult> {
    tauri::async_runtime::spawn_blocking(move || delete_codex_sessions_inner(input))
        .await
        .map_err(|e| CodexxError::Config(format!("永久删除会话失败: {e}")))?
}

#[tauri::command]
async fn read_ccswitch_official_auth(
    db_path: Option<String>,
) -> Result<Option<OfficialAuthCandidate>> {
    tauri::async_runtime::spawn_blocking(move || read_ccswitch_official_auth_inner(db_path))
        .await
        .map_err(|e| CodexxError::Config(format!("读取 cc-switch 官方 Auth 失败: {e}")))?
}

#[tauri::command]
async fn import_ccswitch_codex_providers(db_path: Option<String>) -> Result<ImportResult> {
    tauri::async_runtime::spawn_blocking(move || import_ccswitch_codex_providers_inner(db_path))
        .await
        .map_err(|e| CodexxError::Config(format!("导入 cc-switch Provider 失败: {e}")))?
}

fn get_about_info_inner(config_dir: Option<String>) -> Result<AboutInfo> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    Ok(AboutInfo {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        codex_version: platform::detect_codex_version(),
        codex_dir: codex_dir.display().to_string(),
        project_url: "https://github.com/yynxxxxx/Codex-X".to_string(),
        github_repo: "yynxxxxx/Codex-X".to_string(),
    })
}

#[tauri::command]
async fn get_about_info(config_dir: Option<String>) -> Result<AboutInfo> {
    tauri::async_runtime::spawn_blocking(move || get_about_info_inner(config_dir))
        .await
        .map_err(|e| CodexxError::Config(format!("读取关于信息失败: {e}")))?
}

#[tauri::command]
async fn list_saved_prompts() -> Result<Vec<SavedPrompt>> {
    tauri::async_runtime::spawn_blocking(list_saved_prompts_inner)
        .await
        .map_err(|e| CodexxError::Config(format!("读取提示词列表失败: {e}")))?
}

#[tauri::command]
async fn get_builtin_prompt_status() -> Result<Vec<BuiltinPromptStatus>> {
    tauri::async_runtime::spawn_blocking(builtin_prompt_status_inner)
        .await
        .map_err(|e| CodexxError::Config(format!("读取内置提示词状态失败: {e}")))?
}

#[tauri::command]
async fn refresh_builtin_prompts(config_dir: Option<String>) -> Result<Vec<BuiltinPromptStatus>> {
    tauri::async_runtime::spawn_blocking(move || refresh_builtin_prompts_inner(config_dir))
        .await
        .map_err(|e| CodexxError::Config(format!("提示词后台更新失败: {e}")))?
}

#[tauri::command]
async fn remember_current_instruction(config_dir: Option<String>) -> Result<Option<SavedPrompt>> {
    tauri::async_runtime::spawn_blocking(move || {
        let codex_dir = resolve_codex_dir(config_dir)?;
        remember_current_instruction_prompt(&codex_dir)
    })
    .await
    .map_err(|e| CodexxError::Config(format!("保存当前外部提示词失败: {e}")))?
}

fn save_prompt_command_inner(prompt: SavedPrompt) -> Result<SavedPrompt> {
    let title = prompt.title.trim().to_string();
    if title.is_empty() {
        return Err(CodexxError::Config("提示词名称不能为空".to_string()));
    }
    let content = prompt.content.trim().to_string();
    if content.is_empty() {
        return Err(CodexxError::Config("提示词内容不能为空".to_string()));
    }
    let id = if prompt.id.trim().is_empty() {
        sanitize_id(&title)
    } else {
        sanitize_id(&prompt.id)
    };
    let filename = normalize_prompt_filename(&prompt.filename, &id);
    save_prompt_inner(SavedPrompt {
        id,
        title,
        filename,
        content,
    })
}

#[tauri::command]
async fn save_prompt(prompt: SavedPrompt) -> Result<SavedPrompt> {
    tauri::async_runtime::spawn_blocking(move || save_prompt_command_inner(prompt))
        .await
        .map_err(|e| CodexxError::Config(format!("保存提示词失败: {e}")))?
}

#[tauri::command]
async fn delete_saved_prompt(id: String) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || delete_prompt_inner(id.trim()))
        .await
        .map_err(|e| CodexxError::Config(format!("删除提示词失败: {e}")))?
}

fn managed_model_instruction_path(codex_dir: &Path, doc: &DocumentMut) -> Result<Option<PathBuf>> {
    let Some(current) = string_value(doc, "model_instructions_file") else {
        return Ok(None);
    };
    if prompt_template_key_for_instruction(&current)?.is_none() {
        return Ok(None);
    }
    Ok(Some(resolve_instruction_path(codex_dir, &current)))
}

fn enable_prompt_content_inner(
    config_dir: Option<String>,
    filename: &str,
    content: &str,
    template_key: &str,
    title: &str,
    content_source: &str,
    injection_mode: PromptInjectionMode,
    action: &str,
) -> Result<ActionResult> {
    if filename.trim().is_empty()
        || !filename.to_ascii_lowercase().ends_with(".md")
        || filename.contains('/')
        || filename.contains('\\')
    {
        return Err(CodexxError::Config("提示词文件名无效".to_string()));
    }
    if template_key.trim().is_empty() || template_key.contains("-->") {
        return Err(CodexxError::Config("提示词模板标识无效".to_string()));
    }

    let codex_dir = resolve_codex_dir(config_dir)?;
    fs::create_dir_all(&codex_dir).map_err(|e| io_err(&codex_dir, e))?;
    let cfg = config_path(&codex_dir);
    let agents = agents_path(&codex_dir);
    let text = read_to_string_if_exists(&cfg)?;
    let mut doc = parse_toml_document(&cfg, &text)?;
    let agents_text = read_to_string_if_exists(&agents)?;
    managed_agents_bounds(&agents_text)?;
    let previous_managed_file = managed_model_instruction_path(&codex_dir, &doc)?;
    if injection_mode == PromptInjectionMode::Replace {
        let _ = remember_current_instruction_prompt(&codex_dir);
    }
    let backup_id = create_backup(&codex_dir, action)?;

    match injection_mode {
        PromptInjectionMode::Replace => {
            if doc.get("model").is_none() {
                doc["model"] = value("gpt-5.5");
            }
            doc["model_instructions_file"] = value(format!("./{filename}"));
            write_text(&codex_dir.join(filename), content)?;
            write_text(&cfg, &doc.to_string())?;
            uninstall_managed_agents_block(&codex_dir)?;
        }
        PromptInjectionMode::Append => {
            install_managed_agents_block(&codex_dir, template_key, content)?;
            if previous_managed_file.is_some() {
                doc.as_table_mut().remove("model_instructions_file");
                write_text(&cfg, &doc.to_string())?;
            }
        }
    }

    if let Some(previous) = previous_managed_file {
        let next = codex_dir.join(filename);
        let should_remove = injection_mode == PromptInjectionMode::Append || previous != next;
        if should_remove && previous.parent() == Some(codex_dir.as_path()) && previous.exists() {
            fs::remove_file(&previous).map_err(|e| io_err(&previous, e))?;
        }
    }

    let state = build_state(codex_dir)?;
    Ok(ActionResult {
        ok: true,
        message: format!(
            "已用{}模式启用 {title}（来源：{content_source}）",
            if injection_mode == PromptInjectionMode::Append {
                "追加"
            } else {
                "替换"
            }
        ),
        backup_id,
        state,
    })
}

fn enable_saved_prompt_inner(
    config_dir: Option<String>,
    id: String,
    injection_mode: Option<String>,
) -> Result<ActionResult> {
    let prompt = get_saved_prompt_inner(id.trim())?;
    let mode = PromptInjectionMode::parse(injection_mode.as_deref())?;
    enable_prompt_content_inner(
        config_dir,
        &prompt.filename,
        &prompt.content,
        &format!("saved:{}", prompt.id),
        &prompt.title,
        "本地自定义",
        mode,
        "enable-custom-prompt",
    )
}

#[tauri::command]
async fn enable_saved_prompt(
    config_dir: Option<String>,
    id: String,
    injection_mode: Option<String>,
) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        enable_saved_prompt_inner(config_dir, id, injection_mode)
    })
    .await
    .map_err(|e| CodexxError::Config(format!("启用自定义提示词失败: {e}")))?
}

#[tauri::command]
async fn list_saved_providers() -> Result<Vec<SavedProvider>> {
    tauri::async_runtime::spawn_blocking(list_saved_providers_inner)
        .await
        .map_err(|e| CodexxError::Config(format!("读取供应商列表失败: {e}")))?
}

fn save_provider_command_inner(provider: SavedProvider) -> Result<SavedProvider> {
    save_provider_inner(provider)
}

#[tauri::command]
async fn save_provider(provider: SavedProvider) -> Result<SavedProvider> {
    tauri::async_runtime::spawn_blocking(move || save_provider_command_inner(provider))
        .await
        .map_err(|e| CodexxError::Config(format!("保存供应商失败: {e}")))?
}

#[tauri::command]
async fn delete_saved_provider(id: String) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || delete_provider_inner(id.trim()))
        .await
        .map_err(|e| CodexxError::Config(format!("删除供应商失败: {e}")))?
}

#[tauri::command]
async fn get_codex_state(config_dir: Option<String>) -> Result<CodexState> {
    tauri::async_runtime::spawn_blocking(move || {
        let codex_dir = resolve_codex_dir(config_dir)?;
        build_state(codex_dir)
    })
    .await
    .map_err(|e| CodexxError::Config(format!("读取 Codex 状态失败: {e}")))?
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

fn switch_official_provider_with_pre_persist<F>(
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

fn switch_official_provider_inner(config_dir: Option<String>) -> Result<ActionResult> {
    switch_official_provider_with_pre_persist(config_dir, persist_detected_live_custom_provider)
}

#[tauri::command]
async fn switch_official_provider(config_dir: Option<String>) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || switch_official_provider_inner(config_dir))
        .await
        .map_err(|e| CodexxError::Config(format!("切换官方配置失败: {e}")))?
}

#[tauri::command]
async fn save_official_config(input: OfficialConfigInput) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        save_official_config_inner(input.config_dir, input.model, input.auth_json)
    })
    .await
    .map_err(|e| CodexxError::Config(format!("保存官方配置失败: {e}")))?
}

fn save_official_config_inner(
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

fn enable_instruction_inner(
    config_dir: Option<String>,
    template_id: &str,
    injection_mode: Option<String>,
) -> Result<ActionResult> {
    let resolved_id = if template_id.trim().is_empty() {
        "gpt5.5-unrestricted"
    } else {
        template_id.trim()
    };
    let (filename, _relative, content, content_source) = builtin_prompt_content(resolved_id)?;
    let mode = PromptInjectionMode::parse(injection_mode.as_deref())?;
    enable_prompt_content_inner(
        config_dir,
        &filename,
        &content,
        &format!("builtin:{resolved_id}"),
        &filename,
        &content_source,
        mode,
        "enable-instruct",
    )
}

#[tauri::command]
async fn enable_instruction(
    config_dir: Option<String>,
    injection_mode: Option<String>,
) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        enable_instruction_inner(config_dir, "gpt5.5-unrestricted", injection_mode)
    })
    .await
    .map_err(|e| CodexxError::Config(format!("启用指令提示词失败: {e}")))?
}

#[tauri::command]
async fn enable_instruction_template(
    config_dir: Option<String>,
    template_id: String,
    injection_mode: Option<String>,
) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        enable_instruction_inner(config_dir, &template_id, injection_mode)
    })
    .await
    .map_err(|e| CodexxError::Config(format!("启用指令提示词失败: {e}")))?
}

fn disable_instruction_inner(
    config_dir: Option<String>,
    delete_file: Option<bool>,
) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    let cfg = config_path(&codex_dir);
    let agents_text = read_to_string_if_exists(&agents_path(&codex_dir))?;
    managed_agents_bounds(&agents_text)?;
    let backup_id = create_backup(&codex_dir, "disable-instruct")?;

    let text = read_to_string_if_exists(&cfg)?;
    let mut doc = parse_toml_document(&cfg, &text)?;
    let current = string_value(&doc, "model_instructions_file");
    let managed_model_path = managed_model_instruction_path(&codex_dir, &doc)?;
    let removed_model = managed_model_path.is_some();
    if removed_model {
        doc.as_table_mut().remove("model_instructions_file");
        write_text(&cfg, &doc.to_string())?;
    }
    let removed_agents = uninstall_managed_agents_block(&codex_dir)?;
    if delete_file.unwrap_or(true) {
        if let Some(md) = managed_model_path {
            if md.parent() == Some(codex_dir.as_path()) && md.exists() {
                fs::remove_file(&md).map_err(|e| io_err(&md, e))?;
            }
        }
    }

    let state = build_state(codex_dir)?;
    let removed = removed_model || removed_agents;
    Ok(ActionResult {
        ok: true,
        message: if removed {
            "已禁用指令提示词".to_string()
        } else if current.is_some() {
            "当前使用的是用户自己的提示词，Codex-X 未做修改".to_string()
        } else {
            "当前没有启用 Codex-X 提示词".to_string()
        },
        backup_id,
        state,
    })
}

#[tauri::command]
async fn disable_instruction(
    config_dir: Option<String>,
    delete_file: Option<bool>,
) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || disable_instruction_inner(config_dir, delete_file))
        .await
        .map_err(|e| CodexxError::Config(format!("禁用指令提示词失败: {e}")))?
}

fn disable_external_instruction_inner(config_dir: Option<String>) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    let cfg = config_path(&codex_dir);
    let text = read_to_string_if_exists(&cfg)?;
    let mut doc = parse_toml_document(&cfg, &text)?;
    let current = string_value(&doc, "model_instructions_file");
    if let Some(value) = current.as_deref() {
        if prompt_template_key_for_instruction(value)?.is_some() {
            return Err(CodexxError::Config(
                "当前是 Codex-X 管理的提示词，请使用普通禁用按钮".to_string(),
            ));
        }
    }
    let backup_id = create_backup(&codex_dir, "disable-external-instruct")?;
    if current.is_some() {
        doc.as_table_mut().remove("model_instructions_file");
        write_text(&cfg, &doc.to_string())?;
    }
    let state = build_state(codex_dir)?;
    Ok(ActionResult {
        ok: true,
        message: if current.is_some() {
            "已禁用用户外部提示词，原 md 文件已保留".to_string()
        } else {
            "当前没有外部提示词".to_string()
        },
        backup_id,
        state,
    })
}

#[tauri::command]
async fn disable_external_instruction(config_dir: Option<String>) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || disable_external_instruction_inner(config_dir))
        .await
        .map_err(|e| CodexxError::Config(format!("禁用外部提示词失败: {e}")))?
}

fn save_provider_toml_config_with_pre_persist<F>(
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

fn save_provider_toml_config_inner(input: ProviderTomlInput) -> Result<ActionResult> {
    save_provider_toml_config_with_pre_persist(input, persist_detected_live_custom_provider)
}

#[tauri::command]
async fn save_provider_toml_config(input: ProviderTomlInput) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || save_provider_toml_config_inner(input))
        .await
        .map_err(|e| CodexxError::Config(format!("保存供应商 TOML 失败: {e}")))?
}

fn provider_test_request(
    agent: &ureq::Agent,
    url: &str,
    api_key: Option<&str>,
) -> std::result::Result<ureq::Response, ureq::Error> {
    let request = agent.get(url);
    if let Some(api_key) = api_key.filter(|s| !s.trim().is_empty()) {
        request.set("Authorization", &format!("Bearer {}", api_key.trim()))
    } else {
        request
    }
    .call()
}

fn provider_status_result(status: u16, duration_ms: u128) -> ProviderConnectionResult {
    ProviderConnectionResult {
        ok: (200..300).contains(&status),
        status: Some(status),
        message: if (200..300).contains(&status) {
            format!("{duration_ms} ms")
        } else if status == 401 || status == 403 {
            format!("HTTP {status} · {duration_ms} ms（认证失败或无权限）")
        } else {
            format!("HTTP {status} · {duration_ms} ms")
        },
        duration_ms,
    }
}

fn test_provider_connection_inner(
    base_url: String,
    api_key: Option<String>,
) -> Result<ProviderConnectionResult> {
    let base = base_url.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return Err(CodexxError::Config("base_url 不能为空".to_string()));
    }
    if !base.starts_with("http://") && !base.starts_with("https://") {
        return Err(CodexxError::Config(
            "base_url 必须以 http:// 或 https:// 开头".to_string(),
        ));
    }

    let api_key = api_key.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(6))
        .build();
    let models_url = format!("{base}/models");
    let started = Instant::now();

    match provider_test_request(&agent, &models_url, api_key) {
        Ok(response) => {
            return Ok(provider_status_result(
                response.status(),
                started.elapsed().as_millis(),
            ))
        }
        Err(ureq::Error::Status(status, _)) => {
            // /models exists but rejected the request. This is not a successful
            // provider test; notably HTTP 403 must not be shown as “连接正常”.
            return Ok(provider_status_result(
                status,
                started.elapsed().as_millis(),
            ));
        }
        Err(_models_error) => {
            // Network-level failure on /models: try the base endpoint once so
            // users can distinguish DNS/TLS failures from a provider with no
            // models route.
            match provider_test_request(&agent, &base, api_key) {
                Ok(response) => Ok(provider_status_result(
                    response.status(),
                    started.elapsed().as_millis(),
                )),
                Err(ureq::Error::Status(status, _)) => Ok(provider_status_result(
                    status,
                    started.elapsed().as_millis(),
                )),
                Err(_base_error) => Ok(ProviderConnectionResult {
                    ok: false,
                    status: None,
                    message: format!("请求失败 · {} ms", started.elapsed().as_millis()),
                    duration_ms: started.elapsed().as_millis(),
                }),
            }
        }
    }
}

#[tauri::command]
async fn test_provider_connection(
    base_url: String,
    api_key: Option<String>,
) -> Result<ProviderConnectionResult> {
    tauri::async_runtime::spawn_blocking(move || test_provider_connection_inner(base_url, api_key))
        .await
        .map_err(|e| CodexxError::Config(format!("测试连接失败: {e}")))?
}

fn switch_provider_with_pre_persist<F>(input: ProviderInput, pre_persist: F) -> Result<ActionResult>
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

fn switch_provider_inner(input: ProviderInput) -> Result<ActionResult> {
    switch_provider_with_pre_persist(input, persist_detected_live_custom_provider)
}

#[tauri::command]
async fn switch_provider(input: ProviderInput) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || switch_provider_inner(input))
        .await
        .map_err(|e| CodexxError::Config(format!("切换供应商失败: {e}")))?
}

#[tauri::command]
async fn list_backups() -> Result<Vec<BackupEntry>> {
    tauri::async_runtime::spawn_blocking(backups)
        .await
        .map_err(|e| CodexxError::Config(format!("读取备份列表失败: {e}")))?
}

fn restore_backup_inner(config_dir: Option<String>, backup_id: String) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    let dir = action_backup_root(&codex_dir)?.join(&backup_id);
    if !dir.exists() {
        return Err(CodexxError::Config(format!("备份不存在: {backup_id}")));
    }

    let restore_marker = create_backup(&codex_dir, "before-restore")?;
    let cfg = config_path(&codex_dir);
    let auth = auth_path(&codex_dir);
    let agents = agents_path(&codex_dir);
    fs::create_dir_all(&codex_dir).map_err(|e| io_err(&codex_dir, e))?;

    let backup_meta = fs::read_to_string(dir.join("meta.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<BackupMeta>(&text).ok());

    let backup_cfg = dir.join("config.toml");
    if backup_cfg.exists() {
        let bytes = fs::read(&backup_cfg).map_err(|e| io_err(&backup_cfg, e))?;
        atomic_write(&cfg, &bytes)?;
    } else if cfg.exists() {
        fs::remove_file(&cfg).map_err(|e| io_err(&cfg, e))?;
    }

    let backup_auth = dir.join("auth.json");
    if backup_auth.exists() {
        let bytes = fs::read(&backup_auth).map_err(|e| io_err(&backup_auth, e))?;
        atomic_write(&auth, &bytes)?;
    } else if auth.exists() {
        fs::remove_file(&auth).map_err(|e| io_err(&auth, e))?;
    }

    if backup_meta.as_ref().is_some_and(|meta| meta.tracks_agents) {
        let backup_agents = dir.join(AGENTS_FILENAME);
        if backup_agents.exists() {
            let bytes = fs::read(&backup_agents).map_err(|e| io_err(&backup_agents, e))?;
            atomic_write(&agents, &bytes)?;
        } else if agents.exists() {
            fs::remove_file(&agents).map_err(|e| io_err(&agents, e))?;
        }
    }

    let state = build_state(codex_dir)?;
    Ok(ActionResult {
        ok: true,
        message: format!("已恢复备份 {backup_id}"),
        backup_id: restore_marker,
        state,
    })
}

#[tauri::command]
async fn restore_backup(config_dir: Option<String>, backup_id: String) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || restore_backup_inner(config_dir, backup_id))
        .await
        .map_err(|e| CodexxError::Config(format!("恢复备份失败: {e}")))?
}

#[tauri::command]
fn open_url(url: String) -> std::result::Result<(), String> {
    let trimmed = url.trim().to_string();
    if trimmed.is_empty() {
        return Err("URL 为空".to_string());
    }

    // Do not wait for the browser process. On Windows, waiting for `cmd /C start` can
    // visibly freeze the WebView for a few seconds before the default browser appears.
    std::thread::spawn(move || {
        #[cfg(target_os = "macos")]
        {
            let _ = Command::new("open").arg(&trimmed).spawn();
        }

        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            let _ = Command::new("cmd")
                .creation_flags(CREATE_NO_WINDOW)
                .args(["/C", "start", ""])
                .arg(&trimmed)
                .spawn();
        }

        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        {
            let _ = Command::new("xdg-open").arg(&trimmed).spawn();
        }
    });

    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_about_info,
            get_skills_mcp_state,
            preview_existing_skills_mcp,
            import_existing_skills_mcp,
            toggle_codex_skill,
            toggle_codex_mcp,
            install_skill_zip,
            check_skill_updates,
            get_startup_diagnostics,
            get_session_sync_status,
            sync_sessions_provider,
            sync_selected_sessions_provider,
            delete_codex_sessions,
            read_ccswitch_official_auth,
            import_ccswitch_codex_providers,
            list_saved_prompts,
            get_builtin_prompt_status,
            refresh_builtin_prompts,
            remember_current_instruction,
            save_prompt,
            delete_saved_prompt,
            enable_saved_prompt,
            list_saved_providers,
            save_provider,
            delete_saved_provider,
            get_codex_state,
            switch_official_provider,
            save_official_config,
            enable_instruction,
            enable_instruction_template,
            disable_instruction,
            disable_external_instruction,
            switch_provider,
            save_provider_toml_config,
            test_provider_connection,
            list_backups,
            restore_backup,
            open_url,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Codex-X");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_codex_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codex-x-{name}-{}-{}",
            std::process::id(),
            Local::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&dir).expect("create temp codex dir");
        dir
    }

    fn provider_test_connection() -> Connection {
        let conn = Connection::open_in_memory().expect("open provider test database");
        conn.execute_batch(
            "CREATE TABLE providers (
                id TEXT PRIMARY KEY,
                provider_name TEXT NOT NULL,
                base_url TEXT NOT NULL,
                model TEXT NOT NULL,
                api_key TEXT,
                toml_config TEXT,
                wire_api TEXT NOT NULL DEFAULT 'responses',
                requires_openai_auth INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );",
        )
        .expect("create providers table");
        conn
    }

    fn provider_fixture(
        id: &str,
        name: &str,
        base_url: &str,
        api_key: Option<&str>,
        model: &str,
        toml_config: Option<&str>,
    ) -> SavedProvider {
        SavedProvider {
            id: id.to_string(),
            provider_name: name.to_string(),
            base_url: base_url.to_string(),
            model: model.to_string(),
            api_key: api_key.map(ToString::to_string),
            toml_config: toml_config.map(ToString::to_string),
            wire_api: "responses".to_string(),
            requires_openai_auth: true,
        }
    }

    fn seed_provider(
        conn: &Connection,
        provider: &SavedProvider,
        created_at: &str,
        updated_at: &str,
    ) {
        conn.execute(
            "INSERT INTO providers
                (id, provider_name, base_url, model, api_key, toml_config, wire_api,
                 requires_openai_auth, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                provider.id,
                provider.provider_name,
                provider.base_url,
                provider.model,
                provider.api_key,
                provider.toml_config,
                provider.wire_api,
                if provider.requires_openai_auth { 1 } else { 0 },
                created_at,
                updated_at,
            ],
        )
        .expect("seed provider");
    }

    #[test]
    fn provider_base_url_canonicalization_preserves_path_case() {
        assert_eq!(
            canonical_provider_base_url("  HTTP://Example.COM:80/V1///  "),
            "http://example.com/V1"
        );
        assert_eq!(
            canonical_provider_base_url("https://EXAMPLE.com:443/v1/#ignored"),
            "https://example.com/v1"
        );
        assert_eq!(
            canonical_provider_base_url("https://example.com:8443/V1/?Region=US#ignored"),
            "https://example.com:8443/V1?Region=US"
        );
    }

    #[test]
    fn delete_provider_requires_an_existing_sqlite_row() {
        let conn = provider_test_connection();
        let provider = provider_fixture(
            "provider-a",
            "Provider A",
            "https://a.example.com/v1",
            Some("sk-a"),
            "gpt-5.5",
            None,
        );
        seed_provider(
            &conn,
            &provider,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z",
        );

        delete_provider_on_connection(&conn, "provider-a").expect("delete existing provider");
        assert!(provider_by_id_on_connection(&conn, "provider-a")
            .expect("query deleted provider")
            .is_none());

        let error = delete_provider_on_connection(&conn, "provider-a")
            .expect_err("deleting a missing provider must fail");
        assert!(error.to_string().contains("供应商不存在或已删除"));
    }

    #[test]
    fn provider_identity_uses_url_and_effective_credential_not_model_or_name() {
        let direct = provider_fixture(
            "direct",
            "Magic AI",
            "https://EXAMPLE.com:443/v1/",
            Some("sk-same"),
            "gpt-5.6-sol",
            None,
        );
        let toml = provider_fixture(
            "toml",
            "Renamed Provider",
            "https://example.com/v1",
            None,
            "gpt-5.5",
            Some(
                r#"model_provider = "custom"
[model_providers.custom]
experimental_bearer_token = "sk-same"
"#,
            ),
        );
        let different_key = provider_fixture(
            "different",
            "Magic AI",
            "https://example.com/v1",
            Some("sk-other"),
            "gpt-5.5",
            None,
        );
        assert_eq!(provider_identity(&direct), provider_identity(&toml));
        assert_ne!(
            provider_identity(&direct),
            provider_identity(&different_key)
        );

        let anonymous_a = provider_fixture(
            "anonymous-a",
            "  Acme\u{2003}API  ",
            "https://example.com/v1/",
            None,
            "one",
            None,
        );
        let anonymous_b = provider_fixture(
            "anonymous-b",
            "acme api",
            "https://EXAMPLE.com/v1",
            None,
            "two",
            None,
        );
        assert_eq!(
            provider_identity(&anonymous_a),
            provider_identity(&anonymous_b)
        );
    }

    #[test]
    fn manual_provider_save_upserts_same_url_and_key_but_keeps_different_keys() {
        let conn = provider_test_connection();
        let first = normalize_saved_provider(provider_fixture(
            "first",
            "First Name",
            "https://example.com/v1/",
            Some("sk-same"),
            "model-a",
            None,
        ))
        .expect("normalize first");
        let added = upsert_provider_on_connection(&conn, first, ProviderUpsertMode::Manual)
            .expect("add first");
        assert_eq!(added.kind, ProviderUpsertKind::Added);

        let renamed = normalize_saved_provider(provider_fixture(
            "second",
            "Second Name",
            "HTTPS://EXAMPLE.COM:443/v1",
            Some("sk-same"),
            "model-b",
            None,
        ))
        .expect("normalize renamed");
        let merged = upsert_provider_on_connection(&conn, renamed, ProviderUpsertMode::Manual)
            .expect("merge same identity");
        assert_eq!(merged.kind, ProviderUpsertKind::Merged);
        assert_eq!(merged.provider.id, "first");
        assert_eq!(merged.provider.provider_name, "Second Name");
        assert_eq!(merged.provider.model, "model-b");

        let other_key = normalize_saved_provider(provider_fixture(
            "third",
            "Second Name",
            "https://example.com/v1",
            Some("sk-other"),
            "model-b",
            None,
        ))
        .expect("normalize other key");
        let second_add =
            upsert_provider_on_connection(&conn, other_key, ProviderUpsertMode::Manual)
                .expect("keep different credential");
        assert_eq!(second_add.kind, ProviderUpsertKind::Added);
        assert_eq!(list_saved_providers_on_connection(&conn).unwrap().len(), 2);
    }

    #[test]
    fn imported_provider_merge_preserves_existing_local_profile_and_toml() {
        let conn = provider_test_connection();
        let local_toml = r#"model_provider = "custom"
model = "local-model"
[model_providers.custom]
base_url = "https://example.com/v1"
experimental_bearer_token = "sk-same"
"#;
        let local = normalize_saved_provider(provider_fixture(
            "local",
            "Local Name",
            "https://example.com/v1",
            Some("sk-same"),
            "local-model",
            Some(local_toml),
        ))
        .expect("normalize local");
        upsert_provider_on_connection(&conn, local, ProviderUpsertMode::Manual)
            .expect("save local");

        let imported = normalize_saved_provider(provider_fixture(
            "cc-switch-id",
            "CC Name",
            "https://EXAMPLE.com:443/v1/",
            Some("sk-same"),
            "cc-model",
            None,
        ))
        .expect("normalize import");
        let result =
            upsert_provider_on_connection(&conn, imported.clone(), ProviderUpsertMode::Imported)
                .expect("merge import");
        assert_eq!(result.kind, ProviderUpsertKind::Merged);
        assert_eq!(result.provider.id, "local");
        assert_eq!(result.provider.provider_name, "Local Name");
        assert_eq!(result.provider.model, "local-model");
        assert_eq!(
            result.provider.toml_config.as_deref(),
            Some(local_toml.trim_end())
        );
        assert_eq!(list_saved_providers_on_connection(&conn).unwrap().len(), 1);

        let repeated = upsert_provider_on_connection(&conn, imported, ProviderUpsertMode::Imported)
            .expect("repeat identical import");
        assert_eq!(repeated.provider.id, "local");
        assert_eq!(
            repeated.provider.toml_config.as_deref(),
            Some(local_toml.trim_end())
        );
        assert_eq!(list_saved_providers_on_connection(&conn).unwrap().len(), 1);
    }

    #[test]
    fn provider_migration_merges_only_exact_nonempty_credentials() {
        let mut conn = provider_test_connection();
        let first = provider_fixture(
            "first-id",
            "Local Name",
            "HTTPS://EXAMPLE.com:443/v1/",
            Some("sk-same"),
            "local-model",
            None,
        );
        let duplicate = provider_fixture(
            "later-id",
            "Imported Name",
            "https://example.com/v1",
            Some("sk-same"),
            "imported-model",
            Some("local preserved toml"),
        );
        let different_key = provider_fixture(
            "different-key",
            "Local Name",
            "https://example.com/v1",
            Some("sk-other"),
            "other-model",
            None,
        );
        let anonymous_a = provider_fixture(
            "anonymous-a",
            "No Key",
            "https://example.com/v1",
            None,
            "one",
            None,
        );
        let anonymous_b = provider_fixture(
            "anonymous-b",
            " no   key ",
            "https://example.com/v1/",
            None,
            "two",
            None,
        );
        seed_provider(
            &conn,
            &first,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z",
        );
        seed_provider(
            &conn,
            &duplicate,
            "2026-02-01T00:00:00Z",
            "2026-02-01T00:00:00Z",
        );
        seed_provider(
            &conn,
            &different_key,
            "2026-03-01T00:00:00Z",
            "2026-03-01T00:00:00Z",
        );
        seed_provider(
            &conn,
            &anonymous_a,
            "2026-04-01T00:00:00Z",
            "2026-04-01T00:00:00Z",
        );
        seed_provider(
            &conn,
            &anonymous_b,
            "2026-05-01T00:00:00Z",
            "2026-05-01T00:00:00Z",
        );

        assert_eq!(merge_duplicate_provider_identities(&mut conn).unwrap(), 1);
        let rows = list_saved_providers_on_connection(&conn).unwrap();
        assert_eq!(rows.len(), 4);
        let survivor = rows.iter().find(|row| row.id == "first-id").unwrap();
        assert_eq!(survivor.provider_name, "Local Name");
        assert_eq!(survivor.model, "local-model");
        assert_eq!(
            survivor.toml_config.as_deref(),
            Some("local preserved toml")
        );
        assert!(rows.iter().any(|row| row.id == "different-key"));
        assert!(rows.iter().any(|row| row.id == "anonymous-a"));
        assert!(rows.iter().any(|row| row.id == "anonymous-b"));
        assert!(!rows.iter().any(|row| row.id == "later-id"));
    }

    #[test]
    fn provider_slug_collision_does_not_overwrite_an_unrelated_id() {
        let conn = provider_test_connection();
        let existing = provider_fixture(
            "collision-id",
            "Existing",
            "https://first.example/v1",
            Some("sk-first"),
            "first",
            None,
        );
        seed_provider(
            &conn,
            &existing,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z",
        );
        let collision = provider_fixture(
            "Collision ID",
            "Unrelated",
            "https://second.example/v1",
            Some("sk-second"),
            "second",
            None,
        );
        assert!(save_manual_provider_on_connection(&conn, collision).is_err());
        let stored = provider_by_id_on_connection(&conn, "collision-id")
            .unwrap()
            .unwrap();
        assert_eq!(stored.provider_name, "Existing");
        assert_eq!(stored.base_url, "https://first.example/v1");
        assert_eq!(list_saved_providers_on_connection(&conn).unwrap().len(), 1);
    }

    #[test]
    fn detected_provider_id_collision_gets_a_unique_id() {
        let conn = provider_test_connection();
        let existing = normalize_saved_provider(provider_fixture(
            "custom",
            "Existing",
            "https://first.example/v1",
            Some("sk-first"),
            "first",
            None,
        ))
        .unwrap();
        upsert_provider_on_connection(&conn, existing, ProviderUpsertMode::Manual).unwrap();
        let detected = normalize_saved_provider(provider_fixture(
            "custom",
            "Detected",
            "https://second.example/v1",
            Some("sk-second"),
            "second",
            None,
        ))
        .unwrap();
        let result = upsert_provider_on_connection(&conn, detected, ProviderUpsertMode::Detected)
            .expect("save collision safely");
        assert_eq!(result.kind, ProviderUpsertKind::Added);
        assert_eq!(result.provider.id, "custom-2");
        assert_eq!(list_saved_providers_on_connection(&conn).unwrap().len(), 2);
    }

    #[test]
    fn ccswitch_row_reader_supports_legacy_schema_without_category() {
        let conn = Connection::open_in_memory().expect("open legacy cc-switch database");
        conn.execute_batch(
            "CREATE TABLE providers (
                id TEXT NOT NULL,
                app_type TEXT NOT NULL,
                name TEXT NOT NULL,
                settings_config TEXT NOT NULL,
                sort_index INTEGER,
                created_at INTEGER,
                PRIMARY KEY (id, app_type)
            );
            INSERT INTO providers (id, app_type, name, settings_config, sort_index, created_at)
            VALUES ('legacy', 'codex', 'Legacy', '{}', 0, 1);",
        )
        .expect("seed legacy cc-switch database");
        let rows = read_ccswitch_codex_rows(&conn).expect("read legacy rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "legacy");
        assert_eq!(rows[0].category, None);

        let official = CcSwitchCodexRow {
            id: "codex-official".to_string(),
            name: "OpenAI Official".to_string(),
            settings_config: "{}".to_string(),
            category: None,
        };
        assert!(is_official_ccswitch_row(&official));
    }

    #[test]
    fn test_app_home_is_stable_and_does_not_use_real_codexx_home() {
        let first = app_home().expect("resolve test app home");
        let second = app_home().expect("resolve test app home again");
        let real = home_dir().expect("resolve real home").join(".codexx");

        assert_eq!(first, second);
        assert_ne!(first, real);
        assert!(first.starts_with(std::env::temp_dir()));
    }

    #[test]
    fn skills_and_mcp_order_does_not_depend_on_enabled_state() {
        let skill = |id: &str, name: &str, enabled: bool| ManagedSkill {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            directory: id.to_string(),
            enabled,
            source: "test".to_string(),
            path: String::new(),
            content_hash: None,
            update_status: String::new(),
        };
        let server = |id: &str, name: &str, enabled: bool| ManagedMcpServer {
            id: id.to_string(),
            name: name.to_string(),
            transport: "stdio".to_string(),
            enabled,
            source: "test".to_string(),
            summary: String::new(),
            command: None,
            url: None,
            config_json: json!({}),
        };
        let mut skills = vec![
            skill("beta", "Beta", true),
            skill("alpha", "alpha", false),
            skill("gamma", "Gamma", true),
        ];
        let mut servers = vec![
            server("beta", "Beta", false),
            server("alpha", "alpha", true),
            server("gamma", "Gamma", false),
        ];

        sort_managed_skills(&mut skills);
        sort_managed_mcp_servers(&mut servers);
        let skill_order = skills
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        let mcp_order = servers
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        for item in &mut skills {
            item.enabled = !item.enabled;
        }
        for item in &mut servers {
            item.enabled = !item.enabled;
        }
        sort_managed_skills(&mut skills);
        sort_managed_mcp_servers(&mut servers);

        assert_eq!(
            skills
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>(),
            skill_order
        );
        assert_eq!(
            servers
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>(),
            mcp_order
        );
    }

    #[test]
    fn managed_agents_block_preserves_user_content_and_replaces_only_managed_block() {
        let codex_dir = temp_codex_dir("managed-agents");
        let original = "# 我自己的规则\n使用 pnpm。\n";
        write_text(&agents_path(&codex_dir), original).expect("write original agents");

        install_managed_agents_block(
            &codex_dir,
            "builtin:first",
            "# First managed prompt\nfirst rule",
        )
        .expect("install first block");
        install_managed_agents_block(
            &codex_dir,
            "builtin:second",
            "# Second managed prompt\nsecond rule",
        )
        .expect("replace managed block");

        let installed = fs::read_to_string(agents_path(&codex_dir)).expect("read agents");
        assert!(installed.starts_with(original.trim_end()));
        assert!(installed.contains("# Second managed prompt"));
        assert!(!installed.contains("# First managed prompt"));
        assert_eq!(installed.matches(AGENTS_MANAGED_BEGIN).count(), 1);
        assert_eq!(
            managed_agents_template_key_from_content(&installed).as_deref(),
            Some("builtin:second")
        );

        assert!(uninstall_managed_agents_block(&codex_dir).expect("uninstall block"));
        assert_eq!(
            fs::read_to_string(agents_path(&codex_dir)).expect("read restored agents"),
            original
        );
        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn managed_agents_block_rejects_incomplete_markers_without_writing() {
        let codex_dir = temp_codex_dir("managed-agents-incomplete");
        let broken = format!("# user\n\n{AGENTS_MANAGED_BEGIN}\nunfinished\n");
        write_text(&agents_path(&codex_dir), &broken).expect("write broken agents");

        let result = install_managed_agents_block(&codex_dir, "builtin:test", "content");
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(agents_path(&codex_dir)).expect("read unchanged agents"),
            broken
        );
        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn github_catalog_discovers_new_markdown_without_a_hardcoded_id() {
        let catalog = github_prompt_catalog_from_entries(vec![
            GithubContentEntry {
                name: "brand-new-prompt.md".to_string(),
                kind: "file".to_string(),
                download_url: Some(
                    "https://raw.githubusercontent.com/yynxxxxx/Codex-X/main/examples/brand-new-prompt.md"
                        .to_string(),
                ),
            },
            GithubContentEntry {
                name: "notes.txt".to_string(),
                kind: "file".to_string(),
                download_url: Some("https://example.invalid/notes.txt".to_string()),
            },
            GithubContentEntry {
                name: "BRAND-NEW-PROMPT.MD".to_string(),
                kind: "file".to_string(),
                download_url: Some(
                    "https://raw.githubusercontent.com/yynxxxxx/Codex-X/main/examples/BRAND-NEW-PROMPT.MD"
                        .to_string(),
                ),
            },
        ])
        .expect("build GitHub prompt catalog");
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].1, "brand-new-prompt.md");
        assert!(catalog[0].0.starts_with("github-brand-new-prompt-"));
        assert_eq!(
            stable_remote_prompt_id("brand-new-prompt.md"),
            stable_remote_prompt_id("BRAND-NEW-PROMPT.MD")
        );
    }

    #[test]
    fn github_catalog_rejects_markdown_without_a_download_url() {
        let catalog = github_prompt_catalog_from_entries(vec![GithubContentEntry {
            name: "missing-url.md".to_string(),
            kind: "file".to_string(),
            download_url: None,
        }]);

        assert!(catalog.is_err());
    }

    #[test]
    fn empty_cache_fallback_uses_only_bundled_prompts() {
        let statuses = cached_prompt_fallback_statuses(Vec::new());
        let ids = statuses
            .iter()
            .map(|status| status.id.as_str())
            .collect::<HashSet<_>>();

        assert_eq!(statuses.len(), bundled_prompt_metas().len());
        assert_eq!(ids.len(), statuses.len());
        assert!(statuses
            .iter()
            .all(|status| status.content_source == "bundled" && !status.cached));
    }

    #[test]
    fn stale_prompt_cache_ids_follow_authoritative_catalog() {
        let cache = |id: &str, filename: &str| CachedBuiltinPrompt {
            id: id.to_string(),
            filename: filename.to_string(),
            source_url: format!("https://example.invalid/{filename}"),
            content: "cached".to_string(),
            checked_at: "2026-07-11T00:00:00+08:00".to_string(),
        };
        let caches = vec![
            cache("gpt5.5-unrestricted", "gpt5.5-unrestricted.md"),
            cache("github-new", "new.md"),
            cache("github-removed", "removed.md"),
            cache("legacy-alias", "new.md"),
        ];
        let active_ids =
            HashSet::from(["gpt5.5-unrestricted".to_string(), "github-new".to_string()]);

        assert_eq!(
            stale_cached_prompt_ids(&caches, &active_ids),
            vec!["github-removed".to_string(), "legacy-alias".to_string()]
        );
    }

    #[test]
    fn cache_fallback_is_unique_and_keeps_remote_templates_offline() {
        let cache = |id: &str, filename: &str| CachedBuiltinPrompt {
            id: id.to_string(),
            filename: filename.to_string(),
            source_url: format!("https://example.invalid/{filename}"),
            content: "cached".to_string(),
            checked_at: "2026-07-11T00:00:00+08:00".to_string(),
        };
        let statuses = cached_prompt_fallback_statuses(vec![
            cache("gpt5.5-unrestricted", "gpt5.5-unrestricted.md"),
            cache("gpt5.4-unrestricted", "gpt5.4-unrestricted.md"),
            cache("gpt5.5-jeli", "gpt5.5-jeli.md"),
            cache("github-new", "new.md"),
            cache("legacy-new", "new.md"),
        ]);
        let ids = statuses
            .iter()
            .map(|status| status.id.to_ascii_lowercase())
            .collect::<HashSet<_>>();
        let filenames = statuses
            .iter()
            .map(|status| status.filename.to_ascii_lowercase())
            .collect::<HashSet<_>>();

        assert_eq!(statuses.len(), 4);
        assert_eq!(ids.len(), statuses.len());
        assert_eq!(filenames.len(), statuses.len());
        assert!(statuses.iter().any(|status| status.filename == "new.md"));
    }

    #[test]
    fn deleting_stale_prompt_cache_ids_removes_database_rows() {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(
            "CREATE TABLE builtin_prompt_cache (id TEXT PRIMARY KEY);
             INSERT INTO builtin_prompt_cache (id) VALUES ('keep'), ('remove-old'), ('remove-alias');",
        )
        .expect("seed prompt cache");
        let stale_ids = vec!["remove-old".to_string(), "remove-alias".to_string()];

        assert_eq!(
            delete_cached_prompt_ids(&mut conn, &stale_ids).expect("delete stale rows"),
            2
        );
        let remaining = conn
            .query_row(
                "SELECT group_concat(id, ',') FROM builtin_prompt_cache",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("read remaining rows");
        assert_eq!(remaining, "keep");
    }

    #[test]
    fn full_toml_match_selects_only_the_actual_provider() {
        let first_toml = r#"model_provider = "custom"
model = "gpt-5.5"
model_reasoning_effort = "high"

[model_providers.custom]
name = "Same API"
base_url = "https://example.com/v1"
wire_api = "responses"
"#;
        let second_toml = r#"model_provider = "custom"
model = "gpt-5.5"
model_reasoning_effort = "xhigh"

[model_providers.custom]
name = "Same API"
base_url = "https://example.com/v1"
wire_api = "responses"
"#;
        let provider = |id: &str, toml: &str| SavedProvider {
            id: id.to_string(),
            provider_name: "Same API".to_string(),
            base_url: "https://example.com/v1".to_string(),
            model: "gpt-5.5".to_string(),
            api_key: Some("sk-same".to_string()),
            toml_config: Some(toml.to_string()),
            wire_api: "responses".to_string(),
            requires_openai_auth: true,
        };
        let live = second_toml.replace(
            "wire_api = \"responses\"",
            "wire_api = \"responses\"\nexperimental_bearer_token = \"sk-same\"",
        );
        let matched = active_saved_provider_id_from_config(
            &live,
            &[
                provider("first", first_toml),
                provider("second", second_toml),
            ],
        );
        assert_eq!(matched.as_deref(), Some("second"));
    }

    #[test]
    fn append_mode_preserves_external_prompt_and_disable_removes_only_managed_agents() {
        let codex_dir = temp_codex_dir("append-prompt");
        write_text(
            &config_path(&codex_dir),
            "model = \"gpt-5.5\"\nmodel_instructions_file = \"./user-original.md\"\n",
        )
        .expect("write config");
        write_text(&codex_dir.join("user-original.md"), "user prompt").expect("write user prompt");
        write_text(&agents_path(&codex_dir), "# User AGENTS\nkeep this\n").expect("write agents");

        let enabled = enable_prompt_content_inner(
            Some(codex_dir.display().to_string()),
            INSTRUCTION_FILENAME,
            "managed prompt",
            "builtin:gpt5.5-unrestricted",
            "managed",
            "test",
            PromptInjectionMode::Append,
            "test-append",
        )
        .expect("enable append");
        assert_eq!(
            enabled.state.instruction_injection_mode.as_deref(),
            Some("append")
        );
        assert!(enabled.state.instruction_enabled);
        let config = fs::read_to_string(config_path(&codex_dir)).expect("read config");
        assert!(config.contains("model_instructions_file = \"./user-original.md\""));
        let agents = fs::read_to_string(agents_path(&codex_dir)).expect("read agents");
        assert!(agents.contains("# User AGENTS"));
        assert!(agents.contains("managed prompt"));

        disable_instruction_inner(Some(codex_dir.display().to_string()), Some(true))
            .expect("disable managed append");
        let config =
            fs::read_to_string(config_path(&codex_dir)).expect("read config after disable");
        assert!(config.contains("model_instructions_file = \"./user-original.md\""));
        assert_eq!(
            fs::read_to_string(agents_path(&codex_dir)).expect("read agents after disable"),
            "# User AGENTS\nkeep this\n"
        );
        assert!(codex_dir.join("user-original.md").exists());
        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn replace_mode_keeps_unrelated_agents_content() {
        let codex_dir = temp_codex_dir("replace-prompt");
        write_text(&agents_path(&codex_dir), "# User AGENTS\nkeep this\n").expect("write agents");

        let enabled = enable_prompt_content_inner(
            Some(codex_dir.display().to_string()),
            INSTRUCTION_FILENAME,
            "managed prompt",
            "builtin:gpt5.5-unrestricted",
            "managed",
            "test",
            PromptInjectionMode::Replace,
            "test-replace",
        )
        .expect("enable replace");
        assert_eq!(
            enabled.state.instruction_injection_mode.as_deref(),
            Some("replace")
        );
        assert_eq!(
            fs::read_to_string(agents_path(&codex_dir)).expect("read agents"),
            "# User AGENTS\nkeep this\n"
        );
        assert!(fs::read_to_string(config_path(&codex_dir))
            .expect("read config")
            .contains("model_instructions_file = \"./gpt5.5-unrestricted.md\""));
        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn restore_backup_restores_agents_file_alongside_config() {
        let codex_dir = temp_codex_dir("restore-agents");
        write_text(&config_path(&codex_dir), "model = \"gpt-5.5\"\n").expect("write config");
        write_text(&agents_path(&codex_dir), "# Original AGENTS\n").expect("write agents");
        let backup_id = create_backup(&codex_dir, "before-agents-change")
            .expect("create backup")
            .expect("backup id");

        write_text(&config_path(&codex_dir), "model = \"changed\"\n").expect("change config");
        write_text(&agents_path(&codex_dir), "# Changed AGENTS\n").expect("change agents");
        restore_backup_inner(Some(codex_dir.display().to_string()), backup_id)
            .expect("restore backup");

        assert_eq!(
            fs::read_to_string(config_path(&codex_dir)).expect("read restored config"),
            "model = \"gpt-5.5\"\n"
        );
        assert_eq!(
            fs::read_to_string(agents_path(&codex_dir)).expect("read restored agents"),
            "# Original AGENTS\n"
        );
        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn skill_metadata_reads_frontmatter_name_before_directory() {
        let dir = temp_codex_dir("skill-frontmatter").join("skill-zip-123");
        fs::create_dir_all(&dir).expect("create skill dir");
        write_text(
            &dir.join("SKILL.md"),
            r#"---
name: ctf-sandbox-runner
description: Resume authorized CTF sandbox projects.
---

# CTF Sandbox Runner
"#,
        )
        .expect("write skill");

        let (name, desc) = read_skill_metadata(&dir, "skill-zip-123");
        assert_eq!(name, "ctf-sandbox-runner");
        assert_eq!(
            desc.as_deref(),
            Some("Resume authorized CTF sandbox projects.")
        );

        let root = dir.parent().unwrap().to_path_buf();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn normalize_legacy_zip_skill_dir_renames_to_metadata_name() {
        let root = temp_codex_dir("skill-normalize");
        let dir = root.join("skill-zip-1783334291187");
        fs::create_dir_all(&dir).expect("create legacy skill dir");
        write_text(
            &dir.join("SKILL.md"),
            r#"---
name: mission-keeper
description: Keep long investigations aligned.
---
"#,
        )
        .expect("write skill");

        normalize_legacy_zip_skill_dirs(&root).expect("normalize");
        assert!(!dir.exists());
        assert!(root.join("mission-keeper").join("SKILL.md").exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn switch_provider_writes_scoped_bearer_and_api_key_auth_mode() {
        let codex_dir = temp_codex_dir("switch-provider");
        let result = switch_provider_inner(ProviderInput {
            config_dir: Some(codex_dir.display().to_string()),
            _provider_id: Some("magicai".to_string()),
            provider_name: "MagicAI".to_string(),
            base_url: "https://example.com/v1/".to_string(),
            model: "gpt-5.5".to_string(),
            api_key: Some("sk-test".to_string()),
            wire_api: Some("responses".to_string()),
            requires_openai_auth: None,
        })
        .expect("switch provider");

        assert_eq!(result.state.model_provider.as_deref(), Some("custom"));
        assert_eq!(result.state.model.as_deref(), Some("gpt-5.5"));

        let config_text = fs::read_to_string(config_path(&codex_dir)).expect("read config");
        assert!(config_text.contains("model_provider = \"custom\""));
        assert!(config_text.contains("[model_providers.custom]"));
        assert!(config_text.contains("name = \"MagicAI\""));
        assert!(config_text.contains("base_url = \"https://example.com/v1\""));
        assert!(config_text.contains("requires_openai_auth = true"));
        let config_doc = config_text
            .parse::<DocumentMut>()
            .expect("parse switched config");
        assert_eq!(
            config_doc["model_providers"]["custom"]["experimental_bearer_token"].as_str(),
            Some("sk-test")
        );
        assert!(config_doc.get("experimental_bearer_token").is_none());

        let auth_text = fs::read_to_string(auth_path(&codex_dir)).expect("read auth");
        let auth: Value = serde_json::from_str(&auth_text).expect("parse auth");
        assert_eq!(
            auth.get("OPENAI_API_KEY").and_then(Value::as_str),
            Some("sk-test")
        );
        assert_eq!(
            auth.get("auth_mode").and_then(Value::as_str),
            Some("apikey")
        );

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn switch_provider_persists_detected_custom_before_overwrite() {
        let codex_dir = temp_codex_dir("switch-provider-persist-current");
        write_text(
            &config_path(&codex_dir),
            r#"model_provider = "custom"
model = "model-a"

[model_providers.custom]
name = "Provider A"
base_url = "https://a.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "sk-a-scoped"
"#,
        )
        .expect("write provider A config");
        write_text(&auth_path(&codex_dir), "{ invalid auth")
            .expect("write malformed provider A auth");

        let persisted = std::cell::RefCell::new(None);
        let result = switch_provider_with_pre_persist(
            ProviderInput {
                config_dir: Some(codex_dir.display().to_string()),
                _provider_id: Some("provider-b".to_string()),
                provider_name: "Provider B".to_string(),
                base_url: "https://b.example.com/v1".to_string(),
                model: "model-b".to_string(),
                api_key: Some("sk-b".to_string()),
                wire_api: Some("responses".to_string()),
                requires_openai_auth: Some(false),
            },
            |dir| {
                *persisted.borrow_mut() = detected_live_custom_provider(dir)?;
                Ok(())
            },
        )
        .expect("switch to provider B");

        let provider_a = persisted.into_inner().expect("provider A persisted");
        assert_eq!(provider_a.provider_name, "Provider A");
        assert_eq!(provider_a.base_url, "https://a.example.com/v1");
        assert_eq!(provider_a.model, "model-a");
        assert_eq!(provider_a.api_key.as_deref(), Some("sk-a-scoped"));
        assert!(!provider_a
            .toml_config
            .as_deref()
            .unwrap_or_default()
            .contains("experimental_bearer_token"));
        assert_eq!(result.state.model.as_deref(), Some("model-b"));
        assert!(result
            .state
            .config_text
            .contains("https://b.example.com/v1"));
        assert!(!result
            .state
            .config_text
            .contains("https://a.example.com/v1"));

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn switch_provider_reserved_builtin_ids_still_write_live_custom() {
        let codex_dir = temp_codex_dir("switch-provider-reserved");
        let result = switch_provider_inner(ProviderInput {
            config_dir: Some(codex_dir.display().to_string()),
            _provider_id: Some("openai".to_string()),
            provider_name: "OpenAI".to_string(),
            base_url: "https://proxy.example.com/v1".to_string(),
            model: "gpt-5.5".to_string(),
            api_key: Some("sk-proxy".to_string()),
            wire_api: Some("responses".to_string()),
            requires_openai_auth: None,
        })
        .expect("switch provider");

        assert_eq!(result.state.model_provider.as_deref(), Some("custom"));
        let config_text = fs::read_to_string(config_path(&codex_dir)).expect("read config");
        assert!(config_text.contains("model_provider = \"custom\""));
        assert!(config_text.contains("[model_providers.custom]"));
        assert!(!config_text.contains("[model_providers.openai]"));

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn switch_official_persists_detected_custom_before_overwrite() {
        let codex_dir = temp_codex_dir("switch-official-persist-current");
        write_text(
            &config_path(&codex_dir),
            r#"model_provider = "custom"
model = "model-a"

[model_providers.custom]
name = "Provider A"
base_url = "https://a.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
"#,
        )
        .expect("write provider A config");
        write_json(
            &auth_path(&codex_dir),
            &json!({"OPENAI_API_KEY": "sk-a-auth", "auth_mode": "apikey"}),
        )
        .expect("write provider A auth");

        let persisted = std::cell::RefCell::new(None);
        let result = switch_official_provider_with_pre_persist(
            Some(codex_dir.display().to_string()),
            |dir| {
                *persisted.borrow_mut() = detected_live_custom_provider(dir)?;
                Ok(())
            },
        )
        .expect("switch to official");

        let provider_a = persisted.into_inner().expect("provider A persisted");
        assert_eq!(provider_a.provider_name, "Provider A");
        assert_eq!(provider_a.base_url, "https://a.example.com/v1");
        assert_eq!(provider_a.api_key.as_deref(), Some("sk-a-auth"));
        assert_eq!(result.state.model_provider.as_deref(), Some("openai"));
        assert!(!result
            .state
            .config_text
            .contains("https://a.example.com/v1"));

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn switch_official_preserves_live_auth_json() {
        let codex_dir = temp_codex_dir("switch-official-preserve-auth");
        write_text(
            &config_path(&codex_dir),
            r#"model_provider = "custom"
model = "gpt-5.5"

[model_providers.custom]
name = "Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
"#,
        )
        .expect("write config");
        write_json(
            &auth_path(&codex_dir),
            &json!({
                "OPENAI_API_KEY": "sk-live",
                "auth_mode": "apikey"
            }),
        )
        .expect("write auth");

        let result =
            switch_official_provider_inner(Some(codex_dir.display().to_string())).expect("switch");
        assert_eq!(result.state.model_provider.as_deref(), Some("openai"));

        let auth_text = fs::read_to_string(auth_path(&codex_dir)).expect("read auth");
        let auth: Value = serde_json::from_str(&auth_text).expect("parse auth");
        assert_eq!(
            auth.get("OPENAI_API_KEY").and_then(Value::as_str),
            Some("sk-live")
        );
        assert_eq!(
            auth.get("auth_mode").and_then(Value::as_str),
            Some("apikey")
        );

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn provider_status_403_is_not_ok() {
        let result = provider_status_result(403, 123);
        assert!(!result.ok);
        assert_eq!(result.status, Some(403));
        assert_eq!(result.duration_ms, 123);
    }

    #[test]
    fn import_ccswitch_provider_reads_experimental_bearer_token() {
        let settings_config = json!({
            "auth": {},
            "config": r#"model_provider = "custom"
model = "gpt-5.5"

[model_providers.custom]
name = "Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "sk-from-config"
"#,
        })
        .to_string();

        let row = CcSwitchCodexRow {
            id: "openai".to_string(),
            name: "Proxy".to_string(),
            settings_config,
            category: None,
        };
        let provider = build_ccswitch_codex_provider(&row, &HashMap::new()).expect("provider");
        assert_eq!(provider.id, "openai-custom");
        assert_eq!(provider.api_key.as_deref(), Some("sk-from-config"));
        assert_eq!(provider.base_url, "https://proxy.example.com/v1");
    }

    #[test]
    fn import_ccswitch_provider_uses_row_id_section_not_stale_active_provider() {
        let sky_row = CcSwitchCodexRow {
            id: "sky2api-1782194988817".to_string(),
            name: "Sky2api".to_string(),
            settings_config: json!({
                "auth": {"OPENAI_API_KEY": "sk-sky"},
                "config": r#"model = "gpt-5.5"
model_provider = "magicai-1782956845071"

[model_providers.magicai-1782956845071]
name = "MagicAI"
base_url = "https://sky1818.com"
wire_api = "responses"
requires_openai_auth = true
"#,
            })
            .to_string(),
            category: None,
        };
        let magic_row = CcSwitchCodexRow {
            id: "magicai-1782956845071".to_string(),
            name: "MagicAI".to_string(),
            settings_config: json!({
                "auth": {"OPENAI_API_KEY": "sk-magic"},
                "config": r#"model = "gpt-5.5"
model_provider = "sky2api-1782194988817"

[model_providers.magicai-1782956845071]
name = "MagicAI"
base_url = "https://sky1818.com"
wire_api = "responses"
requires_openai_auth = true

[model_providers.sky2api-1782194988817]
name = "Sky2api"
base_url = "https://ikuncode.site/v1"
wire_api = "responses"
requires_openai_auth = true
"#,
            })
            .to_string(),
            category: None,
        };

        let mut sections = HashMap::new();
        for row in [&sky_row, &magic_row] {
            let settings: Value = serde_json::from_str(&row.settings_config).expect("settings");
            for section in codex_sections_from_config(
                settings.get("config").and_then(Value::as_str).unwrap_or(""),
            ) {
                sections.entry(section.id.clone()).or_insert(section);
            }
        }

        let sky = build_ccswitch_codex_provider(&sky_row, &sections).expect("sky");
        let magic = build_ccswitch_codex_provider(&magic_row, &sections).expect("magic");

        assert_eq!(sky.provider_name, "Sky2api");
        assert_eq!(sky.base_url, "https://ikuncode.site/v1");
        assert_eq!(sky.api_key.as_deref(), Some("sk-sky"));

        assert_eq!(magic.provider_name, "MagicAI");
        assert_eq!(magic.base_url, "https://sky1818.com");
        assert_eq!(magic.api_key.as_deref(), Some("sk-magic"));
    }

    #[test]
    fn save_provider_toml_config_writes_provider_scoped_bearer_token() {
        let codex_dir = temp_codex_dir("save-provider-toml-token");
        let result = save_provider_toml_config_inner(ProviderTomlInput {
            config_dir: Some(codex_dir.display().to_string()),
            config_text: r#"model_provider = "proxy"
model = "gpt-5.5"

[model_providers.proxy]
name = "Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
"#
            .to_string(),
            api_key: Some("sk-provider-table".to_string()),
        })
        .expect("save provider toml");

        assert!(result.ok);
        let config_text = fs::read_to_string(config_path(&codex_dir)).expect("read config");
        assert!(config_text.contains("[model_providers.proxy]"));
        assert!(config_text.contains("experimental_bearer_token = \"sk-provider-table\""));

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn save_provider_toml_persists_detected_custom_before_overwrite() {
        let codex_dir = temp_codex_dir("save-provider-toml-persist-current");
        write_text(
            &config_path(&codex_dir),
            r#"model_provider = "custom"
model = "model-a"

[model_providers.custom]
name = "Provider A"
base_url = "https://a.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
experimental_bearer_token = "sk-a"
"#,
        )
        .expect("write provider A config");

        let persisted = std::cell::RefCell::new(None);
        let result = save_provider_toml_config_with_pre_persist(
            ProviderTomlInput {
                config_dir: Some(codex_dir.display().to_string()),
                config_text: r#"model_provider = "custom"
model = "model-b"

[model_providers.custom]
name = "Provider B"
base_url = "https://b.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
"#
                .to_string(),
                api_key: Some("sk-b".to_string()),
            },
            |dir| {
                *persisted.borrow_mut() = detected_live_custom_provider(dir)?;
                Ok(())
            },
        )
        .expect("save provider B toml");

        let provider_a = persisted.into_inner().expect("provider A persisted");
        assert_eq!(provider_a.provider_name, "Provider A");
        assert_eq!(provider_a.base_url, "https://a.example.com/v1");
        assert_eq!(provider_a.api_key.as_deref(), Some("sk-a"));
        assert_eq!(result.state.model.as_deref(), Some("model-b"));
        assert!(result
            .state
            .config_text
            .contains("https://b.example.com/v1"));
        assert!(result
            .state
            .config_text
            .contains("experimental_bearer_token = \"sk-b\""));
        assert!(!result
            .state
            .config_text
            .contains("https://a.example.com/v1"));

        let _ = fs::remove_dir_all(codex_dir);
    }

    fn seed_thread_database(
        path: &Path,
        sessions: &[(&str, &Path)],
        spawn_edge: Option<(&str, &str)>,
    ) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create sqlite parent");
        }
        let conn = Connection::open(path).expect("open thread database");
        conn.execute_batch(
            "CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                model_provider TEXT NOT NULL,
                rollout_path TEXT
             );
             CREATE TABLE thread_dynamic_tools (thread_id TEXT NOT NULL);
             CREATE TABLE thread_spawn_edges (parent_thread_id TEXT NOT NULL, child_thread_id TEXT NOT NULL);
             CREATE TABLE agent_job_items (assigned_thread_id TEXT);",
        )
        .expect("create thread schema");
        for (id, rollout) in sessions {
            conn.execute(
                "INSERT INTO threads (id, model_provider, rollout_path) VALUES (?1, 'openai', ?2)",
                (id, rollout.display().to_string()),
            )
            .expect("insert thread");
            conn.execute(
                "INSERT INTO thread_dynamic_tools (thread_id) VALUES (?1)",
                [id],
            )
            .expect("insert dynamic tool");
            conn.execute(
                "INSERT INTO agent_job_items (assigned_thread_id) VALUES (?1)",
                [id],
            )
            .expect("insert job item");
        }
        if let Some((parent, child)) = spawn_edge {
            conn.execute(
                "INSERT INTO thread_spawn_edges (parent_thread_id, child_thread_id) VALUES (?1, ?2)",
                (parent, child),
            )
            .expect("insert spawn edge");
        }
    }

    fn sqlite_count(path: &Path, sql: &str) -> i64 {
        Connection::open(path)
            .expect("open sqlite for count")
            .query_row(sql, [], |row| row.get(0))
            .expect("read sqlite count")
    }

    #[test]
    fn active_session_database_prefers_current_root_over_legacy_sqlite_copy() {
        let codex_dir = temp_codex_dir("active-session-db");
        let current = codex_dir.join("state_5.sqlite");
        let legacy = codex_dir.join("sqlite/state_5.sqlite");
        seed_thread_database(&current, &[], None);
        seed_thread_database(&legacy, &[], None);

        assert_eq!(sqlite_candidate_paths(&codex_dir), vec![current]);

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn active_session_database_prefers_highest_numeric_state_version() {
        let codex_dir = temp_codex_dir("active-session-db-version");
        let old_current = codex_dir.join("state_4.sqlite");
        let newest_current = codex_dir.join("state_10.sqlite");
        let legacy = codex_dir.join("sqlite/state_99.sqlite");
        seed_thread_database(&old_current, &[], None);
        seed_thread_database(&newest_current, &[], None);
        seed_thread_database(&legacy, &[], None);

        assert_eq!(sqlite_candidate_paths(&codex_dir), vec![newest_current]);

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn active_session_verifier_rejects_missing_predelete_database_paths() {
        let ids = HashSet::from(["019f6000-0000-7000-8000-000000000001".to_string()]);

        assert!(active_session_ids_present(&[], &ids).is_err());
    }

    #[test]
    fn active_session_verifier_checks_the_precaptured_database() {
        let codex_dir = temp_codex_dir("active-session-db-verifier");
        let database = codex_dir.join("state_5.sqlite");
        let present_id = "019f6000-0000-7000-8000-000000000001";
        let absent_id = "019f6000-0000-7000-8000-000000000002";
        let rollout = codex_dir.join("sessions/rollout.jsonl");
        seed_thread_database(&database, &[(present_id, &rollout)], None);
        let ids = HashSet::from([present_id.to_string(), absent_id.to_string()]);

        assert_eq!(
            active_session_ids_present(&[database], &ids).expect("verify active database"),
            HashSet::from([present_id.to_string()])
        );

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn session_previews_hide_subagents_without_deduplicating_root_titles() {
        let codex_dir = temp_codex_dir("session-preview-subagents");
        let database = codex_dir.join("state_5.sqlite");
        let root_a = "019f6000-0000-7000-8000-000000000001";
        let root_b = "019f6000-0000-7000-8000-000000000002";
        let child = "019f6000-0000-7000-8000-000000000003";
        let orphan_subagent = "019f6000-0000-7000-8000-000000000004";
        let forked_user = "019f6000-0000-7000-8000-000000000005";
        let rollout = codex_dir.join("sessions/rollout.jsonl");
        seed_thread_database(
            &database,
            &[
                (root_a, &rollout),
                (root_b, &rollout),
                (child, &rollout),
                (forked_user, &rollout),
            ],
            Some((root_a, child)),
        );
        let conn = Connection::open(&database).expect("open thread database");
        conn.execute_batch(
            "ALTER TABLE threads ADD COLUMN title TEXT;
             ALTER TABLE threads ADD COLUMN source TEXT;
             ALTER TABLE threads ADD COLUMN thread_source TEXT;
             UPDATE threads SET title = 'same title';",
        )
        .expect("extend thread schema");
        conn.execute(
            "UPDATE threads SET thread_source = 'subagent' WHERE id = ?1",
            [child],
        )
        .expect("mark child subagent");
        conn.execute(
            "UPDATE threads SET thread_source = 'user' WHERE id = ?1",
            [forked_user],
        )
        .expect("mark forked user thread");
        conn.execute(
            "INSERT INTO thread_spawn_edges (parent_thread_id, child_thread_id) VALUES (?1, ?2)",
            (root_a, forked_user),
        )
        .expect("insert user fork edge");
        conn.execute(
            "INSERT INTO threads (id, model_provider, rollout_path, title, source)
             VALUES (?1, 'openai', ?2, 'same title', ?3)",
            params![
                orphan_subagent,
                rollout.display().to_string(),
                r#"{"subagent":{"thread_spawn":{"depth":1}}}"#
            ],
        )
        .expect("insert source-marked subagent");
        drop(conn);

        let scan = scan_sqlite(&codex_dir, "openai").expect("scan sqlite");
        assert_eq!(scan.sqlite_threads, 5);
        assert_eq!(scan.top_level_threads, 3);
        assert_eq!(scan.subagent_threads, 2);

        let (previews, warnings) =
            list_session_previews(&codex_dir, "openai", 50).expect("list previews");
        assert!(warnings.is_empty());
        assert_eq!(
            previews
                .into_iter()
                .map(|item| item.id)
                .collect::<HashSet<_>>(),
            HashSet::from([
                root_a.to_string(),
                root_b.to_string(),
                forked_user.to_string()
            ])
        );

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn local_session_delete_removes_duplicates_descendants_files_and_related_rows() {
        let codex_dir = temp_codex_dir("hard-delete-sessions");
        let parent_id = "019f6000-0000-7000-8000-000000000001";
        let child_id = "019f6000-0000-7000-8000-000000000002";
        let keep_id = "019f6000-0000-7000-8000-000000000003";
        let active_dir = codex_dir.join("sessions/2026/07/13");
        let archived_dir = codex_dir.join("archived_sessions/2026/07/13");
        fs::create_dir_all(&active_dir).expect("create active sessions");
        fs::create_dir_all(&archived_dir).expect("create archived sessions");
        let parent_rollout = active_dir.join(format!("rollout-test-{parent_id}.jsonl"));
        let child_rollout = archived_dir.join(format!("rollout-test-{child_id}.jsonl"));
        let child_compressed = archived_dir.join(format!("rollout-test-{child_id}.jsonl.zst"));
        let keep_rollout = active_dir.join(format!("rollout-test-{keep_id}.jsonl"));
        for (id, path) in [
            (parent_id, &parent_rollout),
            (child_id, &child_rollout),
            (keep_id, &keep_rollout),
        ] {
            write_text(
                path,
                &format!(r#"{{"type":"session_meta","payload":{{"id":"{id}"}}}}"#),
            )
            .expect("write rollout");
        }
        fs::write(&child_compressed, b"compressed-placeholder").expect("write zstd rollout");

        let current = codex_dir.join("state_5.sqlite");
        let legacy = codex_dir.join("sqlite/state_5.sqlite");
        seed_thread_database(
            &current,
            &[
                (parent_id, &parent_rollout),
                (child_id, &child_rollout),
                (keep_id, &keep_rollout),
            ],
            Some((parent_id, child_id)),
        );
        seed_thread_database(
            &legacy,
            &[(parent_id, &parent_rollout), (keep_id, &keep_rollout)],
            Some((parent_id, keep_id)),
        );

        let unrelated = codex_dir.join("unrelated.sqlite");
        let unrelated_conn = Connection::open(&unrelated).expect("open unrelated database");
        unrelated_conn
            .execute("CREATE TABLE logs (thread_id TEXT)", [])
            .expect("create unrelated table");
        unrelated_conn
            .execute("INSERT INTO logs (thread_id) VALUES (?1)", [parent_id])
            .expect("insert unrelated row");
        drop(unrelated_conn);

        let catalog = codex_dir.join("sqlite/codex-dev.db");
        let catalog_conn = Connection::open(&catalog).expect("open catalog");
        catalog_conn
            .execute_batch(
                "CREATE TABLE local_thread_catalog (thread_id TEXT);
                 CREATE TABLE automation_runs (thread_id TEXT);
                 CREATE TABLE inbox_items (thread_id TEXT);",
            )
            .expect("create catalog schema");
        for id in [parent_id, child_id, keep_id] {
            for table in ["local_thread_catalog", "automation_runs", "inbox_items"] {
                catalog_conn
                    .execute(
                        &format!("INSERT INTO {table} (thread_id) VALUES (?1)"),
                        [id],
                    )
                    .expect("insert catalog reference");
            }
        }
        drop(catalog_conn);

        for (filename, table) in [
            ("logs_2.sqlite", "logs"),
            ("memories_1.sqlite", "stage1_outputs"),
            ("goals_1.sqlite", "thread_goals"),
        ] {
            let path = codex_dir.join(filename);
            let conn = Connection::open(path).expect("open related database");
            conn.execute(&format!("CREATE TABLE {table} (thread_id TEXT)"), [])
                .expect("create related schema");
            for id in [parent_id, child_id, keep_id] {
                conn.execute(
                    &format!("INSERT INTO {table} (thread_id) VALUES (?1)"),
                    [id],
                )
                .expect("insert related row");
            }
        }

        write_text(
            &codex_dir.join("session_index.jsonl"),
            &format!(
                "{{\"id\":\"{parent_id}\",\"thread_name\":\"parent\"}}\nnot-json\n{{\"id\":\"{child_id}\",\"thread_name\":\"child\"}}\n{{\"id\":\"{keep_id}\",\"thread_name\":\"keep\"}}\n"
            ),
        )
        .expect("write session index");
        write_text(
            &codex_dir.join("history.jsonl"),
            &format!(
                "{{\"session_id\":\"{parent_id}\",\"text\":\"parent secret\"}}\ninvalid-history\n{{\"session_id\":\"{child_id}\",\"text\":\"child secret\"}}\n{{\"session_id\":\"{keep_id}\",\"text\":\"keep\"}}\n"
            ),
        )
        .expect("write session history");
        let snapshots = codex_dir.join("shell_snapshots");
        fs::create_dir_all(&snapshots).expect("create shell snapshots");
        let parent_snapshot = snapshots.join(format!("{parent_id}.100.sh"));
        let child_snapshot = snapshots.join(format!("{child_id}.200.sh"));
        let keep_snapshot = snapshots.join(format!("{keep_id}.300.sh"));
        fs::write(&parent_snapshot, "parent").expect("write parent snapshot");
        fs::write(&child_snapshot, "child").expect("write child snapshot");
        fs::write(&keep_snapshot, "keep").expect("write keep snapshot");

        let result = hard_delete_sessions_locally(&codex_dir, &[parent_id.to_string()])
            .expect("hard delete parent session");

        assert!(result.errors.is_empty());
        assert_eq!(result.deleted_ids.len(), 2);
        assert!(result.deleted_ids.contains(parent_id));
        assert!(result.deleted_ids.contains(child_id));
        assert_eq!(result.deleted_thread_rows, 3);
        assert_eq!(result.deleted_rollout_files, 3);
        assert!(!parent_rollout.exists());
        assert!(!child_rollout.exists());
        assert!(!child_compressed.exists());
        assert!(keep_rollout.exists());
        assert_eq!(sqlite_count(&current, "SELECT COUNT(*) FROM threads"), 1);
        assert_eq!(sqlite_count(&legacy, "SELECT COUNT(*) FROM threads"), 1);
        assert_eq!(
            sqlite_count(
                &current,
                "SELECT COUNT(*) FROM agent_job_items WHERE assigned_thread_id IS NOT NULL"
            ),
            1
        );
        assert_eq!(
            sqlite_count(&catalog, "SELECT COUNT(*) FROM local_thread_catalog"),
            1
        );
        assert_eq!(
            sqlite_count(
                &codex_dir.join("logs_2.sqlite"),
                "SELECT COUNT(*) FROM logs"
            ),
            1
        );
        assert_eq!(
            sqlite_count(
                &codex_dir.join("memories_1.sqlite"),
                "SELECT COUNT(*) FROM stage1_outputs"
            ),
            1
        );
        assert_eq!(
            sqlite_count(
                &codex_dir.join("goals_1.sqlite"),
                "SELECT COUNT(*) FROM thread_goals"
            ),
            1
        );
        assert_eq!(sqlite_count(&unrelated, "SELECT COUNT(*) FROM logs"), 1);
        let index = fs::read_to_string(codex_dir.join("session_index.jsonl"))
            .expect("read filtered session index");
        assert!(!index.contains(parent_id));
        assert!(!index.contains(child_id));
        assert!(index.contains(keep_id));
        assert!(index.contains("not-json"));
        let history =
            fs::read_to_string(codex_dir.join("history.jsonl")).expect("read filtered history");
        assert!(!history.contains("parent secret"));
        assert!(!history.contains("child secret"));
        assert!(history.contains(keep_id));
        assert!(history.contains("invalid-history"));
        assert!(!parent_snapshot.exists());
        assert!(!child_snapshot.exists());
        assert!(keep_snapshot.exists());

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn local_session_delete_reports_partial_database_cleanup() {
        let codex_dir = temp_codex_dir("hard-delete-partial-database");
        let id = "019f6000-0000-7000-8000-000000000020";
        let session_dir = codex_dir.join("sessions/2026/07/13");
        fs::create_dir_all(&session_dir).expect("create sessions directory");
        let rollout = session_dir.join(format!("rollout-test-{id}.jsonl"));
        write_text(&rollout, "session").expect("write rollout");
        let current = codex_dir.join("state_5.sqlite");
        seed_thread_database(&current, &[(id, &rollout)], None);

        let blocked = codex_dir.join("logs_3.sqlite");
        let conn = Connection::open(&blocked).expect("open blocked related database");
        conn.execute_batch(
            "CREATE TABLE logs (thread_id TEXT);
             INSERT INTO logs (thread_id) VALUES ('019f6000-0000-7000-8000-000000000020');
             CREATE TRIGGER block_log_delete BEFORE DELETE ON logs
             BEGIN SELECT RAISE(ABORT, 'blocked cleanup'); END;",
        )
        .expect("create blocked cleanup schema");
        drop(conn);

        let result = hard_delete_sessions_locally(&codex_dir, &[id.to_string()])
            .expect("return partial cleanup result");

        assert!(!rollout.exists());
        assert_eq!(sqlite_count(&current, "SELECT COUNT(*) FROM threads"), 0);
        assert_eq!(sqlite_count(&blocked, "SELECT COUNT(*) FROM logs"), 1);
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].contains("blocked cleanup"));

        let _ = fs::remove_dir_all(codex_dir);
    }

    #[test]
    fn local_session_delete_rejects_rollout_outside_codex_session_roots() {
        let codex_dir = temp_codex_dir("hard-delete-path-guard");
        let id = "019f6000-0000-7000-8000-000000000010";
        let outside_dir = temp_codex_dir("hard-delete-outside");
        let outside = outside_dir.join(format!("rollout-test-{id}.jsonl"));
        write_text(&outside, "outside").expect("write outside rollout");
        let current = codex_dir.join("state_5.sqlite");
        seed_thread_database(&current, &[(id, &outside)], None);

        let error = hard_delete_sessions_locally(&codex_dir, &[id.to_string()])
            .expect_err("reject external rollout path");
        assert!(error.to_string().contains("超出 Codex 会话目录"));
        assert!(outside.exists());
        assert_eq!(sqlite_count(&current, "SELECT COUNT(*) FROM threads"), 1);

        let _ = fs::remove_dir_all(codex_dir);
        let _ = fs::remove_dir_all(outside_dir);
    }

    #[cfg(unix)]
    #[test]
    fn local_session_delete_does_not_follow_rollout_directory_symlinks() {
        use std::os::unix::fs::symlink;

        let codex_dir = temp_codex_dir("hard-delete-symlink-guard");
        let id = "019f6000-0000-7000-8000-000000000011";
        let outside_dir = temp_codex_dir("hard-delete-symlink-outside");
        let outside = outside_dir.join(format!("rollout-test-{id}.jsonl"));
        write_text(&outside, "outside").expect("write outside rollout");

        let sessions_dir = codex_dir.join("sessions");
        fs::create_dir_all(&sessions_dir).expect("create sessions directory");
        symlink(&outside_dir, sessions_dir.join("external")).expect("create directory symlink");

        let missing_rollout = sessions_dir.join(format!("missing/rollout-test-{id}.jsonl"));
        let current = codex_dir.join("state_5.sqlite");
        seed_thread_database(&current, &[(id, &missing_rollout)], None);

        let result = hard_delete_sessions_locally(&codex_dir, &[id.to_string()])
            .expect("delete database row without following symlink");
        assert_eq!(result.deleted_thread_rows, 1);
        assert_eq!(result.deleted_rollout_files, 0);
        assert!(outside.exists());

        let _ = fs::remove_dir_all(codex_dir);
        let _ = fs::remove_dir_all(outside_dir);
    }
}
