use chrono::Local;
#[cfg(test)]
use rusqlite::params;
use rusqlite::Connection;
use serde::Serialize;
#[cfg(test)]
use serde_json::{json, Value};
#[cfg(test)]
use std::collections::HashMap;
#[cfg(any(test, target_os = "windows"))]
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod app_db;
mod backups;
mod ccswitch;
mod config_migration;
mod constants;
mod error;
mod file_io;
mod paths;
mod platform;
mod prompts;
mod providers;
mod remote;
mod sessions;
mod skills_mcp;
mod skins;
mod sqlite_utils;
mod state;
mod toml_utils;
mod updates;

use backups::{action_backup_root, backups, create_backup, BackupEntry, BackupMeta};
use constants::*;
use error::{CodexxError, Result};
#[cfg(test)]
use file_io::write_json;
use file_io::{
    atomic_write, directory_exists, ensure_directory, io_err, parse_toml_document,
    read_to_string_if_exists, write_text,
};
#[cfg(test)]
use paths::app_home;
use paths::home_dir;
use prompts::{
    agents_path, builtin_prompt_content, builtin_prompt_status_inner, bundled_prompt_meta,
    delete_prompt_inner, get_saved_prompt_inner, install_managed_agents_block,
    list_saved_prompts_inner, managed_agents_bounds, normalize_prompt_filename,
    prompt_template_key_for_instruction, refresh_builtin_prompts_with_active,
    remember_current_instruction_prompt, resolve_instruction_path, save_prompt_inner,
    uninstall_managed_agents_block, BuiltinPromptStatus, SavedPrompt,
};
#[cfg(test)]
use prompts::{
    bundled_prompt_metas, cached_prompt_fallback_statuses, delete_cached_prompt_ids,
    github_prompt_catalog_from_entries, jsdelivr_prompt_catalog_from_entries,
    managed_agents_template_key_from_content, prompt_content_source_urls, stable_remote_prompt_id,
    stale_cached_prompt_ids, CachedBuiltinPrompt, GithubContentEntry,
};
#[cfg(test)]
use providers::{
    build_ccswitch_codex_provider, canonical_provider_base_url, codex_sections_from_config,
    detected_live_custom_provider, is_official_ccswitch_row, list_saved_providers_on_connection,
    merge_duplicate_provider_identities, normalize_saved_provider, provider_by_id_on_connection,
    provider_identity, provider_status_result, read_ccswitch_codex_rows,
    save_manual_provider_on_connection, save_provider_toml_config_with_pre_persist,
    switch_official_provider_with_pre_persist, switch_provider_with_pre_persist,
    upsert_provider_on_connection, CcSwitchCodexRow, ProviderUpsertKind, ProviderUpsertMode,
};
use providers::{
    delete_provider_inner, fetch_provider_models_inner, import_ccswitch_codex_providers_inner,
    list_saved_providers_inner, read_ccswitch_official_auth_inner, save_official_config_inner,
    save_provider_inner, save_provider_toml_config_inner, switch_official_provider_inner,
    switch_provider_inner, test_provider_connection_inner, ImportResult, OfficialAuthCandidate,
    OfficialConfigInput, ProviderConnectionResult, ProviderInput, ProviderModelsResult,
    ProviderTomlInput, SavedProvider,
};
#[cfg(test)]
use sessions::{
    active_session_ids_present, apply_session_changes, backup_sqlite_to_backup,
    hard_delete_sessions_locally, list_session_previews, provider_sync_backup_root,
    prune_provider_sync_backups, restore_session_changes, scan_rollouts, scan_sqlite,
    sqlite_session_db_paths,
};
use sessions::{
    delete_codex_sessions_inner, session_sync_status_inner, sqlite_candidate_paths,
    sync_sessions_provider_inner, SessionDeleteInput, SessionDeleteResult, SessionSyncResult,
    SessionSyncStatus,
};
use skills_mcp::{
    build_skills_mcp_state_inner, check_skill_updates_inner, import_existing_skills_mcp_inner,
    install_skill_zip_inner, preview_existing_skills_mcp_inner, toggle_codex_mcp_inner,
    toggle_codex_skill_inner, SkillsMcpActionResult, SkillsMcpImportPreview, SkillsMcpState,
};
#[cfg(test)]
use skills_mcp::{
    normalize_legacy_zip_skill_dirs, read_skill_metadata, sort_managed_mcp_servers,
    sort_managed_skills, ManagedMcpServer, ManagedSkill,
};
use skins::{
    enable_skin_theme_inner, export_skin_theme_inner, get_skin_center_state_inner,
    import_skin_theme_zip_inner, SkinActionResult, SkinCenterState, SkinExportResult,
};
#[cfg(test)]
use state::active_saved_provider_id_from_config;
use state::{auth_has_material, build_state, ActionResult, CodexState};
use toml_edit::{value, DocumentMut};
pub(crate) use toml_utils::string_value;
use updates::check_app_update;

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
struct AboutInfo {
    app_version: String,
    codex_version: Option<String>,
    codex_dir: String,
    project_url: String,
    github_repo: String,
    native_updater_supported: bool,
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

fn open_db() -> Result<Connection> {
    providers::open_store()
}

pub(crate) fn now_rfc3339() -> String {
    Local::now().to_rfc3339()
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
    refresh_builtin_prompts_with_active(|| active_remote_builtin_prompt_id(config_dir))
}
pub(crate) fn sanitize_id(input: &str) -> String {
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

fn default_codex_dir() -> Result<PathBuf> {
    if let Ok(value) = std::env::var("CODEX_HOME") {
        if let Some(path) = codex_dir_from_text(&value)? {
            return Ok(path);
        }
    }
    Ok(home_dir()?.join(".codex"))
}

fn codex_dir_from_text(value: &str) -> Result<Option<PathBuf>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let unquoted = if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    if unquoted.trim().is_empty() {
        return Ok(None);
    }
    if unquoted == "~" {
        return Ok(Some(home_dir()?));
    }
    if let Some(rest) = unquoted
        .strip_prefix("~/")
        .or_else(|| unquoted.strip_prefix("~\\"))
    {
        return Ok(Some(home_dir()?.join(rest)));
    }
    Ok(Some(PathBuf::from(unquoted)))
}

#[cfg(target_os = "windows")]
fn resolve_windows_linked_directory(path: PathBuf) -> Result<PathBuf> {
    use std::os::windows::fs::FileTypeExt;

    let original = path.clone();
    let mut current = path;
    let mut followed_link = false;
    let mut visited = HashSet::new();
    for _ in 0..16 {
        if !visited.insert(current.clone()) {
            return Err(CodexxError::Config(format!(
                "当前 Codex 目录链接形成了循环：{}",
                original.display()
            )));
        }
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !followed_link => {
                return Ok(current);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CodexxError::Config(format!(
                    "当前 Codex 目录链接的目标不存在：{}",
                    original.display()
                )));
            }
            Err(error) => return Err(io_err(&current, error)),
        };
        let file_type = metadata.file_type();
        if metadata.is_dir() && !file_type.is_symlink_dir() {
            return Ok(current);
        }
        if file_type.is_symlink_file() || file_type.is_symlink_dir() || file_type.is_symlink() {
            let target = fs::read_link(&current).map_err(|error| io_err(&current, error))?;
            current = if target.is_absolute() {
                target
            } else {
                current
                    .parent()
                    .map(|parent| parent.join(&target))
                    .unwrap_or(target)
            };
            followed_link = true;
            continue;
        }
        return Err(CodexxError::Config(format!(
            "当前 CODEX_HOME 不是文件夹：{}",
            original.display()
        )));
    }

    Err(CodexxError::Config(format!(
        "当前 Codex 目录链接层级过多：{}",
        original.display()
    )))
}

#[cfg(not(target_os = "windows"))]
fn resolve_windows_linked_directory(path: PathBuf) -> Result<PathBuf> {
    Ok(path)
}

pub(crate) fn resolve_codex_dir(config_dir: Option<String>) -> Result<PathBuf> {
    let path = match config_dir.as_deref().map(codex_dir_from_text).transpose()? {
        Some(Some(path)) => Ok(path),
        _ => default_codex_dir(),
    }?;
    resolve_windows_linked_directory(path)
}

pub(crate) fn config_path(codex_dir: &Path) -> PathBuf {
    codex_dir.join("config.toml")
}

pub(crate) fn auth_path(codex_dir: &Path) -> PathBuf {
    codex_dir.join("auth.json")
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
    let codex_dir_ok = directory_exists(&codex_dir);
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
async fn get_skin_center_state() -> Result<SkinCenterState> {
    tauri::async_runtime::spawn_blocking(get_skin_center_state_inner)
        .await
        .map_err(|e| CodexxError::Config(format!("读取皮肤中心失败: {e}")))?
}

#[tauri::command]
async fn enable_skin_theme(id: String) -> Result<SkinActionResult> {
    tauri::async_runtime::spawn_blocking(move || enable_skin_theme_inner(id))
        .await
        .map_err(|e| CodexxError::Config(format!("启用皮肤失败: {e}")))?
}

#[tauri::command]
async fn import_skin_theme_zip(file_name: String, bytes: Vec<u8>) -> Result<SkinActionResult> {
    tauri::async_runtime::spawn_blocking(move || import_skin_theme_zip_inner(file_name, bytes))
        .await
        .map_err(|e| CodexxError::Config(format!("导入皮肤失败: {e}")))?
}

#[tauri::command]
async fn export_skin_theme(id: String) -> Result<SkinExportResult> {
    tauri::async_runtime::spawn_blocking(move || export_skin_theme_inner(id))
        .await
        .map_err(|e| CodexxError::Config(format!("导出皮肤失败: {e}")))?
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
    #[cfg(target_os = "windows")]
    let native_updater_supported = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("Codex-X.portable")))
        .map(|marker| !marker.is_file())
        .unwrap_or(true);
    #[cfg(target_os = "linux")]
    let native_updater_supported = std::env::var_os("APPIMAGE")
        .map(std::path::PathBuf::from)
        .is_some_and(|path| path.is_file());
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    let native_updater_supported = true;
    Ok(AboutInfo {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        codex_version: platform::detect_codex_version(),
        codex_dir: codex_dir.display().to_string(),
        project_url: "https://github.com/yynxxxxx/Codex-X".to_string(),
        github_repo: "yynxxxxx/Codex-X".to_string(),
        native_updater_supported,
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

#[allow(clippy::too_many_arguments)]
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
    ensure_directory(&codex_dir)?;
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

#[tauri::command]
async fn save_provider_toml_config(input: ProviderTomlInput) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || save_provider_toml_config_inner(input))
        .await
        .map_err(|e| CodexxError::Config(format!("保存供应商 TOML 失败: {e}")))?
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

#[tauri::command]
async fn fetch_provider_models(
    base_url: String,
    api_key: Option<String>,
) -> Result<ProviderModelsResult> {
    tauri::async_runtime::spawn_blocking(move || fetch_provider_models_inner(base_url, api_key))
        .await
        .map_err(|e| CodexxError::Config(format!("获取模型列表失败: {e}")))?
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
    ensure_directory(&codex_dir)?;

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
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            get_about_info,
            check_app_update,
            get_skills_mcp_state,
            preview_existing_skills_mcp,
            import_existing_skills_mcp,
            toggle_codex_skill,
            toggle_codex_mcp,
            install_skill_zip,
            get_skin_center_state,
            enable_skin_theme,
            import_skin_theme_zip,
            export_skin_theme,
            check_skill_updates,
            get_startup_diagnostics,
            get_session_sync_status,
            sync_sessions_provider,
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
            fetch_provider_models,
            list_backups,
            restore_backup,
            open_url,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Codex-X");
}

#[cfg(test)]
mod tests;
