mod ccswitch;
mod connection;
mod live;
pub(crate) mod model_catalog;
mod official_auth;
pub(crate) mod official_profiles;
pub(crate) mod quota;
mod selection;
mod store;
pub(crate) mod transport;

use crate::error::Result;
use rusqlite::Connection;

#[cfg(test)]
pub(crate) use ccswitch::{
    build_ccswitch_codex_provider, codex_sections_from_config, is_official_ccswitch_row,
    read_ccswitch_codex_rows, CcSwitchCodexRow,
};
pub(crate) use ccswitch::{
    import_ccswitch_codex_providers_inner, read_ccswitch_official_auth_inner, ImportResult,
    OfficialAuthCandidate,
};
#[cfg(test)]
pub(crate) use connection::provider_status_result;
pub(crate) use connection::{
    fetch_provider_models_inner, fetch_provider_models_with_protocol_inner,
    test_provider_connection_inner, test_provider_connection_with_protocol_inner,
    ProviderConnectionResult, ProviderModelsResult,
};
pub(crate) use live::{
    activate_saved_provider_inner, build_provider_toml_draft_inner,
    build_provider_toml_draft_with_origin_inner, delete_saved_provider_inner,
    get_provider_config_base_inner, save_active_provider_inner,
    save_active_provider_with_common_config_inner, save_official_config_inner,
    save_provider_toml_config_inner, switch_provider_inner, OfficialConfigInput, ProviderInput,
    ProviderTomlInput,
};
pub(crate) use live::{detected_live_custom_provider, replacement_write_order, LiveWriteOrder};
#[cfg(test)]
pub(crate) use live::{
    reset_official_provider_inner, restore_official_provider_inner,
    save_provider_toml_config_with_pre_persist, switch_official_provider_inner,
    switch_official_provider_with_pre_persist, switch_provider_with_pre_persist,
};
#[cfg(test)]
pub(crate) use official_auth::{
    capture_live_chatgpt_config, get_official_config_draft_inner, official_snapshot_path_for_test,
};
pub(crate) use official_auth::{document_is_official, official_auth_available};
pub(crate) use selection::{
    clear_active_provider_on_connection, clear_provider_selections_on_connection,
    reconcile_active_provider_on_connection, remember_active_provider_on_connection,
};
#[cfg(test)]
pub(crate) use store::{
    canonical_provider_base_url, provider_by_id_on_connection, provider_identity,
    save_manual_provider_on_connection, upsert_provider_on_connection, ProviderUpsertMode,
};
pub(crate) use store::{
    consolidate_legacy_provider_duplicates_on_connection, custom_provider_id,
    delete_provider_inner, duplicate_provider_inner, experimental_bearer_token_from_doc,
    get_provider_order_inner, is_placeholder_provider, list_saved_providers_inner,
    list_saved_providers_on_connection, matching_saved_provider_ids_for_live,
    matching_saved_provider_ids_for_live_on_connection, normalize_saved_provider,
    normalize_saved_provider_for_save, provider_template_from_document, provider_with_editor_auth,
    read_provider_headers_inner, reserved_codex_provider_id, rollback_provider_store_inner,
    save_detected_provider_with_rollback_inner, save_provider_inner, save_provider_order_inner,
    save_provider_with_rollback_inner, strip_provider_bearer_tokens, update_provider_headers_inner,
    upsert_ccswitch_provider_on_connection, DuplicateProviderResult, ProviderHeader,
    ProviderStoreRollback, ProviderUpsertKind, SavedProvider,
};

pub(crate) fn open_store() -> Result<Connection> {
    crate::app_db::open()
}
