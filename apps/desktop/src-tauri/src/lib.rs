use chrono::Local;
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
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
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderInput {
    config_dir: Option<String>,
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
    source_url: String,
    cached: bool,
    updated: bool,
    content_source: String,
    checked_at: Option<String>,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportResult {
    imported: usize,
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
    if let Ok(value) = std::env::var("CODEXX_HOME") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    Ok(home_dir()?.join(".codexx"))
}

fn db_path() -> Result<PathBuf> {
    Ok(app_home()?.join("codexx.db"))
}

fn open_db() -> Result<Connection> {
    let path = db_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    }
    let conn = Connection::open(&path).map_err(|e| CodexxError::Database(e.to_string()))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS providers (
            id TEXT PRIMARY KEY,
            provider_name TEXT NOT NULL,
            base_url TEXT NOT NULL,
            model TEXT NOT NULL,
            api_key TEXT,
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
    Ok(conn)
}

fn now_rfc3339() -> String {
    Local::now().to_rfc3339()
}

fn list_saved_providers_inner() -> Result<Vec<SavedProvider>> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare(
            "SELECT id, provider_name, base_url, model, api_key, wire_api, requires_openai_auth
             FROM providers
             ORDER BY created_at ASC, updated_at ASC",
        )
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SavedProvider {
                id: row.get(0)?,
                provider_name: row.get(1)?,
                base_url: row.get(2)?,
                model: row.get(3)?,
                api_key: row.get(4)?,
                wire_api: row.get(5)?,
                requires_openai_auth: row.get::<_, i64>(6)? != 0,
            })
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;

    let mut providers = Vec::new();
    for row in rows {
        providers.push(row.map_err(|e| CodexxError::Database(e.to_string()))?);
    }
    Ok(providers)
}

fn save_provider_inner(provider: SavedProvider) -> Result<SavedProvider> {
    let conn = open_db()?;
    let now = now_rfc3339();
    conn.execute(
        "INSERT INTO providers
            (id, provider_name, base_url, model, api_key, wire_api, requires_openai_auth, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
         ON CONFLICT(id) DO UPDATE SET
            provider_name = excluded.provider_name,
            base_url = excluded.base_url,
            model = excluded.model,
            api_key = excluded.api_key,
            wire_api = excluded.wire_api,
            requires_openai_auth = excluded.requires_openai_auth,
            updated_at = excluded.updated_at",
        params![
            provider.id,
            provider.provider_name,
            provider.base_url,
            provider.model,
            provider.api_key,
            provider.wire_api,
            if provider.requires_openai_auth { 1 } else { 0 },
            now,
        ],
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;

    // Re-read to return the normalized persisted row.
    let providers = list_saved_providers_inner()?;
    providers
        .into_iter()
        .find(|p| p.id == provider.id)
        .ok_or_else(|| CodexxError::Database("provider saved but not found".to_string()))
}

fn delete_provider_inner(id: &str) -> Result<()> {
    let conn = open_db()?;
    conn.execute("DELETE FROM providers WHERE id = ?1", params![id])
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    Ok(())
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
        let duplicate_index = prompts.iter().position(|existing: &SavedPrompt| {
            existing.filename.to_ascii_lowercase() == filename_key
                || (existing.content == prompt.content
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

fn find_saved_prompt_by_filename(filename: &str) -> Result<Option<SavedPrompt>> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare(
            "SELECT id, title, filename, content FROM prompts
             WHERE lower(filename) = lower(?1)
             ORDER BY CASE WHEN id LIKE 'external-%' THEN 1 ELSE 0 END, updated_at DESC, created_at DESC
             LIMIT 1",
        )
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    match stmt.query_row([filename], |row| {
        Ok(SavedPrompt {
            id: row.get(0)?,
            title: row.get(1)?,
            filename: row.get(2)?,
            content: row.get(3)?,
        })
    }) {
        Ok(prompt) => Ok(Some(prompt)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(CodexxError::Database(e.to_string())),
    }
}

fn find_saved_prompt_by_content(content: &str) -> Result<Option<SavedPrompt>> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare(
            "SELECT id, title, filename, content FROM prompts
             WHERE content = ?1
             ORDER BY CASE WHEN id LIKE 'external-%' THEN 1 ELSE 0 END, updated_at DESC, created_at DESC
             LIMIT 1",
        )
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    match stmt.query_row([content], |row| {
        Ok(SavedPrompt {
            id: row.get(0)?,
            title: row.get(1)?,
            filename: row.get(2)?,
            content: row.get(3)?,
        })
    }) {
        Ok(prompt) => Ok(Some(prompt)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(CodexxError::Database(e.to_string())),
    }
}

fn builtin_prompt_meta(
    template_id: &str,
) -> Result<(&'static str, &'static str, &'static str, &'static str)> {
    match template_id.trim() {
        "gpt5.4-unrestricted" => Ok((
            "gpt5.4-unrestricted",
            INSTRUCTION_54_FILENAME,
            INSTRUCTION_54_RELATIVE,
            INSTRUCTION_54_CONTENT,
        )),
        "gpt5.5-unrestricted" | "" => Ok((
            "gpt5.5-unrestricted",
            INSTRUCTION_FILENAME,
            INSTRUCTION_RELATIVE,
            INSTRUCTION_CONTENT,
        )),
        other => Err(CodexxError::Config(format!("未知指令提示词模板: {other}"))),
    }
}

fn builtin_prompt_source_url(filename: &str) -> String {
    format!("https://raw.githubusercontent.com/yynxxxxx/Codex-X/main/examples/{filename}")
}

fn cached_builtin_prompt(id: &str) -> Result<Option<(String, String)>> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare("SELECT content, checked_at FROM builtin_prompt_cache WHERE id = ?1")
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    match stmt.query_row([id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }) {
        Ok(value) => Ok(Some(value)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(CodexxError::Database(e.to_string())),
    }
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

fn refresh_builtin_prompt_inner(template_id: &str) -> Result<BuiltinPromptStatus> {
    let (id, filename, _relative, bundled) = builtin_prompt_meta(template_id)?;
    let source_url = builtin_prompt_source_url(filename);
    let cached_before = cached_builtin_prompt(id)?;
    match fetch_remote_prompt(&source_url) {
        Ok(remote) => {
            let updated = cached_before
                .as_ref()
                .map(|(content, _)| content != &remote)
                .unwrap_or(remote != bundled);
            save_builtin_prompt_cache(id, filename, &source_url, &remote)?;
            let checked_at = cached_builtin_prompt(id)?.map(|(_, checked_at)| checked_at);
            Ok(BuiltinPromptStatus {
                id: id.to_string(),
                filename: filename.to_string(),
                source_url,
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
                source_url,
                cached,
                updated: false,
                content_source: if cached { "cache" } else { "bundled" }.to_string(),
                checked_at: cached_before.map(|(_, checked_at)| checked_at),
                message: format!(
                    "无法连接 GitHub，已使用{}：{}",
                    if cached {
                        "本地缓存"
                    } else {
                        "打包内置版本"
                    },
                    e
                ),
            })
        }
    }
}

fn builtin_prompt_status_inner() -> Result<Vec<BuiltinPromptStatus>> {
    ["gpt5.5-unrestricted", "gpt5.4-unrestricted"]
        .iter()
        .map(|template_id| {
            let (id, filename, _relative, _bundled) = builtin_prompt_meta(template_id)?;
            let source_url = builtin_prompt_source_url(filename);
            let cached = cached_builtin_prompt(id)?;
            Ok(BuiltinPromptStatus {
                id: id.to_string(),
                filename: filename.to_string(),
                source_url,
                cached: cached.is_some(),
                updated: false,
                content_source: if cached.is_some() { "cache" } else { "bundled" }.to_string(),
                checked_at: cached.map(|(_, checked_at)| checked_at),
                message: "未检查 GitHub 更新".to_string(),
            })
        })
        .collect()
}

fn refresh_builtin_prompts_inner() -> Result<Vec<BuiltinPromptStatus>> {
    ["gpt5.5-unrestricted", "gpt5.4-unrestricted"]
        .iter()
        .map(|template_id| refresh_builtin_prompt_inner(template_id))
        .collect()
}

fn builtin_prompt_content(
    template_id: &str,
) -> Result<(&'static str, &'static str, String, String)> {
    let (id, filename, relative, bundled) = builtin_prompt_meta(template_id)?;
    let _ = refresh_builtin_prompt_inner(id);
    if let Some((content, _checked_at)) = cached_builtin_prompt(id)? {
        return Ok((filename, relative, content, "github/cache".to_string()));
    }
    Ok((
        filename,
        relative,
        bundled.to_string(),
        "bundled".to_string(),
    ))
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
    if is_managed_instruction_value(&current) {
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
    let existing = find_saved_prompt_by_content(&content)?.or_else(|| {
        find_saved_prompt_by_filename(&normalized_filename)
            .ok()
            .flatten()
    });
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

fn read_skill_metadata(skill_dir: &Path, fallback: &str) -> (String, Option<String>) {
    let skill_md = skill_dir.join("SKILL.md");
    let text = read_to_string_if_exists(&skill_md).unwrap_or_default();
    let mut title = None;
    let mut desc = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if title.is_none() && trimmed.starts_with('#') {
            title = Some(trimmed.trim_start_matches('#').trim().to_string());
            continue;
        }
        if desc.is_none() && !trimmed.starts_with('#') && !trimmed.starts_with("---") {
            desc = Some(trimmed.trim_matches('`').to_string());
            break;
        }
    }
    (
        title
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| fallback.to_string()),
        desc.filter(|s| !s.is_empty()),
    )
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

fn build_skills_mcp_state_inner(config_dir: Option<String>) -> Result<SkillsMcpState> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    let skills_dir = codex_skills_dir(&codex_dir);
    let disabled_dir = disabled_skills_dir()?;
    let mut warnings = Vec::new();
    let mut skills = Vec::new();
    let mut seen = HashSet::new();
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
    mcp_servers.sort_by(|a, b| b.enabled.cmp(&a.enabled).then_with(|| a.name.cmp(&b.name)));
    skills.sort_by(|a, b| b.enabled.cmp(&a.enabled).then_with(|| a.name.cmp(&b.name)));
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
            doc["mcp_servers"][&id] = json_to_toml_item(&config);
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
        doc["mcp_servers"][&id] = json_to_toml_item(&db.2);
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
        if disabled_path.exists() && !enabled_path.exists() {
            fs::rename(&disabled_path, &enabled_path)
                .or_else(|_| {
                    copy_dir_recursive(&disabled_path, &enabled_path).map(|_| {
                        let _ = fs::remove_dir_all(&disabled_path);
                    })
                })
                .map_err(|e| CodexxError::Config(format!("启用 Skill 失败: {e}")))?;
        }
    } else if enabled_path.exists() && !disabled_path.exists() {
        fs::rename(&enabled_path, &disabled_path)
            .or_else(|_| {
                copy_dir_recursive(&enabled_path, &disabled_path).map(|_| {
                    let _ = fs::remove_dir_all(&enabled_path);
                })
            })
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
    let mut total_size = 0u64;
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| CodexxError::Config(format!("读取 ZIP 条目失败: {e}")))?;
        let Some(path) = file.enclosed_name().map(|p| p.to_path_buf()) else {
            continue;
        };
        total_size += file.size();
        if total_size > 80 * 1024 * 1024 {
            return Err(CodexxError::Config("ZIP 解压后超过 80MB".to_string()));
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
        let dst_name = sanitize_dir_name(dir_name, "skill");
        let dst = skills_dir.join(dst_name);
        if dst.exists() {
            fs::remove_dir_all(&dst).map_err(|e| io_err(&dst, e))?;
        }
        copy_dir_recursive(&src, &dst)?;
        imported_skills += 1;
    }
    let _ = fs::remove_dir_all(&tmp);
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

fn extract_ccswitch_codex_provider(
    id: &str,
    name: &str,
    settings_config: &str,
) -> Option<SavedProvider> {
    let settings: Value = serde_json::from_str(settings_config).ok()?;
    let auth = settings.get("auth");
    let api_key = auth
        .and_then(|v| v.get("OPENAI_API_KEY"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);

    let config_text = settings.get("config").and_then(Value::as_str).unwrap_or("");
    if config_text.trim().is_empty() {
        return None;
    }
    let doc = config_text.parse::<DocumentMut>().ok()?;
    let model = string_value(&doc, "model").unwrap_or_else(|| "gpt-5.5".to_string());
    let active_provider =
        string_value(&doc, "model_provider").unwrap_or_else(|| "custom".to_string());

    let provider_table = doc
        .get("model_providers")
        .and_then(|item| item.as_table())
        .and_then(|providers| providers.get(&active_provider))
        .and_then(|item| item.as_table());

    let base_url = provider_table
        .and_then(|table| table.get("base_url"))
        .and_then(|item| item.as_str())
        .or_else(|| doc.get("base_url").and_then(|item| item.as_str()))
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .trim_end_matches('/')
        .to_string();

    let provider_name = provider_table
        .and_then(|table| table.get("name"))
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(name)
        .to_string();

    let wire_api = provider_table
        .and_then(|table| table.get("wire_api"))
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("responses")
        .to_string();

    let requires_openai_auth = provider_table
        .and_then(|table| table.get("requires_openai_auth"))
        .and_then(|item| item.as_bool())
        .unwrap_or(true);

    Some(SavedProvider {
        id: sanitize_id(id),
        provider_name,
        base_url,
        model,
        api_key,
        wire_api,
        requires_openai_auth,
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

    let mut stmt = conn
        .prepare("SELECT id, name, settings_config FROM providers WHERE app_type = 'codex' ORDER BY sort_index ASC, created_at ASC")
        .map_err(|e| CodexxError::Database(e.to_string()))?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;

    let mut imported = 0usize;
    let mut skipped = 0usize;
    let mut warnings = Vec::new();

    for row in rows {
        let (id, name, settings_config) = row.map_err(|e| CodexxError::Database(e.to_string()))?;
        match extract_ccswitch_codex_provider(&id, &name, &settings_config) {
            Some(provider) => {
                save_provider_inner(provider)?;
                imported += 1;
            }
            None => {
                skipped += 1;
                warnings.push(format!(
                    "跳过 {name} ({id})：未找到可用 config/base_url，可能是官方登录或空模板"
                ));
            }
        }
    }

    Ok(ImportResult {
        imported,
        skipped,
        warnings,
        providers: list_saved_providers_inner()?,
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
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    }
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        Local::now().format("%Y%m%d%H%M%S%3f")
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

fn backup_root() -> Result<PathBuf> {
    Ok(app_home()?.join("backups"))
}

fn create_backup(codex_dir: &Path, action: &str) -> Result<Option<String>> {
    let cfg = config_path(codex_dir);
    let auth = auth_path(codex_dir);
    let had_config = cfg.exists();
    let had_auth = auth.exists();

    if !had_config && !had_auth {
        return Ok(None);
    }

    let id = format!("{}-{}", Local::now().format("%Y%m%d-%H%M%S"), action);
    let dir = backup_root()?.join(&id);
    fs::create_dir_all(&dir).map_err(|e| io_err(&dir, e))?;

    if had_config {
        fs::copy(&cfg, dir.join("config.toml")).map_err(|e| io_err(&cfg, e))?;
    }
    if had_auth {
        fs::copy(&auth, dir.join("auth.json")).map_err(|e| io_err(&auth, e))?;
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

fn build_state(codex_dir: PathBuf) -> Result<CodexState> {
    let cfg = config_path(&codex_dir);
    let auth = auth_path(&codex_dir);
    let text = read_to_string_if_exists(&cfg)?;
    let doc = parse_toml_document(&cfg, &text)?;
    let model = string_value(&doc, "model");
    let model_provider = string_value(&doc, "model_provider");
    let instruction_file = string_value(&doc, "model_instructions_file");
    let instruction_enabled = instruction_file
        .as_deref()
        .is_some_and(is_managed_instruction_value);
    let providers = extract_providers(&doc, model_provider.as_deref());

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

fn sqlite_candidate_paths(codex_dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let sqlite_dir = codex_dir.join("sqlite");
    if let Ok(entries) = fs::read_dir(&sqlite_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let ext = path.extension().and_then(|v| v.to_str()).unwrap_or("");
            if matches!(ext, "db" | "sqlite" | "sqlite3") {
                paths.push(path);
            }
        }
    }
    paths.sort();
    let legacy = codex_dir.join("state_5.sqlite");
    if legacy.exists() && !paths.iter().any(|p| p == &legacy) {
        paths.push(legacy);
    }
    paths
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
        scan.sqlite_threads += total.max(0) as usize;
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
            "SELECT \"id\", {title_col}, {first_message_col}, {preview_col}, {provider_col}, {model_col}, {cwd_col}, {rollout_col}, {updated_ms_col}, {updated_col}, {archived_col}, {has_user_event_col} FROM threads ORDER BY {order_col} DESC LIMIT {}",
            limit.max(1)
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
            if seen.insert(session.id.clone()) {
                sessions.push(session);
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
    let session_limit = sqlite.sqlite_threads.max(50).min(1000);
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
        mismatched_threads: sqlite.mismatched_threads,
        needs_sync: rollouts.mismatched_session_meta > 0 || sqlite.mismatched_threads > 0,
        backup_dir: None,
        warnings,
        sessions,
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
    let lock_dir = codex_dir.join("tmp").join("provider-sync.lock");
    fs::create_dir_all(lock_dir.parent().unwrap_or(&codex_dir))
        .map_err(|e| io_err(lock_dir.parent().unwrap_or(&codex_dir), e))?;
    if lock_dir.exists() {
        return Err(CodexxError::Config(format!(
            "会话同步锁已存在: {}",
            lock_dir.display()
        )));
    }
    fs::create_dir_all(&lock_dir).map_err(|e| io_err(&lock_dir, e))?;

    let result = (|| -> Result<SessionSyncResult> {
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
    })();

    let _ = fs::remove_dir_all(&lock_dir);
    result
}

fn sync_sessions_provider_inner(
    config_dir: Option<String>,
    target_provider: Option<String>,
) -> Result<SessionSyncResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    fs::create_dir_all(&codex_dir).map_err(|e| io_err(&codex_dir, e))?;
    let target = current_model_provider(&codex_dir, target_provider)?;
    let lock_dir = codex_dir.join("tmp").join("provider-sync.lock");
    fs::create_dir_all(lock_dir.parent().unwrap_or(&codex_dir))
        .map_err(|e| io_err(lock_dir.parent().unwrap_or(&codex_dir), e))?;
    if lock_dir.exists() {
        return Err(CodexxError::Config(format!(
            "会话同步锁已存在: {}",
            lock_dir.display()
        )));
    }
    fs::create_dir_all(&lock_dir).map_err(|e| io_err(&lock_dir, e))?;

    let result = (|| -> Result<SessionSyncResult> {
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
    })();

    let _ = fs::remove_dir_all(&lock_dir);
    result
}

fn is_managed_instruction_value(value: &str) -> bool {
    [INSTRUCTION_FILENAME, INSTRUCTION_54_FILENAME]
        .iter()
        .any(|filename| {
            value == format!("./{filename}")
                || value == *filename
                || value.ends_with(&format!("/{filename}"))
                || value.ends_with(&format!("\\{filename}"))
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
async fn refresh_builtin_prompts() -> Result<Vec<BuiltinPromptStatus>> {
    tauri::async_runtime::spawn_blocking(refresh_builtin_prompts_inner)
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

fn enable_saved_prompt_inner(config_dir: Option<String>, id: String) -> Result<ActionResult> {
    let prompt = get_saved_prompt_inner(id.trim())?;
    let codex_dir = resolve_codex_dir(config_dir)?;
    let _ = remember_current_instruction_prompt(&codex_dir);
    fs::create_dir_all(&codex_dir).map_err(|e| io_err(&codex_dir, e))?;
    let cfg = config_path(&codex_dir);
    let backup_id = create_backup(&codex_dir, "enable-custom-prompt")?;

    let text = read_to_string_if_exists(&cfg)?;
    let mut doc = parse_toml_document(&cfg, &text)?;
    if doc.get("model").is_none() {
        doc["model"] = value("gpt-5.5");
    }
    doc["model_instructions_file"] = value(format!("./{}", prompt.filename));
    write_text(&codex_dir.join(&prompt.filename), &prompt.content)?;
    write_text(&cfg, &doc.to_string())?;

    let state = build_state(codex_dir)?;
    Ok(ActionResult {
        ok: true,
        message: format!("已启用 {}", prompt.title),
        backup_id,
        state,
    })
}

#[tauri::command]
async fn enable_saved_prompt(config_dir: Option<String>, id: String) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || enable_saved_prompt_inner(config_dir, id))
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
    let normalized = SavedProvider {
        id: provider.id.trim().to_string(),
        provider_name: provider.provider_name.trim().to_string(),
        base_url: provider.base_url.trim().trim_end_matches('/').to_string(),
        model: provider.model.trim().to_string(),
        api_key: provider
            .api_key
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        wire_api: if provider.wire_api.trim().is_empty() {
            "responses".to_string()
        } else {
            provider.wire_api.trim().to_string()
        },
        requires_openai_auth: provider.requires_openai_auth,
    };
    if normalized.id.is_empty() {
        return Err(CodexxError::Config("provider id 不能为空".to_string()));
    }
    if normalized.provider_name.is_empty() {
        return Err(CodexxError::Config("供应商名称不能为空".to_string()));
    }
    if normalized.base_url.is_empty() {
        return Err(CodexxError::Config("base_url 不能为空".to_string()));
    }
    if normalized.model.is_empty() {
        return Err(CodexxError::Config("model 不能为空".to_string()));
    }
    save_provider_inner(normalized)
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

    // 官方模式不应指向自定义路由。
    doc.as_table_mut().remove("model_provider");
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

#[tauri::command]
async fn switch_official_provider(config_dir: Option<String>) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        apply_official_config(
            config_dir,
            None,
            None,
            "switch-official",
            "已切换到 OpenAI Official",
        )
    })
    .await
    .map_err(|e| CodexxError::Config(format!("切换官方配置失败: {e}")))?
}

#[tauri::command]
async fn save_official_config(input: OfficialConfigInput) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        apply_official_config(
            input.config_dir,
            input.model,
            input.auth_json,
            "save-official",
            "已保存 OpenAI Official 配置",
        )
    })
    .await
    .map_err(|e| CodexxError::Config(format!("保存官方配置失败: {e}")))?
}

fn enable_instruction_inner(config_dir: Option<String>, template_id: &str) -> Result<ActionResult> {
    let (filename, relative, content, content_source) = builtin_prompt_content(template_id)?;
    let codex_dir = resolve_codex_dir(config_dir)?;
    let _ = remember_current_instruction_prompt(&codex_dir);
    fs::create_dir_all(&codex_dir).map_err(|e| io_err(&codex_dir, e))?;
    let cfg = config_path(&codex_dir);
    let backup_id = create_backup(&codex_dir, "enable-instruct")?;

    let text = read_to_string_if_exists(&cfg)?;
    let mut doc = parse_toml_document(&cfg, &text)?;
    if doc.get("model").is_none() {
        doc["model"] = value("gpt-5.5");
    }
    doc["model_instructions_file"] = value(relative);

    write_text(&codex_dir.join(filename), &content)?;
    write_text(&cfg, &doc.to_string())?;

    let state = build_state(codex_dir)?;
    Ok(ActionResult {
        ok: true,
        message: format!("已启用 {filename}（来源：{content_source}）"),
        backup_id,
        state,
    })
}

#[tauri::command]
async fn enable_instruction(config_dir: Option<String>) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        enable_instruction_inner(config_dir, "gpt5.5-unrestricted")
    })
    .await
    .map_err(|e| CodexxError::Config(format!("启用指令提示词失败: {e}")))?
}

#[tauri::command]
async fn enable_instruction_template(
    config_dir: Option<String>,
    template_id: String,
) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || enable_instruction_inner(config_dir, &template_id))
        .await
        .map_err(|e| CodexxError::Config(format!("启用指令提示词失败: {e}")))?
}

fn disable_instruction_inner(
    config_dir: Option<String>,
    delete_file: Option<bool>,
) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    let cfg = config_path(&codex_dir);
    let backup_id = create_backup(&codex_dir, "disable-instruct")?;

    let text = read_to_string_if_exists(&cfg)?;
    let mut doc = parse_toml_document(&cfg, &text)?;
    let current = string_value(&doc, "model_instructions_file");
    let removed = current.is_some();
    if removed {
        doc.as_table_mut().remove("model_instructions_file");
    }

    write_text(&cfg, &doc.to_string())?;
    if delete_file.unwrap_or(true) {
        for filename in [INSTRUCTION_FILENAME, INSTRUCTION_54_FILENAME] {
            let md = codex_dir.join(filename);
            if md.exists() {
                fs::remove_file(&md).map_err(|e| io_err(&md, e))?;
            }
        }
    }

    let state = build_state(codex_dir)?;
    Ok(ActionResult {
        ok: true,
        message: if removed {
            "已禁用指令提示词".to_string()
        } else {
            "当前未设置 model_instructions_file".to_string()
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

fn save_provider_toml_config_inner(input: ProviderTomlInput) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(input.config_dir.clone())?;
    fs::create_dir_all(&codex_dir).map_err(|e| io_err(&codex_dir, e))?;
    let cfg = config_path(&codex_dir);
    let auth = auth_path(&codex_dir);
    let backup_id = create_backup(&codex_dir, "save-provider-toml")?;

    let config_text = input.config_text.trim_end().to_string();
    let doc = parse_toml_document(&cfg, &config_text)?;
    if string_value(&doc, "model").is_none() {
        return Err(CodexxError::Config(
            "config.toml 必须包含 model".to_string(),
        ));
    }
    write_text(&cfg, &(config_text + "\n"))?;

    if let Some(api_key) = input
        .api_key
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
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

#[tauri::command]
async fn save_provider_toml_config(input: ProviderTomlInput) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || save_provider_toml_config_inner(input))
        .await
        .map_err(|e| CodexxError::Config(format!("保存供应商 TOML 失败: {e}")))?
}

fn switch_provider_inner(input: ProviderInput) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(input.config_dir.clone())?;
    fs::create_dir_all(&codex_dir).map_err(|e| io_err(&codex_dir, e))?;
    let cfg = config_path(&codex_dir);
    let auth = auth_path(&codex_dir);
    let backup_id = create_backup(&codex_dir, "switch-provider")?;

    let provider_name = input.provider_name.trim();
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
    doc["model_provider"] = value("custom");
    doc["model"] = value(model);
    set_top_level_defaults(&mut doc);

    let root = doc.as_table_mut();
    let providers = ensure_table(root, "model_providers")?;
    let custom = ensure_table(providers, "custom")?;
    custom["name"] = value(provider_name);
    custom["base_url"] = value(base_url);
    custom["wire_api"] = value(input.wire_api.unwrap_or_else(|| "responses".to_string()));
    custom["requires_openai_auth"] = value(input.requires_openai_auth.unwrap_or(true));

    write_text(&cfg, &doc.to_string())?;

    if let Some(api_key) = input
        .api_key
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
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
    let dir = backup_root()?.join(&backup_id);
    if !dir.exists() {
        return Err(CodexxError::Config(format!("备份不存在: {backup_id}")));
    }

    let restore_marker = create_backup(&codex_dir, "before-restore")?;
    let cfg = config_path(&codex_dir);
    let auth = auth_path(&codex_dir);
    fs::create_dir_all(&codex_dir).map_err(|e| io_err(&codex_dir, e))?;

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
            import_existing_skills_mcp,
            toggle_codex_skill,
            toggle_codex_mcp,
            install_skill_zip,
            check_skill_updates,
            get_startup_diagnostics,
            get_session_sync_status,
            sync_sessions_provider,
            sync_selected_sessions_provider,
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
            switch_provider,
            save_provider_toml_config,
            list_backups,
            restore_backup,
            open_url,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Codex-X");
}
