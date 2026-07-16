mod catalog;
mod managed_agents;
mod store;
mod types;

pub(crate) use catalog::{
    builtin_prompt_content, builtin_prompt_status_inner, bundled_prompt_meta, bundled_prompt_metas,
    refresh_builtin_prompts_with_active,
};
pub(crate) use managed_agents::{
    agents_path, install_managed_agents_block, managed_agents_bounds, managed_agents_template_key,
    uninstall_managed_agents_block,
};
pub(crate) use store::{
    delete_prompt_inner, get_saved_prompt_inner, list_saved_prompts_inner,
    normalize_prompt_filename, save_prompt_inner,
};
pub(crate) use types::{BuiltinPromptStatus, SavedPrompt};

#[cfg(test)]
pub(crate) use catalog::{
    cached_prompt_fallback_statuses, delete_cached_prompt_ids, github_prompt_catalog_from_entries,
    jsdelivr_prompt_catalog_from_entries, prompt_content_source_urls, stable_remote_prompt_id,
    stale_cached_prompt_ids,
};
#[cfg(test)]
pub(crate) use managed_agents::managed_agents_template_key_from_content;
#[cfg(test)]
pub(crate) use types::{CachedBuiltinPrompt, GithubContentEntry};

use crate::error::Result;
use crate::file_io::{parse_toml_document, read_to_string_if_exists};
use crate::paths::home_dir;
use crate::{config_path, sanitize_id, string_value};
use catalog::cached_builtin_prompts;
use std::path::{Path, PathBuf};
use store::find_saved_prompt_by_current_file;

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

pub(crate) fn prompt_template_key_for_instruction(value: &str) -> Result<Option<String>> {
    let normalized = value.replace('\\', "/");
    let filename = normalized.rsplit('/').next().unwrap_or(&normalized);
    if let Some(id) = builtin_prompt_id_for_filename(filename)? {
        return Ok(Some(format!("builtin:{id}")));
    }
    Ok(saved_prompt_id_for_filename(filename)?.map(|id| format!("saved:{id}")))
}

pub(crate) fn resolve_instruction_path(codex_dir: &Path, value: &str) -> PathBuf {
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

pub(crate) fn remember_current_instruction_prompt(codex_dir: &Path) -> Result<Option<SavedPrompt>> {
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
