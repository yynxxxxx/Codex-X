use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SavedPrompt {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) filename: String,
    pub(crate) content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BuiltinPromptStatus {
    pub(crate) id: String,
    pub(crate) filename: String,
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) badge: String,
    pub(crate) source_url: String,
    pub(crate) cached: bool,
    pub(crate) updated: bool,
    pub(crate) content_source: String,
    pub(crate) sync_issue: Option<String>,
    pub(crate) checked_at: Option<String>,
    pub(crate) message: String,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BundledPromptMeta {
    pub(super) id: &'static str,
    pub(super) filename: &'static str,
    pub(super) title: &'static str,
    pub(super) subtitle: &'static str,
    pub(super) badge: &'static str,
    pub(super) content: &'static str,
}

#[derive(Debug, Clone)]
pub(crate) struct CachedBuiltinPrompt {
    pub(crate) id: String,
    pub(crate) filename: String,
    pub(crate) source_url: String,
    pub(crate) content: String,
    pub(crate) checked_at: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GithubContentEntry {
    pub(crate) name: String,
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) download_url: Option<String>,
}
