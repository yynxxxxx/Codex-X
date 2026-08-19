use super::types::SkillMcpNoteUpdate;
use crate::app_db;
use crate::error::{CodexxError, Result};
use crate::{now_rfc3339, resolve_codex_dir};
use rusqlite::params;
use std::collections::HashMap;
use std::path::Path;

pub(super) const SKILL_NOTE_KIND: &str = "skill";
pub(super) const MCP_NOTE_KIND: &str = "mcp";
const MAX_NOTE_CHARS: usize = 1000;

fn validate_kind(item_kind: &str) -> Result<&str> {
    match item_kind.trim() {
        SKILL_NOTE_KIND => Ok(SKILL_NOTE_KIND),
        MCP_NOTE_KIND => Ok(MCP_NOTE_KIND),
        _ => Err(CodexxError::Config(
            "备注类型必须是 skill 或 mcp".to_string(),
        )),
    }
}

fn validate_item_id(item_id: &str) -> Result<&str> {
    let item_id = item_id.trim();
    if item_id.is_empty() {
        return Err(CodexxError::Config("备注项目 ID 不能为空".to_string()));
    }
    Ok(item_id)
}

fn codex_dir_key(codex_dir: &Path) -> String {
    let key = codex_dir
        .canonicalize()
        .unwrap_or_else(|_| codex_dir.to_path_buf())
        .to_string_lossy()
        .to_string();
    if cfg!(target_os = "windows") {
        key.to_ascii_lowercase()
    } else {
        key
    }
}

pub(super) fn load_notes(codex_dir: &Path) -> Result<HashMap<(String, String), String>> {
    let conn = app_db::open()?;
    let codex_dir = codex_dir_key(codex_dir);
    let mut stmt = conn
        .prepare(
            "SELECT item_kind, item_id, note
             FROM skills_mcp_notes
             WHERE codex_dir = ?1",
        )
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    let rows = stmt
        .query_map([codex_dir], |row| {
            Ok((
                (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    let mut notes = HashMap::new();
    for row in rows {
        let (key, note) = row.map_err(|error| CodexxError::Database(error.to_string()))?;
        notes.insert(key, note);
    }
    Ok(notes)
}

fn update_note_at(
    codex_dir: &Path,
    item_kind: &str,
    item_id: &str,
    note: &str,
) -> Result<Option<String>> {
    let item_kind = validate_kind(item_kind)?;
    let item_id = validate_item_id(item_id)?;
    let note = note.trim();
    if note.chars().count() > MAX_NOTE_CHARS {
        return Err(CodexxError::Config(format!(
            "备注不能超过 {MAX_NOTE_CHARS} 个字符"
        )));
    }

    let conn = app_db::open()?;
    let codex_dir = codex_dir_key(codex_dir);
    if note.is_empty() {
        conn.execute(
            "DELETE FROM skills_mcp_notes
             WHERE codex_dir = ?1 AND item_kind = ?2 AND item_id = ?3",
            params![codex_dir, item_kind, item_id],
        )
        .map_err(|error| CodexxError::Database(error.to_string()))?;
        return Ok(None);
    }

    conn.execute(
        "INSERT INTO skills_mcp_notes (codex_dir, item_kind, item_id, note, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(codex_dir, item_kind, item_id) DO UPDATE SET
           note = excluded.note,
           updated_at = excluded.updated_at",
        params![codex_dir, item_kind, item_id, note, now_rfc3339()],
    )
    .map_err(|error| CodexxError::Database(error.to_string()))?;
    Ok(Some(note.to_string()))
}

pub(crate) fn update_skills_mcp_note_inner(
    config_dir: Option<String>,
    item_kind: String,
    item_id: String,
    note: String,
) -> Result<SkillMcpNoteUpdate> {
    let codex_dir = resolve_codex_dir(config_dir)?;
    let note = update_note_at(&codex_dir, &item_kind, &item_id, &note)?;
    Ok(SkillMcpNoteUpdate {
        item_kind: validate_kind(&item_kind)?.to_string(),
        item_id: validate_item_id(&item_id)?.to_string(),
        note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_db::test_db_guard;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir(label: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "codex-x-notes-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create notes test directory");
        path
    }

    fn with_test_db(test: impl FnOnce(&Path)) {
        let _guard = test_db_guard();
        let app_home = temp_dir("db");
        std::env::set_var("CODEXX_HOME", &app_home);
        test(&app_home);
        std::env::remove_var("CODEXX_HOME");
        fs::remove_dir_all(app_home).expect("remove notes test database");
    }

    #[test]
    fn notes_support_create_update_delete_and_kind_isolation() {
        with_test_db(|_| {
            let codex_dir = temp_dir("home");
            assert_eq!(
                update_note_at(&codex_dir, SKILL_NOTE_KIND, "shared", "first")
                    .expect("save skill note")
                    .as_deref(),
                Some("first")
            );
            update_note_at(&codex_dir, SKILL_NOTE_KIND, "shared", "second")
                .expect("update skill note");
            update_note_at(&codex_dir, MCP_NOTE_KIND, "shared", "mcp").expect("save mcp note");
            let notes = load_notes(&codex_dir).expect("load notes");
            assert_eq!(
                notes.get(&(SKILL_NOTE_KIND.to_string(), "shared".to_string())),
                Some(&"second".to_string())
            );
            assert_eq!(
                notes.get(&(MCP_NOTE_KIND.to_string(), "shared".to_string())),
                Some(&"mcp".to_string())
            );

            assert_eq!(
                update_note_at(&codex_dir, SKILL_NOTE_KIND, "shared", "  ")
                    .expect("delete empty note"),
                None
            );
            let notes = load_notes(&codex_dir).expect("reload notes");
            assert!(!notes.contains_key(&(SKILL_NOTE_KIND.to_string(), "shared".to_string())));
            assert!(notes.contains_key(&(MCP_NOTE_KIND.to_string(), "shared".to_string())));
            fs::remove_dir_all(codex_dir).expect("remove Codex home");
        });
    }

    #[test]
    fn notes_are_isolated_by_codex_home() {
        with_test_db(|_| {
            let home_a = temp_dir("home-a");
            let home_b = temp_dir("home-b");
            for kind in [SKILL_NOTE_KIND, MCP_NOTE_KIND] {
                update_note_at(&home_a, kind, "pdf", "A").expect("save A note");
                update_note_at(&home_b, kind, "pdf", "B").expect("save B note");
            }
            for kind in [SKILL_NOTE_KIND, MCP_NOTE_KIND] {
                assert_eq!(
                    load_notes(&home_a)
                        .expect("load A")
                        .get(&(kind.to_string(), "pdf".to_string())),
                    Some(&"A".to_string())
                );
                assert_eq!(
                    load_notes(&home_b)
                        .expect("load B")
                        .get(&(kind.to_string(), "pdf".to_string())),
                    Some(&"B".to_string())
                );
            }
            fs::remove_dir_all(home_a).expect("remove A");
            fs::remove_dir_all(home_b).expect("remove B");
        });
    }

    #[test]
    fn state_build_attaches_notes_only_to_current_items() {
        with_test_db(|_| {
            let codex_dir = temp_dir("state");
            let skill_dir = codex_dir.join("skills/pdf");
            fs::create_dir_all(&skill_dir).expect("create skill directory");
            fs::write(
                skill_dir.join("SKILL.md"),
                "---\nname: PDF\ndescription: PDF tools\n---\n",
            )
            .expect("write skill metadata");
            let plain_skill_dir = codex_dir.join("skills/plain");
            fs::create_dir_all(&plain_skill_dir).expect("create unnoted skill directory");
            fs::write(plain_skill_dir.join("SKILL.md"), "---\nname: Plain\n---\n")
                .expect("write unnoted skill metadata");
            fs::write(
                codex_dir.join("config.toml"),
                "[mcp_servers.filesystem]\ncommand = \"fs\"\n",
            )
            .expect("write MCP config");
            update_note_at(&codex_dir, SKILL_NOTE_KIND, "pdf", "PDF note")
                .expect("save skill note");
            update_note_at(&codex_dir, MCP_NOTE_KIND, "filesystem", "MCP note")
                .expect("save MCP note");
            update_note_at(&codex_dir, SKILL_NOTE_KIND, "orphan", "orphan note")
                .expect("save orphan note");

            let state = crate::skills_mcp::build_skills_mcp_state_inner(Some(
                codex_dir.display().to_string(),
            ))
            .expect("build Skills/MCP state");
            assert_eq!(state.skills.len(), 2);
            assert_eq!(
                state
                    .skills
                    .iter()
                    .find(|skill| skill.id == "pdf")
                    .and_then(|skill| skill.note.as_deref()),
                Some("PDF note")
            );
            assert!(state
                .skills
                .iter()
                .find(|skill| skill.id == "plain")
                .is_some_and(|skill| skill.note.is_none()));
            assert_eq!(state.mcp_servers.len(), 1);
            assert_eq!(state.mcp_servers[0].note.as_deref(), Some("MCP note"));
            assert!(state.skills.iter().all(|skill| skill.id != "orphan"));
            fs::remove_dir_all(codex_dir).expect("remove state home");
        });
    }

    #[test]
    fn note_validation_rejects_invalid_kind_empty_id_and_long_text() {
        with_test_db(|_| {
            let codex_dir = temp_dir("validation");
            assert!(update_note_at(&codex_dir, "prompt", "id", "note").is_err());
            assert!(update_note_at(&codex_dir, SKILL_NOTE_KIND, " ", "note").is_err());
            assert!(update_note_at(
                &codex_dir,
                SKILL_NOTE_KIND,
                "id",
                &"x".repeat(MAX_NOTE_CHARS + 1)
            )
            .is_err());
            fs::remove_dir_all(codex_dir).expect("remove validation home");
        });
    }
}
