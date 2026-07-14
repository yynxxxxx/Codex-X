use crate::error::{CodexxError, Result};
use crate::file_io::{atomic_write, io_err, json_err, write_text};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
pub(super) struct GlobalStateWrite {
    pub(super) path: std::path::PathBuf,
    pub(super) original_bytes: Option<Vec<u8>>,
    pub(super) written_bytes: Vec<u8>,
}

pub(super) fn normalize_workspace_path(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with(r"\\?\unc\") {
        return Some(format!(r"\\{}", trimmed[8..].replace('/', r"\")));
    }
    if let Some(stripped) = trimmed.strip_prefix(r"\\?\") {
        return Some(stripped.replace('\\', "/"));
    }
    Some(trimmed.to_string())
}

fn load_global_state(path: &Path) -> Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let text = fs::read_to_string(path).map_err(|error| io_err(path, error))?;
    let value = serde_json::from_str::<Value>(&text).map_err(|error| json_err(path, error))?;
    Ok(value.as_object().cloned().unwrap_or_default())
}

pub(super) fn projectless_thread_ids(path: &Path) -> Result<HashSet<String>> {
    let state = load_global_state(path)?;
    Ok(state
        .get("projectless-thread-ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
        .collect())
}

fn normalized_path_array(value: &Value) -> Vec<String> {
    if let Some(items) = value.as_array() {
        items
            .iter()
            .filter_map(Value::as_str)
            .filter_map(normalize_workspace_path)
            .collect()
    } else {
        value
            .as_str()
            .and_then(normalize_workspace_path)
            .into_iter()
            .collect()
    }
}

fn dedupe_workspace_paths(paths: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| {
            seen.insert(
                path.replace('/', r"\")
                    .trim_end_matches('\\')
                    .to_ascii_lowercase(),
            )
        })
        .collect()
}

fn normalized_global_state(state: &Map<String, Value>) -> Map<String, Value> {
    let mut next = Map::new();
    for key in ["electron-saved-workspace-roots", "project-order"] {
        if let Some(value) = state.get(key) {
            next.insert(
                key.to_string(),
                json!(dedupe_workspace_paths(normalized_path_array(value))),
            );
        }
    }
    if let Some(value) = state.get("active-workspace-roots") {
        let normalized = dedupe_workspace_paths(normalized_path_array(value));
        let next_value = if value.is_array() {
            json!(normalized)
        } else if let Some(first) = normalized.first() {
            json!(first)
        } else {
            value.clone()
        };
        next.insert("active-workspace-roots".to_string(), next_value);
    }
    if let Some(labels) = state
        .get("electron-workspace-root-labels")
        .and_then(Value::as_object)
    {
        let mut normalized = Map::new();
        for (path, value) in labels {
            normalized.insert(
                normalize_workspace_path(path).unwrap_or_else(|| path.clone()),
                value.clone(),
            );
        }
        next.insert(
            "electron-workspace-root-labels".to_string(),
            Value::Object(normalized),
        );
    }
    if let Some(open_targets) = state
        .get("open-in-target-preferences")
        .and_then(Value::as_object)
    {
        let mut normalized = open_targets.clone();
        if let Some(per_path) = open_targets.get("perPath").and_then(Value::as_object) {
            let mut normalized_per_path = Map::new();
            for (path, value) in per_path {
                normalized_per_path.insert(
                    normalize_workspace_path(path).unwrap_or_else(|| path.clone()),
                    value.clone(),
                );
            }
            normalized.insert("perPath".to_string(), Value::Object(normalized_per_path));
        }
        next.insert(
            "open-in-target-preferences".to_string(),
            Value::Object(normalized),
        );
    }
    next
}

pub(super) fn count_global_state_updates(path: &Path) -> Result<usize> {
    let state = load_global_state(path)?;
    let next = normalized_global_state(&state);
    Ok(next
        .iter()
        .filter(|(key, value)| state.get(*key) != Some(*value))
        .count())
}

#[cfg(test)]
fn apply_global_state_update(path: &Path) -> Result<usize> {
    apply_global_state_update_with_journal(path, &mut Vec::new(), &mut || Ok(()))
}

fn read_optional_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_err(path, error)),
    }
}

pub(super) fn apply_global_state_update_with_journal<F>(
    path: &Path,
    writes: &mut Vec<GlobalStateWrite>,
    after_main_write: &mut F,
) -> Result<usize>
where
    F: FnMut() -> Result<()>,
{
    let original_bytes = read_optional_bytes(path)?;
    let mut state = match &original_bytes {
        Some(bytes) => serde_json::from_slice::<Value>(bytes)
            .map_err(|error| json_err(path, error))?
            .as_object()
            .cloned()
            .unwrap_or_default(),
        None => Map::new(),
    };
    let next = normalized_global_state(&state);
    let updated = next
        .iter()
        .filter(|(key, value)| state.get(*key) != Some(*value))
        .count();
    if updated > 0 {
        for (key, value) in next {
            state.insert(key, value);
        }
        let text = serde_json::to_string_pretty(&Value::Object(state))
            .map_err(|error| json_err(path, error))?;
        let written_bytes = text.as_bytes().to_vec();
        if read_optional_bytes(path)? != original_bytes {
            return Err(CodexxError::Config(
                "全局状态已发生变化，请重试。".to_string(),
            ));
        }
        write_text(path, &text)?;
        writes.push(GlobalStateWrite {
            path: path.to_path_buf(),
            original_bytes,
            written_bytes: written_bytes.clone(),
        });
        after_main_write()?;
        if let Some(parent) = path.parent() {
            let backup = parent.join(".codex-global-state.json.bak");
            let original_bytes = read_optional_bytes(&backup)?;
            if read_optional_bytes(&backup)? != original_bytes {
                return Err(CodexxError::Config(
                    "全局状态已发生变化，请重试。".to_string(),
                ));
            }
            write_text(&backup, &text)?;
            writes.push(GlobalStateWrite {
                path: backup,
                original_bytes,
                written_bytes,
            });
        }
    }
    Ok(updated)
}

pub(super) fn restore_global_write(write: &GlobalStateWrite) -> Result<()> {
    match fs::read(&write.path) {
        Ok(current) if current == write.written_bytes => {}
        Ok(_) => {
            return Err(CodexxError::Config(format!(
                "全局状态已发生变化，无法安全恢复: {}",
                write.path.display()
            )));
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound && write.original_bytes.is_none() =>
        {
            return Ok(());
        }
        Err(error) => return Err(io_err(&write.path, error)),
    }

    if let Some(original) = &write.original_bytes {
        atomic_write(&write.path, original)
    } else {
        fs::remove_file(&write.path).map_err(|error| io_err(&write.path, error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "codex-x-global-state-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test directory");
        path
    }

    #[test]
    fn global_state_update_keeps_unknown_fields_and_matches_backup() {
        let dir = temp_dir("update");
        let path = dir.join(".codex-global-state.json");
        fs::write(
            &path,
            r#"{
  "electron-saved-workspace-roots": "/tmp/project",
  "unrelated-setting": { "enabled": true }
}"#,
        )
        .expect("write original global state");

        assert_eq!(
            apply_global_state_update(&path).expect("update global state"),
            1
        );

        let main_text = fs::read_to_string(&path).expect("read global state");
        let backup_path = dir.join(".codex-global-state.json.bak");
        let backup_text = fs::read_to_string(&backup_path).expect("read global state backup");
        assert_eq!(main_text, backup_text);
        let state: Value = serde_json::from_str(&main_text).expect("parse global state");
        assert_eq!(
            state.get("electron-saved-workspace-roots"),
            Some(&json!(["/tmp/project"]))
        );
        assert_eq!(
            state.get("unrelated-setting"),
            Some(&json!({ "enabled": true }))
        );

        fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn restores_distinct_existing_global_main_and_backup() {
        let dir = temp_dir("distinct-snapshots");
        let main = dir.join(".codex-global-state.json");
        let backup = dir.join(".codex-global-state.json.bak");
        let original_main = br#"{"source":"main","value":1}"#;
        let original_backup = br#"{"source":"backup","value":2}"#;
        let written_text = r#"{"source":"mutated"}"#.to_string();
        fs::write(&main, original_main).expect("write original main");
        fs::write(&backup, original_backup).expect("write original backup");
        write_text(&main, &written_text).expect("mutate global main");
        write_text(&backup, &written_text).expect("mutate global backup");
        let writes = [
            GlobalStateWrite {
                path: main.clone(),
                original_bytes: Some(original_main.to_vec()),
                written_bytes: written_text.as_bytes().to_vec(),
            },
            GlobalStateWrite {
                path: backup.clone(),
                original_bytes: Some(original_backup.to_vec()),
                written_bytes: written_text.into_bytes(),
            },
        ];

        for write in writes.iter().rev() {
            restore_global_write(write).expect("restore global write");
        }
        assert_eq!(fs::read(&main).expect("read restored main"), original_main);
        assert_eq!(
            fs::read(&backup).expect("read restored backup"),
            original_backup
        );

        fs::remove_dir_all(dir).expect("remove test directory");
    }
}
