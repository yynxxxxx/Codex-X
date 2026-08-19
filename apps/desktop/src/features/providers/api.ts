import { invokeCommand as invoke } from "../../shared/api/tauri";
import type {
  ActionResult,
  ImportResult,
  ProviderConnectionResult,
  ProviderModelsResult,
  SavedProvider,
} from "../../types";

type ConfigDir = string | null;

type ProviderInput = {
  configDir: ConfigDir;
  providerId: string;
  providerName: string;
  baseUrl: string;
  model: string;
  apiKey: string;
  wireApi: string;
  requiresOpenaiAuth: boolean;
};

export function listSavedProviders() {
  return invoke<SavedProvider[]>("list_saved_providers");
}

export function buildProviderTomlDraft(provider: SavedProvider, configDir: ConfigDir) {
  return invoke<string>("build_provider_toml_draft", { provider, configDir });
}

export function saveProvider(provider: SavedProvider) {
  return invoke<SavedProvider>("save_provider", { provider });
}

export function saveActiveProvider(provider: SavedProvider, configDir: ConfigDir) {
  return invoke<ActionResult>("save_active_provider", { provider, configDir });
}

export function saveProviderTomlConfig(configDir: ConfigDir, configText: string, apiKey: string) {
  return invoke<ActionResult>("save_provider_toml_config", {
    input: { configDir, configText, apiKey },
  });
}

export function switchProvider(input: ProviderInput) {
  return invoke<ActionResult>("switch_provider", { input });
}

export function deleteSavedProvider(id: string, configDir: ConfigDir) {
  return invoke<void>("delete_saved_provider", { id, configDir });
}

export function testProviderConnection(baseUrl: string, apiKey: string | null) {
  return invoke<ProviderConnectionResult>("test_provider_connection", { baseUrl, apiKey });
}

export function fetchProviderModels(baseUrl: string, apiKey: string) {
  return invoke<ProviderModelsResult>("fetch_provider_models", { baseUrl, apiKey });
}

export function importCcSwitchProviders() {
  return invoke<ImportResult>("import_ccswitch_codex_providers", { dbPath: null });
}
