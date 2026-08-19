use super::build_skills_mcp_state_inner;
use super::types::{ManagedMcpServer, SkillsMcpState};
use crate::backups::create_backup;
use crate::ccswitch::default_ccswitch_db_path;
use crate::error::{CodexxError, Result};
use crate::file_io::{ensure_directory, parse_toml_document, read_to_string_if_exists};
use crate::live_config::{
    acquire_live_config_lock, apply_file_change, fail_with_file_rollback, read_file_snapshot,
    text_from_snapshot,
};
use crate::toml_utils::ensure_table;
use crate::{config_path, now_rfc3339, open_db, resolve_codex_dir};
use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;
use toml_edit::{value, Item, Table};

type CcSwitchMcpCandidate = (String, String, Value, bool);

fn toml_value_to_json(value: &toml_edit::Value) -> Value {
    if let Some(s) = value.as_str() {
        return json!(s);
    }
    if let Some(i) = value.as_integer() {
        return json!(i);
    }
    if let Some(f) = value.as_float() {
        return json!(f);
    }
    if let Some(b) = value.as_bool() {
        return json!(b);
    }
    if let Some(arr) = value.as_array() {
        return Value::Array(arr.iter().map(toml_value_to_json).collect());
    }
    Value::String(value.to_string())
}

fn toml_item_to_json(item: &Item) -> Value {
    if let Some(v) = item.as_value() {
        return toml_value_to_json(v);
    }
    if let Some(tbl) = item.as_table() {
        let mut obj = serde_json::Map::new();
        for (k, v) in tbl.iter() {
            obj.insert(k.to_string(), toml_item_to_json(v));
        }
        return Value::Object(obj);
    }
    Value::Null
}

fn json_to_toml_item(value_json: &Value) -> Item {
    match value_json {
        Value::String(s) => value(s.clone()),
        Value::Bool(b) => value(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                value(i)
            } else if let Some(f) = n.as_f64() {
                value(f)
            } else {
                value(n.to_string())
            }
        }
        Value::Array(arr) => {
            let mut toml_arr = toml_edit::Array::default();
            for item in arr {
                match item {
                    Value::String(s) => {
                        toml_arr.push(s.as_str());
                    }
                    Value::Bool(b) => {
                        toml_arr.push(*b);
                    }
                    Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            toml_arr.push(i);
                        } else if let Some(f) = n.as_f64() {
                            toml_arr.push(f);
                        }
                    }
                    _ => {}
                }
            }
            value(toml_arr)
        }
        Value::Object(obj) => {
            let mut table = Table::new();
            for (k, v) in obj {
                table.insert(k, json_to_toml_item(v));
            }
            Item::Table(table)
        }
        Value::Null => value(""),
    }
}

pub(super) fn mcp_summary(config: &Value) -> (String, Option<String>, Option<String>, String) {
    let transport = config
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            if config.get("url").is_some() {
                "http"
            } else {
                "stdio"
            }
        })
        .to_string();
    let command = config
        .get("command")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let url = config
        .get("url")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let args = config
        .get("args")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let summary = if let Some(cmd) = &command {
        if args.is_empty() {
            cmd.clone()
        } else {
            format!("{cmd} {args}")
        }
    } else if let Some(url) = &url {
        url.clone()
    } else {
        transport.clone()
    };
    (transport, command, url, summary)
}

fn save_managed_mcp_on_connection(
    conn: &Connection,
    id: &str,
    name: &str,
    config: &Value,
    enabled: bool,
) -> Result<()> {
    conn.execute(
        "INSERT INTO managed_mcp_servers (id, name, server_config, enabled, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET
           name = excluded.name,
           server_config = excluded.server_config,
           enabled = excluded.enabled,
           updated_at = excluded.updated_at",
        params![
            id,
            name,
            serde_json::to_string(config).unwrap_or_default(),
            enabled,
            now_rfc3339()
        ],
    )
    .map_err(|e| CodexxError::Database(e.to_string()))?;
    Ok(())
}

pub(super) fn save_managed_mcp(id: &str, name: &str, config: &Value, enabled: bool) -> Result<()> {
    let conn = open_db()?;
    save_managed_mcp_on_connection(&conn, id, name, config, enabled)
}

fn managed_mcp_on_connection(conn: &Connection, id: &str) -> Result<Option<(String, Value, bool)>> {
    conn.query_row(
        "SELECT name, server_config, enabled FROM managed_mcp_servers WHERE id = ?1 LIMIT 1",
        [id],
        |row| {
            let config_text: String = row.get(1)?;
            Ok((
                row.get(0)?,
                serde_json::from_str(&config_text).unwrap_or(Value::Object(Default::default())),
                row.get(2)?,
            ))
        },
    )
    .optional()
    .map_err(|error| CodexxError::Database(error.to_string()))
}

fn document_mcp_ids(doc: &toml_edit::DocumentMut) -> HashSet<String> {
    doc.get("mcp_servers")
        .and_then(|item| item.as_table())
        .map(|table| {
            table
                .iter()
                .filter(|(_, item)| item.is_table())
                .map(|(id, _)| id.to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn document_bytes(snapshot: Option<&[u8]>, doc: &toml_edit::DocumentMut) -> Option<Vec<u8>> {
    let bytes = doc.to_string().into_bytes();
    if snapshot.is_none() && bytes.is_empty() {
        None
    } else {
        Some(bytes)
    }
}

fn commit_mcp_transaction_with_config<BeforeApply, BeforeCommit>(
    transaction: Transaction<'_>,
    codex_dir: &Path,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
    backup_action: &str,
    before_apply: BeforeApply,
    before_commit: BeforeCommit,
) -> Result<()>
where
    BeforeApply: FnOnce(&Path) -> Result<()>,
    BeforeCommit: FnOnce(&Transaction<'_>) -> Result<()>,
{
    let cfg = config_path(codex_dir);
    let mut changes = Vec::new();
    if before != after {
        create_backup(codex_dir, backup_action)?;
        before_apply(&cfg)?;
        changes.push(apply_file_change(&cfg, before, after)?);
    }

    if let Err(error) = before_commit(&transaction) {
        return fail_with_file_rollback(error, &changes);
    }
    if let Err(error) = transaction
        .commit()
        .map_err(|error| CodexxError::Database(error.to_string()))
    {
        return fail_with_file_rollback(error, &changes);
    }
    Ok(())
}

pub(super) fn db_managed_mcp() -> Result<Vec<(String, String, Value, bool)>> {
    let conn = open_db()?;
    let mut stmt = conn.prepare("SELECT id, name, server_config, enabled FROM managed_mcp_servers ORDER BY name ASC, id ASC")
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            let text: String = row.get(2)?;
            let config = serde_json::from_str(&text).unwrap_or(Value::Object(Default::default()));
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                config,
                row.get::<_, bool>(3)?,
            ))
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| CodexxError::Database(e.to_string()))?);
    }
    Ok(out)
}

pub(super) fn list_mcp_from_config(codex_dir: &Path) -> Result<Vec<ManagedMcpServer>> {
    let cfg = config_path(codex_dir);
    let text = read_to_string_if_exists(&cfg)?;
    if text.trim().is_empty() {
        return Ok(vec![]);
    }
    let doc = parse_toml_document(&cfg, &text)?;
    let Some(mcp_item) = doc.get("mcp_servers") else {
        return Ok(vec![]);
    };
    let Some(mcp_tbl) = mcp_item.as_table() else {
        return Ok(vec![]);
    };
    let mut out = Vec::new();
    for (id, item) in mcp_tbl.iter() {
        if !item.is_table() {
            continue;
        }
        let config = toml_item_to_json(item);
        let (transport, command, url, summary) = mcp_summary(&config);
        out.push(ManagedMcpServer {
            id: id.to_string(),
            name: id.to_string(),
            transport,
            enabled: true,
            source: "config.toml".to_string(),
            summary,
            command,
            url,
            config_json: config,
            note: None,
        });
    }
    Ok(out)
}

pub(crate) fn sort_managed_mcp_servers(servers: &mut [ManagedMcpServer]) {
    servers.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
}

pub(super) fn import_ccswitch_mcp_servers_for_codex(
    codex_dir: &Path,
    imported_ids: &mut HashSet<String>,
) -> Result<usize> {
    let db = default_ccswitch_db_path()?;
    if !db.exists() {
        return Ok(0);
    }
    let conn = Connection::open_with_flags(
        &db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| {
        CodexxError::Database(format!(
            "打开 cc-switch MCP 数据库失败 {}: {e}",
            db.display()
        ))
    })?;
    let mut stmt = match conn
        .prepare("SELECT id, name, server_config, enabled_codex FROM mcp_servers ORDER BY name ASC, id ASC")
        .or_else(|_| {
            conn.prepare("SELECT id, name, server_config, 0 AS enabled_codex FROM mcp_servers ORDER BY name ASC, id ASC")
        }) {
        Ok(stmt) => stmt,
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if message.to_lowercase().contains("no such table") =>
        {
            return Ok(0);
        }
        Err(e) => return Err(CodexxError::Database(e.to_string())),
    };
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
            ))
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let mut candidates = Vec::new();
    for row in rows {
        let (id, name, config_text, enabled_codex) =
            row.map_err(|e| CodexxError::Database(e.to_string()))?;
        let config: Value =
            serde_json::from_str(&config_text).unwrap_or(Value::Object(Default::default()));
        candidates.push((id, name, config, enabled_codex));
    }

    import_ccswitch_mcp_candidates_with_hooks(
        codex_dir,
        imported_ids,
        candidates,
        |_| Ok(()),
        |_| Ok(()),
    )
}

fn import_ccswitch_mcp_candidates_with_hooks<BeforeApply, BeforeCommit>(
    codex_dir: &Path,
    imported_ids: &mut HashSet<String>,
    candidates: Vec<CcSwitchMcpCandidate>,
    before_apply: BeforeApply,
    before_commit: BeforeCommit,
) -> Result<usize>
where
    BeforeApply: FnOnce(&Path) -> Result<()>,
    BeforeCommit: FnOnce(&Transaction<'_>) -> Result<()>,
{
    let mut staged_ids = HashSet::new();
    let candidates = candidates
        .into_iter()
        .filter(|(id, _, _, _)| !imported_ids.contains(id) && staged_ids.insert(id.clone()))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(0);
    }

    ensure_directory(codex_dir)?;
    let _live_lock = acquire_live_config_lock(codex_dir)?;
    let cfg = config_path(codex_dir);
    let before = read_file_snapshot(&cfg)?;
    let text = text_from_snapshot(&cfg, before.as_deref())?;
    let mut doc = parse_toml_document(&cfg, &text)?;
    let live_enabled = document_mcp_ids(&doc);

    let mut app_conn = open_db()?;
    let transaction = app_conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    for (id, name, config, enabled_codex) in &candidates {
        let enabled = *enabled_codex || live_enabled.contains(id);
        save_managed_mcp_on_connection(&transaction, id, name, config, enabled)?;
        if *enabled_codex && !live_enabled.contains(id) {
            ensure_table(doc.as_table_mut(), "mcp_servers")?.insert(id, json_to_toml_item(config));
        }
    }
    let after = document_bytes(before.as_deref(), &doc);
    commit_mcp_transaction_with_config(
        transaction,
        codex_dir,
        before,
        after,
        "import-ccswitch-mcp",
        before_apply,
        before_commit,
    )?;

    let imported = candidates.len();
    imported_ids.extend(staged_ids);
    Ok(imported)
}

pub(super) fn preview_ccswitch_mcp_servers_for_codex(
    codex_dir: &Path,
) -> Result<Vec<ManagedMcpServer>> {
    let db = default_ccswitch_db_path()?;
    if !db.exists() {
        return Ok(vec![]);
    }
    let conn = Connection::open_with_flags(
        &db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| {
        CodexxError::Database(format!(
            "打开 cc-switch MCP 数据库失败 {}: {e}",
            db.display()
        ))
    })?;
    let mut stmt = match conn
        .prepare("SELECT id, name, server_config, enabled_codex FROM mcp_servers ORDER BY name ASC, id ASC")
        .or_else(|_| {
            conn.prepare("SELECT id, name, server_config, 0 AS enabled_codex FROM mcp_servers ORDER BY name ASC, id ASC")
        }) {
        Ok(stmt) => stmt,
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if message.to_lowercase().contains("no such table") =>
        {
            return Ok(vec![]);
        }
        Err(e) => return Err(CodexxError::Database(e.to_string())),
    };
    let live_enabled = list_mcp_from_config(codex_dir)?
        .into_iter()
        .map(|server| server.id)
        .collect::<HashSet<_>>();
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
            ))
        })
        .map_err(|e| CodexxError::Database(e.to_string()))?;
    let mut out = Vec::new();
    for row in rows {
        let (id, name, config_text, enabled_codex) =
            row.map_err(|e| CodexxError::Database(e.to_string()))?;
        let config: Value =
            serde_json::from_str(&config_text).unwrap_or(Value::Object(Default::default()));
        let (transport, command, url, summary) = mcp_summary(&config);
        out.push(ManagedMcpServer {
            id: id.clone(),
            name,
            transport,
            enabled: enabled_codex || live_enabled.contains(&id),
            source: "cc-switch".to_string(),
            summary,
            command,
            url,
            config_json: config,
            note: None,
        });
    }
    Ok(out)
}

pub(crate) fn toggle_codex_mcp_inner(
    config_dir: Option<String>,
    id: String,
    enabled: bool,
) -> Result<SkillsMcpState> {
    toggle_codex_mcp_with_hooks(config_dir, id, enabled, |_| Ok(()), |_| Ok(()))
}

fn toggle_codex_mcp_with_hooks<BeforeApply, BeforeCommit>(
    config_dir: Option<String>,
    id: String,
    enabled: bool,
    before_apply: BeforeApply,
    before_commit: BeforeCommit,
) -> Result<SkillsMcpState>
where
    BeforeApply: FnOnce(&Path) -> Result<()>,
    BeforeCommit: FnOnce(&Transaction<'_>) -> Result<()>,
{
    let codex_dir = resolve_codex_dir(config_dir.clone())?;
    ensure_directory(&codex_dir)?;
    let _live_lock = acquire_live_config_lock(&codex_dir)?;
    let cfg = config_path(&codex_dir);
    let before = read_file_snapshot(&cfg)?;
    let text = text_from_snapshot(&cfg, before.as_deref())?;
    let mut doc = parse_toml_document(&cfg, &text)?;

    let mut conn = open_db()?;
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| CodexxError::Database(error.to_string()))?;
    let stored = managed_mcp_on_connection(&transaction, &id)?;
    if enabled {
        let (name, config, _) =
            stored.ok_or_else(|| CodexxError::Config(format!("未找到 MCP: {id}")))?;
        ensure_table(doc.as_table_mut(), "mcp_servers")?.insert(&id, json_to_toml_item(&config));
        save_managed_mcp_on_connection(&transaction, &id, &name, &config, true)?;
    } else {
        let live_config = doc
            .get("mcp_servers")
            .and_then(|m| m.as_table())
            .and_then(|tbl| tbl.get(&id))
            .map(toml_item_to_json);
        if let Some(config) = live_config {
            let name = stored
                .as_ref()
                .map(|(name, _, _)| name.as_str())
                .unwrap_or(&id);
            save_managed_mcp_on_connection(&transaction, &id, name, &config, false)?;
        } else if let Some((name, config, _)) = stored {
            save_managed_mcp_on_connection(&transaction, &id, &name, &config, false)?;
        }
        if let Some(tbl) = doc.get_mut("mcp_servers").and_then(|m| m.as_table_mut()) {
            tbl.remove(&id);
        }
    }

    let after = document_bytes(before.as_deref(), &doc);
    commit_mcp_transaction_with_config(
        transaction,
        &codex_dir,
        before,
        after,
        "toggle-mcp",
        before_apply,
        before_commit,
    )?;
    build_skills_mcp_state_inner(config_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_case(name: &str) -> (PathBuf, String) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let suffix = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "codex-x-mcp-{name}-{}-{suffix}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create MCP test directory");
        (dir, format!("test-mcp-{name}-{suffix}"))
    }

    fn database_error(error: rusqlite::Error) -> CodexxError {
        CodexxError::Database(error.to_string())
    }

    fn remove_test_mcp(id: &str) {
        open_db()
            .expect("open test database")
            .execute("DELETE FROM managed_mcp_servers WHERE id = ?1", [id])
            .expect("remove test MCP");
    }

    #[test]
    fn toggle_updates_only_the_target_mcp_and_keeps_database_in_sync() {
        let _db_guard = crate::app_db::test_db_guard();
        let (codex_dir, id) = test_case("success");
        let cfg = config_path(&codex_dir);
        let original = b"# keep this comment\nmodel = \"gpt-5.5\"\n\n[features]\njs_repl = false\n\n[mcp_servers.existing]\ncommand = \"existing\"\n";
        fs::write(&cfg, original).expect("seed original config");
        save_managed_mcp(&id, "Managed", &json!({ "command": "managed" }), false)
            .expect("seed managed MCP");

        let enabled_state =
            toggle_codex_mcp_inner(Some(codex_dir.display().to_string()), id.clone(), true)
                .expect("enable managed MCP");
        assert!(enabled_state
            .mcp_servers
            .iter()
            .any(|server| server.id == id && server.enabled));
        let enabled_text = fs::read_to_string(&cfg).expect("read enabled config");
        let enabled_doc = enabled_text
            .parse::<toml_edit::DocumentMut>()
            .expect("parse enabled config");
        assert!(enabled_text.contains("# keep this comment"));
        assert_eq!(enabled_doc["features"]["js_repl"].as_bool(), Some(false));
        assert_eq!(
            enabled_doc["mcp_servers"]["existing"]["command"].as_str(),
            Some("existing")
        );
        assert_eq!(
            enabled_doc["mcp_servers"][&id]["command"].as_str(),
            Some("managed")
        );
        let conn = open_db().expect("open test database");
        assert!(
            managed_mcp_on_connection(&conn, &id)
                .expect("read enabled MCP")
                .expect("enabled MCP exists")
                .2
        );
        drop(conn);

        toggle_codex_mcp_inner(Some(codex_dir.display().to_string()), id.clone(), false)
            .expect("disable managed MCP");
        let disabled_text = fs::read_to_string(&cfg).expect("read disabled config");
        let disabled_doc = disabled_text
            .parse::<toml_edit::DocumentMut>()
            .expect("parse disabled config");
        assert!(disabled_text.contains("# keep this comment"));
        assert!(disabled_doc["mcp_servers"].get(&id).is_none());
        assert_eq!(
            disabled_doc["mcp_servers"]["existing"]["command"].as_str(),
            Some("existing")
        );
        let conn = open_db().expect("open test database");
        let (name, config, enabled) = managed_mcp_on_connection(&conn, &id)
            .expect("read disabled MCP")
            .expect("disabled MCP exists");
        assert_eq!(name, "Managed");
        assert_eq!(config["command"].as_str(), Some("managed"));
        assert!(!enabled);
        drop(conn);
        assert!(codex_dir.join(".codexx-test-backups").is_dir());

        remove_test_mcp(&id);
        fs::remove_dir_all(codex_dir).expect("remove MCP test directory");
    }

    #[test]
    fn toggle_rejects_a_stale_snapshot_without_changing_database_or_external_config() {
        let _db_guard = crate::app_db::test_db_guard();
        let (codex_dir, id) = test_case("stale");
        let cfg = config_path(&codex_dir);
        let original = b"# original\nmodel = \"gpt-5.5\"\n";
        let external = b"# external update\nmodel = \"gpt-5.5\"\n\n[mcp_servers.external]\ncommand = \"external\"\n";
        fs::write(&cfg, original).expect("seed original config");
        save_managed_mcp(&id, "Managed", &json!({ "command": "managed" }), false)
            .expect("seed managed MCP");

        let error = toggle_codex_mcp_with_hooks(
            Some(codex_dir.display().to_string()),
            id.clone(),
            true,
            |path| {
                fs::write(path, external).map_err(|error| CodexxError::Config(error.to_string()))
            },
            |_| Ok(()),
        )
        .expect_err("stale MCP toggle must fail");

        assert!(error.to_string().contains("已被其他程序修改"));
        assert_eq!(fs::read(&cfg).expect("read external config"), external);
        let conn = open_db().expect("open test database");
        let (_, config, enabled) = managed_mcp_on_connection(&conn, &id)
            .expect("read managed MCP")
            .expect("managed MCP still exists");
        assert_eq!(config["command"].as_str(), Some("managed"));
        assert!(!enabled);
        drop(conn);

        remove_test_mcp(&id);
        fs::remove_dir_all(codex_dir).expect("remove MCP test directory");
    }

    #[test]
    fn toggle_rolls_back_config_when_database_commit_fails() {
        let _db_guard = crate::app_db::test_db_guard();
        let (codex_dir, id) = test_case("commit");
        let cfg = config_path(&codex_dir);
        let original = b"# keep exact bytes\nmodel = \"gpt-5.5\"\n\n[features]\njs_repl = false\n";
        fs::write(&cfg, original).expect("seed original config");
        save_managed_mcp(&id, "Managed", &json!({ "command": "managed" }), false)
            .expect("seed managed MCP");

        toggle_codex_mcp_with_hooks(
            Some(codex_dir.display().to_string()),
            id.clone(),
            true,
            |_| Ok(()),
            |transaction| {
                transaction
                    .execute_batch("ROLLBACK")
                    .map_err(database_error)
            },
        )
        .expect_err("database commit failure must fail the toggle");

        assert_eq!(fs::read(&cfg).expect("read rolled-back config"), original);
        let conn = open_db().expect("open test database");
        let (_, _, enabled) = managed_mcp_on_connection(&conn, &id)
            .expect("read managed MCP")
            .expect("managed MCP still exists");
        assert!(!enabled);
        drop(conn);
        assert!(codex_dir.join(".codexx-test-backups").is_dir());

        remove_test_mcp(&id);
        fs::remove_dir_all(codex_dir).expect("remove MCP test directory");
    }

    #[test]
    fn ccswitch_import_rolls_back_file_database_and_ids_when_commit_fails() {
        let _db_guard = crate::app_db::test_db_guard();
        let (codex_dir, id) = test_case("import-commit");
        let second_id = format!("{id}-second");
        let cfg = config_path(&codex_dir);
        let original = b"# import baseline\nmodel = \"gpt-5.5\"\n";
        fs::write(&cfg, original).expect("seed original config");
        let mut imported_ids = HashSet::new();

        import_ccswitch_mcp_candidates_with_hooks(
            &codex_dir,
            &mut imported_ids,
            vec![
                (
                    id.clone(),
                    "Imported".to_string(),
                    json!({ "command": "imported" }),
                    true,
                ),
                (
                    second_id.clone(),
                    "Imported second".to_string(),
                    json!({ "command": "imported-second" }),
                    true,
                ),
            ],
            |_| Ok(()),
            |transaction| {
                transaction
                    .execute_batch("ROLLBACK")
                    .map_err(database_error)
            },
        )
        .expect_err("database commit failure must fail the import");

        assert_eq!(fs::read(&cfg).expect("read rolled-back config"), original);
        assert!(imported_ids.is_empty());
        let conn = open_db().expect("open test database");
        assert!(managed_mcp_on_connection(&conn, &id)
            .expect("read imported MCP")
            .is_none());
        assert!(managed_mcp_on_connection(&conn, &second_id)
            .expect("read second imported MCP")
            .is_none());
        drop(conn);
        assert!(codex_dir.join(".codexx-test-backups").is_dir());

        fs::remove_dir_all(codex_dir).expect("remove MCP test directory");
    }
}
