use crate::constants::{
    AGENTS_FILENAME, AGENTS_MANAGED_BEGIN, AGENTS_MANAGED_END, AGENTS_TEMPLATE_PREFIX,
};
use crate::error::{CodexxError, Result};
use crate::file_io::{io_err, read_to_string_if_exists, write_text};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn agents_path(codex_dir: &Path) -> PathBuf {
    codex_dir.join(AGENTS_FILENAME)
}

pub(crate) fn managed_agents_bounds(content: &str) -> Result<Option<(usize, usize)>> {
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

pub(crate) fn managed_agents_template_key_from_content(content: &str) -> Option<String> {
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

pub(crate) fn managed_agents_template_key(codex_dir: &Path) -> Result<Option<String>> {
    let path = agents_path(codex_dir);
    let content = read_to_string_if_exists(&path)?;
    Ok(managed_agents_template_key_from_content(&content))
}

pub(crate) fn install_managed_agents_block(
    codex_dir: &Path,
    template_key: &str,
    content: &str,
) -> Result<()> {
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

pub(crate) fn uninstall_managed_agents_block(codex_dir: &Path) -> Result<bool> {
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
