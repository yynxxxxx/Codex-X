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
mod app_update;
mod app_update_channel;
mod backups;
mod ccswitch;
mod codex_language;
mod config_health;
mod config_migration;
mod constants;
mod context_config;
mod desktop_lifecycle;
mod error;
mod failover;
mod file_io;
mod live_config;
mod official_model_draft;
mod official_models;
mod paths;
mod platform;
mod prompts;
mod provider_balance;
mod providers;
mod remote;
mod sessions;
mod skills_mcp;
mod sqlite_utils;
mod state;
mod toml_utils;
mod transfers;
mod updates;
mod usage;

use backups::{
    action_backup_root, backups, create_backup, validate_backup_codex_dir, BackupEntry, BackupMeta,
};
use config_migration::{migrate_legacy_prompt_config_locked, migrated_legacy_prompt_config_text};
use constants::*;
use error::{CodexxError, Result};
#[cfg(target_os = "windows")]
use file_io::io_err;
use file_io::{directory_exists, ensure_directory, parse_toml_document};
#[cfg(test)]
use file_io::{write_json, write_text};
use live_config::{
    acquire_live_config_lock, apply_file_change, fail_with_file_rollback, read_file_snapshot,
    text_from_snapshot, AppliedFileChange,
};
#[cfg(test)]
use paths::app_home;
use paths::home_dir;
use prompts::{
    agents_path, builtin_prompt_content, builtin_prompt_detail_inner, builtin_prompt_status_inner,
    bundled_prompt_meta, delete_prompt_inner, get_saved_prompt_inner,
    install_managed_agents_block_in_content, list_saved_prompts_inner, managed_agents_bounds,
    normalize_prompt_filename, prompt_template_key_for_instruction,
    refresh_builtin_prompts_with_active, remember_current_instruction_prompt,
    remove_managed_agents_block_from_content, resolve_instruction_path,
    save_builtin_prompt_override_inner, save_prompt_inner, BuiltinPromptDetail,
    BuiltinPromptStatus, SavedPrompt,
};
#[cfg(test)]
use prompts::{
    bundled_prompt_metas, cached_prompt_fallback_statuses, delete_cached_prompt_ids,
    github_prompt_catalog_from_entries, install_managed_agents_block,
    jsdelivr_prompt_catalog_from_entries, managed_agents_template_key_from_content,
    prompt_content_source_urls, stable_remote_prompt_id, stale_cached_prompt_ids,
    uninstall_managed_agents_block, CachedBuiltinPrompt, GithubContentEntry,
};
use providers::official_profiles::{
    delete_official_profile_inner, duplicate_official_profile_inner, get_official_profile_inner,
    list_official_profiles_inner, save_official_profile_inner, switch_official_profile_inner,
    OfficialProfileActionResult, OfficialProfileDetail, OfficialProfileInput,
    OfficialProfileSummary,
};
#[cfg(test)]
use providers::{
    build_ccswitch_codex_provider, canonical_provider_base_url, codex_sections_from_config,
    consolidate_legacy_provider_duplicates_on_connection, detected_live_custom_provider,
    get_official_config_draft_inner, is_official_ccswitch_row, list_saved_providers_on_connection,
    normalize_saved_provider, official_snapshot_path_for_test, provider_by_id_on_connection,
    provider_identity, provider_status_result, read_ccswitch_codex_rows,
    reset_official_provider_inner, restore_official_provider_inner,
    save_manual_provider_on_connection, save_provider_toml_config_with_pre_persist,
    switch_official_provider_inner, switch_official_provider_with_pre_persist,
    switch_provider_with_pre_persist, upsert_ccswitch_provider_on_connection,
    upsert_provider_on_connection, CcSwitchCodexRow, ProviderUpsertKind, ProviderUpsertMode,
};
use providers::{
    build_provider_toml_draft_inner, build_provider_toml_draft_with_origin_inner,
    clear_active_provider_on_connection, delete_saved_provider_inner, fetch_provider_models_inner,
    import_ccswitch_codex_providers_inner, list_saved_providers_inner, open_store,
    read_ccswitch_official_auth_inner, remember_active_provider_on_connection,
    save_active_provider_inner, save_official_config_inner, save_provider_inner,
    save_provider_toml_config_inner, switch_provider_inner, test_provider_connection_inner,
    ImportResult, OfficialAuthCandidate, OfficialConfigInput, ProviderConnectionResult,
    ProviderInput, ProviderModelsResult, ProviderTomlInput, SavedProvider,
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
    install_skill_zip_inner, preview_existing_skills_mcp_inner, save_skills_mcp_note_inner,
    toggle_codex_mcp_inner, toggle_codex_skill_inner, SkillsMcpActionResult,
    SkillsMcpImportPreview, SkillsMcpState,
};
#[cfg(test)]
use skills_mcp::{
    normalize_legacy_zip_skill_dirs, read_skill_metadata, sort_managed_mcp_servers,
    sort_managed_skills, ManagedMcpServer, ManagedSkill,
};
#[cfg(test)]
use state::active_saved_provider_id_from_config;
#[cfg(test)]
use state::build_state;
use state::{auth_has_material, build_state_after_migration, ActionResult, CodexState};
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
struct CodexDesktopRestartResult {
    app_name: String,
    was_running: bool,
    warning: Option<String>,
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
    let state = build_state_after_migration(codex_dir).ok()?;
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
    // Explicit app-only override for isolated native validation and portable setups.
    if let Ok(value) = std::env::var("CODEXX_CONFIG_DIR") {
        if let Some(path) = codex_dir_from_text(&value)? {
            return Ok(path);
        }
    }
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
async fn save_skills_mcp_note(
    config_dir: Option<String>,
    item_kind: String,
    id: String,
    note: String,
) -> Result<SkillsMcpState> {
    tauri::async_runtime::spawn_blocking(move || {
        save_skills_mcp_note_inner(config_dir, item_kind, id, note)
    })
    .await
    .map_err(|e| CodexxError::Config(format!("保存 Skills/MCP 备注失败: {e}")))?
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
async fn check_codex_config(
    config_dir: Option<String>,
) -> Result<config_health::ConfigHealthReport> {
    tauri::async_runtime::spawn_blocking(move || {
        config_health::check_codex_config_inner(config_dir)
    })
    .await
    .map_err(|_| CodexxError::Config("检查配置失败，请稍后重试。".to_string()))?
}

#[tauri::command]
async fn repair_codex_config(
    config_dir: Option<String>,
    expected_fingerprint: String,
) -> Result<config_health::ConfigHealthRepairResult> {
    tauri::async_runtime::spawn_blocking(move || {
        config_health::repair_codex_config_inner(config_dir, expected_fingerprint)
    })
    .await
    .map_err(|_| CodexxError::Config("修复配置失败，请稍后重试。".to_string()))?
}

#[tauri::command]
async fn open_codex_config_file(config_dir: Option<String>) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || {
        config_health::open_codex_config_file_inner(config_dir)
    })
    .await
    .map_err(|_| CodexxError::Config("打开配置文件失败，请稍后重试。".to_string()))?
}

#[tauri::command]
async fn refresh_provider_balance(
    config_dir: Option<String>,
    source: String,
    provider_id: String,
) -> Result<provider_balance::ProviderBalanceResult> {
    tauri::async_runtime::spawn_blocking(move || {
        provider_balance::refresh_provider_balance_inner(config_dir, source, provider_id)
    })
    .await
    .map_err(|_| CodexxError::Config("刷新供应商余额失败，请稍后重试。".into()))?
}

#[tauri::command]
async fn get_provider_balances(
    config_dir: Option<String>,
) -> Result<Vec<provider_balance::ProviderBalanceResult>> {
    tauri::async_runtime::spawn_blocking(move || {
        provider_balance::get_provider_balances_inner(config_dir)
    })
    .await
    .map_err(|_| CodexxError::Config("读取供应商余额失败，请稍后重试。".into()))?
}

#[tauri::command]
async fn get_codex_language_status(
    config_dir: Option<String>,
) -> Result<codex_language::LanguageStatus> {
    tauri::async_runtime::spawn_blocking(move || codex_language::get_status(config_dir))
        .await
        .map_err(|_| CodexxError::Config("读取 Codex 界面语言状态失败，请重试。".into()))?
}

#[tauri::command]
async fn save_codex_language_preferences(
    config_dir: Option<String>,
    locale: codex_language::Locale,
    enabled: bool,
) -> Result<codex_language::LanguageStatus> {
    tauri::async_runtime::spawn_blocking(move || {
        codex_language::save_preferences(config_dir, locale, enabled)
    })
    .await
    .map_err(|_| CodexxError::Config("保存 Codex 界面语言偏好失败，请重试。".into()))?
}

#[tauri::command]
async fn apply_codex_language_preferences(
    config_dir: Option<String>,
    restore: bool,
) -> Result<codex_language::LanguageStatus> {
    tauri::async_runtime::spawn_blocking(move || {
        let codex_dir = resolve_codex_dir(config_dir.clone())?;
        sessions::with_session_maintenance(&codex_dir, || {
            codex_language::apply(config_dir, restore)
        })
    })
    .await
    .map_err(|_| CodexxError::Config("应用 Codex 界面语言失败，请查看状态后重试。".into()))?
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
    tauri::async_runtime::spawn_blocking(move || {
        let result = import_ccswitch_codex_providers_inner(db_path)?;
        failover::refresh_saved_routes().map_err(|error| {
            CodexxError::Config(format!("供应商已导入，但自动切换状态更新失败：{error}"))
        })?;
        Ok(result)
    })
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
async fn restart_codex_desktop(
    config_dir: Option<String>,
) -> std::result::Result<CodexDesktopRestartResult, String> {
    let (app_name, was_running, warning) = tauri::async_runtime::spawn_blocking(move || {
        let codex_dir = resolve_codex_dir(config_dir.clone()).map_err(|e| e.to_string())?;
        sessions::with_session_maintenance(&codex_dir, || {
            codex_language::restart_client(config_dir)
        })
        .map_err(|error| match error {
            CodexxError::Config(message) => format!("重启 Codex/ChatGPT 未完成：{message}"),
            other => other.to_string(),
        })
    })
    .await
    .map_err(|error| format!("重启 Codex 桌面客户端失败: {error}"))??;
    Ok(CodexDesktopRestartResult {
        app_name,
        was_running,
        warning,
    })
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
async fn get_builtin_prompt_detail(template_id: String) -> Result<BuiltinPromptDetail> {
    tauri::async_runtime::spawn_blocking(move || builtin_prompt_detail_inner(&template_id))
        .await
        .map_err(|e| CodexxError::Config(format!("读取内置提示词失败: {e}")))?
}

#[tauri::command]
async fn save_builtin_prompt_override(
    template_id: String,
    content: String,
) -> Result<BuiltinPromptDetail> {
    tauri::async_runtime::spawn_blocking(move || {
        let id = template_id.trim();
        // Resolve first so stale or unknown IDs cannot create detached local records.
        let current = builtin_prompt_detail_inner(id)?;
        let content = content.trim();
        if !current.customized && current.content.trim() == content {
            return Ok(current);
        }
        save_builtin_prompt_override_inner(id, content)?;
        builtin_prompt_detail_inner(id)
    })
    .await
    .map_err(|e| CodexxError::Config(format!("保存内置提示词修改失败: {e}")))?
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

fn finish_file_action(
    codex_dir: PathBuf,
    changes: &[AppliedFileChange],
    message: String,
    backup_id: Option<String>,
) -> Result<ActionResult> {
    match build_state_after_migration(codex_dir) {
        Ok(state) => Ok(ActionResult {
            ok: true,
            message,
            backup_id,
            state,
        }),
        Err(error) => fail_with_file_rollback(error, changes),
    }
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
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    let cfg = config_path(&codex_dir);
    let agents = agents_path(&codex_dir);
    let config_before = read_file_snapshot(&cfg)?;
    let text = text_from_snapshot(&cfg, config_before.as_deref())?;
    let mut doc = parse_toml_document(&cfg, &text)?;
    let agents_before = read_file_snapshot(&agents)?;
    let agents_text = text_from_snapshot(&agents, agents_before.as_deref())?;
    managed_agents_bounds(&agents_text)?;
    let previous_managed_file = managed_model_instruction_path(&codex_dir, &doc)?;
    if injection_mode == PromptInjectionMode::Replace {
        remember_current_instruction_prompt(&codex_dir)?;
    }
    let backup_id = create_backup(&codex_dir, action)?;
    let mut changes = Vec::new();

    let mutation = (|| -> Result<()> {
        match injection_mode {
            PromptInjectionMode::Replace => {
                if doc.get("model").is_none() {
                    doc["model"] = value("gpt-5.5");
                }
                doc["model_instructions_file"] = value(format!("./{filename}"));
                let prompt_path = codex_dir.join(filename);
                let prompt_before = read_file_snapshot(&prompt_path)?;
                changes.push(apply_file_change(
                    &prompt_path,
                    prompt_before,
                    Some(content.as_bytes().to_vec()),
                )?);
                changes.push(apply_file_change(
                    &cfg,
                    config_before,
                    Some(doc.to_string().into_bytes()),
                )?);
                let (next_agents, _) = remove_managed_agents_block_from_content(&agents_text)?;
                let next_agents =
                    (!next_agents.trim().is_empty()).then(|| next_agents.into_bytes());
                changes.push(apply_file_change(&agents, agents_before, next_agents)?);
            }
            PromptInjectionMode::Append => {
                let next_agents =
                    install_managed_agents_block_in_content(&agents_text, template_key, content)?;
                changes.push(apply_file_change(
                    &agents,
                    agents_before,
                    Some(next_agents.into_bytes()),
                )?);
                if previous_managed_file.is_some() {
                    doc.as_table_mut().remove("model_instructions_file");
                    changes.push(apply_file_change(
                        &cfg,
                        config_before,
                        Some(doc.to_string().into_bytes()),
                    )?);
                }
            }
        }

        if let Some(previous) = previous_managed_file {
            let next = codex_dir.join(filename);
            let should_remove = injection_mode == PromptInjectionMode::Append || previous != next;
            if should_remove && previous.parent() == Some(codex_dir.as_path()) {
                let previous_before = read_file_snapshot(&previous)?;
                changes.push(apply_file_change(&previous, previous_before, None)?);
            }
        }
        Ok(())
    })();
    if let Err(error) = mutation {
        return fail_with_file_rollback(error, &changes);
    }

    finish_file_action(
        codex_dir,
        &changes,
        format!(
            "已用{}模式启用 {title}（来源：{content_source}）",
            if injection_mode == PromptInjectionMode::Append {
                "追加"
            } else {
                "替换"
            }
        ),
        backup_id,
    )
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

#[tauri::command]
async fn get_provider_order(config_dir: Option<String>) -> Result<Vec<String>> {
    tauri::async_runtime::spawn_blocking(move || providers::get_provider_order_inner(config_dir))
        .await
        .map_err(|error| CodexxError::Config(format!("读取供应商排序失败: {error}")))?
}

#[tauri::command]
async fn save_provider_order(
    config_dir: Option<String>,
    order: Vec<String>,
) -> Result<Vec<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        providers::save_provider_order_inner(config_dir, order)
    })
    .await
    .map_err(|error| CodexxError::Config(format!("保存供应商排序失败: {error}")))?
}

enum ActiveProviderSelectionUpdate {
    Set(String),
    ClearIfOfficial,
}

fn finish_provider_selection(
    mut result: ActionResult,
    update: ActiveProviderSelectionUpdate,
) -> ActionResult {
    if matches!(update, ActiveProviderSelectionUpdate::ClearIfOfficial)
        && !result.state.is_official_provider
    {
        return result;
    }
    let codex_dir = PathBuf::from(&result.state.codex_dir);
    let refreshed_state = (|| -> Result<CodexState> {
        let conn = open_store()?;
        match update {
            ActiveProviderSelectionUpdate::Set(provider_id) => {
                let saved = providers::list_saved_providers_on_connection(&conn)?;
                let matches = providers::detected_live_custom_provider(&codex_dir)?
                    .map(|live| providers::matching_saved_provider_ids_for_live(&live, &saved))
                    .unwrap_or_default();
                // A detected UI row can carry a provisional ID. Adoption may
                // allocate a different ID, so only honor an explicit selection
                // that actually matches the now-active saved configuration.
                if matches.contains(&provider_id) {
                    remember_active_provider_on_connection(&conn, &codex_dir, &provider_id)?;
                }
            }
            ActiveProviderSelectionUpdate::ClearIfOfficial => {
                clear_active_provider_on_connection(&conn, &codex_dir)?;
            }
        }
        drop(conn);
        build_state_after_migration(codex_dir)
    })();
    match refreshed_state {
        Ok(state) => result.state = state,
        Err(error) => {
            result
                .message
                .push_str(&format!("；当前供应商状态记录失败：{error}"));
        }
    }
    result
}

#[tauri::command]
async fn get_provider_config_base(config_dir: Option<String>) -> Result<String> {
    tauri::async_runtime::spawn_blocking(move || {
        providers::get_provider_config_base_inner(config_dir)
    })
    .await
    .map_err(|error| CodexxError::Config(format!("读取供应商通用配置失败: {error}")))?
}

#[tauri::command]
async fn build_provider_toml_draft(
    provider: SavedProvider,
    config_dir: Option<String>,
    new_provider: Option<bool>,
) -> Result<String> {
    tauri::async_runtime::spawn_blocking(move || {
        if new_provider.unwrap_or(false) {
            build_provider_toml_draft_with_origin_inner(provider, config_dir, true)
        } else {
            build_provider_toml_draft_inner(provider, config_dir)
        }
    })
    .await
    .map_err(|e| CodexxError::Config(format!("生成供应商 TOML 失败: {e}")))?
}

#[tauri::command]
fn read_provider_headers(config_text: String) -> Result<Vec<providers::ProviderHeader>> {
    providers::read_provider_headers_inner(config_text)
}

#[tauri::command]
fn update_provider_headers(
    config_text: String,
    headers: Vec<providers::ProviderHeader>,
) -> Result<String> {
    providers::update_provider_headers_inner(config_text, headers)
}

fn save_provider_command_inner(provider: SavedProvider) -> Result<SavedProvider> {
    let saved = save_provider_inner(provider)?;
    failover::refresh_saved_routes().map_err(|error| {
        CodexxError::Config(format!("供应商已保存，但自动切换状态更新失败：{error}"))
    })?;
    Ok(saved)
}

#[tauri::command]
async fn save_provider(
    provider: SavedProvider,
    prefer_toml_auth: Option<bool>,
) -> Result<SavedProvider> {
    tauri::async_runtime::spawn_blocking(move || {
        let provider =
            providers::provider_with_editor_auth(provider, prefer_toml_auth.unwrap_or(false))?;
        save_provider_command_inner(provider)
    })
    .await
    .map_err(|e| CodexxError::Config(format!("保存供应商失败: {e}")))?
}

#[tauri::command]
async fn duplicate_provider(
    config_dir: Option<String>,
    provider_id: Option<String>,
    provider_name: Option<String>,
) -> Result<providers::DuplicateProviderResult> {
    tauri::async_runtime::spawn_blocking(move || {
        providers::duplicate_provider_inner(config_dir, provider_id, provider_name)
    })
    .await
    .map_err(|error| CodexxError::Config(format!("复制供应商失败: {error}")))?
}

#[tauri::command]
async fn update_codex_context_window(
    config_text: String,
    enabled: Option<bool>,
    previous_values: Option<context_config::ContextWindowValues>,
) -> Result<context_config::ContextWindowConfig> {
    context_config::update_codex_context_window_inner(config_text, enabled, previous_values)
}

#[tauri::command]
async fn get_usage_statistics(
    config_dir: Option<String>,
    range: Option<String>,
    model: Option<String>,
    force_refresh: Option<bool>,
) -> Result<usage::UsageStatistics> {
    tauri::async_runtime::spawn_blocking(move || {
        usage::get_usage_statistics_inner(config_dir, range, model, force_refresh)
    })
    .await
    .map_err(|error| CodexxError::Config(format!("读取用量统计失败: {error}")))?
}

#[tauri::command]
async fn get_provider_failover(config_dir: Option<String>) -> Result<failover::FailoverStatus> {
    tauri::async_runtime::spawn_blocking(move || failover::get_status(config_dir))
        .await
        .map_err(|error| CodexxError::Config(format!("读取自动切换设置失败: {error}")))?
}

#[tauri::command]
async fn save_provider_failover(
    config_dir: Option<String>,
    settings: failover::FailoverSettings,
) -> Result<failover::FailoverStatus> {
    tauri::async_runtime::spawn_blocking(move || failover::save_settings(config_dir, settings))
        .await
        .map_err(|error| CodexxError::Config(format!("保存自动切换设置失败: {error}")))?
}

#[tauri::command]
async fn reset_provider_failover_health(
    config_dir: Option<String>,
    provider_id: Option<String>,
) -> Result<failover::FailoverStatus> {
    tauri::async_runtime::spawn_blocking(move || failover::reset_health(config_dir, provider_id))
        .await
        .map_err(|error| CodexxError::Config(format!("重置供应商健康状态失败: {error}")))?
}

#[tauri::command]
async fn activate_saved_provider(
    config_dir: Option<String>,
    provider_id: String,
) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        let provider = providers::list_saved_providers_inner()?
            .into_iter()
            .find(|provider| provider.id == provider_id)
            .ok_or_else(|| CodexxError::Config("供应商不存在".to_string()))?;
        failover::ensure_conversion_route(config_dir.clone(), &provider)?;
        failover::with_provider_change(config_dir.clone(), || {
            let result = providers::activate_saved_provider_inner(config_dir, provider_id.clone())?;
            Ok(finish_provider_selection(
                result,
                ActiveProviderSelectionUpdate::Set(provider_id),
            ))
        })
    })
    .await
    .map_err(|error| CodexxError::Config(format!("启用供应商失败: {error}")))?
}

#[tauri::command]
async fn save_active_provider(
    provider: SavedProvider,
    config_dir: Option<String>,
    apply_common_config: Option<bool>,
    prefer_toml_auth: Option<bool>,
) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        let provider =
            providers::provider_with_editor_auth(provider, prefer_toml_auth.unwrap_or(false))?;
        let provider =
            providers::normalize_saved_provider_for_save(&providers::open_store()?, provider)?;
        failover::ensure_conversion_route(config_dir.clone(), &provider)?;
        let result = failover::with_provider_change(config_dir.clone(), || {
            let provider_id = provider.id.clone();
            let result = if apply_common_config.unwrap_or(false) {
                providers::save_active_provider_with_common_config_inner(provider, config_dir)?
            } else {
                save_active_provider_inner(provider, config_dir)?
            };
            Ok(finish_provider_selection(
                result,
                ActiveProviderSelectionUpdate::Set(provider_id),
            ))
        })?;
        failover::refresh_saved_routes().map_err(|error| {
            CodexxError::Config(format!("供应商已保存，但自动切换状态更新失败：{error}"))
        })?;
        Ok(result)
    })
    .await
    .map_err(|e| CodexxError::Config(format!("保存活动供应商失败: {e}")))?
}

#[tauri::command]
async fn delete_saved_provider(id: String, config_dir: Option<String>) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || {
        delete_saved_provider_inner(id.trim(), config_dir)?;
        failover::refresh_saved_routes().map_err(|error| {
            CodexxError::Config(format!("供应商已删除，但自动切换状态更新失败：{error}"))
        })
    })
    .await
    .map_err(|e| CodexxError::Config(format!("删除供应商失败: {e}")))?
}

#[tauri::command]
async fn repair_provider_model_catalog(
    config_dir: Option<String>,
) -> Result<providers::model_catalog::ProviderModelCatalogRepairResult> {
    tauri::async_runtime::spawn_blocking(move || {
        let codex_dir = resolve_codex_dir(config_dir)?;
        let changed = providers::model_catalog::upgrade_owned_model_catalog(&codex_dir)?;
        Ok(providers::model_catalog::ProviderModelCatalogRepairResult {
            changed,
            message: changed.then(|| {
                "已修复供应商模型菜单的图片输入和思考强度，请重启 Codex 后使用".to_string()
            }),
        })
    })
    .await
    .map_err(|e| CodexxError::Config(format!("修复供应商模型菜单失败: {e}")))?
}

#[tauri::command]
async fn list_official_models(
    force_refresh: Option<bool>,
) -> Result<official_models::OfficialModelsResult> {
    tauri::async_runtime::spawn_blocking(move || {
        official_models::list_official_models(force_refresh.unwrap_or(false))
    })
    .await
    .map_err(|_| CodexxError::Config("读取官方模型目录失败，请稍后刷新。".into()))
}

#[tauri::command]
async fn update_official_model_draft(config_text: String, model: String) -> Result<String> {
    tauri::async_runtime::spawn_blocking(move || official_model_draft::update(&config_text, &model))
        .await
        .map_err(|_| CodexxError::Config("更新模型草稿失败，未修改配置。".into()))?
}

#[tauri::command]
async fn inspect_official_model_draft(config_text: String) -> Result<Option<String>> {
    tauri::async_runtime::spawn_blocking(move || official_model_draft::inspect(&config_text))
        .await
        .map_err(|_| CodexxError::Config("读取模型草稿失败。".into()))?
}

#[tauri::command]
async fn get_codex_state(config_dir: Option<String>) -> Result<CodexState> {
    tauri::async_runtime::spawn_blocking(move || get_codex_state_inner(config_dir))
        .await
        .map_err(|e| CodexxError::Config(format!("读取 Codex 状态失败: {e}")))?
}

fn get_codex_state_inner(config_dir: Option<String>) -> Result<CodexState> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    build_state_after_migration(codex_dir)
}

#[tauri::command]
async fn switch_official_provider(config_dir: Option<String>) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        failover::with_provider_change(config_dir.clone(), || {
            let codex_dir = resolve_codex_dir(config_dir.clone())?;
            let profile_id = providers::official_profiles::default_profile_id(&codex_dir)?;
            let result = switch_official_profile_inner(config_dir, profile_id)?;
            Ok(finish_provider_selection(
                result,
                ActiveProviderSelectionUpdate::ClearIfOfficial,
            ))
        })
    })
    .await
    .map_err(|e| CodexxError::Config(format!("切换官方配置失败: {e}")))?
}

#[tauri::command]
async fn list_official_profiles(config_dir: Option<String>) -> Result<Vec<OfficialProfileSummary>> {
    tauri::async_runtime::spawn_blocking(move || list_official_profiles_inner(config_dir))
        .await
        .map_err(|error| CodexxError::Config(format!("读取官方配置列表失败: {error}")))?
}

#[tauri::command]
async fn get_official_profile_quota(
    config_dir: Option<String>,
    profile_id: String,
) -> Result<providers::quota::OfficialQuotaSnapshot> {
    tauri::async_runtime::spawn_blocking(move || {
        providers::quota::get_official_profile_quota_inner(config_dir, profile_id)
    })
    .await
    .map_err(|_| CodexxError::Config("读取官方账号额度失败，请重试".to_string()))?
}

#[tauri::command]
async fn get_official_profile_reset_credits(
    config_dir: Option<String>,
    profile_id: String,
) -> Result<providers::quota::OfficialResetCreditsSnapshot> {
    tauri::async_runtime::spawn_blocking(move || {
        providers::quota::get_official_profile_reset_credits_inner(config_dir, profile_id)
    })
    .await
    .map_err(|_| CodexxError::Config("读取官方账号重置次数失败，请重试".to_string()))?
}

#[tauri::command]
async fn get_official_profile(
    config_dir: Option<String>,
    profile_id: String,
) -> Result<OfficialProfileDetail> {
    tauri::async_runtime::spawn_blocking(move || get_official_profile_inner(config_dir, profile_id))
        .await
        .map_err(|error| CodexxError::Config(format!("读取官方配置失败: {error}")))?
}

#[tauri::command]
async fn save_official_profile(input: OfficialProfileInput) -> Result<OfficialProfileActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        // Saving a separate account is a library edit, not a manual route change.
        let applies_live = match input.id.as_ref() {
            Some(id) => {
                get_official_profile_inner(input.config_dir.clone(), id.clone())?
                    .profile
                    .is_current
            }
            None => false,
        };
        if applies_live {
            failover::with_provider_change(input.config_dir.clone(), || {
                save_official_profile_inner(input)
            })
        } else {
            save_official_profile_inner(input)
        }
    })
    .await
    .map_err(|error| CodexxError::Config(format!("保存官方配置失败: {error}")))?
}

#[tauri::command]
async fn duplicate_official_profile(
    config_dir: Option<String>,
    profile_id: String,
    provider_name: Option<String>,
) -> Result<OfficialProfileActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        duplicate_official_profile_inner(config_dir, profile_id, provider_name)
    })
    .await
    .map_err(|error| CodexxError::Config(format!("复制官方配置失败: {error}")))?
}

#[tauri::command]
async fn switch_official_profile(
    config_dir: Option<String>,
    profile_id: String,
) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        failover::with_provider_change(config_dir.clone(), || {
            switch_official_profile_inner(config_dir, profile_id)
        })
    })
    .await
    .map_err(|error| CodexxError::Config(format!("切换官方配置失败: {error}")))?
}

#[tauri::command]
async fn delete_official_profile(config_dir: Option<String>, profile_id: String) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || {
        delete_official_profile_inner(config_dir, profile_id)
    })
    .await
    .map_err(|error| CodexxError::Config(format!("删除官方配置失败: {error}")))?
}

#[tauri::command]
async fn reset_official_provider(input: OfficialConfigInput) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        failover::with_provider_change(input.config_dir.clone(), || {
            let result = providers::official_profiles::reset_default_official_profile_inner(input)?;
            Ok(finish_provider_selection(
                result,
                ActiveProviderSelectionUpdate::ClearIfOfficial,
            ))
        })
    })
    .await
    .map_err(|e| CodexxError::Config(format!("新建官方配置失败: {e}")))?
}

#[tauri::command]
async fn save_official_config(input: OfficialConfigInput) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        failover::with_provider_change(input.config_dir.clone(), || {
            let result = save_official_config_inner(
                input.config_dir,
                input.model,
                input.auth_json,
                input.config_text,
            )?;
            Ok(finish_provider_selection(
                result,
                ActiveProviderSelectionUpdate::ClearIfOfficial,
            ))
        })
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
    ensure_directory(&codex_dir)?;
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    let cfg = config_path(&codex_dir);
    let agents = agents_path(&codex_dir);
    let agents_before = read_file_snapshot(&agents)?;
    let agents_text = text_from_snapshot(&agents, agents_before.as_deref())?;
    managed_agents_bounds(&agents_text)?;
    let backup_id = create_backup(&codex_dir, "disable-instruct")?;

    let config_before = read_file_snapshot(&cfg)?;
    let text = text_from_snapshot(&cfg, config_before.as_deref())?;
    let mut doc = parse_toml_document(&cfg, &text)?;
    let current = string_value(&doc, "model_instructions_file");
    let managed_model_path = managed_model_instruction_path(&codex_dir, &doc)?;
    let removed_model = managed_model_path.is_some();
    let mut changes = Vec::new();
    if removed_model {
        doc.as_table_mut().remove("model_instructions_file");
        changes.push(apply_file_change(
            &cfg,
            config_before,
            Some(doc.to_string().into_bytes()),
        )?);
    }
    let (next_agents, removed_agents) = remove_managed_agents_block_from_content(&agents_text)?;
    if removed_agents {
        let next_agents = (!next_agents.trim().is_empty()).then(|| next_agents.into_bytes());
        match apply_file_change(&agents, agents_before, next_agents) {
            Ok(change) => changes.push(change),
            Err(error) => return fail_with_file_rollback(error, &changes),
        }
    }
    if delete_file.unwrap_or(true) {
        if let Some(md) = managed_model_path {
            if md.parent() == Some(codex_dir.as_path()) {
                let before = match read_file_snapshot(&md) {
                    Ok(before) => before,
                    Err(error) => return fail_with_file_rollback(error, &changes),
                };
                match apply_file_change(&md, before, None) {
                    Ok(change) => changes.push(change),
                    Err(error) => return fail_with_file_rollback(error, &changes),
                }
            }
        }
    }

    let removed = removed_model || removed_agents;
    finish_file_action(
        codex_dir,
        &changes,
        if removed {
            "已禁用指令提示词".to_string()
        } else if current.is_some() {
            "当前使用的是用户自己的提示词，Codex-X 未做修改".to_string()
        } else {
            "当前没有启用 Codex-X 提示词".to_string()
        },
        backup_id,
    )
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
    ensure_directory(&codex_dir)?;
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    let cfg = config_path(&codex_dir);
    let config_before = read_file_snapshot(&cfg)?;
    let text = text_from_snapshot(&cfg, config_before.as_deref())?;
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
    let mut changes = Vec::new();
    if current.is_some() {
        doc.as_table_mut().remove("model_instructions_file");
        changes.push(apply_file_change(
            &cfg,
            config_before,
            Some(doc.to_string().into_bytes()),
        )?);
    }
    finish_file_action(
        codex_dir,
        &changes,
        if current.is_some() {
            "已禁用用户外部提示词，原 md 文件已保留".to_string()
        } else {
            "当前没有外部提示词".to_string()
        },
        backup_id,
    )
}

#[tauri::command]
async fn disable_external_instruction(config_dir: Option<String>) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || disable_external_instruction_inner(config_dir))
        .await
        .map_err(|e| CodexxError::Config(format!("禁用外部提示词失败: {e}")))?
}

fn direct_provider_toml_input(mut input: ProviderTomlInput) -> Result<ProviderTomlInput> {
    let codex_dir = resolve_codex_dir(input.config_dir.clone())?;
    let doc = parse_toml_document(&config_path(&codex_dir), &input.config_text)?;
    let direct = failover::direct_document(&codex_dir, &doc)?;
    if direct.to_string() != doc.to_string() {
        let old_id = string_value(&doc, "model_provider");
        let old_token = providers::experimental_bearer_token_from_doc(&doc, old_id.as_deref());
        // Only replace the known local credential echoed back by the editor.
        // A newly entered key belongs to the user's edit and must be retained.
        if old_token.is_some() && input.api_key.as_deref().map(str::trim) == old_token.as_deref() {
            let direct_id = string_value(&direct, "model_provider");
            input.api_key =
                providers::experimental_bearer_token_from_doc(&direct, direct_id.as_deref());
        }
        input.config_text = direct.to_string();
    }
    Ok(input)
}

#[tauri::command]
async fn save_provider_toml_config(
    input: ProviderTomlInput,
    provider_id: Option<String>,
) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        failover::with_provider_change(input.config_dir.clone(), || {
            let input = direct_provider_toml_input(input)?;
            let result = save_provider_toml_config_inner(input)?;
            Ok(match provider_id {
                Some(provider_id) => finish_provider_selection(
                    result,
                    ActiveProviderSelectionUpdate::Set(provider_id),
                ),
                None => result,
            })
        })
    })
    .await
    .map_err(|e| CodexxError::Config(format!("保存供应商 TOML 失败: {e}")))?
}

#[tauri::command]
async fn test_provider_connection(
    base_url: String,
    api_key: Option<String>,
    upstream_api: Option<String>,
    config_text: Option<String>,
) -> Result<ProviderConnectionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        if upstream_api.is_none() && config_text.is_none() {
            test_provider_connection_inner(base_url, api_key)
        } else {
            providers::test_provider_connection_with_protocol_inner(
                base_url,
                api_key,
                upstream_api,
                config_text,
            )
        }
    })
    .await
    .map_err(|e| CodexxError::Config(format!("测试连接失败: {e}")))?
}

#[tauri::command]
async fn fetch_provider_models(
    base_url: String,
    api_key: Option<String>,
    upstream_api: Option<String>,
    config_text: Option<String>,
) -> Result<ProviderModelsResult> {
    tauri::async_runtime::spawn_blocking(move || {
        if upstream_api.is_none() && config_text.is_none() {
            fetch_provider_models_inner(base_url, api_key)
        } else {
            providers::fetch_provider_models_with_protocol_inner(
                base_url,
                api_key,
                upstream_api,
                config_text,
            )
        }
    })
    .await
    .map_err(|e| CodexxError::Config(format!("获取模型列表失败: {e}")))?
}

#[tauri::command]
async fn switch_provider(input: ProviderInput) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        failover::with_provider_change(input.config_dir.clone(), || {
            let provider_id = input.provider_id.clone();
            let result = switch_provider_inner(input)?;
            Ok(match provider_id {
                Some(provider_id) => finish_provider_selection(
                    result,
                    ActiveProviderSelectionUpdate::Set(provider_id),
                ),
                None => result,
            })
        })
    })
    .await
    .map_err(|e| CodexxError::Config(format!("切换供应商失败: {e}")))?
}

#[tauri::command]
async fn list_backups() -> Result<Vec<BackupEntry>> {
    tauri::async_runtime::spawn_blocking(backups)
        .await
        .map_err(|e| CodexxError::Config(format!("读取备份列表失败: {e}")))?
}

fn read_backup_file_snapshot(path: &Path) -> Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(file_io::io_err(path, error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CodexxError::Config(format!(
            "备份文件不是普通文件: {}",
            path.display()
        )));
    }
    read_file_snapshot(path)
}

fn declared_backup_file_snapshot(path: &Path, declared_present: bool) -> Result<Option<Vec<u8>>> {
    match (declared_present, read_backup_file_snapshot(path)?) {
        (true, Some(bytes)) => Ok(Some(bytes)),
        (true, None) => Err(CodexxError::Config(format!(
            "备份元数据声明文件存在，但备份文件缺失: {}",
            path.display()
        ))),
        (false, None) => Ok(None),
        (false, Some(_)) => Err(CodexxError::Config(format!(
            "备份元数据声明文件不存在，但目录中出现了多余文件: {}",
            path.display()
        ))),
    }
}

fn restore_backup_inner(config_dir: Option<String>, backup_id: String) -> Result<ActionResult> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    let backup_id = backup_id.trim().to_string();
    let mut backup_components = Path::new(&backup_id).components();
    if !matches!(
        backup_components.next(),
        Some(std::path::Component::Normal(_))
    ) || backup_components.next().is_some()
    {
        return Err(CodexxError::Config("备份 ID 无效".to_string()));
    }
    let dir = action_backup_root(&codex_dir)?.join(&backup_id);
    let dir_metadata = fs::symlink_metadata(&dir).ok();
    if !dir_metadata
        .as_ref()
        .is_some_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
    {
        return Err(CodexxError::Config(format!("备份不存在: {backup_id}")));
    }

    let backup_meta_path = dir.join("meta.json");
    let backup_meta_bytes = read_backup_file_snapshot(&backup_meta_path)?.ok_or_else(|| {
        CodexxError::Config(format!(
            "备份缺少元数据文件: {}",
            backup_meta_path.display()
        ))
    })?;
    let backup_meta = serde_json::from_slice::<BackupMeta>(&backup_meta_bytes)
        .map_err(|error| file_io::json_err(&backup_meta_path, error))?;
    if backup_meta.id != backup_id {
        return Err(CodexxError::Config(format!(
            "备份元数据 ID 与请求不一致：元数据为 {}，请求为 {backup_id}",
            backup_meta.id
        )));
    }
    validate_backup_codex_dir(&backup_meta, &codex_dir)?;

    let cfg = config_path(&codex_dir);
    let auth = auth_path(&codex_dir);
    let agents = agents_path(&codex_dir);
    let backup_cfg = dir.join("config.toml");
    let backup_auth = dir.join("auth.json");
    let backup_agents = dir.join(AGENTS_FILENAME);
    let backup_config = declared_backup_file_snapshot(&backup_cfg, backup_meta.had_config)?;
    let backup_config = match backup_config {
        Some(bytes) => {
            let text = text_from_snapshot(&backup_cfg, Some(&bytes))?;
            let text = migrated_legacy_prompt_config_text(&cfg, &text)?.unwrap_or(text);
            let doc = parse_toml_document(&cfg, &text)?;
            let direct = failover::direct_document(&codex_dir, &doc)?;
            Some(if direct.to_string() == doc.to_string() {
                text.into_bytes()
            } else {
                direct.to_string().into_bytes()
            })
        }
        None => None,
    };
    let backup_auth = declared_backup_file_snapshot(&backup_auth, backup_meta.had_auth)?;
    if let Some(bytes) = backup_auth.as_deref() {
        serde_json::from_slice::<serde_json::Value>(bytes)
            .map_err(|error| file_io::json_err(&dir.join("auth.json"), error))?;
    }
    let backup_agents = declared_backup_file_snapshot(&backup_agents, backup_meta.had_agents)?;

    ensure_directory(&codex_dir)?;
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    migrate_legacy_prompt_config_locked(&codex_dir)?;
    let restore_marker = create_backup(&codex_dir, "before-restore")?;
    let config_before = read_file_snapshot(&cfg)?;
    let auth_before = read_file_snapshot(&auth)?;
    let agents_before = read_file_snapshot(&agents)?;

    let mut changes = Vec::new();
    let mutation = (|| -> Result<()> {
        changes.push(apply_file_change(&cfg, config_before, backup_config)?);
        changes.push(apply_file_change(&auth, auth_before, backup_auth)?);
        changes.push(apply_file_change(&agents, agents_before, backup_agents)?);
        Ok(())
    })();
    if let Err(error) = mutation {
        return fail_with_file_rollback(error, &changes);
    }

    finish_file_action(
        codex_dir,
        &changes,
        format!("已恢复备份 {backup_id}"),
        restore_marker,
    )
}

#[tauri::command]
async fn restore_backup(config_dir: Option<String>, backup_id: String) -> Result<ActionResult> {
    tauri::async_runtime::spawn_blocking(move || {
        failover::with_provider_change(config_dir.clone(), || {
            let mut result = restore_backup_inner(config_dir.clone(), backup_id)?;
            failover::recover_stale_route(config_dir)?;
            result.state = build_state_after_migration(PathBuf::from(&result.state.codex_dir))?;
            Ok(result)
        })
    })
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
    let app = tauri::Builder::default()
        // This must remain the first plugin so a second launch cannot initialize
        // another tray or start concurrent configuration work.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            desktop_lifecycle::restore_main_window(app);
        }))
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            desktop_lifecycle::setup_system_tray(app)?;
            failover::attach_app_handle(app.handle().clone());
            if let Err(error) = failover::initialize() {
                desktop_lifecycle::report_failover_lifecycle_error(
                    app.handle(),
                    "自动切换暂时无法启动",
                    &error.to_string(),
                );
            }
            Ok(())
        })
        .on_window_event(desktop_lifecycle::handle_window_event)
        .invoke_handler(tauri::generate_handler![
            app_update_channel::check_online_app_update,
            app_update::install_app_update,
            get_about_info,
            restart_codex_desktop,
            check_app_update,
            get_skills_mcp_state,
            preview_existing_skills_mcp,
            import_existing_skills_mcp,
            toggle_codex_skill,
            toggle_codex_mcp,
            save_skills_mcp_note,
            install_skill_zip,
            transfers::import_skills_mcp_archive,
            transfers::export_skills_mcp_archive,
            transfers::export_codex_sessions,
            check_skill_updates,
            get_startup_diagnostics,
            check_codex_config,
            repair_codex_config,
            open_codex_config_file,
            refresh_provider_balance,
            get_provider_balances,
            get_codex_language_status,
            save_codex_language_preferences,
            apply_codex_language_preferences,
            get_session_sync_status,
            sync_sessions_provider,
            delete_codex_sessions,
            read_ccswitch_official_auth,
            import_ccswitch_codex_providers,
            list_saved_prompts,
            get_builtin_prompt_status,
            get_builtin_prompt_detail,
            save_builtin_prompt_override,
            refresh_builtin_prompts,
            remember_current_instruction,
            save_prompt,
            delete_saved_prompt,
            enable_saved_prompt,
            list_saved_providers,
            get_provider_order,
            save_provider_order,
            get_provider_config_base,
            build_provider_toml_draft,
            read_provider_headers,
            update_provider_headers,
            save_provider,
            duplicate_provider,
            update_codex_context_window,
            get_usage_statistics,
            get_provider_failover,
            save_provider_failover,
            reset_provider_failover_health,
            save_active_provider,
            activate_saved_provider,
            delete_saved_provider,
            get_codex_state,
            repair_provider_model_catalog,
            list_official_models,
            update_official_model_draft,
            inspect_official_model_draft,
            switch_official_provider,
            list_official_profiles,
            get_official_profile,
            get_official_profile_quota,
            get_official_profile_reset_credits,
            save_official_profile,
            duplicate_official_profile,
            switch_official_profile,
            delete_official_profile,
            reset_official_provider,
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
        .build(tauri::generate_context!())
        .expect("error while building Codex-X");

    app.run(desktop_lifecycle::handle_run_event);
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod failover_integration_tests;
