import React from "react";
import { flushSync } from "react-dom";
import ReactDOM from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import {
  SessionManagementPage,
  type SessionPreview,
  type SessionSyncStatus,
} from "./pages/SessionManagementPage";
import { OverviewPage } from "./pages/OverviewPage";
import { AboutPage, SettingsPage, TomlConfigPage } from "./pages/UtilityPages";
import { PromptsPage } from "./pages/PromptsPage";
import { SkillsMcpPage } from "./pages/SkillsMcpPage";
import { SkinsPage } from "./pages/SkinsPage";
import { ProvidersPage, type ProviderCopy, type ProviderRow } from "./pages/ProvidersPage";
import { AppShell, type AppTab, type AppTheme } from "./components/AppShell";
import { AppToast, StartupWizardDialog, UpdateDialog } from "./components/AppDialogs";
import { PageTransition } from "./components/PageTransition";
import { cx } from "./components/ui";
import { appUpdater, useAppUpdater } from "./appUpdater";
import type {
  AboutInfo,
  ActionResult,
  AppUpdateInfo,
  BuiltinPromptStatus,
  CodexState,
  ImportResult,
  InstructionMode,
  InstructionTemplate,
  Lang,
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
  SkinActionResult,
  SkinCenterState,
  SkinExportResult,
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

type Tab = AppTab;

const LANG_KEY = "codexx.lang";
const THEME_KEY = "codexx.theme";
const STARTUP_WIZARD_SEEN_KEY = "codexx.startupWizardSeen";
const ACTIVE_PROVIDER_KEY = "codexx.activeProviderId";
const PROMPT_INJECTION_MODE_KEY = "codexx.promptInjectionMode";
const FALLBACK_GITHUB_REPO = "yynxxxxx/Codex-X";

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
      skins: "皮肤中心",
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
      skins: "Skins",
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
    removeLabel: t.provider.remove,
    deleteTitle: isChinese ? "删除供应商" : "Delete provider",
    deleteDescription: (providerName) => isChinese
      ? `“${providerName}”将从供应商列表中删除，此操作无法撤销。`
      : `“${providerName}” will be removed from the provider list. This cannot be undone.`,
    deleteCurrentDescription: (providerName) => isChinese
      ? `“${providerName}”当前正在使用。删除后不会自动切换供应商，确定继续吗？`
      : `“${providerName}” is currently active. Deleting it will not switch providers automatically. Continue?`,
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
    officialSaveLabel: isChinese ? "保存官方配置" : "Save official config",
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
    chooseModelLabel: (count) => isChinese ? `选择已获取的模型（${count}）` : `Choose a fetched model (${count})`,
    wireApiLabel: t.provider.wireApi,
    requiresAuthLabel: t.provider.requiresAuth,
    authPreviewTitle: "auth.json (JSON)",
    authPreviewDescription: isChinese
      ? "预览保存时写入或保留的认证配置；API Key 留空不会覆盖现有认证。"
      : "Preview the authentication data. An empty API key keeps the current auth file.",
    tomlTitle: "config.toml (TOML)",
    tomlDescription: isChinese
      ? "这里保存供应商模板，只有启用供应商时才会写入 Codex 当前配置。"
      : "This stores the provider template and is written to the live config only when enabled.",
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

function normalizeProviderBaseUrl(value?: string | null) {
  const raw = (value || "").trim();
  if (!raw) return "";
  try {
    const parsed = new URL(raw);
    const credentials = parsed.username
      ? `${parsed.username}${parsed.password ? `:${parsed.password}` : ""}@`
      : "";
    const path = parsed.pathname.replace(/\/+$/, "");
    return `${parsed.protocol.toLowerCase()}//${credentials}${parsed.host.toLowerCase()}${path}${parsed.search}`;
  } catch {
    return raw.replace(/\/+$/, "");
  }
}

function normalizeProviderName(value?: string | null) {
  return (value || "").trim().replace(/\s+/gu, " ").toLowerCase();
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

function savedProviderApiKey(provider: SavedProvider) {
  return (provider.apiKey || "").trim() || extractTomlProviderApiKey(provider.tomlConfig);
}

function providerIdentityKey(baseUrl?: string | null, apiKey?: string | null, providerName?: string | null) {
  const normalizedUrl = normalizeProviderBaseUrl(baseUrl);
  if (!normalizedUrl) return "";
  const normalizedKey = (apiKey || "").trim();
  return JSON.stringify([
    normalizedUrl,
    normalizedKey ? `key:${normalizedKey}` : `name:${normalizeProviderName(providerName)}`,
  ]);
}

function buildProviderTomlPreview(provider: SavedProvider, state: CodexState | null) {
  const model = provider.model.trim() || "gpt-5.5";
  const name = provider.providerName.trim() || "your-provider";
  // Codex live config follows cc-switch: all third-party providers are applied as `custom`.
  const providerKey = "custom";
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
  const [skinCenterState, setSkinCenterState] = React.useState<SkinCenterState | null>(null);
  const [startupDiagnostics, setStartupDiagnostics] = React.useState<StartupDiagnostics | null>(null);
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
  const [loading, setLoading] = React.useState(false);
  const [toast, setToast] = React.useState<string>("");
  const [error, setError] = React.useState<string>("");
  const [providerForm, setProviderForm] = React.useState<SavedProvider>(defaultProviderForm);
  const [providerTomlDraft, setProviderTomlDraft] = React.useState("");
  const [providerTomlDirty, setProviderTomlDirty] = React.useState(false);
  const [providerApiKeyVisible, setProviderApiKeyVisible] = React.useState(false);
  const [providerTestingId, setProviderTestingId] = React.useState("");
  const [availableProviderModels, setAvailableProviderModels] = React.useState<ProviderModel[]>([]);
  const [providerModelsLoading, setProviderModelsLoading] = React.useState(false);
  const [actionBusy, setActionBusy] = React.useState<string>("");
  const [promptSyncing, setPromptSyncing] = React.useState(false);
  const [promptCatalogReady, setPromptCatalogReady] = React.useState(false);
  const [promptForm, setPromptForm] = React.useState<SavedPrompt>(blankPromptForm);
  const [officialForm, setOfficialForm] = React.useState({ model: "gpt-5.5", authJson: "" });
  const [promptModeHelpOpen, setPromptModeHelpOpen] = React.useState(false);
  const autoUpdateCheckedRef = React.useRef(false);
  const promptImportRef = React.useRef<HTMLInputElement | null>(null);
  const skillZipImportRef = React.useRef<HTMLInputElement | null>(null);
  const skinZipImportRef = React.useRef<HTMLInputElement | null>(null);
  const providerTomlEditorRef = React.useRef<HTMLTextAreaElement | null>(null);
  const providerModelsRequestRef = React.useRef(0);
  const promptModeHelpRef = React.useRef<HTMLDivElement | null>(null);
  const promptRefreshRequestRef = React.useRef(0);
  const promptRefreshInFlightRef = React.useRef<Promise<BuiltinPromptStatus[]> | null>(null);
  const promptAutoRefreshAttemptedRef = React.useRef(false);
  const promptCatalogReadyRef = React.useRef(false);
  const promptModeSyncedRef = React.useRef("");
  const skillsMcpLoadedRef = React.useRef(false);
  const skinCenterLoadedRef = React.useRef(false);
  const themeTransitionTimerRef = React.useRef<number | null>(null);
  const providerTomlPreview = React.useMemo(() => buildProviderTomlPreview(providerForm, state), [providerForm, state]);
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
  const currentInstructionId = instructionIdFromPath(state?.instructionFile, instructionTemplates);
  const releaseStatusLabel = React.useMemo(() => {
    if (updater.state.phase === "downloading") return lang === "zh" ? "下载中" : "Downloading";
    if (updater.state.phase === "installing") return lang === "zh" ? "安装中" : "Installing";
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
    const editor = providerTomlEditorRef.current;
    if (!editor) return;
    editor.style.height = "0px";
    editor.style.height = `${Math.max(560, editor.scrollHeight)}px`;
  }, [providerMode, providerTomlDraft]);

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

  React.useEffect(() => {
    if (providerMode === "form" && !providerTomlDirty) {
      setProviderTomlDraft(providerTomlPreview);
    }
  }, [providerMode, providerTomlDirty, providerTomlPreview]);

  const currentProvider = state?.providers.find((p) => p.isCurrent);
  const liveProviderId = (state?.modelProvider || "openai").trim();
  const liveCustomProvider = React.useMemo(() => (state?.providers || []).find((item) => item.id === "custom"), [state?.providers]);
  const liveProviderApiKey = React.useMemo(() => {
    const configKey = extractTomlProviderApiKey(state?.configText, liveProviderId);
    const authKey = extractOpenAiApiKey(state?.authText).trim();
    return configKey || authKey;
  }, [liveProviderId, state?.authText, state?.configText]);
  const inferredActiveProviderId = React.useMemo(() => {
    if (liveProviderId !== "custom") return "";
    const liveIdentity = providerIdentityKey(liveCustomProvider?.baseUrl, liveProviderApiKey, liveCustomProvider?.name || liveCustomProvider?.id);
    if (!liveIdentity) return "";
    const identityMatches = savedProviders.filter((item) =>
      providerIdentityKey(item.baseUrl, savedProviderApiKey(item), item.providerName) === liveIdentity,
    );
    const remembered = identityMatches.find((item) => item.id === activeProviderId);
    if (remembered) return remembered.id;
    const backendMatch = identityMatches.find((item) => item.id === state?.activeSavedProviderId);
    return backendMatch?.id || identityMatches[0]?.id || "";
  }, [activeProviderId, liveCustomProvider?.baseUrl, liveCustomProvider?.id, liveCustomProvider?.name, liveProviderApiKey, liveProviderId, savedProviders, state?.activeSavedProviderId]);
  const effectiveActiveProviderId = liveProviderId === "custom" ? inferredActiveProviderId : liveProviderId;
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
  const canonicalSavedProviders = React.useMemo(() => {
    const groups = new Map<string, SavedProvider[]>();
    savedProviders.forEach((provider) => {
      const identity = providerIdentityKey(provider.baseUrl, savedProviderApiKey(provider), provider.providerName);
      const key = identity || `id:${provider.id}`;
      const group = groups.get(key);
      if (group) group.push(provider);
      else groups.set(key, [provider]);
    });
    return Array.from(groups.values()).map((group) =>
      group.find((item) => item.id === effectiveActiveProviderId)
      || group.find((item) => item.id === activeProviderId)
      || group[0],
    );
  }, [activeProviderId, effectiveActiveProviderId, savedProviders]);

  const detectedRows = React.useMemo(() => {
    return (state?.providers || []).map((p) => {
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
  }, [liveProviderApiKey, state?.configText, state?.model, state?.providers]);

  const localRows = React.useMemo(() => {
    return canonicalSavedProviders.map((p) => ({
      ...p,
      source: "local" as const,
      isCurrent: effectiveActiveProviderId === p.id,
    }));
  }, [canonicalSavedProviders, effectiveActiveProviderId]);

  const providerRows = React.useMemo(() => {
    const officialRow = {
      id: "openai-official",
      source: "official" as const,
      providerName: "OpenAI Official",
      baseUrl: "https://chatgpt.com/codex",
      model: state?.model || "official",
      apiKey: "",
      wireApi: "official",
      requiresOpenaiAuth: false,
      isCurrent: !state?.modelProvider || state.modelProvider === "openai",
    };
    const seen = new Set<string>();
    const rows: Array<typeof officialRow | (typeof detectedRows)[number] | (typeof localRows)[number]> = [officialRow];
    localRows.forEach((row) => {
      const key = providerIdentityKey(row.baseUrl, savedProviderApiKey(row), row.providerName);
      if (key) seen.add(key);
      rows.push(row);
    });
    detectedRows.forEach((row) => {
      if (row.id === "detected-custom" && inferredActiveProviderId) return;
      const key = providerIdentityKey(row.baseUrl, row.apiKey, row.providerName);
      if (key && seen.has(key)) return;
      if (key) seen.add(key);
      rows.push(row);
    });
    return rows;
  }, [detectedRows, inferredActiveProviderId, localRows, state?.model, state?.modelProvider]);

  const findLocalProviderForRow = React.useCallback((row: ProviderRow) => {
    if (row.source === "official") return undefined;
    return canonicalSavedProviders.find((item) =>
      row.source === "local"
        ? item.id === row.id
        : providerIdentityKey(item.baseUrl, savedProviderApiKey(item), item.providerName)
          === providerIdentityKey(row.baseUrl, row.apiKey, row.providerName),
    );
  }, [canonicalSavedProviders]);

  const providerPageRows = React.useMemo<ProviderRow[]>(() => providerRows.map((row) => {
    const local = row.source === "official"
      ? undefined
      : canonicalSavedProviders.find((item) =>
        row.source === "local"
          ? item.id === row.id
          : providerIdentityKey(item.baseUrl, savedProviderApiKey(item), item.providerName)
            === providerIdentityKey(row.baseUrl, row.apiKey, row.providerName),
      );
    return {
      id: row.id,
      source: row.source,
      providerName: row.providerName,
      baseUrl: row.baseUrl,
      model: row.model,
      apiKey: row.apiKey,
      wireApi: row.wireApi,
      requiresOpenaiAuth: row.requiresOpenaiAuth,
      isCurrent: row.isCurrent,
      sourceLabel: row.source === "official" ? (lang === "zh" ? "Codex 登录" : "Codex login") : undefined,
      editable: row.source === "official" || Boolean(local) || row.source === "detected",
      deletable: Boolean(local),
      testable: row.source !== "official",
      testingKey: `${row.source}-${row.id}`,
    };
  }), [canonicalSavedProviders, lang, providerRows]);

  const visibleSessions = React.useMemo(
    () => (sessionStatus?.sessions || []).filter((item) => showInternalSessions || !item.isSubagent),
    [sessionStatus?.sessions, showInternalSessions],
  );

  const filteredSessions = React.useMemo(() => {
    const query = deferredSessionQuery.trim().toLowerCase();
    if (!query) return visibleSessions;
    return visibleSessions.filter((item) => [item.title, item.cwd, item.rolloutPath, item.modelProvider, item.model, item.id]
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

  const sessionRolloutMismatchCount = sessionStatus?.mismatchedRollouts ?? 0;
  const sessionIndexMismatchCount = sessionStatus?.mismatchedThreads ?? 0;
  const sessionHasMismatches = Boolean(sessionStatus?.needsSync);
  const sessionTargetProvider = sessionStatus?.targetProvider || state?.modelProvider || "openai";
  const sessionTargetLabel = canonicalSavedProviders.find((item) => item.id === effectiveActiveProviderId)?.providerName
    || currentProvider?.name
    || sessionTargetProvider;
  const previewSessionSyncCount = new Set(
    (sessionStatus?.sessions || []).filter((item) => item.needsSync).map((item) => item.id),
  ).size;
  const sessionSyncCount = sessionHasMismatches
    ? Math.max(1, previewSessionSyncCount, sessionRolloutMismatchCount, sessionIndexMismatchCount)
    : 0;
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
        const [next, providerList, promptList, promptStatus, about] = await Promise.all([
          invoke<CodexState>("get_codex_state", { configDir: configDir || null }),
          invoke<SavedProvider[]>("list_saved_providers"),
          invoke<SavedPrompt[]>("list_saved_prompts"),
          invoke<BuiltinPromptStatus[]>("get_builtin_prompt_status"),
          invoke<AboutInfo>("get_about_info", { configDir: configDir || null }),
        ]);
        return { next, providerList, promptList, promptStatus, about };
      },
      ({ next, providerList, promptList, promptStatus, about }) => {
        setState(next);
        setSavedProviders(providerList);
        setSavedPrompts(promptList);
        setBuiltinPromptStatus(uniqueBuiltinPromptStatuses(promptStatus));
        setAboutInfo(about);
        const resolvedConfigDir = configDir || null;
        void Promise.all([
          invoke<StartupDiagnostics>("get_startup_diagnostics", { configDir: resolvedConfigDir }),
          invoke<SessionSyncStatus>("get_session_sync_status", { configDir: resolvedConfigDir, targetProvider: null }),
        ])
          .then(([diagnostics, sessions]) => {
            setStartupDiagnostics(diagnostics);
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
    if (!state) return;
    if (liveProviderId !== "custom") {
      if (activeProviderId) {
        localStorage.removeItem(ACTIVE_PROVIDER_KEY);
        setActiveProviderId("");
      }
      return;
    }
    if (!savedProviders.length) return;
    if (inferredActiveProviderId && inferredActiveProviderId !== activeProviderId) {
      localStorage.setItem(ACTIVE_PROVIDER_KEY, inferredActiveProviderId);
      setActiveProviderId(inferredActiveProviderId);
      return;
    }
    if (activeProviderId && !savedProviders.some((item) => item.id === activeProviderId)) {
      localStorage.removeItem(ACTIVE_PROVIDER_KEY);
      setActiveProviderId("");
    }
  }, [activeProviderId, inferredActiveProviderId, liveProviderId, savedProviders, state]);

  const handleActionResult = (result: ActionResult) => {
    setState(result.state);
    setToast(result.message);
    const resolvedConfigDir = configDir || null;
    void Promise.all([
      invoke<SavedPrompt[]>("list_saved_prompts"),
      invoke<SavedProvider[]>("list_saved_providers"),
      invoke<SessionSyncStatus>("get_session_sync_status", { configDir: resolvedConfigDir, targetProvider: null }),
    ])
      .then(([promptList, providerList, sessions]) => {
        setSavedPrompts(promptList);
        setSavedProviders(providerList);
        setSessionStatus(sessions);
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

  const resetAvailableProviderModels = () => {
    providerModelsRequestRef.current += 1;
    setAvailableProviderModels([]);
    setProviderModelsLoading(false);
  };

  const fetchProviderModels = async () => {
    const baseUrl = providerForm.baseUrl.trim();
    const apiKey = (providerForm.apiKey || "").trim();
    if (!baseUrl || !apiKey) {
      setError("");
      setToast(lang === "zh" ? "请先填写 API 请求地址和 API Key" : "Enter the API URL and API key first");
      return;
    }

    const requestId = providerModelsRequestRef.current + 1;
    providerModelsRequestRef.current = requestId;
    setProviderModelsLoading(true);
    setError("");
    setToast(lang === "zh" ? "正在获取模型列表..." : "Fetching model list...");
    try {
      const result = await invoke<ProviderModelsResult>("fetch_provider_models", { baseUrl, apiKey });
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

  const testProvider = async (id: string, baseUrl: string, apiKey?: string | null) => {
    setProviderTestingId(id);
    setError("");
    setToast(lang === "zh" ? "正在检测连接..." : "Testing connection...");
    try {
      const result = await invoke<ProviderConnectionResult>("test_provider_connection", { baseUrl, apiKey: apiKey || null });
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

  const importFromCcSwitch = async () => {
    setActionBusy("importCcSwitch");
    try {
      await call(
        () => invoke<ImportResult>("import_ccswitch_codex_providers", { dbPath: null }),
        (result) => {
          setSavedProviders(result.providers);
          const warningText = result.skipped > 0 ? `，跳过 ${result.skipped}` : "";
          setToast(
            lang === "zh"
              ? `cc-switch 导入完成：新增 ${result.added}，更新 ${result.updated}，合并 ${result.merged}${warningText}`
              : `cc-switch import complete: ${result.added} added, ${result.updated} updated, ${result.merged} merged${warningText}`,
          );
        },
      );
    } finally {
      setActionBusy("");
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
    try {
      if (aboutInfo?.nativeUpdaterSupported !== false) {
        const updaterResult = await appUpdater.check({ force: !quiet, timeout: 15_000 });
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
          });
          if (!quiet) setToast(lang === "zh" ? "当前已是最新版本" : "You are up to date");
          return;
        }
      }

      // Keep the existing lightweight release check as a manual-download fallback for
      // bootstrap and portable builds that cannot use the native updater yet.
      const update = await invoke<AppUpdateInfo>("check_app_update");
      const message = update.hasUpdate
        ? (lang === "zh" ? "发现新版本" : "Update available")
        : (lang === "zh" ? "当前已是最新版本" : "You are up to date");
      setReleaseInfo({
        status: "ok",
        latestVersion: update.latestVersion,
        htmlUrl: update.htmlUrl,
        hasUpdate: update.hasUpdate,
        updateMethod: update.hasUpdate ? "download" : undefined,
      });
      if (update.hasUpdate) {
        if (quiet) {
          setToast(lang === "zh" ? `发现新版本 ${update.latestVersion}，可在概览页查看` : `New version ${update.latestVersion} is available`);
        } else {
          setUpdatePromptOpen(true);
        }
      } else if (!quiet) {
        setToast(message);
      }
    } catch {
      const message = quiet ? (lang === "zh" ? "自动检查失败" : "Auto check failed") : (lang === "zh" ? "检查失败" : "Check failed");
      setReleaseInfo({
        status: "error",
      });
      if (!quiet) setToast(message);
    }
  }, [aboutInfo?.appVersion, aboutInfo?.nativeUpdaterSupported, lang]);

  React.useEffect(() => {
    if (!state || autoUpdateCheckedRef.current) return;
    autoUpdateCheckedRef.current = true;
    void checkForUpdates({ quiet: true });
  }, [state, checkForUpdates]);

  React.useEffect(() => {
    if (!state || promptAutoRefreshAttemptedRef.current) return;
    promptAutoRefreshAttemptedRef.current = true;
    void refreshBuiltinPrompts({ quiet: true });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state]);

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

  const loadSkinCenter = React.useCallback(async ({ quiet = false }: { quiet?: boolean } = {}) => {
    if (!quiet) {
      setActionBusy("loadSkins");
      setError("");
    }
    try {
      const result = await invoke<SkinCenterState>("get_skin_center_state");
      setSkinCenterState(result);
    } catch (e) {
      if (!quiet) setError(String(e));
    } finally {
      if (!quiet) setActionBusy("");
    }
  }, []);

  React.useEffect(() => {
    if (tab !== "skins" || skinCenterLoadedRef.current) return;
    skinCenterLoadedRef.current = true;
    void loadSkinCenter();
  }, [tab, loadSkinCenter]);

  const importSkinThemeZip = async (file?: File | null) => {
    if (!file) return;
    if (!file.name.toLowerCase().endsWith(".zip")) {
      setError(lang === "zh" ? "请选择 .zip 主题包" : "Please choose a .zip theme pack");
      return;
    }
    if (file.size > 24 * 1024 * 1024) {
      setError(lang === "zh" ? "主题包不能超过 24MB" : "Theme ZIP must be smaller than 24MB");
      return;
    }
    setActionBusy("importSkinZip");
    setError("");
    try {
      const bytes = Array.from(new Uint8Array(await file.arrayBuffer()));
      const result = await invoke<SkinActionResult>("import_skin_theme_zip", { fileName: file.name, bytes });
      setSkinCenterState(result.state);
      setToast(result.message);
    } catch (e) {
      setError(String(e));
    } finally {
      setActionBusy("");
      if (skinZipImportRef.current) skinZipImportRef.current.value = "";
    }
  };

  const enableSkinTheme = async (id: string) => {
    setActionBusy(`skin:${id}`);
    setError("");
    try {
      const result = await invoke<SkinActionResult>("enable_skin_theme", { id });
      setSkinCenterState(result.state);
      setToast(result.message);
    } catch (e) {
      setError(String(e));
    } finally {
      setActionBusy("");
    }
  };

  const exportSkinTheme = async (id: string) => {
    setActionBusy(`skinExport:${id}`);
    setError("");
    try {
      const result = await invoke<SkinExportResult>("export_skin_theme", { id });
      setToast(result.message || (lang === "zh" ? `主题包已导出：${result.path}` : `Theme exported: ${result.path}`));
    } catch (e) {
      setError(String(e));
    } finally {
      setActionBusy("");
    }
  };

  const openImportExistingSkillsMcpPreview = async () => {
    setActionBusy("previewExistingSkillsMcp");
    setError("");
    try {
      const preview = await invoke<SkillsMcpImportPreview>("preview_existing_skills_mcp", { configDir: configDir || null });
      if (preview.skills.length + preview.mcpServers.length === 0) {
        setSkillsMcpImportPreview(null);
        setSkillsMcpImportOpen(false);
        setToast(lang === "zh" ? "没有需要新导入的 Skills 或 MCP" : "No new Skills or MCP items to import");
        return;
      }
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

  const officialAuthPlaceholder = '{\n  "OPENAI_API_KEY": null,\n  "auth_mode": "chatgpt",\n  "tokens": {\n    "access_token": "",\n    "refresh_token": "",\n    "id_token": ""\n  }\n}';

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
    resetAvailableProviderModels();
    setEditingProviderId(null);
    setProviderForm(next);
    setProviderTomlDraft(buildProviderTomlPreview(next, state));
    setProviderTomlDirty(false);
    setProviderMode("form");
  };

  const openEditProvider = (provider: SavedProvider) => {
    resetAvailableProviderModels();
    setEditingProviderId(provider.id);
    setProviderForm(provider);
    setProviderTomlDraft(provider.tomlConfig?.trim() || buildProviderTomlPreview(provider, state));
    setProviderTomlDirty(false);
    setProviderMode("form");
  };

  const openEditDetectedProvider = (provider: { id: string; providerName: string; baseUrl: string; model: string; apiKey?: string; wireApi: string; requiresOpenaiAuth: boolean }) => {
    resetAvailableProviderModels();
    setEditingProviderId(null);
    const next = {
      id: customProviderId(provider.providerName || provider.baseUrl),
      providerName: provider.providerName,
      baseUrl: provider.baseUrl,
      model: provider.model,
      apiKey: provider.apiKey || extractOpenAiApiKey(state?.authText),
      tomlConfig: "",
      wireApi: provider.wireApi || "responses",
      requiresOpenaiAuth: provider.requiresOpenaiAuth,
    };
    setProviderForm(next);
    setProviderTomlDraft(buildProviderTomlPreview(next, state));
    setProviderTomlDirty(false);
    setProviderMode("form");
  };

  const removeProvider = async (id: string) => {
    setLoading(true);
    setError("");
    try {
      await invoke<void>("delete_saved_provider", { id });
      const providerList = await invoke<SavedProvider[]>("list_saved_providers");
      setSavedProviders(providerList);
      setToast(lang === "zh" ? "供应商已删除" : "Provider deleted");
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    } finally {
      setLoading(false);
    }
  };

  const checkSessions = async () => {
    setActionBusy("checkSessions");
    await call(
      () => invoke<SessionSyncStatus>("get_session_sync_status", { configDir: configDir || null, targetProvider: null }),
      (status) => {
        setSessionStatus(status);
        const hasMismatches = Boolean(status.needsSync);
        const previewCount = new Set(status.sessions.filter((item) => item.needsSync).map((item) => item.id)).size;
        const syncCount = hasMismatches
          ? Math.max(1, previewCount, status.mismatchedRollouts, status.mismatchedThreads)
          : 0;
        setToast(hasMismatches
          ? (lang === "zh" ? `有 ${syncCount} 条会话需要同步` : `${syncCount} session(s) need syncing`)
          : (lang === "zh" ? "全部会话已同步" : "All sessions are synced"));
      },
    );
    setActionBusy("");
  };

  const syncSessions = async () => {
    const pendingCount = sessionSyncCount;
    setActionBusy("syncSessions");
    await call(
      () => invoke<SessionSyncResult>("sync_sessions_provider", { configDir: configDir || null, targetProvider: null }),
      (result) => {
        setSessionStatus(result.status);
        setSelectedSessionIds([]);
        const syncedCount = pendingCount || Math.max(result.updatedRollouts, result.updatedThreads);
        setToast(lang === "zh"
          ? `已同步 ${syncedCount} 条会话，聊天内容未改动`
          : `Synced ${syncedCount} session(s). Chat content was not changed.`);
      },
    );
    setActionBusy("");
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
    if (!selectedSessionIds.length || sessionDeleteBusy || !sessionDeleteSafetyConfirmed) return;
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
      setSessionDeleteBusy(false);
    }
  };

  const closeStartupWizard = () => {
    localStorage.setItem(STARTUP_WIZARD_SEEN_KEY, "1");
    setStartupClosing(true);
    window.setTimeout(() => {
      setStartupWizardOpen(false);
      setStartupClosing(false);
    }, 260);
  };

  return (
    <AppShell
      activeTab={tab}
      onTabChange={(nextTab) => {
        if (!state && nextTab !== "dashboard" && nextTab !== "settings" && nextTab !== "about") {
          setTab("dashboard");
          return;
        }
        setTab(nextTab);
      }}
      lang={lang}
      theme={theme}
      onToggleTheme={toggleTheme}
      codexVersion={aboutInfo?.codexVersion}
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
          || tab === "skins"
          || (tab === "instruction" && instructionMode === "list")
        ) && "cx-app-content--fixed",
        skillsMcpImportOpen && Boolean(skillsMcpImportPreview) && "cx-app-content--modal-locked",
      )}
    >
      <AppToast
        lang={lang}
        message={toast}
        error={error}
        loading={Boolean(providerTestingId || providerModelsLoading) && Boolean(toast)}
        onDismissMessage={() => setToast("")}
        onDismissError={() => setError("")}
      />
      <UpdateDialog
        open={updatePromptOpen && Boolean(releaseInfo.hasUpdate)}
        lang={lang}
        state={releaseInfo.updateMethod === "native" ? updater.state : undefined}
        currentVersion={aboutInfo?.appVersion}
        latestVersion={releaseInfo.latestVersion}
        onClose={() => setUpdatePromptOpen(false)}
        onUpdate={releaseInfo.updateMethod === "native" ? updater.downloadAndInstall : undefined}
        onRetry={releaseInfo.updateMethod === "native" ? updater.retry : undefined}
        onRestart={releaseInfo.updateMethod === "native" ? updater.restart : undefined}
        onDownload={() => {
          setUpdatePromptOpen(false);
          openExternalUrl(releaseInfo.htmlUrl);
        }}
      />
      <StartupWizardDialog
        open={startupWizardOpen}
        closing={startupClosing}
        lang={lang}
        diagnostics={startupDiagnostics}
        configDir={configDir}
        loading={loading}
        onConfigDirChange={setConfigDir}
        onRecheck={refresh}
        onSkip={closeStartupWizard}
        onOpenSettings={() => {
          setTab("settings");
          closeStartupWizard();
        }}
        onEnter={closeStartupWizard}
      />

      <PageTransition pageKey={tab}>
            {tab === "dashboard" && (
              <OverviewPage
                lang={lang}
                model={state?.model}
                configDir={configDir}
                resolvedCodexDir={state?.codexDir || ""}
                configExists={Boolean(state?.configExists)}
                providerLabel={currentProvider?.name || state?.modelProvider}
                instructionEnabled={Boolean(state?.instructionEnabled)}
                authExists={Boolean(state?.authExists)}
                configPath={state?.configPath}
                modelProvider={state?.modelProvider}
                instructionPath={state
                  ? (state.instructionInjectionMode === "append"
                    ? `${state.agentsPath} (${lang === "zh" ? "追加模式" : "append"})`
                    : state.instructionFile)
                  : null}
                loading={loading}
                hasUpdate={Boolean(releaseInfo.status === "ok" && releaseInfo.hasUpdate)}
                latestVersion={releaseInfo.latestVersion}
                onConfigDirChange={setConfigDir}
                onRefresh={refresh}
                onOpenUpdate={() => setUpdatePromptOpen(true)}
              />
            )}

            {state && tab === "provider" && (
              <ProvidersPage
                lang={lang}
                copy={getProviderPageCopy(lang)}
                mode={providerMode}
                providerRows={providerPageRows}
                loading={loading}
                testingId={providerTestingId}
                actionBusy={actionBusy}
                editingProviderId={editingProviderId}
                providerForm={{
                  apiKey: providerForm.apiKey || "",
                  baseUrl: providerForm.baseUrl,
                  providerName: providerForm.providerName,
                  model: providerForm.model,
                  wireApi: providerForm.wireApi,
                  requiresOpenaiAuth: providerForm.requiresOpenaiAuth,
                }}
                officialForm={officialForm}
                officialInfo={{
                  officialUrl: "https://chatgpt.com/codex",
                  authPath: state.authPath,
                  current: (!state.modelProvider || state.modelProvider === "openai") ? "OpenAI Official" : state.modelProvider,
                }}
                providerAuthPreview={<JsonPreview text={providerAuthPreview} />}
                providerTomlDraft={providerTomlDraft}
                providerTomlRef={providerTomlEditorRef}
                apiKeyVisible={providerApiKeyVisible}
                availableModels={availableProviderModels.map((model) => model.id)}
                fetchingModels={providerModelsLoading}
                onImportCcSwitch={importFromCcSwitch}
                onAddProvider={openAddProvider}
                onEnableProvider={(row) => {
                  if (row.source === "official") {
                    switchOfficialProvider();
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
                  void testProvider(row.testingKey || `${row.source}-${row.id}`, row.baseUrl, local?.apiKey || row.apiKey || null);
                }}
                onEditProvider={(row) => {
                  if (row.source === "official") {
                    openOfficialEdit();
                    return;
                  }
                  const local = findLocalProviderForRow(row);
                  if (local) openEditProvider(local);
                  else if (row.source === "detected") openEditDetectedProvider(row);
                }}
                onDeleteProvider={(row) => {
                  const local = findLocalProviderForRow(row);
                  return local ? removeProvider(local.id) : Promise.resolve(false);
                }}
                onCancelMode={() => setProviderMode("list")}
                onOfficialModelChange={(value) => setOfficialForm((current) => ({ ...current, model: value }))}
                onOfficialAuthChange={(value) => setOfficialForm((current) => ({ ...current, authJson: value }))}
                onSaveOfficial={saveOfficialConfig}
                onApiKeyChange={(value) => {
                  resetAvailableProviderModels();
                  setProviderForm((current) => ({ ...current, apiKey: value }));
                }}
                onBaseUrlChange={(value) => {
                  resetAvailableProviderModels();
                  setProviderForm((current) => ({ ...current, baseUrl: value }));
                }}
                onProviderNameChange={(value) => setProviderForm((current) => ({
                  ...current,
                  providerName: value,
                  id: editingProviderId || customProviderId(value),
                }))}
                onProviderModelChange={(value) => setProviderForm((current) => ({ ...current, model: value }))}
                onFetchModels={() => void fetchProviderModels()}
                onWireApiChange={(value) => setProviderForm((current) => ({ ...current, wireApi: value }))}
                onRequiresAuthChange={(value) => setProviderForm((current) => ({ ...current, requiresOpenaiAuth: value }))}
                onToggleApiKeyVisibility={() => setProviderApiKeyVisible((value) => !value)}
                onProviderTomlDraftChange={(value) => {
                  setProviderTomlDraft(value);
                  setProviderTomlDirty(true);
                }}
                onResetProviderToml={() => {
                  setProviderTomlDraft(providerTomlPreview);
                  setProviderTomlDirty(false);
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
                zipInputRef={skillZipImportRef}
                className={tab !== "skillsMcp" ? "page-pane-hidden" : undefined}
                onTabChange={setSkillsMcpTab}
                onLoad={loadSkillsMcp}
                onOpenImportPreview={openImportExistingSkillsMcpPreview}
                onCloseImportPreview={() => setSkillsMcpImportOpen(false)}
                onConfirmImport={importExistingSkillsMcp}
                onInstallZip={installSkillZipFile}
                onCheckUpdates={checkSkillUpdatesAction}
                onToggleSkill={toggleSkillEnabled}
                onToggleMcp={toggleMcpEnabled}
              />
            )}

            {state && (tab === "skins" || visitedTabs.has("skins")) && (
              <div className={tab !== "skins" ? "page-pane-hidden" : undefined}>
                <SkinsPage
                  lang={lang}
                  state={skinCenterState}
                  actionBusy={actionBusy}
                  zipInputRef={skinZipImportRef}
                  onLoad={loadSkinCenter}
                  onImportZip={importSkinThemeZip}
                  onEnableTheme={enableSkinTheme}
                  onExportTheme={exportSkinTheme}
                />
              </div>
            )}

            {state && tab === "instruction" && (
              <PromptsPage
                lang={lang}
                instructionMode={instructionMode}
                promptForm={promptForm}
                editingPromptId={editingPromptId}
                loading={loading}
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
                onInstructionModeChange={setInstructionMode}
                onPromptInjectionModeChange={setPromptInjectionMode}
                onTogglePromptModeHelp={() => setPromptModeHelpOpen((open) => !open)}
                onEnableBuiltinPrompt={switchInstructionTemplate}
                onDisableInstruction={disableInstruction}
                onEnableSavedPrompt={enableSavedPrompt}
                onDisableExternalPrompt={disableExternalInstruction}
                onEditPrompt={openEditPrompt}
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
                appVersion={aboutInfo?.appVersion || "-"}
                codexVersion={aboutInfo?.codexVersion || (lang === "zh" ? "未检测到" : "Not detected")}
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
                    || updater.state.phase === "downloading"
                    || updater.state.phase === "installing",
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
                  recheckTitle: lang === "zh" ? "首次启动向导" : "First-run wizard",
                  recheckDescription: lang === "zh"
                    ? "重新检测 CODEX_HOME、config.toml、auth.json 和 SQLite 会话库。"
                    : "Recheck CODEX_HOME, config.toml, auth.json and SQLite session stores.",
                  recheckLabel: lang === "zh" ? "重新检测" : "Recheck",
                }}
                onLanguageChange={setLang}
                recheckBusy={loading}
                onRecheck={() => {
                  localStorage.removeItem(STARTUP_WIZARD_SEEN_KEY);
                  setStartupWizardOpen(true);
                  refresh();
                }}
              />
            )}
      </PageTransition>
    </AppShell>
  );
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode><App /></React.StrictMode>,
);
