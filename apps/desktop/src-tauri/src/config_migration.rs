use crate::backups::create_backup;
use crate::config_path;
use crate::error::Result;
use crate::file_io::{parse_toml_document, read_to_string_if_exists, write_text};
use std::path::Path;
use toml_edit::{DocumentMut, Item, Table};

const INSTRUCTION_KEY: &str = "model_instructions_file";
const MODEL_AVAILABILITY_NUX_KEY: &str = "model_availability_nux";

fn markdown_path(item: Option<&Item>) -> bool {
    item.and_then(|item| item.as_str())
        .map(str::trim)
        .is_some_and(|path| !path.is_empty() && path.to_ascii_lowercase().ends_with(".md"))
}

fn tui_table(doc: &DocumentMut) -> Option<&Table> {
    doc.get("tui").and_then(|item| item.as_table())
}

fn remove_markdown_path(table: &mut Table, key: &str) -> Option<Item> {
    if markdown_path(table.get(key)) {
        table.remove(key)
    } else {
        None
    }
}

/// Repairs prompt paths appended by older Codex-X versions after a `[tui]`
/// header. Only the exact legacy keys with Markdown path values are touched.
pub(crate) fn migrate_legacy_prompt_config(codex_dir: &Path) -> Result<bool> {
    let cfg = config_path(codex_dir);
    let text = read_to_string_if_exists(&cfg)?;
    if text.trim().is_empty() {
        return Ok(false);
    }

    let mut doc = parse_toml_document(&cfg, &text)?;
    let nested_instruction = tui_table(&doc)
        .and_then(|tui| tui.get(MODEL_AVAILABILITY_NUX_KEY))
        .and_then(|item| item.as_table())
        .is_some_and(|nux| markdown_path(nux.get(INSTRUCTION_KEY)));
    let tui_instruction =
        tui_table(&doc).is_some_and(|tui| markdown_path(tui.get(INSTRUCTION_KEY)));
    let nux_as_instruction =
        tui_table(&doc).is_some_and(|tui| markdown_path(tui.get(MODEL_AVAILABILITY_NUX_KEY)));

    if !nested_instruction && !tui_instruction && !nux_as_instruction {
        return Ok(false);
    }

    let root_instruction_exists = doc.as_table().contains_key(INSTRUCTION_KEY);
    create_backup(codex_dir, "migrate-legacy-prompt-config")?;

    let removed_nested = doc
        .get_mut("tui")
        .and_then(|item| item.as_table_mut())
        .and_then(|tui| tui.get_mut(MODEL_AVAILABILITY_NUX_KEY))
        .and_then(|item| item.as_table_mut())
        .and_then(|nux| remove_markdown_path(nux, INSTRUCTION_KEY));
    let removed_tui = doc
        .get_mut("tui")
        .and_then(|item| item.as_table_mut())
        .and_then(|tui| remove_markdown_path(tui, INSTRUCTION_KEY));
    let removed_nux_value = doc
        .get_mut("tui")
        .and_then(|item| item.as_table_mut())
        .and_then(|tui| remove_markdown_path(tui, MODEL_AVAILABILITY_NUX_KEY));

    if !root_instruction_exists {
        let instruction = removed_nested
            .or(removed_tui)
            .or(removed_nux_value)
            .expect("a detected legacy prompt path must still be removable");
        doc.as_table_mut().insert(INSTRUCTION_KEY, instruction);
    }

    write_text(&cfg, &doc.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backups::action_backup_root;
    use crate::file_io::write_text;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_codex_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "codex-x-config-migration-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp Codex directory");
        dir
    }

    fn read_doc(codex_dir: &Path) -> DocumentMut {
        fs::read_to_string(config_path(codex_dir))
            .expect("read migrated config")
            .parse()
            .expect("parse migrated config")
    }

    fn backup_count(codex_dir: &Path) -> usize {
        let root = action_backup_root(codex_dir).expect("resolve backup root");
        if !root.exists() {
            return 0;
        }
        fs::read_dir(root).expect("read backup root").count()
    }

    fn only_backup_config(codex_dir: &Path) -> String {
        let root = action_backup_root(codex_dir).expect("resolve backup root");
        let entries = fs::read_dir(root)
            .expect("read backup root")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("read backup entries");
        assert_eq!(entries.len(), 1);
        fs::read_to_string(entries[0].path().join("config.toml")).expect("read backed-up config")
    }

    #[test]
    fn migrates_instruction_nested_under_model_availability_nux() {
        let codex_dir = temp_codex_dir("nested-nux");
        let original = r#"model = "gpt-5.6"

[tui.model_availability_nux]
# Keep valid model counters and their comments.
"gpt-5.5" = 4
model_instructions_file = "./legacy-prompt.md" # Keep the prompt note too.
"gpt-5.6" = 2

[tui.notifications]
enabled = true
"#;
        write_text(&config_path(&codex_dir), original).expect("write legacy config");

        assert!(migrate_legacy_prompt_config(&codex_dir).expect("migrate config"));

        let migrated_text = fs::read_to_string(config_path(&codex_dir)).expect("read config");
        let doc = read_doc(&codex_dir);
        assert_eq!(
            doc.get(INSTRUCTION_KEY).and_then(|item| item.as_str()),
            Some("./legacy-prompt.md")
        );
        let nux = doc["tui"][MODEL_AVAILABILITY_NUX_KEY]
            .as_table()
            .expect("keep model availability table");
        assert!(!nux.contains_key(INSTRUCTION_KEY));
        assert_eq!(
            nux.get("gpt-5.5").and_then(|item| item.as_integer()),
            Some(4)
        );
        assert_eq!(
            nux.get("gpt-5.6").and_then(|item| item.as_integer()),
            Some(2)
        );
        assert!(migrated_text.contains("# Keep valid model counters and their comments."));
        assert!(migrated_text.contains("# Keep the prompt note too."));
        assert_eq!(doc["tui"]["notifications"]["enabled"].as_bool(), Some(true));
        assert_eq!(backup_count(&codex_dir), 1);
        assert_eq!(only_backup_config(&codex_dir), original);
        fs::remove_dir_all(codex_dir).expect("remove temp directory");
    }

    #[test]
    fn migrates_instruction_directly_under_tui() {
        let codex_dir = temp_codex_dir("direct-tui");
        write_text(
            &config_path(&codex_dir),
            "[tui]\nnotifications = true\nmodel_instructions_file = \"./direct.md\"\n",
        )
        .expect("write legacy config");

        assert!(migrate_legacy_prompt_config(&codex_dir).expect("migrate config"));

        let doc = read_doc(&codex_dir);
        assert_eq!(
            doc.get(INSTRUCTION_KEY).and_then(|item| item.as_str()),
            Some("./direct.md")
        );
        assert!(doc["tui"].as_table().is_some_and(|tui| {
            !tui.contains_key(INSTRUCTION_KEY)
                && tui.get("notifications").and_then(|item| item.as_bool()) == Some(true)
        }));
        fs::remove_dir_all(codex_dir).expect("remove temp directory");
    }

    #[test]
    fn migrates_markdown_path_stored_as_tui_model_availability_nux() {
        let codex_dir = temp_codex_dir("nux-as-path");
        write_text(
            &config_path(&codex_dir),
            "[tui]\nmodel_availability_nux = \"./gpt-5.6-sol-unrestricted.md\"\n",
        )
        .expect("write legacy config");

        assert!(migrate_legacy_prompt_config(&codex_dir).expect("migrate config"));

        let doc = read_doc(&codex_dir);
        assert_eq!(
            doc.get(INSTRUCTION_KEY).and_then(|item| item.as_str()),
            Some("./gpt-5.6-sol-unrestricted.md")
        );
        assert!(doc["tui"]
            .as_table()
            .is_some_and(|tui| { !tui.contains_key(MODEL_AVAILABILITY_NUX_KEY) }));
        fs::remove_dir_all(codex_dir).expect("remove temp directory");
    }

    #[test]
    fn root_instruction_wins_over_legacy_nested_value() {
        let codex_dir = temp_codex_dir("root-wins");
        write_text(
            &config_path(&codex_dir),
            r#"model_instructions_file = "./current.md"

[tui.model_availability_nux]
"gpt-5.5" = 4
model_instructions_file = "./stale.md"
"#,
        )
        .expect("write conflicting config");

        assert!(migrate_legacy_prompt_config(&codex_dir).expect("migrate config"));

        let doc = read_doc(&codex_dir);
        assert_eq!(
            doc.get(INSTRUCTION_KEY).and_then(|item| item.as_str()),
            Some("./current.md")
        );
        assert!(!doc["tui"][MODEL_AVAILABILITY_NUX_KEY]
            .as_table()
            .expect("keep nux table")
            .contains_key(INSTRUCTION_KEY));
        fs::remove_dir_all(codex_dir).expect("remove temp directory");
    }

    #[test]
    fn leaves_legal_tui_values_unchanged_without_backup() {
        let codex_dir = temp_codex_dir("legal-tui");
        let original = "[tui]\nmodel_availability_nux = 4\nnotifications = true\n";
        write_text(&config_path(&codex_dir), original).expect("write legal config");

        assert!(!migrate_legacy_prompt_config(&codex_dir).expect("inspect config"));

        assert_eq!(
            fs::read_to_string(config_path(&codex_dir)).expect("read unchanged config"),
            original
        );
        assert_eq!(backup_count(&codex_dir), 0);
        fs::remove_dir_all(codex_dir).expect("remove temp directory");
    }

    #[test]
    fn migration_is_idempotent_and_backs_up_only_once() {
        let codex_dir = temp_codex_dir("idempotent");
        write_text(
            &config_path(&codex_dir),
            "[tui]\nmodel_instructions_file = \"./once.md\"\n",
        )
        .expect("write legacy config");

        assert!(migrate_legacy_prompt_config(&codex_dir).expect("first migration"));
        let after_first = fs::read_to_string(config_path(&codex_dir)).expect("read first result");
        assert!(!migrate_legacy_prompt_config(&codex_dir).expect("second migration"));

        assert_eq!(
            fs::read_to_string(config_path(&codex_dir)).expect("read second result"),
            after_first
        );
        assert_eq!(backup_count(&codex_dir), 1);
        fs::remove_dir_all(codex_dir).expect("remove temp directory");
    }

    #[test]
    fn loading_codex_state_runs_the_migration() {
        let codex_dir = temp_codex_dir("state-load");
        write_text(
            &config_path(&codex_dir),
            "[tui]\nmodel_availability_nux = \"./loaded.md\"\n",
        )
        .expect("write legacy config");

        crate::state::build_state(codex_dir.clone()).expect("load Codex state");

        let doc = read_doc(&codex_dir);
        assert_eq!(
            doc.get(INSTRUCTION_KEY).and_then(|item| item.as_str()),
            Some("./loaded.md")
        );
        assert_eq!(backup_count(&codex_dir), 1);
        fs::remove_dir_all(codex_dir).expect("remove temp directory");
    }
}
