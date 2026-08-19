import { invokeCommand as invoke } from "../../shared/api/tauri";
import type {
  ActionResult,
  BuiltinPromptDetail,
  BuiltinPromptStatus,
  PromptInjectionMode,
  SavedPrompt,
} from "../../types";

type ConfigDir = string | null;

export function listSavedPrompts() {
  return invoke<SavedPrompt[]>("list_saved_prompts");
}

export function getBuiltinPromptStatus() {
  return invoke<BuiltinPromptStatus[]>("get_builtin_prompt_status");
}

export function getBuiltinPromptDetail(templateId: string) {
  return invoke<BuiltinPromptDetail>("get_builtin_prompt_detail", { templateId });
}

export function saveBuiltinPromptOverride(templateId: string, content: string) {
  return invoke<BuiltinPromptDetail>("save_builtin_prompt_override", { templateId, content });
}

export function refreshBuiltinPrompts(configDir: ConfigDir) {
  return invoke<BuiltinPromptStatus[]>("refresh_builtin_prompts", { configDir });
}

export function savePrompt(prompt: SavedPrompt) {
  return invoke<SavedPrompt>("save_prompt", { prompt });
}

export function deleteSavedPrompt(id: string) {
  return invoke<void>("delete_saved_prompt", { id });
}

export function enableSavedPrompt(configDir: ConfigDir, id: string, injectionMode: PromptInjectionMode) {
  return invoke<ActionResult>("enable_saved_prompt", { configDir, id, injectionMode });
}

export function enableInstructionTemplate(configDir: ConfigDir, templateId: string, injectionMode: PromptInjectionMode) {
  return invoke<ActionResult>("enable_instruction_template", { configDir, templateId, injectionMode });
}

export function disableInstruction(configDir: ConfigDir) {
  return invoke<ActionResult>("disable_instruction", { configDir, deleteFile: true });
}

export function disableExternalInstruction(configDir: ConfigDir) {
  return invoke<ActionResult>("disable_external_instruction", { configDir });
}
