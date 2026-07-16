mod mcp;
mod skills;
mod types;

pub(crate) use mcp::{sort_managed_mcp_servers, toggle_codex_mcp_inner};
pub(crate) use skills::{
    check_skill_updates_inner, install_skill_zip_inner, normalize_legacy_zip_skill_dirs,
    sort_managed_skills, toggle_codex_skill_inner,
};
pub(crate) use types::{
    ManagedMcpServer, SkillsMcpActionResult, SkillsMcpImportPreview, SkillsMcpState,
};

#[cfg(test)]
pub(crate) use skills::read_skill_metadata;
#[cfg(test)]
pub(crate) use types::ManagedSkill;

use crate::error::Result;
use crate::file_io::{ensure_directory, io_err};
use crate::paths::home_dir;
use crate::resolve_codex_dir;
use mcp::{
    db_managed_mcp, import_ccswitch_mcp_servers_for_codex, list_mcp_from_config, mcp_summary,
    preview_ccswitch_mcp_servers_for_codex, save_managed_mcp,
};
use skills::{
    codex_skills_dir, copy_dir_recursive, disabled_skills_dir, sanitize_dir_name, scan_skill_dir,
};
use std::collections::HashSet;
use std::fs;

fn extend_unmanaged_mcp_candidates(
    output: &mut Vec<ManagedMcpServer>,
    seen_ids: &mut HashSet<String>,
    candidates: impl IntoIterator<Item = ManagedMcpServer>,
) {
    for server in candidates {
        if seen_ids.insert(server.id.clone()) {
            output.push(server);
        }
    }
}

pub(crate) fn build_skills_mcp_state_inner(config_dir: Option<String>) -> Result<SkillsMcpState> {
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

pub(crate) fn import_existing_skills_mcp_inner(
    config_dir: Option<String>,
) -> Result<SkillsMcpActionResult> {
    let codex_dir = resolve_codex_dir(config_dir.clone())?;
    let skills_dir = codex_skills_dir(&codex_dir);
    ensure_directory(&skills_dir)?;
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
    let mut imported_mcp_ids = db_managed_mcp()?
        .into_iter()
        .map(|(id, _, _, _)| id)
        .collect::<HashSet<_>>();
    for server in list_mcp_from_config(&codex_dir)? {
        if !imported_mcp_ids.insert(server.id.clone()) {
            continue;
        }
        save_managed_mcp(&server.id, &server.name, &server.config_json, true)?;
        imported_mcp += 1;
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

pub(crate) fn preview_existing_skills_mcp_inner(
    config_dir: Option<String>,
) -> Result<SkillsMcpImportPreview> {
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

    let mut config_mcp_servers = list_mcp_from_config(&codex_dir)?;
    for server in &mut config_mcp_servers {
        server.source = "config.toml".to_string();
    }
    let mut seen_mcp = db_managed_mcp()?
        .into_iter()
        .map(|(id, _, _, _)| id)
        .collect::<HashSet<_>>();
    let mut mcp_servers = Vec::new();
    extend_unmanaged_mcp_candidates(&mut mcp_servers, &mut seen_mcp, config_mcp_servers);
    extend_unmanaged_mcp_candidates(
        &mut mcp_servers,
        &mut seen_mcp,
        preview_ccswitch_mcp_servers_for_codex(&codex_dir)?,
    );
    sort_managed_skills(&mut skills);
    sort_managed_mcp_servers(&mut mcp_servers);
    Ok(SkillsMcpImportPreview {
        skills,
        mcp_servers,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mcp_candidate(id: &str, source: &str) -> ManagedMcpServer {
        ManagedMcpServer {
            id: id.to_string(),
            name: id.to_string(),
            transport: "stdio".to_string(),
            enabled: true,
            source: source.to_string(),
            summary: id.to_string(),
            command: Some(id.to_string()),
            url: None,
            config_json: json!({ "command": id }),
        }
    }

    #[test]
    fn mcp_import_preview_excludes_managed_and_duplicate_ids() {
        let mut seen = HashSet::from(["alpha".to_string()]);
        let mut candidates = Vec::new();
        extend_unmanaged_mcp_candidates(
            &mut candidates,
            &mut seen,
            [
                mcp_candidate("alpha", "config.toml"),
                mcp_candidate("beta", "config.toml"),
            ],
        );
        extend_unmanaged_mcp_candidates(
            &mut candidates,
            &mut seen,
            [
                mcp_candidate("beta", "cc-switch"),
                mcp_candidate("gamma", "cc-switch"),
            ],
        );

        assert_eq!(
            candidates
                .iter()
                .map(|server| (server.id.as_str(), server.source.as_str()))
                .collect::<Vec<_>>(),
            vec![("beta", "config.toml"), ("gamma", "cc-switch")]
        );

        let mut imported_ids = seen;
        let mut second_preview = Vec::new();
        extend_unmanaged_mcp_candidates(
            &mut second_preview,
            &mut imported_ids,
            [
                mcp_candidate("alpha", "config.toml"),
                mcp_candidate("beta", "config.toml"),
                mcp_candidate("gamma", "cc-switch"),
            ],
        );
        assert!(second_preview.is_empty());
    }
}
