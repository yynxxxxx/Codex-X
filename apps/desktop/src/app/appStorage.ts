import type { AppTheme } from "../components/AppShell";
import type { Lang, PromptInjectionMode } from "../types";

const LANG_KEY = "codexx.lang";
const THEME_KEY = "codexx.theme";
const STARTUP_WIZARD_SEEN_KEY = "codexx.startupWizardSeen";
const ACTIVE_PROVIDER_KEY = "codexx.activeProviderId";
const PROMPT_INJECTION_MODE_KEY = "codexx.promptInjectionMode";
const AUTO_RESTART_CODEX_KEY = "codexx.autoRestartCodexAfterConfigChange";

export function getLanguage(): Lang {
  return localStorage.getItem(LANG_KEY) === "en" ? "en" : "zh";
}

export function setLanguage(lang: Lang) {
  localStorage.setItem(LANG_KEY, lang);
}

export function getTheme(): AppTheme {
  return localStorage.getItem(THEME_KEY) === "dark" ? "dark" : "light";
}

export function setTheme(theme: AppTheme) {
  localStorage.setItem(THEME_KEY, theme);
}

export function getPromptInjectionMode(): PromptInjectionMode {
  return localStorage.getItem(PROMPT_INJECTION_MODE_KEY) === "replace" ? "replace" : "append";
}

export function setPromptInjectionMode(mode: PromptInjectionMode) {
  localStorage.setItem(PROMPT_INJECTION_MODE_KEY, mode);
}

export function getActiveProviderId(): string {
  return localStorage.getItem(ACTIVE_PROVIDER_KEY) || "";
}

export function setActiveProviderId(id: string) {
  if (id) localStorage.setItem(ACTIVE_PROVIDER_KEY, id);
  else localStorage.removeItem(ACTIVE_PROVIDER_KEY);
}

export function getAutoRestartCodex(): boolean {
  return localStorage.getItem(AUTO_RESTART_CODEX_KEY) === "1";
}

export function setAutoRestartCodex(enabled: boolean) {
  localStorage.setItem(AUTO_RESTART_CODEX_KEY, enabled ? "1" : "0");
}

export function hasSeenStartupWizard(): boolean {
  return localStorage.getItem(STARTUP_WIZARD_SEEN_KEY) === "1";
}

export function markStartupWizardSeen() {
  localStorage.setItem(STARTUP_WIZARD_SEEN_KEY, "1");
}

export function clearStartupWizardSeen() {
  localStorage.removeItem(STARTUP_WIZARD_SEEN_KEY);
}
