import React from "react";
import type { ReactNode } from "react";
import {
  Blocks,
  Download,
  FileCode2,
  History,
  Info,
  LayoutDashboard,
  LoaderCircle,
  Moon,
  RotateCcw,
  Settings,
  Sparkles,
  Sun,
  TerminalSquare,
  Zap,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { AppUpdaterPhase } from "../appUpdater";
import { IconButton } from "./ui/IconButton";

export type AppLanguage = "zh" | "en";
export type AppTheme = "light" | "dark";

export type AppTab =
  | "dashboard"
  | "provider"
  | "sessions"
  | "skillsMcp"
  | "instruction"
  | "toml"
  | "settings"
  | "about";

type NavItem = {
  id: AppTab;
  icon: LucideIcon;
  label: Record<AppLanguage, string>;
};

const NAV_ITEMS: readonly NavItem[] = [
  { id: "dashboard", icon: LayoutDashboard, label: { zh: "概览", en: "Overview" } },
  { id: "provider", icon: Zap, label: { zh: "供应商", en: "Providers" } },
  { id: "sessions", icon: History, label: { zh: "会话管理", en: "Sessions" } },
  { id: "skillsMcp", icon: Blocks, label: { zh: "技能和MCP", en: "Skills & MCP" } },
  { id: "instruction", icon: Sparkles, label: { zh: "指令提示词", en: "Prompts" } },
  { id: "toml", icon: FileCode2, label: { zh: "TOML", en: "TOML" } },
  { id: "settings", icon: Settings, label: { zh: "设置", en: "Settings" } },
  { id: "about", icon: Info, label: { zh: "关于", en: "About" } },
] as const;

export type AppShellProps = {
  activeTab: AppTab;
  onTabChange: (tab: AppTab) => void;
  lang: AppLanguage;
  theme: AppTheme;
  onToggleTheme: () => void;
  codexVersion?: string | null;
  appVersion?: string | null;
  hasUpdate?: boolean;
  updatePhase?: AppUpdaterPhase;
  onOpenUpdate?: () => void;
  isMacRuntime?: boolean;
  children: ReactNode;
  sidebarFooter?: ReactNode;
  className?: string;
  contentClassName?: string;
};

export function AppShell({
  activeTab,
  onTabChange,
  lang,
  theme,
  onToggleTheme,
  codexVersion,
  appVersion,
  hasUpdate = false,
  updatePhase = "idle",
  onOpenUpdate,
  isMacRuntime = false,
  children,
  sidebarFooter,
  className,
  contentClassName,
}: AppShellProps) {
  const shellClassName = [
    "cx-app-shell",
    isMacRuntime ? "cx-app-shell--mac" : "",
    className,
  ].filter(Boolean).join(" ");
  const contentClasses = ["cx-app-content", contentClassName].filter(Boolean).join(" ");

  const navigationLabel = lang === "zh" ? "主导航" : "Main navigation";
  const codexVersionLabel = codexVersion || (lang === "zh" ? "未检测到" : "Not detected");
  const themeLabel = lang === "zh" ? "外观模式" : "Appearance";
  const themeValue = theme === "dark"
    ? (lang === "zh" ? "深色" : "Dark")
    : (lang === "zh" ? "浅色" : "Light");
  const toggleThemeLabel = theme === "dark"
    ? (lang === "zh" ? "切换为浅色模式" : "Switch to light mode")
    : (lang === "zh" ? "切换为深色模式" : "Switch to dark mode");
  const ThemeIcon = theme === "dark" ? Moon : Sun;
  const updateActionState = updatePhase === "downloading"
    || updatePhase === "installing"
    || updatePhase === "ready"
    || updatePhase === "available"
    ? updatePhase
    : hasUpdate
      ? "available"
      : null;
  const updateActionLabel = updateActionState === "downloading"
    ? (lang === "zh" ? "查看更新下载进度" : "View update download progress")
    : updateActionState === "installing"
      ? (lang === "zh" ? "查看更新安装进度" : "View update installation progress")
      : updateActionState === "ready"
        ? (lang === "zh" ? "打开更新窗口并重新启动" : "Open update window and restart")
        : (lang === "zh" ? "查看并下载更新" : "View and download update");
  const UpdateActionIcon = updateActionState === "downloading" || updateActionState === "installing"
    ? LoaderCircle
    : updateActionState === "ready"
      ? RotateCcw
      : Download;

  return (
    <div className={shellClassName}>
      {isMacRuntime && (
        <div className="cx-window-drag-region" data-tauri-drag-region aria-hidden="true" />
      )}

      <aside className="cx-sidebar">
        <div className="cx-brand">
          <div className="cx-brand-mark" aria-hidden="true">X</div>
          <div className="cx-brand-copy">
            <div className="cx-brand-title-row">
              <h1>Codex-X</h1>
              {appVersion && <span className="cx-app-version">v{appVersion.replace(/^v/i, "")}</span>}
            </div>
            <p>{lang === "zh" ? "切换 · 指令 · 配置" : "Switch · Instruct · Config"}</p>
          </div>
          {updateActionState && onOpenUpdate && (
            <IconButton
              className="cx-brand-update"
              variant="ghost"
              size="md"
              icon={<UpdateActionIcon aria-hidden="true" />}
              label={updateActionLabel}
              title={updateActionLabel}
              onClick={onOpenUpdate}
              aria-busy={updateActionState === "downloading" || updateActionState === "installing"}
              data-update-state={updateActionState}
            />
          )}
        </div>

        <nav className="cx-sidebar-nav" aria-label={navigationLabel}>
          {NAV_ITEMS.map((item) => {
            const Icon = item.icon;
            const isActive = activeTab === item.id;

            return (
              <button
                key={item.id}
                type="button"
                className={`cx-nav-item${isActive ? " cx-nav-item--active" : ""}`}
                onClick={() => React.startTransition(() => onTabChange(item.id))}
                aria-current={isActive ? "page" : undefined}
                title={item.label[lang]}
              >
                <span className="cx-nav-active-mark" aria-hidden="true" />
                <Icon size={18} strokeWidth={1.9} aria-hidden="true" />
                <span className="cx-nav-label">{item.label[lang]}</span>
              </button>
            );
          })}
        </nav>

        <div className="cx-sidebar-footer">
          {sidebarFooter}
          <div className="cx-codex-version" title={`Codex CLI ${codexVersionLabel}`}>
            <TerminalSquare size={17} strokeWidth={1.8} aria-hidden="true" />
            <span>
              <small>Codex CLI</small>
              <strong>{codexVersionLabel}</strong>
            </span>
          </div>
          <button
            type="button"
            className="cx-theme-button"
            onClick={onToggleTheme}
            role="switch"
            aria-checked={theme === "dark"}
            aria-label={toggleThemeLabel}
            title={toggleThemeLabel}
          >
            <ThemeIcon size={17} strokeWidth={1.9} aria-hidden="true" />
            <span className="cx-theme-copy">
              <span>{themeLabel}</span>
              <strong>{themeValue}</strong>
            </span>
            <span className="cx-theme-switch" aria-hidden="true">
              <span className="cx-theme-switch-thumb" />
            </span>
          </button>
        </div>
      </aside>

      <main className="cx-app-main">
        <div className={contentClasses}>
          <div className="cx-app-content-inner">{children}</div>
        </div>
      </main>
    </div>
  );
}
