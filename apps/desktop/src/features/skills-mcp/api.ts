import { invokeCommand as invoke } from "../../shared/api/tauri";
import type {
  SkillMcpNoteUpdate,
  SkillsMcpActionResult,
  SkillsMcpImportPreview,
  SkillsMcpState,
} from "../../types";

type ConfigDir = string | null;

export function getSkillsMcpState(configDir: ConfigDir) {
  return invoke<SkillsMcpState>("get_skills_mcp_state", { configDir });
}

export function previewExistingSkillsMcp(configDir: ConfigDir) {
  return invoke<SkillsMcpImportPreview>("preview_existing_skills_mcp", { configDir });
}

export function importExistingSkillsMcp(configDir: ConfigDir) {
  return invoke<SkillsMcpActionResult>("import_existing_skills_mcp", { configDir });
}

export function checkSkillUpdates(configDir: ConfigDir) {
  return invoke<SkillsMcpState>("check_skill_updates", { configDir });
}

export function toggleCodexSkill(configDir: ConfigDir, id: string, enabled: boolean) {
  return invoke<SkillsMcpState>("toggle_codex_skill", { configDir, id, enabled });
}

export function toggleCodexMcp(configDir: ConfigDir, id: string, enabled: boolean) {
  return invoke<SkillsMcpState>("toggle_codex_mcp", { configDir, id, enabled });
}

export function updateSkillsMcpNote(
  configDir: ConfigDir,
  itemKind: "skill" | "mcp",
  itemId: string,
  note: string,
) {
  return invoke<SkillMcpNoteUpdate>("update_skills_mcp_note", {
    configDir,
    itemKind,
    itemId,
    note,
  });
}

export function installSkillZip(configDir: ConfigDir, fileName: string, bytes: number[]) {
  return invoke<SkillsMcpActionResult>("install_skill_zip", { configDir, fileName, bytes });
}
