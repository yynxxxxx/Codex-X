import React from "react";
import ReactDOM from "react-dom/client";
import {
  CheckCircle2,
  ChevronRight,
  Code2,
  Download,
  ExternalLink,
  FileCode2,
  Globe2,
  History,
  Info,
  KeyRound,
  Layers3,
  Loader2,
  Plus,
  Power,
  RefreshCw,
  RotateCcw,
  Settings,
  Sparkles,
  TerminalSquare,
  Trash2,
  Zap,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import "./styles.css";

type Lang = "zh" | "en";
type ProviderMode = "list" | "form" | "official";
type InstructionMode = "list" | "form";
type Tab = "dashboard" | "provider" | "instruction" | "toml" | "settings" | "about";

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
  wireApi: string;
  requiresOpenaiAuth: boolean;
};

type SavedPrompt = {
  id: string;
  title: string;
  filename: string;
  content: string;
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

const INSTRUCTION_RELATIVE_UI = "./gpt5.5-unrestricted.md";
const LANG_KEY = "codexx.lang";
const FALLBACK_GITHUB_REPO = "yynxxxxx/Codex-X";

const instructionTemplates: InstructionTemplate[] = [
  {
    id: "gpt5.5-unrestricted",
    filename: "gpt5.5-unrestricted.md",
    title: "gpt-5.5 unrestricted",
    subtitle: "Codex-X 默认模板，适合 GPT-5.5 / Codex 5.5。",
    badge: "推荐",
  },
  {
    id: "gpt5.4-unrestricted",
    filename: "gpt5.4-unrestricted.md",
    title: "gpt-5.4 unrestricted",
    subtitle: "兼容旧版 GPT-5.4 / Codex 配置。",
    badge: "兼容",
  },
];

const defaultProviderForm: SavedProvider = {
  id: "magicai",
  providerName: "MagicAI",
  baseUrl: "https://sky1818.com",
  model: "gpt-5.5",
  apiKey: "",
  wireApi: "responses",
  requiresOpenaiAuth: true,
};

const blankProviderForm: SavedProvider = {
  id: "",
  providerName: "",
  baseUrl: "",
  model: "gpt-5.5",
  apiKey: "",
  wireApi: "responses",
  requiresOpenaiAuth: true,
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
      officialAuthLoaded: "已载入 cc-switch 官方认证",
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
      officialAuthLoaded: "Loaded cc-switch official auth",
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
  const baseUrl = provider.baseUrl.trim().replace(/\/+$/, "") || "https://example.com/v1";
  const wireApi = provider.wireApi || "responses";
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
      skippingCustomProvider = currentSection === "model_providers.custom";
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
    'model_provider = "custom"',
    `model = "${tomlEscape(model)}"`,
  ];
  if (!hasReasoningEffort) {
    headerLines.push('model_reasoning_effort = "high"');
  }

  const providerLines = [
    "[model_providers.custom]",
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
  return JSON.stringify({ OPENAI_API_KEY: key || null }, null, 2);
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

function App() {
  const initialLang = (localStorage.getItem(LANG_KEY) as Lang | null) || "zh";
  const [lang, setLang] = React.useState<Lang>(initialLang === "en" ? "en" : "zh");
  const t = dict[lang];
  const [tab, setTab] = React.useState<Tab>("dashboard");
  const [providerMode, setProviderMode] = React.useState<ProviderMode>("list");
  const [instructionMode, setInstructionMode] = React.useState<InstructionMode>("list");
  const [editingProviderId, setEditingProviderId] = React.useState<string | null>(null);
  const [editingPromptId, setEditingPromptId] = React.useState<string | null>(null);
  const [savedProviders, setSavedProviders] = React.useState<SavedProvider[]>([]);
  const [savedPrompts, setSavedPrompts] = React.useState<SavedPrompt[]>([]);
  const [aboutInfo, setAboutInfo] = React.useState<AboutInfo | null>(null);
  const [releaseInfo, setReleaseInfo] = React.useState<ReleaseInfo>({ status: "idle" });
  const [state, setState] = React.useState<CodexState | null>(null);
  const [backups, setBackups] = React.useState<BackupEntry[]>([]);
  const [configDir, setConfigDir] = React.useState("");
  const [loading, setLoading] = React.useState(false);
  const [toast, setToast] = React.useState<string>("");
  const [error, setError] = React.useState<string>("");
  const [providerForm, setProviderForm] = React.useState<SavedProvider>(defaultProviderForm);
  const [providerTomlDraft, setProviderTomlDraft] = React.useState("");
  const [providerTomlDirty, setProviderTomlDirty] = React.useState(false);
  const [promptForm, setPromptForm] = React.useState<SavedPrompt>(blankPromptForm);
  const [officialForm, setOfficialForm] = React.useState({ model: "gpt-5.5", authJson: "" });
  const providerTomlPreview = React.useMemo(() => buildProviderTomlPreview(providerForm, state), [providerForm, state]);
  const providerAuthPreview = React.useMemo(() => buildProviderAuthPreview(providerForm), [providerForm]);
  const currentInstructionId = instructionIdFromPath(state?.instructionFile);

  React.useEffect(() => {
    localStorage.setItem(LANG_KEY, lang);
  }, [lang]);

  React.useEffect(() => {
    if (providerMode === "form" && !providerTomlDirty) {
      setProviderTomlDraft(providerTomlPreview);
    }
  }, [providerMode, providerTomlDirty, providerTomlPreview]);


  const currentProvider = state?.providers.find((p) => p.isCurrent);
  const detectedRows = React.useMemo(() => {
    return (state?.providers || []).map((p) => ({
      id: `detected-${p.id}`,
      source: "detected" as const,
      providerName: p.name || p.id,
      baseUrl: p.baseUrl || "",
      model: state?.model || "gpt-5.5",
      wireApi: p.wireApi || "responses",
      requiresOpenaiAuth: p.requiresOpenaiAuth ?? true,
      isCurrent: p.isCurrent,
    }));
  }, [state]);

  const localRows = React.useMemo(() => {
    return savedProviders.map((p) => ({
      ...p,
      source: "local" as const,
      isCurrent:
        Boolean(currentProvider) &&
        currentProvider?.baseUrl === p.baseUrl &&
        (state?.model || "") === p.model,
    }));
  }, [savedProviders, currentProvider, state?.model]);

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
        const next = await invoke<CodexState>("get_codex_state", { configDir: configDir || null });
        const backupList = await invoke<BackupEntry[]>("list_backups");
        const providerList = await invoke<SavedProvider[]>("list_saved_providers");
        const promptList = await invoke<SavedPrompt[]>("list_saved_prompts");
        const about = await invoke<AboutInfo>("get_about_info", { configDir: configDir || null });
        return { next, backupList, providerList, promptList, about };
      },
      ({ next, backupList, providerList, promptList, about }) => {
        setState(next);
        setBackups(backupList);
        setSavedProviders(providerList);
        setSavedPrompts(promptList);
        setAboutInfo(about);
        if (!configDir) setConfigDir(next.codexDir);
      },
    );
  }, [call, configDir]);

  React.useEffect(() => {
    refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const handleActionResult = (result: ActionResult) => {
    setState(result.state);
    setToast(result.message);
    invoke<BackupEntry[]>("list_backups").then(setBackups).catch(() => undefined);
    invoke<SavedPrompt[]>("list_saved_prompts").then(setSavedPrompts).catch(() => undefined);
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

  const normalizedPromptForm = (): SavedPrompt => ({
    ...promptForm,
    id: editingPromptId || promptForm.id || providerId(promptForm.title || promptForm.filename),
    title: promptForm.title.trim(),
    filename: promptForm.filename.trim(),
    content: promptForm.content,
  });

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
        const promptList = await invoke<SavedPrompt[]>("list_saved_prompts");
        return { result, promptList };
      },
      ({ result, promptList }) => {
        setSavedPrompts(promptList);
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

  const normalizedProviderForm = (): SavedProvider => ({
    ...providerForm,
    id: editingProviderId || providerForm.id || providerId(providerForm.providerName || providerForm.baseUrl),
    providerName: providerForm.providerName.trim(),
    baseUrl: providerForm.baseUrl.trim().replace(/\/+$/, ""),
    model: providerForm.model.trim(),
    apiKey: (providerForm.apiKey || "").trim(),
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
        setToast(lang === "zh" ? "供应商已保存到 SQLite" : "Provider saved to SQLite");
      },
    );

  const switchProvider = (provider: SavedProvider) =>
    call(
      () =>
        invoke<ActionResult>("switch_provider", {
          input: {
            configDir: configDir || null,
            providerName: provider.providerName,
            baseUrl: provider.baseUrl,
            model: provider.model,
            apiKey: provider.apiKey || "",
            wireApi: provider.wireApi,
            requiresOpenaiAuth: provider.requiresOpenaiAuth,
          },
        }),
      handleActionResult,
    );

  const saveAndSwitch = () =>
    call(
      async () => {
        const saved = await invoke<SavedProvider>("save_provider", { provider: normalizedProviderForm() });
        const result = await invoke<ActionResult>("save_provider_toml_config", {
          input: {
            configDir: configDir || null,
            configText: providerTomlDraft || buildProviderTomlPreview(saved, state),
            apiKey: saved.apiKey || "",
          },
        });
        const providerList = await invoke<SavedProvider[]>("list_saved_providers");
        return { result, providerList };
      },
      ({ result, providerList }) => {
        setSavedProviders(providerList);
        setProviderMode("list");
        setEditingProviderId(null);
        setProviderTomlDirty(false);
        handleActionResult(result);
      },
    );

  const switchOfficialProvider = () =>
    call(
      () => invoke<ActionResult>("switch_official_provider", { configDir: configDir || null }),
      handleActionResult,
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

  const checkForUpdates = async () => {
    const repo = aboutInfo?.githubRepo || FALLBACK_GITHUB_REPO;
    const appVersion = aboutInfo?.appVersion || "0.0.0";
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
      setReleaseInfo({
        status: "ok",
        latestVersion,
        htmlUrl: asset?.browser_download_url || release.html_url,
        assetName: asset?.name,
        body: release.body || "",
        hasUpdate: compareVersions(latestVersion, appVersion) > 0,
        message: compareVersions(latestVersion, appVersion) > 0
          ? (lang === "zh" ? "发现新版本" : "Update available")
          : (lang === "zh" ? "当前已是最新版本" : "You are up to date"),
      });
    } catch (e) {
      setReleaseInfo({
        status: "error",
        message: lang === "zh" ? `检查失败：${String(e)}` : `Check failed: ${String(e)}`,
      });
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

  const openOfficialEdit = () => {
    setOfficialForm({
      model: state?.model || "gpt-5.5",
      authJson: state?.authText || '{\n  "OPENAI_API_KEY": null,\n  "auth_mode": "chatgpt",\n  "tokens": {\n    "access_token": "",\n    "refresh_token": "",\n    "id_token": ""\n  }\n}',
    });
    setProviderMode("official");
    void loadCcSwitchOfficialAuth(false);
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
    setProviderTomlDraft(buildProviderTomlPreview(provider, state));
    setProviderTomlDirty(false);
    setProviderMode("form");
  };

  const openEditDetectedProvider = (provider: { id: string; providerName: string; baseUrl: string; model: string; wireApi: string; requiresOpenaiAuth: boolean }) => {
    setEditingProviderId(providerId(provider.providerName || provider.baseUrl));
    const next = {
      id: providerId(provider.providerName || provider.baseUrl),
      providerName: provider.providerName,
      baseUrl: provider.baseUrl,
      model: provider.model,
      apiKey: extractOpenAiApiKey(state?.authText),
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

  const navItems: Array<[Tab, string, React.ReactNode]> = [
    ["dashboard", t.nav.dashboard, <Layers3 size={18} />],
    ["provider", t.nav.provider, <Zap size={18} />],
    ["instruction", t.nav.instruction, <Sparkles size={18} />],
    ["toml", t.nav.toml, <FileCode2 size={18} />],
    ["settings", t.nav.settings, <Settings size={18} />],
    ["about", t.nav.about, <Info size={18} />],
  ];

  return (
    <main className="app-shell">
      <div className="orb orb-a" />
      <div className="orb orb-b" />

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
            <button key={id} className={cx("nav-item", tab === id && "active")} onClick={() => setTab(id)}>
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

        {toast && (
          <div className="toast ok" onAnimationEnd={() => setToast("")}>
            <CheckCircle2 size={18} /> {toast}
          </div>
        )}
        {error && <div className="toast error">{error}</div>}

        {!state ? (
          <div className="panel glass center-panel">
            <Loader2 className="spin" />
            <p>{t.loadingConfig}</p>
          </div>
        ) : (
          <>
            {tab === "dashboard" && (
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
                          id: providerId(p.providerName),
                          providerName: p.providerName,
                          baseUrl: p.baseUrl,
                          model: p.model,
                          apiKey: "",
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
                            <div className="provider-meta">
                              <span>{p.source === "official" ? t.provider.official : p.source === "detected" ? t.provider.detected : t.provider.local}</span>
                              <code>{p.model}</code>
                            </div>
                            <div className="provider-badges">
                              {p.isCurrent && <StatusPill active label={t.provider.current} />}
                              {p.source === "official" && <StatusPill active={false} label={t.provider.noRouting} />}
                              {p.source === "official" && <StatusPill active={state.officialAuthAvailable} label={state.officialAuthAvailable ? t.provider.authReady : t.provider.authMissing} />}
                            </div>
                            <div className="provider-actions">
                              <button className="secondary-btn small" onClick={() => switchable ? switchProvider(switchable) : switchOfficialProvider()} disabled={loading || p.isCurrent}>{t.provider.switch}</button>
                              {p.source === "official" && <button className="ghost-btn small" onClick={openOfficialEdit}>{t.provider.viewEdit}</button>}
                              {local && <button className="ghost-btn small" onClick={() => openEditProvider(local)}>{t.provider.edit}</button>}
                              {!local && p.source === "detected" && <button className="ghost-btn small" onClick={() => openEditDetectedProvider(p)}>{t.provider.edit}</button>}
                              {local && <button className="danger-btn small" onClick={() => removeProvider(local.id)}><Trash2 size={14} /> {t.provider.remove}</button>}
                            </div>
                          </div>
                        );
                      })}
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
                          <Field label={t.provider.apiKey}><input type="password" value={providerForm.apiKey || ""} onChange={(e) => setProviderForm({ ...providerForm, apiKey: e.target.value })} placeholder={t.provider.apiKeyPlaceholder} /></Field>
                          <Field label={lang === "zh" ? "API 请求地址" : t.provider.baseUrl}><input value={providerForm.baseUrl} onChange={(e) => setProviderForm({ ...providerForm, baseUrl: e.target.value })} /></Field>
                          <Field label={t.provider.name}><input value={providerForm.providerName} onChange={(e) => setProviderForm({ ...providerForm, providerName: e.target.value, id: editingProviderId || providerId(e.target.value) })} /></Field>
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
                            <p>{lang === "zh" ? "可直接编辑，保存时会写入 Codex live config.toml。" : "Editable. Saved directly to the Codex live config.toml."}</p>
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
                        <button className="primary-btn big" onClick={saveAndSwitch} disabled={loading}><Zap size={18} /> {t.provider.saveAndSwitch}</button>
                      </div>
                    </div>
                  </div>
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
                        <button className="primary-btn add-provider-btn" onClick={openAddPrompt}><Plus size={18} /> {lang === "zh" ? "添加提示词" : "Add prompt"}</button>
                      </div>
                    </div>

                    <div className="instruction-row-list">
                      {instructionTemplates.map((item) => {
                        const isCurrent = currentInstructionId === item.id;
                        return (
                          <div className={cx("instruction-row", isCurrent && "selected")} key={item.id}>
                            <div className="instruction-icon"><Sparkles size={22} /></div>
                            <div className="instruction-main">
                              <strong>{item.title}</strong>
                              <p>{item.subtitle}</p>
                              <code>./{item.filename}</code>
                            </div>
                            <div className="instruction-status-col">
                              {isCurrent ? <StatusPill active label={t.provider.current} /> : <span />}
                            </div>
                            <div className="instruction-action-col">
                              <button className="secondary-btn small" onClick={() => switchInstructionTemplate(item.id)} disabled={loading || isCurrent}>{t.instruction.enable}</button>
                              <button className="ghost-btn small" onClick={disableInstruction} disabled={loading || !isCurrent}>{lang === "zh" ? "禁用" : "Disable"}</button>
                            </div>
                          </div>
                        );
                      })}

                      {savedPrompts.map((prompt) => {
                        const isCurrent = Boolean(state.instructionFile) && (state.instructionFile || "").replace(/\\/g, "/").endsWith(prompt.filename);
                        return (
                          <div className={cx("instruction-row", isCurrent && "selected")} key={prompt.id}>
                            <div className="instruction-icon custom"><FileCode2 size={22} /></div>
                            <div className="instruction-main">
                              <strong>{prompt.title}</strong>
                              <p>{lang === "zh" ? "自定义指令提示词" : "Custom instruction prompt"}</p>
                              <code>./{prompt.filename}</code>
                            </div>
                            <div className="instruction-status-col">
                              {isCurrent ? <StatusPill active label={t.provider.current} /> : <span />}
                            </div>
                            <div className="instruction-action-col">
                              <button className="secondary-btn small" onClick={() => enableSavedPrompt(prompt.id)} disabled={loading || isCurrent}>{t.instruction.enable}</button>
                              <button className="ghost-btn small" onClick={disableInstruction} disabled={loading || !isCurrent}>{lang === "zh" ? "禁用" : "Disable"}</button>
                              <button className="ghost-btn small" onClick={() => openEditPrompt(prompt)}>{t.provider.edit}</button>
                              <button className="danger-btn small" onClick={() => removeSavedPrompt(prompt.id)}><Trash2 size={14} /> {t.provider.remove}</button>
                            </div>
                          </div>
                        );
                      })}

                      {state.instructionFile && currentInstructionId === "custom" && !savedPrompts.some((p) => state.instructionFile?.replace(/\\/g, "/").endsWith(p.filename)) && (
                        <div className="instruction-row selected">
                          <div className="instruction-icon custom"><FileCode2 size={22} /></div>
                          <div className="instruction-main">
                            <strong>{lang === "zh" ? "当前自定义指令提示词" : "Current custom prompt"}</strong>
                            <p>{lang === "zh" ? "当前 model_instructions_file 不是 Codex-X 内置模板。" : "The current model_instructions_file is not a built-in Codex-X template."}</p>
                            <code>{state.instructionFile}</code>
                          </div>
                          <div className="instruction-status-col"><StatusPill active label={t.provider.current} /></div>
                          <div className="instruction-action-col"><button className="ghost-btn small" onClick={disableInstruction} disabled={loading}>{lang === "zh" ? "禁用" : "Disable"}</button></div>
                        </div>
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
                      <button className="secondary-btn big" onClick={savePromptOnly} disabled={loading}>{lang === "zh" ? "保存" : "Save"}</button>
                      <button className="primary-btn big" onClick={saveAndEnablePrompt} disabled={loading}><Zap size={18} /> {lang === "zh" ? "保存并启用" : "Save & enable"}</button>
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
                    <button className="secondary-btn" onClick={() => window.open(aboutInfo?.projectUrl || `https://github.com/${FALLBACK_GITHUB_REPO}`, "_blank")}><ExternalLink size={16} /> {lang === "zh" ? "打开项目主页" : "Open project"}</button>
                    <button className="ghost-btn" onClick={() => window.open(`${aboutInfo?.projectUrl || `https://github.com/${FALLBACK_GITHUB_REPO}`}/issues`, "_blank")}><ExternalLink size={16} /> {lang === "zh" ? "反馈问题" : "Issues"}</button>
                  </div>
                </section>

                <section className="panel glass about-card">
                  <div className="panel-head compact">
                    <div><p className="eyebrow">GitHub Releases</p><h3>{lang === "zh" ? "更新检查" : "Update check"}</h3></div>
                    <StatusPill active={releaseInfo.status === "ok" && Boolean(releaseInfo.hasUpdate)} label={releaseInfo.status === "checking" ? (lang === "zh" ? "检查中" : "Checking") : releaseInfo.message || (lang === "zh" ? "未检查" : "Idle")} />
                  </div>
                  <div className="about-kv">
                    <div><span>{lang === "zh" ? "状态" : "Status"}</span><strong>{releaseInfo.status}</strong></div>
                    <div><span>{lang === "zh" ? "最新版本" : "Latest"}</span><strong>{releaseInfo.latestVersion || "-"}</strong></div>
                    <div><span>{lang === "zh" ? "资源" : "Asset"}</span><code>{releaseInfo.assetName || "-"}</code></div>
                    <div><span>{lang === "zh" ? "仓库" : "Repo"}</span><code>{aboutInfo?.githubRepo || FALLBACK_GITHUB_REPO}</code></div>
                  </div>
                  {releaseInfo.body && <pre className="release-notes">{releaseInfo.body}</pre>}
                  <div className="about-actions">
                    <button className="primary-btn" onClick={checkForUpdates} disabled={releaseInfo.status === "checking"}><RefreshCw size={16} className={cx(releaseInfo.status === "checking" && "spin")} /> {lang === "zh" ? "检查更新" : "Check updates"}</button>
                    <button className="secondary-btn" onClick={() => releaseInfo.htmlUrl && window.open(releaseInfo.htmlUrl, "_blank")} disabled={!releaseInfo.htmlUrl}><Download size={16} /> {lang === "zh" ? "打开下载页" : "Open download"}</button>
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
