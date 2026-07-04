use chrono::Local;
use serde::{Deserialize, Serialize};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;
use toml_edit::{value, DocumentMut, Item, Table};

const INSTRUCTION_FILENAME: &str = "gpt5.5-unrestricted.md";
const INSTRUCTION_RELATIVE: &str = "./gpt5.5-unrestricted.md";
const INSTRUCTION_CONTENT: &str = include_str!("../../../../examples/gpt5.5-unrestricted.md");
const INSTRUCTION_54_FILENAME: &str = "gpt5.4-unrestricted.md";
const INSTRUCTION_54_RELATIVE: &str = "./gpt5.4-unrestricted.md";
const INSTRUCTION_54_CONTENT: &str = include_str!("../../../../examples/gpt5.4-unrestricted.md");

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
        CREATE INDEX IF NOT EXISTS idx_prompts_updated_at ON prompts(updated_at DESC);",
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
        prompts.push(row.map_err(|e| CodexxError::Database(e.to_string()))?);
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
        params![prompt.id, prompt.title, prompt.filename, prompt.content, now],
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

fn extract_ccswitch_codex_provider(id: &str, name: &str, settings_config: &str) -> Option<SavedProvider> {
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
    let active_provider = string_value(&doc, "model_provider").unwrap_or_else(|| "custom".to_string());

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
    let Some(path) = candidate else { return; };
    if !candidates.iter().any(|item| item == &path) {
        candidates.push(path);
    }
}

fn ccswitch_db_candidates() -> Result<Vec<PathBuf>> {
    let mut candidates = Vec::new();

    if let Ok(value) = std::env::var("CC_SWITCH_HOME") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            push_existing_candidate(&mut candidates, Some(PathBuf::from(trimmed).join("cc-switch.db")));
        }
    }

    let home = home_dir()?;
    // cc-switch 当前主要使用这个位置，macOS/Windows/Linux 都适用。
    push_existing_candidate(&mut candidates, Some(home.join(".cc-switch").join("cc-switch.db")));

    // 兼容 Tauri/AppData 风格位置，防止未来或不同发行版变更数据目录。
    if let Some(data_dir) = dirs::data_dir() {
        push_existing_candidate(&mut candidates, Some(data_dir.join("com.ccswitch.desktop").join("cc-switch.db")));
        push_existing_candidate(&mut candidates, Some(data_dir.join("cc-switch").join("cc-switch.db")));
        push_existing_candidate(&mut candidates, Some(data_dir.join("CC Switch").join("cc-switch.db")));
    }
    if let Some(data_local_dir) = dirs::data_local_dir() {
        push_existing_candidate(&mut candidates, Some(data_local_dir.join("com.ccswitch.desktop").join("cc-switch.db")));
        push_existing_candidate(&mut candidates, Some(data_local_dir.join("cc-switch").join("cc-switch.db")));
        push_existing_candidate(&mut candidates, Some(data_local_dir.join("CC Switch").join("cc-switch.db")));
    }

    #[cfg(target_os = "macos")]
    {
        push_existing_candidate(
            &mut candidates,
            Some(home.join("Library").join("Application Support").join("com.ccswitch.desktop").join("cc-switch.db")),
        );
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            push_existing_candidate(&mut candidates, Some(PathBuf::from(appdata).join("com.ccswitch.desktop").join("cc-switch.db")));
        }
        if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
            push_existing_candidate(&mut candidates, Some(PathBuf::from(localappdata).join("com.ccswitch.desktop").join("cc-switch.db")));
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg_data_home) = std::env::var("XDG_DATA_HOME") {
            push_existing_candidate(&mut candidates, Some(PathBuf::from(xdg_data_home).join("com.ccswitch.desktop").join("cc-switch.db")));
        }
        push_existing_candidate(&mut candidates, Some(home.join(".local").join("share").join("com.ccswitch.desktop").join("cc-switch.db")));
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
            db.display(), candidates
        )));
    }

    let conn = Connection::open_with_flags(
        &db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| CodexxError::Database(format!("打开 cc-switch 数据库失败 {}: {e}", db.display())))?;

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
                warnings.push(format!("跳过 {name} ({id})：未找到可用 config/base_url，可能是官方登录或空模板"));
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
    match config_dir.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
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



fn read_ccswitch_official_auth_inner(path: Option<String>) -> Result<Option<OfficialAuthCandidate>> {
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
    .map_err(|e| CodexxError::Database(format!("打开 cc-switch 数据库失败 {}: {e}", db.display())))?;

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

    let Some(row) = rows.next().map_err(|e| CodexxError::Database(e.to_string()))? else {
        return Ok(None);
    };

    let id: String = row.get(0).map_err(|e| CodexxError::Database(e.to_string()))?;
    let name: String = row.get(1).map_err(|e| CodexxError::Database(e.to_string()))?;
    let settings_config: String = row.get(2).map_err(|e| CodexxError::Database(e.to_string()))?;
    let settings: Value = serde_json::from_str(&settings_config)
        .map_err(|e| CodexxError::Database(format!("cc-switch official settings JSON 解析失败: {e}")))?;

    let auth = settings
        .get("auth")
        .cloned()
        .filter(|value| value.is_object())
        .ok_or_else(|| CodexxError::Database("cc-switch official provider 缺少 auth object".to_string()))?;

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
    write_json(&dir.join("meta.json"), &serde_json::to_value(meta).expect("meta serialize"))?;
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
    let Some(obj) = value.as_object() else { return Ok(false); };
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
    let instruction_enabled = instruction_file.as_deref().is_some_and(is_managed_instruction_value);
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


fn instruction_template(template_id: &str) -> Result<(&'static str, &'static str, &'static str)> {
    match template_id.trim() {
        "gpt5.4-unrestricted" => Ok((INSTRUCTION_54_FILENAME, INSTRUCTION_54_RELATIVE, INSTRUCTION_54_CONTENT)),
        "gpt5.5-unrestricted" | "" => Ok((INSTRUCTION_FILENAME, INSTRUCTION_RELATIVE, INSTRUCTION_CONTENT)),
        other => Err(CodexxError::Config(format!("未知指令提示词模板: {other}"))),
    }
}

fn is_managed_instruction_value(value: &str) -> bool {
    [INSTRUCTION_FILENAME, INSTRUCTION_54_FILENAME].iter().any(|filename| {
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
fn read_ccswitch_official_auth(db_path: Option<String>) -> Result<Option<OfficialAuthCandidate>> {
    read_ccswitch_official_auth_inner(db_path)
}

#[tauri::command]
fn import_ccswitch_codex_providers(db_path: Option<String>) -> Result<ImportResult> {
    import_ccswitch_codex_providers_inner(db_path)
}

fn command_version(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

#[cfg(target_os = "macos")]
fn macos_codex_app_version() -> Option<String> {
    let output = Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :CFBundleShortVersionString", "/Applications/Codex.app/Contents/Info.plist"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

#[cfg(not(target_os = "macos"))]
fn macos_codex_app_version() -> Option<String> { None }

fn detect_codex_version() -> Option<String> {
    command_version("codex", &["--version"])
        .or_else(|| command_version("codex", &["-V"]))
        .or_else(macos_codex_app_version)
}

#[tauri::command]
fn get_about_info(config_dir: Option<String>) -> Result<AboutInfo> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    Ok(AboutInfo {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        codex_version: detect_codex_version(),
        codex_dir: codex_dir.display().to_string(),
        project_url: "https://github.com/yynxxxxx/Codex-X".to_string(),
        github_repo: "yynxxxxx/Codex-X".to_string(),
    })
}

#[tauri::command]
fn list_saved_prompts() -> Result<Vec<SavedPrompt>> {
    list_saved_prompts_inner()
}

#[tauri::command]
fn save_prompt(prompt: SavedPrompt) -> Result<SavedPrompt> {
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
    save_prompt_inner(SavedPrompt { id, title, filename, content })
}

#[tauri::command]
fn delete_saved_prompt(id: String) -> Result<()> {
    delete_prompt_inner(id.trim())
}

#[tauri::command]
fn enable_saved_prompt(config_dir: Option<String>, id: String) -> Result<ActionResult> {
    let prompt = get_saved_prompt_inner(id.trim())?;
    let codex_dir = resolve_codex_dir(config_dir)?;
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
fn list_saved_providers() -> Result<Vec<SavedProvider>> {
    list_saved_providers_inner()
}

#[tauri::command]
fn save_provider(provider: SavedProvider) -> Result<SavedProvider> {
    let normalized = SavedProvider {
        id: provider.id.trim().to_string(),
        provider_name: provider.provider_name.trim().to_string(),
        base_url: provider.base_url.trim().trim_end_matches('/').to_string(),
        model: provider.model.trim().to_string(),
        api_key: provider.api_key.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
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
fn delete_saved_provider(id: String) -> Result<()> {
    delete_provider_inner(id.trim())
}

#[tauri::command]
fn get_codex_state(config_dir: Option<String>) -> Result<CodexState> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    build_state(codex_dir)
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

    if let Some(model) = model.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        doc["model"] = value(model);
    }

    write_text(&cfg, &doc.to_string())?;

    if let Some(auth_json) = auth_json.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        let parsed: Value = serde_json::from_str(&auth_json).map_err(|e| json_err(&auth, e))?;
        if !parsed.is_object() {
            return Err(CodexxError::Config("auth.json 必须是 JSON object".to_string()));
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
fn switch_official_provider(config_dir: Option<String>) -> Result<ActionResult> {
    apply_official_config(
        config_dir,
        None,
        None,
        "switch-official",
        "已切换到 OpenAI Official",
    )
}

#[tauri::command]
fn save_official_config(input: OfficialConfigInput) -> Result<ActionResult> {
    apply_official_config(
        input.config_dir,
        input.model,
        input.auth_json,
        "save-official",
        "已保存 OpenAI Official 配置",
    )
}

fn enable_instruction_inner(config_dir: Option<String>, template_id: &str) -> Result<ActionResult> {
    let (filename, relative, content) = instruction_template(template_id)?;
    let codex_dir = resolve_codex_dir(config_dir)?;
    fs::create_dir_all(&codex_dir).map_err(|e| io_err(&codex_dir, e))?;
    let cfg = config_path(&codex_dir);
    let backup_id = create_backup(&codex_dir, "enable-instruct")?;

    let text = read_to_string_if_exists(&cfg)?;
    let mut doc = parse_toml_document(&cfg, &text)?;
    if doc.get("model").is_none() {
        doc["model"] = value("gpt-5.5");
    }
    doc["model_instructions_file"] = value(relative);

    write_text(&codex_dir.join(filename), content)?;
    write_text(&cfg, &doc.to_string())?;

    let state = build_state(codex_dir)?;
    Ok(ActionResult {
        ok: true,
        message: format!("已启用 {filename}"),
        backup_id,
        state,
    })
}

#[tauri::command]
fn enable_instruction(config_dir: Option<String>) -> Result<ActionResult> {
    enable_instruction_inner(config_dir, "gpt5.5-unrestricted")
}

#[tauri::command]
fn enable_instruction_template(config_dir: Option<String>, template_id: String) -> Result<ActionResult> {
    enable_instruction_inner(config_dir, &template_id)
}

#[tauri::command]
fn disable_instruction(config_dir: Option<String>, delete_file: Option<bool>) -> Result<ActionResult> {
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
fn save_provider_toml_config(input: ProviderTomlInput) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(input.config_dir.clone())?;
    fs::create_dir_all(&codex_dir).map_err(|e| io_err(&codex_dir, e))?;
    let cfg = config_path(&codex_dir);
    let auth = auth_path(&codex_dir);
    let backup_id = create_backup(&codex_dir, "save-provider-toml")?;

    let config_text = input.config_text.trim_end().to_string();
    let doc = parse_toml_document(&cfg, &config_text)?;
    if string_value(&doc, "model").is_none() {
        return Err(CodexxError::Config("config.toml 必须包含 model".to_string()));
    }
    write_text(&cfg, &(config_text + "\n"))?;

    if let Some(api_key) = input.api_key.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
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
fn switch_provider(input: ProviderInput) -> Result<ActionResult> {
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

    if let Some(api_key) = input.api_key.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
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
fn list_backups() -> Result<Vec<BackupEntry>> {
    backups()
}

#[tauri::command]
fn restore_backup(config_dir: Option<String>, backup_id: String) -> Result<ActionResult> {
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

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_about_info,
            read_ccswitch_official_auth,
            import_ccswitch_codex_providers,
            list_saved_prompts,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Codex-X");
}
