use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManagedMcpServer {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) transport: String,
    pub(crate) enabled: bool,
    pub(crate) source: String,
    pub(crate) summary: String,
    pub(crate) command: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) config_json: Value,
    pub(crate) note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManagedSkill {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) directory: String,
    pub(crate) enabled: bool,
    pub(crate) source: String,
    pub(crate) path: String,
    pub(crate) content_hash: Option<String>,
    pub(crate) update_status: String,
    pub(crate) note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillMcpNoteUpdate {
    pub(crate) item_kind: String,
    pub(crate) item_id: String,
    pub(crate) note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillsMcpState {
    pub(crate) codex_dir: String,
    pub(crate) codex_skills_dir: String,
    pub(crate) disabled_skills_dir: String,
    pub(crate) skills: Vec<ManagedSkill>,
    pub(crate) mcp_servers: Vec<ManagedMcpServer>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillsMcpActionResult {
    pub(crate) imported_skills: usize,
    pub(crate) imported_mcp: usize,
    pub(crate) message: String,
    pub(crate) state: SkillsMcpState,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillsMcpImportPreview {
    pub(crate) skills: Vec<ManagedSkill>,
    pub(crate) mcp_servers: Vec<ManagedMcpServer>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct CcSwitchSkillMeta {
    pub(super) repo_owner: String,
    pub(super) repo_name: String,
    pub(super) repo_branch: String,
    pub(super) content_hash: Option<String>,
}
