import React from "react";
import ReactDOM from "react-dom/client";
import {
  Activity,
  CheckCircle2,
  ChevronRight,
  Code2,
  Download,
  ExternalLink,
  AlertCircle,
  Eye,
  EyeOff,
  FileCode2,
  Globe2,
  History,
  Info,
  KeyRound,
  Layers3,
  Loader2,
  PencilLine,
  Plus,
  Power,
  RefreshCw,
  RotateCcw,
  Search,
  Settings,
  Sparkles,
  TerminalSquare,
  Trash2,
  Upload,
  Zap,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import "./styles.css";

type Lang = "zh" | "en";
type ProviderMode = "list" | "form" | "official";
type InstructionMode = "list" | "form";
type Tab = "dashboard" | "provider" | "sessions" | "skillsMcp" | "instruction" | "toml" | "settings" | "about";

type InstructionTemplate = {
  id: string;
  filename: string;
  title: string;
  subtitle: string;
  badge: string;
};

type ProviderSummary = {
  id: string;
  name?: string;
  baseUrl?: string;
  wireApi?: string;
  requiresOpenaiAuth?: boolean;
  isCurrent: boolean;
};

type SavedProvider = {
  id: string;
  providerName: string;
  baseUrl: string;
  model: string;
  apiKey?: string;
  tomlConfig?: string;
  wireApi: string;
  requiresOpenaiAuth: boolean;
};

type SavedPrompt = {
  id: string;
  title: string;
  filename: string;
  content: string;
};

type BuiltinPromptStatus = {
  id: string;
  filename: string;
  sourceUrl: string;
  cached: boolean;
  updated: boolean;
  contentSource: string;
  checkedAt?: string | null;
  message: string;
};

type BackupEntry = {
  id: string;
  action: string;
  createdAt: string;
  path: string;
  hadConfig: boolean;
  hadAuth: boolean;
};

type CodexState = {
  codexDir: string;
  configPath: string;
  authPath: string;
  configExists: boolean;
  authExists: boolean;
  officialAuthAvailable: boolean;
  model?: string;
  modelProvider?: string;
  instructionFile?: string;
  instructionEnabled: boolean;
  providers: ProviderSummary[];
  configText: string;
  authPreview?: unknown;
  authText: string;
  lastBackup?: BackupEntry;
};

type ActionResult = {
  ok: boolean;
  message: string;
  backupId?: string;
  state: CodexState;
};

type ImportResult = {
  imported: number;
  skipped: number;
  warnings: string[];
  providers: SavedProvider[];
};

type OfficialAuthCandidate = {
  authJson: string;
  model?: string;
  source: string;
};

type AboutInfo = {
  appVersion: string;
  codexVersion?: string;
  codexDir: string;
  projectUrl: string;
  githubRepo: string;
};

type ReleaseInfo = {
  status: "idle" | "checking" | "ok" | "error";
  latestVersion?: string;
  htmlUrl?: string;
  assetName?: string;
  body?: string;
  message?: string;
  hasUpdate?: boolean;
};

type ProviderConnectionResult = {
  ok: boolean;
  status?: number | null;
  message: string;
  durationMs: number;
};


type SessionSyncStatus = {
  codexDir: string;
  targetProvider: string;
  rolloutFiles: number;
  sessionMetaCount: number;
  mismatchedRollouts: number;
  mismatchedSessionMeta: number;
  sqliteDbs: number;
  sqliteThreads: number;
  mismatchedThreads: number;
  needsSync: boolean;
  backupDir?: string | null;
  warnings: string[];
  sessions: SessionPreview[];
};

type SessionPreview = {
  id: string;
  title: string;
  modelProvider?: string | null;
  model?: string | null;
  cwd?: string | null;
  rolloutPath?: string | null;
  updatedAtMs?: number | null;
  archived: boolean;
  hasUserEvent: boolean;
  needsSync: boolean;
};

type SessionSyncResult = {
  status: SessionSyncStatus;
  updatedRollouts: number;
  updatedThreads: number;
  backupDir: string;
};

type ManagedSkill = {
  id: string;
  name: string;
  description?: string | null;
  directory: string;
  enabled: boolean;
  source: string;
  path: string;
  contentHash?: string | null;
  updateStatus: string;
};

type ManagedMcpServer = {
  id: string;
  name: string;
  transport: string;
  enabled: boolean;
  source: string;
  summary: string;
  command?: string | null;
  url?: string | null;
  configJson: unknown;
};

type SkillsMcpState = {
  codexDir: string;
  codexSkillsDir: string;
  disabledSkillsDir: string;
  skills: ManagedSkill[];
  mcpServers: ManagedMcpServer[];
  warnings: string[];
};

type SkillsMcpActionResult = {
  importedSkills: number;
  importedMcp: number;
  message: string;
  state: SkillsMcpState;
};

type SkillsMcpImportPreview = {
  skills: ManagedSkill[];
  mcpServers: ManagedMcpServer[];
  warnings: string[];
};

type DiagnosticItem = {
  key: string;
  label: string;
  path?: string | null;
  status: "ok" | "missing" | "manual" | string;
  message: string;
};

type StartupDiagnostics = {
  codexDir: string;
  needsManualSelect: boolean;
  summary: string;
  items: DiagnosticItem[];
};

const INSTRUCTION_RELATIVE_UI = "./gpt5.5-unrestricted.md";
const LANG_KEY = "codexx.lang";
const STARTUP_WIZARD_SEEN_KEY = "codexx.startupWizardSeen";
const AUTO_SESSION_SYNC_KEY = "codexx.autoSessionSync";
const ACTIVE_PROVIDER_KEY = "codexx.activeProviderId";
const FALLBACK_GITHUB_REPO = "yynxxxxx/Codex-X";

const instructionTemplates: InstructionTemplate[] = [
  {
    id: "gpt5.5-unrestricted",
    filename: "gpt5.5-unrestricted.md",
    title: "gpt-5.5 unrestricted 破甲",
    subtitle: "方法：先让ai分析项目，分析完之后发【不直白的逆向】命令",
    badge: "推荐",
  },
  {
    id: "gpt5.4-unrestricted",
    filename: "gpt5.4-unrestricted.md",
    title: "gpt-5.4 unrestricted 破甲",
    subtitle: "方法：先让ai分析项目，分析完之后发【不直白的逆向】命令",
    badge: "兼容",
  },
  {
    id: "gpt5.5-jeli",
    filename: "gpt5.5-jeli.md",
    title: "gpt5.5-jeli.md",
    subtitle: "gpt5.5 大白话（80%场景）破甲",
    badge: "通用",
  },
];

const bootHints = {
  zh: ["检测 Codex 环境", "加载本地配置", "同步界面状态", "准备进入 Codex-X"],
  en: ["Checking Codex environment", "Loading local config", "Syncing UI state", "Preparing Codex-X"],
};

const defaultProviderForm: SavedProvider = {
  id: "magicai",
  providerName: "MagicAI",
  baseUrl: "https://sky1818.com",
  model: "gpt-5.5",
  apiKey: "",
  tomlConfig: "",
  wireApi: "responses",
  requiresOpenaiAuth: false,
};

const blankProviderForm: SavedProvider = {
  id: "",
  providerName: "",
  baseUrl: "",
  model: "gpt-5.5",
  apiKey: "",
  tomlConfig: "",
  wireApi: "responses",
  requiresOpenaiAuth: false,
};

const blankPromptForm: SavedPrompt = {
  id: "",
  title: "",
  filename: "",
  content: "",
};

const dict = {
  zh: {
    appSubtitle: "切换 · 指令 · 配置",
    manager: "Codex 配置管理器",
    load: "加载",
    refresh: "刷新",
    nav: {
      dashboard: "概览",
      provider: "供应商",
      sessions: "会话管理",
      skillsMcp: "技能和MCP",
      instruction: "指令提示词",
      toml: "TOML",
      settings: "设置",
      about: "关于",
    },
    dashboard: {
      config: "配置文件",
      found: "已找到",
      missing: "不存在",
      provider: "供应商",
      instruction: "指令提示词状态",
      enabled: "已启用",
      disabled: "未启用",
      auth: "认证文件",
      currentConfig: "当前 Codex 配置",
      liveStatus: "实时状态",
      dir: "目录",
      configPath: "配置",
      model: "模型",
      providerName: "供应商",
      instructionFile: "指令文件",
      notSet: "未设置",
      officialDefault: "官方默认",
      quickActions: "快捷操作",
      enableInstruction: "启用指令提示词",
      disableInstruction: "禁用指令提示词",
      restoreLatest: "恢复最新备份",
    },
    provider: {
      title: "供应商列表",
      subtitle: "像 cc-switch 一样管理 Codex 第三方 API。点击卡片可切换，点击 + 添加新供应商。",
      add: "添加供应商",
      importCc: "从 cc-switch 导入",
      edit: "编辑",
      viewEdit: "编辑",
      remove: "删除",
      switch: "切换",
      current: "当前",
      official: "官方配置",
      noRouting: "不支持路由",
      authReady: "认证文件存在",
      authMissing: "未找到认证文件",
      detected: "从 TOML 检测",
      local: "本地保存",
      noProviders: "还没有供应商，点击右上角 + 添加。",
      officialEdit: "OpenAI Official 编辑",
      officialHint: "官方配置不使用第三方路由；这里可以编辑官方模式下的模型和完整 auth.json（ChatGPT 登录通常包含 access_token / refresh_token / id_token）。",
      officialUrl: "官方入口",
      loadOfficialAuth: "从 cc-switch 载入官方认证",
      refreshOfficialAuth: "刷新当前 auth.json",
      officialAuthLoaded: "已载入 cc-switch 官方认证",
      officialAuthRefreshed: "已刷新当前 auth.json",
      officialAuthNotFound: "未找到 cc-switch 官方认证",
      formAdd: "添加新供应商",
      formEdit: "编辑供应商",
      formHint: "保存后会写入供应商列表，并同步写入 Codex live 配置。下方可预览将生成的 config.toml。",
      name: "供应商名称",
      baseUrl: "Base URL",
      model: "模型",
      wireApi: "Wire API",
      apiKey: "API Key",
      apiKeyPlaceholder: "留空则不覆盖 auth.json",
      requiresAuth: "requires_openai_auth",
      save: "保存到列表",
      saveAndSwitch: "保存",
      cancel: "返回列表",
    },
    instruction: {
      title: "一键管理指令提示词",
      desc: "启用时写入指令提示词文件并设置 model_instructions_file；禁用时只移除 Codex-X 管理的指令提示词字段并删除 md 文件。每次操作前都会创建备份。",
      enabled: "已启用",
      disabled: "未启用",
      unset: "model_instructions_file 未设置",
      enable: "启用",
      disable: "禁用 / 删除",
    },
    toml: {
      title: "当前 live TOML 配置",
      desc: "这里显示的是 Codex 当前正在使用的 ~/.codex/config.toml，不是本地保存的供应商模板。切换供应商后，这里会变成新写入的 live 配置。",
      loaded: "已读取",
      missingText: "# config.toml 不存在，执行切换或启用后会自动创建。",
    },
    backups: {
      title: "备份与撤回",
      empty: "还没有备份。首次写入前会自动创建。",
      restore: "恢复",
    },
    settings: {
      title: "设置",
      language: "界面语言",
      zh: "中文",
      en: "English",
      languageDesc: "默认中文，可随时切换。设置会保存在浏览器本地存储。",
      productName: "产品名",
      productDesc: "当前名称为 Codex-X，定位是 Codex Switch & Instruct。",
    },
    loadingConfig: "正在读取 Codex 配置...",
    noAuth: "无 auth",
    authJson: "auth.json",
  },
  en: {
    appSubtitle: "Switch · Instruct · Config",
    manager: "Codex config manager",
    load: "Load",
    refresh: "Refresh",
    nav: {
      dashboard: "Overview",
      provider: "Provider",
      sessions: "Sessions",
      skillsMcp: "Skills & MCP",
      instruction: "Prompt",
      toml: "TOML",
      settings: "Settings",
      about: "About",
    },
    dashboard: {
      config: "Config",
      found: "Found",
      missing: "Missing",
      provider: "Provider",
      instruction: "Instruction Prompt",
      enabled: "Enabled",
      disabled: "Disabled",
      auth: "Auth",
      currentConfig: "Current Codex config",
      liveStatus: "Live status",
      dir: "Directory",
      configPath: "Config",
      model: "Model",
      providerName: "Provider",
      instructionFile: "Instruction",
      notSet: "Not set",
      officialDefault: "Official / Default",
      quickActions: "Quick actions",
      enableInstruction: "Enable prompt",
      disableInstruction: "Disable prompt",
      restoreLatest: "Restore latest backup",
    },
    provider: {
      title: "Provider list",
      subtitle: "Manage Codex third-party APIs like cc-switch. Click a row to switch; use + to add a provider.",
      add: "Add provider",
      importCc: "Import from cc-switch",
      edit: "Edit",
      viewEdit: "Edit",
      remove: "Delete",
      switch: "Switch",
      current: "Current",
      official: "Official",
      noRouting: "No routing",
      authReady: "Auth found",
      authMissing: "Auth missing",
      detected: "Detected from TOML",
      local: "Local",
      noProviders: "No provider yet. Click + to add one.",
      officialEdit: "OpenAI Official settings",
      officialHint: "Official mode does not use third-party routing. You can edit the official model and the full auth.json (ChatGPT login usually contains access_token / refresh_token / id_token).",
      officialUrl: "Official URL",
      loadOfficialAuth: "Load official auth from cc-switch",
      refreshOfficialAuth: "Refresh current auth.json",
      officialAuthLoaded: "Loaded cc-switch official auth",
      officialAuthRefreshed: "Current auth.json refreshed",
      officialAuthNotFound: "No cc-switch official auth found",
      formAdd: "Add provider",
      formEdit: "Edit provider",
      formHint: "Save writes the provider to the list and applies it to the Codex live config. The generated config.toml is previewed below.",
      name: "Provider name",
      baseUrl: "Base URL",
      model: "Model",
      wireApi: "Wire API",
      apiKey: "API Key",
      apiKeyPlaceholder: "Leave blank to keep auth.json unchanged",
      requiresAuth: "requires_openai_auth",
      save: "Save",
      saveAndSwitch: "Save",
      cancel: "Back",
    },
    instruction: {
      title: "Manage instruction prompt",
      desc: "Enable writes the instruction prompt file and sets model_instructions_file; disable removes Codex-X-managed instruction prompt config and deletes the md file. Every write creates a backup first.",
      enabled: "Enabled",
      disabled: "Disabled",
      unset: "model_instructions_file is not set",
      enable: "Enable",
      disable: "Disable / delete",
    },
    toml: {
      title: "Current live TOML config",
      desc: "This is the active ~/.codex/config.toml used by Codex, not a saved provider template. After switching providers, this page shows the newly written live config.",
      loaded: "Loaded",
      missingText: "# config.toml is missing. It will be created after switching or enabling instruction.",
    },
    backups: {
      title: "Backups & restore",
      empty: "No backups yet. A backup will be created before the first write.",
      restore: "Restore",
    },
    settings: {
      title: "Settings",
      language: "Language",
      zh: "中文",
      en: "English",
      languageDesc: "Chinese is the default. You can switch at any time; the setting is saved locally.",
      productName: "Product name",
      productDesc: "Current name is Codex-X, positioned as Codex Switch & Instruct.",
    },
    loadingConfig: "Reading Codex config...",
    noAuth: "No auth",
    authJson: "auth.json",
  },
} as const;

function cx(...items: Array<string | false | undefined>) {
  return items.filter(Boolean).join(" ");
}

function providerId(name: string) {
  const slug = name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return slug || `provider-${Date.now()}`;
}

function isReservedCodexProviderId(id: string) {
  return ["openai", "amazon-bedrock", "ollama", "lmstudio", "oss"].includes(id.trim().toLowerCase());
}

function customProviderId(name: string) {
  const id = providerId(name);
  return isReservedCodexProviderId(id) ? `${id}-custom` : id;
}

function uniqueId(base: string, existingIds: Iterable<string>) {
  const used = new Set(Array.from(existingIds).map((id) => id.trim().toLowerCase()));
  const clean = providerId(base);
  let candidate = clean;
  let index = 2;
  while (used.has(candidate.toLowerCase())) {
    candidate = `${clean}-${index}`;
    index += 1;
  }
  return candidate;
}

function splitMarkdownFilename(filename: string) {
  const clean = filename.trim().replace(/[\/\\]+/g, "-") || "prompt.md";
  const stem = clean.replace(/\.md$/i, "") || "prompt";
  return { stem, filename: `${stem}.md` };
}

function uniquePromptFilename(filename: string, existingFilenames: Iterable<string>) {
  const used = new Set(Array.from(existingFilenames).map((name) => name.trim().toLowerCase()));
  const { stem } = splitMarkdownFilename(filename);
  let candidate = `${stem}.md`;
  let index = 2;
  while (used.has(candidate.toLowerCase())) {
    candidate = `${stem}-${index}.md`;
    index += 1;
  }
  return candidate;
}

function StatusPill({ active, label }: { active: boolean; label: string }) {
  return <span className={cx("pill", active ? "pill-ok" : "pill-muted")}>{label}</span>;
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="field">
      <span>{label}</span>
      {children}
    </label>
  );
}

function StatCard({ icon, label, value, ok }: { icon: React.ReactNode; label: string; value: React.ReactNode; ok?: boolean }) {
  return (
    <div className="stat-card">
      <div className={cx("stat-icon", ok ? "stat-icon-ok" : undefined)}>{icon}</div>
      <div>
        <p>{label}</p>
        <strong>{value}</strong>
      </div>
    </div>
  );
}

function Avatar({ name }: { name: string }) {
  const initials = name
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((s) => s[0]?.toUpperCase())
    .join("") || "P";
  return <div className="provider-avatar">{initials}</div>;
}


function OpenAIIcon() {
  return (
    <div className="openai-avatar" aria-label="OpenAI Official">
      <svg viewBox="0 0 64 64" width="30" height="30" role="img">
        <path
          d="M31.6 6.5c4.4 0 8.3 2.3 10.5 5.8 4.1.2 8 2.5 10.2 6.3 2.2 3.8 2.1 8.4.2 11.9 1.9 3.7 1.9 8.2-.3 12-2.2 3.8-6.1 6.1-10.2 6.3-2.2 3.5-6.1 5.7-10.5 5.7-4.4 0-8.3-2.2-10.5-5.7-4.1-.2-8-2.5-10.2-6.3-2.2-3.8-2.2-8.3-.3-12-1.9-3.6-1.9-8.1.3-11.9 2.2-3.8 6.1-6.1 10.2-6.3 2.2-3.5 6.1-5.8 10.6-5.8Zm0 5.6c-2.3 0-4.3 1-5.7 2.7l12.1 7V16c0-2.2-2.9-3.9-6.4-3.9Zm11 6.1v14l5-2.9c1.9-1.1 2.1-4.5.4-7.5-1.2-2.2-3.2-3.5-5.4-3.6Zm-23.9.1c-2.1.2-4.1 1.5-5.3 3.6-1.8 3-1.6 6.4.4 7.5l5 2.9v-14Zm5.2 1.2v14.1l7.7 4.5 7.7-4.5V19.5l-7.7 4.5-7.7-4.5Zm-9.2 15.9c-1.9 1.2-2.1 4.5-.4 7.5 1.2 2.1 3.2 3.4 5.3 3.6v-14l-4.9 2.9Zm34 .1-5 2.9v14c2.1-.2 4.1-1.5 5.3-3.6 1.8-3 1.6-6.4-.3-7.3Zm-17.1 8.7-7.7-4.5v5.8c0 2.2 2.9 3.9 6.4 3.9 2.3 0 4.4-1 5.7-2.7l-4.4-2.5Z"
          fill="currentColor"
        />
      </svg>
    </div>
  );
}



function tomlEscape(value: string) {
  return value.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

function extractOpenAiApiKey(authText?: string) {
  if (!authText?.trim()) return "";
  try {
    const parsed = JSON.parse(authText) as { OPENAI_API_KEY?: unknown };
    return typeof parsed.OPENAI_API_KEY === "string" ? parsed.OPENAI_API_KEY : "";
  } catch {
    return "";
  }
}

function buildProviderTomlPreview(provider: SavedProvider, state: CodexState | null) {
  const model = provider.model.trim() || "gpt-5.5";
  const name = provider.providerName.trim() || "your-provider";
  // Codex live config follows cc-switch: all third-party providers are applied as `custom`.
  const providerKey = "custom";
  const baseUrl = provider.baseUrl.trim().replace(/\/+$/, "") || "https://example.com/v1";
  const wireApi = provider.wireApi || "responses";
  const apiKey = provider.apiKey?.trim();
  const source = state?.configText?.trimEnd() || "";
  const sourceLines = source ? source.split("\n") : [];
  const keptLines: string[] = [];
  let currentSection = "";
  let skippingCustomProvider = false;
  let hasReasoningEffort = false;

  for (const line of sourceLines) {
    const sectionMatch = line.match(/^\s*\[([^\]]+)]\s*$/);
    if (sectionMatch) {
      currentSection = sectionMatch[1].trim();
      skippingCustomProvider = currentSection === `model_providers.${providerKey}`;
      if (skippingCustomProvider) continue;
    }
    if (skippingCustomProvider) continue;

    if (!currentSection) {
      const keyMatch = line.match(/^\s*([A-Za-z0-9_-]+)\s*=/);
      const key = keyMatch?.[1];
      if (key === "model_provider" || key === "model") continue;
      if (key === "model_reasoning_effort") hasReasoningEffort = true;
    }
    keptLines.push(line);
  }

  const firstSectionIndex = keptLines.findIndex((line) => /^\s*\[[^\]]+]\s*$/.test(line));
  const rootLines = (firstSectionIndex === -1 ? keptLines : keptLines.slice(0, firstSectionIndex)).filter((line, index, lines) => {
    if (line.trim()) return true;
    return index > 0 && index < lines.length - 1;
  });
  const sectionLines = firstSectionIndex === -1 ? [] : keptLines.slice(firstSectionIndex).filter((line, index, lines) => {
    if (line.trim()) return true;
    return index > 0 && index < lines.length - 1;
  });

  const headerLines = [
    `model_provider = "${tomlEscape(providerKey)}"`,
    `model = "${tomlEscape(model)}"`,
  ];
  if (!hasReasoningEffort) {
    headerLines.push('model_reasoning_effort = "high"');
  }

  const providerLines = [
    `[model_providers.${providerKey}]`,
    `name = "${tomlEscape(name)}"`,
    `base_url = "${tomlEscape(baseUrl)}"`,
    `wire_api = "${tomlEscape(wireApi)}"`,
    `requires_openai_auth = ${provider.requiresOpenaiAuth ? "true" : "false"}`,
  ];

  return [
    ...headerLines,
    ...(rootLines.length ? ["", ...rootLines] : []),
    "",
    ...providerLines,
    ...(sectionLines.length ? ["", ...sectionLines] : []),
  ].join("\n");
}


function buildProviderAuthPreview(provider: SavedProvider) {
  const key = provider.apiKey?.trim();
  return JSON.stringify({ OPENAI_API_KEY: key || null, auth_mode: key ? "apikey" : undefined }, null, 2);
}


function instructionIdFromPath(path?: string) {
  if (!path) return "";
  const normalized = path.replace(/\\/g, "/");
  const found = instructionTemplates.find((item) => normalized.endsWith(item.filename));
  return found?.id || "custom";
}

function JsonPreview({ text }: { text: string }) {
  return (
    <pre className="toml-preview json-preview" aria-label="JSON preview">
      {text.split("\n").map((line, index) => (
        <div className="toml-line" key={index}>
          <span className="toml-line-no">{index + 1}</span>
          <code>{line}</code>
        </div>
      ))}
    </pre>
  );
}

function renderTomlValue(value: string, lineKey: string) {
  const parts = value.split(/("(?:\\.|[^"])*")/g);
  return parts.map((part, index) => {
    if (!part) return null;
    const key = `${lineKey}-v-${index}`;
    if (/^"(?:\\.|[^"])*"$/.test(part)) {
      return <span className="toml-string" key={key}>{part}</span>;
    }
    const boolParts = part.split(/\b(true|false)\b/g);
    return boolParts.map((piece, boolIndex) => {
      if (piece === "true" || piece === "false") {
        return <span className="toml-bool" key={`${key}-b-${boolIndex}`}>{piece}</span>;
      }
      return <React.Fragment key={`${key}-t-${boolIndex}`}>{piece}</React.Fragment>;
    });
  });
}

function renderTomlLine(line: string, index: number) {
  const key = `toml-${index}`;
  if (line.trim().startsWith("#")) {
    return <span className="toml-comment">{line}</span>;
  }
  if (/^\s*\[[^\]]+\]\s*$/.test(line)) {
    return <span className="toml-section">{line}</span>;
  }
  const eqIndex = line.indexOf("=");
  if (eqIndex > -1) {
    const left = line.slice(0, eqIndex);
    const right = line.slice(eqIndex + 1);
    return (
      <>
        <span className="toml-key">{left}</span>
        <span className="toml-eq">=</span>
        {renderTomlValue(right, key)}
      </>
    );
  }
  return <>{line}</>;
}

function TomlPreview({ text }: { text: string }) {
  return (
    <pre className="toml-preview" aria-label="TOML preview">
      {text.split("\n").map((line, index) => (
        <div className="toml-line" key={index}>
          <span className="toml-line-no">{index + 1}</span>
          <code>{renderTomlLine(line, index)}</code>
        </div>
      ))}
    </pre>
  );
}


function normalizeVersion(value?: string) {
  return (value || "").trim().replace(/^v/i, "");
}

function compareVersions(a?: string, b?: string) {
  const pa = normalizeVersion(a).split(/[.-]/).map((x) => Number.parseInt(x, 10) || 0);
  const pb = normalizeVersion(b).split(/[.-]/).map((x) => Number.parseInt(x, 10) || 0);
  const len = Math.max(pa.length, pb.length, 3);
  for (let i = 0; i < len; i += 1) {
    const diff = (pa[i] || 0) - (pb[i] || 0);
    if (diff !== 0) return diff;
  }
  return 0;
}

function releaseAssetForPlatform(assets: Array<{ name?: string; browser_download_url?: string }>) {
  const platform = navigator.userAgent.toLowerCase();
  const isMac = platform.includes("mac");
  const isWindows = platform.includes("windows");
  const isLinux = platform.includes("linux");
  return assets.find((asset) => {
    const name = (asset.name || "").toLowerCase();
    if (isMac) return name.endsWith(".dmg") || name.endsWith(".app.tar.gz");
    if (isWindows) return name.endsWith(".msi") || name.endsWith(".exe");
    if (isLinux) return name.endsWith(".appimage") || name.endsWith(".deb") || name.endsWith(".rpm");
    return Boolean(name);
  }) || assets[0];
}

function formatSessionTime(value?: number | null) {
  if (!value) return "未知时间";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "未知时间";
  return date.toLocaleString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function compactPath(value?: string | null, max = 58) {
  if (!value) return "未记录路径";
  const normalized = value.replace(/\\/g, "/");
  if (normalized.length <= max) return normalized;
  const parts = normalized.split("/").filter(Boolean);
  if (parts.length >= 3) {
    const tail = parts.slice(-3).join("/");
    return `…/${tail}`;
  }
  return `…${normalized.slice(-max + 1)}`;
}

function shortId(value: string) {
  return value.length > 8 ? value.slice(0, 8) : value;
}

function App() {
  const initialLang = (localStorage.getItem(LANG_KEY) as Lang | null) || "zh";
  const [lang, setLang] = React.useState<Lang>(initialLang === "en" ? "en" : "zh");
  const t = dict[lang];
  const isMacRuntime = navigator.userAgent.toLowerCase().includes("mac");
  const [tab, setTab] = React.useState<Tab>("dashboard");
  const [visitedTabs, setVisitedTabs] = React.useState<Set<Tab>>(() => new Set(["dashboard"]));
  const [providerMode, setProviderMode] = React.useState<ProviderMode>("list");
  const [instructionMode, setInstructionMode] = React.useState<InstructionMode>("list");
  const [skillsMcpTab, setSkillsMcpTab] = React.useState<"mcp" | "skills">("mcp");
  const [editingProviderId, setEditingProviderId] = React.useState<string | null>(null);
  const [editingPromptId, setEditingPromptId] = React.useState<string | null>(null);
  const [savedProviders, setSavedProviders] = React.useState<SavedProvider[]>([]);
  const [activeProviderId, setActiveProviderId] = React.useState(() => localStorage.getItem(ACTIVE_PROVIDER_KEY) || "");
  const [savedPrompts, setSavedPrompts] = React.useState<SavedPrompt[]>([]);
  const [builtinPromptStatus, setBuiltinPromptStatus] = React.useState<BuiltinPromptStatus[]>([]);
  const [aboutInfo, setAboutInfo] = React.useState<AboutInfo | null>(null);
  const [releaseInfo, setReleaseInfo] = React.useState<ReleaseInfo>({ status: "idle" });
  const [updatePromptOpen, setUpdatePromptOpen] = React.useState(false);
  const [sessionStatus, setSessionStatus] = React.useState<SessionSyncStatus | null>(null);
  const [skillsMcpState, setSkillsMcpState] = React.useState<SkillsMcpState | null>(null);
  const [skillsMcpImportPreview, setSkillsMcpImportPreview] = React.useState<SkillsMcpImportPreview | null>(null);
  const [skillsMcpImportOpen, setSkillsMcpImportOpen] = React.useState(false);
  const [startupDiagnostics, setStartupDiagnostics] = React.useState<StartupDiagnostics | null>(null);
  const [startupWizardOpen, setStartupWizardOpen] = React.useState(() => localStorage.getItem(STARTUP_WIZARD_SEEN_KEY) !== "1");
  const [startupClosing, setStartupClosing] = React.useState(false);
  const [sessionQuery, setSessionQuery] = React.useState("");
  const deferredSessionQuery = React.useDeferredValue(sessionQuery);
  const [sessionGroupByCwd, setSessionGroupByCwd] = React.useState(true);
  const [selectedSessionIds, setSelectedSessionIds] = React.useState<string[]>([]);
  const [autoSessionSync, setAutoSessionSync] = React.useState(() => localStorage.getItem(AUTO_SESSION_SYNC_KEY) === "1");
  const [autoSessionSyncBusy, setAutoSessionSyncBusy] = React.useState(false);
  const [state, setState] = React.useState<CodexState | null>(null);
  const [backups, setBackups] = React.useState<BackupEntry[]>([]);
  const [configDir, setConfigDir] = React.useState("");
  const [loading, setLoading] = React.useState(false);
  const [bootVisible, setBootVisible] = React.useState(true);
  const [bootLeaving, setBootLeaving] = React.useState(false);
  const [bootHintIndex, setBootHintIndex] = React.useState(0);
  const [toast, setToast] = React.useState<string>("");
  const [error, setError] = React.useState<string>("");
  const [providerForm, setProviderForm] = React.useState<SavedProvider>(defaultProviderForm);
  const [providerTomlDraft, setProviderTomlDraft] = React.useState("");
  const [providerTomlDirty, setProviderTomlDirty] = React.useState(false);
  const [providerApiKeyVisible, setProviderApiKeyVisible] = React.useState(false);
  const [providerTestingId, setProviderTestingId] = React.useState("");
  const [actionBusy, setActionBusy] = React.useState<string>("");
  const [promptForm, setPromptForm] = React.useState<SavedPrompt>(blankPromptForm);
  const [officialForm, setOfficialForm] = React.useState({ model: "gpt-5.5", authJson: "" });
  const autoUpdateCheckedRef = React.useRef(false);
  const autoSessionSyncRanRef = React.useRef(false);
  const promptImportRef = React.useRef<HTMLInputElement | null>(null);
  const skillZipImportRef = React.useRef<HTMLInputElement | null>(null);
  const promptUpdateCheckedRef = React.useRef(false);
  const skillsMcpLoadedRef = React.useRef(false);
  const bootStartedAtRef = React.useRef(Date.now());
  const providerTomlPreview = React.useMemo(() => buildProviderTomlPreview(providerForm, state), [providerForm, state]);
  const providerAuthPreview = React.useMemo(() => buildProviderAuthPreview(providerForm), [providerForm]);
  const currentInstructionId = instructionIdFromPath(state?.instructionFile);
  const builtinPromptStatusMap = React.useMemo(() => new Map(builtinPromptStatus.map((item) => [item.id, item])), [builtinPromptStatus]);
  const releaseStatusLabel = React.useMemo(() => {
    if (releaseInfo.status === "checking") return lang === "zh" ? "检查中" : "Checking";
    if (releaseInfo.status === "error") return lang === "zh" ? "失败" : "Failed";
    if (releaseInfo.hasUpdate) return lang === "zh" ? "有更新" : "Update found";
    if (releaseInfo.status === "ok") return lang === "zh" ? "已是最新" : "Up to date";
    return lang === "zh" ? "未检查" : "Idle";
  }, [lang, releaseInfo.hasUpdate, releaseInfo.status]);

  React.useEffect(() => {
    localStorage.setItem(LANG_KEY, lang);
  }, [lang]);

  React.useEffect(() => {
    setVisitedTabs((tabs) => {
      if (tabs.has(tab)) return tabs;
      const next = new Set(tabs);
      next.add(tab);
      return next;
    });
  }, [tab]);

  React.useEffect(() => {
    if (providerMode === "form" && !providerTomlDirty) {
      setProviderTomlDraft(providerTomlPreview);
    }
  }, [providerMode, providerTomlDirty, providerTomlPreview]);

  React.useEffect(() => {
    if (!state || !bootVisible || bootLeaving) return undefined;
    const elapsed = Date.now() - bootStartedAtRef.current;
    const minBootMs = 920;
    const leaveDelay = Math.max(120, minBootMs - elapsed);
    const leaveTimer = window.setTimeout(() => {
      setBootLeaving(true);
      window.setTimeout(() => setBootVisible(false), 300);
    }, leaveDelay);
    return () => window.clearTimeout(leaveTimer);
  }, [bootLeaving, bootVisible, state]);

  React.useEffect(() => {
    if (!bootVisible || bootLeaving) return undefined;
    const timer = window.setInterval(() => {
      setBootHintIndex((index) => (index + 1) % bootHints.zh.length);
    }, 620);
    return () => window.clearInterval(timer);
  }, [bootLeaving, bootVisible]);


  const currentProvider = state?.providers.find((p) => p.isCurrent);
  const liveProviderId = (state?.modelProvider || "openai").trim();
  const liveCustomProvider = React.useMemo(() => (state?.providers || []).find((item) => item.id === "custom"), [state?.providers]);
  const inferredActiveProviderId = React.useMemo(() => {
    if (liveProviderId !== "custom" || activeProviderId) return activeProviderId;
    const liveBaseUrl = (liveCustomProvider?.baseUrl || "").replace(/\/+$/, "");
    const liveWireApi = liveCustomProvider?.wireApi || "responses";
    const liveModel = state?.model || "";
    const matched = savedProviders.find((item) =>
      item.baseUrl.replace(/\/+$/, "") === liveBaseUrl &&
      (item.model || "") === liveModel &&
      (item.wireApi || "responses") === liveWireApi,
    );
    return matched?.id || "";
  }, [activeProviderId, liveCustomProvider?.baseUrl, liveCustomProvider?.wireApi, liveProviderId, savedProviders, state?.model]);
  const effectiveActiveProviderId = liveProviderId === "custom" ? inferredActiveProviderId : liveProviderId;
  const currentInstructionPath = (state?.instructionFile || "").replace(/\\/g, "/");
  const currentInstructionFilename = currentInstructionPath.split("/").pop() || "";
  const detectedRows = React.useMemo(() => {
    return (state?.providers || []).map((p) => ({
      id: `detected-${p.id}`,
      source: "detected" as const,
      providerName: p.name || p.id,
      baseUrl: p.baseUrl || "",
      model: state?.model || "gpt-5.5",
      wireApi: p.wireApi || "responses",
      requiresOpenaiAuth: p.requiresOpenaiAuth ?? false,
      isCurrent: p.isCurrent,
    }));
  }, [state]);

  const localRows = React.useMemo(() => {
    return savedProviders.map((p) => ({
      ...p,
      source: "local" as const,
      isCurrent: effectiveActiveProviderId === p.id,
    }));
  }, [savedProviders, effectiveActiveProviderId]);

  const providerRows = React.useMemo(() => {
    const officialRow = {
      id: "openai-official",
      source: "official" as const,
      providerName: "OpenAI Official",
      baseUrl: "https://chatgpt.com/codex",
      model: state?.model || "official",
      wireApi: "official",
      requiresOpenaiAuth: false,
      isCurrent: !state?.modelProvider || state.modelProvider === "openai",
    };
    const seen = new Set<string>();
    const rows: Array<typeof officialRow | (typeof detectedRows)[number] | (typeof localRows)[number]> = [officialRow];
    localRows.forEach((row) => {
      const key = `${row.baseUrl}::${row.model}`;
      if (key !== "::") seen.add(key);
      rows.push(row);
    });
    detectedRows.forEach((row) => {
      const key = `${row.baseUrl}::${row.model}`;
      if (key !== "::" && seen.has(key)) return;
      if (key !== "::") seen.add(key);
      rows.push(row);
    });
    return rows;
  }, [detectedRows, localRows, state?.model, state?.modelProvider]);

  const filteredSessions = React.useMemo(() => {
    const query = deferredSessionQuery.trim().toLowerCase();
    const list = sessionStatus?.sessions || [];
    if (!query) return list;
    return list.filter((item) => [item.title, item.cwd, item.rolloutPath, item.modelProvider, item.model, item.id]
      .filter(Boolean)
      .some((value) => String(value).toLowerCase().includes(query)));
  }, [deferredSessionQuery, sessionStatus?.sessions]);

  const groupedSessions = React.useMemo(() => {
    const groups = new Map<string, SessionPreview[]>();
    if (!sessionGroupByCwd) {
      groups.set(lang === "zh" ? "全部会话" : "All sessions", filteredSessions);
      return Array.from(groups.entries());
    }
    for (const item of filteredSessions) {
      const key = item.cwd || (lang === "zh" ? "未记录工作目录" : "No workspace recorded");
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key)!.push(item);
    }
    return Array.from(groups.entries()).sort((a, b) => b[1].length - a[1].length);
  }, [filteredSessions, lang, sessionGroupByCwd]);

  const unsyncedChatCount = Math.max(sessionStatus?.mismatchedThreads ?? 0, sessionStatus?.mismatchedSessionMeta ?? 0);
  const selectedSessionSet = React.useMemo(() => new Set(selectedSessionIds), [selectedSessionIds]);
  const selectedNeedsSyncCount = React.useMemo(() => {
    const set = new Set(selectedSessionIds);
    return (sessionStatus?.sessions || []).filter((item) => set.has(item.id) && item.needsSync).length;
  }, [selectedSessionIds, sessionStatus?.sessions]);

  const enabledSkillCount = skillsMcpState?.skills.filter((item) => item.enabled).length ?? 0;
  const enabledMcpCount = skillsMcpState?.mcpServers.filter((item) => item.enabled).length ?? 0;
  const activeSkillsMcpCount = skillsMcpTab === "mcp"
    ? (skillsMcpState?.mcpServers.length ?? 0)
    : (skillsMcpState?.skills.length ?? 0);

  React.useEffect(() => {
    setSelectedSessionIds((ids) => ids.filter((id) => (sessionStatus?.sessions || []).some((item) => item.id === id)));
  }, [sessionStatus?.sessions]);

  const call = React.useCallback(async <T,>(fn: () => Promise<T>, success?: (data: T) => void) => {
    setLoading(true);
    setError("");
    try {
      const data = await fn();
      success?.(data);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const refresh = React.useCallback(() => {
    call(
      async () => {
        const [next, providerList, promptList, builtinStatus, about] = await Promise.all([
          invoke<CodexState>("get_codex_state", { configDir: configDir || null }),
          invoke<SavedProvider[]>("list_saved_providers"),
          invoke<SavedPrompt[]>("list_saved_prompts"),
          invoke<BuiltinPromptStatus[]>("get_builtin_prompt_status"),
          invoke<AboutInfo>("get_about_info", { configDir: configDir || null }),
        ]);
        return { next, providerList, promptList, builtinStatus, about };
      },
      ({ next, providerList, promptList, builtinStatus, about }) => {
        setState(next);
        setSavedProviders(providerList);
        setSavedPrompts(promptList);
        setBuiltinPromptStatus(builtinStatus);
        setAboutInfo(about);
        if (!configDir) setConfigDir(next.codexDir);
        const resolvedConfigDir = configDir || next.codexDir || null;
        void Promise.all([
          invoke<StartupDiagnostics>("get_startup_diagnostics", { configDir: resolvedConfigDir }),
          invoke<BackupEntry[]>("list_backups"),
          invoke<SessionSyncStatus>("get_session_sync_status", { configDir: resolvedConfigDir, targetProvider: null }),
        ])
          .then(([diagnostics, backupList, sessions]) => {
            setStartupDiagnostics(diagnostics);
            setBackups(backupList);
            setSessionStatus(sessions);
          })
          .catch(() => undefined);
      },
    );
  }, [call, configDir]);

  React.useEffect(() => {
    refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  React.useEffect(() => {
    localStorage.setItem(AUTO_SESSION_SYNC_KEY, autoSessionSync ? "1" : "0");
  }, [autoSessionSync]);

  React.useEffect(() => {
    if (!state) return;
    if (liveProviderId !== "custom") {
      if (activeProviderId) {
        localStorage.removeItem(ACTIVE_PROVIDER_KEY);
        setActiveProviderId("");
      }
      return;
    }
    if (!savedProviders.length) return;
    if (activeProviderId && !savedProviders.some((item) => item.id === activeProviderId)) {
      localStorage.removeItem(ACTIVE_PROVIDER_KEY);
      setActiveProviderId("");
    }
  }, [activeProviderId, liveProviderId, savedProviders, state]);

  React.useEffect(() => {
    if (!autoSessionSync || autoSessionSyncRanRef.current || !state?.codexDir) return;
    autoSessionSyncRanRef.current = true;
    const resolvedConfigDir = configDir || state.codexDir || null;
    setAutoSessionSyncBusy(true);
    invoke<SessionSyncStatus>("get_session_sync_status", { configDir: resolvedConfigDir, targetProvider: null })
      .then((status) => {
        if (!status.needsSync) {
          setSessionStatus(status);
          return null;
        }
        return invoke<SessionSyncResult>("sync_sessions_provider", { configDir: resolvedConfigDir, targetProvider: null })
          .then((result) => {
            setSessionStatus(result.status);
            setToast(lang === "zh" ? `已自动修复 ${result.updatedThreads} 条会话` : `Auto repaired ${result.updatedThreads} session(s)`);
          });
      })
      .catch(() => undefined)
      .finally(() => setAutoSessionSyncBusy(false));
  }, [autoSessionSync, configDir, lang, state?.codexDir]);

  const handleActionResult = (result: ActionResult) => {
    setState(result.state);
    setToast(result.message);
    const resolvedConfigDir = configDir || result.state.codexDir || null;
    void Promise.all([
      invoke<BackupEntry[]>("list_backups"),
      invoke<SavedPrompt[]>("list_saved_prompts"),
      invoke<SavedProvider[]>("list_saved_providers"),
      invoke<SessionSyncStatus>("get_session_sync_status", { configDir: resolvedConfigDir, targetProvider: null }),
    ])
      .then(([backupList, promptList, providerList, sessions]) => {
        setBackups(backupList);
        setSavedPrompts(promptList);
        setSavedProviders(providerList);
        setSessionStatus(sessions);
      })
      .catch(() => undefined);
  };

  const enableInstruction = () =>
    call(() => invoke<ActionResult>("enable_instruction", { configDir: configDir || null }), handleActionResult);

  const switchInstructionTemplate = (templateId: string) =>
    call(
      () => invoke<ActionResult>("enable_instruction_template", { configDir: configDir || null, templateId }),
      handleActionResult,
    );

  const disableInstruction = () =>
    call(
      () => invoke<ActionResult>("disable_instruction", { configDir: configDir || null, deleteFile: true }),
      handleActionResult,
    );

  const openAddPrompt = () => {
    setEditingPromptId(null);
    setPromptForm({ ...blankPromptForm });
    setInstructionMode("form");
  };

  const openEditPrompt = (prompt: SavedPrompt) => {
    setEditingPromptId(prompt.id);
    setPromptForm(prompt);
    setInstructionMode("form");
  };

  const normalizedPromptForm = (): SavedPrompt => {
    const existing = savedPrompts.filter((item) => item.id !== editingPromptId);
    const requestedFilename = promptForm.filename.trim() || `${providerId(promptForm.title || "prompt")}.md`;
    const filename = editingPromptId ? requestedFilename : uniquePromptFilename(requestedFilename, existing.map((item) => item.filename));
    return {
      ...promptForm,
      id: editingPromptId || uniqueId(promptForm.id || promptForm.title || filename, existing.map((item) => item.id)),
      title: promptForm.title.trim(),
      filename,
      content: promptForm.content,
    };
  };

  const savePromptOnly = () =>
    call(
      async () => {
        await invoke<SavedPrompt>("save_prompt", { prompt: normalizedPromptForm() });
        return invoke<SavedPrompt[]>("list_saved_prompts");
      },
      (promptList) => {
        setSavedPrompts(promptList);
        setInstructionMode("list");
        setEditingPromptId(null);
        setToast(lang === "zh" ? "提示词已保存" : "Prompt saved");
      },
    );

  const saveAndEnablePrompt = () =>
    call(
      async () => {
        const saved = await invoke<SavedPrompt>("save_prompt", { prompt: normalizedPromptForm() });
        const result = await invoke<ActionResult>("enable_saved_prompt", { configDir: configDir || null, id: saved.id });
        return result;
      },
      (result) => {
        setInstructionMode("list");
        setEditingPromptId(null);
        handleActionResult(result);
      },
    );

  const enableSavedPrompt = (id: string) =>
    call(() => invoke<ActionResult>("enable_saved_prompt", { configDir: configDir || null, id }), handleActionResult);

  const removeSavedPrompt = (id: string) =>
    call(
      async () => {
        await invoke<void>("delete_saved_prompt", { id });
        return invoke<SavedPrompt[]>("list_saved_prompts");
      },
      (promptList) => {
        setSavedPrompts(promptList);
        setToast(lang === "zh" ? "提示词已删除" : "Prompt deleted");
      },
    );

  const importPromptMd = async (file?: File | null) => {
    if (!file) return;
    if (!file.name.toLowerCase().endsWith(".md")) {
      setError(lang === "zh" ? "请选择 .md 提示词文件" : "Please choose a .md prompt file");
      return;
    }
    setActionBusy("importPrompt");
    setLoading(true);
    setError("");
    try {
      const content = await file.text();
      const title = file.name.replace(/\.md$/i, "");
      const filename = uniquePromptFilename(file.name, savedPrompts.map((item) => item.filename));
      await invoke<SavedPrompt>("save_prompt", {
        prompt: {
          id: uniqueId(title, savedPrompts.map((item) => item.id)),
          title: filename.replace(/\.md$/i, ""),
          filename,
          content,
        },
      });
      const promptList = await invoke<SavedPrompt[]>("list_saved_prompts");
      setSavedPrompts(promptList);
      setToast(lang === "zh" ? `已导入提示词：${file.name}` : `Prompt imported: ${file.name}`);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
      setActionBusy("");
      if (promptImportRef.current) promptImportRef.current.value = "";
    }
  };

  const refreshBuiltinPrompts = async ({ quiet = false }: { quiet?: boolean } = {}) => {
    if (!quiet) {
      setActionBusy("refreshPrompts");
      setLoading(true);
    }
    try {
      const list = await invoke<BuiltinPromptStatus[]>("refresh_builtin_prompts");
      setBuiltinPromptStatus(list);
      const updated = list.filter((item) => item.updated).length;
      if (!quiet) {
        setToast(updated > 0
          ? (lang === "zh" ? `已更新 ${updated} 个内置提示词模板` : `${updated} built-in prompt template(s) updated`)
          : (lang === "zh" ? "内置提示词已是 GitHub 最新" : "Built-in prompts are up to date"));
      }
    } catch (e) {
      if (!quiet) setError(String(e));
    } finally {
      if (!quiet) {
        setLoading(false);
        setActionBusy("");
      }
    }
  };

  const normalizedProviderForm = (): SavedProvider => ({
    ...providerForm,
    id: editingProviderId || uniqueId(providerForm.id || customProviderId(providerForm.providerName || providerForm.baseUrl), savedProviders.map((item) => item.id)),
    providerName: providerForm.providerName.trim(),
    baseUrl: providerForm.baseUrl.trim().replace(/\/+$/, ""),
    model: providerForm.model.trim(),
    apiKey: (providerForm.apiKey || "").trim(),
    tomlConfig: (providerTomlDraft || providerForm.tomlConfig || buildProviderTomlPreview(providerForm, state)).trimEnd(),
    wireApi: providerForm.wireApi || "responses",
    requiresOpenaiAuth: providerForm.requiresOpenaiAuth,
  });

  const reloadSavedProviders = async () => {
    const providerList = await invoke<SavedProvider[]>("list_saved_providers");
    setSavedProviders(providerList);
    return providerList;
  };

  const saveProviderOnly = () =>
    call(
      async () => {
        const saved = await invoke<SavedProvider>("save_provider", { provider: normalizedProviderForm() });
        const providerList = await invoke<SavedProvider[]>("list_saved_providers");
        return { saved, providerList };
      },
      ({ providerList }) => {
        setSavedProviders(providerList);
        setProviderMode("list");
        setEditingProviderId(null);
        setProviderTomlDirty(false);
        setToast(lang === "zh" ? "供应商配置已保存" : "Provider saved");
      },
    );

  const switchProvider = (provider: SavedProvider) =>
    call(
      () => {
        const tomlConfig = provider.tomlConfig?.trim();
        if (tomlConfig) {
          return invoke<ActionResult>("save_provider_toml_config", {
            input: {
              configDir: configDir || null,
              configText: tomlConfig,
              apiKey: provider.apiKey || "",
            },
          });
        }
        return invoke<ActionResult>("switch_provider", {
          input: {
            configDir: configDir || null,
            providerId: provider.id,
            providerName: provider.providerName,
            baseUrl: provider.baseUrl,
            model: provider.model,
            apiKey: provider.apiKey || "",
            wireApi: provider.wireApi,
            requiresOpenaiAuth: provider.requiresOpenaiAuth,
          },
        });
      },
      (result) => {
        localStorage.setItem(ACTIVE_PROVIDER_KEY, provider.id);
        setActiveProviderId(provider.id);
        handleActionResult(result);
      },
    );

  const testProvider = async (id: string, baseUrl: string, apiKey?: string | null) => {
    setProviderTestingId(id);
    setError("");
    try {
      const result = await invoke<ProviderConnectionResult>("test_provider_connection", { baseUrl, apiKey: apiKey || null });
      setToast(result.ok
        ? (lang === "zh" ? `连接正常\n${result.durationMs} ms` : `Connection OK\n${result.durationMs} ms`)
        : (lang === "zh" ? `连接失败\n${result.message}` : `Connection failed\n${result.message}`));
    } catch (e) {
      setError(String(e));
    } finally {
      setProviderTestingId("");
    }
  };

  const saveProviderConfig = saveProviderOnly;

  const switchOfficialProvider = () =>
    call(
      () => invoke<ActionResult>("switch_official_provider", { configDir: configDir || null }),
      (result) => {
        localStorage.removeItem(ACTIVE_PROVIDER_KEY);
        setActiveProviderId("");
        handleActionResult(result);
      },
    );

  const importFromCcSwitch = () =>
    call(
      () => invoke<ImportResult>("import_ccswitch_codex_providers", { dbPath: null }),
      (result) => {
        setSavedProviders(result.providers);
        const warningText = result.warnings.length > 0 ? `，跳过 ${result.skipped}` : "";
        setToast(
          lang === "zh"
            ? `已从 cc-switch 导入 ${result.imported} 个供应商${warningText}`
            : `Imported ${result.imported} provider(s) from cc-switch${warningText}`,
        );
      },
    );

  const restoreBackup = (backupId: string) =>
    call(() => invoke<ActionResult>("restore_backup", { configDir: configDir || null, backupId }), handleActionResult);

  const openExternalUrl = React.useCallback((url?: string | null) => {
    if (!url) return;
    window.setTimeout(() => {
      void invoke("open_url", { url }).catch(() => {
        setToast(lang === "zh" ? "打开浏览器失败" : "Failed to open browser");
      });
    }, 0);
  }, [lang]);

  const checkForUpdates = React.useCallback(async ({ quiet = false }: { quiet?: boolean } = {}) => {
    const repo = aboutInfo?.githubRepo || FALLBACK_GITHUB_REPO;
    const appVersion = aboutInfo?.appVersion || "0.0.0";
    const releasesUrl = `https://github.com/${repo}/releases/`;
    setReleaseInfo({ status: "checking" });
    try {
      const response = await fetch(`https://api.github.com/repos/${repo}/releases/latest`, {
        headers: { Accept: "application/vnd.github+json" },
      });
      if (!response.ok) {
        throw new Error(`GitHub Releases ${response.status}`);
      }
      const release = await response.json() as {
        tag_name?: string;
        name?: string;
        html_url?: string;
        body?: string;
        assets?: Array<{ name?: string; browser_download_url?: string }>;
      };
      const latestVersion = release.tag_name || release.name || "";
      const asset = releaseAssetForPlatform(release.assets || []);
      const hasUpdate = compareVersions(latestVersion, appVersion) > 0;
      const message = hasUpdate
        ? (lang === "zh" ? "发现新版本" : "Update available")
        : (lang === "zh" ? "当前已是最新版本" : "You are up to date");
      setReleaseInfo({
        status: "ok",
        latestVersion,
        htmlUrl: releasesUrl,
        assetName: asset?.name,
        body: release.body || "",
        hasUpdate,
        message,
      });
      if (hasUpdate) {
        if (quiet) {
          setToast(lang === "zh" ? `发现新版本 ${latestVersion}，可在概览页查看` : `New version ${latestVersion} is available`);
        } else {
          setUpdatePromptOpen(true);
        }
      } else if (!quiet) {
        setToast(message);
      }
    } catch (e) {
      const message = quiet ? (lang === "zh" ? "自动检查失败" : "Auto check failed") : (lang === "zh" ? "检查失败" : "Check failed");
      setReleaseInfo({
        status: "error",
        message,
      });
      if (!quiet) setToast(message);
    }
  }, [aboutInfo?.githubRepo, aboutInfo?.appVersion, lang]);

  React.useEffect(() => {
    if (!state || !aboutInfo || autoUpdateCheckedRef.current) return;
    autoUpdateCheckedRef.current = true;
    void checkForUpdates({ quiet: true });
  }, [state, aboutInfo, checkForUpdates]);

  React.useEffect(() => {
    if (tab !== "instruction" || promptUpdateCheckedRef.current) return;
    promptUpdateCheckedRef.current = true;
    const run = () => void refreshBuiltinPrompts({ quiet: true });
    const timer = window.setTimeout(() => {
      if ("requestIdleCallback" in window) {
        window.requestIdleCallback(run, { timeout: 2200 });
      } else {
        run();
      }
    }, 1600);
    return () => window.clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab]);

  const loadSkillsMcp = React.useCallback(async ({ quiet = false }: { quiet?: boolean } = {}) => {
    if (!quiet) {
      setActionBusy("loadSkillsMcp");
      setError("");
    }
    try {
      const result = await invoke<SkillsMcpState>("get_skills_mcp_state", { configDir: configDir || null });
      setSkillsMcpState(result);
    } catch (e) {
      if (!quiet) setError(String(e));
    } finally {
      if (!quiet) setActionBusy("");
    }
  }, [configDir]);

  React.useEffect(() => {
    if (tab !== "skillsMcp" || skillsMcpLoadedRef.current) return;
    skillsMcpLoadedRef.current = true;
    void loadSkillsMcp();
  }, [tab, loadSkillsMcp]);

  const openImportExistingSkillsMcpPreview = async () => {
    setActionBusy("previewExistingSkillsMcp");
    setError("");
    try {
      const preview = await invoke<SkillsMcpImportPreview>("preview_existing_skills_mcp", { configDir: configDir || null });
      setSkillsMcpImportPreview(preview);
      setSkillsMcpImportOpen(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setActionBusy("");
    }
  };

  const importExistingSkillsMcp = async () => {
    setActionBusy("importExistingSkillsMcp");
    setError("");
    try {
      const result = await invoke<SkillsMcpActionResult>("import_existing_skills_mcp", { configDir: configDir || null });
      setSkillsMcpState(result.state);
      setSkillsMcpImportOpen(false);
      setSkillsMcpImportPreview(null);
      setToast(result.message);
    } catch (e) {
      setError(String(e));
    } finally {
      setActionBusy("");
    }
  };

  const checkSkillUpdatesAction = async () => {
    setActionBusy("checkSkillUpdates");
    setError("");
    try {
      const result = await invoke<SkillsMcpState>("check_skill_updates", { configDir: configDir || null });
      setSkillsMcpState(result);
      setToast(lang === "zh" ? "Skills 更新状态已刷新" : "Skill update status refreshed");
    } catch (e) {
      setError(String(e));
    } finally {
      setActionBusy("");
    }
  };

  const toggleSkillEnabled = async (id: string, enabled: boolean) => {
    setActionBusy(`skill:${id}`);
    setError("");
    try {
      const result = await invoke<SkillsMcpState>("toggle_codex_skill", { configDir: configDir || null, id, enabled });
      setSkillsMcpState(result);
      setToast(enabled ? (lang === "zh" ? "Skill 已启用" : "Skill enabled") : (lang === "zh" ? "Skill 已禁用" : "Skill disabled"));
    } catch (e) {
      setError(String(e));
    } finally {
      setActionBusy("");
    }
  };

  const toggleMcpEnabled = async (id: string, enabled: boolean) => {
    setActionBusy(`mcp:${id}`);
    setError("");
    try {
      const result = await invoke<SkillsMcpState>("toggle_codex_mcp", { configDir: configDir || null, id, enabled });
      setSkillsMcpState(result);
      setToast(enabled ? (lang === "zh" ? "MCP 已启用" : "MCP enabled") : (lang === "zh" ? "MCP 已禁用" : "MCP disabled"));
    } catch (e) {
      setError(String(e));
    } finally {
      setActionBusy("");
    }
  };

  const installSkillZipFile = async (file?: File | null) => {
    if (!file) return;
    if (!file.name.toLowerCase().endsWith(".zip")) {
      setError(lang === "zh" ? "请选择 .zip 技能包" : "Please choose a .zip skill package");
      return;
    }
    if (file.size > 20 * 1024 * 1024) {
      setError(lang === "zh" ? "ZIP 技能包不能超过 20MB" : "Skill ZIP must be smaller than 20MB");
      return;
    }
    setActionBusy("installSkillZip");
    setError("");
    try {
      const bytes = Array.from(new Uint8Array(await file.arrayBuffer()));
      const result = await invoke<SkillsMcpActionResult>("install_skill_zip", { configDir: configDir || null, fileName: file.name, bytes });
      setSkillsMcpState(result.state);
      setToast(result.message);
    } catch (e) {
      setError(String(e));
    } finally {
      setActionBusy("");
      if (skillZipImportRef.current) skillZipImportRef.current.value = "";
    }
  };

  const loadCcSwitchOfficialAuth = async (showToast = true) => {
    const candidate = await invoke<OfficialAuthCandidate | null>("read_ccswitch_official_auth", { dbPath: null });
    if (candidate) {
      setOfficialForm({
        model: candidate.model || state?.model || "gpt-5.5",
        authJson: candidate.authJson,
      });
      if (showToast) setToast(t.provider.officialAuthLoaded);
      return true;
    }
    if (showToast) setToast(t.provider.officialAuthNotFound);
    return false;
  };

  const officialAuthPlaceholder = '{\n  "OPENAI_API_KEY": null,\n  "auth_mode": "chatgpt",\n  "tokens": {\n    "access_token": "",\n    "refresh_token": "",\n    "id_token": ""\n  }\n}';

  const refreshLiveOfficialAuth = async (showToast = true) => {
    const next = await invoke<CodexState>("get_codex_state", { configDir: configDir || null });
    setState(next);
    setOfficialForm((form) => ({
      ...form,
      model: next.model || form.model || "gpt-5.5",
      authJson: next.authText || officialAuthPlaceholder,
    }));
    if (showToast) setToast(t.provider.officialAuthRefreshed);
  };

  const openOfficialEdit = () => {
    setOfficialForm({
      model: state?.model || "gpt-5.5",
      authJson: state?.authText || officialAuthPlaceholder,
    });
    setProviderMode("official");
  };

  const saveOfficialConfig = () =>
    call(
      () =>
        invoke<ActionResult>("save_official_config", {
          input: {
            configDir: configDir || null,
            model: officialForm.model,
            authJson: officialForm.authJson,
          },
        }),
      (result) => {
        handleActionResult(result);
        setProviderMode("list");
      },
    );

  const openAddProvider = () => {
    const next = { ...blankProviderForm };
    setEditingProviderId(null);
    setProviderForm(next);
    setProviderTomlDraft(buildProviderTomlPreview(next, state));
    setProviderTomlDirty(false);
    setProviderMode("form");
  };

  const openEditProvider = (provider: SavedProvider) => {
    setEditingProviderId(provider.id);
    setProviderForm(provider);
    setProviderTomlDraft(provider.tomlConfig?.trim() || buildProviderTomlPreview(provider, state));
    setProviderTomlDirty(false);
    setProviderMode("form");
  };

  const openEditDetectedProvider = (provider: { id: string; providerName: string; baseUrl: string; model: string; wireApi: string; requiresOpenaiAuth: boolean }) => {
    setEditingProviderId(customProviderId(provider.providerName || provider.baseUrl));
    const next = {
      id: customProviderId(provider.providerName || provider.baseUrl),
      providerName: provider.providerName,
      baseUrl: provider.baseUrl,
      model: provider.model,
      apiKey: extractOpenAiApiKey(state?.authText),
      tomlConfig: "",
      wireApi: provider.wireApi || "responses",
      requiresOpenaiAuth: provider.requiresOpenaiAuth,
    };
    setProviderForm(next);
    setProviderTomlDraft(buildProviderTomlPreview(next, state));
    setProviderTomlDirty(false);
    setProviderMode("form");
  };

  const removeProvider = (id: string) => {
    call(
      async () => {
        await invoke<void>("delete_saved_provider", { id });
        return invoke<SavedProvider[]>("list_saved_providers");
      },
      (providerList) => {
        setSavedProviders(providerList);
        setToast(lang === "zh" ? "已从 SQLite 删除供应商" : "Provider deleted from SQLite");
      },
    );
  };

  const checkSessions = async () => {
    setActionBusy("checkSessions");
    await call(
      () => invoke<SessionSyncStatus>("get_session_sync_status", { configDir: configDir || null, targetProvider: null }),
      (status) => {
        setSessionStatus(status);
        setToast(status.needsSync
          ? (lang === "zh" ? `发现 ${Math.max(status.mismatchedThreads, status.mismatchedSessionMeta)} 个聊天需要修复` : `${Math.max(status.mismatchedThreads, status.mismatchedSessionMeta)} chat(s) need repair`)
          : (lang === "zh" ? "会话已同步" : "Sessions are in sync"));
      },
    );
    setActionBusy("");
  };

  const syncSessions = async () => {
    setActionBusy("syncSessions");
    await call(
      () => invoke<SessionSyncResult>("sync_sessions_provider", { configDir: configDir || null, targetProvider: null }),
      (result) => {
        setSessionStatus(result.status);
        setSelectedSessionIds([]);
        setToast(lang === "zh"
          ? `已修复 ${result.updatedThreads} 条聊天记录`
          : `Updated ${result.updatedThreads} chat row(s)`);
      },
    );
    setActionBusy("");
  };

  const toggleSessionSelected = (id: string) => {
    setSelectedSessionIds((ids) => ids.includes(id) ? ids.filter((item) => item !== id) : [...ids, id]);
  };

  const selectVisibleNeedsSyncSessions = () => {
    const ids = filteredSessions.filter((item) => item.needsSync).map((item) => item.id);
    setSelectedSessionIds(ids);
  };

  const syncSelectedSessions = async () => {
    setActionBusy("syncSelectedSessions");
    await call(
      () => invoke<SessionSyncResult>("sync_selected_sessions_provider", {
        input: {
          configDir: configDir || null,
          targetProvider: null,
          sessionIds: selectedSessionIds,
        },
      }),
      (result) => {
        setSessionStatus(result.status);
        setSelectedSessionIds([]);
        setToast(lang === "zh"
          ? `已修复选中的 ${result.updatedThreads} 条聊天记录`
          : `Selected sessions repaired: ${result.updatedThreads} chat row(s)`);
      },
    );
    setActionBusy("");
  };

  const closeStartupWizard = () => {
    localStorage.setItem(STARTUP_WIZARD_SEEN_KEY, "1");
    setStartupClosing(true);
    window.setTimeout(() => {
      setStartupWizardOpen(false);
      setStartupClosing(false);
    }, 260);
  };

  const navItems: Array<[Tab, string, React.ReactNode]> = [
    ["dashboard", t.nav.dashboard, <Layers3 size={18} />],
    ["provider", t.nav.provider, <Zap size={18} />],
    ["sessions", t.nav.sessions, <History size={18} />],
    ["skillsMcp", t.nav.skillsMcp, <Layers3 size={18} />],
    ["instruction", t.nav.instruction, <Sparkles size={18} />],
    ["toml", t.nav.toml, <FileCode2 size={18} />],
    ["settings", t.nav.settings, <Settings size={18} />],
    ["about", t.nav.about, <Info size={18} />],
  ];

  const toastLayer = toast ? (() => {
    const [title, ...rest] = toast.split("\n");
    const message = rest.join("\n").trim();
    return (
      <div className="toast ok" onAnimationEnd={() => setToast("")}>
        <div className="toast-icon"><CheckCircle2 size={16} /></div>
        <div className="toast-copy">
          <strong>{title}</strong>
          {message && <span>{message}</span>}
        </div>
        <button className="toast-close" onClick={() => setToast("")}>×</button>
      </div>
    );
  })() : error ? (
    <div className="toast error">
      <div className="toast-icon"><AlertCircle size={16} /></div>
      <div className="toast-copy">
        <strong>{lang === "zh" ? "操作失败" : "Action failed"}</strong>
        <span>{error}</span>
      </div>
      <button className="toast-close" onClick={() => setError("")}>×</button>
    </div>
  ) : null;

  return (
    <main className={cx("app-shell", isMacRuntime && "mac-shell")}>
      {isMacRuntime && <div className="window-drag-strip" data-tauri-drag-region />}
      <div className="orb orb-a" />
      <div className="orb orb-b" />
      {toastLayer}

      <aside className="sidebar glass">
        <div className="brand">
          <div className="brand-mark">X</div>
          <div>
            <h1>Codex-X</h1>
            <p>{t.appSubtitle}</p>
          </div>
        </div>

        <nav>
          {navItems.map(([id, label, icon]) => (
            <button key={id} className={cx("nav-item", tab === id && "active")} onClick={() => React.startTransition(() => setTab(id))}>
              {icon}
              <span>{label}</span>
              {tab === id && <ChevronRight size={16} />}
            </button>
          ))}
        </nav>

        <div className="sidebar-footer" />
      </aside>

      <section className="content">
        {tab === "dashboard" && (
          <header className="topbar glass">
            <div>
              <p className="eyebrow">{t.manager}</p>
              <h2>{state?.model || "gpt-5.5"}</h2>
            </div>
            <div className="path-box">
              <span>CODEX_HOME</span>
              <input value={configDir} onChange={(e) => setConfigDir(e.target.value)} placeholder="~/.codex" />
            </div>
            <button className="primary-btn" onClick={refresh} disabled={loading}>
              {loading ? <Loader2 className="spin" size={17} /> : <RefreshCw size={17} />}
              {t.load}
            </button>
          </header>
        )}

        {updatePromptOpen && releaseInfo.hasUpdate && (
          <div className="update-mask" onClick={() => setUpdatePromptOpen(false)}>
            <div className="update-dialog glass" onClick={(e) => e.stopPropagation()}>
              <div className="update-head">
                <div className="update-icon"><Sparkles size={22} /></div>
                <div>
                  <p className="eyebrow">Codex-X</p>
                  <h3>{lang === "zh" ? "发现新版本" : "New version available"}</h3>
                </div>
              </div>
              <div className="update-body">
                <p>{lang === "zh" ? "检测到新版本，是否立即打开下载页？" : "A new version was found. Open the download page now?"}</p>
                <div className="about-kv compact">
                  <div><span>{lang === "zh" ? "当前版本" : "Current"}</span><strong>{aboutInfo?.appVersion || "-"}</strong></div>
                  <div><span>{lang === "zh" ? "最新版本" : "Latest"}</span><strong>{releaseInfo.latestVersion || "-"}</strong></div>
                </div>
              </div>
              <div className="update-actions">
                <button className="primary-btn" onClick={() => {
                  setUpdatePromptOpen(false);
                  openExternalUrl(releaseInfo.htmlUrl);
                }}>
                  <Download size={16} /> {lang === "zh" ? "现在下载" : "Download now"}
                </button>
                <button className="secondary-btn" onClick={() => setUpdatePromptOpen(false)}>
                  {lang === "zh" ? "稍后" : "Later"}
                </button>
              </div>
            </div>
          </div>
        )}

        {skillsMcpImportOpen && skillsMcpImportPreview && (
          <div className="update-mask" onClick={() => setSkillsMcpImportOpen(false)}>
            <div className="import-preview-dialog glass" onClick={(e) => e.stopPropagation()}>
              <div className="update-head">
                <div className="update-icon"><Download size={21} /></div>
                <div>
                  <p className="eyebrow">Skills / MCP</p>
                  <h3>{lang === "zh" ? "确认导入已有内容" : "Confirm import"}</h3>
                </div>
              </div>
              <div className="import-preview-summary">
                <div><strong>{skillsMcpImportPreview.skills.length}</strong><span>Skills</span></div>
                <div><strong>{skillsMcpImportPreview.mcpServers.length}</strong><span>MCP</span></div>
              </div>
              <div className="import-preview-body">
                {skillsMcpImportPreview.skills.length === 0 && skillsMcpImportPreview.mcpServers.length === 0 ? (
                  <div className="session-empty compact"><span>{lang === "zh" ? "没有发现可导入的已有 Skills / MCP。" : "No existing Skills / MCP items were found."}</span></div>
                ) : (
                  <>
                    <section className="import-preview-section">
                      <div className="section-title-row">
                        <strong>Skills</strong>
                        <span>{skillsMcpImportPreview.skills.length}</span>
                      </div>
                      <div className="import-preview-list">
                        {skillsMcpImportPreview.skills.length === 0 ? (
                          <p className="empty">{lang === "zh" ? "没有可导入的 Skill" : "No Skill to import"}</p>
                        ) : skillsMcpImportPreview.skills.map((skill) => (
                          <div className="import-preview-row" key={`skill-${skill.id}-${skill.path}`}>
                            <strong>{skill.name}</strong>
                            <span>{skill.directory}</span>
                            <em>{skill.source}</em>
                          </div>
                        ))}
                      </div>
                    </section>
                    <section className="import-preview-section">
                      <div className="section-title-row">
                        <strong>MCP</strong>
                        <span>{skillsMcpImportPreview.mcpServers.length}</span>
                      </div>
                      <div className="import-preview-list">
                        {skillsMcpImportPreview.mcpServers.length === 0 ? (
                          <p className="empty">{lang === "zh" ? "没有可导入的 MCP" : "No MCP to import"}</p>
                        ) : skillsMcpImportPreview.mcpServers.map((server) => (
                          <div className="import-preview-row" key={`mcp-${server.id}-${server.source}`}>
                            <strong>{server.name}</strong>
                            <span>{server.transport}</span>
                            <em>{server.source}</em>
                          </div>
                        ))}
                      </div>
                    </section>
                  </>
                )}
                {skillsMcpImportPreview.warnings.length > 0 && (
                  <div className="skills-mcp-warnings compact">
                    {skillsMcpImportPreview.warnings.map((item, index) => <p key={index}><AlertCircle size={14} /> {item}</p>)}
                  </div>
                )}
              </div>
              <div className="update-actions">
                <button
                  className="primary-btn"
                  onClick={importExistingSkillsMcp}
                  disabled={Boolean(actionBusy) || (skillsMcpImportPreview.skills.length + skillsMcpImportPreview.mcpServers.length === 0)}
                >
                  {actionBusy === "importExistingSkillsMcp" ? <Loader2 size={16} className="spin" /> : <Download size={16} />} {lang === "zh" ? "导入" : "Import"}
                </button>
                <button className="secondary-btn" onClick={() => setSkillsMcpImportOpen(false)} disabled={actionBusy === "importExistingSkillsMcp"}>
                  {lang === "zh" ? "取消" : "Cancel"}
                </button>
              </div>
            </div>
          </div>
        )}

        {startupWizardOpen && startupDiagnostics && (
          <div className={cx("startup-mask", startupClosing && "closing")}>
            <div className="startup-card glass">
              <div className="startup-head">
                <div>
                  <p className="eyebrow">First Run Check</p>
                  <h3>{lang === "zh" ? "首次启动向导" : "First-run wizard"}</h3>
                  <p>{lang === "zh" ? startupDiagnostics.summary : startupDiagnostics.summary}</p>
                </div>
                <button className="ghost-btn" onClick={closeStartupWizard}>{lang === "zh" ? "跳过" : "Skip"}</button>
              </div>

              <div className="startup-path-row">
                <Field label="CODEX_HOME">
                  <input value={configDir || startupDiagnostics.codexDir} onChange={(e) => setConfigDir(e.target.value)} placeholder="~/.codex" />
                </Field>
                <button className="secondary-btn" onClick={refresh} disabled={loading}>
                  <RefreshCw size={16} className={cx(loading && "spin")} /> {lang === "zh" ? "重新检测" : "Recheck"}
                </button>
              </div>

              <div className="startup-check-grid">
                {startupDiagnostics.items.map((item) => (
                  <div className={cx("startup-check-item", item.status === "ok" && "ok", item.status === "manual" && "manual")} key={item.key}>
                    <div className="startup-check-icon">
                      {item.status === "ok" ? <CheckCircle2 size={18} /> : <AlertCircle size={18} />}
                    </div>
                    <div>
                      <strong>{item.label}</strong>
                      <p>{lang === "zh" ? item.message : item.status === "ok" ? "Detected" : item.status === "manual" ? "Manual selection required" : "Not found"}</p>
                      {item.path && <code>{item.path}</code>}
                    </div>
                  </div>
                ))}
              </div>

              <div className="startup-actions">
                <button className="secondary-btn" onClick={() => { setTab("settings"); closeStartupWizard(); }}>
                  <Settings size={16} /> {lang === "zh" ? "去设置" : "Settings"}
                </button>
                <button className="primary-btn" onClick={closeStartupWizard}>
                  <CheckCircle2 size={16} /> {lang === "zh" ? "进入 Codex-X" : "Enter Codex-X"}
                </button>
              </div>
            </div>
          </div>
        )}

        {(!state || bootVisible) ? (
          <div className={cx("panel glass center-panel boot-panel", bootLeaving && "leaving")}>
            <div className="boot-logo-wrap">
              <div className="boot-logo">Codex-X</div>
              <div className="boot-orbit" />
              <div className="boot-orbit boot-orbit-secondary" />
            </div>
            <p className="boot-hint" key={bootHintIndex}>{bootHints[lang][bootHintIndex]}</p>
            <div className="boot-progress"><span /></div>
          </div>
        ) : (
          <>
            {tab === "dashboard" && (
              <>
                {releaseInfo.status === "ok" && releaseInfo.hasUpdate && (
                  <div className="update-strip glass">
                    <div>
                      <span className="update-dot" />
                      <strong>{lang === "zh" ? "发现新版本" : "New version found"}</strong>
                      <p>{lang === "zh" ? `Codex-X ${releaseInfo.latestVersion || ""} 已发布` : `Codex-X ${releaseInfo.latestVersion || ""} is available`}</p>
                    </div>
                    <button className="secondary-btn small" onClick={() => openExternalUrl(releaseInfo.htmlUrl)}>
                      {lang === "zh" ? "查看更新" : "View"}
                    </button>
                  </div>
                )}
              <div className="grid dashboard-grid">
                <StatCard icon={<TerminalSquare size={20} />} label={t.dashboard.config} value={state.configExists ? t.dashboard.found : t.dashboard.missing} ok={state.configExists} />
                <StatCard icon={<Code2 size={20} />} label={t.dashboard.provider} value={currentProvider?.name || state.modelProvider || t.dashboard.officialDefault} ok={Boolean(state.modelProvider)} />
                <StatCard icon={<Sparkles size={20} />} label={t.dashboard.instruction} value={state.instructionEnabled ? t.dashboard.enabled : t.dashboard.disabled} ok={state.instructionEnabled} />
                <StatCard icon={<KeyRound size={20} />} label={t.dashboard.auth} value={state.authExists ? t.authJson : t.noAuth} ok={state.authExists} />

                <section className="panel glass wide">
                  <div className="panel-head">
                    <div>
                      <p className="eyebrow">{t.dashboard.liveStatus}</p>
                      <h3>{t.dashboard.currentConfig}</h3>
                    </div>
                    <StatusPill active={state.instructionEnabled} label={state.instructionEnabled ? "Instruct ON" : "Instruct OFF"} />
                  </div>
                  <div className="kv-list">
                    <div><span>{t.dashboard.dir}</span><code>{state.codexDir}</code></div>
                    <div><span>{t.dashboard.configPath}</span><code>{state.configPath}</code></div>
                    <div><span>{t.dashboard.model}</span><code>{state.model || t.dashboard.notSet}</code></div>
                    <div><span>{t.dashboard.providerName}</span><code>{state.modelProvider || t.dashboard.officialDefault}</code></div>
                    <div><span>{t.dashboard.instructionFile}</span><code>{state.instructionFile || t.dashboard.notSet}</code></div>
                  </div>
                </section>

                <section className="panel glass">
                  <div className="panel-head compact"><h3>{t.dashboard.quickActions}</h3></div>
                  <div className="action-stack">
                    <button className="primary-btn big" onClick={enableInstruction} disabled={loading}><Power size={18} /> {t.dashboard.enableInstruction}</button>
                    <button className="secondary-btn big" onClick={disableInstruction} disabled={loading}><RotateCcw size={18} /> {t.dashboard.disableInstruction}</button>
                    {backups[0] && <button className="ghost-btn big" onClick={() => restoreBackup(backups[0].id)} disabled={loading}><History size={18} /> {t.dashboard.restoreLatest}</button>}
                  </div>
                </section>
              </div>
              </>
            )}

            {tab === "provider" && (
              <section className={cx("panel glass provider-panel", providerMode !== "list" && "provider-edit-panel")}> 
                {providerMode === "list" ? (
                  <>
                    <div className="panel-head provider-title-row">
                      <div>
                        <p className="eyebrow">Provider</p>
                        <h3>{t.provider.title}</h3>
                        <p className="muted-desc">{t.provider.subtitle}</p>
                      </div>
                      <div className="provider-title-actions">
                        <button className="secondary-btn add-provider-btn" onClick={importFromCcSwitch} disabled={loading}><RefreshCw size={18} /> {t.provider.importCc}</button>
                        <button className="primary-btn add-provider-btn" onClick={openAddProvider}><Plus size={18} /> {t.provider.add}</button>
                      </div>
                    </div>

                    <div className="provider-list-frame">
                      <div className="provider-row-list">
                        {providerRows.length === 0 && <p className="empty">{t.provider.noProviders}</p>}
                        {providerRows.map((p) => {
                          const local = p.source === "official"
                            ? undefined
                            : savedProviders.find((item) =>
                              p.source === "local"
                                ? item.id === p.id
                                : item.baseUrl === p.baseUrl && item.model === p.model,
                            );
                          const switchable: SavedProvider | null = p.source === "official" ? null : local || {
                            id: customProviderId(p.providerName),
                            providerName: p.providerName,
                            baseUrl: p.baseUrl,
                            model: p.model,
                            apiKey: "",
                            tomlConfig: "",
                            wireApi: p.wireApi,
                            requiresOpenaiAuth: p.requiresOpenaiAuth,
                          };
                          return (
                            <div className={cx("provider-row", p.isCurrent && "selected")} key={`${p.source}-${p.id}-${p.baseUrl}`}>
                              <div className="drag-dot">⋮⋮</div>
                              {p.source === "official" ? <OpenAIIcon /> : <Avatar name={p.providerName} />}
                              <div className="provider-main">
                                <strong>{p.providerName}</strong>
                                <a>{p.baseUrl || "no base_url"}</a>
                              </div>
                              <div className="provider-badges">
                                {p.isCurrent && <StatusPill active label={t.provider.current} />}
                              </div>
                              <div className="provider-actions">
                                <button className="secondary-btn small" onClick={() => switchable ? switchProvider(switchable) : switchOfficialProvider()} disabled={loading || p.isCurrent}>{lang === "zh" ? "启用" : "Enable"}</button>
                                {p.source !== "official" && (
                                  <button className="icon-btn small" title={lang === "zh" ? "测试连接" : "Test connection"} onClick={() => void testProvider(`${p.source}-${p.id}`, p.baseUrl, local?.apiKey || (p.source === "local" ? p.apiKey : null))} disabled={loading || providerTestingId === `${p.source}-${p.id}`}>
                                    {providerTestingId === `${p.source}-${p.id}` ? <Loader2 size={15} className="spin" /> : <Activity size={15} />}
                                  </button>
                                )}
                                {p.source === "official" && <button className="icon-btn small" title={t.provider.edit} onClick={openOfficialEdit}><PencilLine size={15} /></button>}
                                {local && <button className="icon-btn small" title={t.provider.edit} onClick={() => openEditProvider(local)}><PencilLine size={15} /></button>}
                                {!local && p.source === "detected" && <button className="icon-btn small" title={t.provider.edit} onClick={() => openEditDetectedProvider(p)}><PencilLine size={15} /></button>}
                                {local && <button className="icon-btn danger small" title={t.provider.remove} onClick={() => removeProvider(local.id)}><Trash2 size={15} /></button>}
                              </div>
                            </div>
                          );
                        })}
                      </div>
                    </div>

                  </>
                ) : providerMode === "official" ? (
                  <div className="provider-form-page">
                    <div className="panel-head">
                      <div>
                        <p className="eyebrow">OpenAI Official</p>
                        <h3>{t.provider.officialEdit}</h3>
                        <p className="muted-desc">{t.provider.officialHint}</p>
                      </div>
                      <button className="ghost-btn" onClick={() => setProviderMode("list")}>{t.provider.cancel}</button>
                    </div>
                    <div className="official-info-card">
                      <div><span>{t.provider.officialUrl}</span><code>https://chatgpt.com/codex</code></div>
                      <div><span>auth.json</span><code>{state.authPath}</code></div>
                      <div><span>{t.provider.current}</span><code>{(!state.modelProvider || state.modelProvider === "openai") ? "OpenAI Official" : state.modelProvider}</code></div>
                    </div>
                    <div className="form-grid provider-form-grid">
                      <Field label={t.provider.model}><input value={officialForm.model} onChange={(e) => setOfficialForm({ ...officialForm, model: e.target.value })} /></Field>
                    </div>
                    <label className="field official-auth-field">
                      <span>auth.json (JSON)</span>
                      <textarea className="official-auth-editor" value={officialForm.authJson} onChange={(e) => setOfficialForm({ ...officialForm, authJson: e.target.value })} spellCheck={false} />
                    </label>
                    <div className="form-actions">
                      <button className="ghost-btn big" onClick={() => void refreshLiveOfficialAuth(true)} disabled={loading}>{t.provider.refreshOfficialAuth}</button>
                      <button className="ghost-btn big" onClick={() => void loadCcSwitchOfficialAuth(true)} disabled={loading}>{t.provider.loadOfficialAuth}</button>
                      <button className="secondary-btn big" onClick={() => setProviderMode("list")}>{t.provider.cancel}</button>
                      <button className="primary-btn big" onClick={saveOfficialConfig} disabled={loading}>保存官方配置</button>
                    </div>
                  </div>
                ) : (
                  <div className="provider-form-page">
                    <div className="panel-head">
                      <div>
                        <p className="eyebrow">Provider</p>
                        <h3>{editingProviderId ? t.provider.formEdit : t.provider.formAdd}</h3>
                        <p className="muted-desc">{t.provider.formHint}</p>
                      </div>
                      <button className="ghost-btn" onClick={() => setProviderMode("list")}>{t.provider.cancel}</button>
                    </div>
                    <div className="provider-edit-stack">
                      <section className="provider-section provider-api-section unified-section">
                        <div className="section-title-row">
                          <div>
                            <strong>{lang === "zh" ? "供应商 API 配置" : "Provider API config"}</strong>
                            <p>{lang === "zh" ? "和 cc-switch 一样，API 信息、auth.json、config.toml 在同一个编辑页纵向展示。" : "API fields, auth.json and config.toml are shown vertically in one edit page."}</p>
                          </div>
                        </div>
                        <div className="form-grid provider-form-grid provider-form-cc">
                          <Field label={t.provider.apiKey}>
                            <div className="secret-input-wrap">
                              <input
                                type={providerApiKeyVisible ? "text" : "password"}
                                value={providerForm.apiKey || ""}
                                onChange={(e) => setProviderForm({ ...providerForm, apiKey: e.target.value })}
                                placeholder={t.provider.apiKeyPlaceholder}
                              />
                              <button
                                type="button"
                                className="secret-toggle"
                                onClick={() => setProviderApiKeyVisible((value) => !value)}
                                aria-label={providerApiKeyVisible ? (lang === "zh" ? "隐藏 API Key" : "Hide API Key") : (lang === "zh" ? "显示 API Key" : "Show API Key")}
                              >
                                {providerApiKeyVisible ? <EyeOff size={16} /> : <Eye size={16} />}
                              </button>
                            </div>
                          </Field>
                          <Field label={lang === "zh" ? "API 请求地址" : t.provider.baseUrl}><input value={providerForm.baseUrl} onChange={(e) => setProviderForm({ ...providerForm, baseUrl: e.target.value })} /></Field>
                          <Field label={t.provider.name}><input value={providerForm.providerName} onChange={(e) => setProviderForm({ ...providerForm, providerName: e.target.value, id: editingProviderId || customProviderId(e.target.value) })} /></Field>
                          <Field label={t.provider.model}><input value={providerForm.model} onChange={(e) => setProviderForm({ ...providerForm, model: e.target.value })} /></Field>
                          <Field label={t.provider.wireApi}>
                            <select value={providerForm.wireApi} onChange={(e) => setProviderForm({ ...providerForm, wireApi: e.target.value })}>
                              <option value="responses">responses</option>
                              <option value="chat">chat</option>
                            </select>
                          </Field>
                          <label className="check-row"><input type="checkbox" checked={providerForm.requiresOpenaiAuth} onChange={(e) => setProviderForm({ ...providerForm, requiresOpenaiAuth: e.target.checked })} /><span>{t.provider.requiresAuth}</span></label>
                        </div>
                      </section>

                      <section className="provider-section provider-auth-section unified-section">
                        <div className="section-title-row">
                          <div>
                            <strong>auth.json (JSON)</strong>
                            <p>{lang === "zh" ? "预览保存时会写入/保留的认证配置；API Key 留空时不会覆盖现有 auth.json。" : "Preview of auth config. Empty API key will not overwrite the existing auth.json."}</p>
                          </div>
                        </div>
                        <JsonPreview text={providerAuthPreview} />
                      </section>

                      <section className="provider-section provider-toml-section unified-section">
                        <div className="section-title-row config-title-row">
                          <div>
                            <strong>config.toml (TOML)</strong>
                            <p>{lang === "zh" ? "可直接编辑为供应商模板；点击启用时才写入 Codex live config.toml。" : "Editable provider template. It is written to the live Codex config.toml only when enabled."}</p>
                          </div>
                          <button className="ghost-btn small" onClick={() => { setProviderTomlDraft(providerTomlPreview); setProviderTomlDirty(false); }}>{lang === "zh" ? "重置生成" : "Reset"}</button>
                        </div>
                        <textarea
                          className="provider-toml-editor"
                          value={providerTomlDraft}
                          onChange={(e) => { setProviderTomlDraft(e.target.value); setProviderTomlDirty(true); }}
                          spellCheck={false}
                        />
                      </section>

                      <div className="form-actions provider-save-actions">
                        <button className="primary-btn big lively-btn" onClick={saveProviderConfig} disabled={loading}>{loading ? <Loader2 size={18} className="spin" /> : <CheckCircle2 size={18} />} {loading ? (lang === "zh" ? "保存中..." : "Saving...") : t.provider.saveAndSwitch}</button>
                      </div>
                    </div>
                  </div>
                )}
              </section>
            )}

            {(tab === "sessions" || visitedTabs.has("sessions")) && (
              <section className={cx("panel glass sessions-panel", tab !== "sessions" && "page-pane-hidden")}>
                <div className="panel-head provider-title-row">
                  <div>
                    <p className="eyebrow">Provider Sync</p>
                    <h3>{lang === "zh" ? "会话管理" : "Session management"}</h3>
                    <p className="muted-desc">
                      {lang === "zh"
                        ? "检查并修复 Codex 本地历史会话的 Provider 元数据，让切换供应商后旧 thread 仍能被原生 Codex 识别、打开和续聊。"
                        : "Check and repair local Codex session provider metadata so old threads stay visible and resumable after provider switching."}
                    </p>
                  </div>
                  <div className="provider-title-actions session-title-actions">
                    <label className="session-auto-toggle" title={lang === "zh" ? "启动 Codex-X 后在后台检查会话；发现未同步时自动修复" : "Check sessions on startup in the background and repair when needed"}>
                      <input type="checkbox" checked={autoSessionSync} onChange={(e) => setAutoSessionSync(e.target.checked)} />
                      <span>{lang === "zh" ? "启动自动修复" : "Auto repair on startup"}</span>
                      {autoSessionSyncBusy && <Loader2 size={14} className="spin" />}
                    </label>
                    <span className="session-provider-chip">
                      {lang === "zh" ? "目标" : "Target"}: {sessionStatus?.targetProvider || state.modelProvider || "openai"}
                    </span>
                    <button className="secondary-btn add-provider-btn lively-btn" onClick={checkSessions} disabled={loading}>
                      {actionBusy === "checkSessions" ? <Loader2 size={18} className="spin" /> : <RefreshCw size={18} />} {actionBusy === "checkSessions" ? (lang === "zh" ? "检查中..." : "Checking...") : (lang === "zh" ? "检查会话" : "Check")}
                    </button>
                    <button className="primary-btn add-provider-btn lively-btn" onClick={syncSessions} disabled={loading || !sessionStatus?.needsSync}>
                      {actionBusy === "syncSessions" ? <Loader2 size={18} className="spin" /> : <Zap size={18} />} {actionBusy === "syncSessions" ? (lang === "zh" ? "修复中..." : "Repairing...") : (lang === "zh" ? "同步 / 修复" : "Sync / repair")}
                    </button>
                  </div>
                </div>

                <div className={cx("session-compact-summary", sessionStatus?.needsSync ? "needs-sync" : "synced")}>
                  <span>{sessionStatus?.needsSync ? <AlertCircle size={16} /> : <CheckCircle2 size={16} />} {sessionStatus?.needsSync ? (lang === "zh" ? "发现未同步" : "Unsynced") : (lang === "zh" ? "会话已同步" : "Synced")}</span>
                  <span>{lang === "zh" ? "聊天总数" : "Total"} <strong>{sessionStatus?.sqliteThreads ?? "-"}</strong></span>
                  <span>{lang === "zh" ? "已展示" : "Shown"} <strong>{filteredSessions.length}</strong></span>
                  <span>{lang === "zh" ? "需修复" : "Need repair"} <strong>{sessionStatus ? unsyncedChatCount : "-"}</strong></span>
                  <span>{lang === "zh" ? "状态" : "Status"} <strong>{sessionStatus?.needsSync ? (lang === "zh" ? "可修复" : "Repairable") : (lang === "zh" ? "正常" : "OK")}</strong></span>
                </div>

                <div className="session-list-card">
                  <div className="session-list-head session-list-head-rich">
                    <div>
                      <p className="eyebrow">{lang === "zh" ? "本地会话" : "Local threads"}</p>
                      <h4>{lang === "zh" ? "会话列表" : "Sessions"}</h4>
                    </div>
                    <span>{lang === "zh" ? `展示 ${filteredSessions.length} / ${sessionStatus?.sqliteThreads ?? 0} 条` : `${filteredSessions.length} / ${sessionStatus?.sqliteThreads ?? 0} shown`}</span>
                  </div>

                  <div className="session-toolbar">
                    <label className="session-search">
                      <Search size={16} />
                      <input
                        value={sessionQuery}
                        onChange={(e) => setSessionQuery(e.target.value)}
                        placeholder={lang === "zh" ? "搜索标题 / cwd / Provider / ID" : "Search title / cwd / Provider / ID"}
                      />
                    </label>
                    <label className="session-toggle">
                      <input type="checkbox" checked={sessionGroupByCwd} onChange={(e) => setSessionGroupByCwd(e.target.checked)} />
                      <span>{lang === "zh" ? "按项目路径分组" : "Group by cwd"}</span>
                    </label>
                    <span className="session-selected-hint">
                      {lang === "zh"
                        ? `已选 ${selectedSessionIds.length} 条 · 需修复 ${selectedNeedsSyncCount} 条`
                        : `${selectedSessionIds.length} selected · ${selectedNeedsSyncCount} repairable`}
                    </span>
                    <button className="ghost-btn small" onClick={selectVisibleNeedsSyncSessions} disabled={!filteredSessions.some((item) => item.needsSync)}>
                      {lang === "zh" ? "选择需同步" : "Select unsynced"}
                    </button>
                    <button className="secondary-btn small" onClick={() => setSelectedSessionIds([])} disabled={!selectedSessionIds.length}>
                      {lang === "zh" ? "清空选择" : "Clear"}
                    </button>
                    <button className="primary-btn small lively-btn" onClick={syncSelectedSessions} disabled={loading || selectedNeedsSyncCount === 0}>
                      {actionBusy === "syncSelectedSessions" ? <Loader2 size={15} className="spin" /> : <Zap size={15} />} {actionBusy === "syncSelectedSessions" ? (lang === "zh" ? "修复中..." : "Repairing...") : (lang === "zh" ? `修复选中 ${selectedNeedsSyncCount}` : `Repair ${selectedNeedsSyncCount}`)}
                    </button>
                  </div>

                  {filteredSessions.length ? (
                    <div className="session-list enhanced-session-list">
                      {groupedSessions.map(([group, items]) => (
                        <div className="session-group" key={group}>
                          <div className="session-group-title">
                            <span title={group}>{compactPath(group, 88)}</span>
                            <em>{items.length}</em>
                          </div>
                          {items.map((item) => {
                            const providerOk = !item.needsSync;
                            return (
                              <article
                                className={cx("session-row", item.needsSync && "needs-sync", selectedSessionSet.has(item.id) && "selected")}
                                key={item.id}
                                onClick={() => toggleSessionSelected(item.id)}
                              >
                                <label className="session-select-box" title={lang === "zh" ? "选择这个会话" : "Select this session"} onClick={(e) => e.stopPropagation()}>
                                  <input
                                    type="checkbox"
                                    checked={selectedSessionSet.has(item.id)}
                                    onChange={() => toggleSessionSelected(item.id)}
                                  />
                                </label>
                                <div className="session-row-main">
                                  <div className="session-thread-icon"><History size={18} /></div>
                                  <div className="session-row-text">
                                    <div className="session-row-title">
                                      <strong>{item.title}</strong>
                                      {item.archived && <span className="mini-tag">{lang === "zh" ? "已归档" : "Archived"}</span>}
                                      <span className={cx("mini-tag", providerOk ? "ok" : "warn")}>{providerOk ? (lang === "zh" ? "Provider 一致" : "Provider OK") : (lang === "zh" ? "需同步" : "Needs sync")}</span>
                                    </div>
                                    <p title={item.cwd || item.rolloutPath || undefined}>{compactPath(item.cwd || item.rolloutPath)}</p>
                                  </div>
                                </div>
                                <div className="session-row-meta">
                                  <span title={item.updatedAtMs ? new Date(item.updatedAtMs).toLocaleString() : undefined}>{formatSessionTime(item.updatedAtMs)}</span>
                                  <code title={item.modelProvider || undefined}>{item.modelProvider || "unknown"}</code>
                                  <em title={item.model || undefined}>{item.model || "model -"}</em>
                                  <small>#{shortId(item.id)}</small>
                                </div>
                              </article>
                            );
                          })}
                        </div>
                      ))}
                    </div>
                  ) : (
                    <div className="session-empty">
                      <History size={22} />
                      <span>{sessionQuery ? (lang === "zh" ? "没有匹配的会话。" : "No matching sessions.") : (lang === "zh" ? "还没有读取到会话。点击右上角“检查会话”刷新。" : "No sessions loaded. Click Check to refresh.")}</span>
                    </div>
                  )}
                </div>

                {sessionStatus?.warnings?.length ? (
                  <div className="session-warning-list">
                    {sessionStatus.warnings.map((item, index) => <p key={index}><AlertCircle size={15} /> {item}</p>)}
                  </div>
                ) : null}
              </section>
            )}

            {(tab === "skillsMcp" || visitedTabs.has("skillsMcp")) && (
              <section className={cx("panel glass skills-mcp-panel", tab !== "skillsMcp" && "page-pane-hidden")}>
                <div className="panel-head provider-title-row">
                  <div>
                    <p className="eyebrow">Skills / MCP</p>
                    <h3>{lang === "zh" ? "技能和 MCP" : "Skills & MCP"}</h3>
                    <p className="muted-desc">{lang === "zh" ? "管理 Codex 当前可用的 Skills 与 MCP：导入已有、从 ZIP 安装、启用或禁用。" : "Manage Codex Skills and MCP servers: import existing items, install ZIP packages, enable or disable."}</p>
                  </div>
                  <div className="provider-title-actions">
                    <input
                      ref={skillZipImportRef}
                      className="hidden-file-input"
                      type="file"
                      accept=".zip,application/zip"
                      onChange={(e) => void installSkillZipFile(e.target.files?.[0])}
                    />
                    <button className="ghost-btn add-provider-btn lively-btn" onClick={() => void loadSkillsMcp()} disabled={actionBusy === "loadSkillsMcp"}>
                      {actionBusy === "loadSkillsMcp" ? <Loader2 size={18} className="spin" /> : <RefreshCw size={18} />} {lang === "zh" ? "刷新" : "Refresh"}
                    </button>
                    <button className="secondary-btn add-provider-btn lively-btn" onClick={openImportExistingSkillsMcpPreview} disabled={Boolean(actionBusy)}>
                      {actionBusy === "previewExistingSkillsMcp" ? <Loader2 size={18} className="spin" /> : <Download size={18} />} {lang === "zh" ? "导入已有" : "Import existing"}
                    </button>
                    <button className="secondary-btn add-provider-btn lively-btn" onClick={() => skillZipImportRef.current?.click()} disabled={Boolean(actionBusy)}>
                      {actionBusy === "installSkillZip" ? <Loader2 size={18} className="spin" /> : <Upload size={18} />} {lang === "zh" ? "从 ZIP 安装" : "Install ZIP"}
                    </button>
                    <button className="primary-btn add-provider-btn lively-btn" onClick={checkSkillUpdatesAction} disabled={Boolean(actionBusy)}>
                      {actionBusy === "checkSkillUpdates" ? <Loader2 size={18} className="spin" /> : <Sparkles size={18} />} {lang === "zh" ? "检查更新" : "Check updates"}
                    </button>
                  </div>
                </div>

                {!skillsMcpState ? (
                  <div className="session-empty skills-loading">
                    <Loader2 className="spin" size={22} />
                    <span>{lang === "zh" ? "正在读取本地 Skills / MCP..." : "Loading local Skills / MCP..."}</span>
                  </div>
                ) : (
                  <>
                    <div className="skills-mcp-tabs" role="tablist" aria-label="Skills and MCP">
                      <button
                        className={cx("skills-mcp-tab", skillsMcpTab === "mcp" && "active")}
                        onClick={() => setSkillsMcpTab("mcp")}
                        role="tab"
                        aria-selected={skillsMcpTab === "mcp"}
                      >
                        MCP <span>{skillsMcpState.mcpServers.length}</span>
                      </button>
                      <button
                        className={cx("skills-mcp-tab", skillsMcpTab === "skills" && "active")}
                        onClick={() => setSkillsMcpTab("skills")}
                        role="tab"
                        aria-selected={skillsMcpTab === "skills"}
                      >
                        Skills <span>{skillsMcpState.skills.length}</span>
                      </button>
                    </div>

                    <p className="skills-mcp-help">
                      {skillsMcpTab === "mcp"
                        ? (lang === "zh" ? `当前共有 ${skillsMcpState.mcpServers.length} 个 MCP；开启后会写入 Codex config.toml。` : `${skillsMcpState.mcpServers.length} MCP server(s). Enabling writes them to Codex config.toml.`)
                        : (lang === "zh" ? `当前共有 ${skillsMcpState.skills.length} 个 Skills；开启后会放入 Codex skills 目录。` : `${skillsMcpState.skills.length} Skill(s). Enabling moves them into the Codex skills directory.`)}
                    </p>

                    <div className="skills-mcp-list-card">
                      <div className="skills-mcp-list-head">
                        <strong>{skillsMcpTab === "mcp" ? "MCP" : "Skills"}</strong>
                        <span>{lang === "zh" ? `共 ${activeSkillsMcpCount} 个` : `${activeSkillsMcpCount} total`}</span>
                      </div>

                      {skillsMcpTab === "mcp" ? (
                        skillsMcpState.mcpServers.length === 0 ? (
                          <div className="session-empty compact"><span>{lang === "zh" ? "还没有发现 MCP，点击导入已有。" : "No MCP server found. Import existing items first."}</span></div>
                        ) : (
                          <div className="skills-mcp-simple-list">
                            {skillsMcpState.mcpServers.map((server) => (
                              <article className="skills-mcp-simple-row" key={server.id}>
                                <strong>{server.name || server.id}</strong>
                                <button
                                  className={cx("switch-toggle", server.enabled && "on")}
                                  onClick={() => void toggleMcpEnabled(server.id, !server.enabled)}
                                  disabled={Boolean(actionBusy) && actionBusy !== `mcp:${server.id}`}
                                  aria-label={server.enabled ? (lang === "zh" ? "关闭 MCP" : "Disable MCP") : (lang === "zh" ? "开启 MCP" : "Enable MCP")}
                                  aria-pressed={server.enabled}
                                >
                                  {actionBusy === `mcp:${server.id}` ? <Loader2 size={14} className="spin" /> : <span />}
                                </button>
                              </article>
                            ))}
                          </div>
                        )
                      ) : (
                        skillsMcpState.skills.length === 0 ? (
                          <div className="session-empty compact"><span>{lang === "zh" ? "还没有发现 Skills，点击导入已有或从 ZIP 安装。" : "No Skills found. Import existing items or install a ZIP."}</span></div>
                        ) : (
                          <div className="skills-mcp-simple-list">
                            {skillsMcpState.skills.map((skill) => (
                              <article className="skills-mcp-simple-row" key={skill.id}>
                                <strong>{skill.name || skill.directory}</strong>
                                <button
                                  className={cx("switch-toggle", skill.enabled && "on")}
                                  onClick={() => void toggleSkillEnabled(skill.id, !skill.enabled)}
                                  disabled={Boolean(actionBusy) && actionBusy !== `skill:${skill.id}`}
                                  aria-label={skill.enabled ? (lang === "zh" ? "禁用 Skill" : "Disable Skill") : (lang === "zh" ? "启用 Skill" : "Enable Skill")}
                                  aria-pressed={skill.enabled}
                                >
                                  {actionBusy === `skill:${skill.id}` ? <Loader2 size={14} className="spin" /> : <span />}
                                </button>
                              </article>
                            ))}
                          </div>
                        )
                      )}
                    </div>

                    {skillsMcpState.warnings.length > 0 && (
                      <div className="session-warning-list">
                        {skillsMcpState.warnings.map((item, index) => <p key={index}><AlertCircle size={15} /> {item}</p>)}
                      </div>
                    )}
                  </>
                )}
              </section>
            )}

            {tab === "instruction" && (
              <section className="panel glass instruction-panel simple-instruction-panel">
                {instructionMode === "list" ? (
                  <>
                    <div className="panel-head provider-title-row">
                      <div>
                        <p className="eyebrow">model_instructions_file</p>
                        <h3>{t.instruction.title}</h3>
                      </div>
                      <div className="provider-title-actions">
                        <input
                          ref={promptImportRef}
                          className="hidden-file-input"
                          type="file"
                          accept=".md,text/markdown,text/plain"
                          onChange={(e) => void importPromptMd(e.target.files?.[0])}
                        />
                        <button className="ghost-btn add-provider-btn lively-btn" onClick={() => void refreshBuiltinPrompts()} disabled={loading || actionBusy === "refreshPrompts"}>
                          {actionBusy === "refreshPrompts" ? <Loader2 size={18} className="spin" /> : <RefreshCw size={18} />} {actionBusy === "refreshPrompts" ? (lang === "zh" ? "更新中..." : "Updating...") : (lang === "zh" ? "更新内置模板" : "Update built-ins")}
                        </button>
                        <button className="secondary-btn add-provider-btn lively-btn" onClick={() => promptImportRef.current?.click()} disabled={loading}>
                          {actionBusy === "importPrompt" ? <Loader2 size={18} className="spin" /> : <Upload size={18} />} {actionBusy === "importPrompt" ? (lang === "zh" ? "导入中..." : "Importing...") : (lang === "zh" ? "导入 md" : "Import md")}
                        </button>
                        <button className="primary-btn add-provider-btn lively-btn" onClick={openAddPrompt}><Plus size={18} /> {lang === "zh" ? "添加提示词" : "Add prompt"}</button>
                      </div>
                    </div>

                    <div className="skills-mcp-simple-list instruction-switch-list">
                      {instructionTemplates.map((item) => {
                        const isCurrent = currentInstructionId === item.id;
                        const remoteStatus = builtinPromptStatusMap.get(item.id);
                        const sourceLabel = remoteStatus?.contentSource === "github"
                          ? (lang === "zh" ? "GitHub 最新" : "GitHub latest")
                          : remoteStatus?.contentSource === "cache"
                            ? (lang === "zh" ? "本地缓存" : "Local cache")
                            : (lang === "zh" ? "打包内置" : "Bundled");
                        return (
                          <article className={cx("skills-mcp-simple-row instruction-switch-row", isCurrent && "selected")} key={item.id}>
                            <div className="instruction-main">
                              <strong>{item.title}</strong>
                              <p>{item.subtitle}</p>
                              <div className="prompt-remote-meta" title={remoteStatus?.message || undefined}>
                                <span className={cx("mini-tag", remoteStatus?.contentSource === "github" ? "ok" : undefined)}>{sourceLabel}</span>
                                {remoteStatus?.checkedAt && <small>{new Date(remoteStatus.checkedAt).toLocaleString()}</small>}
                              </div>
                            </div>
                            <button
                              className={cx("switch-toggle", isCurrent && "on")}
                              title={isCurrent ? (lang === "zh" ? "关闭" : "Disable") : (lang === "zh" ? "启用" : "Enable")}
                              onClick={() => isCurrent ? disableInstruction() : switchInstructionTemplate(item.id)}
                              disabled={loading}
                            >
                              <span />
                            </button>
                          </article>
                        );
                      })}

                      {savedPrompts.map((prompt) => {
                        const isCurrent = Boolean(currentInstructionFilename) && currentInstructionFilename === prompt.filename;
                        return (
                          <article className={cx("skills-mcp-simple-row instruction-switch-row", isCurrent && "selected")} key={prompt.id}>
                            <div className="instruction-main">
                              <strong>{prompt.title}</strong>
                              <p>{lang === "zh" ? "自定义指令提示词" : "Custom instruction prompt"}</p>
                            </div>
                            <div className="instruction-icon-actions">
                              <button
                                className={cx("switch-toggle", isCurrent && "on")}
                                title={isCurrent ? (lang === "zh" ? "关闭" : "Disable") : (lang === "zh" ? "启用" : "Enable")}
                                onClick={() => isCurrent ? disableInstruction() : enableSavedPrompt(prompt.id)}
                                disabled={loading}
                              >
                                <span />
                              </button>
                              <button className="icon-btn small" title={t.provider.edit} onClick={() => openEditPrompt(prompt)}><PencilLine size={15} /></button>
                              <button className="icon-btn danger small" title={t.provider.remove} onClick={() => removeSavedPrompt(prompt.id)}><Trash2 size={15} /></button>
                            </div>
                          </article>
                        );
                      })}

                      {state.instructionFile && currentInstructionId === "custom" && !savedPrompts.some((p) => currentInstructionFilename === p.filename) && (
                        <article className="skills-mcp-simple-row instruction-switch-row selected">
                          <div className="instruction-main">
                            <strong>{lang === "zh" ? "当前自定义指令提示词" : "Current custom prompt"}</strong>
                            <p>{lang === "zh" ? "当前使用的是外部提示词；切换到内置模板前会自动保存到下方列表，之后仍可重新启用。" : "An external prompt is currently active. It will be saved before switching to a built-in template so you can enable it again later."}</p>
                          </div>
                          <button className="switch-toggle on" title={lang === "zh" ? "关闭" : "Disable"} onClick={disableInstruction} disabled={loading}><span /></button>
                        </article>
                      )}
                    </div>
                  </>
                ) : (
                  <div className="prompt-form-page">
                    <div className="panel-head">
                      <div>
                        <p className="eyebrow">Prompt</p>
                        <h3>{editingPromptId ? (lang === "zh" ? "编辑提示词" : "Edit prompt") : (lang === "zh" ? "添加提示词" : "Add prompt")}</h3>
                      </div>
                      <button className="ghost-btn" onClick={() => setInstructionMode("list")}>{t.provider.cancel}</button>
                    </div>
                    <div className="form-grid prompt-form-grid">
                      <Field label={lang === "zh" ? "提示词名称" : "Prompt name"}><input value={promptForm.title} onChange={(e) => setPromptForm({ ...promptForm, title: e.target.value, id: editingPromptId || providerId(e.target.value) })} /></Field>
                      <Field label={lang === "zh" ? "文件名" : "Filename"}><input value={promptForm.filename} onChange={(e) => setPromptForm({ ...promptForm, filename: e.target.value })} placeholder="my-prompt.md" /></Field>
                      <label className="field prompt-content-field">
                        <span>{lang === "zh" ? "提示词内容" : "Prompt content"}</span>
                        <textarea className="prompt-editor" value={promptForm.content} onChange={(e) => setPromptForm({ ...promptForm, content: e.target.value })} spellCheck={false} />
                      </label>
                    </div>
                    <div className="form-actions">
                      <button className="secondary-btn big lively-btn" onClick={savePromptOnly} disabled={loading}>{lang === "zh" ? "保存" : "Save"}</button>
                      <button className="primary-btn big lively-btn" onClick={saveAndEnablePrompt} disabled={loading}><Zap size={18} /> {lang === "zh" ? "保存并启用" : "Save & enable"}</button>
                    </div>
                  </div>
                )}
              </section>
            )}

            {tab === "toml" && (
              <section className="panel glass code-panel">
                <div className="panel-head">
                  <div>
                    <p className="eyebrow">~/.codex/config.toml</p>
                    <h3>{t.toml.title}</h3>
                    <p className="muted-desc">{t.toml.desc}</p>
                  </div>
                  <StatusPill active={state.configExists} label={state.configExists ? t.toml.loaded : t.dashboard.missing} />
                </div>
                <TomlPreview text={state.configText || t.toml.missingText} />
              </section>
            )}



            {tab === "about" && (
              <section className="about-page">
                <section className="panel glass about-card">
                  <div className="panel-head compact">
                    <div><p className="eyebrow">About</p><h3>{lang === "zh" ? "关于 Codex-X" : "About Codex-X"}</h3></div>
                  </div>
                  <div className="about-kv">
                    <div><span>Codex-X {lang === "zh" ? "版本" : "Version"}</span><strong>{aboutInfo?.appVersion || "0.2.0"}</strong></div>
                    <div><span>Codex {lang === "zh" ? "版本" : "Version"}</span><strong>{aboutInfo?.codexVersion || (lang === "zh" ? "未检测到" : "Not detected")}</strong></div>
                    <div><span>CODEX_HOME</span><code>{aboutInfo?.codexDir || state.codexDir}</code></div>
                    <div><span>{lang === "zh" ? "项目地址" : "Project"}</span><code>{aboutInfo?.projectUrl || `https://github.com/${FALLBACK_GITHUB_REPO}`}</code></div>
                  </div>
                  <div className="about-actions">
                    <button className="secondary-btn" onClick={() => openExternalUrl(aboutInfo?.projectUrl || `https://github.com/${FALLBACK_GITHUB_REPO}`)}><ExternalLink size={16} /> {lang === "zh" ? "打开项目主页" : "Open project"}</button>
                    <button className="ghost-btn" onClick={() => openExternalUrl(`${aboutInfo?.projectUrl || `https://github.com/${FALLBACK_GITHUB_REPO}`}/issues`)}><ExternalLink size={16} /> {lang === "zh" ? "反馈问题" : "Issues"}</button>
                  </div>
                </section>

                <section className="panel glass about-card">
                  <div className="panel-head compact">
                    <div><p className="eyebrow">GitHub Releases</p><h3>{lang === "zh" ? "更新检查" : "Update check"}</h3></div>
                    <span className={cx("update-status-pill", releaseInfo.hasUpdate && "available")}>{releaseStatusLabel}</span>
                  </div>
                  <div className="about-kv">
                    <div><span>{lang === "zh" ? "状态" : "Status"}</span><strong>{releaseStatusLabel}</strong></div>
                    <div><span>{lang === "zh" ? "最新版本" : "Latest"}</span><strong>{releaseInfo.latestVersion || "-"}</strong></div>
                  </div>
                  <div className="about-actions">
                    <button className="primary-btn" onClick={() => void checkForUpdates()} disabled={releaseInfo.status === "checking"}><RefreshCw size={16} className={cx(releaseInfo.status === "checking" && "spin")} /> {lang === "zh" ? "检查更新" : "Check updates"}</button>
                    <button className="secondary-btn" onClick={() => openExternalUrl(releaseInfo.htmlUrl)} disabled={!releaseInfo.htmlUrl}><Download size={16} /> {lang === "zh" ? "打开下载页" : "Open releases"}</button>
                  </div>
                </section>
              </section>
            )}

            {tab === "settings" && (
              <section className="panel glass settings-panel">
                <div className="panel-head">
                  <div><p className="eyebrow">Settings</p><h3>{t.settings.title}</h3></div>
                </div>
                <div className="settings-list">
                  <div className="settings-row">
                    <div className="settings-icon"><Globe2 size={20} /></div>
                    <div className="settings-copy"><strong>{t.settings.language}</strong><p>{t.settings.languageDesc}</p></div>
                    <div className="segmented">
                      <button className={cx(lang === "zh" && "active")} onClick={() => setLang("zh")}>{t.settings.zh}</button>
                      <button className={cx(lang === "en" && "active")} onClick={() => setLang("en")}>{t.settings.en}</button>
                    </div>
                  </div>
                  <div className="settings-row">
                    <div className="settings-icon"><Sparkles size={20} /></div>
                    <div className="settings-copy"><strong>{t.settings.productName}</strong><p>{t.settings.productDesc}</p></div>
                    <StatusPill active label="Codex-X" />
                  </div>
                  <div className="settings-row">
                    <div className="settings-icon"><CheckCircle2 size={20} /></div>
                    <div className="settings-copy">
                      <strong>{lang === "zh" ? "首次启动向导" : "First-run wizard"}</strong>
                      <p>{lang === "zh" ? "重新检测 CODEX_HOME、config.toml、auth.json 和 SQLite 会话库。" : "Recheck CODEX_HOME, config.toml, auth.json and SQLite session stores."}</p>
                    </div>
                    <button className="secondary-btn" onClick={() => { localStorage.removeItem(STARTUP_WIZARD_SEEN_KEY); setStartupWizardOpen(true); refresh(); }}>
                      {lang === "zh" ? "重新检测" : "Recheck"}
                    </button>
                  </div>
                </div>
              </section>
            )}
          </>
        )}
      </section>
    </main>
  );
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode><App /></React.StrictMode>,
);
