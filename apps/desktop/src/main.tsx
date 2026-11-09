import { releaseInfoFromFallback, releaseInfoFromCheckFailure } from "./updateCheckFlow";
import { providerUpstreamApi, providerRequiresConversion } from "./providerProtocol";
import React from "react";
import { flushSync } from "react-dom";
import ReactDOM from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Loader2 } from "lucide-react";
import {
  SessionManagementPage,
  type SessionPreview,
  type SessionSyncStatus,
} from "./pages/SessionManagementPage";
import { OverviewPage } from "./pages/OverviewPage";
import { AboutPage, SettingsPage, TomlConfigPage } from "./pages/UtilityPages";
import { PromptsPage } from "./pages/PromptsPage";
import { SkillsMcpPage, type SkillsMcpNoteKind } from "./pages/SkillsMcpPage";
import { useProviderBalances } from "./providerBalance";
import { ProvidersPage, type ProviderCopy, type ProviderRow } from "./pages/ProvidersPage";
import { AppShell, type AppTab, type AppTheme } from "./components/AppShell";
import {
  AppToast,
  StartupWizardDialog,
  UpdateDialog,
} from "./components/AppDialogs";
import { PageTransition } from "./components/PageTransition";
import { cx } from "./components/ui";
import { appUpdater, isAppUpdateBusy, useAppUpdater } from "./appUpdater";
import { providerProfilesMatch, type ProviderProfile } from "./providerProfiles";
import { applyProviderRowOrder, orderProviderRows } from "./providerRowOrder";
import { createPresetProvider, getProviderPreset, getProviderPresetVariant } from "./providerPresets";
import { validateProviderModelMappings } from "./components/ProviderModelMappings";
import { createOfficialProfileMonitor } from "./officialProfileMonitor";
import { BUNDLED_OFFICIAL_MODELS, INITIAL_OFFICIAL_MODEL, normalizeOfficialModels, type OfficialModelCatalog } from "./officialModels";
import { createSessionStatusTracker, providerActivationIsBusy, readCurrentSessionStatus, sessionStatusContextKey } from "./sessionOperationState";
import type { LoadingScope, SessionStatusContext } from "./sessionOperationState";
import { useConfigHealth } from "./useConfigHealth";
import { ConfigHealthPanel, ConfigHealthStatus } from "./components/ConfigHealthPanel";
import { ConfigHealthToast } from "./components/ConfigHealthToast";
import type {
  AboutInfo,
  ActionResult,
  AppUpdateInfo,
  BuiltinPromptDetail,
  BuiltinPromptStatus,
  CodexDesktopRestartResult,
  CodexState,
  ImportResult,
  InstructionMode,
  InstructionTemplate,
  Lang,
  OfficialAuthCandidate,
  OfficialProfileActionResult,
  OfficialProfileDetail,
  OfficialProfileSummary,
  DuplicateProviderResult,
  PromptInjectionMode,
  ProviderConnectionResult,
  ProviderModel,
  ProviderModelsResult,
  ProviderMode,
  ReleaseInfo,
  SavedPrompt,
  SavedProvider,
  SessionDeleteResult,
  SessionSyncResult,
  SkillsMcpActionResult,
  SkillsMcpImportPreview,
  SkillsMcpState,
  StartupDiagnostics,
} from "./types";
import "./styles/base.css";
import "./styles/app-shell.css";
import "./styles/ui-primitives.css";
import "./styles/app-dialogs.css";
import "./styles/dark-theme.css";
import "./styles/form-controls.css";

type Tab = AppTab;

const LANG_KEY = "codexx.lang";
const THEME_KEY = "codexx.theme";
const STARTUP_WIZARD_SEEN_KEY = "codexx.startupWizardSeen";
const ACTIVE_PROVIDER_KEY = "codexx.activeProviderId";
const PROMPT_INJECTION_MODE_KEY = "codexx.promptInjectionMode";
const FALLBACK_GITHUB_REPO = "yynxxxxx/Codex-X";
const DEFAULT_OFFICIAL_PROFILE_ID = "openai-official";

type ThemeTransitionDocument = Document & {
  startViewTransition?: (update: () => void | Promise<void>) => { finished: Promise<void> };
};

const bundledInstructionTemplates: InstructionTemplate[] = [
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
  {
    id: "github-gpt-5-6-sol-unrestricted-33b86c71",
    filename: "gpt-5.6-sol-unrestricted.md",
    title: "gpt-5.6-sol-unrestricted.md",
    subtitle: "gpt5.6-sol 破甲提示词",
    badge: "内置",
  },
  {
    id: "github-3-0-b459e1e8",
    filename: "海鸥3.0破甲.md",
    title: "海鸥3.0破甲.md",
    subtitle: "测试生效：海鸥在线，你要整点薯条吗？",
    badge: "内置",
  },
];

const defaultProviderForm: SavedProvider = {
  id: "magicai",
  providerName: "MagicAI",
  baseUrl: "https://sky1818.com",
  model: "gpt-5.5",
  apiKey: "",
  tomlConfig: "",
  wireApi: "responses",
  upstreamApi: "responses",
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
  upstreamApi: "responses",
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
    },
    provider: {
      title: "供应商列表",
      subtitle: "管理多个官方 Codex 登录与第三方 API，按名称区分账号并随时切换。",
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
      officialHint: "官方配置与当前中转配置独立保存。读取配置不会切换当前供应商；切换回官方时会优先使用已保存配置。",
      officialUrl: "官方入口",
      formAdd: "添加新供应商",
      formEdit: "编辑供应商",
      formHint: "选择供应商类型并填写配置。新增配置保存到列表，点击启用后生效。",
      name: "供应商名称",
      baseUrl: "Base URL",
      model: "模型",
      wireApi: "Wire API",
      apiKey: "API Key",
      apiKeyPlaceholder: "填写 API Key",
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
    },
    provider: {
      title: "Provider list",
      subtitle: "Manage named Codex sign-in profiles and third-party APIs, and switch between them.",
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
      officialHint: "Official settings are stored separately from the active proxy. Loading a config does not switch providers; switching back uses the saved config first.",
      officialUrl: "Official URL",
      formAdd: "Add provider",
      formEdit: "Edit provider",
      formHint: "Choose a provider type and enter its settings. New profiles are saved to the list; enable one to use it.",
      name: "Provider name",
      baseUrl: "Base URL",
      model: "Model",
      wireApi: "Wire API",
      apiKey: "API Key",
      apiKeyPlaceholder: "Enter API key",
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

function getProviderPageCopy(lang: Lang): ProviderCopy {
  const t = dict[lang];
  const isChinese = lang === "zh";
  return {
    eyebrow: "Provider",
    title: t.provider.title,
    subtitle: t.provider.subtitle,
    importLabel: t.provider.importCc,
    addLabel: t.provider.add,
    noProviders: t.provider.noProviders,
    currentLabel: isChinese ? "当前使用" : "Current",
    enableLabel: isChinese ? "启用" : "Enable",
    testLabel: isChinese ? "测试连接" : "Test connection",
    editLabel: t.provider.edit,
    duplicateLabel: isChinese ? "复制供应商" : "Duplicate provider",
    removeLabel: t.provider.remove,
    deleteTitle: isChinese ? "删除供应商" : "Delete provider",
    deleteDescription: (providerName) => isChinese
      ? `“${providerName}”将从供应商列表中删除，此操作无法撤销。`
      : `“${providerName}” will be removed from the provider list. This cannot be undone.`,
    deleteCurrentDescription: (providerName) => isChinese
      ? `“${providerName}”当前正在使用。删除前会先切换到其他官方账号，确定继续吗？`
      : `“${providerName}” is currently active. Codex-X will switch to another official account before deleting it. Continue?`,
    deleteCancelLabel: isChinese ? "取消" : "Cancel",
    deleteConfirmLabel: isChinese ? "确认删除" : "Delete",
    noBaseUrlLabel: "no base_url",
    officialEyebrow: "OpenAI Official",
    officialTitle: t.provider.officialEdit,
    officialHint: t.provider.officialHint,
    officialUrlLabel: t.provider.officialUrl,
    authPathLabel: "auth.json",
    officialCurrentLabel: t.provider.current,
    officialAuthLabel: "auth.json (JSON)",
    officialTomlLabel: "config.toml (TOML)",
    officialSaveLabel: isChinese ? "保存官方配置" : "Save official config",
    loadCcSwitchOfficialLabel: isChinese ? "从 CC Switch 载入" : "Load from CC Switch",
    resetOfficialLabel: isChinese ? "重置默认官方配置" : "Reset default official config",
    resetOfficialTitle: isChinese ? "重置默认官方配置" : "Reset default official config",
    resetOfficialDescription: isChinese
      ? "这会切换到 OpenAI Official、清除当前 live auth.json，并要求你在 Codex 中重新登录。操作前会自动备份当前配置。"
      : "This switches to OpenAI Official, removes the live auth.json, and requires a new Codex login. The current files are backed up first.",
    resetOfficialCancelLabel: isChinese ? "取消" : "Cancel",
    resetOfficialConfirmLabel: isChinese ? "确认重置" : "Reset",
    cancelLabel: t.provider.cancel,
    formEyebrow: "Provider",
    formAddTitle: t.provider.formAdd,
    formEditTitle: t.provider.formEdit,
    formHint: t.provider.formHint,
    apiConfigTitle: isChinese ? "供应商 API 配置" : "Provider API configuration",
    apiConfigDescription: isChinese
      ? "在同一个页面管理 API、认证信息和 config.toml。"
      : "Manage API, authentication, and config.toml in one place.",
    apiKeyLabel: t.provider.apiKey,
    apiKeyPlaceholder: t.provider.apiKeyPlaceholder,
    showApiKeyLabel: isChinese ? "显示 API Key" : "Show API key",
    hideApiKeyLabel: isChinese ? "隐藏 API Key" : "Hide API key",
    baseUrlLabel: isChinese ? "API 请求地址" : t.provider.baseUrl,
    nameLabel: t.provider.name,
    modelLabel: t.provider.model,
    fetchModelsLabel: isChinese ? "获取模型列表" : "Fetch models",
    fetchingModelsLabel: isChinese ? "获取中" : "Fetching",
    chooseModelLabel: (count) => isChinese ? `选择模型（${count}）` : `Choose a model (${count})`,
    wireApiLabel: t.provider.wireApi,
    requiresAuthLabel: t.provider.requiresAuth,
    authPreviewTitle: "auth.json (JSON)",
    authPreviewDescription: isChinese
      ? "启用时会按供应商的认证方式应用 API Key；此处预览 auth.json 中保存的内容。"
      : "Enabling applies the API key using this provider's authentication settings. This previews the contents saved in auth.json.",
    tomlTitle: "config.toml (TOML)",
    tomlDescription: isChinese
      ? "上方标准字段是启用时的权威值；已有模板中的其他扩展字段会保留。只有点击“重置生成”才会替换为标准模板。"
      : "The standard fields above are authoritative when enabled. Other fields from an existing template are preserved; only Reset replaces it with the standard template.",
    resetTomlLabel: isChinese ? "重置生成" : "Reset",
    saveLabel: t.provider.saveAndSwitch,
    savingLabel: isChinese ? "保存中..." : "Saving...",
  };
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
  return ["openai", "custom", "amazon-bedrock", "ollama", "lmstudio", "oss"].includes(id.trim().toLowerCase());
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

function parseTomlStringValue(value: string) {
  const raw = value.trim();
  if (raw.startsWith('"')) {
    try {
      return JSON.parse(raw) as string;
    } catch {
      const end = raw.lastIndexOf('"');
      return end > 0 ? raw.slice(1, end) : raw.slice(1);
    }
  }
  if (raw.startsWith("'")) {
    const end = raw.lastIndexOf("'");
    return end > 0 ? raw.slice(1, end) : raw.slice(1);
  }
  return raw.replace(/\s+#.*$/, "").trim();
}

function extractTomlProviderApiKey(configText: string | undefined, providerId?: string) {
  if (!configText?.trim()) return "";
  const targetSection = providerId ? `model_providers.${providerId}` : "";
  let currentSection = "";
  let topLevelValue = "";
  let firstProviderValue = "";

  for (const line of configText.replace(/\r\n?/g, "\n").split("\n")) {
    const section = line.match(/^\s*\[([^\]]+)]\s*(?:#.*)?$/);
    if (section) {
      currentSection = section[1].trim();
      continue;
    }
    const token = line.match(/^\s*experimental_bearer_token\s*=\s*(.+?)\s*$/);
    if (!token) continue;
    const value = parseTomlStringValue(token[1]).trim();
    if (!value) continue;
    if (!currentSection) topLevelValue = value;
    if (currentSection.startsWith("model_providers.") && !firstProviderValue) firstProviderValue = value;
    if (targetSection && currentSection === targetSection) return value;
  }

  return topLevelValue || (!providerId ? firstProviderValue : "");
}

function extractTomlModelProvider(configText: string | undefined) {
  if (!configText?.trim()) return "";
  for (const line of configText.replace(/\r\n?/g, "\n").split("\n")) {
    if (/^\s*\[/.test(line)) break;
    const entry = line.match(/^\s*model_provider\s*=\s*(.+?)\s*$/);
    if (entry) return parseTomlStringValue(entry[1]).trim();
  }
  return "";
}

function savedProviderApiKey(provider: SavedProvider) {
  const providerId = extractTomlModelProvider(provider.tomlConfig);
  return (provider.apiKey || "").trim()
    || extractTomlProviderApiKey(provider.tomlConfig, providerId || undefined);
}

function savedProviderMatchesProfile(provider: SavedProvider, profile: ProviderProfile) {
  return providerProfilesMatch({
    baseUrl: provider.baseUrl,
    providerName: provider.providerName,
    model: provider.model,
    apiKey: savedProviderApiKey(provider),
  }, profile);
}

function buildProviderTomlPreview(provider: SavedProvider) {
  const model = provider.model.trim();
  const name = provider.providerName.trim();
  const providerKey = "custom";
  const baseUrl = provider.baseUrl.trim().replace(/\/+$/, "");
  if (!model || !name || !baseUrl) return "";
  const wireApi = "responses";
  return [
    `model_provider = "${tomlEscape(providerKey)}"`,
    `model = "${tomlEscape(model)}"`,
    "",
    `[model_providers.${providerKey}]`,
    `name = "${tomlEscape(name)}"`,
    `base_url = "${tomlEscape(baseUrl)}"`,
    `wire_api = "${tomlEscape(wireApi)}"`,
    `requires_openai_auth = ${provider.requiresOpenaiAuth ? "true" : "false"}`,
    `supports_websockets = false`,
  ].join("\n");
}

function buildOfficialTomlPreview(model: string) {
  return [
    `model_provider = "custom"`,
    ...(model.trim() ? [`model = "${tomlEscape(model.trim())}"`] : []),
    "",
    "[model_providers.custom]",
    `name = "OpenAI"`,
    `wire_api = "responses"`,
    `requires_openai_auth = true`,
    `supports_websockets = true`,
  ].join("\n");
}

function buildProviderAuthPreview(provider: SavedProvider) {
  const key = provider.apiKey?.trim();
  return JSON.stringify(key ? { OPENAI_API_KEY: key } : {}, null, 2);
}

function sessionMismatchCount(status: SessionSyncStatus | null) {
  if (!status?.needsSync) return 0;
  const exactCount = Number.isFinite(status.mismatchedSessions)
    ? status.mismatchedSessions
    : Math.max(status.mismatchedThreads, status.mismatchedRollouts);
  return Math.max(1, exactCount);
}

function instructionIdFromPath(path: string | undefined, templates: InstructionTemplate[]) {
  if (!path) return "";
  const normalized = path.replace(/\\/g, "/");
  const found = templates.find((item) => normalized.toLowerCase().endsWith(item.filename.toLowerCase()));
  return found?.id || "custom";
}

function uniqueBuiltinPromptStatuses(statuses: BuiltinPromptStatus[]) {
  const sourcePriority: Record<string, number> = {
    unavailable: 0,
    bundled: 1,
    cache: 2,
    removed: 2,
    github: 3,
  };
  const seenIds = new Set<string>();
  const seenFilenames = new Set<string>();
  const selected = statuses
    .map((item, index) => ({ item, index }))
    .filter(({ item }) => item.id.trim() && item.filename.trim())
    .sort((a, b) =>
      (sourcePriority[b.item.contentSource] ?? -1) - (sourcePriority[a.item.contentSource] ?? -1)
      || a.index - b.index,
    )
    .filter(({ item }) => {
      const id = item.id.trim().toLowerCase();
      const filename = item.filename.trim().toLowerCase();
      if (seenIds.has(id) || seenFilenames.has(filename)) return false;
      seenIds.add(id);
      seenFilenames.add(filename);
      return true;
    });
  return selected.sort((a, b) => a.index - b.index).map(({ item }) => item);
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

function fitCodeEditorHeight(editor: HTMLTextAreaElement | null, minHeight: number) {
  if (!editor) return;
  editor.style.height = "auto";
  editor.style.height = `${Math.max(minHeight, editor.scrollHeight + 2)}px`;
}

function normalizedConfigDirForComparison(value: string) {
  let normalized = value.trim().replace(/\\/g, "/").replace(/\/+$/, "");
  if (/^(?:[a-z]:\/|\/\/)/i.test(normalized)) normalized = normalized.toLowerCase();
  return normalized;
}

function CodexStateLoading({ lang, loading }: { lang: Lang; loading: boolean }) {
  return (
    <section className="cx-state-loading" role="status" aria-live="polite">
      {loading && <Loader2 className="spin" size={22} aria-hidden="true" />}
      <span>{loading
        ? (lang === "zh" ? "正在读取 Codex 配置..." : "Loading Codex configuration...")
        : (lang === "zh" ? "Codex 配置读取失败，请返回概览重试" : "Could not load the Codex configuration. Retry from Overview.")}</span>
    </section>
  );
}

function App() {
  const initialLang = (localStorage.getItem(LANG_KEY) as Lang | null) || "zh";
  const [lang, setLang] = React.useState<Lang>(initialLang === "en" ? "en" : "zh");
  const [theme, setTheme] = React.useState<AppTheme>(() =>
    localStorage.getItem(THEME_KEY) === "dark" ? "dark" : "light",
  );
  const t = dict[lang];
  const updater = useAppUpdater();
  const isMacRuntime = navigator.userAgent.toLowerCase().includes("mac");
  const [tab, setTab] = React.useState<Tab>("dashboard");
  const [visitedTabs, setVisitedTabs] = React.useState<Set<Tab>>(() => new Set(["dashboard"]));
  const [providerMode, setProviderMode] = React.useState<ProviderMode>("list");
  const [instructionMode, setInstructionMode] = React.useState<InstructionMode>("list");
  const [promptInjectionMode, setPromptInjectionMode] = React.useState<PromptInjectionMode>(() =>
    localStorage.getItem(PROMPT_INJECTION_MODE_KEY) === "replace" ? "replace" : "append",
  );
  const [skillsMcpTab, setSkillsMcpTab] = React.useState<"mcp" | "skills">("mcp");
  const [editingProviderId, setEditingProviderId] = React.useState<string | null>(null);
  const [editingDetectedProvider, setEditingDetectedProvider] = React.useState(false);
  const [editingPromptId, setEditingPromptId] = React.useState<string | null>(null);
  const [editingBuiltinPrompt, setEditingBuiltinPrompt] = React.useState<BuiltinPromptDetail | null>(null);
  const [savedProviders, setSavedProviders] = React.useState<SavedProvider[]>([]);
  const [providerOrder, setProviderOrder] = React.useState<{ directory: string; order: string[] }>({ directory: "", order: [] });
  const [officialProfiles, setOfficialProfiles] = React.useState<OfficialProfileSummary[]>([]);
  const [editingOfficialProfileId, setEditingOfficialProfileId] = React.useState<string | null>(null);
  const [creatingProvider, setCreatingProvider] = React.useState(false);
  const [providerCreationBase, setProviderCreationBase] = React.useState("");
  const [selectedPresetId, setSelectedPresetId] = React.useState("custom");
  const [selectedPresetVariantId, setSelectedPresetVariantId] = React.useState("");
  const [officialAuthDirty, setOfficialAuthDirty] = React.useState(false);
  const [activeProviderId, setActiveProviderId] = React.useState(() => localStorage.getItem(ACTIVE_PROVIDER_KEY) || "");
  const [savedPrompts, setSavedPrompts] = React.useState<SavedPrompt[]>([]);
  const [builtinPromptStatus, setBuiltinPromptStatus] = React.useState<BuiltinPromptStatus[]>([]);
  const [aboutInfo, setAboutInfo] = React.useState<AboutInfo | null>(null);
  const [aboutLoading, setAboutLoading] = React.useState(false);
  const [releaseInfo, setReleaseInfo] = React.useState<ReleaseInfo>({ status: "idle" });
  const [updatePromptOpen, setUpdatePromptOpen] = React.useState(false);
  const [sessionStatus, setSessionStatus] = React.useState<SessionSyncStatus | null>(null);
  const [skillsMcpState, setSkillsMcpState] = React.useState<SkillsMcpState | null>(null);
  const [skillsMcpImportPreview, setSkillsMcpImportPreview] = React.useState<SkillsMcpImportPreview | null>(null);
  const [skillsMcpImportOpen, setSkillsMcpImportOpen] = React.useState(false);
  const [startupDiagnostics, setStartupDiagnostics] = React.useState<StartupDiagnostics | null>(null);
  const [startupDiagnosticsError, setStartupDiagnosticsError] = React.useState("");
  const [startupDiagnosticsLoading, setStartupDiagnosticsLoading] = React.useState(false);
  const [startupCheckMode, setStartupCheckMode] = React.useState<"startup" | "manual">("startup");
  const [startupWizardOpen, setStartupWizardOpen] = React.useState(() => localStorage.getItem(STARTUP_WIZARD_SEEN_KEY) !== "1");
  const [startupClosing, setStartupClosing] = React.useState(false);
  const [sessionQuery, setSessionQuery] = React.useState("");
  const deferredSessionQuery = React.useDeferredValue(sessionQuery);
  const [sessionGroupByCwd, setSessionGroupByCwd] = React.useState(false);
  const [showInternalSessions, setShowInternalSessions] = React.useState(false);
  const [selectedSessionIds, setSelectedSessionIds] = React.useState<string[]>([]);
  const [sessionDeleteConfirmOpen, setSessionDeleteConfirmOpen] = React.useState(false);
  const [sessionDeleteBusy, setSessionDeleteBusy] = React.useState(false);
  const [sessionDeleteSafetyConfirmed, setSessionDeleteSafetyConfirmed] = React.useState(false);
  const [state, setState] = React.useState<CodexState | null>(null);
  const [configDir, setConfigDir] = React.useState("");
  const [configDirDraft, setConfigDirDraft] = React.useState("");
  const [healthConfigDir, setHealthConfigDir] = React.useState("");
  const [settingsGeneralRequest, setSettingsGeneralRequest] = React.useState(0);
  const [loading, setLoading] = React.useState(false);
  const [providerActivationBusy, setProviderActivationBusy] = React.useState(false);
  const [refreshing, setRefreshing] = React.useState(true);
  const [toast, setToast] = React.useState<string>("");
  const [languageRestartNotice, setLanguageRestartNotice] = React.useState<{ configDir: string; message: string } | null>(null);
  const [error, setError] = React.useState<string>("");
  const [providerForm, setProviderForm] = React.useState<SavedProvider>(defaultProviderForm);
  const [providerTomlDraft, setProviderTomlDraft] = React.useState("");
  const [providerTomlDirty, setProviderTomlDirty] = React.useState(false);
  const providerApiKeyEditedRef = React.useRef(false);
  const [providerCommonConfigDirty, setProviderCommonConfigDirty] = React.useState(false);
  const [providerDraftRefreshToken, setProviderDraftRefreshToken] = React.useState(0);
  const [providerApiKeyVisible, setProviderApiKeyVisible] = React.useState(false);
  const [providerTestingId, setProviderTestingId] = React.useState("");
  const [availableProviderModels, setAvailableProviderModels] = React.useState<ProviderModel[]>([]);
  const [providerModelsLoading, setProviderModelsLoading] = React.useState(false);
  const [actionBusy, setActionBusy] = React.useState<string>("");
  const [skillsMcpNoteBusy, setSkillsMcpNoteBusy] = React.useState("");
  const [restartCodexError, setRestartCodexError] = React.useState("");
  const [restartCodexBusy, setRestartCodexBusy] = React.useState(false);
  React.useEffect(() => { setLanguageRestartNotice(null); setRestartCodexError(""); }, [configDir]);
  const [promptSyncing, setPromptSyncing] = React.useState(false);
  const [promptDetailLoading, setPromptDetailLoading] = React.useState(false);
  const [promptCatalogReady, setPromptCatalogReady] = React.useState(false);
  const [promptForm, setPromptForm] = React.useState<SavedPrompt>(blankPromptForm);
  const [officialForm, setOfficialForm] = React.useState({
    providerName: "OpenAI Official",
    model: INITIAL_OFFICIAL_MODEL,
    authJson: "",
    configText: buildOfficialTomlPreview(INITIAL_OFFICIAL_MODEL),
  });
  const officialFormRef = React.useRef(officialForm);
  officialFormRef.current = officialForm;
  const officialAuthDirtyRef = React.useRef(officialAuthDirty);
  officialAuthDirtyRef.current = officialAuthDirty;
  const [officialModels, setOfficialModels] = React.useState(BUNDLED_OFFICIAL_MODELS);
  const officialModelsRef = React.useRef(officialModels);
  officialModelsRef.current = officialModels;
  const [officialModelsLoading, setOfficialModelsLoading] = React.useState(false);
  const officialModelsRequestRef = React.useRef(0);
  const officialModelsScopeRef = React.useRef(configDir);
  officialModelsScopeRef.current = configDir;
  const officialModelsPendingRef = React.useRef<{ scope: string; promise: Promise<OfficialModelCatalog> } | null>(null);
  const [officialDraftSyncing, setOfficialDraftSyncing] = React.useState(false);
  const [officialDraftSyncError, setOfficialDraftSyncError] = React.useState("");
  const officialDraftSyncErrorRef = React.useRef("");
  const officialDraftRevisionRef = React.useRef(0);
  const officialDraftSyncRef = React.useRef<Promise<boolean>>(Promise.resolve(true));
  const officialDraftPendingEditRef = React.useRef<{ scope: string; edit: "model" | "toml" } | null>(null);
  const officialModelPristineRef = React.useRef(false);
  const officialContextRef = React.useRef("");
  officialContextRef.current = JSON.stringify([configDir, tab, providerMode]);
  const [promptModeHelpOpen, setPromptModeHelpOpen] = React.useState(false);
  const autoUpdateCheckedRef = React.useRef(false);
  const promptImportRef = React.useRef<HTMLInputElement | null>(null);
  const nativeTransferBusyRef = React.useRef(false);
  const [sessionExportBusy, setSessionExportBusy] = React.useState(false);
  const officialAuthEditorRef = React.useRef<HTMLTextAreaElement | null>(null);
  const officialTomlEditorRef = React.useRef<HTMLTextAreaElement | null>(null);
  const providerTomlEditorRef = React.useRef<HTMLTextAreaElement | null>(null);
  const providerModelsRequestRef = React.useRef(0);
  const providerDraftRequestRef = React.useRef(0);
  const providerCreationRequestRef = React.useRef(0);
  const officialDraftRequestRef = React.useRef(0);
  const officialProfilesRequestRef = React.useRef(0);
  const officialProfilesLiveRef = React.useRef(officialProfiles);
  officialProfilesLiveRef.current = officialProfiles;
  const officialMonitorReadyRef = React.useRef(false);
  officialMonitorReadyRef.current = Boolean(state) && !refreshing;
  const savedProvidersRequestRef = React.useRef(0);
  const providerOrderRequestRef = React.useRef(0);
  const loadingGenerationRef = React.useRef(0);
  const loadingTokensRef = React.useRef(new Map<number, LoadingScope>());
  const languageLoadingTokenRef = React.useRef<number | null>(null);
  const actionBusyGenerationRef = React.useRef(0);
  const actionBusyTokensRef = React.useRef(new Map<number, string>());
  const skillsMcpNoteBusyRef = React.useRef("");
  const restartCodexBusyRef = React.useRef(false);
  const promptModeHelpRef = React.useRef<HTMLDivElement | null>(null);
  const promptRefreshRequestRef = React.useRef(0);
  const promptDetailRequestRef = React.useRef(0);
  const refreshRequestRef = React.useRef(0);
  const aboutLoadKeyRef = React.useRef("");
  const sessionAutoLoadKeyRef = React.useRef("");
  const sessionStatusTrackerRef = React.useRef(createSessionStatusTracker());
  const sessionStateRef = React.useRef(state);
  sessionStateRef.current = state;
  const providerActivationRevisionRef = React.useRef(0);
  const providerActivationInFlightRef = React.useRef(0);
  const sessionMutationInFlightRef = React.useRef(false);
  const sessionDeleteBusyRef = React.useRef(false);
  const sessionRefreshPendingRef = React.useRef(false);
  const sessionSyncToastPendingRef = React.useRef(false);
  const refreshSyncedSessionStatusRef = React.useRef<() => Promise<SessionSyncStatus | null>>(async () => null);
  const promptRefreshInFlightRef = React.useRef<Promise<BuiltinPromptStatus[]> | null>(null);
  const promptAutoRefreshAttemptedRef = React.useRef(false);
  const promptCatalogReadyRef = React.useRef(false);
  const promptModeSyncedRef = React.useRef("");
  const skillsMcpLoadedRef = React.useRef("");
  const skillsMcpAutoLoadAttemptedRef = React.useRef("");
  const skillsMcpRequestRef = React.useRef(0);
  const activeConfigDirKeyRef = React.useRef("");
  const routingWakeRef = React.useRef<() => void>(() => {});
  const routingRequestRefreshRef = React.useRef<() => void>(() => {});
  const routingUiRef = React.useRef({ ready: false, editing: false });
  routingUiRef.current = { ready: Boolean(state) && !refreshing, editing: providerMode !== "list" };

  const loadOfficialModels = React.useCallback((forceRefresh = false): Promise<OfficialModelCatalog> => {
    const scope = configDir;
    if (officialModelsPendingRef.current?.scope === scope) return officialModelsPendingRef.current.promise;
    const request = ++officialModelsRequestRef.current;
    setOfficialModelsLoading(true);
    const promise = invoke<OfficialModelCatalog>("list_official_models", { forceRefresh })
      .then(normalizeOfficialModels)
      .catch(() => ({ ...BUNDLED_OFFICIAL_MODELS, warning: lang === "zh"
        ? "本机模型目录暂不可用，已使用内置目录。" : "The local catalog is unavailable. Using the bundled catalog." }))
      .then(catalog => {
        if (request === officialModelsRequestRef.current && officialModelsScopeRef.current === scope
          && officialModelsPendingRef.current?.scope === scope) {
          officialModelsRef.current = catalog;
          setOfficialModels(catalog);
        }
        return catalog;
      })
      .finally(() => {
        if (request === officialModelsRequestRef.current) {
          officialModelsPendingRef.current = null;
          setOfficialModelsLoading(false);
        }
      });
    officialModelsPendingRef.current = { scope, promise };
    return promise;
  }, [configDir, lang]);

  const invalidateOfficialDraftSync = React.useCallback((discard = true) => {
    officialDraftRevisionRef.current += 1;
    if (discard) {
      officialDraftPendingEditRef.current = null;
      officialDraftSyncErrorRef.current = "";
      setOfficialDraftSyncError("");
    }
    officialDraftSyncRef.current = Promise.resolve(!officialDraftPendingEditRef.current && !officialDraftSyncErrorRef.current);
    setOfficialDraftSyncing(false);
  }, []);

  const synchronizeOfficialDraft = (configText: string, model: string, edit: "model" | "toml") => {
    const revision = ++officialDraftRevisionRef.current;
    const request = officialDraftRequestRef.current;
    const context = JSON.stringify([configDir, "provider", "official"]);
    const isCurrent = () => revision === officialDraftRevisionRef.current
      && request === officialDraftRequestRef.current && context === officialContextRef.current;
    officialDraftPendingEditRef.current = { scope: configDir, edit };
    officialDraftSyncErrorRef.current = "";
    setOfficialDraftSyncError("");
    setOfficialDraftSyncing(true);
    const promise = (edit === "model"
      ? invoke<string>("update_official_model_draft", { configText, model }).then(text => ({ configText: text, model }))
      : invoke<string | null>("inspect_official_model_draft", { configText }).then(value => ({ configText, model: value ?? "" })))
      .then(patch => {
        if (!isCurrent()) return false;
        const next = { ...officialFormRef.current, ...patch };
        officialFormRef.current = next;
        setOfficialForm(next);
        officialDraftPendingEditRef.current = null;
        return true;
      })
      .catch(cause => {
        if (isCurrent()) {
          officialDraftSyncErrorRef.current = String(cause);
          setOfficialDraftSyncError(String(cause));
        }
        return false;
      })
      .finally(() => { if (isCurrent()) setOfficialDraftSyncing(false); });
    officialDraftSyncRef.current = promise;
    return promise;
  };

  const changeOfficialModel = (model: string) => {
    officialModelPristineRef.current = false;
    const next = { ...officialFormRef.current, model };
    officialFormRef.current = next;
    setOfficialForm(next);
    void synchronizeOfficialDraft(next.configText, model, "model");
  };

  const changeOfficialConfig = (configText: string) => {
    officialModelPristineRef.current = false;
    const next = { ...officialFormRef.current, configText };
    officialFormRef.current = next;
    setOfficialForm(next);
    void synchronizeOfficialDraft(configText, next.model, "toml");
  };

  const themeTransitionTimerRef = React.useRef<number | null>(null);
  const providerTomlPreview = React.useMemo(() => buildProviderTomlPreview(providerForm), [providerForm]);
  const providerAuthPreview = React.useMemo(() => buildProviderAuthPreview(providerForm), [providerForm]);
  const activeBuiltinTemplateId = state?.instructionTemplateKey?.startsWith("builtin:")
    ? state.instructionTemplateKey.slice("builtin:".length)
    : "";
  const instructionTemplates = React.useMemo<InstructionTemplate[]>(() => {
    if (!builtinPromptStatus.length) return bundledInstructionTemplates;
    return builtinPromptStatus
      .filter((item) => item.contentSource !== "removed" || item.id === activeBuiltinTemplateId)
      .map(({ id, filename, title, subtitle, badge }) => ({ id, filename, title, subtitle, badge }));
  }, [activeBuiltinTemplateId, builtinPromptStatus]);
  const missingActiveBuiltinTemplateId = activeBuiltinTemplateId
    && !instructionTemplates.some((item) => item.id === activeBuiltinTemplateId)
    ? activeBuiltinTemplateId
    : "";

  const syncProviderActivationBusy = React.useCallback(() => {
    setProviderActivationBusy(providerActivationIsBusy(loadingTokensRef.current.values(), actionBusyTokensRef.current.values()));
  }, []);

  const syncActionBusy = React.useCallback(() => {
    let latestToken = -1;
    let latestAction = "";
    for (const [token, action] of actionBusyTokensRef.current) {
      if (token > latestToken) {
        latestToken = token;
        latestAction = action;
      }
    }
    setActionBusy(latestAction);
    syncProviderActivationBusy();
  }, [syncProviderActivationBusy]);

  const beginLoading = React.useCallback((scope: LoadingScope = "general") => {
    const token = ++loadingGenerationRef.current;
    loadingTokensRef.current.set(token, scope);
    setLoading(true);
    syncProviderActivationBusy();
    return token;
  }, [syncProviderActivationBusy]);

  const endLoading = React.useCallback((token: number) => {
    if (!loadingTokensRef.current.delete(token)) return;
    setLoading(loadingTokensRef.current.size > 0);
    syncProviderActivationBusy();
  }, [syncProviderActivationBusy]);

  const beginActionBusy = React.useCallback((action: string) => {
    const token = ++actionBusyGenerationRef.current;
    actionBusyTokensRef.current.set(token, action);
    setActionBusy(action);
    syncProviderActivationBusy();
    return token;
  }, [syncProviderActivationBusy]);

  const handleLanguageBusyChange = React.useCallback((busy: boolean) => {
    if (busy && languageLoadingTokenRef.current === null) {
      languageLoadingTokenRef.current = beginLoading();
    } else if (!busy && languageLoadingTokenRef.current !== null) {
      endLoading(languageLoadingTokenRef.current);
      languageLoadingTokenRef.current = null;
    }
  }, [beginLoading, endLoading]);

  const endActionBusy = React.useCallback((token: number) => {
    if (!actionBusyTokensRef.current.delete(token)) return;
    syncActionBusy();
  }, [syncActionBusy]);

  const clearActionBusy = React.useCallback((action: string) => {
    let changed = false;
    for (const [token, activeAction] of actionBusyTokensRef.current) {
      if (activeAction !== action) continue;
      actionBusyTokensRef.current.delete(token);
      changed = true;
    }
    if (changed) syncActionBusy();
  }, [syncActionBusy]);

  const invalidateSessionStatus = React.useCallback(() => {
    sessionStatusTrackerRef.current.invalidate();
    sessionAutoLoadKeyRef.current = "";
    setSessionStatus(null);
  }, []);

  const getSessionStatusContext = React.useCallback((): SessionStatusContext | null => {
    const current = sessionStateRef.current;
    if (!current || providerActivationInFlightRef.current) return null;
    return {
      configDir: current.codexDir,
      targetProvider: current.modelProvider || "openai",
      providerRevision: providerActivationRevisionRef.current,
    };
  }, []);

  const loadCurrentSessionStatus = React.useCallback(() => readCurrentSessionStatus(
    sessionStatusTrackerRef.current,
    getSessionStatusContext,
    (context) => invoke<SessionSyncStatus>("get_session_sync_status", {
      configDir: context.configDir,
      targetProvider: context.targetProvider,
    }),
    (status) => {
      setSessionStatus(status);
      const context = getSessionStatusContext();
      if (context) sessionAutoLoadKeyRef.current = sessionStatusContextKey(context);
    },
  ), [getSessionStatusContext]);

  const invalidatePromptDetail = React.useCallback(() => {
    promptDetailRequestRef.current += 1;
    setPromptDetailLoading(false);
  }, []);
  const commitSavedProviders = React.useCallback((providers: SavedProvider[]) => {
    savedProvidersRequestRef.current += 1;
    setSavedProviders(providers);
  }, []);
  React.useEffect(() => {
    const directory = state?.codexDir || "";
    const request = ++providerOrderRequestRef.current;
    if (!directory) { setProviderOrder({ directory: "", order: [] }); return; }
    void invoke<string[]>("get_provider_order", { configDir: directory }).then((order) => {
      if (request === providerOrderRequestRef.current) setProviderOrder({ directory, order });
    }).catch((nextError) => {
      if (request !== providerOrderRequestRef.current) return;
      setProviderOrder({ directory, order: [] });
      setError(String(nextError));
    });
  }, [state?.codexDir]);
  const currentInstructionId = instructionIdFromPath(state?.instructionFile, instructionTemplates);
  const releaseStatusLabel = React.useMemo(() => {
    if (updater.state.phase === "downloading") return lang === "zh" ? "下载中" : "Downloading";
    if (updater.state.phase === "verifying") return lang === "zh" ? "验证中" : "Verifying";
    if (updater.state.phase === "preparing") return lang === "zh" ? "准备更新" : "Preparing update";
    if (updater.state.phase === "installing" || updater.state.phase === "handed-off") return lang === "zh" ? "安装中" : "Installing";
    if (updater.state.phase === "ready") return lang === "zh" ? "等待重启" : "Restart required";
    if (releaseInfo.status === "checking") return lang === "zh" ? "检查中" : "Checking";
    if (releaseInfo.status === "error") return lang === "zh" ? "失败" : "Failed";
    if (releaseInfo.hasUpdate) return lang === "zh" ? "有更新" : "Update found";
    if (releaseInfo.status === "ok") return lang === "zh" ? "已是最新" : "Up to date";
    return lang === "zh" ? "未检查" : "Idle";
  }, [lang, releaseInfo.hasUpdate, releaseInfo.status, updater.state.phase]);

  React.useEffect(() => {
    localStorage.setItem(LANG_KEY, lang);
  }, [lang]);

  React.useLayoutEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem(THEME_KEY, theme);
  }, [theme]);

  React.useEffect(() => () => {
    if (themeTransitionTimerRef.current !== null) {
      window.clearTimeout(themeTransitionTimerRef.current);
    }
    document.documentElement.classList.remove("cx-theme-view-transition", "cx-theme-fallback-transition");
  }, []);

  const toggleTheme = React.useCallback(() => {
    const nextTheme: AppTheme = theme === "dark" ? "light" : "dark";
    const root = document.documentElement;
    const commitTheme = () => {
      flushSync(() => setTheme(nextTheme));
    };

    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      commitTheme();
      return;
    }

    const transitionDocument = document as ThemeTransitionDocument;
    if (typeof transitionDocument.startViewTransition === "function") {
      root.classList.remove("cx-theme-fallback-transition");
      root.classList.add("cx-theme-view-transition");
      try {
        const transition = transitionDocument.startViewTransition(commitTheme);
        const clearTransitionClass = () => root.classList.remove("cx-theme-view-transition");
        void transition.finished.then(clearTransitionClass, clearTransitionClass);
        return;
      } catch {
        root.classList.remove("cx-theme-view-transition");
      }
    }

    root.classList.add("cx-theme-fallback-transition");
    commitTheme();
    if (themeTransitionTimerRef.current !== null) {
      window.clearTimeout(themeTransitionTimerRef.current);
    }
    themeTransitionTimerRef.current = window.setTimeout(() => {
      root.classList.remove("cx-theme-fallback-transition");
      themeTransitionTimerRef.current = null;
    }, 260);
  }, [theme]);

  React.useEffect(() => {
    localStorage.setItem(PROMPT_INJECTION_MODE_KEY, promptInjectionMode);
  }, [promptInjectionMode]);

  React.useEffect(() => {
    activeConfigDirKeyRef.current = normalizedConfigDirForComparison(configDir);
  }, [configDir]);

  React.useEffect(() => {
    if (error) setToast("");
  }, [error]);

  React.useEffect(() => {
    if (!promptModeHelpOpen) return undefined;
    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (target instanceof Node && promptModeHelpRef.current?.contains(target)) return;
      setPromptModeHelpOpen(false);
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setPromptModeHelpOpen(false);
    };
    document.addEventListener("pointerdown", handlePointerDown);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [promptModeHelpOpen]);

  React.useLayoutEffect(() => {
    if (providerMode !== "form") return;
    fitCodeEditorHeight(providerTomlEditorRef.current, 560);
  }, [providerMode, providerTomlDraft]);

  React.useEffect(() => {
    if (providerMode !== "form") {
      providerDraftRequestRef.current += 1;
      return;
    }
    const requestId = ++providerDraftRequestRef.current;
    if (providerTomlDirty) return;

    const fallback = providerForm.tomlConfig?.trim()
      || state?.configText?.trim()
      || providerTomlPreview;
    if (!providerForm.providerName.trim() || !providerForm.baseUrl.trim() || !providerForm.model.trim()) {
      setProviderTomlDraft(fallback);
      return;
    }

    void invoke<string>("build_provider_toml_draft", {
      provider: {
        ...providerForm,
        id: providerForm.id || customProviderId(providerForm.providerName || providerForm.baseUrl),
      },
      newProvider: creatingProvider,
      configDir: configDir || null,
    })
      .then((draft) => {
        if (requestId === providerDraftRequestRef.current) setProviderTomlDraft(draft);
      })
      .catch(() => {
        if (requestId === providerDraftRequestRef.current) setProviderTomlDraft(fallback);
      });
  }, [configDir, creatingProvider, providerDraftRefreshToken, providerForm, providerMode, providerTomlDirty, providerTomlPreview, state?.configText]);

  React.useEffect(() => {
    if (tab === "provider" && providerMode === "form") return;
    providerModelsRequestRef.current += 1;
    setAvailableProviderModels([]);
    setProviderModelsLoading(false);
    if (providerModelsLoading) setToast("");
  }, [providerMode, providerModelsLoading, tab]);

  React.useEffect(() => {
    if (tab === "provider" && providerMode === "official") return;
    officialDraftRequestRef.current += 1;
    invalidateOfficialDraftSync(false);
    clearActionBusy("loadOfficialDraft");
  }, [clearActionBusy, invalidateOfficialDraftSync, providerMode, tab]);

  React.useEffect(() => { invalidateOfficialDraftSync(); }, [configDir, invalidateOfficialDraftSync]);

  React.useEffect(() => {
    if (tab !== "provider" || providerMode !== "official") return;
    void loadOfficialModels();
    // A tab change may have cancelled an in-flight response. Resume the user's
    // last edit direction instead of treating an unsynchronized pair as saved.
    const pending = officialDraftPendingEditRef.current;
    if (pending?.scope === configDir) {
      const form = officialFormRef.current;
      void synchronizeOfficialDraft(form.configText, form.model, pending.edit);
    }
  }, [loadOfficialModels, providerMode, tab]);

  React.useEffect(() => {
    if (providerMode !== "official") return;
    const fit = () => {
      fitCodeEditorHeight(officialTomlEditorRef.current, 360);
      fitCodeEditorHeight(officialAuthEditorRef.current, 420);
    };
    const frame = window.requestAnimationFrame(fit);
    window.addEventListener("resize", fit);
    return () => {
      window.cancelAnimationFrame(frame);
      window.removeEventListener("resize", fit);
    };
  }, [officialForm.authJson, officialForm.configText, providerMode]);

  React.useEffect(() => {
    if (!state || promptModeSyncedRef.current === state.codexDir) return;
    promptModeSyncedRef.current = state.codexDir;
    if (state.instructionInjectionMode) {
      setPromptInjectionMode(state.instructionInjectionMode);
    }
  }, [state]);

  React.useEffect(() => {
    setVisitedTabs((tabs) => {
      if (tabs.has(tab)) return tabs;
      const next = new Set(tabs);
      next.add(tab);
      return next;
    });
  }, [tab]);

  const currentProvider = state?.providers.find((p) => p.isCurrent);
  const liveProviderId = (state?.modelProvider || "openai").trim();
  const liveProviderApiKey = React.useMemo(() => {
    const configKey = extractTomlProviderApiKey(state?.configText, liveProviderId);
    if (!state?.isOfficialProvider) return configKey;
    return extractOpenAiApiKey(state?.authText).trim();
  }, [liveProviderId, state?.authText, state?.configText, state?.isOfficialProvider]);
  const inferredActiveProviderId = React.useMemo(() => {
    if (state?.isOfficialProvider) return "";
    if (state?.activeSavedProviderId && savedProviders.some((item) => item.id === state.activeSavedProviderId)) {
      return state.activeSavedProviderId;
    }
    const fallback = liveProviderId === "custom"
      ? savedProviders.find((item) => item.id === activeProviderId)
      : savedProviders.find((item) => item.id === liveProviderId);
    if (!fallback || !currentProvider) return "";
    return savedProviderMatchesProfile(fallback, {
      baseUrl: currentProvider.baseUrl,
      apiKey: liveProviderApiKey,
      providerName: currentProvider.name,
      model: state?.model,
    }) ? fallback.id : "";
  }, [activeProviderId, currentProvider, liveProviderApiKey, liveProviderId, savedProviders, state?.activeSavedProviderId, state?.isOfficialProvider, state?.model]);
  const effectiveActiveProviderId = state?.isOfficialProvider ? "" : inferredActiveProviderId;
  const currentInstructionPath = (state?.instructionFile || "").replace(/\\/g, "/");
  const currentInstructionFilename = currentInstructionPath.split("/").pop() || "";
  const activeInstructionTitle = React.useMemo(() => {
    const templateKey = state?.instructionTemplateKey || "";
    if (templateKey.startsWith("builtin:")) {
      const id = templateKey.slice("builtin:".length);
      return instructionTemplates.find((item) => item.id === id)?.title || id;
    }
    if (templateKey.startsWith("saved:")) {
      const id = templateKey.slice("saved:".length);
      return savedPrompts.find((item) => item.id === id)?.title || id;
    }
    return savedPrompts.find((item) => item.filename === currentInstructionFilename)?.title
      || instructionTemplates.find((item) => item.filename === currentInstructionFilename)?.title
      || currentInstructionFilename
      || (lang === "zh" ? "当前提示词" : "Current prompt");
  }, [currentInstructionFilename, instructionTemplates, lang, savedPrompts, state?.instructionTemplateKey]);
  const detectedRows = React.useMemo(() => {
    if (state?.isOfficialProvider) return [];
    return (state?.providers || []).filter((p) => p.isCurrent).map((p) => {
      const configKey = extractTomlProviderApiKey(state?.configText, p.id);
      const apiKey = p.isCurrent ? liveProviderApiKey || configKey : configKey;
      return {
        id: `detected-${p.id}`,
        source: "detected" as const,
        providerName: p.name || p.id,
        baseUrl: p.baseUrl || "",
        model: state?.model || "gpt-5.5",
        apiKey,
        wireApi: p.wireApi || "responses",
        requiresOpenaiAuth: p.requiresOpenaiAuth ?? false,
        isCurrent: p.isCurrent,
      };
    });
  }, [liveProviderApiKey, state?.configText, state?.isOfficialProvider, state?.model, state?.providers]);

  const localRows = React.useMemo(() => {
    return savedProviders.map((p) => ({
      ...p,
      model: effectiveActiveProviderId === p.id ? state?.model || p.model : p.model,
      source: "local" as const,
      isCurrent: effectiveActiveProviderId === p.id,
    }));
  }, [effectiveActiveProviderId, savedProviders, state?.model]);

  const defaultOfficialProfileId = officialProfiles.find((profile) => profile.isDefault)?.id
    || officialProfiles[0]?.id || DEFAULT_OFFICIAL_PROFILE_ID;
  const currentOfficialProfileId = state?.isOfficialProvider
    ? state.activeOfficialProfileId || defaultOfficialProfileId
    : "";
  const currentOfficialProfile = officialProfiles.find((profile) => profile.id === currentOfficialProfileId);
  const providerRows = React.useMemo<ProviderRow[]>(() => {
    const defaultProfile = officialProfiles.find((profile) => profile.isDefault) || officialProfiles[0] || {
      id: DEFAULT_OFFICIAL_PROFILE_ID,
      providerName: "OpenAI Official",
      model: null,
      isDefault: true,
      hasAuth: Boolean(state?.officialAuthAvailable),
      hasOwnedAuth: Boolean(state?.officialAuthAvailable),
      email: null,
      planType: null,
      canQueryQuota: false,
    };
    const officialRow = (profile: typeof defaultProfile): ProviderRow => ({
      id: profile.id,
      source: "official",
      providerName: profile.providerName,
      baseUrl: "https://chatgpt.com/codex",
      model: profile.id === currentOfficialProfileId ? profile.model || state?.model || "" : profile.model || "",
      apiKey: "",
      wireApi: "official",
      requiresOpenaiAuth: true,
      isCurrent: profile.id === currentOfficialProfileId,
      isDefaultOfficial: profile.isDefault,
      hasAuth: profile.hasOwnedAuth,
      email: profile.email,
      planType: profile.planType,
      canQueryQuota: profile.canQueryQuota,
    });
    const rows = orderProviderRows(officialRow(defaultProfile), detectedRows, localRows);
    return [rows[0], ...officialProfiles.filter((profile) => profile.id !== defaultProfile.id).map(officialRow), ...rows.slice(1)];
  }, [currentOfficialProfileId, detectedRows, lang, localRows, officialProfiles, state?.officialAuthAvailable, state?.model]);

  const findLocalProviderForRow = React.useCallback((row: ProviderRow) => {
    if (row.source === "official") return undefined;
    if (row.source === "local") return savedProviders.find((item) => item.id === row.id);
    const matches = savedProviders.filter((item) => savedProviderMatchesProfile(item, row));
    return matches.find((item) => item.id === effectiveActiveProviderId)
      || matches.find((item) => item.id === activeProviderId)
      || (matches.length === 1 ? matches[0] : undefined);
  }, [activeProviderId, effectiveActiveProviderId, savedProviders]);

  const providerCopySourceForRow = React.useCallback((row: ProviderRow): SavedProvider | undefined => {
    const local = findLocalProviderForRow(row);
    if (local) return local;
    if (row.source !== "detected") return undefined;
    return {
      id: customProviderId(row.providerName || row.baseUrl),
      providerName: row.providerName,
      baseUrl: row.baseUrl,
      model: row.model,
      apiKey: row.apiKey || "",
      tomlConfig: state?.configText || "",
      wireApi: row.wireApi,
      requiresOpenaiAuth: row.requiresOpenaiAuth,
    };
  }, [findLocalProviderForRow, state?.configText]);

  const officialProfileCount = providerRows.filter((row) => row.source === "official").length;
  const providerPageRows = React.useMemo<ProviderRow[]>(() => applyProviderRowOrder(providerRows,
    providerOrder.directory === state?.codexDir ? providerOrder.order : []).map((row) => {
    const local = findLocalProviderForRow(row);
    return {
      id: row.id,
      source: row.source,
      providerName: row.providerName,
      baseUrl: row.baseUrl,
      model: row.model,
      modelDisplayName: local?.modelMappings?.find((mapping) => mapping.model === row.model)?.displayName,
      apiKey: row.apiKey,
      wireApi: row.wireApi,
      upstreamApi: local?.upstreamApi || row.upstreamApi,
      requiresOpenaiAuth: row.requiresOpenaiAuth,
      isCurrent: row.isCurrent,
      isDefaultOfficial: row.isDefaultOfficial,
      hasAuth: row.hasAuth,
      email: row.email,
      planType: row.planType,
      canQueryQuota: row.canQueryQuota,
      meta: row.meta,
      sourceLabel: row.source === "official" ? (lang === "zh" ? "Codex 登录" : "Codex login") : undefined,
      editable: row.source === "official" || Boolean(local) || row.source === "detected",
      duplicable: row.source === "official" || Boolean(providerCopySourceForRow(row)),
      deletable: row.source === "official" ? officialProfileCount > 1 : Boolean(local),
      testable: row.source !== "official",
      testingKey: `${row.source}-${row.id}`,
    };
  }), [findLocalProviderForRow, lang, officialProfileCount, providerCopySourceForRow, providerRows, providerOrder, state?.codexDir]);

  const balanceRevision = React.useMemo(() => JSON.stringify([
    providerRows.filter(row => row.source !== "official").map(row => [row.source, row.id, row.baseUrl, row.apiKey]),
    savedProviders.map(provider => [provider.id, provider.tomlConfig]),
    state?.configText,
  ]), [providerRows, savedProviders, state?.configText]);
  const providerBalances = useProviderBalances(state?.codexDir || configDir, balanceRevision, Boolean(state) && !refreshing);

  const saveProviderOrder = async (order: string[]): Promise<boolean> => {
    const directory = state?.codexDir;
    if (!directory || providerOrder.directory !== directory || loadingTokensRef.current.size || actionBusyTokensRef.current.size) return false;
    const previous = providerOrder;
    const request = ++providerOrderRequestRef.current;
    const actionToken = beginActionBusy("reorderProviders");
    setProviderOrder({ directory, order });
    try {
      const saved = await invoke<string[]>("save_provider_order", { configDir: directory, order });
      if (request === providerOrderRequestRef.current && normalizedConfigDirForComparison(directory) === activeConfigDirKeyRef.current) {
        setProviderOrder({ directory, order: saved });
        setToast(lang === "zh" ? "供应商顺序已保存" : "Provider order saved");
      }
      return true;
    } catch (nextError) {
      if (request === providerOrderRequestRef.current && normalizedConfigDirForComparison(directory) === activeConfigDirKeyRef.current) {
        setProviderOrder(previous);
        setError(String(nextError));
      }
      return false;
    } finally {
      endActionBusy(actionToken);
    }
  };

  const visibleSessions = React.useMemo(
    () => (sessionStatus?.sessions || []).filter((item) => showInternalSessions || !item.isSubagent),
    [sessionStatus?.sessions, showInternalSessions],
  );

  const filteredSessions = React.useMemo(() => {
    const query = deferredSessionQuery.trim().toLowerCase();
    if (!query) return visibleSessions;
    return visibleSessions.filter((item) => [item.title, item.firstUserMessage, item.cwd, item.rolloutPath, item.modelProvider, item.model, item.id]
      .filter(Boolean)
      .some((value) => String(value).toLowerCase().includes(query)));
  }, [deferredSessionQuery, visibleSessions]);

  const allSessionsByCwd = React.useMemo(() => {
    const groups = new Map<string, SessionPreview[]>();
    for (const item of visibleSessions) {
      const key = item.cwd || (lang === "zh" ? "未记录工作目录" : "No workspace recorded");
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key)!.push(item);
    }
    return groups;
  }, [lang, visibleSessions]);

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

  const sessionHasMismatches = Boolean(sessionStatus?.needsSync);
  const sessionTargetLabel = lang === "zh" ? "共享会话" : "Shared history";
  const sessionSyncCount = sessionMismatchCount(sessionStatus);
  const sessionVisibleTotal = showInternalSessions
    ? (sessionStatus?.topLevelThreads ?? 0) + (sessionStatus?.subagentThreads ?? 0)
    : (sessionStatus?.topLevelThreads ?? 0);
  const sessionPreviewTruncated = sessionVisibleTotal > visibleSessions.length;
  const selectedSessionSet = React.useMemo(() => new Set(selectedSessionIds), [selectedSessionIds]);
  const selectedSessions = React.useMemo(
    () => (sessionStatus?.sessions || []).filter((item) => selectedSessionSet.has(item.id)),
    [selectedSessionSet, sessionStatus?.sessions],
  );

  React.useEffect(() => {
    setSelectedSessionIds((ids) => ids.filter((id) => (sessionStatus?.sessions || []).some((item) => item.id === id)));
  }, [sessionStatus?.sessions]);

  React.useEffect(() => {
    if (sessionDeleteConfirmOpen && selectedSessions.length === 0) {
      setSessionDeleteConfirmOpen(false);
    }
  }, [selectedSessions.length, sessionDeleteConfirmOpen]);

  const call = React.useCallback(async <T,>(fn: () => Promise<T>, success?: (data: T) => void) => {
    const loadingToken = beginLoading();
    setError("");
    try {
      const data = await fn();
      success?.(data);
    } catch (e) {
      setError(String(e));
    } finally {
      endLoading(loadingToken);
    }
  }, [beginLoading, endLoading]);

  const refresh = React.useCallback((includeDiagnostics: boolean) => {
    if (Array.from(actionBusyTokensRef.current.values()).includes("reorderProviders")) return;
    invalidatePromptDetail();
    const requestId = ++refreshRequestRef.current;
    const profilesRequestId = ++officialProfilesRequestRef.current;
    const providersRequestId = ++savedProvidersRequestRef.current;
    officialDraftRequestRef.current += 1;
    clearActionBusy("loadOfficialDraft");
    skillsMcpRequestRef.current += 1;
    skillsMcpLoadedRef.current = "";
    skillsMcpAutoLoadAttemptedRef.current = "";
    const requestedConfigDir = configDirDraft.trim();
    setHealthConfigDir(requestedConfigDir);
    const resolvedConfigDir = requestedConfigDir || null;
    const activeCodexDir = state?.codexDir || configDir;
    setRefreshing(true);
    setError("");
    sessionStateRef.current = null;
    setState(null);
    setOfficialProfiles([]);
    setSessionStatus(null);
    setSkillsMcpState(null);
    setSkillsMcpImportOpen(false);
    setSkillsMcpImportPreview(null);
    setAboutInfo(null);
    aboutLoadKeyRef.current = "";
    sessionAutoLoadKeyRef.current = "";
    sessionStatusTrackerRef.current.invalidate();

    if (includeDiagnostics) {
      setStartupDiagnostics(null);
      setStartupDiagnosticsError("");
      setStartupDiagnosticsLoading(true);
      void invoke<StartupDiagnostics>("get_startup_diagnostics", { configDir: resolvedConfigDir })
        .then((diagnostics) => {
          if (requestId === refreshRequestRef.current) {
            setStartupDiagnostics(diagnostics);
            setStartupDiagnosticsLoading(false);
          }
        })
        .catch(() => {
          if (requestId === refreshRequestRef.current) {
            setStartupDiagnosticsError("unavailable");
            setStartupDiagnosticsLoading(false);
          }
        });
    } else {
      setStartupDiagnosticsLoading(false);
    }

    void invoke<{ changed: boolean }>("repair_provider_model_catalog", { configDir: resolvedConfigDir })
      .then((repair) => {
        if (repair.changed && requestId === refreshRequestRef.current) {
          setToast(lang === "zh" ? "已修复第三方模型的图片输入能力，请重启 Codex 加载新菜单" : "Third-party model image input repaired. Restart Codex to reload the model menu.");
        }
      })
      .catch((repairError) => {
        if (requestId === refreshRequestRef.current) setError(String(repairError));
      })
      .then(() => invoke<CodexState>("get_codex_state", { configDir: resolvedConfigDir }))
      .then((next) => {
        if (requestId !== refreshRequestRef.current) return;
        if (normalizedConfigDirForComparison(next.codexDir)
          !== normalizedConfigDirForComparison(activeCodexDir)) {
          providerDraftRequestRef.current += 1;
          providerModelsRequestRef.current += 1;
          setProviderMode("list");
          setEditingProviderId(null);
          setEditingOfficialProfileId(null);
          setCreatingProvider(false);
          setEditingDetectedProvider(false);
          setProviderTomlDirty(false);
          setProviderCommonConfigDirty(false);
          setProviderTomlDraft("");
          setAvailableProviderModels([]);
          setProviderModelsLoading(false);
        }
        activeConfigDirKeyRef.current = normalizedConfigDirForComparison(next.codexDir);
        setConfigDir(next.codexDir);
        setHealthConfigDir(next.codexDir);
        setConfigDirDraft(next.codexDir);
        sessionStateRef.current = next;
        setState(next);
        setRefreshing(false);

        void Promise.allSettled([
          invoke<SavedProvider[]>("list_saved_providers"),
          invoke<SavedPrompt[]>("list_saved_prompts"),
          invoke<BuiltinPromptStatus[]>("get_builtin_prompt_status"),
          invoke<OfficialProfileSummary[]>("list_official_profiles", { configDir: next.codexDir }),
        ]).then(([providers, prompts, promptStatus, profiles]) => {
          if (requestId !== refreshRequestRef.current) return;
          if (providersRequestId === savedProvidersRequestRef.current && providers.status === "fulfilled") setSavedProviders(providers.value);
          if (profilesRequestId === officialProfilesRequestRef.current) {
            if (profiles.status === "fulfilled") setOfficialProfiles(profiles.value);
            else setError(String(profiles.reason));
          }
          if (prompts.status === "fulfilled") setSavedPrompts(prompts.value);
          if (promptStatus.status === "fulfilled") {
            setBuiltinPromptStatus(uniqueBuiltinPromptStatuses(promptStatus.value));
          }
        });
      })
      .catch((nextError) => {
        if (requestId !== refreshRequestRef.current) return;
        setRefreshing(false);
        setError(String(nextError));
      });
  }, [clearActionBusy, configDir, configDirDraft, invalidatePromptDetail, lang, state?.codexDir]);

  // Independent of get_codex_state: this must still work when a broken TOML
  // prevents the normal app state from loading. No shared loading flags change.
  const configHealth = useConfigHealth({
    configDir: healthConfigDir,
    canCheck: !refreshing && !loading && !actionBusy
      && !(tab === "provider" && providerMode !== "list") && tab !== "toml",
    canNotify: !toast && !error && !startupWizardOpen && !updatePromptOpen
      && !loading && !refreshing && !actionBusy
      && !(tab === "provider" && providerMode !== "list") && tab !== "toml",
    lang,
    reviewing: startupWizardOpen,
    onHint: setToast,
    onRepaired: () => refresh(startupWizardOpen),
  });

  React.useEffect(() => {
    refresh(startupWizardOpen);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  React.useEffect(() => {
    const directory = configDir;
    const scope = normalizedConfigDirForComparison(directory);
    if (!scope) return;
    let disposed = false;
    let inFlight = false;
    let pending = false;
    let eventRevision = 0;
    let unlisten: (() => void) | undefined;
    const canRead = () => !disposed
      && routingUiRef.current.ready && !routingUiRef.current.editing
      && document.visibilityState !== "hidden"
      && scope === activeConfigDirKeyRef.current
      && loadingTokensRef.current.size === 0 && actionBusyTokensRef.current.size === 0;
    const drain = async () => {
      if (!pending || inFlight || !canRead()) return;
      pending = false;
      inFlight = true;
      const eventGeneration = eventRevision;
      const refreshGeneration = refreshRequestRef.current;
      const loadingGeneration = loadingGenerationRef.current;
      const actionGeneration = actionBusyGenerationRef.current;
      const profileGeneration = officialProfilesRequestRef.current;
      const stillCurrent = () => canRead()
        && refreshGeneration === refreshRequestRef.current
        && loadingGeneration === loadingGenerationRef.current
        && actionGeneration === actionBusyGenerationRef.current
        && eventGeneration === eventRevision;
      try {
        const [next, profiles] = await Promise.allSettled([
          invoke<CodexState>("get_codex_state", { configDir: directory }),
          invoke<OfficialProfileSummary[]>("list_official_profiles", { configDir: directory }),
        ]);
        if (!stillCurrent()) {
          pending = !disposed;
          return;
        }
        if (next.status === "fulfilled" && normalizedConfigDirForComparison(next.value.codexDir) === scope) {
          setState((current) => current && stillCurrent()
            && normalizedConfigDirForComparison(current.codexDir) === scope ? next.value : current);
        }
        if (profiles.status === "fulfilled" && profileGeneration === officialProfilesRequestRef.current) {
          officialProfilesRequestRef.current += 1;
          officialProfilesLiveRef.current = profiles.value;
          setOfficialProfiles(profiles.value);
        }
      } catch {
        // A transient bridge failure leaves the current screen and drafts intact.
      } finally {
        inFlight = false;
        // Coalesce events arriving during a read. A busy action/editor will
        // resume this pending update after it closes, preserving unsaved drafts.
        if (pending && canRead()) void drain();
      }
    };
    const wake = () => { void drain(); };
    const request = () => { pending = true; eventRevision += 1; wake(); };
    routingWakeRef.current = wake;
    routingRequestRefreshRef.current = request;
    const onFocus = () => request();
    const onVisibility = () => { if (document.visibilityState !== "hidden") request(); };
    // Backend paths may be canonical while this home is a symlink/alias. Events
    // are hints only: always re-read this configured home, never the payload path.
    void listen<{ codexDir: string }>("provider-routing-changed", () => request())
      .then((release) => { if (disposed) release(); else unlisten = release; }).catch(() => {});
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      disposed = true;
      pending = false;
      unlisten?.();
      if (routingWakeRef.current === wake) routingWakeRef.current = () => {};
      if (routingRequestRefreshRef.current === request) routingRequestRefreshRef.current = () => {};
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [configDir]);

  React.useEffect(() => {
    routingWakeRef.current();
  }, [loading, actionBusy, refreshing, providerMode]);

  React.useEffect(() => {
    const directory = state?.codexDir;
    if (!directory || !(tab === "dashboard" || (tab === "provider" && providerMode === "list"))) return;
    const scope = normalizedConfigDirForComparison(directory);
    const monitor = createOfficialProfileMonitor({
      read: () => invoke<OfficialProfileSummary[]>("list_official_profiles", { configDir: directory }),
      revision: () => officialProfilesRequestRef.current,
      isVisible: () => document.visibilityState !== "hidden",
      canRead: () => officialMonitorReadyRef.current
        && activeConfigDirKeyRef.current === scope
        && loadingTokensRef.current.size === 0
        && actionBusyTokensRef.current.size === 0,
      apply: (profiles) => {
        if (JSON.stringify(profiles) === JSON.stringify(officialProfilesLiveRef.current)) return;
        // This newer snapshot supersedes older foreground list reads, while a
        // mutation that starts after the poll still invalidates it via revision.
        officialProfilesRequestRef.current += 1;
        officialProfilesLiveRef.current = profiles;
        setOfficialProfiles(profiles);
      },
    });
    window.addEventListener("focus", monitor.wake);
    document.addEventListener("visibilitychange", monitor.wake);
    monitor.wake();
    return () => {
      monitor.stop();
      window.removeEventListener("focus", monitor.wake);
      document.removeEventListener("visibilitychange", monitor.wake);
    };
  }, [state?.codexDir, tab, providerMode]);

  React.useEffect(() => {
    if (!state?.codexDir) return;
    const loadKey = state.codexDir;
    if (aboutLoadKeyRef.current === loadKey) return;
    aboutLoadKeyRef.current = loadKey;
    setAboutLoading(true);
    void invoke<AboutInfo>("get_about_info", { configDir: loadKey })
      .then((about) => {
        if (aboutLoadKeyRef.current === loadKey) setAboutInfo(about);
      })
      .catch((aboutError) => {
        if (aboutLoadKeyRef.current !== loadKey) return;
        aboutLoadKeyRef.current = "";
        setError(String(aboutError));
      })
      .finally(() => {
        if (aboutLoadKeyRef.current === loadKey || aboutLoadKeyRef.current === "") {
          setAboutLoading(false);
        }
      });
  }, [state?.codexDir, tab]);

  React.useEffect(() => {
    if (!state) return;
    if (state.isOfficialProvider) {
      if (activeProviderId) {
        localStorage.removeItem(ACTIVE_PROVIDER_KEY);
        setActiveProviderId("");
      }
      return;
    }
    if (!savedProviders.length) return;
    const mappedProviderId = savedProviders.some((item) => item.id === inferredActiveProviderId)
      ? inferredActiveProviderId
      : "";
    if (mappedProviderId && mappedProviderId !== activeProviderId) {
      localStorage.setItem(ACTIVE_PROVIDER_KEY, mappedProviderId);
      setActiveProviderId(mappedProviderId);
      return;
    }
    if (activeProviderId && !savedProviders.some((item) => item.id === activeProviderId)) {
      localStorage.removeItem(ACTIVE_PROVIDER_KEY);
      setActiveProviderId("");
    }
  }, [activeProviderId, inferredActiveProviderId, liveProviderId, savedProviders, state]);

  const handleActionResult = (result: ActionResult) => {
    const profilesRequestId = ++officialProfilesRequestRef.current;
    const providersRequestId = ++savedProvidersRequestRef.current;
    sessionStateRef.current = result.state;
    setState(result.state);
    invalidateSessionStatus();
    setToast(result.message);
    return Promise.allSettled([
      invoke<SavedPrompt[]>("list_saved_prompts"),
      invoke<SavedProvider[]>("list_saved_providers"),
      invoke<OfficialProfileSummary[]>("list_official_profiles", { configDir: result.state.codexDir }),
    ])
      .then(([prompts, providers, profiles]) => {
        if (normalizedConfigDirForComparison(result.state.codexDir) !== activeConfigDirKeyRef.current) return;
        if (prompts.status === "fulfilled") setSavedPrompts(prompts.value);
        if (providersRequestId === savedProvidersRequestRef.current && providers.status === "fulfilled") setSavedProviders(providers.value);
        if (profilesRequestId === officialProfilesRequestRef.current) {
          if (profiles.status === "fulfilled") setOfficialProfiles(profiles.value);
          else setError(String(profiles.reason));
        }
      })
      .catch(() => undefined);
  };

  const switchInstructionTemplate = (templateId: string) =>
    call(
      () => invoke<ActionResult>("enable_instruction_template", { configDir: configDir || null, templateId, injectionMode: promptInjectionMode }),
      handleActionResult,
    );

  const disableInstruction = () =>
    call(
      () => invoke<ActionResult>("disable_instruction", { configDir: configDir || null, deleteFile: true }),
      handleActionResult,
    );

  const disableExternalInstruction = () =>
    call(
      () => invoke<ActionResult>("disable_external_instruction", { configDir: configDir || null }),
      handleActionResult,
    );

  const openAddPrompt = () => {
    invalidatePromptDetail();
    setEditingPromptId(null);
    setEditingBuiltinPrompt(null);
    setPromptForm({ ...blankPromptForm });
    setInstructionMode("form");
  };

  const openEditPrompt = (prompt: SavedPrompt) => {
    invalidatePromptDetail();
    setEditingPromptId(prompt.id);
    setEditingBuiltinPrompt(null);
    setPromptForm(prompt);
    setInstructionMode("form");
  };

  const openEditBuiltinPrompt = async (templateId: string) => {
    const requestId = ++promptDetailRequestRef.current;
    // A local template read must not disable provider or other page actions.
    setPromptDetailLoading(true);
    setError("");
    try {
      const detail = await invoke<BuiltinPromptDetail>("get_builtin_prompt_detail", { templateId });
      if (requestId !== promptDetailRequestRef.current) return;
      setEditingPromptId(null);
      setEditingBuiltinPrompt(detail);
      setPromptForm({
        id: detail.id,
        title: detail.title,
        filename: detail.filename,
        content: detail.content,
      });
      setInstructionMode("form");
    } catch (detailError) {
      if (requestId === promptDetailRequestRef.current) setError(String(detailError));
    } finally {
      if (requestId === promptDetailRequestRef.current) setPromptDetailLoading(false);
    }
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

  const savePromptOnly = () => {
    if (editingBuiltinPrompt) {
      call(
        async () => {
          const detail = await invoke<BuiltinPromptDetail>("save_builtin_prompt_override", {
            templateId: editingBuiltinPrompt.id,
            content: promptForm.content,
          });
          const statuses = await invoke<BuiltinPromptStatus[]>("get_builtin_prompt_status");
          return { detail, statuses };
        },
        ({ detail, statuses }) => {
          setEditingBuiltinPrompt(detail);
          setBuiltinPromptStatus(uniqueBuiltinPromptStatuses(statuses));
          setInstructionMode("list");
          setToast(detail.customized
            ? (lang === "zh"
              ? "本地修改已保存，下次启用时生效；后续 GitHub 同步将跳过这个模板"
              : "Local changes saved for the next activation. Future GitHub syncs will skip this template.")
            : (lang === "zh" ? "内容没有变化，模板将继续参与 GitHub 同步" : "No changes detected. This template will continue to sync from GitHub."));
        },
      );
      return;
    }
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
  };

  const enableSavedPrompt = (id: string) =>
    call(() => invoke<ActionResult>("enable_saved_prompt", { configDir: configDir || null, id, injectionMode: promptInjectionMode }), handleActionResult);

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
    const actionToken = beginActionBusy("importPrompt");
    const loadingToken = beginLoading();
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
      endLoading(loadingToken);
      endActionBusy(actionToken);
      if (promptImportRef.current) promptImportRef.current.value = "";
    }
  };

  const refreshBuiltinPrompts = async ({ quiet = false }: { quiet?: boolean } = {}) => {
    const requestId = ++promptRefreshRequestRef.current;
    if (!quiet) promptAutoRefreshAttemptedRef.current = true;
    if (!quiet) setError("");
    try {
      const existingRequest = promptRefreshInFlightRef.current;
      const request = existingRequest || invoke<BuiltinPromptStatus[]>("refresh_builtin_prompts", { configDir: configDir || null });
      if (!existingRequest) {
        promptRefreshInFlightRef.current = request;
        setPromptSyncing(true);
        const clearRequest = () => {
          if (promptRefreshInFlightRef.current !== request) return;
          promptRefreshInFlightRef.current = null;
          setPromptSyncing(false);
        };
        void request.then(clearRequest, clearRequest);
      }
      const list = await request;
      if (requestId !== promptRefreshRequestRef.current) return;
      const uniqueList = uniqueBuiltinPromptStatuses(list);
      const catalogFailed = uniqueList.some((item) => item.syncIssue === "catalog");
      const contentFetchFailures = uniqueList.filter((item) =>
        item.contentSource === "unavailable" || item.syncIssue === "content",
      ).length;
      if (!catalogFailed) {
        promptCatalogReadyRef.current = true;
        setPromptCatalogReady(true);
        setBuiltinPromptStatus(uniqueList);
      } else if (!promptCatalogReadyRef.current) {
        setBuiltinPromptStatus(uniqueList);
      }
      const updated = uniqueList.filter((item) => item.updated).length;
      if (!quiet) {
        setToast(catalogFailed
          ? promptCatalogReadyRef.current
            ? (lang === "zh" ? "在线模板库暂时不可用，已保留当前列表" : "Online templates are unavailable; keeping the current list")
            : (lang === "zh" ? "在线模板库暂时不可用，已使用本地模板" : "Online templates are unavailable; using local templates")
          : contentFetchFailures > 0
            ? (lang === "zh" ? `模板目录已同步，${contentFetchFailures} 个模板暂用本地内容` : `Template catalog synced; ${contentFetchFailures} template(s) are using local content`)
          : updated > 0
            ? (lang === "zh" ? `已同步 ${updated} 个提示词模板` : `${updated} prompt template(s) synced`)
            : (lang === "zh" ? "提示词模板已是最新" : "Prompt templates are up to date"));
      }
    } catch (e) {
      if (requestId === promptRefreshRequestRef.current) {
        if (!quiet) setError(String(e));
      }
    }
  };

  const normalizedProviderForm = (tomlConfig = providerTomlDraft || providerForm.tomlConfig || buildProviderTomlPreview(providerForm)): SavedProvider => ({
    ...providerForm,
    id: editingProviderId || uniqueId(providerForm.id || customProviderId(providerForm.providerName || providerForm.baseUrl), savedProviders.map((item) => item.id)),
    providerName: providerForm.providerName.trim(),
    baseUrl: providerForm.baseUrl.trim().replace(/\/+$/, ""),
    model: providerForm.model.trim(),
    apiKey: (providerForm.apiKey || "").trim(),
    tomlConfig: tomlConfig.trimEnd(),
    wireApi: "responses",
    upstreamApi: providerUpstreamApi(providerForm),
    requiresOpenaiAuth: providerForm.requiresOpenaiAuth,
  });

  const applyProviderConfig = (provider: SavedProvider) => {
    if (savedProviders.some((saved) => saved.id === provider.id)) {
      return invoke<ActionResult>("activate_saved_provider", { configDir: configDir || null, providerId: provider.id });
    }
    if (providerRequiresConversion(provider)) {
      return Promise.reject(lang === "zh" ? "请先保存供应商，并在设置中开启本地路由和配置接管后启用。" : "Save this provider and enable the local router with config takeover in Settings before activating it.");
    }
    const tomlConfig = provider.tomlConfig?.trim();
    if (tomlConfig) {
      return invoke<ActionResult>("save_provider_toml_config", {
        input: {
          configDir: configDir || null,
          configText: tomlConfig,
          apiKey: provider.apiKey || "",
        },
        providerId: provider.id,
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
  };

  const saveProviderOnly = () => {
    const pendingProvider = normalizedProviderForm();
    const preferTomlAuth = providerTomlDirty && !providerApiKeyEditedRef.current;
    const mappingValidation = validateProviderModelMappings(pendingProvider.modelMappings || [], pendingProvider.model, lang);
    if (pendingProvider.modelMappings?.length && pendingProvider.wireApi !== "responses") {
      setError(lang === "zh" ? "模型映射需要 Responses 接口，请将 Wire API 设为 responses" : "Model mappings require the Responses API. Set Wire API to responses");
      return;
    }
    if (!mappingValidation.valid) {
      setError(lang === "zh" ? "请先修正模型映射中的错误" : "Correct the model mappings before saving");
      return;
    }
    if (!pendingProvider.providerName || !pendingProvider.baseUrl || !pendingProvider.model) {
      setError(lang === "zh"
        ? "请填写供应商名称、API 请求地址和模型"
        : "Provider name, API URL, and model are required");
      return;
    }
    return call(
      async () => {
        let provider = pendingProvider;
        if (!providerTomlDirty) {
          const draftSource = providerForm.tomlConfig?.trim() || "";
          const providerForDraft = normalizedProviderForm(draftSource);
          const latestDraft = await invoke<string>("build_provider_toml_draft", {
            provider: providerForDraft,
            newProvider: creatingProvider,
            configDir: configDir || null,
          });
          provider = { ...providerForDraft, tomlConfig: latestDraft.trimEnd() };
          setProviderTomlDraft(provider.tomlConfig || "");
        }
        const applyAfterSave = editingDetectedProvider
          || Boolean(editingProviderId && editingProviderId === effectiveActiveProviderId);
        const applied = applyAfterSave
          ? await invoke<ActionResult>("save_active_provider", { provider, configDir: configDir || null, applyCommonConfig: providerCommonConfigDirty, preferTomlAuth })
          : null;
        if (!applyAfterSave) await invoke<SavedProvider>("save_provider", { provider, preferTomlAuth });
        const providerList = await invoke<SavedProvider[]>("list_saved_providers");
        return { applied, providerList };
      },
      ({ applied, providerList }) => {
        if (applied) handleActionResult(applied);
        commitSavedProviders(providerList);
        setProviderMode("list");
        setEditingProviderId(null);
        setEditingDetectedProvider(false);
        setProviderTomlDirty(false);
        setProviderCommonConfigDirty(false);
        setToast(applied && pendingProvider.modelMappings?.length
          ? (lang === "zh" ? "已保存，请重启 Codex 更新模型菜单" : "Saved. Restart Codex to update its model menu")
          : applied
          ? (lang === "zh" ? "供应商配置已保存并热更新" : "Provider saved and hot-applied")
          : (lang === "zh" ? "供应商配置已保存" : "Provider saved"));
      },
    );
  };

  const activateProvider = async (apply: () => Promise<ActionResult>, success: (result: ActionResult) => void) => {
    if (!getSessionStatusContext() || providerActivationInFlightRef.current || sessionDeleteBusyRef.current
      || providerActivationIsBusy(loadingTokensRef.current.values(), actionBusyTokensRef.current.values())) return;
    providerActivationInFlightRef.current += 1;
    providerActivationRevisionRef.current += 1;
    if (Array.from(actionBusyTokensRef.current.values()).includes("syncSessions")) sessionRefreshPendingRef.current = true;
    invalidateSessionStatus();
    try {
      await call(apply, success);
    } finally {
      providerActivationInFlightRef.current -= 1;
      if (sessionRefreshPendingRef.current && !sessionMutationInFlightRef.current) {
        await refreshSyncedSessionStatusRef.current();
      }
    }
  };

  const switchProvider = (provider: SavedProvider) =>
    activateProvider(
      () => applyProviderConfig(provider),
      (result) => {
        localStorage.setItem(ACTIVE_PROVIDER_KEY, provider.id);
        setActiveProviderId(provider.id);
        handleActionResult(result);
        if (provider.modelMappings?.length) {
          setToast(lang === "zh" ? "已启用，请重启 Codex 更新模型菜单" : "Enabled. Restart Codex to update its model menu");
        }
      },
    );

  const resetAvailableProviderModels = () => {
    providerModelsRequestRef.current += 1;
    setAvailableProviderModels([]);
    setProviderModelsLoading(false);
  };

  const fetchProviderModels = async () => {
    const baseUrl = providerForm.baseUrl.trim();
    const apiKey = (providerForm.apiKey || "").trim();
    if (!baseUrl) {
      setError("");
      setToast(lang === "zh" ? "请先填写 API 请求地址；认证可使用 API Key 或供应商 Headers" : "Enter the API URL first. Authentication can use an API key or provider headers");
      return;
    }

    const requestId = providerModelsRequestRef.current + 1;
    providerModelsRequestRef.current = requestId;
    setProviderModelsLoading(true);
    setError("");
    setToast(lang === "zh" ? "正在获取模型列表..." : "Fetching model list...");
    try {
      const result = await invoke<ProviderModelsResult>("fetch_provider_models", { baseUrl, apiKey, upstreamApi: providerUpstreamApi(providerForm), configText: providerTomlDraft });
      if (providerModelsRequestRef.current !== requestId) return;
      setAvailableProviderModels(result.models);
      setToast(result.models.length > 0
        ? (lang === "zh" ? `已获取 ${result.models.length} 个模型` : `${result.models.length} models fetched`)
        : (lang === "zh" ? "连接成功，但供应商没有返回模型" : "Connected, but the provider returned no models"));
    } catch (e) {
      if (providerModelsRequestRef.current !== requestId) return;
      setToast("");
      setError(String(e));
    } finally {
      if (providerModelsRequestRef.current === requestId) setProviderModelsLoading(false);
    }
  };

  const testProvider = async (id: string, baseUrl: string, apiKey?: string | null, provider?: SavedProvider) => {
    const actionToken = beginActionBusy("testProvider");
    setProviderTestingId(id);
    setError("");
    setToast(lang === "zh" ? "正在检测连接..." : "Testing connection...");
    try {
      const result = await invoke<ProviderConnectionResult>("test_provider_connection", { baseUrl, apiKey: apiKey || null, upstreamApi: provider ? providerUpstreamApi(provider) : "responses", configText: provider?.tomlConfig || null });
      if (result.ok) {
        setToast(lang === "zh" ? `连接成功，响应延迟 ${result.durationMs}ms` : `Connected, ${result.durationMs}ms latency`);
      } else {
        setToast("");
        setError(lang === "zh" ? `连接失败：${result.message}` : `Connection failed: ${result.message}`);
      }
    } catch (e) {
      setToast("");
      setError(String(e));
    } finally {
      setProviderTestingId("");
      endActionBusy(actionToken);
    }
  };

  const saveProviderConfig = saveProviderOnly;

  const switchOfficialProvider = (profileId = defaultOfficialProfileId) =>
    activateProvider(
      () => invoke<ActionResult>("switch_official_profile", { configDir: configDir || null, profileId }),
      (result) => {
        localStorage.removeItem(ACTIVE_PROVIDER_KEY);
        setActiveProviderId("");
        handleActionResult(result);
      },
    );

  const loadCcSwitchOfficial = async () => {
    if (officialDraftSyncing) return;
    const requestId = ++officialDraftRequestRef.current;
    const revision = officialDraftRevisionRef.current;
    officialModelPristineRef.current = false;
    const actionToken = beginActionBusy("loadCcSwitchOfficial");
    setError("");
    try {
      const candidate = await invoke<OfficialAuthCandidate | null>("read_ccswitch_official_auth", {
        dbPath: null,
      });
      if (requestId !== officialDraftRequestRef.current || revision !== officialDraftRevisionRef.current) return;
      if (!candidate) {
        setToast(lang === "zh" ? "未找到 CC Switch 官方配置" : "No CC Switch official config found");
        return;
      }
      invalidateOfficialDraftSync();
      setOfficialAuthDirty(true);
      const current = officialFormRef.current;
      const next = {
        ...current,
        model: candidate.model || current.model,
        authJson: candidate.authJson,
        configText: candidate.configText || current.configText,
      };
      officialFormRef.current = next;
      setOfficialForm(next);
      void synchronizeOfficialDraft(next.configText, next.model, candidate.configText?.trim() ? "toml" : "model");
      setToast(lang === "zh" ? "已从 CC Switch 载入" : "Loaded from CC Switch");
    } catch (e) {
      if (requestId === officialDraftRequestRef.current) setError(String(e));
    } finally {
      endActionBusy(actionToken);
    }
  };

  const resetOfficialProvider = () =>
    call(
      () => invoke<ActionResult>("reset_official_provider", {
        input: {
          configDir: configDir || null,
          model: officialForm.model,
          authJson: null,
          configText: officialForm.configText,
        },
      }),
      (result) => {
        localStorage.removeItem(ACTIVE_PROVIDER_KEY);
        setActiveProviderId("");
        setOfficialAuthDirty(false);
        setOfficialForm({
          providerName: officialForm.providerName,
          model: result.state.model || officialForm.model,
          authJson: "",
          configText: result.state.configText
            || officialForm.configText
            || buildOfficialTomlPreview(result.state.model || officialForm.model),
        });
        handleActionResult(result);
      },
    );

  const importFromCcSwitch = async () => {
    const actionToken = beginActionBusy("importCcSwitch");
    const loadingToken = beginLoading();
    setError("");
    try {
      const result = await invoke<ImportResult>("import_ccswitch_codex_providers", { dbPath: null });
      const warningText = result.skipped > 0
        ? (lang === "zh" ? `，跳过 ${result.skipped}` : `, ${result.skipped} skipped`)
        : "";
      const successText = lang === "zh"
        ? `cc-switch 导入完成：新增 ${result.added}，更新 ${result.updated}，合并 ${result.merged}${warningText}；未切换当前供应商`
        : `cc-switch import complete: ${result.added} added, ${result.updated} updated, ${result.merged} merged${warningText}; current provider unchanged`;
      try {
        const nextState = await invoke<CodexState>("get_codex_state", { configDir: configDir || null });
        commitSavedProviders(result.providers);
        setState(nextState);
        setToast(successText);
      } catch (refreshError) {
        commitSavedProviders(result.providers);
        setToast(lang === "zh"
          ? `${successText}；状态刷新失败，请手动刷新：${String(refreshError)}`
          : `${successText}; state refresh failed, refresh manually: ${String(refreshError)}`);
      }
    } catch (importError) {
      setError(String(importError));
    } finally {
      endLoading(loadingToken);
      endActionBusy(actionToken);
    }
  };

  const openExternalUrl = React.useCallback((url?: string | null) => {
    if (!url) return;
    window.setTimeout(() => {
      void invoke("open_url", { url }).catch(() => {
        setToast(lang === "zh" ? "打开浏览器失败" : "Failed to open browser");
      });
    }, 0);
  }, [lang]);

  const checkForUpdates = React.useCallback(async ({ quiet = false }: { quiet?: boolean } = {}) => {
    setReleaseInfo({ status: "checking" });
    let nativeCheckAttempted = false;
    try {
      if (aboutInfo?.nativeUpdaterSupported !== false) {
        nativeCheckAttempted = true;
        const updaterResult = await appUpdater.check({ force: !quiet, timeout: 30_000 });
        if (updaterResult === "available") {
          const snapshot = appUpdater.getSnapshot();
          const latestVersion = snapshot.latestVersion || "";
          const releaseTag = latestVersion.startsWith("v") ? latestVersion : `v${latestVersion}`;
          setReleaseInfo({
            status: "ok",
            latestVersion: releaseTag,
            htmlUrl: `https://github.com/${FALLBACK_GITHUB_REPO}/releases/tag/${releaseTag}`,
            hasUpdate: true,
            updateMethod: "native",
          });
          if (quiet) {
            setToast(lang === "zh" ? `发现新版本 ${releaseTag}，可在概览页查看` : `New version ${releaseTag} is available`);
          } else {
            setUpdatePromptOpen(true);
          }
          return;
        }

        if (updaterResult === "up-to-date") {
          setReleaseInfo({
            status: "ok",
            latestVersion: aboutInfo?.appVersion,
            htmlUrl: `https://github.com/${FALLBACK_GITHUB_REPO}/releases/latest`,
            hasUpdate: false,
            updateMethod: "native",
          });
          if (!quiet) setToast(lang === "zh" ? "当前已是最新版本" : "You are up to date");
          return;
        }
      }

      // Metadata is useful even when the native channel is unavailable. It
      // cannot supply an updater RID or replace the signed installation check.
      const update = await invoke<AppUpdateInfo>("check_app_update");
      setReleaseInfo(releaseInfoFromFallback(update, nativeCheckAttempted));
      if (nativeCheckAttempted) {
        if (quiet && update.hasUpdate) {
          setToast(lang === "zh"
            ? `发现新版本 ${update.latestVersion}，但在线更新检查失败，可重新检查`
            : `Version ${update.latestVersion} is available, but the in-app update check failed. Retry the check`);
        } else if (!quiet) {
          setUpdatePromptOpen(true);
        }
        return;
      }
      if (update.hasUpdate) {
        if (quiet) {
          setToast(lang === "zh" ? `发现新版本 ${update.latestVersion}，可在概览页查看` : `New version ${update.latestVersion} is available`);
        } else {
          setUpdatePromptOpen(true);
        }
      } else if (!quiet) {
        setToast(lang === "zh" ? "当前已是最新版本" : "You are up to date");
      }
    } catch {
      setReleaseInfo(releaseInfoFromCheckFailure(nativeCheckAttempted));
      if (!quiet && nativeCheckAttempted) {
        setUpdatePromptOpen(true);
      } else if (!quiet) {
        setToast(lang === "zh" ? "检查失败" : "Check failed");
      }
    }
  }, [aboutInfo?.appVersion, aboutInfo?.nativeUpdaterSupported, lang]);

  React.useEffect(() => {
    if (!state || !aboutInfo || autoUpdateCheckedRef.current) return;
    autoUpdateCheckedRef.current = true;
    void checkForUpdates({ quiet: true });
  }, [aboutInfo, state, checkForUpdates]);

  React.useEffect(() => {
    if (!state || tab !== "instruction" || promptAutoRefreshAttemptedRef.current) return;
    promptAutoRefreshAttemptedRef.current = true;
    void refreshBuiltinPrompts({ quiet: true });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state, tab]);

  const beginSkillsMcpRequest = React.useCallback(() => ({
    requestId: ++skillsMcpRequestRef.current,
    configDir: configDir || null,
    configDirKey: normalizedConfigDirForComparison(configDir),
  }), [configDir]);

  const isCurrentSkillsMcpRequest = React.useCallback((requestId: number, configDirKey: string) => (
    requestId === skillsMcpRequestRef.current
      && configDirKey === activeConfigDirKeyRef.current
  ), []);

  const loadSkillsMcp = React.useCallback(async ({ quiet = false }: { quiet?: boolean } = {}) => {
    const request = beginSkillsMcpRequest();
    const actionToken = quiet ? null : beginActionBusy("loadSkillsMcp");
    if (!quiet) setError("");
    try {
      const result = await invoke<SkillsMcpState>("get_skills_mcp_state", { configDir: request.configDir });
      if (!isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) return;
      skillsMcpLoadedRef.current = request.configDirKey;
      setSkillsMcpState(result);
    } catch (e) {
      if (!quiet && isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) {
        setError(String(e));
      }
    } finally {
      if (actionToken !== null) endActionBusy(actionToken);
    }
  }, [beginActionBusy, beginSkillsMcpRequest, endActionBusy, isCurrentSkillsMcpRequest]);

  React.useEffect(() => {
    const configDirKey = normalizedConfigDirForComparison(configDir);
    if (tab !== "skillsMcp") {
      skillsMcpAutoLoadAttemptedRef.current = "";
      return;
    }
    if (!state?.codexDir
      || !configDirKey
      || Boolean(actionBusy)
      || skillsMcpAutoLoadAttemptedRef.current === configDirKey
      || skillsMcpLoadedRef.current === configDirKey) return;
    skillsMcpAutoLoadAttemptedRef.current = configDirKey;
    void loadSkillsMcp();
  }, [actionBusy, configDir, loadSkillsMcp, state?.codexDir, tab]);

  const openImportExistingSkillsMcpPreview = async () => {
    const request = beginSkillsMcpRequest();
    const actionToken = beginActionBusy("previewExistingSkillsMcp");
    setError("");
    try {
      const preview = await invoke<SkillsMcpImportPreview>("preview_existing_skills_mcp", { configDir: request.configDir });
      if (!isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) return;
      if (preview.skills.length + preview.mcpServers.length === 0) {
        setSkillsMcpImportPreview(null);
        setSkillsMcpImportOpen(false);
        setToast(lang === "zh" ? "没有需要新导入的 Skills 或 MCP" : "No new Skills or MCP items to import");
        return;
      }
      setSkillsMcpImportPreview(preview);
      setSkillsMcpImportOpen(true);
    } catch (e) {
      if (isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) setError(String(e));
    } finally {
      endActionBusy(actionToken);
    }
  };

  const importExistingSkillsMcp = async () => {
    const request = beginSkillsMcpRequest();
    const actionToken = beginActionBusy("importExistingSkillsMcp");
    setError("");
    try {
      const result = await invoke<SkillsMcpActionResult>("import_existing_skills_mcp", { configDir: request.configDir });
      if (!isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) return;
      skillsMcpLoadedRef.current = request.configDirKey;
      setSkillsMcpState(result.state);
      setSkillsMcpImportOpen(false);
      setSkillsMcpImportPreview(null);
      setToast(result.message);
    } catch (e) {
      if (isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) setError(String(e));
    } finally {
      endActionBusy(actionToken);
    }
  };

  const checkSkillUpdatesAction = async () => {
    const request = beginSkillsMcpRequest();
    const actionToken = beginActionBusy("checkSkillUpdates");
    setError("");
    try {
      const result = await invoke<SkillsMcpState>("check_skill_updates", { configDir: request.configDir });
      if (!isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) return;
      skillsMcpLoadedRef.current = request.configDirKey;
      setSkillsMcpState(result);
      setToast(lang === "zh" ? "Skills 更新状态已刷新" : "Skill update status refreshed");
    } catch (e) {
      if (isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) setError(String(e));
    } finally {
      endActionBusy(actionToken);
    }
  };

  const toggleSkillEnabled = async (id: string, enabled: boolean) => {
    const request = beginSkillsMcpRequest();
    const actionToken = beginActionBusy(`skill:${id}`);
    setError("");
    try {
      const result = await invoke<SkillsMcpState>("toggle_codex_skill", { configDir: request.configDir, id, enabled });
      if (!isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) return;
      skillsMcpLoadedRef.current = request.configDirKey;
      setSkillsMcpState(result);
      setToast(enabled ? (lang === "zh" ? "Skill 已启用" : "Skill enabled") : (lang === "zh" ? "Skill 已禁用" : "Skill disabled"));
    } catch (e) {
      if (isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) setError(String(e));
    } finally {
      endActionBusy(actionToken);
    }
  };

  const toggleMcpEnabled = async (id: string, enabled: boolean) => {
    const request = beginSkillsMcpRequest();
    const actionToken = beginActionBusy(`mcp:${id}`);
    setError("");
    try {
      const result = await invoke<SkillsMcpState>("toggle_codex_mcp", { configDir: request.configDir, id, enabled });
      if (!isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) return;
      skillsMcpLoadedRef.current = request.configDirKey;
      setSkillsMcpState(result);
      setToast(enabled ? (lang === "zh" ? "MCP 已启用" : "MCP enabled") : (lang === "zh" ? "MCP 已禁用" : "MCP disabled"));
    } catch (e) {
      if (isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) setError(String(e));
    } finally {
      endActionBusy(actionToken);
    }
  };

  const saveSkillsMcpNote = async (itemKind: SkillsMcpNoteKind, id: string, note: string) => {
    if (skillsMcpNoteBusyRef.current) return false;
    const noteBusyKey = `note:${itemKind}:${id}`;
    skillsMcpNoteBusyRef.current = noteBusyKey;
    setSkillsMcpNoteBusy(noteBusyKey);
    const request = beginSkillsMcpRequest();
    const actionToken = beginActionBusy(noteBusyKey);
    setError("");
    try {
      const result = await invoke<SkillsMcpState>("save_skills_mcp_note", {
        configDir: request.configDir,
        itemKind,
        id,
        note,
      });
      if (!isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) return false;
      skillsMcpLoadedRef.current = request.configDirKey;
      setSkillsMcpState(result);
      setToast(lang === "zh" ? "备注已保存" : "Note saved");
      return true;
    } catch (e) {
      if (isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) setError(String(e));
      return false;
    } finally {
      endActionBusy(actionToken);
      if (skillsMcpNoteBusyRef.current === noteBusyKey) {
        skillsMcpNoteBusyRef.current = "";
        setSkillsMcpNoteBusy("");
      }
    }
  };

  const restartCodexDesktop = async () => {
    if (restartCodexBusyRef.current || loadingTokensRef.current.size || actionBusyTokensRef.current.size) return false;
    restartCodexBusyRef.current = true;
    setRestartCodexBusy(true);
    const actionToken = beginActionBusy("restartCodexDesktop");
    setError("");
    setRestartCodexError("");
    try {
      const result = await invoke<CodexDesktopRestartResult>("restart_codex_desktop", { configDir });
      setLanguageRestartNotice(null);
      const message = result.wasRunning
        ? (lang === "zh" ? `${result.appName} 已重新启动` : `${result.appName} restarted`)
        : (lang === "zh" ? `${result.appName} 已启动` : `${result.appName} started`);
      setToast(result.warning ? `${message}\n${result.warning}` : message);
      return true;
    } catch (e) {
      setError(String(e));
      setRestartCodexError(String(e));
      return false;
    } finally {
      endActionBusy(actionToken);
      restartCodexBusyRef.current = false;
      setRestartCodexBusy(false);
    }
  };

  const installSkillZipFile = async () => {
    if (nativeTransferBusyRef.current) return;
    nativeTransferBusyRef.current = true;
    const request = beginSkillsMcpRequest();
    const actionToken = beginActionBusy("installSkillZip");
    setError("");
    try {
      const result = await invoke<SkillsMcpActionResult | null>("import_skills_mcp_archive", { configDir: request.configDir, lang });
      if (!result || !isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) return;
      skillsMcpLoadedRef.current = request.configDirKey;
      setSkillsMcpState(result.state);
      setToast(result.message);
    } catch (e) {
      if (isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) setError(String(e));
    } finally {
      endActionBusy(actionToken);
      nativeTransferBusyRef.current = false;
    }
  };

  const exportSkillsMcp = async (kind: "mcp" | "skills") => {
    if (nativeTransferBusyRef.current) return;
    nativeTransferBusyRef.current = true;
    const request = beginSkillsMcpRequest();
    const actionToken = beginActionBusy("exportSkillsMcp");
    setError("");
    try {
      const result = await invoke<{ path: string; exportedSkills: number; exportedMcp: number } | null>("export_skills_mcp_archive", { configDir: request.configDir, kind, lang });
      if (!result || !isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) return;
      const count = kind === "mcp" ? result.exportedMcp : result.exportedSkills;
      setToast(lang === "zh" ? `已导出 ${count} 个 ${kind === "mcp" ? "MCP" : "Skills"}` : `Exported ${count} ${kind === "mcp" ? "MCP servers" : "Skills"}`);
    } catch (e) {
      if (isCurrentSkillsMcpRequest(request.requestId, request.configDirKey)) setError(String(e));
    } finally {
      endActionBusy(actionToken);
      nativeTransferBusyRef.current = false;
    }
  };

  const exportSessions = async (ids: string[]) => {
    if (!ids.length || nativeTransferBusyRef.current) return;
    nativeTransferBusyRef.current = true;
    setSessionExportBusy(true);
    const directory = configDir;
    const directoryKey = normalizedConfigDirForComparison(directory);
    const generation = refreshRequestRef.current;
    const isCurrentExport = () => generation === refreshRequestRef.current && activeConfigDirKeyRef.current === directoryKey;
    const actionToken = beginActionBusy("exportSessions");
    setError("");
    try {
      const suggestedName = ids.length === 1
        ? sessionStatus?.sessions.find((session) => session.id === ids[0])?.title || "Codex-session"
        : `Codex-sessions-${new Date().toISOString().slice(0, 10)}`;
      const result = await invoke<{ path: string; exportedSessions: number; failedSessions: number; warnings: string[] } | null>("export_codex_sessions", { configDir: directory || null, sessionIds: ids, suggestedName, lang });
      if (!result || !isCurrentExport()) return;
      const message = lang === "zh" ? `已导出 ${result.exportedSessions} 个会话` : `Exported ${result.exportedSessions} conversation(s)`;
      if (result.failedSessions || result.warnings.length) setError(`${message}；${result.warnings.join("；")}`);
      else setToast(message);
    } catch (e) {
      if (isCurrentExport()) setError(String(e));
    } finally {
      endActionBusy(actionToken);
      setSessionExportBusy(false);
      nativeTransferBusyRef.current = false;
    }
  };

  const openOfficialEdit = async (profileId = defaultOfficialProfileId) => {
    providerCreationRequestRef.current += 1;
    const requestId = ++officialDraftRequestRef.current;
    invalidateOfficialDraftSync();
    officialModelPristineRef.current = false;
    const actionToken = beginActionBusy("loadOfficialDraft");
    const profile = officialProfiles.find((item) => item.id === profileId);
    setEditingDetectedProvider(false);
    setEditingOfficialProfileId(profileId);
    setCreatingProvider(false);
    setOfficialAuthDirty(false);
    setOfficialForm({
      providerName: profile?.providerName || "OpenAI Official",
      model: profile?.model || "",
      authJson: "",
      configText: "",
    });
    setProviderMode("official");
    setError("");
    try {
      const draft = await invoke<OfficialProfileDetail>("get_official_profile", {
        configDir: configDir || null,
        profileId,
      });
      if (requestId !== officialDraftRequestRef.current) return;
      const next = {
        providerName: draft.providerName,
        model: draft.model || "",
        authJson: draft.authJson,
        configText: draft.configText,
      };
      officialFormRef.current = next;
      setOfficialForm(next);
      void synchronizeOfficialDraft(next.configText, next.model, "toml");
    } catch (e) {
      if (requestId === officialDraftRequestRef.current) {
        setError(String(e));
        setProviderMode("list");
      }
    } finally {
      endActionBusy(actionToken);
    }
  };

  const saveOfficialConfig = async () => {
    if (loadingTokensRef.current.size || actionBusyTokensRef.current.size) return;
    const request = officialDraftRequestRef.current;
    const context = officialContextRef.current;
    let synchronized = true;
    // A fast click/keyboard save must wait for the latest model/TOML pair,
    // including an edit queued while an earlier synchronization was pending.
    for (;;) {
      const pending = officialDraftSyncRef.current;
      synchronized = await pending;
      if (pending === officialDraftSyncRef.current) break;
    }
    if (request !== officialDraftRequestRef.current || context !== officialContextRef.current) return;
    if (loadingTokensRef.current.size || actionBusyTokensRef.current.size) return;
    if (!synchronized || officialDraftSyncErrorRef.current) {
      setError(officialDraftSyncErrorRef.current || (lang === "zh" ? "模型与 TOML 尚未同步，请修正草稿后再保存。" : "The model and TOML draft are not synchronized. Correct the draft before saving."));
      return;
    }
    officialModelPristineRef.current = false;
    const form = officialFormRef.current;
    if (!form.providerName.trim()) {
      setError(lang === "zh" ? "请填写供应商名称" : "Provider name is required");
      return;
    }
    return call(
      () =>
        invoke<OfficialProfileActionResult>("save_official_profile", {
          input: {
            configDir: configDir || null,
            id: editingOfficialProfileId,
            providerName: form.providerName.trim(),
            model: form.model,
            authJson: officialAuthDirtyRef.current || editingOfficialProfileId === null ? form.authJson : null,
            configText: form.configText,
          },
        }),
      (result) => {
        handleActionResult(result);
        setCreatingProvider(false);
        setProviderMode("list");
      },
    );
  };

  const loadCurrentOfficial = async () => {
    if (officialDraftSyncing) return;
    const requestId = ++officialDraftRequestRef.current;
    const revision = officialDraftRevisionRef.current;
    officialModelPristineRef.current = false;
    const actionToken = beginActionBusy("loadCurrentOfficial");
    setError("");
    try {
      const current = await invoke<CodexState>("get_codex_state", { configDir: configDir || null });
      if (requestId !== officialDraftRequestRef.current || revision !== officialDraftRevisionRef.current) return;
      if (!current.isOfficialProvider) {
        throw new Error(lang === "zh" ? "请先在 Codex 中切换到官方登录" : "Switch Codex to official sign-in first");
      }
      invalidateOfficialDraftSync();
      setOfficialAuthDirty(true);
      const next = {
        ...officialFormRef.current,
        model: current.model || "",
        configText: current.configText,
        authJson: current.authText || "",
      };
      officialFormRef.current = next;
      setOfficialForm(next);
      void synchronizeOfficialDraft(next.configText, next.model, "toml");
      setToast(lang === "zh" ? "已读取当前官方登录，保存后保留到此配置" : "Current sign-in loaded. Save to keep it in this profile.");
    } catch (loadError) {
      if (requestId === officialDraftRequestRef.current) setError(String(loadError));
    } finally {
      endActionBusy(actionToken);
    }
  };

  const duplicateOfficialProfile = (row: ProviderRow) => {
    const providerName = `${row.providerName}${lang === "zh" ? " 副本" : " Copy"}`;
    return call(
      () => invoke<OfficialProfileActionResult>("duplicate_official_profile", {
        configDir: configDir || null, profileId: row.id, providerName,
      }),
      (result) => {
        if (normalizedConfigDirForComparison(result.state.codexDir) !== activeConfigDirKeyRef.current) return;
        handleActionResult(result);
        setToast(lang === "zh" ? `已复制为“${result.profile.providerName}”` : `Created “${result.profile.providerName}”`);
      },
    );
  };

  const changeProviderKind = async (kind: "api" | "official") => {
    const requestId = ++officialDraftRequestRef.current;
    invalidateOfficialDraftSync();
    clearActionBusy("loadOfficialDraft");
    if (kind === "api") {
      setProviderMode("form");
      return;
    }
    setEditingOfficialProfileId(null);
    setProviderMode("official");
    if (officialFormRef.current.configText.trim()) {
      void synchronizeOfficialDraft(officialFormRef.current.configText, officialFormRef.current.model, "toml");
      return;
    }
    // Start with a complete official template so a context-window toggle does
    // not turn an otherwise empty draft into a two-line replacement config.
    const revision = officialDraftRevisionRef.current;
    const scope = activeConfigDirKeyRef.current;
    const generation = refreshRequestRef.current;
    const actionToken = beginActionBusy("loadOfficialDraft");
    try {
      const catalogRequest = loadOfficialModels();
      const template = await invoke<OfficialProfileDetail>("get_official_profile", {
        configDir: configDir || null, profileId: defaultOfficialProfileId,
      });
      const isCurrent = () => requestId === officialDraftRequestRef.current
        && revision === officialDraftRevisionRef.current && scope === activeConfigDirKeyRef.current
        && generation === refreshRequestRef.current;
      if (!isCurrent()) return;
      const model = officialModelPristineRef.current
        ? officialModelsRef.current.recommendedModel || INITIAL_OFFICIAL_MODEL : officialFormRef.current.model;
      const configText = await invoke<string>("update_official_model_draft", { configText: template.configText, model });
      if (!isCurrent()) return;
      const next = { ...officialFormRef.current, configText, model };
      officialFormRef.current = next;
      setOfficialForm(next);
      // The template can be edited immediately while local discovery finishes.
      // Only this untouched creation draft may adopt its explicit recommendation.
      void catalogRequest.then(catalog => {
        if (!isCurrent() || !officialModelPristineRef.current || !catalog.recommendedModel
          || loadingTokensRef.current.size || actionBusyTokensRef.current.size
          || catalog.recommendedModel === officialFormRef.current.model) return;
        void synchronizeOfficialDraft(officialFormRef.current.configText, catalog.recommendedModel, "model");
      });
    } catch (templateError) {
      if (requestId === officialDraftRequestRef.current) {
        setError(String(templateError));
        setProviderMode("list");
      }
    } finally {
      endActionBusy(actionToken);
    }
  };

  const newCustomProviderForm = (configText = providerCreationBase): SavedProvider => ({
    ...blankProviderForm,
    model: state?.model?.trim() || blankProviderForm.model,
    wireApi: "responses",
    upstreamApi: providerUpstreamApi(currentProvider || blankProviderForm),
    requiresOpenaiAuth: currentProvider?.requiresOpenaiAuth ?? blankProviderForm.requiresOpenaiAuth,
    tomlConfig: configText.trim(),
  });

  const openAddProvider = () => {
    const requestId = ++providerCreationRequestRef.current;
    const requestedDir = activeConfigDirKeyRef.current;
    const contextGeneration = refreshRequestRef.current;
    // Read a fresh, shared configuration once for this creation flow. The UI's
    // last state may be stale or a legacy provider-only template.
    return call(
      () => invoke<string>("get_provider_config_base", { configDir: configDir || null }),
      (configText) => {
        if (requestId !== providerCreationRequestRef.current || requestedDir !== activeConfigDirKeyRef.current
          || contextGeneration !== refreshRequestRef.current) return;
        officialDraftRequestRef.current += 1;
        invalidateOfficialDraftSync();
        officialModelPristineRef.current = true;
        setProviderCreationBase(configText);
        setCreatingProvider(true);
        setSelectedPresetId("custom");
        setSelectedPresetVariantId("");
        setEditingOfficialProfileId(null);
        setOfficialAuthDirty(false);
        const officialDraft = { providerName: "", model: officialModelsRef.current.recommendedModel || INITIAL_OFFICIAL_MODEL, configText: "", authJson: "" };
        officialFormRef.current = officialDraft;
        setOfficialForm(officialDraft);
        const next = newCustomProviderForm(configText);
        resetAvailableProviderModels();
        setEditingProviderId(null);
        setEditingDetectedProvider(false);
        setProviderForm(next);
        setProviderTomlDraft(next.tomlConfig || buildProviderTomlPreview(next));
        setProviderTomlDirty(false);
        setProviderCommonConfigDirty(false);
        setProviderMode("form");
      },
    );
  };

  const applyProviderPreset = (presetId: string, variantId: string) => {
    const preset = getProviderPreset(presetId);
    if (!creatingProvider || !preset) return;
    const variant = getProviderPresetVariant(presetId, variantId);
    setSelectedPresetId(presetId);
    setSelectedPresetVariantId(variant?.id || "");
    resetAvailableProviderModels();
    setError("");
    if (presetId === "official") {
      void changeProviderKind("official");
      return;
    }
    officialDraftRequestRef.current += 1;
    providerDraftRequestRef.current += 1;
    clearActionBusy("loadOfficialDraft");
    const inherited = newCustomProviderForm();
    const draft = createPresetProvider(presetId, variantId, inherited.tomlConfig) || inherited;
    // Presets use the same full TOML base as Custom. The existing draft builder
    // updates provider fields while retaining common and extended settings.
    // Switching services still starts with an empty API key field.
    const next = {
      ...draft,
      id: draft.id ? uniqueId(draft.id, savedProviders.map((provider) => provider.id)) : "",
    };
    setProviderForm(next);
    setProviderTomlDraft(next.tomlConfig || buildProviderTomlPreview(next));
    setProviderTomlDirty(false);
    setProviderCommonConfigDirty(false);
    setProviderApiKeyVisible(false);
    setAvailableProviderModels(variant?.models.map((entry) => ({ id: entry.model })) || []);
    setProviderMode("form");
  };

  const openEditProvider = (provider: SavedProvider) => {
    providerCreationRequestRef.current += 1;
    setCreatingProvider(false);
    resetAvailableProviderModels();
    setEditingProviderId(provider.id);
    setEditingDetectedProvider(false);
    setProviderForm(provider);
    setProviderTomlDraft(provider.tomlConfig?.trim() || buildProviderTomlPreview(provider));
    setProviderTomlDirty(false);
    setProviderCommonConfigDirty(false);
    setProviderMode("form");
  };

  const duplicateProvider = (row: ProviderRow) => {
    const requestedDirKey = activeConfigDirKeyRef.current;
    return call(
      () => invoke<DuplicateProviderResult>("duplicate_provider", {
        configDir: configDir || null,
        providerId: findLocalProviderForRow(row)?.id || null,
        providerName: `${row.providerName}${lang === "zh" ? " 副本" : " Copy"}`,
      }),
      (result) => {
        if (requestedDirKey !== activeConfigDirKeyRef.current) return;
        commitSavedProviders(result.providers);
        if (result.activeProviderId) {
          setActiveProviderId(result.activeProviderId);
          localStorage.setItem(ACTIVE_PROVIDER_KEY, result.activeProviderId);
          setState((current) => current && !current.isOfficialProvider
            ? { ...current, activeSavedProviderId: result.activeProviderId || undefined } : current);
        }
        setToast(lang === "zh" ? `已复制为“${result.provider.providerName}”` : `Created “${result.provider.providerName}”`);
      },
    );
  };

  const openEditDetectedProvider = (provider: { id: string; providerName: string; baseUrl: string; model: string; apiKey?: string; wireApi: string; requiresOpenaiAuth: boolean }) => {
    providerCreationRequestRef.current += 1;
    setCreatingProvider(false);
    resetAvailableProviderModels();
    const id = uniqueId(
      customProviderId(provider.providerName || provider.baseUrl),
      savedProviders.map((item) => item.id),
    );
    setEditingProviderId(id);
    setEditingDetectedProvider(true);
    const next = {
      id,
      providerName: provider.providerName,
      baseUrl: provider.baseUrl,
      model: provider.model,
      apiKey: provider.apiKey || "",
      tomlConfig: state?.configText?.trim() || "",
      wireApi: "responses",
      upstreamApi: providerUpstreamApi(provider),
      requiresOpenaiAuth: provider.requiresOpenaiAuth,
    };
    setProviderForm(next);
    setProviderTomlDraft(next.tomlConfig || buildProviderTomlPreview(next));
    setProviderTomlDirty(false);
    setProviderCommonConfigDirty(false);
    setProviderMode("form");
  };

  const removeProvider = async (id: string, isCurrent: boolean) => {
    const loadingToken = beginLoading();
    setError("");
    try {
      if (isCurrent) {
        const result = await invoke<ActionResult>("switch_official_provider", { configDir: configDir || null });
        localStorage.removeItem(ACTIVE_PROVIDER_KEY);
        setActiveProviderId("");
        setState(result.state);
      }
      await invoke<void>("delete_saved_provider", { id, configDir: configDir || null });
      const providerList = await invoke<SavedProvider[]>("list_saved_providers");
      commitSavedProviders(providerList);
      setToast(lang === "zh" ? "供应商已删除" : "Provider deleted");
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    } finally {
      endLoading(loadingToken);
    }
  };

  const removeOfficialProfile = async (id: string) => {
    if (loadingTokensRef.current.size || actionBusyTokensRef.current.size) return false;
    const directory = state?.codexDir || configDir;
    const directoryKey = normalizedConfigDirForComparison(directory);
    const generation = refreshRequestRef.current;
    const isCurrentContext = () => generation === refreshRequestRef.current
      && activeConfigDirKeyRef.current === directoryKey;
    const loadingToken = beginLoading();
    setError("");
    try {
      const [freshProfiles, freshState] = await Promise.all([
        invoke<OfficialProfileSummary[]>("list_official_profiles", { configDir: directory || null }),
        invoke<CodexState>("get_codex_state", { configDir: directory || null }),
      ]);
      if (!isCurrentContext()) return false;
      officialProfilesRequestRef.current += 1;
      officialProfilesLiveRef.current = freshProfiles;
      setOfficialProfiles(freshProfiles);
      sessionStateRef.current = freshState;
      setState(freshState);
      if (!freshProfiles.some((profile) => profile.id === id)) {
        throw new Error(lang === "zh" ? "该官方账号已不存在，请刷新列表。" : "This official account no longer exists. Refresh the list.");
      }
      if (freshProfiles.length <= 1) {
        throw new Error(lang === "zh" ? "至少保留一个官方账号，无法删除最后一个账号。" : "Keep at least one official account. The last account cannot be deleted.");
      }
      const freshDefaultId = freshProfiles.find((profile) => profile.isDefault)?.id || freshProfiles[0].id;
      if (freshState.isOfficialProvider && (freshState.activeOfficialProfileId || freshDefaultId) === id) {
        const survivor = freshProfiles.find((profile) => profile.id !== id && profile.isDefault)
          || freshProfiles.find((profile) => profile.id !== id);
        if (!survivor) throw new Error(lang === "zh" ? "没有可切换的其他官方账号，已取消删除。" : "No other official account is available. Deletion cancelled.");
        const result = await invoke<ActionResult>("switch_official_profile", {
          configDir: directory || null, profileId: survivor.id,
        });
        if (!isCurrentContext()) return false;
        localStorage.removeItem(ACTIVE_PROVIDER_KEY);
        setActiveProviderId("");
        await handleActionResult(result);
      }
      if (!isCurrentContext()) return false;
      await invoke<void>("delete_official_profile", { configDir: directory || null, profileId: id });
      if (!isCurrentContext()) return true;
      const profilesRequestId = ++officialProfilesRequestRef.current;
      const [nextState, profiles] = await Promise.allSettled([
        invoke<CodexState>("get_codex_state", { configDir: directory || null }),
        invoke<OfficialProfileSummary[]>("list_official_profiles", { configDir: directory || null }),
      ]);
      if (!isCurrentContext()) return true;
      if (profilesRequestId === officialProfilesRequestRef.current && profiles.status === "fulfilled") {
        officialProfilesLiveRef.current = profiles.value;
        setOfficialProfiles(profiles.value);
      }
      if (nextState.status === "fulfilled") {
        sessionStateRef.current = nextState.value;
        setState(nextState.value);
        invalidateSessionStatus();
      }
      if (nextState.status === "rejected" || profiles.status === "rejected") {
        const cause = nextState.status === "rejected" ? nextState.reason
          : profiles.status === "rejected" ? profiles.reason : "";
        setError(`${lang === "zh" ? "账号已删除，但刷新状态失败，请重新检查。" : "Account deleted, but status refresh failed. Check again."} ${String(cause)}`);
        return true;
      }
      setToast(lang === "zh" ? "官方登录配置已删除" : "Official sign-in profile deleted");
      return true;
    } catch (deleteError) {
      if (isCurrentContext()) setError(String(deleteError));
      return false;
    } finally {
      endLoading(loadingToken);
    }
  };

  const checkSessions = async () => {
    if (!getSessionStatusContext() || loadingTokensRef.current.size || actionBusyTokensRef.current.size || sessionDeleteBusyRef.current) return;
    sessionStatusTrackerRef.current.invalidate();
    const actionToken = beginActionBusy("checkSessions");
    setSessionStatus(null);
    await call(
      loadCurrentSessionStatus,
      (status) => {
        if (!status) return;
        if (!status.scanComplete) {
          setToast(status.scanFailures[0] || (lang === "zh" ? "无法确认会话同步状态" : "Unable to verify session sync status"));
          return;
        }
        const hasMismatches = Boolean(status.needsSync);
        const syncCount = sessionMismatchCount(status);
        setToast(hasMismatches
          ? (lang === "zh" ? `有 ${syncCount} 条会话需要同步` : `${syncCount} session(s) need syncing`)
          : (lang === "zh" ? "普通会话已同步" : "User conversations are synced"));
      },
    );
    endActionBusy(actionToken);
  };

  React.useEffect(() => {
    if (tab !== "sessions" || refreshing || loading) return;
    const context = getSessionStatusContext();
    if (!context) return;
    const loadKey = sessionStatusContextKey(context);
    if (sessionAutoLoadKeyRef.current === loadKey) return;
    const actionToken = beginActionBusy("checkSessions");
    sessionAutoLoadKeyRef.current = loadKey;
    void loadCurrentSessionStatus()
      .catch((sessionError) => {
        sessionAutoLoadKeyRef.current = "";
        setError(String(sessionError));
      })
      .finally(() => {
        endActionBusy(actionToken);
      });
  }, [beginActionBusy, endActionBusy, getSessionStatusContext, loadCurrentSessionStatus, loading, refreshing, state?.codexDir, state?.modelProvider, state?.activeSavedProviderId, state?.activeOfficialProfileId, tab]);

  const refreshSyncedSessionStatus = async () => {
    if (sessionMutationInFlightRef.current || !getSessionStatusContext()) {
      sessionRefreshPendingRef.current = true;
      return null;
    }
    sessionRefreshPendingRef.current = false;
    const loadingToken = beginLoading("sessionSync");
    const actionToken = beginActionBusy("syncSessions");
    try {
      const status = await loadCurrentSessionStatus();
      if (status && sessionSyncToastPendingRef.current) {
        sessionSyncToastPendingRef.current = false;
        if (!status.scanComplete) {
          setToast(status.scanFailures[0] || (lang === "zh" ? "无法确认会话同步状态" : "Unable to verify session sync status"));
        } else if (status.needsSync) {
          const remaining = sessionMismatchCount(status);
          setToast(lang === "zh"
            ? `本轮同步已完成，当前供应商仍有 ${remaining} 条会话需要同步；聊天内容未改动`
            : `This sync finished; ${remaining} session(s) still need syncing for the current provider. Chat content was not changed.`);
        } else {
          setToast(lang === "zh"
            ? "会话索引已全部同步，聊天内容未改动"
            : "All session indexes are synced. Chat content was not changed.");
        }
      }
      return status;
    } catch (sessionError) {
      sessionSyncToastPendingRef.current = false;
      setError(String(sessionError));
      return null;
    } finally {
      endLoading(loadingToken);
      endActionBusy(actionToken);
    }
  };
  refreshSyncedSessionStatusRef.current = refreshSyncedSessionStatus;

  const syncSessions = async () => {
    const context = getSessionStatusContext();
    if (!context || loadingTokensRef.current.size || actionBusyTokensRef.current.size || sessionDeleteBusyRef.current) return;
    // Capture primitives at the click, before the IPC worker can be queued
    // behind a provider switch. The backend keeps this target for the whole job.
    const target = { ...context };
    sessionStatusTrackerRef.current.invalidate();
    const actionToken = beginActionBusy("syncSessions");
    const loadingToken = beginLoading("sessionSync");
    sessionMutationInFlightRef.current = true;
    sessionSyncToastPendingRef.current = false;
    setError("");
    try {
      await invoke<SessionSyncResult>("sync_sessions_provider", {
        configDir: target.configDir,
        targetProvider: target.targetProvider,
      });
      setSelectedSessionIds([]);
      sessionSyncToastPendingRef.current = true;
    } catch (syncError) {
      if (target.configDir === sessionStateRef.current?.codexDir) setError(String(syncError));
    } finally {
      sessionMutationInFlightRef.current = false;
      sessionRefreshPendingRef.current = true;
      // Never apply the old job's status to a newly active provider. Re-read
      // the current context, or let the in-flight activation do so on completion.
      await refreshSyncedSessionStatusRef.current();
      endLoading(loadingToken);
      endActionBusy(actionToken);
    }
  };

  const toggleSessionSelected = (id: string) => {
    setSelectedSessionIds((ids) => ids.includes(id) ? ids.filter((item) => item !== id) : [...ids, id]);
  };

  const setSessionGroupSelected = (sessions: SessionPreview[], checked: boolean) => {
    const groupIds = new Set(sessions.map((item) => item.id));
    setSelectedSessionIds((ids) => {
      const next = new Set(ids);
      if (checked) groupIds.forEach((id) => next.add(id));
      else groupIds.forEach((id) => next.delete(id));
      return Array.from(next);
    });
  };

  const closeSessionDeleteConfirm = () => {
    if (!sessionDeleteBusy) {
      setSessionDeleteConfirmOpen(false);
      setSessionDeleteSafetyConfirmed(false);
    }
  };

  const deleteSelectedSessions = async () => {
    if (!selectedSessionIds.length || sessionDeleteBusyRef.current || !sessionDeleteSafetyConfirmed
      || loadingTokensRef.current.size || actionBusyTokensRef.current.size) return;
    sessionDeleteBusyRef.current = true;
    const loadingToken = beginLoading();
    setSessionDeleteBusy(true);
    setToast("");
    setError("");
    try {
      const result = await invoke<SessionDeleteResult>("delete_codex_sessions", {
        input: {
          configDir: configDir || null,
          sessionIds: selectedSessionIds,
        },
      });
      setSessionStatus(result.status);
      const remainingIds = new Set(result.status.sessions.map((item) => item.id));
      setSelectedSessionIds((ids) => ids.filter((id) => remainingIds.has(id)));
      setSessionDeleteConfirmOpen(false);
      setSessionDeleteSafetyConfirmed(false);
      const hasPartialFailure = result.failedSessions > 0 || Boolean(result.failureMessage);
      if (hasPartialFailure) {
        setError(result.failureMessage || (lang === "zh"
          ? `${result.failedSessions} 个会话删除失败，请关闭其他 Codex 窗口或 CLI 后重试。`
          : `${result.failedSessions} session deletion(s) failed. Close other Codex windows or CLIs and retry.`));
      } else {
        setToast(lang === "zh"
          ? `已永久删除 ${result.deletedSessions} 条会话，并清理数据库、Rollout 与关联历史`
          : `Permanently deleted ${result.deletedSessions} session(s) and cleaned database, rollout, and related history data`);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      sessionDeleteBusyRef.current = false;
      setSessionDeleteBusy(false);
      endLoading(loadingToken);
    }
  };

  const closeStartupWizard = () => {
    if (startupCheckMode === "startup") localStorage.setItem(STARTUP_WIZARD_SEEN_KEY, "1");
    setStartupClosing(true);
    window.setTimeout(() => {
      setStartupWizardOpen(false);
      setStartupClosing(false);
    }, 260);
  };

  const changeTab = (nextTab: Tab) => {
    if (nextTab !== "instruction") invalidatePromptDetail();
    if (nextTab !== "provider") {
      officialDraftRequestRef.current += 1;
      if (actionBusy === "loadOfficialDraft") setProviderMode("list");
      clearActionBusy("loadOfficialDraft");
    }
    setTab(nextTab);
  };

  const openConfigurationChecks = () => {
    configHealth.dismiss();
    setStartupCheckMode("manual");
    setStartupClosing(false);
    setStartupWizardOpen(true);
    refresh(true);
  };

  return (
    <AppShell
      activeTab={tab}
      onTabChange={changeTab}
      lang={lang}
      theme={theme}
      onToggleTheme={toggleTheme}
      codexVersion={aboutInfo?.codexVersion
        || (aboutLoading ? (lang === "zh" ? "正在检测..." : "Detecting...") : undefined)}
      appVersion={aboutInfo?.appVersion}
      hasUpdate={Boolean(releaseInfo.hasUpdate)}
      updatePhase={updater.state.phase}
      onOpenUpdate={() => setUpdatePromptOpen(true)}
      isMacRuntime={isMacRuntime}
      contentClassName={cx(
        tab === "sessions" && "cx-app-content--sessions",
        (
          (tab === "provider" && providerMode === "list")
          || tab === "skillsMcp"
        ) && "cx-app-content--fixed",
        skillsMcpImportOpen && Boolean(skillsMcpImportPreview) && "cx-app-content--modal-locked",
      )}
    >
      <AppToast
        lang={lang}
        message={toast}
        error={error}
        loading={Boolean(providerTestingId || providerModelsLoading) && Boolean(toast)}
        persistent={Boolean(error) && error === restartCodexError}
        action={error && error === restartCodexError ? {
          label: lang === "zh" ? "重试" : "Retry",
          onClick: () => { void restartCodexDesktop(); },
          disabled: restartCodexBusy || loading || Boolean(actionBusy),
        } : !error && languageRestartNotice?.configDir === configDir && languageRestartNotice.message === toast ? {
          label: lang === "zh" ? "点我重启" : "Restart now",
          onClick: () => { void restartCodexDesktop(); },
          disabled: restartCodexBusy || loading || Boolean(actionBusy),
        } : undefined}
        onDismissMessage={() => { setToast(""); setLanguageRestartNotice(null); }}
        onDismissError={() => { setError(""); setRestartCodexError(""); }}
      />
      {configHealth.noticeVisible && configHealth.notice && <ConfigHealthToast
        lang={lang}
        report={configHealth.notice}
        repairing={configHealth.repairing}
        onRepair={() => void configHealth.repair()}
        onDismiss={() => configHealth.dismiss(true)}
        onOpenSettings={() => {
          configHealth.dismiss();
          setSettingsGeneralRequest((value) => value + 1);
          changeTab("settings");
          openConfigurationChecks();
        }}
      />}
      <UpdateDialog
        open={updatePromptOpen && Boolean(releaseInfo.hasUpdate)}
        lang={lang}
        state={releaseInfo.updateMethod === "native" ? updater.state : undefined}
        currentVersion={aboutInfo?.appVersion}
        latestVersion={releaseInfo.latestVersion}
        onClose={() => setUpdatePromptOpen(false)}
        onUpdate={releaseInfo.updateMethod === "native" ? updater.downloadAndInstall : undefined}
        onRetry={releaseInfo.updateMethod === "native" ? async () => {
          if (updater.state.failure === "check") await checkForUpdates();
          else await updater.retry();
        } : undefined}
        onRestart={releaseInfo.updateMethod === "native" ? updater.restart : undefined}
        onDownload={() => {
          setUpdatePromptOpen(false);
          openExternalUrl(releaseInfo.htmlUrl);
        }}
      />
      <StartupWizardDialog
        open={startupWizardOpen}
        mode={startupCheckMode}
        closing={startupClosing}
        lang={lang}
        diagnostics={startupDiagnostics}
        diagnosticsError={startupDiagnosticsError ? (lang === "zh" ? "暂时无法读取环境信息，请重新检查。配置检查仍可使用。" : "Environment information is unavailable. Try again; configuration checks remain available.") : ""}
        configDir={configDirDraft}
        loading={loading || refreshing || startupDiagnosticsLoading || configHealth.checking || configHealth.repairing}
        configHealthPanel={normalizedConfigDirForComparison(configDirDraft.trim()) !== normalizedConfigDirForComparison(healthConfigDir)
          ? <p className="cx-config-health-note" role="status">{lang === "zh" ? "目录已更改，请先点击「重新检查」查看此目录的配置。" : "The directory has changed. Choose Recheck to review its configuration."}</p>
          : <ConfigHealthPanel
          lang={lang}
          report={configHealth.report}
          checking={configHealth.checking || loading || refreshing || Boolean(actionBusy)}
          repairing={configHealth.repairing}
          error={configHealth.error}
          onCheck={() => void configHealth.check()}
          onRepair={() => void configHealth.repair()}
          onOpenConfig={() => void configHealth.openConfig()}
        />}
        onConfigDirChange={setConfigDirDraft}
        onRecheck={() => refresh(true)}
        onSkip={closeStartupWizard}
        onOpenSettings={() => {
          changeTab("settings");
          closeStartupWizard();
        }}
        onEnter={closeStartupWizard}
      />

      <PageTransition pageKey={tab}>
            {!state && tab !== "dashboard" && tab !== "settings" && tab !== "about" && (
              <CodexStateLoading lang={lang} loading={refreshing} />
            )}

            {tab === "dashboard" && (
              <OverviewPage
                lang={lang}
                ready={Boolean(state)}
                model={state?.model}
                configDir={configDirDraft}
                resolvedCodexDir={state?.codexDir || ""}
                configExists={Boolean(state?.configExists)}
                providerLabel={currentOfficialProfile?.providerName || currentProvider?.name || state?.modelProvider}
                instructionEnabled={Boolean(state?.instructionEnabled)}
                authExists={currentOfficialProfile?.isCurrent ? currentOfficialProfile.hasOwnedAuth : Boolean(state?.authExists)}
                officialAuthAvailable={currentOfficialProfile?.isCurrent ? currentOfficialProfile.hasAuth : Boolean(state?.officialAuthAvailable)}
                configPath={state?.configPath}
                modelProvider={state?.modelProvider}
                instructionPath={state
                  ? (state.instructionInjectionMode === "append"
                    ? `${state.agentsPath} (${lang === "zh" ? "追加模式" : "append"})`
                    : state.instructionFile)
                  : null}
                loading={loading || refreshing || actionBusy === "reorderProviders"}
                hasUpdate={Boolean(releaseInfo.status === "ok" && releaseInfo.hasUpdate)}
                latestVersion={releaseInfo.latestVersion}
                onConfigDirChange={setConfigDirDraft}
                onRefresh={() => refresh(false)}
                onOpenUpdate={() => setUpdatePromptOpen(true)}
              />
            )}

            {state && tab === "provider" && (
              <ProvidersPage
                lang={lang}
                copy={getProviderPageCopy(lang)}
                mode={providerMode}
                creatingProvider={creatingProvider}
                selectedPresetId={selectedPresetId}
                selectedPresetVariantId={selectedPresetVariantId}
                onPresetSelect={(id) => applyProviderPreset(id, "")}
                onPresetVariantSelect={(id) => applyProviderPreset(selectedPresetId, id)}
                officialProfileIsDefault={editingOfficialProfileId === defaultOfficialProfileId}
                canLoadCurrentOfficial={Boolean(state.isOfficialProvider)}
                providerRows={providerPageRows}
                providerBalances={providerBalances}
                configDir={configDir}
                loading={loading}
                testingId={providerTestingId}
                actionBusy={actionBusy}
                activationBusy={providerActivationBusy || refreshing || sessionDeleteBusy}
                orderBusy={providerOrder.directory !== state.codexDir}
                onReorderProviders={saveProviderOrder}
                editingProviderId={editingProviderId || (editingDetectedProvider ? providerForm.id : null)}
                providerForm={{
                  apiKey: providerForm.apiKey || "",
                  baseUrl: providerForm.baseUrl,
                  providerName: providerForm.providerName,
                  model: providerForm.model,
                  wireApi: "responses",
                  upstreamApi: providerUpstreamApi(providerForm),
                  requiresOpenaiAuth: providerForm.requiresOpenaiAuth,
                }}
                officialForm={officialForm}
                officialModels={officialModels}
                officialModelsLoading={officialModelsLoading}
                officialDraftSyncing={officialDraftSyncing}
                officialDraftSyncError={officialDraftSyncError}
                onRefreshOfficialModels={() => { void loadOfficialModels(true); }}
                officialAuthRef={officialAuthEditorRef}
                officialTomlRef={officialTomlEditorRef}
                officialInfo={{
                  officialUrl: "https://chatgpt.com/codex",
                  authPath: state.authPath,
                  current: state.isOfficialProvider ? currentOfficialProfile?.providerName || "OpenAI Official" : state.modelProvider,
                }}
                providerAuthPreview={<JsonPreview text={providerAuthPreview} />}
                providerModelMappings={providerForm.modelMappings || []}
                onProviderModelMappingsChange={(modelMappings) => setProviderForm((current) => ({ ...current, modelMappings }))}
                providerTomlDraft={providerTomlDraft}
                providerTomlRef={providerTomlEditorRef}
                apiKeyVisible={providerApiKeyVisible}
                availableModels={availableProviderModels.map((model) => model.id)}
                fetchingModels={providerModelsLoading}
                onImportCcSwitch={importFromCcSwitch}
                onAddProvider={openAddProvider}
                onLoadCcSwitchOfficial={() => void loadCcSwitchOfficial()}
                onEnableProvider={(row) => {
                  if (row.source === "official") {
                    switchOfficialProvider(row.id);
                    return;
                  }
                  const local = findLocalProviderForRow(row);
                  switchProvider(local || {
                    id: customProviderId(row.providerName),
                    providerName: row.providerName,
                    baseUrl: row.baseUrl,
                    model: row.model,
                    apiKey: row.apiKey || "",
                    tomlConfig: "",
                    wireApi: row.wireApi,
                    requiresOpenaiAuth: row.requiresOpenaiAuth,
                  });
                }}
                onTestProvider={(row) => {
                  const local = findLocalProviderForRow(row);
                  void testProvider(row.testingKey || `${row.source}-${row.id}`, row.baseUrl, local?.apiKey || row.apiKey || null, local);
                }}
                onEditProvider={(row) => {
                  if (row.source === "official") {
                    void openOfficialEdit(row.id);
                    return;
                  }
                  const local = findLocalProviderForRow(row);
                  if (local) openEditProvider(local);
                  else if (row.source === "detected") openEditDetectedProvider(row);
                }}
                onDuplicateProvider={(row) => {
                  if (row.source === "official") {
                    void duplicateOfficialProfile(row);
                    return;
                  }
                  void duplicateProvider(row);
                }}
                onDeleteProvider={(row) => {
                  if (row.source === "official") return removeOfficialProfile(row.id);
                  const local = findLocalProviderForRow(row);
                  return local ? removeProvider(local.id, row.isCurrent) : Promise.resolve(false);
                }}
                onResetOfficial={resetOfficialProvider}
                onCancelMode={() => {
                  officialDraftRequestRef.current += 1;
                  invalidateOfficialDraftSync();
                  setProviderMode("list");
                  setCreatingProvider(false);
                  setEditingDetectedProvider(false);
                  setProviderTomlDirty(false);
                  setProviderCommonConfigDirty(false);
                }}
                onOfficialModelChange={changeOfficialModel}
                onOfficialNameChange={(value) => {
                  const next = { ...officialFormRef.current, providerName: value };
                  officialFormRef.current = next;
                  setOfficialForm(next);
                }}
                onLoadCurrentOfficial={() => void loadCurrentOfficial()}
                onOfficialAuthChange={(value) => {
                  officialAuthDirtyRef.current = true;
                  setOfficialAuthDirty(true);
                  const next = { ...officialFormRef.current, authJson: value };
                  officialFormRef.current = next;
                  setOfficialForm(next);
                }}
                onOfficialConfigChange={changeOfficialConfig}
                onSaveOfficial={saveOfficialConfig}
                onApiKeyChange={(value) => {
                  resetAvailableProviderModels();
                  providerApiKeyEditedRef.current = true;
                  setProviderForm((current) => ({ ...current, apiKey: value }));
                }}
                onBaseUrlChange={(value) => {
                  resetAvailableProviderModels();
                  setProviderForm((current) => ({ ...current, baseUrl: value }));
                }}
                onProviderNameChange={(value) => setProviderForm((current) => ({
                  ...current,
                  providerName: value,
                  id: editingProviderId || uniqueId(customProviderId(value), savedProviders.map((provider) => provider.id)),
                }))}
                onProviderModelChange={(value) => setProviderForm((current) => ({ ...current, model: value }))}
                onFetchModels={() => void fetchProviderModels()}
                onUpstreamApiChange={(value) => {
                  resetAvailableProviderModels();
                  setProviderForm((current) => ({ ...current, upstreamApi: value, wireApi: "responses" }));
                }}
                onRequiresAuthChange={(value) => setProviderForm((current) => ({ ...current, requiresOpenaiAuth: value }))}
                onToggleApiKeyVisibility={() => setProviderApiKeyVisible((value) => !value)}
                onProviderTomlDraftChange={(value, origin = "manual") => {
                  providerDraftRequestRef.current += 1;
                  setProviderTomlDraft(value);
                  setProviderTomlDirty(true);
                  if (origin === "manual") {
                    providerApiKeyEditedRef.current = false;
                    setProviderCommonConfigDirty(true);
                  }
                }}
                onProviderHeadersConfigChange={(value) => {
                  providerDraftRequestRef.current += 1;
                  setProviderTomlDraft(value);
                  setProviderForm((current) => ({ ...current, tomlConfig: value }));
                }}
                onResetProviderToml={() => {
                  providerDraftRequestRef.current += 1;
                  setProviderTomlDraft(
                    providerForm.tomlConfig?.trim()
                    || state?.configText?.trim()
                    || providerTomlPreview,
                  );
                  setProviderTomlDirty(false);
                  setProviderCommonConfigDirty(false);
                  setProviderDraftRefreshToken((token) => token + 1);
                }}
                onSaveProvider={saveProviderConfig}
              />
            )}

            {state && (tab === "sessions" || visitedTabs.has("sessions")) && (
              <SessionManagementPage
                active={tab === "sessions"}
                lang={lang}
                sessionStatus={sessionStatus}
                sessionHasMismatches={sessionHasMismatches}
                sessionSyncCount={sessionSyncCount}
                sessionTargetLabel={sessionTargetLabel}
                sessionVisibleTotal={sessionVisibleTotal}
                sessionPreviewTruncated={sessionPreviewTruncated}
                visibleSessions={visibleSessions}
                filteredSessions={filteredSessions}
                allSessionsByCwd={allSessionsByCwd}
                groupedSessions={groupedSessions}
                selectedSessionIds={selectedSessionIds}
                selectedSessionSet={selectedSessionSet}
                selectedSessions={selectedSessions}
                sessionQuery={sessionQuery}
                sessionGroupByCwd={sessionGroupByCwd}
                showInternalSessions={showInternalSessions}
                loading={loading}
                actionBusy={actionBusy}
                sessionDeleteConfirmOpen={sessionDeleteConfirmOpen}
                sessionDeleteBusy={sessionDeleteBusy}
                sessionDeleteSafetyConfirmed={sessionDeleteSafetyConfirmed}
                sessionExportBusy={sessionExportBusy}
                onExportSessions={exportSessions}
                onCheckSessions={checkSessions}
                onSyncSessions={syncSessions}
                onSessionQueryChange={(value) => {
                  setSessionQuery(value);
                  setSelectedSessionIds([]);
                  setSessionDeleteConfirmOpen(false);
                }}
                onSessionGroupByCwdChange={setSessionGroupByCwd}
                onShowInternalSessionsChange={(checked) => {
                  setShowInternalSessions(checked);
                  setSelectedSessionIds([]);
                  setSessionDeleteConfirmOpen(false);
                }}
                onOpenDeleteConfirm={() => {
                  setSessionDeleteSafetyConfirmed(false);
                  setSessionDeleteConfirmOpen(true);
                }}
                onToggleSessionSelected={toggleSessionSelected}
                onSetSessionGroupSelected={setSessionGroupSelected}
                onCloseDeleteConfirm={closeSessionDeleteConfirm}
                onDeleteSelectedSessions={deleteSelectedSessions}
                onDeleteSafetyConfirmedChange={setSessionDeleteSafetyConfirmed}
              />
            )}

            {state && (tab === "skillsMcp" || visitedTabs.has("skillsMcp")) && (
              <SkillsMcpPage
                lang={lang}
                state={skillsMcpState}
                activeTab={skillsMcpTab}
                actionBusy={actionBusy}
                importOpen={skillsMcpImportOpen}
                importPreview={skillsMcpImportPreview}
                className={tab !== "skillsMcp" ? "page-pane-hidden" : undefined}
                onTabChange={setSkillsMcpTab}
                onLoad={loadSkillsMcp}
                onOpenImportPreview={openImportExistingSkillsMcpPreview}
                onCloseImportPreview={() => setSkillsMcpImportOpen(false)}
                onConfirmImport={importExistingSkillsMcp}
                onInstallZip={installSkillZipFile}
                onExport={exportSkillsMcp}
                onCheckUpdates={checkSkillUpdatesAction}
                onToggleSkill={toggleSkillEnabled}
                onToggleMcp={toggleMcpEnabled}
                noteBusyKey={skillsMcpNoteBusy}
                onSaveNote={saveSkillsMcpNote}
              />
            )}

            {state && tab === "instruction" && (
              <PromptsPage
                lang={lang}
                instructionMode={instructionMode}
                promptForm={promptForm}
                editingPromptId={editingPromptId}
                editingBuiltinPrompt={editingBuiltinPrompt}
                loading={loading || promptDetailLoading}
                actionBusy={actionBusy}
                promptSyncing={promptSyncing}
                promptCatalogReady={promptCatalogReady}
                promptImportRef={promptImportRef}
                promptInjectionMode={promptInjectionMode}
                promptModeHelpOpen={promptModeHelpOpen}
                promptModeHelpRef={promptModeHelpRef}
                instructionEnabled={state.instructionEnabled}
                activeInstructionTitle={activeInstructionTitle}
                activeInjectionMode={state.instructionInjectionMode}
                instructionTemplates={instructionTemplates}
                builtinPromptStatuses={builtinPromptStatus}
                activeBuiltinTemplateId={activeBuiltinTemplateId}
                orphanedBuiltinPrompt={missingActiveBuiltinTemplateId ? {
                  id: missingActiveBuiltinTemplateId,
                  title: activeInstructionTitle,
                  description: lang === "zh"
                    ? "该模板已从在线目录移除，当前配置仍在使用。"
                    : "This template was removed online but is still active.",
                } : null}
                savedPrompts={savedPrompts}
                managedSavedPromptId={state.instructionTemplateKey?.startsWith("saved:")
                  ? state.instructionTemplateKey.slice("saved:".length)
                  : null}
                preservedSavedPromptFilename={state.instructionInjectionMode === "append" ? currentInstructionFilename : null}
                externalPrompt={state.instructionFile
                  && currentInstructionId === "custom"
                  && !savedPrompts.some((prompt) => currentInstructionFilename === prompt.filename)
                  && !(missingActiveBuiltinTemplateId && state.instructionInjectionMode !== "append")
                  ? {
                    title: lang === "zh" ? "用户原有指令提示词" : "Existing user prompt",
                    description: state.instructionInjectionMode === "append"
                      ? (lang === "zh"
                        ? "追加模式已保留这份外部提示词，并同时加载 Codex-X 的 AGENTS.md 区块。"
                        : "Append mode preserves this external prompt alongside the Codex-X AGENTS.md block.")
                      : (lang === "zh"
                        ? "当前使用的是非 Codex-X 管理的外部提示词。"
                        : "This external prompt is not managed by Codex-X."),
                    filename: currentInstructionFilename,
                  }
                  : null}
                onSyncBuiltinPrompts={() => refreshBuiltinPrompts()}
                onImportPrompt={importPromptMd}
                onAddPrompt={openAddPrompt}
                onInstructionModeChange={(mode) => {
                  invalidatePromptDetail();
                  setInstructionMode(mode);
                }}
                onPromptInjectionModeChange={setPromptInjectionMode}
                onTogglePromptModeHelp={() => setPromptModeHelpOpen((open) => !open)}
                onEnableBuiltinPrompt={switchInstructionTemplate}
                onDisableInstruction={disableInstruction}
                onEnableSavedPrompt={enableSavedPrompt}
                onDisableExternalPrompt={disableExternalInstruction}
                onEditPrompt={openEditPrompt}
                onEditBuiltinPrompt={openEditBuiltinPrompt}
                onDeletePrompt={removeSavedPrompt}
                onPromptFormFieldChange={(field, value) => setPromptForm((current) => ({
                  ...current,
                  [field]: value,
                  ...(field === "title" ? { id: editingPromptId || providerId(value) } : {}),
                }))}
                onSavePrompt={savePromptOnly}
              />
            )}

            {state && tab === "toml" && (
              <TomlConfigPage
                eyebrow="~/.codex/config.toml"
                title={t.toml.title}
                description={t.toml.desc}
                loaded={state.configExists ? t.toml.loaded : t.dashboard.missing}
                isLoaded={state.configExists}
                preview={<TomlPreview text={state.configText || t.toml.missingText} />}
              />
            )}

            {tab === "about" && (
              <AboutPage
                copy={{
                  eyebrow: "About",
                  title: lang === "zh" ? "关于 Codex-X" : "About Codex-X",
                  appVersionLabel: `Codex-X ${lang === "zh" ? "版本" : "Version"}`,
                  codexVersionLabel: `Codex CLI ${lang === "zh" ? "版本" : "Version"}`,
                  codexHomeLabel: "CODEX_HOME",
                  projectLabel: lang === "zh" ? "项目地址" : "Project",
                  openProjectLabel: lang === "zh" ? "打开项目主页" : "Open project",
                  openIssuesLabel: lang === "zh" ? "反馈问题" : "Issues",
                  releasesEyebrow: "GitHub Releases",
                  releasesTitle: lang === "zh" ? "更新检查" : "Update check",
                  releaseStatusLabel: lang === "zh" ? "状态" : "Status",
                  latestVersionLabel: lang === "zh" ? "最新版本" : "Latest version",
                  checkUpdateLabel: lang === "zh" ? "检查更新" : "Check updates",
                  openReleasesLabel: lang === "zh" ? "打开下载页" : "Open releases",
                }}
                appVersion={aboutInfo?.appVersion || (aboutLoading ? (lang === "zh" ? "正在检测" : "Detecting") : "-")}
                codexVersion={aboutInfo?.codexVersion || (aboutLoading
                  ? (lang === "zh" ? "正在检测..." : "Detecting...")
                  : (lang === "zh" ? "未检测到" : "Not detected"))}
                codexHome={aboutInfo?.codexDir || state?.codexDir || configDir || "~/.codex"}
                projectUrl={aboutInfo?.projectUrl || `https://github.com/${FALLBACK_GITHUB_REPO}`}
                release={{
                  status: releaseStatusLabel,
                  latestVersion: releaseInfo.latestVersion || "-",
                  tone: releaseInfo.status === "error"
                    ? "error"
                    : releaseInfo.hasUpdate
                      ? "warning"
                      : releaseInfo.status === "ok"
                        ? "success"
                        : "neutral",
                  checking: releaseInfo.status === "checking"
                    || isAppUpdateBusy(updater.state.phase),
                  canOpenReleases: Boolean(releaseInfo.htmlUrl),
                }}
                onOpenProject={() => openExternalUrl(aboutInfo?.projectUrl || `https://github.com/${FALLBACK_GITHUB_REPO}`)}
                onOpenIssues={() => openExternalUrl(`${aboutInfo?.projectUrl || `https://github.com/${FALLBACK_GITHUB_REPO}`}/issues`)}
                onCheckUpdate={() => void checkForUpdates()}
                onOpenReleases={() => openExternalUrl(releaseInfo.htmlUrl)}
              />
            )}

            {tab === "settings" && (
              <SettingsPage
                lang={lang}
                configDir={configDir}
                onChange={async () => { routingRequestRefreshRef.current(); }}
                generalRequest={settingsGeneralRequest}
                interactionBusy={loading || refreshing || Boolean(actionBusy) || restartCodexBusy || isAppUpdateBusy(updater.state.phase)}
                onLanguageBusyChange={handleLanguageBusyChange}
                onCodexLanguageSaved={(status) => {
                  setError("");
                  if (status.pendingEnabled == null) {
                    setLanguageRestartNotice(null);
                    setToast(lang === "zh" ? "已取消待重启的语言更改" : "Pending language change cancelled");
                    return;
                  }
                  const message = lang === "zh" ? "语言设置已保存，重启 Codex / ChatGPT 后生效" : "Language setting saved. Restart Codex / ChatGPT to apply.";
                  setLanguageRestartNotice({ configDir, message });
                  setToast(message);
                }}
                configHealthStatus={<ConfigHealthStatus
                  lang={lang}
                  report={configHealth.report}
                  checking={configHealth.checking || loading || refreshing || Boolean(actionBusy)}
                  repairing={configHealth.repairing}
                  error={configHealth.error}
                />}
                copy={{
                  eyebrow: "Settings",
                  title: t.settings.title,
                  languageTitle: t.settings.language,
                  languageDescription: t.settings.languageDesc,
                  chineseLabel: t.settings.zh,
                  englishLabel: t.settings.en,
                  productTitle: t.settings.productName,
                  productDescription: t.settings.productDesc,
                  productValue: "Codex-X",
                  recheckTitle: lang === "zh" ? "环境与配置检查" : "Environment & configuration check",
                  recheckDescription: lang === "zh"
                    ? "查看 Codex 环境与配置状态，按需检查和修复。"
                    : "Review your Codex environment and configuration, and repair issues when needed.",
                  recheckLabel: lang === "zh" ? "检查" : "Check",
                  restartTitle: lang === "zh" ? "Codex 桌面客户端" : "Codex desktop app",
                  restartDescription: lang === "zh"
                    ? "重新启动本机的 Codex（ChatGPT）桌面客户端，不会重启 Codex-X。"
                    : "Restart the local Codex (ChatGPT) desktop app without restarting Codex-X.",
                  restartLabel: lang === "zh" ? "重启 Codex" : "Restart Codex",
                  restartTargetLabel: lang === "zh" ? "Codex（ChatGPT）桌面客户端" : "Codex (ChatGPT) desktop app",
                  restartConfirmTitle: lang === "zh" ? "重启 Codex？" : "Restart Codex?",
                  restartConfirmDescription: lang === "zh"
                    ? "正在运行的 Codex 窗口会关闭并重新打开，未保存的输入可能丢失。"
                    : "The running Codex window will close and reopen. Unsaved input may be lost.",
                  restartCancelLabel: lang === "zh" ? "取消" : "Cancel",
                  restartConfirmLabel: lang === "zh" ? "确认重启" : "Restart",
                  restartingLabel: lang === "zh" ? "正在重启" : "Restarting",
                }}
                onLanguageChange={setLang}
                recheckBusy={loading || refreshing}
                restartBusy={restartCodexBusy}
                onRestartCodex={restartCodexDesktop}
                onRecheck={openConfigurationChecks}
              />
            )}
      </PageTransition>
    </AppShell>
  );
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode><App /></React.StrictMode>,
);
