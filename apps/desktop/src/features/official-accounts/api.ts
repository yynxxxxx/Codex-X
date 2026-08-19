import { invokeCommand as invoke } from "../../shared/api/tauri";
import type {
  ActionResult,
  OfficialAccountDraft,
  OfficialAccountSummary,
  OfficialAuthCandidate,
  OfficialConfigDraft,
} from "../../types";

type ConfigDir = string | null;

type OfficialConfigInput = {
  configDir: ConfigDir;
  model: string | null;
  authJson: string | null;
  configText: string | null;
};

type OfficialAccountUpdateInput = OfficialConfigInput & {
  accountId: string;
  name: string;
};

export function listOfficialAccounts(configDir: ConfigDir) {
  return invoke<OfficialAccountSummary[]>("list_official_accounts", { configDir });
}

export function getOfficialAccount(configDir: ConfigDir, accountId: string) {
  return invoke<OfficialAccountDraft>("get_official_account", { configDir, accountId });
}

export function getOfficialConfigDraft(configDir: ConfigDir) {
  return invoke<OfficialConfigDraft | null>("get_official_config_draft", { configDir });
}

export function captureCurrentOfficialAccount(configDir: ConfigDir, name: string) {
  return invoke<ActionResult>("capture_current_official_account", { configDir, name });
}

export function updateOfficialAccount(input: OfficialAccountUpdateInput) {
  return invoke<ActionResult>("update_official_account", { input });
}

export function switchOfficialAccount(configDir: ConfigDir, accountId: string) {
  return invoke<ActionResult>("switch_official_account", { configDir, accountId });
}

export function deleteOfficialAccount(configDir: ConfigDir, accountId: string) {
  return invoke<ActionResult>("delete_official_account", { configDir, accountId });
}

export function prepareNewOfficialAccount(configDir: ConfigDir) {
  return invoke<ActionResult>("prepare_new_official_account", { configDir });
}

export function switchOfficialProvider(configDir: ConfigDir) {
  return invoke<ActionResult>("switch_official_provider", { configDir });
}

export function restoreOfficialProvider(configDir: ConfigDir) {
  return invoke<ActionResult>("restore_official_provider", { configDir });
}

export function resetOfficialProvider(input: OfficialConfigInput) {
  return invoke<ActionResult>("reset_official_provider", { input });
}

export function saveOfficialConfig(input: OfficialConfigInput) {
  return invoke<ActionResult>("save_official_config", { input });
}

export function readCcSwitchOfficialAuth() {
  return invoke<OfficialAuthCandidate | null>("read_ccswitch_official_auth", { dbPath: null });
}
