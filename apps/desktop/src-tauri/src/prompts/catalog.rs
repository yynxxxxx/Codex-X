use super::store::normalize_prompt_filename;
use super::types::{
    BuiltinPromptStatus, BundledPromptMeta, CachedBuiltinPrompt, GithubContentEntry,
};
use crate::constants::{
    GITHUB_EXAMPLES_API, GITHUB_EXAMPLES_BASE, INSTRUCTION_54_CONTENT, INSTRUCTION_54_FILENAME,
    INSTRUCTION_56_SOL_CONTENT, INSTRUCTION_56_SOL_FILENAME, INSTRUCTION_CONTENT,
    INSTRUCTION_FILENAME, INSTRUCTION_JELI_CONTENT, INSTRUCTION_JELI_FILENAME,
    INSTRUCTION_SEAGULL_CONTENT, INSTRUCTION_SEAGULL_FILENAME, JSDELIVR_EXAMPLES_API,
    JSDELIVR_EXAMPLES_BASE,
};
use crate::error::{CodexxError, Result};
#[cfg(test)]
use crate::remote::fetch_first_valid_with;
use crate::remote::{fetch_first_valid, RemoteSource};
use crate::{now_rfc3339, open_db};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use rusqlite::{params, Connection, TransactionBehavior};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

static BUILTIN_PROMPT_CACHE_LOCK: Mutex<()> = Mutex::new(());
const CATALOG_CDN_KEY: &str = "模板 CDN";
const CATALOG_GITHUB_KEY: &str = "GitHub 模板目录";
const PROMPT_CDN_KEY: &str = "模板 CDN";
const PROMPT_GITHUB_KEY: &str = "GitHub 模板源";

#[derive(Debug, Deserialize)]
struct JsdelivrPackage {
    files: Vec<JsdelivrFile>,
}

#[derive(Debug, Deserialize)]
struct JsdelivrFile {
    name: String,
}

#[derive(Debug)]
struct RemotePromptCatalog {
    prompts: Vec<(String, String)>,
    authoritative: bool,
}

#[derive(Debug, Clone, Copy)]
struct PromptContentTrust<'a> {
    cached: Option<&'a str>,
    bundled: Option<&'a str>,
}

impl PromptContentTrust<'_> {
    fn accepts_cdn(self, content: &str) -> bool {
        match (self.cached, self.bundled) {
            (None, None) => true,
            (Some(cached), None) => content == cached,
            (None, Some(bundled)) => content == bundled,
            (Some(cached), Some(bundled)) => cached == bundled && content == cached,
        }
    }
}

pub(crate) fn bundled_prompt_metas() -> [BundledPromptMeta; 5] {
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
        // Preserve the former remote IDs so cached and active templates upgrade in place.
        BundledPromptMeta {
            id: "github-gpt-5-6-sol-unrestricted-33b86c71",
            filename: INSTRUCTION_56_SOL_FILENAME,
            title: "gpt-5.6-sol-unrestricted.md",
            subtitle: "gpt5.6-sol 破甲提示词",
            badge: "内置",
            content: INSTRUCTION_56_SOL_CONTENT,
        },
        BundledPromptMeta {
            id: "github-3-0-b459e1e8",
            filename: INSTRUCTION_SEAGULL_FILENAME,
            title: "海鸥3.0破甲.md",
            subtitle: "测试生效：海鸥在线，你要整点薯条吗？",
            badge: "内置",
            content: INSTRUCTION_SEAGULL_CONTENT,
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
        "来自在线模板库".to_string(),
        "远程".to_string(),
    )
}

fn builtin_prompt_source_url(filename: &str) -> String {
    prompt_content_source_urls(filename)
        .into_iter()
        .next()
        .unwrap_or_default()
}

pub(crate) fn prompt_content_source_urls(filename: &str) -> Vec<String> {
    let encoded = utf8_percent_encode(filename, NON_ALPHANUMERIC).to_string();
    vec![
        format!("{JSDELIVR_EXAMPLES_BASE}{encoded}"),
        format!("{GITHUB_EXAMPLES_BASE}{encoded}"),
    ]
}

fn cached_builtin_prompt_from_connection(
    conn: &Connection,
    id: &str,
) -> Result<Option<CachedBuiltinPrompt>> {
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

fn cached_builtin_prompt(id: &str) -> Result<Option<CachedBuiltinPrompt>> {
    let conn = open_db()?;
    cached_builtin_prompt_from_connection(&conn, id)
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

fn save_builtin_prompt_cache_on_connection(
    conn: &Connection,
    id: &str,
    filename: &str,
    source_url: &str,
    content: &str,
) -> Result<()> {
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

fn save_builtin_prompt_cache(
    id: &str,
    filename: &str,
    source_url: &str,
    content: &str,
) -> Result<()> {
    let conn = open_db()?;
    save_builtin_prompt_cache_on_connection(&conn, id, filename, source_url, content)
}

fn parse_prompt_content(
    source: &RemoteSource<'_>,
    body: &str,
    trust: PromptContentTrust<'_>,
) -> Result<(String, String)> {
    if source.key == PROMPT_CDN_KEY && !trust.accepts_cdn(body) {
        return Err(CodexxError::Config(
            "CDN 内容变化，需要源站确认".to_string(),
        ));
    }
    Ok((body.to_string(), source.url.to_string()))
}

fn fetch_remote_prompt(filename: &str, trust: PromptContentTrust<'_>) -> Result<(String, String)> {
    let urls = prompt_content_source_urls(filename);
    let sources = [
        RemoteSource::new(PROMPT_CDN_KEY, &urls[0], Some("text/markdown, text/plain")),
        RemoteSource::new(
            PROMPT_GITHUB_KEY,
            &urls[1],
            Some("text/markdown, text/plain"),
        ),
    ];
    fetch_first_valid(&sources, |source, body| {
        parse_prompt_content(source, body, trust)
    })
}

fn finalize_prompt_catalog(mut prompts: Vec<(String, String)>) -> Result<Vec<(String, String)>> {
    prompts.sort_by_key(|prompt| prompt.1.to_ascii_lowercase());
    let mut seen_ids = HashSet::new();
    let mut seen_filenames = HashSet::new();
    prompts.retain(|(id, filename)| {
        seen_ids.insert(id.clone()) && seen_filenames.insert(filename.to_ascii_lowercase())
    });
    if prompts.is_empty() {
        return Err(CodexxError::Config(
            "在线模板目录中没有可用的 Markdown 文件".to_string(),
        ));
    }
    Ok(prompts)
}

pub(crate) fn jsdelivr_prompt_catalog_from_entries(
    entries: Vec<String>,
) -> Result<Vec<(String, String)>> {
    let mut prompts = Vec::new();
    for path in entries {
        let Some(filename) = path.strip_prefix("/examples/") else {
            continue;
        };
        if filename.is_empty()
            || filename.contains('/')
            || filename.contains('\\')
            || !filename.to_ascii_lowercase().ends_with(".md")
        {
            continue;
        }
        prompts.push((stable_remote_prompt_id(filename), filename.to_string()));
    }
    finalize_prompt_catalog(prompts)
}

pub(crate) fn github_prompt_catalog_from_entries(
    entries: Vec<GithubContentEntry>,
) -> Result<Vec<(String, String)>> {
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
        if source_url.trim().is_empty() {
            return Err(CodexxError::Config(format!(
                "GitHub 模板缺少下载地址: {}",
                entry.name
            )));
        }
        let id = stable_remote_prompt_id(&entry.name);
        prompts.push((id, entry.name));
    }
    finalize_prompt_catalog(prompts)
}

fn parse_prompt_catalog(source: &RemoteSource<'_>, body: &str) -> Result<RemotePromptCatalog> {
    if source.key == CATALOG_CDN_KEY {
        let package: JsdelivrPackage = serde_json::from_str(body)
            .map_err(|_| CodexxError::Config("CDN 模板目录格式无效".to_string()))?;
        return Ok(RemotePromptCatalog {
            prompts: jsdelivr_prompt_catalog_from_entries(
                package.files.into_iter().map(|file| file.name).collect(),
            )?,
            authoritative: false,
        });
    }
    if source.key == CATALOG_GITHUB_KEY {
        let entries: Vec<GithubContentEntry> = serde_json::from_str(body)
            .map_err(|_| CodexxError::Config("GitHub 模板目录格式无效".to_string()))?;
        return Ok(RemotePromptCatalog {
            prompts: github_prompt_catalog_from_entries(entries)?,
            authoritative: true,
        });
    }
    Err(CodexxError::Config("未知模板目录来源".to_string()))
}

fn fetch_prompt_catalog() -> Result<RemotePromptCatalog> {
    let sources = [
        RemoteSource::new(
            CATALOG_CDN_KEY,
            JSDELIVR_EXAMPLES_API,
            Some("application/json"),
        ),
        RemoteSource::new(
            CATALOG_GITHUB_KEY,
            GITHUB_EXAMPLES_API,
            Some("application/vnd.github+json"),
        ),
    ];
    fetch_first_valid(&sources, parse_prompt_catalog)
}

fn fetch_github_prompt_catalog() -> Result<RemotePromptCatalog> {
    let sources = [RemoteSource::new(
        CATALOG_GITHUB_KEY,
        GITHUB_EXAMPLES_API,
        Some("application/vnd.github+json"),
    )];
    fetch_first_valid(&sources, parse_prompt_catalog)
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
        sync_issue: None,
        checked_at: Some(cache.checked_at),
        message: message.to_string(),
    }
}

fn unconfirmed_prompt_status(cache: CachedBuiltinPrompt) -> BuiltinPromptStatus {
    let mut status = prompt_status_from_cache(cache, "在线目录仍在更新，暂时保留本地模板");
    status.sync_issue = Some("content".to_string());
    status
}

fn mark_catalog_confirmation_failed(statuses: &mut [BuiltinPromptStatus]) {
    if statuses.iter().any(|status| status.sync_issue.is_some()) {
        return;
    }
    if let Some(status) = statuses.first_mut() {
        status.sync_issue = Some("content".to_string());
        status.message = "在线目录尚未完全确认，稍后会再次同步".to_string();
    }
}

fn refresh_builtin_prompt_from_source(
    id: &str,
    filename: &str,
    bundled: Option<&str>,
) -> Result<BuiltinPromptStatus> {
    let cached_before = cached_builtin_prompt(id)?;
    let (title, subtitle, badge) = prompt_display_meta(filename);
    let trust = PromptContentTrust {
        cached: cached_before.as_ref().map(|cache| cache.content.as_str()),
        bundled,
    };
    match fetch_remote_prompt(filename, trust) {
        Ok((remote, source_url)) => {
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
                source_url,
                cached: true,
                updated,
                content_source: "github".to_string(),
                sync_issue: None,
                checked_at,
                message: if updated {
                    "已更新到最新在线模板"
                } else {
                    "已是最新在线模板"
                }
                .to_string(),
            })
        }
        Err(_) => {
            let cached = cached_before.is_some();
            Ok(BuiltinPromptStatus {
                id: id.to_string(),
                filename: filename.to_string(),
                title,
                subtitle,
                badge,
                source_url: builtin_prompt_source_url(filename),
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
                sync_issue: Some("content".to_string()),
                checked_at: cached_before.map(|item| item.checked_at),
                message: if cached {
                    "在线模板暂时不可用，已使用本地缓存"
                } else if bundled.is_some() {
                    "在线模板暂时不可用，已使用软件内置版本"
                } else {
                    "在线模板暂时不可用，且没有本地副本"
                }
                .to_string(),
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
        sync_issue: None,
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
            .map(|cache| prompt_status_from_cache(cache, "使用上次成功同步的本地缓存"))
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
            "使用上次成功同步的本地缓存",
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
    let mut catalog = match fetch_prompt_catalog() {
        Ok(catalog) => catalog,
        Err(_) => {
            let mut statuses = builtin_prompt_status_inner()?;
            for status in &mut statuses {
                status.sync_issue = Some("catalog".to_string());
                status.message = "在线模板目录暂时不可用，已保留本地内容".to_string();
            }
            return Ok(statuses);
        }
    };
    let cached_before = cached_builtin_prompts()?;
    let mut catalog_confirmation_failed = false;
    if !catalog.authoritative {
        let cdn_ids = catalog
            .prompts
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<HashSet<_>>();
        let cache_may_be_newer = cached_before
            .iter()
            .any(|cache| !cdn_ids.contains(&cache.id));
        let bundled_missing = bundled_prompt_metas()
            .into_iter()
            .any(|meta| !cdn_ids.contains(meta.id));
        if cache_may_be_newer || bundled_missing {
            match fetch_github_prompt_catalog() {
                Ok(authoritative) => catalog = authoritative,
                Err(_) => catalog_confirmation_failed = true,
            }
        }
    }
    let remote_ids = catalog
        .prompts
        .iter()
        .map(|(id, _)| id.clone())
        .collect::<HashSet<_>>();
    let remote_filenames = catalog
        .prompts
        .iter()
        .map(|(_, filename)| filename.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut statuses = Vec::new();
    for (id, filename) in catalog.prompts {
        let bundled = bundled_prompt_meta(&id).map(|meta| meta.content);
        statuses.push(refresh_builtin_prompt_from_source(&id, &filename, bundled)?);
    }
    for meta in bundled_prompt_metas() {
        if !remote_ids.contains(meta.id) {
            statuses.push(bundled_prompt_status(
                meta,
                "在线目录暂未提供该模板，使用软件内置版本",
            ));
        }
    }

    if catalog.authoritative {
        let mut retained_ids = remote_ids.clone();
        if let Some(active_id) = active_remote_builtin_prompt_id() {
            if !remote_ids.contains(&active_id) {
                if let Some(cache) = cached_builtin_prompt(&active_id)? {
                    let mut status = prompt_status_from_cache(
                        cache,
                        "该在线模板已下架，当前配置继续使用本地副本",
                    );
                    status.content_source = "removed".to_string();
                    statuses.push(status);
                }
                retained_ids.insert(active_id);
            }
        }
        prune_builtin_prompt_cache(&retained_ids)?;
    } else {
        let mut seen_ids = remote_ids.clone();
        let mut seen_filenames = remote_filenames;
        for cache in cached_before {
            if !seen_ids.insert(cache.id.clone())
                || !seen_filenames.insert(cache.filename.to_ascii_lowercase())
            {
                continue;
            }
            statuses.push(unconfirmed_prompt_status(cache));
        }
    }
    if catalog_confirmation_failed {
        mark_catalog_confirmation_failed(&mut statuses);
    }
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
    let trust = PromptContentTrust {
        cached: cached.as_ref().map(|item| item.content.as_str()),
        bundled: bundled.map(|item| item.content),
    };
    if let Ok((remote, source_url)) = fetch_remote_prompt(&filename, trust) {
        save_builtin_prompt_cache(id, &filename, &source_url, &remote)?;
        return Ok((
            filename.clone(),
            format!("./{filename}"),
            remote,
            "在线最新".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SOURCES: [RemoteSource<'static>; 2] = [
        RemoteSource::new(PROMPT_CDN_KEY, "https://cdn.test/prompt.md", None),
        RemoteSource::new(PROMPT_GITHUB_KEY, "https://github.test/prompt.md", None),
    ];

    fn initialize_prompt_cache_schema(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE builtin_prompt_cache (
                id TEXT PRIMARY KEY,
                filename TEXT NOT NULL,
                source_url TEXT NOT NULL,
                content TEXT NOT NULL,
                checked_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );",
        )
        .expect("create prompt cache table");
    }

    fn prompt_cache_connection() -> Connection {
        let conn = Connection::open_in_memory().expect("open prompt cache database");
        initialize_prompt_cache_schema(&conn);
        conn
    }

    #[test]
    fn synced_remote_prompt_is_available_from_sqlite_after_restart() {
        let database_path = std::env::temp_dir().join(format!(
            "codexx-prompt-cache-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        let id = stable_remote_prompt_id("online-only.md");
        {
            let conn = Connection::open(&database_path).expect("open prompt cache database");
            initialize_prompt_cache_schema(&conn);
            save_builtin_prompt_cache_on_connection(
                &conn,
                &id,
                "online-only.md",
                "https://example.test/online-only.md",
                "first online content",
            )
            .expect("save downloaded prompt");
        }

        let conn = Connection::open(&database_path).expect("reopen prompt cache database");
        let reopened_cache = cached_builtin_prompts_from_connection(&conn)
            .expect("load cache without a network request");
        let statuses = cached_prompt_fallback_statuses(reopened_cache);
        let remote = statuses
            .iter()
            .find(|status| status.id == id)
            .expect("cached remote prompt remains in the startup catalog");

        assert_eq!(remote.filename, "online-only.md");
        assert_eq!(remote.content_source, "cache");
        assert!(remote.cached);
        drop(conn);
        std::fs::remove_file(database_path).expect("remove prompt cache database");
    }

    #[test]
    fn syncing_remote_prompt_updates_the_existing_sqlite_row() {
        let conn = prompt_cache_connection();
        let id = stable_remote_prompt_id("updated-online.md");
        save_builtin_prompt_cache_on_connection(
            &conn,
            &id,
            "updated-online.md",
            "https://example.test/old.md",
            "old content",
        )
        .expect("save initial prompt");
        save_builtin_prompt_cache_on_connection(
            &conn,
            &id,
            "updated-online.md",
            "https://example.test/current.md",
            "current content",
        )
        .expect("update cached prompt");

        let cached = cached_builtin_prompt_from_connection(&conn, &id)
            .expect("read cached prompt")
            .expect("cached prompt exists");
        let row_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM builtin_prompt_cache WHERE id = ?1",
                [&id],
                |row| row.get(0),
            )
            .expect("count cached rows");

        assert_eq!(row_count, 1);
        assert_eq!(cached.source_url, "https://example.test/current.md");
        assert_eq!(cached.content, "current content");
    }

    #[test]
    fn unchanged_cdn_prompt_does_not_request_github() {
        let mut calls = Vec::new();
        let result = fetch_first_valid_with(
            &TEST_SOURCES,
            |source| {
                calls.push(source.key);
                Ok("local".to_string())
            },
            |source, body| {
                parse_prompt_content(
                    source,
                    body,
                    PromptContentTrust {
                        cached: Some("local"),
                        bundled: None,
                    },
                )
            },
        )
        .expect("unchanged CDN content is accepted");

        assert_eq!(calls, [PROMPT_CDN_KEY]);
        assert_eq!(result.0, "local");
        assert_eq!(result.1, TEST_SOURCES[0].url);
    }

    #[test]
    fn changed_cdn_prompt_uses_github_confirmation() {
        let mut calls = Vec::new();
        let result = fetch_first_valid_with(
            &TEST_SOURCES,
            |source| {
                calls.push(source.key);
                Ok(if source.key == PROMPT_CDN_KEY {
                    "possibly-stale-cdn".to_string()
                } else {
                    "confirmed-origin".to_string()
                })
            },
            |source, body| {
                parse_prompt_content(
                    source,
                    body,
                    PromptContentTrust {
                        cached: Some("local"),
                        bundled: None,
                    },
                )
            },
        )
        .expect("GitHub confirms changed content");

        assert_eq!(calls, [PROMPT_CDN_KEY, PROMPT_GITHUB_KEY]);
        assert_eq!(result.0, "confirmed-origin");
        assert_eq!(result.1, TEST_SOURCES[1].url);
    }

    #[test]
    fn changed_cdn_prompt_is_rejected_when_github_is_unavailable() {
        let result = fetch_first_valid_with(
            &TEST_SOURCES,
            |source| {
                if source.key == PROMPT_CDN_KEY {
                    Ok("possibly-stale-cdn".to_string())
                } else {
                    Err(CodexxError::Config("offline".to_string()))
                }
            },
            |source, body| {
                parse_prompt_content(
                    source,
                    body,
                    PromptContentTrust {
                        cached: Some("local"),
                        bundled: None,
                    },
                )
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn conflicting_cache_and_bundle_force_github_confirmation() {
        let mut calls = Vec::new();
        let result = fetch_first_valid_with(
            &TEST_SOURCES,
            |source| {
                calls.push(source.key);
                Ok(if source.key == PROMPT_CDN_KEY {
                    "old-cache".to_string()
                } else {
                    "new-bundle".to_string()
                })
            },
            |source, body| {
                parse_prompt_content(
                    source,
                    body,
                    PromptContentTrust {
                        cached: Some("old-cache"),
                        bundled: Some("new-bundle"),
                    },
                )
            },
        )
        .expect("conflicting local copies require origin confirmation");

        assert_eq!(calls, [PROMPT_CDN_KEY, PROMPT_GITHUB_KEY]);
        assert_eq!(result.0, "new-bundle");
    }

    #[test]
    fn unconfirmed_cached_prompt_requests_a_later_retry() {
        let status = unconfirmed_prompt_status(CachedBuiltinPrompt {
            id: "cached".to_string(),
            filename: "cached.md".to_string(),
            source_url: "https://example.test/cached.md".to_string(),
            content: "cached".to_string(),
            checked_at: "2026-07-14T00:00:00+08:00".to_string(),
        });

        assert_eq!(status.sync_issue.as_deref(), Some("content"));
    }

    #[test]
    fn failed_catalog_confirmation_always_requests_a_later_retry() {
        let mut statuses = vec![bundled_prompt_status(
            bundled_prompt_metas()[0],
            "在线目录暂未提供该模板，使用软件内置版本",
        )];

        mark_catalog_confirmation_failed(&mut statuses);

        assert!(statuses.iter().any(|status| status.sync_issue.is_some()));
    }
}
