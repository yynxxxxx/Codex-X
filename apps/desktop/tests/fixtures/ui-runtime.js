(() => {
  "use strict";

  // Browser-only fixture. Every command below changes this in-memory model only.
  const clone = (value) => structuredClone(value);
  const codexDir = "/fixture/codex";
  let defaultOfficialId = "openai-official";
  const storageKey = "codexx.fixture.backend.v2";
  const counts = {};
  const switches = [];
  const commands = [];
  const pending = { sync: [], detail: [], quota: [], resetCredits: [], usage: [], providerOrder: [] };
  const providerOrders = new Map();
  const callbacks = new Map();
  const eventListeners = new Map();
  let nextCallback = 1;
  let output;
  let loginButton;
  let nextOfficialId = 1;
  let externalOfficialLogins = 0;
  let savedPrompts = [];
  let usageMode = "sample";
  let pauseUsage = false;
  let quotaMode = "dual";
  let resetCreditsMode = "available";
  let configHealthMode = new URLSearchParams(location.search).get("health") || "healthy";
  let configStateError = new URLSearchParams(location.search).has("stateError");
  let transferMode = "success";
  let lastTransfer = null;
  let existingImportAvailable = true;
  // Independent of the persisted provider fixture: language controls only model
  // the current page's memory, never a real Codex process or configuration.
  let codexLanguageEnabled = true;
  let codexLanguagePendingEnabled = null;
  let codexLanguageMode = "normal";

  function codexLanguageStatus() {
    return {
      locale: codexLanguageEnabled ? "zh-CN" : "auto",
      enabled: codexLanguageEnabled,
      pendingEnabled: codexLanguagePendingEnabled,
      supported: true,
      appName: "ChatGPT",
      appVersion: "fixture",
      phase: codexLanguageEnabled ? "active" : "inactive",
      message: "",
      readyPages: 0,
      restoreAvailable: false,
    };
  }

  function setCodexLanguageActual(enabled) {
    if (typeof enabled !== "boolean") throw new Error("Unknown fixture language state");
    codexLanguageEnabled = enabled;
    codexLanguagePendingEnabled = null;
    render();
  }

  function setCodexLanguageMode(mode) {
    if (!["normal", "save-error", "apply-error"].includes(mode)) throw new Error("Unknown fixture language mode");
    codexLanguageMode = mode;
    render();
  }
  const defaultRoutingSettings = () => ({ version: 2, routerEnabled: false, takeoverEnabled: false, autoFailoverEnabled: false,
    listenAddress: "127.0.0.1", listenPort: 15721, providerIds: [], maxRetries: 3, streamingFirstByteTimeout: 60,
    streamingIdleTimeout: 120, nonStreamingTimeout: 600, circuitFailureThreshold: 4, circuitSuccessThreshold: 2,
    circuitTimeoutSeconds: 60, circuitErrorRateThreshold: 0.6, circuitMinRequests: 10 });
  let failoverSettings = defaultRoutingSettings();
  const resetRoutingHealth = new Set();
  let failoverMode = "normal";
  let providerBaseReadFailure = false;
  let providerOrderMode = "normal";
  const updaterEnabled = new URLSearchParams(location.search).has("updater");
  let updateRequest = null;
  function updateStage(event) { updateRequest?.onEvent.onmessage(event); }
  function finishUpdate(failure = false) {
    if (!updateRequest) return;
    const request = updateRequest;
    updateRequest = null;
    if (failure) request.reject({ stage: "prepare", message: "Fixture：无法恢复连接配置，尚未启动安装程序。", logPath: "C:\\Users\\Fixture\\AppData\\Local\\Codex-X\\logs\\update.log" });
    else request.resolve({ restartRequired: true });
    render();
  }
  const sharedProviderTables = '\n[mcp_servers.fixture_docs]\ncommand = "fixture-docs-server"\n'
    + '\n[mcp_servers.fixture_docs.env]\nFIXTURE_TIMEOUT = "1000"\n'
    + '\n[desktop]\nsansFontSize = 14\ncodeFontSize = 13\n'
    + '\n[marketplaces.fixture]\nsource_type = "local"\nsource = "/fixture/marketplace"\n'
    + '\n[plugins."browser@fixture"]\nenabled = true\n'
    + '\n[projects."/fixture/project"]\ntrust_level = "trusted"\n';
  const fixtureSkill = (id, name, enabled = true) => ({ id, name, description: "Fixture skill", note: "合成测试备注", directory: id, enabled, source: "Codex", path: `${codexDir}/skills/${id}`, contentHash: null, updateStatus: "未检查" });
  const fixtureMcp = (id, name, enabled = true) => ({ id, name, enabled, transport: "stdio", source: "Codex", note: "合成测试备注", summary: "npx fixture-tools", command: "npx", url: null, configJson: { command: "npx", args: ["fixture-tools"] } });
  const transferState = { codexDir, codexSkillsDir: `${codexDir}/skills`, disabledSkillsDir: `${codexDir}/disabled-skills`, skills: [fixtureSkill("writing", "写作助手")], mcpServers: [fixtureMcp("docs", "项目文档")], warnings: [] };
  const fixtureSessions = ["项目排错记录", "功能设计讨论"].map((title, index) => ({ id: `fixture-session-${index + 1}`, title, firstUserMessage: index === 0 ? "帮我检查项目启动时的报错。" : null, modelProvider: "custom", model: "fixture-model", cwd: "/fixture/project", rolloutPath: `${codexDir}/sessions/fixture-${index}.jsonl`, updatedAtMs: Date.now() - index * 60000, archived: false, hasUserEvent: true, isSubagent: false, needsSync: false }));
  fixtureSessions.push({ id: "fixture-internal-guardian", title: "The following is the Codex agent history whose request action you are assessing...", modelProvider: "openai", model: "codex-auto-review", cwd: "/fixture/project", rolloutPath: `${codexDir}/sessions/guardian.jsonl`, updatedAtMs: Date.now() - 120000, archived: false, hasUserEvent: false, isSubagent: true, needsSync: false });

  function healthReport() {
    const broken = configHealthMode !== "healthy";
    const manual = configHealthMode === "syntax";
    return {
      codexDir, fingerprint: `fixture-health-${configHealthMode}`,
      status: broken ? "issues" : "healthy", checkedAt: new Date().toISOString(),
      canRepair: broken && !manual,
      issues: !broken ? [] : [{ code: manual ? "syntax" : "missing-provider", title: manual ? "配置文件格式不正确" : "旧会话的供应商配置缺失",
        description: manual ? "配置文件第 4 行附近的格式不正确，需要手动检查。" : "部分旧会话仍在使用 my_codex，但配置中已找不到这条供应商。", repairable: !manual }],
      repairSummary: broken && !manual ? ["为 my_codex 补齐配置，继续使用当前供应商 Fixture Saved Provider。", "修复前自动备份现有配置。"] : [],
    };
  }

  function officialToml(model) {
    return `model_provider = "openai"\nmodel = ${JSON.stringify(model || "fixture-official-model")}\n`;
  }

  function officialModelDraft(configText, replacement) {
    // Limited to this fixture's simple drafts; production uses Rust toml_edit.
    if (typeof configText !== "string") throw new Error("Fixture: invalid TOML draft");
    const lines = configText.split("\n");
    const table = lines.findIndex(line => /^\s*\[/.test(line));
    const root = lines.slice(0, table < 0 ? lines.length : table);
    if (root.some(line => line.includes('"""') || line.includes("'''"))) throw new Error("Fixture: multiline draft requires the native parser");
    const index = root.findIndex(line => /^\s*model\s*=/.test(line));
    const match = index < 0 ? null : /^(\s*model\s*=\s*)("(?:\\.|[^"\\])*"|'[^']*')(\s*(?:#.*)?)$/.exec(lines[index]);
    if (index >= 0 && !match) throw new Error("Fixture: model must be a TOML string");
    const model = match ? (match[2].startsWith('"') ? JSON.parse(match[2]) : match[2].slice(1, -1)) : null;
    if (replacement === undefined) return { model, configText };
    const next = replacement.trim();
    if (next.length > 256 || /[\s\u0000-\u001f\u007f]/.test(next)) throw new Error("Fixture: invalid model ID");
    if (index >= 0) {
      if (next) lines[index] = match[1] + JSON.stringify(next) + match[3];
      else lines.splice(index, 1);
    } else if (next) lines.unshift(`model = ${JSON.stringify(next)}`);
    return { model: next || null, configText: lines.join("\n") };
  }

  function officialAuth(account) {
    return JSON.stringify({
      auth_mode: "chatgpt",
      email: `${account}@example.test`,
      plan_type: account === "one" ? "pro" : "pro-lite",
      OPENAI_API_KEY: null,
      tokens: {
        id_token: `fixture-only-id-token-${account}`,
        access_token: `fixture-only-access-token-${account}`,
        refresh_token: `fixture-only-refresh-token-${account}`,
        account_id: `fixture-account-${account}`,
      },
      last_refresh: "2026-09-08T00:00:00Z",
    }, null, 2);
  }

  const officialProfiles = new Map([[defaultOfficialId, {
    id: defaultOfficialId,
    providerName: "OpenAI Official",
    model: "fixture-official-model",
    authJson: officialAuth("one"),
    configText: officialToml("fixture-official-model") + sharedProviderTables,
    isDefault: true,
  }]]);

  function toml(provider) {
    return [
      `model = ${JSON.stringify(provider.model)}`,
      `model_provider = ${JSON.stringify(provider.id)}`,
      "",
      `[model_providers.${provider.id}]`,
      `name = ${JSON.stringify(provider.providerName)}`,
      `base_url = ${JSON.stringify(provider.baseUrl)}`,
      `wire_api = ${JSON.stringify(provider.wireApi)}`,
      `experimental_bearer_token = ${JSON.stringify(provider.apiKey || "")}`,
      "requires_openai_auth = false",
      "",
    ].join("\n");
  }

  function providerTomlDraft(provider) {
    const original = provider.tomlConfig?.trim();
    if (!original) return toml(provider);
    // Preserve the complete simple fixture draft, including 1M and MCP settings,
    // while reflecting form fields in its active provider section.
    const originalId = original.match(/^model_provider\s*=\s*"([^"]+)"/m)?.[1];
    let section = "";
    let foundSection = false;
    const values = {
      name: provider.providerName, base_url: provider.baseUrl,
      wire_api: provider.wireApi, experimental_bearer_token: provider.apiKey || "",
      requires_openai_auth: provider.requiresOpenaiAuth,
    };
    const lines = original.split("\n").map((line) => {
      const header = line.match(/^\s*\[([^\]]+)\]\s*$/);
      if (header) {
        section = header[1];
        if (section === `model_providers.${originalId}`) {
          foundSection = true;
          return `[model_providers.${provider.id}]`;
        }
      }
      if (!section && /^model\s*=/.test(line)) return `model = ${JSON.stringify(provider.model)}`;
      if (!section && /^model_provider\s*=/.test(line)) return `model_provider = ${JSON.stringify(provider.id)}`;
      if (section === `model_providers.${originalId}`) {
        const key = line.match(/^([a-z_]+)\s*=/)?.[1];
        if (key && Object.hasOwn(values, key)) return `${key} = ${JSON.stringify(values[key])}`;
      }
      return line;
    });
    if (!foundSection) lines.push("", ...toml(provider).split("\n").slice(3));
    return `${lines.join("\n")}\n`;
  }

  const detectedProvider = {
    id: "fixture-ccswitch",
    providerName: "Fixture CC Switch",
    baseUrl: "https://ccswitch.example.test/v1",
    model: "fixture-model-a",
    apiKey: "sk-fixture-only-a",
    wireApi: "responses",
    requiresOpenaiAuth: false,
  };
  // Visible inherited settings for provider-preset regression checks.
  detectedProvider.tomlConfig = '# fixture inherited settings\nmodel_reasoning_effort = "high"\napproval_policy = "on-request"\n'
    + toml(detectedProvider)
    + sharedProviderTables + '\n[features]\nshell_snapshot = true\n';
  let savedProviders = [{
    id: "fixture-saved",
    providerName: "Fixture Saved Provider",
    baseUrl: "https://saved.example.test/v1",
    model: "fixture-model-b",
    apiKey: "sk-fixture-only-b",
    wireApi: "responses",
    requiresOpenaiAuth: false,
    tomlConfig: "",
  }];
  const statuses = ["fixture-template", "fixture-template-other"].map((id, index) => ({
    id,
    filename: `${id}.md`,
    title: index === 0 ? "Fixture 内置模板" : "Fixture 第二模板",
    subtitle: "用于延迟请求与切页验证的本地假模板",
    badge: "Fixture",
    sourceUrl: `https://templates.example.test/${id}.md`,
    cached: true,
    updated: false,
    contentSource: "cache",
    syncIssue: null,
    checkedAt: null,
    message: "Fixture cached template",
    customized: false,
  }));
  const details = new Map(statuses.map((item) => [item.id, {
    id: item.id,
    filename: item.filename,
    title: item.title,
    content: `# ${item.title}\n\nThis is a local UI fixture.\n`,
    customized: false,
  }]));
  let state = {
    codexDir,
    configPath: `${codexDir}/config.toml`,
    authPath: `${codexDir}/auth.json`,
    agentsPath: `${codexDir}/AGENTS.md`,
    configExists: true,
    authExists: true,
    officialAuthAvailable: true,
    model: detectedProvider.model,
    modelProvider: detectedProvider.id,
    isOfficialProvider: false,
    instructionEnabled: false,
    instructionInjectionMode: "append",
    instructionTemplateKey: undefined,
    activeSavedProviderId: undefined,
    activeOfficialProfileId: null,
    providers: [{
      id: detectedProvider.id,
      name: detectedProvider.providerName,
      baseUrl: detectedProvider.baseUrl,
      wireApi: "responses",
      requiresOpenaiAuth: false,
      isCurrent: true,
    }],
    configText: detectedProvider.tomlConfig,
    authText: JSON.stringify({ OPENAI_API_KEY: "sk-fixture-only-a" }),
    authPreview: { OPENAI_API_KEY: "sk-fixture-only-a" },
  };

  try {
    const saved = JSON.parse(sessionStorage.getItem(storageKey) || "null");
    if (saved?.version === 2 && saved.state && Array.isArray(saved.savedProviders)
      && Array.isArray(saved.savedPrompts) && Number.isSafeInteger(saved.nextOfficialId)
      && Array.isArray(saved.officialProfiles)
      && saved.officialProfiles.some((profile) => profile.id === (saved.defaultOfficialId || defaultOfficialId))) {
      defaultOfficialId = saved.defaultOfficialId || defaultOfficialId;
      state = saved.state;
      if (saved.failoverSettings?.version === 2) failoverSettings = { ...defaultRoutingSettings(), ...saved.failoverSettings };
      savedProviders = saved.savedProviders;
      for (const [scope, order] of Object.entries(saved.providerOrders || {})) {
        if (Array.isArray(order) && order.every((key) => typeof key === "string")) {
          providerOrders.set(scope, clone(order));
        }
      }
      savedPrompts = saved.savedPrompts;
      nextOfficialId = saved.nextOfficialId;
      officialProfiles.clear();
      for (const profile of saved.officialProfiles) officialProfiles.set(profile.id, profile);
    }
  } catch {
    // Corrupt or unavailable fixture storage leaves the fresh in-memory model intact.
  }

  function persist() {
    try {
      sessionStorage.setItem(storageKey, JSON.stringify({
        version: 2,
        state,
        savedProviders,
        providerOrders: Object.fromEntries(providerOrders),
        savedPrompts,
        nextOfficialId,
        defaultOfficialId,
        failoverSettings,
        officialProfiles: Array.from(officialProfiles.values()),
      }));
    } catch {
      // Tests still work in memory if sessionStorage is unavailable.
    }
  }

  localStorage.setItem("codexx.lang", "zh");
  const firstRunFixture = new URLSearchParams(location.search).has("firstRun");
  if (firstRunFixture && sessionStorage.getItem("codexx.fixture.firstRunStarted") !== "1") {
    localStorage.removeItem("codexx.startupWizardSeen");
    sessionStorage.setItem("codexx.fixture.firstRunStarted", "1");
  } else if (!firstRunFixture) localStorage.setItem("codexx.startupWizardSeen", "1");
  localStorage.removeItem("codexx.activeProviderId");
  localStorage.removeItem("codexx.promptCategories.v1");

  function snapshot() {
    return {
      startupWizardSeen: localStorage.getItem("codexx.startupWizardSeen"),
      codexLanguage: codexLanguageStatus(),
      codexLanguageMode,
      transferMode, lastTransfer,
      failoverSettings: clone(failoverSettings), failoverMode,
      pendingSync: pending.sync.length,
      pendingDetail: pending.detail.length,
      pendingQuota: pending.quota.length,
      pendingResetCredits: pending.resetCredits.length,
      resetCreditsMode,
      quotaMode,
      commandCounts: clone(counts),
      providerSwitches: clone(switches),
      currentProvider: state.modelProvider,
      activeOfficialProfileId: state.activeOfficialProfileId,
      currentOfficialAccount: state.isOfficialProvider ? accountId(state.authText) : null,
      officialProfiles: Array.from(officialProfiles.values(), (profile) => ({
        ...officialSummary(profile),
        accountId: accountId(profile.authJson),
      })),
      externalOfficialLogins,
      usageMode,
      pendingUsage: pending.usage.length,
      savedProviderIds: savedProviders.map((provider) => provider.id),
      providerOrders: Object.fromEntries(providerOrders),
      providerOrderMode,
      pendingProviderOrder: pending.providerOrder.length,
      commands: clone(commands),
    };
  }

  function render() {
    if (output) output.textContent = JSON.stringify(snapshot(), null, 2);
    if (loginButton) loginButton.disabled = !state.isOfficialProvider;
  }

  function accountId(authJson) {
    try { return JSON.parse(authJson || "null")?.tokens?.account_id || null; }
    catch { return null; }
  }

  function officialSummary(profile) {
    const authJson = state.isOfficialProvider && state.activeOfficialProfileId === profile.id ? state.authText : profile.authJson;
    let authentication;
    try { authentication = JSON.parse(authJson || "null"); } catch { authentication = null; }
    return {
      id: profile.id,
      providerName: profile.providerName,
      model: profile.model || null,
      hasAuth: Boolean(profile.authJson.trim()),
      hasOwnedAuth: Boolean(authentication && typeof authentication === "object"),
      email: typeof authentication?.email === "string" ? authentication.email.trim() || null : null,
      planType: typeof authentication?.tokens?.access_token === "string" ? authentication.plan_type || null : null,
      canQueryQuota: Boolean(!authentication?.OPENAI_API_KEY && typeof authentication?.tokens?.access_token === "string" && authentication.tokens.access_token.trim()),
      isDefault: profile.id === defaultOfficialId,
      isCurrent: state.isOfficialProvider && state.activeOfficialProfileId === profile.id,
    };
  }

  function officialProfile(profileId) {
    const profile = officialProfiles.get(profileId);
    if (!profile) throw new Error(`Unknown fixture official profile: ${profileId}`);
    return profile;
  }

  function officialDetail(profile) {
    return clone({ ...officialSummary(profile), authJson: profile.authJson, configText: profile.configText });
  }

  function captureCurrentOfficial() {
    if (!state.isOfficialProvider || !state.activeOfficialProfileId) return;
    const profile = officialProfile(state.activeOfficialProfileId);
    profile.authJson = state.authText || "";
    profile.configText = state.configText;
    profile.model = state.model || null;
  }

  function loginSecondOfficialAccount() {
    if (!state.isOfficialProvider) return false;
    // Simulates Codex changing auth.json externally; the profile snapshot is saved later.
    state.authText = officialAuth("two");
    state.authPreview = JSON.parse(state.authText);
    state.authExists = true;
    externalOfficialLogins += 1;
    persist();
    render();
    return true;
  }

  function defer(kind, result, failure = null) {
    return new Promise((resolve, reject) => {
      pending[kind].push({ resolve, reject, result, failure });
      render();
    });
  }

  function settle(kind, fail) {
    for (const request of pending[kind].splice(0)) {
      if (fail || request.failure) request.reject(new Error(request.failure || `Fixture ${kind} failure`));
      else request.resolve(clone(request.result()));
    }
    render();
  }

  function saveProvider(provider) {
    const next = clone(provider);
    const existingIndex = savedProviders.findIndex((item) => item.id === next.id);
    if (existingIndex >= 0) savedProviders[existingIndex] = next;
    else savedProviders.push(next);
    return next;
  }

  function providerOrderScope(configDir) {
    // Virtual fixture paths have no filesystem or symlinks. Normalize simple
    // absolute paths only; Rust tests cover real canonical paths and aliases.
    const raw = typeof configDir === "string" && configDir.trim() ? configDir.trim() : codexDir;
    const parts = [];
    for (const part of raw.replaceAll("\\", "/").split("/")) {
      if (!part || part === ".") continue;
      if (part === "..") parts.pop();
      else parts.push(part);
    }
    return `/${parts.join("/")}`;
  }

  function getProviderOrder(configDir) {
    return clone(providerOrders.get(providerOrderScope(configDir)) || []);
  }

  function setProviderOrderMode(mode) {
    if (!["normal", "error", "pending", "pending-error"].includes(mode)) throw new Error("Unknown fixture provider-order mode");
    providerOrderMode = mode;
    render();
  }

  function saveProviderOrder({ configDir, order }) {
    if (!Array.isArray(order) || order.some((key) => {
      if (typeof key !== "string" || /[\u0000-\u001f\u007f-\u009f]/.test(key)) return true;
      const separator = key.indexOf(":");
      const source = key.slice(0, separator);
      const id = key.slice(separator + 1);
      return !["official", "local", "detected"].includes(source) || !id || id !== id.trim();
    }) || new Set(order).size !== order.length) throw new Error("Fixture：排序包含无效或重复供应商");
    const scope = providerOrderScope(configDir);
    const next = clone(order);
    const mode = providerOrderMode;
    const failure = "Fixture：供应商排序保存失败，原顺序未改变";
    if (mode === "error") throw new Error(failure);
    const save = () => {
      providerOrders.set(scope, next);
      return next;
    };
    if (mode === "pending" || mode === "pending-error") {
      return defer("providerOrder", save, mode === "pending-error" ? failure : null);
    }
    return clone(save());
  }

  function uniqueCopyName(requested, names) {
    const base = requested?.trim() || "Fixture 副本";
    const used = new Set(names);
    let name = base;
    for (let suffix = 2; used.has(name); suffix += 1) name = `${base} ${suffix}`;
    return name;
  }

  function duplicateProvider(providerId, providerName) {
    if (providerId == null) preserveDetectedProvider();
    const sourceId = providerId || state.activeSavedProviderId || state.modelProvider;
    const source = savedProviders.find((provider) => provider.id === sourceId);
    if (!source || state.isOfficialProvider && providerId == null) throw new Error("Unknown fixture provider to duplicate");
    let suffix = 1;
    let id = `fixture-copy-${suffix}`;
    while (savedProviders.some((provider) => provider.id === id)) id = `fixture-copy-${++suffix}`;
    const provider = saveProvider({
      ...clone(source),
      id,
      providerName: uniqueCopyName(providerName || `${source.providerName} 副本`, savedProviders.map((item) => item.providerName)),
    });
    // Capturing a detected row makes its existing identity explicit; copying never switches it.
    if (!state.isOfficialProvider && !state.activeSavedProviderId && sourceId === state.modelProvider) {
      state.activeSavedProviderId = sourceId;
    }
    return clone({ provider, providers: savedProviders, activeProviderId: state.activeSavedProviderId || null });
  }

  function updateContextWindow({ configText, enabled, previousValues }) {
    // This is a deliberately limited model for the fixture's simple TOML drafts,
    // not a production TOML parser. Rust tests cover the real toml_edit behavior.
    const keys = ["model_context_window", "model_auto_compact_token_limit"];
    function scan(text) {
      const lines = text.split("\n");
      const fields = {};
      let firstTable = lines.length;
      let multiline = null;
      for (let index = 0; index < lines.length; index += 1) {
        const line = lines[index];
        const trimmed = line.trim();
        if (multiline) {
          if (line.includes(multiline)) multiline = null;
          continue;
        }
        if (!trimmed || trimmed.startsWith("#")) continue;
        if (trimmed.startsWith("[")) {
          if (!/^\[.+\]\s*(?:#.*)?$/.test(trimmed)) throw new Error("Fixture TOML table syntax error");
          firstTable = Math.min(firstTable, index);
          continue;
        }
        for (const quote of ['"""', "'''"]) {
          if (line.split(quote).length % 2 === 0) multiline = quote;
        }
        if (index >= firstTable) continue;
        for (const key of keys) {
          const start = new RegExp(`^\\s*(?:${key}|"${key}"|'${key}')\\s*=`);
          if (!start.test(line)) continue;
          const match = line.match(new RegExp(`^(\\s*(?:${key}|"${key}"|'${key}')\\s*=\\s*)([+-]?[0-9][0-9_]*)(\\s*(?:#.*)?)$`));
          if (!match || fields[key]) throw new Error(`Fixture invalid integer setting: ${key}`);
          const value = Number(match[2].replaceAll("_", ""));
          if (!Number.isSafeInteger(value)) throw new Error(`Fixture unsupported integer: ${key}`);
          fields[key] = { index, value, prefix: match[1], suffix: match[3] };
        }
      }
      if (multiline) throw new Error("Fixture TOML multiline string syntax error");
      return { lines, fields, firstTable };
    }
    function set(text, key, value) {
      const { lines, fields, firstTable } = scan(text);
      const field = fields[key];
      if (field) {
        if (value == null) lines.splice(field.index, 1);
        else lines[field.index] = `${field.prefix}${value}${field.suffix}`;
      } else if (value != null) {
        lines.splice(Math.min(firstTable, lines.length - (lines.at(-1) === "" ? 1 : 0)), 0, `${key} = ${value}`);
      }
      return lines.join("\n");
    }
    let next = configText;
    const original = scan(next).fields;
    const context = original[keys[0]]?.value ?? null;
    const compact = original[keys[1]]?.value ?? null;
    if (enabled === true) {
      next = set(next, keys[0], 1000000);
      if (compact == null) next = set(next, keys[1], 900000);
    } else if (enabled === false) {
      if (context === 1000000) next = set(next, keys[0], previousValues?.contextWindow ?? null);
      if (compact === (previousValues?.compactTokenLimit ?? 900000)) {
        next = set(next, keys[1], previousValues?.compactTokenLimit ?? null);
      }
    }
    const fields = scan(next).fields;
    return {
      configText: next,
      enabled: fields[keys[0]]?.value === 1000000,
      contextWindow: fields[keys[0]]?.value ?? null,
      compactTokenLimit: fields[keys[1]]?.value ?? null,
    };
  }

  function preserveDetectedProvider() {
    // This stub models the expected Rust persistence behavior. It does not test Rust.
    if (state.modelProvider === detectedProvider.id
      && !savedProviders.some((provider) => provider.id === detectedProvider.id
        || provider.baseUrl === detectedProvider.baseUrl && provider.apiKey === detectedProvider.apiKey)) {
      saveProvider(detectedProvider);
    }
  }

  function action(message) {
    return clone({ ok: true, message, state });
  }

  function usageStatistics({ range = "7d", model = null } = {}) {
    if (usageMode === "error") throw new Error("Fixture usage read failed; no real files were accessed");
    const now = new Date();
    const today = new Date(now.getFullYear(), now.getMonth(), now.getDate(), 12);
    const dateKey = (value) => `${value.getFullYear()}-${String(value.getMonth() + 1).padStart(2, "0")}-${String(value.getDate()).padStart(2, "0")}`;
    const dayAt = (daysAgo) => { const day = new Date(today); day.setDate(day.getDate() - daysAgo); return day; };
    const daysBack = ({ today: 0, "7d": 6, "30d": 29, all: 39 })[range];
    if (daysBack === undefined) throw new Error(`Unknown fixture usage range: ${range}`);
    const modelNames = ["gpt-5.5", "gpt-5.4-mini", "gpt-5.3-codex"];
    const titles = ["优化供应商切换", "设计用量统计", "完善本地回归验证"];
    const emptyTotals = () => ({ inputTokens: 0, cachedInputTokens: 0, outputTokens: 0, reasoningTokens: 0, totalTokens: 0, sessionCount: 0, usageEventCount: 0 });
    const sum = (rows) => rows.reduce((totals, row) => {
      for (const key of Object.keys(totals)) totals[key] += row[key];
      return totals;
    }, emptyTotals());
    const records = [];
    if (usageMode !== "empty") {
      for (let daysAgo = daysBack; daysAgo >= 0; daysAgo -= 1) {
        if (daysAgo === 3 || daysAgo === 12 || daysAgo === 25) continue;
        for (let modelIndex = 0; modelIndex < modelNames.length; modelIndex += 1) {
          if ((daysAgo + modelIndex) % 5 === 4) continue;
          const volume = [2.4, 1.1, 0.7][modelIndex] * (1 + ((daysAgo * 7 + modelIndex * 3) % 13) / 5);
          const inputTokens = Math.round(134500 * volume);
          const cachedInputTokens = Math.round(inputTokens * (0.42 + (daysAgo % 4) / 10));
          const outputTokens = Math.round(18400 * volume);
          const reasoningTokens = Math.round(outputTokens * 0.36);
          const date = dateKey(dayAt(daysAgo));
          const requestedEnd = new Date(`${date}T${15 + modelIndex}:32:00`);
          const lastActive = requestedEnd > now ? now : requestedEnd;
          const midnight = new Date(lastActive.getFullYear(), lastActive.getMonth(), lastActive.getDate());
          const startedAt = new Date(Math.max(midnight.getTime(), lastActive.getTime() - 80 * 60 * 1000));
          records.push({
            id: `fixture-usage-${date}-${modelIndex}`,
            title: `${titles[modelIndex]} · ${date.slice(5)}`,
            model: modelNames[modelIndex], date,
            startedAt: startedAt.toISOString(), lastActiveAt: lastActive.toISOString(),
            inputTokens, cachedInputTokens, outputTokens, reasoningTokens,
            totalTokens: inputTokens + outputTokens, sessionCount: 1, usageEventCount: 8 + daysAgo % 19,
          });
        }
      }
    }
    const availableModels = Array.from(new Set(records.map((record) => record.model))).sort();
    const selected = records.filter((record) => !model || record.model === model);
    const days = [];
    for (let daysAgo = daysBack; daysAgo >= 0; daysAgo -= 1) {
      const date = dateKey(dayAt(daysAgo));
      days.push({ date, ...sum(selected.filter((record) => record.date === date)) });
    }
    return {
      totals: sum(selected), days,
      models: availableModels.map((name) => ({ model: name, ...sum(selected.filter((record) => record.model === name)) }))
        .filter((item) => item.totalTokens > 0).sort((left, right) => right.totalTokens - left.totalTokens),
      sessions: selected.slice().sort((left, right) => right.lastActiveAt.localeCompare(left.lastActiveAt)).slice(0, 10)
        .map(({ date: _date, ...session }) => session),
      availableModels,
      coverage: {
        scannedFiles: usageMode === "empty" ? 0 : 103,
        matchedSessions: selected.length,
        skippedFiles: usageMode === "partial" ? 2 : 0,
        warnings: usageMode === "partial" ? ["Fixture: two synthetic files could not be parsed"] : [],
        truncated: false,
      },
      rangeStart: range === "all" ? null : new Date(today.getFullYear(), today.getMonth(), today.getDate() - daysBack).toISOString(),
      rangeEnd: now.toISOString(), refreshedAt: now.toISOString(),
      timezone: `UTC${now.getTimezoneOffset() > 0 ? "-" : "+"}${String(Math.floor(Math.abs(now.getTimezoneOffset()) / 60)).padStart(2, "0")}:${String(Math.abs(now.getTimezoneOffset()) % 60).padStart(2, "0")}`,
      sourceDirectories: [`${codexDir}/sessions`, `${codexDir}/archived_sessions`],
    };
  }

  function setUsageMode(mode) {
    if (!["sample", "empty", "error", "partial"].includes(mode)) throw new Error(`Unknown fixture usage mode: ${mode}`);
    usageMode = mode;
    render();
  }

  const quotaModes = [
    ["dual", "双窗口 Team：93% / 52%"],
    ["weekly", "单周 Pro：91%"],
    ["additional", "Spark 双窗 + reserve 周"],
    ["empty", "无主额度窗口"],
    ["unknown", "未知窗口周期"],
    ["error", "认证过期 401"],
    ["pending", "延迟：完成后成功"],
    ["pending-error", "延迟：完成后失败"],
  ];

  function setQuotaMode(mode) {
    if (!quotaModes.some(([id]) => id === mode)) throw new Error(`Unknown fixture quota mode: ${mode}`);
    quotaMode = mode;
    render();
  }

  function officialQuota({ profileId }) {
    const profile = officialProfile(profileId);
    const summary = officialSummary(profile);
    if (!summary.canQueryQuota) throw new Error("Fixture：此官方配置没有可查询额度的登录凭据");
    const requestedMode = quotaMode;
    const failure = "Fixture：登录已过期或无效（HTTP 401），请在 Codex 中重新登录";
    if (requestedMode === "error") throw new Error(failure);
    const checkedAt = new Date();
    const window = (id, seconds, remaining, resetSeconds) => ({
      id, windowSeconds: seconds, usedPercent: remaining === null ? null : 100 - remaining,
      remainingPercent: remaining,
      resetsAt: resetSeconds === null ? null : new Date(checkedAt.getTime() + resetSeconds * 1000).toISOString(),
    });
    const main = { id: "main", name: null, allowed: true, limitReached: false, windows: [window("primary", 18000, 93, 7200), window("secondary", 604800, 52, 259200)] };
    const result = { profileId, email: summary.email, planType: summary.planType || "team", limits: [main], checkedAt: checkedAt.toISOString() };
    if (requestedMode === "weekly") {
      result.planType = "pro";
      main.windows = [window("primary", 604800, 91, 432000)];
    } else if (requestedMode === "additional") {
      result.limits.push(
        { id: "additional-spark", name: "GPT-5.3-Codex-Spark", allowed: true, limitReached: false, windows: [window("primary", 18000, 78, 1800), window("secondary", 604800, 64, 345600)] },
        { id: "reserve", name: "Reserve", allowed: true, limitReached: false, windows: [window("primary", 604800, 86, 172800)] },
      );
    } else if (requestedMode === "empty") {
      result.planType = null;
      result.limits = [];
    } else if (requestedMode === "unknown") {
      main.windows = [window("primary", 7200, 37, 1200), window("secondary", null, null, null)];
      main.allowed = null;
      main.limitReached = null;
    }
    // Capture the selected scenario now; changing the selector cannot settle an
    // existing request differently. Only explicit quota/refresh clicks invoke it.
    if (requestedMode === "pending" || requestedMode === "pending-error") {
      return defer("quota", () => result, requestedMode === "pending-error" ? failure : null);
    }
    return clone(result);
  }

  function officialResetCredits({ profileId }) {
    const profile = officialProfile(profileId);
    if (!officialSummary(profile).canQueryQuota) throw new Error("Fixture：请先登录此账号");
    const mode = resetCreditsMode;
    const failure = "Fixture：重置次数查询失败，请稍后重试";
    if (mode === "error") throw new Error(failure);
    const result = { profileId, availableCount: mode === "zero" ? 0 : 3, checkedAt: new Date().toISOString() };
    if (mode === "pending" || mode === "pending-error") return defer("resetCredits", () => result, mode === "pending-error" ? failure : null);
    return clone(result);
  }

  function switchProvider(provider) {
    preserveDetectedProvider();
    captureCurrentOfficial();
    const from = state.modelProvider;
    const fromProfile = state.activeOfficialProfileId;
    state = {
      ...state,
      model: provider.model,
      modelProvider: provider.id,
      isOfficialProvider: false,
      activeOfficialProfileId: null,
      activeSavedProviderId: provider.id,
      authExists: Boolean(provider.apiKey),
      configText: provider.tomlConfig || toml(provider),
      authText: JSON.stringify({ OPENAI_API_KEY: provider.apiKey }),
      providers: [{
        id: provider.id,
        name: provider.providerName,
        baseUrl: provider.baseUrl,
        wireApi: provider.wireApi,
        requiresOpenaiAuth: provider.requiresOpenaiAuth,
        isCurrent: true,
      }],
    };
    switches.push({ from, fromProfile, to: provider.id, toProfile: null });
    render();
    return action(`Fixture switched to ${provider.providerName}`);
  }

  function switchOfficial(profileId = defaultOfficialId) {
    officialProfile(profileId);
    preserveDetectedProvider();
    captureCurrentOfficial();
    const profile = officialProfile(profileId);
    const from = state.modelProvider;
    const fromProfile = state.activeOfficialProfileId;
    state = {
      ...state,
      modelProvider: "openai",
      model: profile.model || "fixture-official-model",
      isOfficialProvider: true,
      activeOfficialProfileId: profile.id,
      activeSavedProviderId: undefined,
      authExists: Boolean(profile.authJson),
      providers: [],
      configText: profile.configText,
      authText: profile.authJson,
      authPreview: profile.authJson ? JSON.parse(profile.authJson) : null,
    };
    switches.push({ from, fromProfile, to: "openai", toProfile: profile.id });
    render();
    return action(`Fixture switched to ${profile.providerName}`);
  }

  function saveOfficial(input) {
    const providerName = input.providerName.trim();
    if (!providerName) throw new Error("Fixture official provider name is required");
    if (input.authJson?.trim()) JSON.parse(input.authJson);
    if (input.id) officialProfile(input.id);
    if (input.id && input.authJson == null) captureCurrentOfficial();
    const authJson = input.authJson == null
      ? (input.id ? officialProfile(input.id).authJson : "")
      : input.authJson.trim();
    const id = input.id || `fixture-official-${nextOfficialId++}`;
    const profile = {
      id,
      providerName,
      model: input.model?.trim() || null,
      configText: input.configText?.trim() || officialToml(input.model),
      authJson,
      isDefault: id === defaultOfficialId,
    };
    officialProfiles.set(id, profile);
    if (state.isOfficialProvider && state.activeOfficialProfileId === id) {
      state.model = profile.model || "fixture-official-model";
      state.configText = profile.configText;
      state.authText = profile.authJson;
      state.authExists = Boolean(profile.authJson);
      state.authPreview = profile.authJson ? JSON.parse(profile.authJson) : null;
    }
    return clone({ ...action("Fixture official profile saved"), profile: officialSummary(profile) });
  }

  function duplicateOfficial(profileId, providerName) {
    captureCurrentOfficial();
    const original = officialProfile(profileId);
    return saveOfficial({
      ...clone(original), id: null,
      providerName: uniqueCopyName(providerName || `${original.providerName} 副本`, Array.from(officialProfiles.values(), (profile) => profile.providerName)),
    });
  }

  function emitRoutingChanged() {
    for (const [eventId, listener] of eventListeners) {
      if (listener.event === "provider-routing-changed") callbacks.get(listener.handler)?.({ event: listener.event, id: eventId, payload: { codexDir } });
    }
  }

  function failoverStatus() {
    const summary = (provider) => ({ id: provider.id, providerName: provider.providerName, model: provider.model,
      baseUrl: provider.baseUrl, official: false, models: [...new Set([provider.model, ...(provider.modelMappings || []).map((mapping) => mapping.model)])] });
    const providers = savedProviders.map((provider) => {
      const reason = provider.wireApi !== "responses" ? "此供应商暂不支持 Responses 接口" : !provider.apiKey ? "请先填写 API Key" : null;
      return { ...summary(provider), eligible: !reason, reason };
    });
    const profile = state.isOfficialProvider ? officialProfiles.get(state.activeOfficialProfileId || defaultOfficialId) : null;
    const primary = profile ? { id: `official:${profile.id}`, providerName: profile.providerName, model: profile.model, models: [profile.model],
      baseUrl: "https://chatgpt.com/codex", official: true, eligible: Boolean(profile.authJson), reason: profile.authJson ? null : "请先登录官方账号" }
      : providers.find((provider) => provider.id === state.activeSavedProviderId) || null;
    const running = failoverSettings.routerEnabled;
    const takeoverActive = Boolean(running && failoverSettings.takeoverEnabled && primary?.eligible);
    const autoFailoverActive = Boolean(takeoverActive && failoverSettings.autoFailoverEnabled && !primary.official);
    const routes = !takeoverActive ? [] : autoFailoverActive ? failoverSettings.providerIds.map((id) => providers.find((provider) => provider.id === id)).filter(Boolean) : [primary];
    const samples = running && ["cooling", "recovering"].includes(failoverMode);
    const host = failoverSettings.listenAddress === "0.0.0.0" ? "127.0.0.1" : failoverSettings.listenAddress === "::" ? "[::1]" : failoverSettings.listenAddress.includes(":") ? `[${failoverSettings.listenAddress}]` : failoverSettings.listenAddress;
    return clone({ settings: failoverSettings, running, takeoverActive, autoFailoverActive, address: running ? `http://${host}:${failoverSettings.listenPort}/v1` : null, primary, providers,
      runtime: { requestCount: samples ? 28 : 0, failoverCount: samples ? 2 : 0, inFlight: 0, successCount: samples ? 26 : 0, failureCount: samples ? 2 : 0,
        uptimeSeconds: running ? 365 : 0, lastRequestAt: samples ? new Date().toISOString() : null,
        lastProviderId: samples ? routes.at(-1)?.id || null : null, lastError: null,
        providers: routes.map((provider, index) => { const state = resetRoutingHealth.has(provider.id) || !samples ? "closed" : index === 0 ? (failoverMode === "recovering" ? "half_open" : "open") : "closed";
          return { id: provider.id, state, cooldownSeconds: state === "open" ? 25 : 0, lastStatus: state === "closed" ? 200 : 503,
            consecutiveFailures: state === "open" ? 4 : 0, consecutiveSuccesses: state === "half_open" ? 1 : 0, totalRequests: samples ? 14 : 0, failedRequests: state === "closed" ? 0 : 4 }; }) },
      message: primary?.official && failoverSettings.autoFailoverEnabled ? "当前为官方登录，仅使用当前账号；自动故障转移暂不运行。" : autoFailoverActive && !routes.length ? "队列为空，请添加 API 供应商。" : null });
  }

  function seedFailover() {
    for (const [id, providerName, model] of [["fixture-failover-primary", "主供应商", "gpt-example"], ["fixture-failover-backup-a", "备用一", "gpt-example"], ["fixture-failover-backup-b", "备用二", "gpt-example"], ["fixture-failover-other", "不同模型供应商", "other-model"]]) {
      saveProvider({ id, providerName, model, apiKey: "sk-fixture-only-failover", baseUrl: `https://${id}.example.test/v1`, wireApi: "responses", requiresOpenaiAuth: false, tomlConfig: "", modelMappings: [] });
    }
    switchProvider(savedProviders.find((provider) => provider.id === "fixture-failover-primary"));
    failoverMode = "normal";
    failoverSettings = defaultRoutingSettings();
    resetRoutingHealth.clear();
    persist(); render(); emitRoutingChanged();
  }

  async function invoke(command, args = {}) {
    counts[command] = (counts[command] || 0) + 1;
    commands.push(command);
    render();
    try {
      return await dispatch(command, args);
    } finally {
      persist();
      render();
    }
  }

  async function dispatch(command, args) {
    switch (command) {
      case "get_codex_language_status": return codexLanguageStatus();
      case "save_codex_language_preferences": {
        if (codexLanguageMode === "save-error") throw new Error("Fixture：语言设置保存失败，当前配置未改变");
        if (typeof args.enabled !== "boolean" || args.locale !== (args.enabled ? "zh-CN" : "auto")) throw new Error("Fixture：语言偏好参数无效");
        codexLanguagePendingEnabled = args.enabled === codexLanguageEnabled ? null : args.enabled;
        return codexLanguageStatus();
      }
      case "apply_codex_language_preferences": {
        if (codexLanguageMode === "apply-error") throw new Error("Fixture：语言设置应用失败，尚未重启客户端");
        if (args.restore === true) throw new Error("Fixture：没有可恢复的语言快照");
        codexLanguageEnabled = codexLanguagePendingEnabled ?? codexLanguageEnabled;
        codexLanguagePendingEnabled = null;
        return codexLanguageStatus();
      }
      case "get_provider_config_base": {
        if (providerBaseReadFailure) throw new Error("Fixture：无法读取供应商配置底稿");
        // Model a legacy route-only file with the official integration snapshot
        // still available. Rust tests cover the actual parser/recovery rules.
        const config = state.configText.includes("[mcp_servers")
          ? state.configText : state.configText + sharedProviderTables;
        return config.replace(/^\s*experimental_bearer_token\s*=.*\n?/gm, "");
      }
      case "get_provider_failover":
        if (failoverMode === "error") throw new Error("Fixture：暂时无法读取运行状态");
        return failoverStatus();
      case "save_provider_failover": {
        if (failoverMode === "save-error") throw new Error("Fixture：设置保存失败，原设置未改变");
        const next = clone(args.settings);
        const status = failoverStatus();
        const ranges = { maxRetries: [0, 10], streamingFirstByteTimeout: [1, 120], streamingIdleTimeout: [0, 600], nonStreamingTimeout: [60, 1200], circuitFailureThreshold: [1, 20], circuitSuccessThreshold: [1, 10], circuitTimeoutSeconds: [0, 300], circuitErrorRateThreshold: [0, 1], circuitMinRequests: [5, 100] };
        for (const [key, [min, max]] of Object.entries(ranges)) if (!Number.isFinite(next[key]) || next[key] < min || next[key] > max || key !== "circuitErrorRateThreshold" && !Number.isInteger(next[key])) throw new Error(`Fixture：参数 ${key} 超出范围`);
        if (!Number.isInteger(next.listenPort) || next.listenPort < 1024 || next.listenPort > 65535) throw new Error("Fixture：监听端口无效");
        const host = next.listenAddress.trim();
        let validAddress = /^(\d{1,3}\.){3}\d{1,3}$/.test(host) && host.split(".").every((part) => Number(part) <= 255 && String(Number(part)) === part);
        if (host.includes(":")) { try { validAddress = new URL(`http://[${host}]/`).hostname.startsWith("["); } catch {} }
        if (host === "localhost") validAddress = true;
        if (!validAddress) throw new Error("Fixture：监听地址无效");
        next.listenAddress = host === "localhost" ? "127.0.0.1" : host;
        if (status.running && (next.listenAddress !== failoverSettings.listenAddress || next.listenPort !== failoverSettings.listenPort)) throw new Error("Fixture：停止路由后再修改监听地址");
        if (!next.routerEnabled) next.takeoverEnabled = false;
        if (next.takeoverEnabled && !status.primary?.eligible) throw new Error("Fixture：请先选择可用供应商或登录官方账号");
        if (next.providerIds.length > 64 || new Set(next.providerIds).size !== next.providerIds.length || next.providerIds.some((id) => !status.providers.find((provider) => provider.id === id)?.eligible)) throw new Error("Fixture：队列包含无效或重复供应商");
        const enablingAuto = next.autoFailoverEnabled && !failoverSettings.autoFailoverEnabled;
        if (enablingAuto && (!next.routerEnabled || !next.takeoverEnabled)) throw new Error("Fixture：请先开启本地路由和 Codex 路由");
        if (enablingAuto && !next.providerIds.length) {
          if (!status.primary?.eligible || status.primary.official) throw new Error("Fixture：请添加 API 供应商到队列");
          next.providerIds.push(status.primary.id);
        }
        if (enablingAuto) switchProvider(savedProviders.find((provider) => provider.id === next.providerIds[0]));
        failoverSettings = next;
        emitRoutingChanged();
        return failoverStatus();
      }
      case "reset_provider_failover_health":
        if (args.providerId) resetRoutingHealth.add(args.providerId); else { failoverMode = "normal"; resetRoutingHealth.clear(); }
        return failoverStatus();
      case "plugin:event|listen": {
        const eventId = nextCallback++;
        eventListeners.set(eventId, { event: args.event, handler: args.handler });
        return eventId;
      }
      case "plugin:event|unlisten": eventListeners.delete(args.eventId); return null;
      case "check_codex_config": return healthReport();
      case "repair_codex_config": {
        if (args.expectedFingerprint !== healthReport().fingerprint) throw new Error("配置已变更，请重新检查。");
        if (!healthReport().canRepair) throw new Error("此配置需要手动检查。");
        await new Promise((resolve) => setTimeout(resolve, 300));
        configHealthMode = "healthy";
        configStateError = false;
        return { report: healthReport(), changed: true, backupId: "fixture-backup" };
      }
      case "open_codex_config_file": return null;
      case "get_codex_state":
        if (configStateError) throw new Error("Fixture：Codex 无法加载配置，Model provider my_codex not found。");
        return clone(state);
      case "list_saved_providers": return clone(savedProviders);
      case "get_provider_order": return getProviderOrder(args.configDir);
      case "save_provider_order": return saveProviderOrder(args);
      case "repair_provider_model_catalog": return { changed: false };
      case "list_official_models": return {
        models: [
          { id: "gpt-6.1-sol", displayName: "GPT-6.1 Sol", isDefault: true },
          { id: "gpt-6-astra", displayName: "GPT-6 Astra", isDefault: false },
          { id: "gpt-6-luna", displayName: "GPT-6 Luna", isDefault: false },
        ],
        recommendedModel: "gpt-6.1-sol", source: "bundled", warning: null,
      };
      case "update_official_model_draft": return officialModelDraft(args.configText, args.model).configText;
      case "inspect_official_model_draft": return officialModelDraft(args.configText).model;
      case "list_saved_prompts": return clone(savedPrompts);
      case "get_builtin_prompt_status": return clone(statuses);
      case "list_official_profiles":
        captureCurrentOfficial();
        return Array.from(officialProfiles.values(), (profile) => clone(officialSummary(profile)));
      case "get_official_profile":
        captureCurrentOfficial();
        return officialDetail(officialProfile(args.profileId));
      case "get_official_profile_quota": return officialQuota(args);
      case "get_official_profile_reset_credits": return officialResetCredits(args);
      case "save_official_profile": return saveOfficial(args.input);
      case "duplicate_official_profile": return duplicateOfficial(args.profileId, args.providerName);
      case "switch_official_profile": return switchOfficial(args.profileId);
      case "delete_official_profile":
        officialProfile(args.profileId);
        if (officialProfiles.size <= 1) throw new Error("Keep at least one official account");
        if (state.isOfficialProvider && state.activeOfficialProfileId === args.profileId) {
          throw new Error("Switch away before deleting the current fixture official profile");
        }
        if (args.profileId === defaultOfficialId) {
          defaultOfficialId = state.activeOfficialProfileId && officialProfiles.has(state.activeOfficialProfileId)
            ? state.activeOfficialProfileId : Array.from(officialProfiles.keys()).find(id => id !== args.profileId);
        }
        officialProfiles.delete(args.profileId);
        return undefined;
      case "get_about_info": return {
        appVersion: "0.3.15", codexVersion: "fixture-cli", codexDir,
        projectUrl: "https://project.example.test/", githubRepo: "fixture/example",
        nativeUpdaterSupported: updaterEnabled,
      };
      case "get_startup_diagnostics": {
        if (new URLSearchParams(location.search).has("environmentError")) throw new Error("Fixture: environment check unavailable");
        return {
          codexDir, needsManualSelect: false, summary: "Fixture ready",
          items: [
            { key: "home", label: "Codex 配置目录", status: "ok", message: "已找到", path: codexDir },
            { key: "config", label: "配置文件", status: "ok", message: "已找到", path: `${codexDir}/config.toml` },
            { key: "auth", label: "登录信息", status: "ok", message: "已找到", path: `${codexDir}/auth.json` },
            { key: "sessions", label: "会话记录", status: "ok", message: "已找到", path: `${codexDir}/state_5.sqlite` },
          ],
        };
      }
      case "check_app_update": return {
        latestVersion: "0.3.15", htmlUrl: "https://releases.example.test/", hasUpdate: false,
      };
      case "plugin:updater|check": return updaterEnabled ? {
        rid: 123, currentVersion: "0.3.20", version: "0.3.21", date: "2026-09-22T00:00:00Z", body: "优化 Windows 在线更新。", rawJson: {},
      } : null;
      case "install_app_update": return new Promise((resolve, reject) => {
        if (updateRequest) { reject({ stage: "install", message: "Fixture：更新已经进行中。" }); return; }
        updateRequest = { resolve, reject, onEvent: args.onEvent };
        updateStage({ event: "Started", data: { contentLength: 1024 * 1024 * 20 } });
        updateStage({ event: "Progress", data: { chunkLength: 1024 * 1024 * 10 } });
        render();
      });
      case "plugin:resources|close": return null;
      case "plugin:process|restart": return null;
      case "refresh_builtin_prompts": return defer("sync", () => statuses);
      case "get_builtin_prompt_detail": {
        if (!details.has(args.templateId)) throw new Error("Unknown fixture template");
        return defer("detail", () => details.get(args.templateId));
      }
      case "save_builtin_prompt_override": {
        const detail = details.get(args.templateId);
        if (!detail) throw new Error("Unknown fixture template");
        detail.content = args.content;
        detail.customized = true;
        statuses.find((item) => item.id === args.templateId).customized = true;
        return clone(detail);
      }
      case "enable_instruction_template":
        state.instructionEnabled = true;
        state.instructionTemplateKey = `builtin:${args.templateId}`;
        state.instructionInjectionMode = args.injectionMode;
        return action("Fixture prompt enabled");
      case "disable_instruction":
      case "disable_external_instruction":
        state.instructionEnabled = false;
        state.instructionTemplateKey = undefined;
        return action("Fixture prompt disabled");
      case "save_prompt":
        savedPrompts = [...savedPrompts.filter((prompt) => prompt.id !== args.prompt.id), clone(args.prompt)];
        return clone(args.prompt);
      case "delete_saved_prompt":
        savedPrompts = savedPrompts.filter((prompt) => prompt.id !== args.id);
        return undefined;
      case "enable_saved_prompt":
        state.instructionEnabled = true;
        state.instructionTemplateKey = `saved:${args.id}`;
        return action("Fixture saved prompt enabled");
      case "build_provider_toml_draft": return providerTomlDraft(args.provider);
      case "update_codex_context_window": return updateContextWindow(args);
      case "get_usage_statistics": {
        const result = usageStatistics(args);
        return pauseUsage ? defer("usage", () => result) : result;
      }
      case "duplicate_provider": return duplicateProvider(args.providerId, args.providerName);
      case "save_provider": return saveProvider(args.provider);
      case "save_active_provider": return switchProvider(saveProvider(args.provider));
      case "activate_saved_provider": {
        const provider = savedProviders.find((item) => item.id === args.providerId);
        if (!provider) throw new Error("Unknown fixture provider");
        return switchProvider(provider);
      }
      case "switch_provider": {
        const input = args.input;
        return switchProvider(savedProviders.find((provider) => provider.id === input.providerId) || {
          id: input.providerId, providerName: input.providerName,
          baseUrl: input.baseUrl, model: input.model, apiKey: input.apiKey,
          wireApi: input.wireApi, requiresOpenaiAuth: input.requiresOpenaiAuth,
        });
      }
      case "save_provider_toml_config": {
        const provider = savedProviders.find((item) => item.id === args.providerId);
        if (!provider) throw new Error("Unknown fixture provider");
        return switchProvider({ ...provider, tomlConfig: args.input.configText, apiKey: args.input.apiKey });
      }
      case "switch_official_provider": return switchOfficial();
      case "reset_official_provider":
        saveOfficial({ id: defaultOfficialId, providerName: officialProfile(defaultOfficialId).providerName, model: "fixture-official-model", authJson: "", configText: "" });
        return switchOfficial();
      case "save_official_config": {
        const input = args.input || args;
        return saveOfficial({ ...input, id: defaultOfficialId, providerName: officialProfile(defaultOfficialId).providerName });
      }
      case "read_ccswitch_official_auth": return {
        authJson: officialAuth("one"), configText: officialToml("fixture-official-model"),
        model: "fixture-official-model", source: "fixture",
      };
      case "delete_saved_provider":
        savedProviders = savedProviders.filter((provider) => provider.id !== args.id);
        render();
        return undefined;
      case "import_ccswitch_codex_providers":
        preserveDetectedProvider();
        return { imported: 1, added: 1, updated: 0, merged: 0, skipped: 0, warnings: [], providers: clone(savedProviders) };
      case "test_provider_connection": return { ok: true, status: 200, message: "Fixture connection OK", durationMs: 1 };
      case "fetch_provider_models": return { models: [{ id: "fixture-model-a" }, { id: "fixture-model-b" }], status: 200, durationMs: 1 };
      case "get_session_sync_status": return {
        codexDir, targetProvider: state.modelProvider, rolloutFiles: 2, sessionMetaCount: 2,
        mismatchedRollouts: 0, mismatchedSessionMeta: 0, sqliteDbs: 1, sqliteThreads: 3,
        topLevelThreads: 2, subagentThreads: 1, mismatchedThreads: 0, mismatchedSessions: 0,
        needsSync: false, scanComplete: true, scanFailures: [], warnings: [], sessions: clone(fixtureSessions),
      };
      case "get_skills_mcp_state": return clone(transferState);
      case "preview_existing_skills_mcp": return existingImportAvailable
        ? { skills: [fixtureSkill("research", "资料整理")], mcpServers: [fixtureMcp("files", "本地文件"), fixtureMcp("search", "网页检索")], warnings: ["Fixture：一条无效配置已跳过。"] }
        : { skills: [], mcpServers: [], warnings: [] };
      case "import_existing_skills_mcp":
        if (existingImportAvailable) {
          transferState.skills.push(fixtureSkill("research", "资料整理"));
          transferState.mcpServers.push(fixtureMcp("files", "本地文件"), fixtureMcp("search", "网页检索"));
          existingImportAvailable = false;
          return { importedSkills: 1, importedMcp: 2, message: "已导入 1 个 Skill、2 个 MCP", state: clone(transferState) };
        }
        return { importedSkills: 0, importedMcp: 0, message: "没有需要新导入的内容", state: clone(transferState) };
      case "export_codex_sessions":
      case "export_skills_mcp_archive":
      case "import_skills_mcp_archive": {
        await new Promise((resolve) => setTimeout(resolve, 250));
        if (transferMode === "cancel") return null;
        if (transferMode === "error") throw new Error("Fixture：无法写入所选位置，请选择其他位置。");
        lastTransfer = { command, kind: args.kind || null, sessionIds: args.sessionIds || null };
        if (command === "export_codex_sessions") return { path: `/fixture/exports/${args.suggestedName}.${args.sessionIds.length === 1 ? "md" : "zip"}`, exportedSessions: args.sessionIds.length, failedSessions: 0, warnings: [] };
        if (command === "export_skills_mcp_archive") return { path: `/fixture/exports/${args.kind}.zip`, exportedSkills: args.kind === "skills" ? transferState.skills.length : 0, exportedMcp: args.kind === "mcp" ? transferState.mcpServers.length : 0 };
        return { importedSkills: 0, importedMcp: 0, message: "内容已存在，无需重复导入", state: clone(transferState) };
      }
      case "open_url": return undefined;
      default: throw new Error(`Fixture has no handler for command: ${command}`);
    }
  }

  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener(event, eventId) { eventListeners.delete(eventId); } };
  window.__TAURI_INTERNALS__ = {
    invoke,
    transformCallback(callback, once = false) {
      const id = nextCallback++;
      callbacks.set(id, (value) => {
        if (once) callbacks.delete(id);
        callback(value);
      });
      return id;
    },
    unregisterCallback(id) { callbacks.delete(id); },
    runCallback(id, value) { callbacks.get(id)?.(value); },
  };
  window.__CODEX_X_FIXTURE__ = {
    snapshot,
    loginSecondOfficialAccount,
    setCodexLanguageActual,
    setCodexLanguageMode,
    setUsageMode,
    setQuotaMode,
    setProviderOrderMode,
    getProviderOrder,
    completeProviderOrder: () => settle("providerOrder", false),
    failProviderOrder: () => settle("providerOrder", true),
    completeQuota: () => settle("quota", false),
    failQuota: () => settle("quota", true),
    completeSync: () => settle("sync", false),
    failSync: () => settle("sync", true),
    completeDetail: () => settle("detail", false),
    failDetail: () => settle("detail", true),
    reset() {
      sessionStorage.removeItem(storageKey);
      window.location.reload();
    },
  };

  document.addEventListener("DOMContentLoaded", () => {
    const panel = document.createElement("aside");
    panel.setAttribute("aria-label", "Fixture 控制面板");
    panel.style.cssText = "position:fixed;left:8px;bottom:8px;z-index:2147483647;width:224px;max-width:calc(100vw - 16px);max-height:calc(100vh - 16px);overflow:auto;box-sizing:border-box;background:#fff;color:#111;border:2px solid #1463d6;border-radius:8px;padding:8px;font:12px monospace;box-shadow:0 2px 8px #0003";
    const panelBody = document.createElement("details");
    panelBody.open = true;
    const label = document.createElement("summary");
    label.textContent = "Fixture：所有命令均为内存模拟";
    panelBody.append(label);
    panel.append(panelBody);
    const controls = document.createElement("div");
    controls.style.cssText = "display:flex;flex-wrap:wrap;gap:4px;margin-top:6px";
    if (updaterEnabled) {
      for (const [label, action] of [
        ["Fixture：验证更新", () => updateStage({ event: "Verifying" })],
        ["Fixture：准备退出", () => updateStage({ event: "Preparing" })],
        ["Fixture：启动安装", () => updateStage({ event: "Installing" })],
        ["Fixture：安装已交接", () => { updateStage({ event: "HandedOff" }); updateRequest?.resolve({ restartRequired: false }); updateRequest = null; render(); }],
        ["Fixture：更新准备失败", () => finishUpdate(true)],
        ["Fixture：更新完成待重启", () => finishUpdate(false)],
      ]) {
        const button = document.createElement("button"); button.type = "button"; button.textContent = label;
        button.addEventListener("click", action); controls.append(button);
      }
    }
    const transferSelect = document.createElement("select");
    transferSelect.setAttribute("aria-label", "Fixture：文件操作");
    for (const [value, label] of [["success", "文件操作成功"], ["cancel", "取消文件选择"], ["error", "文件操作失败"]]) {
      const option = document.createElement("option"); option.value = value; option.textContent = label; transferSelect.append(option);
    }
    transferSelect.addEventListener("change", () => { transferMode = transferSelect.value; render(); }); controls.append(transferSelect);
    for (const [text, action] of [
      ["Fixture：语言外部改为中文", () => setCodexLanguageActual(true)],
      ["Fixture：语言外部改为自动", () => setCodexLanguageActual(false)],
      ["Fixture：语言保存失败", () => setCodexLanguageMode("save-error")],
      ["Fixture：语言应用失败", () => setCodexLanguageMode("apply-error")],
      ["Fixture：语言恢复正常", () => setCodexLanguageMode("normal")],
    ]) {
      const button = document.createElement("button");
      button.type = "button"; button.textContent = text;
      button.style.cssText = "font:inherit;padding:5px;border:1px solid #aaa;border-radius:4px;background:#eee;color:#111;cursor:pointer";
      button.addEventListener("click", action); controls.append(button);
    }
    for (const [text, mode] of [["Fixture：配置正常", "healthy"], ["Fixture：缺失供应商配置", "missing"], ["Fixture：配置语法错误", "syntax"]]) {
      const button = document.createElement("button");
      button.type = "button"; button.textContent = text;
      button.addEventListener("click", () => { configHealthMode = mode; window.dispatchEvent(new Event("focus")); });
      controls.append(button);
    }
    for (const [text, kind, fail] of [
      ["Fixture：完成模板同步", "sync", false],
      ["Fixture：使模板同步失败", "sync", true],
      ["Fixture：完成模板详情", "detail", false],
      ["Fixture：使模板详情失败", "detail", true],
    ]) {
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = text;
      button.style.cssText = "font:inherit;padding:5px;border:1px solid #aaa;border-radius:4px;background:#eee;color:#111;cursor:pointer";
      button.addEventListener("click", () => settle(kind, fail));
      controls.append(button);
    }
    loginButton = document.createElement("button");
    loginButton.type = "button";
    loginButton.textContent = "Fixture：登录第二个官方账号";
    loginButton.style.cssText = "font:inherit;padding:5px;border:1px solid #aaa;border-radius:4px;background:#eee;color:#111;cursor:pointer";
    loginButton.addEventListener("click", loginSecondOfficialAccount);
    controls.append(loginButton);
    const quotaModeLabel = document.createElement("label");
    quotaModeLabel.textContent = "Fixture：额度模式 ";
    const quotaModeSelect = document.createElement("select");
    quotaModeSelect.setAttribute("aria-label", "Fixture：额度模式");
    quotaModeSelect.style.cssText = "font:inherit;max-width:200px;padding:4px;color:#111;background:#fff";
    for (const [value, text] of quotaModes) {
      const option = document.createElement("option");
      option.value = value;
      option.textContent = text;
      quotaModeSelect.append(option);
    }
    quotaModeSelect.value = quotaMode;
    quotaModeSelect.addEventListener("change", () => setQuotaMode(quotaModeSelect.value));
    quotaModeLabel.append(quotaModeSelect);
    controls.append(quotaModeLabel);
    const quotaCompleteButton = document.createElement("button");
    quotaCompleteButton.type = "button";
    quotaCompleteButton.textContent = "Fixture：完成额度请求";
    quotaCompleteButton.style.cssText = loginButton.style.cssText;
    quotaCompleteButton.addEventListener("click", () => settle("quota", false));
    controls.append(quotaCompleteButton);
    const resetModeLabel = document.createElement("label");
    resetModeLabel.textContent = "Fixture：重置次数模式 ";
    const resetModeSelect = document.createElement("select");
    resetModeSelect.setAttribute("aria-label", "Fixture：重置次数模式");
    resetModeSelect.style.cssText = quotaModeSelect.style.cssText;
    for (const [value, text] of [["available", "可用 3 次"], ["zero", "可用 0 次"], ["error", "查询失败"], ["pending", "延迟成功"], ["pending-error", "延迟失败"]]) {
      const option = document.createElement("option");
      option.value = value; option.textContent = text; resetModeSelect.append(option);
    }
    resetModeSelect.addEventListener("change", () => { resetCreditsMode = resetModeSelect.value; render(); });
    resetModeLabel.append(resetModeSelect); controls.append(resetModeLabel);
    const resetCompleteButton = document.createElement("button");
    resetCompleteButton.type = "button";
    resetCompleteButton.textContent = "Fixture：完成重置次数请求";
    resetCompleteButton.style.cssText = loginButton.style.cssText;
    resetCompleteButton.addEventListener("click", () => settle("resetCredits", false));
    controls.append(resetCompleteButton);
    const orderModeLabel = document.createElement("label");
    orderModeLabel.textContent = "Fixture：排序保存 ";
    const orderModeSelect = document.createElement("select");
    orderModeSelect.setAttribute("aria-label", "Fixture：排序保存");
    orderModeSelect.style.cssText = quotaModeSelect.style.cssText;
    for (const [value, text] of [["normal", "正常"], ["error", "保存失败"], ["pending", "延迟成功"], ["pending-error", "延迟失败"]]) {
      const option = document.createElement("option");
      option.value = value; option.textContent = text; orderModeSelect.append(option);
    }
    orderModeSelect.addEventListener("change", () => setProviderOrderMode(orderModeSelect.value));
    orderModeLabel.append(orderModeSelect); controls.append(orderModeLabel);
    for (const [text, failure] of [["Fixture：完成排序保存", false], ["Fixture：使排序保存失败", true]]) {
      const button = document.createElement("button");
      button.type = "button"; button.textContent = text; button.style.cssText = loginButton.style.cssText;
      button.addEventListener("click", () => settle("providerOrder", failure)); controls.append(button);
    }
    const pauseUsageLabel = document.createElement("label");
    const pauseUsageInput = document.createElement("input");
    pauseUsageInput.type = "checkbox";
    pauseUsageInput.addEventListener("change", () => { pauseUsage = pauseUsageInput.checked; });
    pauseUsageLabel.append(pauseUsageInput, "Fixture：暂停用量响应");
    controls.append(pauseUsageLabel);
    for (const [text, newest] of [["Fixture：完成最新用量请求", true], ["Fixture：完成旧用量请求", false]]) {
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = text;
      button.style.cssText = loginButton.style.cssText;
      button.addEventListener("click", () => {
        const request = newest ? pending.usage.pop() : pending.usage.shift();
        if (request) request.resolve(clone(request.result()));
        render();
      });
      controls.append(button);
    }
    for (const [text, mode] of [
      ["Fixture：用量示例数据", "sample"],
      ["Fixture：用量空数据", "empty"],
      ["Fixture：用量读取失败", "error"],
      ["Fixture：用量部分数据", "partial"],
    ]) {
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = text;
      button.style.cssText = loginButton.style.cssText;
      button.addEventListener("click", () => setUsageMode(mode));
      controls.append(button);
    }
    for (const [text, action] of [
      ["Fixture：精简的供应商配置", () => { state.configText = 'model_reasoning_effort = "high"\ndisable_response_storage = true\n' + toml(detectedProvider); providerBaseReadFailure = false; render(); }],
      ["Fixture：供应商底稿读取失败", () => { providerBaseReadFailure = true; render(); }],
      ["Fixture：供应商底稿读取正常", () => { providerBaseReadFailure = false; render(); }],
      ["Fixture：准备自动切换数据", seedFailover],
      ["Fixture：自动切换状态正常", () => { failoverMode = "normal"; render(); }],
      ["Fixture：自动切换状态失败", () => { failoverMode = "error"; render(); }],
      ["Fixture：自动切换保存失败", () => { failoverMode = "save-error"; render(); }],
      ["Fixture：自动切换冷却状态", () => { failoverMode = "cooling"; resetRoutingHealth.clear(); render(); }],
      ["Fixture：自动切换恢复状态", () => { failoverMode = "recovering"; resetRoutingHealth.clear(); render(); }],
      ["Fixture：路由外部设置变化", () => { failoverSettings.maxRetries = failoverSettings.maxRetries === 3 ? 4 : 3; render(); emitRoutingChanged(); }],
      ["Fixture：路由切换官方账号", () => { switchOfficial(defaultOfficialId); emitRoutingChanged(); }],
      ["Fixture：路由切换其他模型", () => { const provider = savedProviders.find((item) => item.id === "fixture-failover-other"); if (provider) switchProvider(provider); emitRoutingChanged(); }],
      ["Fixture：删除第一备用", () => { savedProviders = savedProviders.filter((provider) => provider.id !== failoverSettings.providerIds[0]); render(); }],
    ]) {
      const button = document.createElement("button");
      button.type = "button"; button.textContent = text; button.style.cssText = loginButton.style.cssText;
      button.addEventListener("click", action); controls.append(button);
    }
    const resetButton = document.createElement("button");
    resetButton.type = "button";
    resetButton.textContent = "Fixture：重置测试数据";
    resetButton.style.cssText = loginButton.style.cssText;
    resetButton.addEventListener("click", () => window.__CODEX_X_FIXTURE__.reset());
    controls.append(resetButton);
    panelBody.append(controls);
    const diagnostics = document.createElement("details");
    diagnostics.open = false;
    const summary = document.createElement("summary");
    summary.textContent = "Fixture：命令次数与供应商切换记录";
    output = document.createElement("pre");
    output.id = "fixture-command-log";
    output.setAttribute("aria-label", "Fixture 命令记录");
    output.style.cssText = "white-space:pre-wrap;max-height:135px;overflow:auto;margin:6px 0 0";
    diagnostics.append(summary, output);
    panelBody.append(diagnostics);
    document.body.append(panel);
    render();
  });
})();
