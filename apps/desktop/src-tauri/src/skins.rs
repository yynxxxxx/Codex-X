use crate::error::{CodexxError, Result};
use crate::file_io::{ensure_directory, io_err};
use crate::{now_rfc3339, sanitize_id};
use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tungstenite::{connect, Message};
use zip::write::SimpleFileOptions;

const MAX_THEME_ZIP_BYTES: usize = 24 * 1024 * 1024;
const MAX_THEME_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
const STATE_FILE: &str = "state.json";
const DEFAULT_CDP_PORT: u16 = 9341;
const STYLE_ID: &str = "codex-x-skin-style";
const CHROME_ID: &str = "codex-x-skin-chrome";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkinThemeColors {
    pub(crate) background: String,
    pub(crate) panel: String,
    pub(crate) panel_alt: String,
    pub(crate) accent: String,
    pub(crate) accent_alt: String,
    pub(crate) secondary: String,
    pub(crate) highlight: String,
    pub(crate) text: String,
    pub(crate) muted: String,
    pub(crate) line: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkinThemeManifest {
    pub(crate) schema_version: u32,
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) brand_subtitle: String,
    pub(crate) tagline: String,
    pub(crate) project_prefix: String,
    pub(crate) project_label: String,
    pub(crate) status_text: String,
    pub(crate) quote: String,
    pub(crate) image: String,
    pub(crate) colors: SkinThemeColors,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkinThemeSummary {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) tagline: String,
    pub(crate) quote: String,
    pub(crate) image: String,
    pub(crate) source: String,
    pub(crate) enabled: bool,
    pub(crate) directory: String,
    pub(crate) colors: SkinThemeColors,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkinCenterState {
    pub(crate) skins_dir: String,
    pub(crate) current_theme_id: Option<String>,
    pub(crate) current_theme_path: Option<String>,
    pub(crate) themes: Vec<SkinThemeSummary>,
    pub(crate) runtime: SkinRuntimeStatus,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkinActionResult {
    pub(crate) message: String,
    pub(crate) state: SkinCenterState,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkinExportResult {
    pub(crate) path: String,
    pub(crate) message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkinRuntimeStatus {
    pub(crate) supported: bool,
    pub(crate) active: bool,
    pub(crate) port: u16,
    pub(crate) theme_id: Option<String>,
    pub(crate) message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SkinStateFile {
    current_theme_id: Option<String>,
    updated_at: Option<String>,
}

fn skins_root() -> Result<PathBuf> {
    Ok(crate::paths::app_home()?.join("codex-x-skins"))
}

fn themes_root() -> Result<PathBuf> {
    Ok(skins_root()?.join("themes"))
}

fn current_root() -> Result<PathBuf> {
    Ok(skins_root()?.join("current"))
}

fn exports_root() -> Result<PathBuf> {
    Ok(skins_root()?.join("exports"))
}

fn state_path() -> Result<PathBuf> {
    Ok(skins_root()?.join(STATE_FILE))
}

fn normalize_theme_id(value: &str) -> String {
    let id = sanitize_id(value);
    if id == "provider" {
        "skin-provider".to_string()
    } else {
        id
    }
}

fn validate_image_name(name: &str) -> Result<()> {
    let clean = name.trim();
    if clean.is_empty()
        || clean.contains('/')
        || clean.contains('\\')
        || clean == "."
        || clean == ".."
    {
        return Err(CodexxError::Config(
            "主题图片必须位于主题包根目录".to_string(),
        ));
    }
    let lower = clean.to_ascii_lowercase();
    if ![".png", ".jpg", ".jpeg", ".webp"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
    {
        return Err(CodexxError::Config(
            "主题图片仅支持 PNG/JPEG/WebP".to_string(),
        ));
    }
    Ok(())
}

fn builtin_image_bytes() -> &'static [u8] {
    // 1x1 transparent PNG. Real previews are rendered from theme colors in Codex-X;
    // Dream Skin compatibility only needs a valid local image file.
    &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ]
}

fn theme_colors(
    background: &str,
    panel: &str,
    panel_alt: &str,
    accent: &str,
    accent_alt: &str,
    secondary: &str,
    highlight: &str,
    text: &str,
    muted: &str,
    line: &str,
) -> SkinThemeColors {
    SkinThemeColors {
        background: background.to_string(),
        panel: panel.to_string(),
        panel_alt: panel_alt.to_string(),
        accent: accent.to_string(),
        accent_alt: accent_alt.to_string(),
        secondary: secondary.to_string(),
        highlight: highlight.to_string(),
        text: text.to_string(),
        muted: muted.to_string(),
        line: line.to_string(),
    }
}

fn builtin_themes() -> Vec<SkinThemeManifest> {
    vec![
        SkinThemeManifest {
            schema_version: 1,
            id: "aurora-terminal".to_string(),
            name: "Aurora Terminal".to_string(),
            brand_subtitle: "CODEX-X SKIN".to_string(),
            tagline: "冷光终端，适合长时间工作。".to_string(),
            project_prefix: "选择项目 · ".to_string(),
            project_label: "◉  选择项目".to_string(),
            status_text: "THEME ONLINE".to_string(),
            quote: "BUILD WITH CLARITY".to_string(),
            image: "background.png".to_string(),
            colors: theme_colors(
                "#071116",
                "#0b1a20",
                "#10272c",
                "#38bdf8",
                "#7dd3fc",
                "#22c55e",
                "#6366f1",
                "#e9fff1",
                "#9ebdb3",
                "rgba(56, 189, 248, .30)",
            ),
        },
        SkinThemeManifest {
            schema_version: 1,
            id: "sakura-glass".to_string(),
            name: "Sakura Glass".to_string(),
            brand_subtitle: "CODEX-X SKIN".to_string(),
            tagline: "清透粉白，适合提示词整理。".to_string(),
            project_prefix: "选择项目 · ".to_string(),
            project_label: "◉  选择项目".to_string(),
            status_text: "THEME ONLINE".to_string(),
            quote: "MAKE SOMETHING WONDERFUL".to_string(),
            image: "background.png".to_string(),
            colors: theme_colors(
                "#f7f4f5",
                "#ffffff",
                "#fff7f8",
                "#e25563",
                "#f07a86",
                "#f3a8af",
                "#c93d4c",
                "#2b2224",
                "#8a7a7d",
                "rgba(196, 120, 128, .22)",
            ),
        },
        SkinThemeManifest {
            schema_version: 1,
            id: "neon-night".to_string(),
            name: "Neon Night".to_string(),
            brand_subtitle: "CODEX-X SKIN".to_string(),
            tagline: "紫蓝夜色，适合逆向和调试。".to_string(),
            project_prefix: "选择项目 · ".to_string(),
            project_label: "◉  选择项目".to_string(),
            status_text: "THEME ONLINE".to_string(),
            quote: "TRACE THE SIGNAL".to_string(),
            image: "background.png".to_string(),
            colors: theme_colors(
                "#0f1020",
                "#17182c",
                "#222246",
                "#a78bfa",
                "#c4b5fd",
                "#38bdf8",
                "#f472b6",
                "#f6f3ff",
                "#b7add8",
                "rgba(167, 139, 250, .30)",
            ),
        },
    ]
}

fn write_manifest(dir: &Path, manifest: &SkinThemeManifest) -> Result<()> {
    ensure_directory(dir)?;
    let path = dir.join("theme.json");
    let text = serde_json::to_string_pretty(manifest)
        .map_err(|e| CodexxError::Config(format!("序列化主题失败: {e}")))?;
    fs::write(&path, format!("{text}\n")).map_err(|e| io_err(&path, e))
}

fn install_builtin_theme(manifest: &SkinThemeManifest) -> Result<()> {
    let dir = themes_root()?.join(&manifest.id);
    ensure_directory(&dir)?;
    write_manifest(&dir, manifest)?;
    let image = dir.join(&manifest.image);
    if !image.is_file() {
        fs::write(&image, builtin_image_bytes()).map_err(|e| io_err(&image, e))?;
    }
    Ok(())
}

fn ensure_builtin_themes() -> Result<()> {
    ensure_directory(&themes_root()?)?;
    ensure_directory(&current_root()?)?;
    ensure_directory(&exports_root()?)?;
    for theme in builtin_themes() {
        install_builtin_theme(&theme)?;
    }
    Ok(())
}

fn read_skin_state() -> Result<SkinStateFile> {
    let path = state_path()?;
    if !path.is_file() {
        return Ok(SkinStateFile::default());
    }
    let text = fs::read_to_string(&path).map_err(|e| io_err(&path, e))?;
    serde_json::from_str(&text).map_err(|e| CodexxError::Config(format!("读取皮肤状态失败: {e}")))
}

fn write_skin_state(state: &SkinStateFile) -> Result<()> {
    let path = state_path()?;
    let text = serde_json::to_string_pretty(state)
        .map_err(|e| CodexxError::Config(format!("序列化皮肤状态失败: {e}")))?;
    fs::write(&path, format!("{text}\n")).map_err(|e| io_err(&path, e))
}

fn read_manifest(dir: &Path) -> Result<SkinThemeManifest> {
    let path = dir.join("theme.json");
    let text = fs::read_to_string(&path).map_err(|e| io_err(&path, e))?;
    let manifest: SkinThemeManifest = serde_json::from_str(&text)
        .map_err(|e| CodexxError::Config(format!("解析 theme.json 失败: {e}")))?;
    if manifest.schema_version != 1 {
        return Err(CodexxError::Config(
            "仅支持 schemaVersion = 1 的主题".to_string(),
        ));
    }
    if manifest.name.trim().is_empty() {
        return Err(CodexxError::Config("主题名称不能为空".to_string()));
    }
    validate_image_name(&manifest.image)?;
    let image = dir.join(&manifest.image);
    let meta = fs::metadata(&image).map_err(|e| io_err(&image, e))?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_THEME_IMAGE_BYTES {
        return Err(CodexxError::Config(
            "主题图片必须存在且不超过 16MB".to_string(),
        ));
    }
    Ok(manifest)
}

fn current_manifest() -> Result<(SkinThemeManifest, PathBuf)> {
    let dir = current_root()?;
    let manifest = read_manifest(&dir)?;
    Ok((manifest, dir))
}

fn copy_theme_files(from: &Path, to: &Path, manifest: &SkinThemeManifest) -> Result<()> {
    if to.exists() {
        fs::remove_dir_all(to).map_err(|e| io_err(to, e))?;
    }
    ensure_directory(to)?;
    write_manifest(to, manifest)?;
    let src_image = from.join(&manifest.image);
    let dst_image = to.join(&manifest.image);
    fs::copy(&src_image, &dst_image).map_err(|e| io_err(&dst_image, e))?;
    Ok(())
}

fn theme_summary(
    id: String,
    dir: PathBuf,
    source: &str,
    enabled: bool,
    manifest: SkinThemeManifest,
) -> SkinThemeSummary {
    SkinThemeSummary {
        id,
        name: manifest.name,
        tagline: manifest.tagline,
        quote: manifest.quote,
        image: manifest.image,
        source: source.to_string(),
        enabled,
        directory: dir.to_string_lossy().to_string(),
        colors: manifest.colors,
    }
}

pub(crate) fn get_skin_center_state_inner() -> Result<SkinCenterState> {
    ensure_builtin_themes()?;
    let skin_state = read_skin_state()?;
    let current_id = skin_state.current_theme_id.clone();
    let builtin_ids = builtin_themes()
        .into_iter()
        .map(|theme| theme.id)
        .collect::<std::collections::HashSet<_>>();
    let mut themes = Vec::new();
    let theme_root = themes_root()?;
    for entry in fs::read_dir(&theme_root).map_err(|e| io_err(&theme_root, e))? {
        let entry = entry.map_err(|e| io_err(&theme_root, e))?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        let manifest = match read_manifest(&dir) {
            Ok(manifest) => manifest,
            Err(_) => continue,
        };
        let source = if builtin_ids.contains(&id) {
            "builtin"
        } else {
            "imported"
        };
        themes.push(theme_summary(
            id.clone(),
            dir,
            source,
            current_id.as_deref() == Some(id.as_str()),
            manifest,
        ));
    }
    themes.sort_by(|a, b| {
        let source_rank = |source: &str| if source == "builtin" { 0 } else { 1 };
        source_rank(&a.source)
            .cmp(&source_rank(&b.source))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let current_theme_path = if current_id.is_some() {
        Some(
            current_root()?
                .join("theme.json")
                .to_string_lossy()
                .to_string(),
        )
    } else {
        None
    };
    Ok(SkinCenterState {
        skins_dir: skins_root()?.to_string_lossy().to_string(),
        current_theme_id: current_id,
        current_theme_path,
        themes,
        runtime: skin_runtime_status_inner(),
    })
}

pub(crate) fn enable_skin_theme_inner(id: String) -> Result<SkinActionResult> {
    ensure_builtin_themes()?;
    let id = normalize_theme_id(&id);
    let src = themes_root()?.join(&id);
    if !src.is_dir() {
        return Err(CodexxError::Config(format!("没有找到主题: {id}")));
    }
    let manifest = read_manifest(&src)?;
    copy_theme_files(&src, &current_root()?, &manifest)?;
    write_skin_state(&SkinStateFile {
        current_theme_id: Some(id.clone()),
        updated_at: Some(now_rfc3339()),
    })?;
    Ok(SkinActionResult {
        message: format!("已启用皮肤主题：{}", manifest.name),
        state: get_skin_center_state_inner()?,
    })
}

pub(crate) fn import_skin_theme_zip_inner(
    file_name: String,
    bytes: Vec<u8>,
) -> Result<SkinActionResult> {
    ensure_builtin_themes()?;
    if !file_name.to_ascii_lowercase().ends_with(".zip") {
        return Err(CodexxError::Config("请选择 .zip 主题包".to_string()));
    }
    if bytes.len() > MAX_THEME_ZIP_BYTES {
        return Err(CodexxError::Config("主题包不能超过 24MB".to_string()));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| CodexxError::Config(format!("读取主题 ZIP 失败: {e}")))?;
    let tmp = skins_root()?
        .join("tmp")
        .join(format!("theme-{}", chrono::Local::now().timestamp_millis()));
    ensure_directory(&tmp)?;
    let result = (|| -> Result<String> {
        let mut total_size = 0u64;
        for i in 0..archive.len() {
            let mut file = archive
                .by_index(i)
                .map_err(|e| CodexxError::Config(format!("读取 ZIP 条目失败: {e}")))?;
            if file.is_dir() {
                continue;
            }
            let Some(path) = file.enclosed_name().map(|p| p.to_path_buf()) else {
                continue;
            };
            let normalized = path.to_string_lossy().replace('\\', "/");
            let parts = normalized.split('/').collect::<Vec<_>>();
            if parts
                .iter()
                .any(|part| part.starts_with('.') || *part == "..")
            {
                continue;
            }
            let relative = if parts.len() > 1 && parts[0] != "theme.json" {
                parts[1..].join("/")
            } else {
                normalized
            };
            if relative.contains('/') {
                continue;
            }
            total_size += file.size();
            if total_size > MAX_THEME_ZIP_BYTES as u64 {
                return Err(CodexxError::Config("主题包解压后超过 24MB".to_string()));
            }
            let out = tmp.join(relative);
            let mut data = Vec::new();
            file.read_to_end(&mut data).map_err(|e| io_err(&out, e))?;
            fs::write(&out, data).map_err(|e| io_err(&out, e))?;
        }
        let mut manifest = read_manifest(&tmp)?;
        let id = normalize_theme_id(if manifest.id.trim().is_empty() {
            &manifest.name
        } else {
            &manifest.id
        });
        manifest.id = id.clone();
        let dest = themes_root()?.join(&id);
        copy_theme_files(&tmp, &dest, &manifest)?;
        Ok(manifest.name)
    })();
    let _ = fs::remove_dir_all(&tmp);
    let name = result?;
    Ok(SkinActionResult {
        message: format!("已导入皮肤主题：{name}"),
        state: get_skin_center_state_inner()?,
    })
}

pub(crate) fn export_skin_theme_inner(id: String) -> Result<SkinExportResult> {
    ensure_builtin_themes()?;
    let id = normalize_theme_id(&id);
    let src = themes_root()?.join(&id);
    if !src.is_dir() {
        return Err(CodexxError::Config(format!("没有找到主题: {id}")));
    }
    let manifest = read_manifest(&src)?;
    ensure_directory(&exports_root()?)?;
    let path = exports_root()?.join(format!("{}.zip", normalize_theme_id(&manifest.name)));
    let file = fs::File::create(&path).map_err(|e| io_err(&path, e))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let manifest_text = serde_json::to_string_pretty(&manifest)
        .map_err(|e| CodexxError::Config(format!("序列化主题失败: {e}")))?;
    zip.start_file("theme.json", options)
        .map_err(|e| CodexxError::Config(format!("写入主题包失败: {e}")))?;
    zip.write_all(format!("{manifest_text}\n").as_bytes())
        .map_err(|e| io_err(&path, e))?;
    zip.start_file(&manifest.image, options)
        .map_err(|e| CodexxError::Config(format!("写入主题图片失败: {e}")))?;
    let image = src.join(&manifest.image);
    let mut image_file = fs::File::open(&image).map_err(|e| io_err(&image, e))?;
    std::io::copy(&mut image_file, &mut zip).map_err(|e| io_err(&image, e))?;
    zip.finish()
        .map_err(|e| CodexxError::Config(format!("完成主题包失败: {e}")))?;
    Ok(SkinExportResult {
        path: path.to_string_lossy().to_string(),
        message: format!("已导出主题包：{}", path.display()),
    })
}

pub(crate) fn skin_runtime_status_inner() -> SkinRuntimeStatus {
    #[cfg(target_os = "macos")]
    {
        match runtime_status_macos(DEFAULT_CDP_PORT) {
            Ok(status) => status,
            Err(error) => SkinRuntimeStatus {
                supported: true,
                active: false,
                port: DEFAULT_CDP_PORT,
                theme_id: None,
                message: error.to_string(),
            },
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        SkinRuntimeStatus {
            supported: false,
            active: false,
            port: DEFAULT_CDP_PORT,
            theme_id: None,
            message: "Skin runtime management is planned for this platform.".to_string(),
        }
    }
}

pub(crate) fn apply_skin_theme_inner() -> Result<SkinActionResult> {
    ensure_builtin_themes()?;
    let (manifest, dir) = current_manifest()?;
    #[cfg(target_os = "macos")]
    {
        apply_skin_macos(DEFAULT_CDP_PORT, &manifest, &dir)?;
        Ok(SkinActionResult {
            message: format!("已应用皮肤到 Codex：{}", manifest.name),
            state: get_skin_center_state_inner()?,
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(SkinActionResult {
            message: "当前平台已保存主题，实机应用将在后续版本接入。".to_string(),
            state: get_skin_center_state_inner()?,
        })
    }
}

pub(crate) fn pause_skin_theme_inner() -> Result<SkinActionResult> {
    #[cfg(target_os = "macos")]
    {
        pause_skin_macos(DEFAULT_CDP_PORT)?;
    }
    Ok(SkinActionResult {
        message: "已暂停 Codex 皮肤注入".to_string(),
        state: get_skin_center_state_inner()?,
    })
}

#[cfg(target_os = "macos")]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CdpTarget {
    #[serde(rename = "type")]
    target_type: String,
    url: String,
    web_socket_debugger_url: Option<String>,
}

#[cfg(target_os = "macos")]
fn cdp_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| CodexxError::Config(format!("创建 CDP 客户端失败: {e}")))
}

#[cfg(target_os = "macos")]
fn cdp_http_ready(port: u16) -> bool {
    cdp_client()
        .and_then(|client| {
            client
                .get(format!("http://127.0.0.1:{port}/json/version"))
                .send()
                .map_err(|e| CodexxError::Config(e.to_string()))
                .map(|response| response.status().is_success())
        })
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn list_cdp_targets(port: u16) -> Result<Vec<CdpTarget>> {
    let response = cdp_client()?
        .get(format!("http://127.0.0.1:{port}/json/list"))
        .send()
        .map_err(|e| CodexxError::Config(format!("读取 Codex CDP 目标失败: {e}")))?;
    if !response.status().is_success() {
        return Err(CodexxError::Config(format!(
            "Codex CDP 返回 HTTP {}",
            response.status()
        )));
    }
    response
        .json::<Vec<CdpTarget>>()
        .map_err(|e| CodexxError::Config(format!("解析 Codex CDP 目标失败: {e}")))
}

#[cfg(target_os = "macos")]
fn app_targets(port: u16) -> Result<Vec<CdpTarget>> {
    Ok(list_cdp_targets(port)?
        .into_iter()
        .filter(|target| {
            target.target_type == "page"
                && target.url.starts_with("app://")
                && target.web_socket_debugger_url.is_some()
        })
        .collect())
}

#[cfg(target_os = "macos")]
fn cdp_evaluate(ws_url: &str, expression: &str) -> Result<serde_json::Value> {
    let (mut socket, _) = connect(ws_url)
        .map_err(|e| CodexxError::Config(format!("连接 Codex renderer 失败: {e}")))?;
    let request = serde_json::json!({
        "id": 1,
        "method": "Runtime.evaluate",
        "params": {
            "expression": expression,
            "awaitPromise": true,
            "returnByValue": true,
            "userGesture": false
        }
    });
    socket
        .send(Message::Text(request.to_string()))
        .map_err(|e| CodexxError::Config(format!("发送 CDP 注入命令失败: {e}")))?;
    loop {
        let message = socket
            .read()
            .map_err(|e| CodexxError::Config(format!("读取 CDP 响应失败: {e}")))?;
        let Message::Text(text) = message else {
            continue;
        };
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| CodexxError::Config(format!("解析 CDP 响应失败: {e}")))?;
        if value.get("id").and_then(|id| id.as_i64()) != Some(1) {
            continue;
        }
        if let Some(error) = value.get("error") {
            return Err(CodexxError::Config(format!("CDP 执行失败: {error}")));
        }
        if let Some(details) = value
            .get("result")
            .and_then(|result| result.get("exceptionDetails"))
        {
            return Err(CodexxError::Config(format!(
                "皮肤注入脚本执行失败: {details}"
            )));
        }
        return Ok(value
            .get("result")
            .and_then(|result| result.get("result"))
            .and_then(|result| result.get("value"))
            .cloned()
            .unwrap_or(serde_json::Value::Null));
    }
}

#[cfg(target_os = "macos")]
fn discover_codex_bundle() -> Result<PathBuf> {
    for candidate in [
        "/Applications/ChatGPT.app",
        "/Applications/Codex.app",
        "/Applications/OpenAI Codex.app",
    ] {
        let path = PathBuf::from(candidate);
        if path.join("Contents/Info.plist").is_file() {
            return Ok(path);
        }
    }
    let home = crate::paths::home_dir()?;
    for candidate in [
        home.join("Applications/ChatGPT.app"),
        home.join("Applications/Codex.app"),
    ] {
        if candidate.join("Contents/Info.plist").is_file() {
            return Ok(candidate);
        }
    }
    Err(CodexxError::Config("未找到 Codex Desktop 应用".to_string()))
}

#[cfg(target_os = "macos")]
fn launch_codex_with_cdp(port: u16) -> Result<()> {
    let bundle = discover_codex_bundle()?;
    Command::new("open")
        .arg("-na")
        .arg(&bundle)
        .arg("--args")
        .arg("--remote-debugging-address=127.0.0.1")
        .arg(format!("--remote-debugging-port={port}"))
        .spawn()
        .map_err(|e| io_err(&bundle, e))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn wait_for_cdp(port: u16) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(45);
    while Instant::now() < deadline {
        if cdp_http_ready(port) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(350));
    }
    Err(CodexxError::Config(format!(
        "Codex 未在 127.0.0.1:{port} 打开 CDP"
    )))
}

#[cfg(target_os = "macos")]
fn ensure_cdp(port: u16) -> Result<()> {
    if cdp_http_ready(port) {
        return Ok(());
    }
    launch_codex_with_cdp(port)?;
    wait_for_cdp(port)
}

#[cfg(target_os = "macos")]
fn image_data_url(manifest: &SkinThemeManifest, dir: &Path) -> Result<String> {
    let image = dir.join(&manifest.image);
    let bytes = fs::read(&image).map_err(|e| io_err(&image, e))?;
    let lower = manifest.image.to_ascii_lowercase();
    let mime = if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else {
        "image/png"
    };
    Ok(format!(
        "data:{mime};base64,{}",
        general_purpose::STANDARD.encode(bytes)
    ))
}

#[cfg(target_os = "macos")]
fn skin_css() -> &'static str {
    r#"
html.codex-x-skin {
  --cx-skin-bg: #071116;
  --cx-skin-panel: rgba(11, 26, 32, .72);
  --cx-skin-accent: #38bdf8;
  --cx-skin-secondary: #22c55e;
  --cx-skin-highlight: #6366f1;
  --cx-skin-text: #e9fff1;
  --cx-skin-muted: #9ebdb3;
}
html.codex-x-skin body::before {
  content: "";
  position: fixed;
  inset: 0;
  z-index: 0;
  pointer-events: none;
  background:
    linear-gradient(90deg, rgba(5, 8, 18, .82), rgba(5, 8, 18, .24) 58%, rgba(5, 8, 18, .72)),
    var(--codex-x-skin-art) center / cover no-repeat,
    var(--cx-skin-bg);
}
html.codex-x-skin aside.app-shell-left-panel {
  background: color-mix(in srgb, var(--cx-skin-panel) 88%, transparent) !important;
  border-right-color: color-mix(in srgb, var(--cx-skin-accent) 26%, transparent) !important;
  backdrop-filter: blur(20px) saturate(1.2);
}
html.codex-x-skin main.main-surface {
  background: transparent !important;
}
html.codex-x-skin [role="main"] {
  position: relative;
  z-index: 1;
}
html.codex-x-skin main.main-surface:not(.codex-x-skin-home-shell) [role="main"] {
  background: color-mix(in srgb, var(--cx-skin-panel) 68%, transparent) !important;
  border: 1px solid color-mix(in srgb, var(--cx-skin-accent) 20%, transparent);
  border-radius: 18px;
  backdrop-filter: blur(18px) saturate(1.15);
}
html.codex-x-skin .composer-surface-chrome {
  background: color-mix(in srgb, var(--cx-skin-panel) 82%, transparent) !important;
  border-color: color-mix(in srgb, var(--cx-skin-accent) 22%, transparent) !important;
  box-shadow: 0 16px 40px rgba(0,0,0,.22) !important;
  backdrop-filter: blur(18px) saturate(1.18);
}
#codex-x-skin-chrome {
  position: fixed;
  inset: 0;
  z-index: 1;
  pointer-events: none;
}
#codex-x-skin-chrome .cx-skin-brand {
  position: absolute;
  top: 18px;
  right: 24px;
  display: flex;
  align-items: center;
  gap: 9px;
  padding: 8px 11px;
  border: 1px solid color-mix(in srgb, var(--cx-skin-accent) 28%, transparent);
  border-radius: 999px;
  color: var(--cx-skin-text);
  background: color-mix(in srgb, var(--cx-skin-panel) 64%, transparent);
  box-shadow: 0 10px 24px rgba(0,0,0,.18);
  backdrop-filter: blur(14px);
  font: 600 12px -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;
}
#codex-x-skin-chrome .cx-skin-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--cx-skin-accent);
  box-shadow: 0 0 14px var(--cx-skin-accent);
}
"#
}

#[cfg(target_os = "macos")]
fn injection_payload(manifest: &SkinThemeManifest, dir: &Path) -> Result<String> {
    let css = serde_json::to_string(skin_css())
        .map_err(|e| CodexxError::Config(format!("序列化皮肤 CSS 失败: {e}")))?;
    let data_url = serde_json::to_string(&image_data_url(manifest, dir)?)
        .map_err(|e| CodexxError::Config(format!("序列化皮肤图片失败: {e}")))?;
    let theme = serde_json::to_string(manifest)
        .map_err(|e| CodexxError::Config(format!("序列化皮肤主题失败: {e}")))?;
    Ok(format!(
        r##"((cssText, artUrl, theme) => {{
  const STYLE_ID = "{STYLE_ID}";
  const CHROME_ID = "{CHROME_ID}";
  const STATE_KEY = "__CODEX_X_SKIN_STATE__";
  const previous = window[STATE_KEY];
  if (previous?.observer) previous.observer.disconnect();
  if (previous?.timer) clearInterval(previous.timer);
  const root = document.documentElement;
  const applyVars = () => {{
    const colors = theme.colors || {{}};
    root.classList.add("codex-x-skin");
    root.style.setProperty("--codex-x-skin-art", `url("${{artUrl}}")`);
    root.style.setProperty("--cx-skin-bg", colors.background || "#071116");
    root.style.setProperty("--cx-skin-panel", colors.panel || "#0b1a20");
    root.style.setProperty("--cx-skin-accent", colors.accent || "#38bdf8");
    root.style.setProperty("--cx-skin-secondary", colors.secondary || "#22c55e");
    root.style.setProperty("--cx-skin-highlight", colors.highlight || "#6366f1");
    root.style.setProperty("--cx-skin-text", colors.text || "#e9fff1");
    root.style.setProperty("--cx-skin-muted", colors.muted || "#9ebdb3");
  }};
  const ensure = () => {{
    applyVars();
    let style = document.getElementById(STYLE_ID);
    if (!style) {{
      style = document.createElement("style");
      style.id = STYLE_ID;
      document.head.appendChild(style);
    }}
    if (style.textContent !== cssText) style.textContent = cssText;
    const home = document.querySelector('[data-testid="home-icon"]')?.closest('[role="main"]')
      || [...document.querySelectorAll('[role="main"]')].find((node) => node.querySelector('[data-feature="game-source"]'));
    document.querySelector("main.main-surface")?.classList.toggle("codex-x-skin-home-shell", Boolean(home));
    let chrome = document.getElementById(CHROME_ID);
    if (!chrome) {{
      chrome = document.createElement("div");
      chrome.id = CHROME_ID;
      chrome.setAttribute("aria-hidden", "true");
      chrome.innerHTML = '<div class="cx-skin-brand"><span class="cx-skin-dot"></span><span></span></div>';
      document.body.appendChild(chrome);
    }}
    const label = chrome.querySelector(".cx-skin-brand span:last-child");
    if (label) label.textContent = theme.name || "Codex-X Skin";
  }};
  const cleanup = () => {{
    root.classList.remove("codex-x-skin");
    root.style.removeProperty("--codex-x-skin-art");
    ["--cx-skin-bg","--cx-skin-panel","--cx-skin-accent","--cx-skin-secondary","--cx-skin-highlight","--cx-skin-text","--cx-skin-muted"].forEach((key) => root.style.removeProperty(key));
    document.querySelector("main.main-surface")?.classList.remove("codex-x-skin-home-shell");
    document.getElementById(STYLE_ID)?.remove();
    document.getElementById(CHROME_ID)?.remove();
    const state = window[STATE_KEY];
    if (state?.observer) state.observer.disconnect();
    if (state?.timer) clearInterval(state.timer);
    delete window[STATE_KEY];
    return true;
  }};
  const observer = new MutationObserver(() => ensure());
  observer.observe(root, {{ childList: true, subtree: true, attributes: true, attributeFilter: ["class", "style", "data-theme", "data-appearance"] }});
  const timer = setInterval(ensure, 4000);
  window[STATE_KEY] = {{ observer, timer, cleanup, themeId: theme.id || null, themeName: theme.name || "" }};
  ensure();
  return {{ installed: true, themeId: theme.id || null, themeName: theme.name || "" }};
}})({css}, {data_url}, {theme})"##
    ))
}

#[cfg(target_os = "macos")]
fn cleanup_payload() -> &'static str {
    r#"(() => {
  const state = window.__CODEX_X_SKIN_STATE__;
  if (state?.cleanup) return state.cleanup();
  document.documentElement.classList.remove("codex-x-skin");
  document.documentElement.style.removeProperty("--codex-x-skin-art");
  document.getElementById("codex-x-skin-style")?.remove();
  document.getElementById("codex-x-skin-chrome")?.remove();
  delete window.__CODEX_X_SKIN_STATE__;
  return true;
})()"#
}

#[cfg(target_os = "macos")]
fn probe_payload() -> &'static str {
    r#"(() => ({
  installed: document.documentElement.classList.contains("codex-x-skin") && Boolean(window.__CODEX_X_SKIN_STATE__),
  themeId: window.__CODEX_X_SKIN_STATE__?.themeId || null
}))()"#
}

#[cfg(target_os = "macos")]
fn apply_skin_macos(port: u16, manifest: &SkinThemeManifest, dir: &Path) -> Result<()> {
    ensure_cdp(port)?;
    let targets = app_targets(port)?;
    if targets.is_empty() {
        return Err(CodexxError::Config(
            "未找到 Codex renderer 页面".to_string(),
        ));
    }
    let payload = injection_payload(manifest, dir)?;
    for target in targets {
        if let Some(ws_url) = target.web_socket_debugger_url {
            cdp_evaluate(&ws_url, &payload)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn pause_skin_macos(port: u16) -> Result<()> {
    if !cdp_http_ready(port) {
        return Ok(());
    }
    for target in app_targets(port)? {
        if let Some(ws_url) = target.web_socket_debugger_url {
            let _ = cdp_evaluate(&ws_url, cleanup_payload());
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn runtime_status_macos(port: u16) -> Result<SkinRuntimeStatus> {
    if !cdp_http_ready(port) {
        return Ok(SkinRuntimeStatus {
            supported: true,
            active: false,
            port,
            theme_id: None,
            message: "Codex CDP 未启动".to_string(),
        });
    }
    for target in app_targets(port)? {
        if let Some(ws_url) = target.web_socket_debugger_url {
            let value = cdp_evaluate(&ws_url, probe_payload())?;
            let active = value
                .get("installed")
                .and_then(|item| item.as_bool())
                .unwrap_or(false);
            let theme_id = value
                .get("themeId")
                .and_then(|item| item.as_str())
                .map(ToString::to_string);
            return Ok(SkinRuntimeStatus {
                supported: true,
                active,
                port,
                theme_id,
                message: if active {
                    "Codex 皮肤运行中".to_string()
                } else {
                    "Codex CDP 已连接，皮肤未注入".to_string()
                },
            });
        }
    }
    Ok(SkinRuntimeStatus {
        supported: true,
        active: false,
        port,
        theme_id: None,
        message: "Codex CDP 已启动，但未找到 renderer".to_string(),
    })
}
