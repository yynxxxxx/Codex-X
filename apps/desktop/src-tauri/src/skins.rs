use crate::error::{CodexxError, Result};
use crate::file_io::{ensure_directory, io_err};
use crate::{now_rfc3339, sanitize_id};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;

const MAX_THEME_ZIP_BYTES: usize = 24 * 1024 * 1024;
const MAX_THEME_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
const STATE_FILE: &str = "state.json";

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
