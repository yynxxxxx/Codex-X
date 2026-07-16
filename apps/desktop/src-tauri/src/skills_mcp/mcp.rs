use super::build_skills_mcp_state_inner;
use super::types::{ManagedMcpServer, SkillsMcpState};
use crate::ccswitch::default_ccswitch_db_path;
use crate::error::{CodexxError, Result};
use crate::file_io::{ensure_directory, parse_toml_document, read_to_string_if_exists, write_text};
use crate::toml_utils::ensure_table;
use crate::{config_path, now_rfc3339, open_db, resolve_codex_dir};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;
use toml_edit::{value, Item, Table};

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

pub(super) fn save_managed_mcp(id: &str, name: &str, config: &Value, enabled: bool) -> Result<()> {
    let conn = open_db()?;
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

    let cfg = config_path(codex_dir);
    let text = read_to_string_if_exists(&cfg)?;
    let mut doc = parse_toml_document(&cfg, &text)?;
    let live_enabled = list_mcp_from_config(codex_dir)?
        .into_iter()
        .map(|server| server.id)
        .collect::<HashSet<_>>();
    let mut imported = 0usize;
    let mut changed_config = false;
    for row in rows {
        let (id, name, config_text, enabled_codex) =
            row.map_err(|e| CodexxError::Database(e.to_string()))?;
        let config: Value =
            serde_json::from_str(&config_text).unwrap_or(Value::Object(Default::default()));
        if !imported_ids.insert(id.clone()) {
            continue;
        }
        let enabled = enabled_codex || live_enabled.contains(&id);
        save_managed_mcp(&id, &name, &config, enabled)?;
        if enabled_codex && !live_enabled.contains(&id) {
            ensure_table(doc.as_table_mut(), "mcp_servers")?
                .insert(&id, json_to_toml_item(&config));
            changed_config = true;
        }
        imported += 1;
    }
    if changed_config {
        write_text(&cfg, &doc.to_string())?;
    }
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
        });
    }
    Ok(out)
}

pub(crate) fn toggle_codex_mcp_inner(
    config_dir: Option<String>,
    id: String,
    enabled: bool,
) -> Result<SkillsMcpState> {
    let codex_dir = resolve_codex_dir(config_dir.clone())?;
    ensure_directory(&codex_dir)?;
    let cfg = config_path(&codex_dir);
    let text = read_to_string_if_exists(&cfg)?;
    let mut doc = parse_toml_document(&cfg, &text)?;
    if enabled {
        let db = db_managed_mcp()?
            .into_iter()
            .find(|(sid, _, _, _)| sid == &id)
            .ok_or_else(|| CodexxError::Config(format!("未找到 MCP: {id}")))?;
        ensure_table(doc.as_table_mut(), "mcp_servers")?.insert(&id, json_to_toml_item(&db.2));
        save_managed_mcp(&id, &db.1, &db.2, true)?;
    } else {
        if let Some(item) = doc
            .get("mcp_servers")
            .and_then(|m| m.as_table())
            .and_then(|tbl| tbl.get(&id))
        {
            let config = toml_item_to_json(item);
            save_managed_mcp(&id, &id, &config, false)?;
        }
        if let Some(tbl) = doc.get_mut("mcp_servers").and_then(|m| m.as_table_mut()) {
            tbl.remove(&id);
        }
    }
    write_text(&cfg, &doc.to_string())?;
    build_skills_mcp_state_inner(config_dir)
}
