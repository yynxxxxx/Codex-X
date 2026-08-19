import { invokeCommand as invoke } from "../../shared/api/tauri";
import type {
  AboutInfo,
  CodexRestartResult,
  CodexState,
  StartupDiagnostics,
} from "../../types";

export function getCodexState(configDir: string | null) {
  return invoke<CodexState>("get_codex_state", { configDir });
}

export function getStartupDiagnostics(configDir: string | null) {
  return invoke<StartupDiagnostics>("get_startup_diagnostics", { configDir });
}

export function getAboutInfo(configDir: string | null) {
  return invoke<AboutInfo>("get_about_info", { configDir });
}

export function restartCodexApp() {
  return invoke<CodexRestartResult>("restart_codex_app");
}

export function openUrl(url: string) {
  return invoke<void>("open_url", { url });
}
