import type { CSSProperties, ChangeEvent, RefObject } from "react";
import {
  Check,
  Download,
  FolderArchive,
  Loader2,
  Palette,
  Play,
  Upload,
} from "lucide-react";

import { Button, StatusBadge, cx } from "../components/ui";
import type { Lang, SkinCenterState, SkinThemeSummary } from "../types";
import "../styles/skins-page.css";

type MaybeAsyncAction = () => void | Promise<void>;

export type SkinsPageProps = {
  lang: Lang;
  state: SkinCenterState | null;
  actionBusy: string;
  zipInputRef: RefObject<HTMLInputElement>;
  onLoad: MaybeAsyncAction;
  onImportZip: (file?: File | null) => void | Promise<void>;
  onEnableTheme: (id: string) => void | Promise<void>;
  onExportTheme: (id: string) => void | Promise<void>;
};

function getCopy(lang: Lang) {
  return lang === "zh"
    ? {
        eyebrow: "SKIN CENTER",
        title: "皮肤中心",
        description: "兼容 Codex Dream Skin 的 theme.json + 图片格式。先管理主题包、切换当前主题，后续接入图片生成与实机注入。",
        refresh: "刷新",
        importZip: "导入主题包",
        loading: "正在读取皮肤库...",
        builtin: "内置",
        imported: "导入",
        enabled: "已启用",
        enable: "启用",
        export: "导出",
        applying: "启用中",
        exporting: "导出中",
        current: "当前主题",
        storage: "皮肤目录",
        currentPath: "当前 theme.json",
        noCurrent: "尚未启用主题",
        noThemes: "还没有主题。导入 .zip 主题包或使用内置主题开始。",
        zipHint: "主题包结构：theme.json + background.png/jpg/webp",
        statusReady: "主题库已就绪",
      }
    : {
        eyebrow: "SKIN CENTER",
        title: "Skin Center",
        description: "Compatible with the Codex Dream Skin theme.json + image format. Manage packs and switch the current theme first; image generation and live injection can follow.",
        refresh: "Refresh",
        importZip: "Import theme ZIP",
        loading: "Loading skin library...",
        builtin: "Built-in",
        imported: "Imported",
        enabled: "Enabled",
        enable: "Enable",
        export: "Export",
        applying: "Enabling",
        exporting: "Exporting",
        current: "Current theme",
        storage: "Skin directory",
        currentPath: "Current theme.json",
        noCurrent: "No theme enabled yet",
        noThemes: "No themes yet. Import a .zip theme pack or start with a built-in theme.",
        zipHint: "Theme pack: theme.json + background.png/jpg/webp",
        statusReady: "Theme library ready",
      };
}

function run(action: MaybeAsyncAction) {
  void action();
}

function sourceLabel(theme: SkinThemeSummary, copy: ReturnType<typeof getCopy>) {
  return theme.source === "builtin" ? copy.builtin : copy.imported;
}

function ThemeCard({
  theme,
  copy,
  busy,
  onEnableTheme,
  onExportTheme,
}: {
  theme: SkinThemeSummary;
  copy: ReturnType<typeof getCopy>;
  busy: string;
  onEnableTheme: SkinsPageProps["onEnableTheme"];
  onExportTheme: SkinsPageProps["onExportTheme"];
}) {
  const enabling = busy === `skin:${theme.id}`;
  const exporting = busy === `skinExport:${theme.id}`;
  const disabled = Boolean(busy);
  const style = {
    "--skin-bg": theme.colors.background,
    "--skin-panel": theme.colors.panel,
    "--skin-accent": theme.colors.accent,
    "--skin-secondary": theme.colors.secondary,
    "--skin-highlight": theme.colors.highlight,
    "--skin-text": theme.colors.text,
    "--skin-muted": theme.colors.muted,
  } as CSSProperties;

  return (
    <article className={cx("cx-skins-card", theme.enabled && "cx-skins-card--enabled")} style={style}>
      <div className="cx-skins-preview" aria-hidden="true">
        <div className="cx-skins-preview-sidebar" />
        <div className="cx-skins-preview-main">
          <span />
          <strong />
          <p />
          <div>
            <i />
            <i />
            <i />
          </div>
        </div>
      </div>

      <div className="cx-skins-card-body">
        <div className="cx-skins-card-title">
          <div>
            <strong>{theme.name}</strong>
            <span>{theme.tagline}</span>
          </div>
          <StatusBadge tone={theme.enabled ? "success" : "neutral"} dot={theme.enabled}>
            {theme.enabled ? copy.enabled : sourceLabel(theme, copy)}
          </StatusBadge>
        </div>
        <p>{theme.quote}</p>
      </div>

      <div className="cx-skins-swatches" aria-hidden="true">
        <span style={{ background: theme.colors.accent }} />
        <span style={{ background: theme.colors.secondary }} />
        <span style={{ background: theme.colors.highlight }} />
        <span style={{ background: theme.colors.panel }} />
      </div>

      <div className="cx-skins-card-actions">
        <Button
          variant={theme.enabled ? "secondary" : "primary"}
          onClick={() => run(() => onEnableTheme(theme.id))}
          disabled={disabled || theme.enabled}
          icon={enabling ? <Loader2 className="cx-skins-spin" size={15} /> : theme.enabled ? <Check size={15} /> : <Play size={15} />}
        >
          {enabling ? copy.applying : theme.enabled ? copy.enabled : copy.enable}
        </Button>
        <Button
          variant="secondary"
          onClick={() => run(() => onExportTheme(theme.id))}
          disabled={disabled}
          icon={exporting ? <Loader2 className="cx-skins-spin" size={15} /> : <Download size={15} />}
        >
          {exporting ? copy.exporting : copy.export}
        </Button>
      </div>
    </article>
  );
}

export function SkinsPage({
  lang,
  state,
  actionBusy,
  zipInputRef,
  onLoad,
  onImportZip,
  onEnableTheme,
  onExportTheme,
}: SkinsPageProps) {
  const copy = getCopy(lang);
  const themes = state?.themes ?? [];
  const current = themes.find((theme) => theme.enabled);
  const loading = actionBusy === "loadSkins";

  const handleZipChange = (event: ChangeEvent<HTMLInputElement>) => {
    void onImportZip(event.currentTarget.files?.[0] ?? null);
  };

  return (
    <section className="cx-skins-page">
      <input
        ref={zipInputRef}
        className="cx-skins-file-input"
        type="file"
        accept=".zip,application/zip"
        onChange={handleZipChange}
      />

      <header className="cx-skins-header">
        <div className="cx-skins-heading">
          <p><Palette size={15} aria-hidden="true" />{copy.eyebrow}</p>
          <h2>{copy.title}</h2>
          <span>{copy.description}</span>
        </div>
        <div className="cx-skins-actions">
          <Button
            variant="secondary"
            onClick={() => run(onLoad)}
            disabled={Boolean(actionBusy)}
            icon={loading ? <Loader2 className="cx-skins-spin" size={15} /> : <FolderArchive size={15} />}
          >
            {copy.refresh}
          </Button>
          <Button
            variant="primary"
            onClick={() => zipInputRef.current?.click()}
            disabled={Boolean(actionBusy)}
            icon={actionBusy === "importSkinZip" ? <Loader2 className="cx-skins-spin" size={15} /> : <Upload size={15} />}
          >
            {copy.importZip}
          </Button>
        </div>
      </header>

      <section className="cx-skins-status" aria-label={copy.statusReady}>
        <div>
          <span>{copy.current}</span>
          <strong>{current?.name || copy.noCurrent}</strong>
        </div>
        <div>
          <span>{copy.storage}</span>
          <strong title={state?.skinsDir || ""}>{state?.skinsDir || "-"}</strong>
        </div>
        <div>
          <span>{copy.currentPath}</span>
          <strong title={state?.currentThemePath || ""}>{state?.currentThemePath || "-"}</strong>
        </div>
      </section>

      <div className="cx-skins-hint">{copy.zipHint}</div>

      {themes.length === 0 ? (
        <div className="cx-skins-empty">
          <Palette size={24} strokeWidth={1.7} aria-hidden="true" />
          <span>{loading ? copy.loading : copy.noThemes}</span>
        </div>
      ) : (
        <div className="cx-skins-grid">
          {themes.map((theme) => (
            <ThemeCard
              key={theme.id}
              theme={theme}
              copy={copy}
              busy={actionBusy}
              onEnableTheme={onEnableTheme}
              onExportTheme={onExportTheme}
            />
          ))}
        </div>
      )}
    </section>
  );
}
