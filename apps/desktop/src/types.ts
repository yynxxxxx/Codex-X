import type { SessionSyncStatus } from "./pages/SessionManagementPage";

export type Lang = "zh" | "en";
export type ProviderMode = "list" | "form" | "official";
export type InstructionMode = "list" | "form";
export type PromptInjectionMode = "append" | "replace";

export type InstructionTemplate = {
  id: string;
  filename: string;
  title: string;
  subtitle: string;
  badge: string;
};

export type ProviderSummary = {
  id: string;
  name?: string;
  baseUrl?: string;
  wireApi?: string;
  requiresOpenaiAuth?: boolean;
  isCurrent: boolean;
};

export type SavedProvider = {
  id: string;
  providerName: string;
  baseUrl: string;
  model: string;
  apiKey?: string;
  tomlConfig?: string;
  wireApi: string;
  requiresOpenaiAuth: boolean;
};

export type SavedPrompt = {
  id: string;
  title: string;
  filename: string;
  content: string;
};

export type BuiltinPromptStatus = {
  id: string;
  filename: string;
  title: string;
  subtitle: string;
  badge: string;
  sourceUrl: string;
  cached: boolean;
  updated: boolean;
  contentSource: string;
  syncIssue?: "catalog" | "content" | null;
  checkedAt?: string | null;
  message: string;
};

export type BackupEntry = {
  id: string;
  action: string;
  createdAt: string;
  path: string;
  hadConfig: boolean;
  hadAuth: boolean;
  hadAgents?: boolean;
};

export type CodexState = {
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
  instructionInjectionMode?: PromptInjectionMode;
  instructionTemplateKey?: string;
  agentsPath: string;
  activeSavedProviderId?: string;
  providers: ProviderSummary[];
  configText: string;
  authPreview?: unknown;
  authText: string;
  lastBackup?: BackupEntry;
};

export type ActionResult = {
  ok: boolean;
  message: string;
  backupId?: string;
  state: CodexState;
};

export type ImportResult = {
  imported: number;
  added: number;
  updated: number;
  merged: number;
  skipped: number;
  warnings: string[];
  providers: SavedProvider[];
};

export type AboutInfo = {
  appVersion: string;
  codexVersion?: string;
  codexDir: string;
  projectUrl: string;
  githubRepo: string;
  nativeUpdaterSupported: boolean;
};

export type ReleaseInfo = {
  status: "idle" | "checking" | "ok" | "error";
  latestVersion?: string;
  htmlUrl?: string;
  hasUpdate?: boolean;
  updateMethod?: "native" | "download";
};

export type AppUpdateInfo = {
  latestVersion: string;
  htmlUrl: string;
  hasUpdate: boolean;
};

export type ProviderConnectionResult = {
  ok: boolean;
  status?: number | null;
  message: string;
  durationMs: number;
};

export type ProviderModel = {
  id: string;
  created?: number | null;
};

export type ProviderModelsResult = {
  models: ProviderModel[];
  status: number;
  durationMs: number;
};

export type SessionSyncResult = {
  status: SessionSyncStatus;
  updatedRollouts: number;
  updatedThreads: number;
  backupDir: string;
};

export type SessionDeleteResult = {
  status: SessionSyncStatus;
  requestedSessions: number;
  deletedSessions: number;
  failedSessions: number;
  failureMessage?: string | null;
  deletedThreadRows: number;
  deletedRolloutFiles: number;
  deletedRelatedRows: number;
};

export type ManagedSkill = {
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

export type ManagedMcpServer = {
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

export type SkillsMcpState = {
  codexDir: string;
  codexSkillsDir: string;
  disabledSkillsDir: string;
  skills: ManagedSkill[];
  mcpServers: ManagedMcpServer[];
  warnings: string[];
};

export type SkillsMcpActionResult = {
  importedSkills: number;
  importedMcp: number;
  message: string;
  state: SkillsMcpState;
};

export type SkillsMcpImportPreview = {
  skills: ManagedSkill[];
  mcpServers: ManagedMcpServer[];
  warnings: string[];
};

export type DiagnosticItem = {
  key: string;
  label: string;
  path?: string | null;
  status: "ok" | "missing" | "manual" | string;
  message: string;
};

export type StartupDiagnostics = {
  codexDir: string;
  needsManualSelect: boolean;
  summary: string;
  items: DiagnosticItem[];
};

export type SkinThemeColors = {
  background: string;
  panel: string;
  panelAlt: string;
  accent: string;
  accentAlt: string;
  secondary: string;
  highlight: string;
  text: string;
  muted: string;
  line: string;
};

export type SkinThemeSummary = {
  id: string;
  name: string;
  tagline: string;
  quote: string;
  image: string;
  source: "builtin" | "imported" | string;
  enabled: boolean;
  directory: string;
  colors: SkinThemeColors;
};

export type SkinCenterState = {
  skinsDir: string;
  currentThemeId?: string | null;
  currentThemePath?: string | null;
  themes: SkinThemeSummary[];
};

export type SkinActionResult = {
  message: string;
  state: SkinCenterState;
};

export type SkinExportResult = {
  path: string;
  message: string;
};
