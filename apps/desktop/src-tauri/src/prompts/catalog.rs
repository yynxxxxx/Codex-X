use super::store::normalize_prompt_filename;
use super::types::{
    BuiltinPromptStatus, BundledPromptMeta, CachedBuiltinPrompt, GithubContentEntry,
};
use crate::constants::{
    GITHUB_EXAMPLES_API, INSTRUCTION_54_CONTENT, INSTRUCTION_54_FILENAME, INSTRUCTION_CONTENT,
    INSTRUCTION_FILENAME, INSTRUCTION_JELI_CONTENT, INSTRUCTION_JELI_FILENAME,
};
use crate::error::{CodexxError, Result};
use crate::{now_rfc3339, open_db};
use rusqlite::{params, Connection, TransactionBehavior};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

static BUILTIN_PROMPT_CACHE_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn bundled_prompt_metas() -> [BundledPromptMeta; 3] {
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

pub(crate) fn bundled_prompt_meta(template_id: &str) -> Option<BundledPromptMeta> {
    let id = if template_id.trim().is_empty() {
        "gpt5.5-unrestricted"
    } else {
        template_id.trim()
    };
    bundled_prompt_metas()
        .into_iter()
        .find(|item| item.id == id)
}

pub(crate) fn stable_remote_prompt_id(filename: &str) -> String {
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

pub(super) fn cached_builtin_prompts() -> Result<Vec<CachedBuiltinPrompt>> {
    let conn = open_db()?;
    cached_builtin_prompts_from_connection(&conn)
}

pub(crate) fn stale_cached_prompt_ids(
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
pub(crate) fn delete_cached_prompt_ids(
    conn: &mut Connection,
    stale_ids: &[String],
) -> Result<usize> {
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

pub(crate) fn github_prompt_catalog_from_entries(
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

pub(crate) fn cached_prompt_fallback_statuses(
    caches: Vec<CachedBuiltinPrompt>,
) -> Vec<BuiltinPromptStatus> {
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

pub(crate) fn builtin_prompt_status_inner() -> Result<Vec<BuiltinPromptStatus>> {
    Ok(cached_prompt_fallback_statuses(cached_builtin_prompts()?))
}

pub(crate) fn refresh_builtin_prompts_with_active(
    active_remote_builtin_prompt_id: impl FnOnce() -> Option<String>,
) -> Result<Vec<BuiltinPromptStatus>> {
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
    if let Some(active_id) = active_remote_builtin_prompt_id() {
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

pub(crate) fn builtin_prompt_content(
    template_id: &str,
) -> Result<(String, String, String, String)> {
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
