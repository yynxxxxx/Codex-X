use super::build_skills_mcp_state_inner;
use super::types::{CcSwitchSkillMeta, ManagedSkill, SkillsMcpActionResult, SkillsMcpState};
use crate::ccswitch::default_ccswitch_db_path;
use crate::constants::MAX_SKILL_ZIP_BYTES;
use crate::error::{CodexxError, Result};
use crate::file_io::{ensure_directory, io_err, read_to_string_if_exists};
use crate::paths::app_home;
use crate::{now_rfc3339, open_db, resolve_codex_dir};
use chrono::Local;
use rusqlite::{params, Connection, OpenFlags};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

pub(super) fn codex_skills_dir(codex_dir: &Path) -> PathBuf {
    codex_dir.join("skills")
}

pub(super) fn disabled_skills_dir() -> Result<PathBuf> {
    Ok(app_home()?.join("disabled-skills"))
}

pub(super) fn sanitize_dir_name(input: &str, fallback: &str) -> String {
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

pub(super) fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    ensure_directory(dst)?;
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
                ensure_directory(parent)?;
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

pub(crate) fn read_skill_metadata(skill_dir: &Path, fallback: &str) -> (String, Option<String>) {
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

pub(crate) fn normalize_legacy_zip_skill_dirs(base: &Path) -> Result<()> {
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

pub(super) fn scan_skill_dir(
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

pub(crate) fn sort_managed_skills(skills: &mut [ManagedSkill]) {
    skills.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
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

pub(crate) fn toggle_codex_skill_inner(
    config_dir: Option<String>,
    id: String,
    enabled: bool,
) -> Result<SkillsMcpState> {
    let codex_dir = resolve_codex_dir(config_dir.clone())?;
    let skills_dir = codex_skills_dir(&codex_dir);
    let disabled_dir = disabled_skills_dir()?;
    ensure_directory(&skills_dir)?;
    ensure_directory(&disabled_dir)?;
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

pub(crate) fn install_skill_zip_inner(
    config_dir: Option<String>,
    file_name: String,
    bytes: Vec<u8>,
) -> Result<SkillsMcpActionResult> {
    let codex_dir = resolve_codex_dir(config_dir.clone())?;
    let skills_dir = codex_skills_dir(&codex_dir);
    ensure_directory(&skills_dir)?;
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| CodexxError::Config(format!("读取 ZIP 失败: {e}")))?;
    let tmp = app_home()?
        .join("tmp")
        .join(format!("skill-zip-{}", Local::now().timestamp_millis()));
    ensure_directory(&tmp)?;
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
                ensure_directory(&out)?;
            } else {
                if let Some(parent) = out.parent() {
                    ensure_directory(parent)?;
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

pub(crate) fn check_skill_updates_inner(config_dir: Option<String>) -> Result<SkillsMcpState> {
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
