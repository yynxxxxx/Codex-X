import {
  ArrowUpRight,
  CheckCircle2,
  Code2,
  FileText,
  KeyRound,
  RefreshCw,
  Sparkles,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import "../styles/overview-page.css";

export type OverviewLanguage = "zh" | "en";

export type OverviewPageProps = {
  lang: OverviewLanguage;
  model?: string | null;
  configDir: string;
  resolvedCodexDir: string;
  configExists: boolean;
  providerLabel?: string | null;
  instructionEnabled: boolean;
  authExists: boolean;
  configPath?: string | null;
  modelProvider?: string | null;
  instructionPath?: string | null;
  loading: boolean;
  hasUpdate: boolean;
  latestVersion?: string | null;
  onConfigDirChange: (value: string) => void;
  onRefresh: () => void;
  onOpenUpdate: () => void;
};

type StatusCardProps = {
  icon: LucideIcon;
  label: string;
  value: string;
  tone: "success" | "active" | "muted";
};

function StatusCard({ icon: Icon, label, value, tone }: StatusCardProps) {
  return (
    <article className={`cx-overview-status-card cx-overview-status-card--${tone}`}>
      <div className="cx-overview-status-icon" aria-hidden="true">
        <Icon size={19} strokeWidth={1.9} />
      </div>
      <div className="cx-overview-status-copy">
        <span>{label}</span>
        <strong title={value}>{value}</strong>
      </div>
    </article>
  );
}

type ConfigRowProps = {
  label: string;
  value: string;
};

function ConfigRow({ label, value }: ConfigRowProps) {
  return (
    <div className="cx-overview-config-row">
      <span>{label}</span>
      <code title={value}>{value}</code>
    </div>
  );
}

export function OverviewPage({
  lang,
  model,
  configDir,
  resolvedCodexDir,
  configExists,
  providerLabel,
  instructionEnabled,
  authExists,
  configPath,
  modelProvider,
  instructionPath,
  loading,
  hasUpdate,
  latestVersion,
  onConfigDirChange,
  onRefresh,
  onOpenUpdate,
}: OverviewPageProps) {
  const isChinese = lang === "zh";
  const text = isChinese
    ? {
        eyebrow: "CODEX 配置管理器",
        notConfigured: "未配置",
        modelMissing: "未配置模型",
        codexHome: "CODEX_HOME",
        directoryPlaceholder: "留空使用默认目录",
        load: "加载",
        config: "配置文件",
        found: "已找到",
        missing: "未找到",
        provider: "供应商",
        official: "官方配置",
        instruction: "指令提示词",
        enabled: "已启用",
        disabled: "未启用",
        auth: "认证文件",
        authFile: "auth.json",
        noAuth: "未找到",
        updateFound: "发现新版本",
        updateAvailable: (version: string) => `Codex-X ${version} 已发布`,
        viewUpdate: "查看更新",
        liveStatus: "实时状态",
        currentConfig: "当前 Codex 配置",
        on: "提示词已启用",
        off: "提示词未启用",
        directory: "目录",
        configPath: "配置",
        model: "模型",
        providerName: "供应商标识",
        instructionFile: "指令文件",
      }
    : {
        eyebrow: "CODEX CONFIG MANAGER",
        notConfigured: "Not configured",
        modelMissing: "Model not configured",
        codexHome: "CODEX_HOME",
        directoryPlaceholder: "Leave empty for the default directory",
        load: "Load",
        config: "Config file",
        found: "Found",
        missing: "Not found",
        provider: "Provider",
        official: "Official",
        instruction: "Instructions",
        enabled: "Enabled",
        disabled: "Disabled",
        auth: "Auth file",
        authFile: "auth.json",
        noAuth: "Not found",
        updateFound: "New version available",
        updateAvailable: (version: string) => `Codex-X ${version} is available`,
        viewUpdate: "View update",
        liveStatus: "LIVE STATUS",
        currentConfig: "Current Codex configuration",
        on: "Instructions enabled",
        off: "Instructions disabled",
        directory: "Directory",
        configPath: "Config",
        model: "Model",
        providerName: "Provider",
        instructionFile: "Instruction file",
      };

  const displayModel = model?.trim() || text.modelMissing;
  const displayProvider = providerLabel?.trim() || modelProvider?.trim() || text.official;
  const displayModelProvider = modelProvider?.trim() || text.notConfigured;
  const displayDirectory = resolvedCodexDir.trim() || configDir.trim() || text.notConfigured;
  const displayConfigPath = configPath?.trim() || text.notConfigured;
  const displayInstructionPath = instructionPath?.trim() || text.notConfigured;
  const updateVersion = latestVersion?.trim() || "";
  const homeInputValue = configDir || resolvedCodexDir;

  return (
    <section className="cx-overview-page" aria-label={isChinese ? "概览" : "Overview"}>
      <header className="cx-overview-header">
        <div className="cx-overview-heading">
          <p className="cx-overview-eyebrow">
            <span className="cx-overview-live-dot" aria-hidden="true" />
            {text.eyebrow}
          </p>
          <h2 title={displayModel}>{displayModel}</h2>
        </div>

        <div className="cx-overview-home-control">
          <label htmlFor="cx-overview-codex-home">{text.codexHome}</label>
          <input
            id="cx-overview-codex-home"
            type="text"
            value={homeInputValue}
            onChange={(event) => onConfigDirChange(event.target.value)}
            placeholder={text.directoryPlaceholder}
            spellCheck={false}
            aria-label={text.codexHome}
          />
          <button type="button" onClick={onRefresh} disabled={loading}>
            <RefreshCw size={15} strokeWidth={2} className={loading ? "cx-overview-spin" : undefined} aria-hidden="true" />
            {text.load}
          </button>
        </div>
      </header>

      {hasUpdate && (
        <aside className="cx-overview-update-strip" role="status">
          <div className="cx-overview-update-copy">
            <span className="cx-overview-update-dot" aria-hidden="true" />
            <div>
              <strong>{text.updateFound}</strong>
              {updateVersion && <p>{text.updateAvailable(updateVersion)}</p>}
            </div>
          </div>
          <button type="button" onClick={onOpenUpdate}>
            {text.viewUpdate}
            <ArrowUpRight size={15} strokeWidth={2} aria-hidden="true" />
          </button>
        </aside>
      )}

      <div className="cx-overview-status-grid">
        <StatusCard
          icon={FileText}
          label={text.config}
          value={configExists ? text.found : text.missing}
          tone={configExists ? "success" : "muted"}
        />
        <StatusCard
          icon={Code2}
          label={text.provider}
          value={displayProvider}
          tone={modelProvider ? "active" : "muted"}
        />
        <StatusCard
          icon={Sparkles}
          label={text.instruction}
          value={instructionEnabled ? text.enabled : text.disabled}
          tone={instructionEnabled ? "success" : "muted"}
        />
        <StatusCard
          icon={KeyRound}
          label={text.auth}
          value={authExists ? text.authFile : text.noAuth}
          tone={authExists ? "success" : "muted"}
        />
      </div>

      <section className="cx-overview-config-panel">
        <div className="cx-overview-panel-heading">
          <div>
            <p className="cx-overview-section-label">{text.liveStatus}</p>
            <h3>{text.currentConfig}</h3>
          </div>
          <span className={`cx-overview-instruction-pill${instructionEnabled ? " cx-overview-instruction-pill--active" : ""}`}>
            <CheckCircle2 size={14} strokeWidth={2} aria-hidden="true" />
            {instructionEnabled ? text.on : text.off}
          </span>
        </div>

        <div className="cx-overview-config-list">
          <ConfigRow label={text.directory} value={displayDirectory} />
          <ConfigRow label={text.configPath} value={displayConfigPath} />
          <ConfigRow label={text.model} value={displayModel} />
          <ConfigRow label={text.providerName} value={displayModelProvider} />
          <ConfigRow label={text.instructionFile} value={displayInstructionPath} />
        </div>
      </section>
    </section>
  );
}
