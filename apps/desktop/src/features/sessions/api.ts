import { invokeCommand } from "../../shared/api/tauri";
import type { SessionDeleteResult, SessionSyncResult } from "../../types";
import type { SessionSyncStatus } from "./types";

type ConfigDir = string | null;

export function getSessionSyncStatus(configDir: ConfigDir) {
  return invokeCommand<SessionSyncStatus>("get_session_sync_status", {
    configDir,
    targetProvider: null,
  });
}

export function syncSessionsProvider(configDir: ConfigDir) {
  return invokeCommand<SessionSyncResult>("sync_sessions_provider", {
    configDir,
    targetProvider: null,
  });
}

export function deleteCodexSessions(configDir: ConfigDir, sessionIds: string[]) {
  return invokeCommand<SessionDeleteResult>("delete_codex_sessions", {
    input: { configDir, sessionIds },
  });
}
