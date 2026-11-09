//! Read display names independently of the first user message stored in title.
use super::types::SessionPreview;
use crate::sqlite_utils::{sqlite_has_table, table_column_set};
use rusqlite::{params_from_iter, Connection, OpenFlags};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

const MAX_INDEX_LINE_BYTES: usize = 64 * 1024;

fn index_names(codex_dir: &Path, ids: &HashSet<String>) -> HashMap<String, String> {
    let mut names = HashMap::new();
    let Ok(file) = File::open(codex_dir.join("session_index.jsonl")) else {
        return names;
    };
    let mut reader = BufReader::new(file);
    loop {
        let mut line = Vec::new();
        let Ok(count) = reader
            .by_ref()
            .take((MAX_INDEX_LINE_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)
        else {
            break;
        };
        if count == 0 {
            break;
        }
        if count > MAX_INDEX_LINE_BYTES {
            if !line.ends_with(b"\n") && reader.skip_until(b'\n').is_err() {
                break;
            }
            continue;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(id) = value.get("id").and_then(|value| value.as_str()) else {
            continue;
        };
        let Some(name) = value.get("thread_name").and_then(|value| value.as_str()) else {
            continue;
        };
        if ids.contains(id) && !name.trim().is_empty() {
            // The index is append-only: the last valid rename wins.
            names.insert(id.to_string(), name.trim().to_string());
        }
    }
    names
}

fn database_names(
    paths: &[PathBuf],
    ids: &HashSet<String>,
    table: &str,
    id_column: &str,
    name_column: &str,
) -> HashMap<String, String> {
    let mut names: HashMap<String, (String, Option<f64>)> = HashMap::new();
    let prefer_latest_catalog = table == "local_thread_catalog";
    let ids = ids.iter().collect::<Vec<_>>();
    for path in paths {
        let Ok(conn) = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) else {
            continue;
        };
        let _ = conn.busy_timeout(std::time::Duration::from_millis(50));
        if !sqlite_has_table(&conn, table).unwrap_or(false) {
            continue;
        }
        let Ok(columns) = table_column_set(&conn, table) else {
            continue;
        };
        if !columns.contains(id_column) || !columns.contains(name_column) {
            continue;
        }
        let source_updated_at = if prefer_latest_catalog && columns.contains("source_updated_at") {
            "source_updated_at"
        } else {
            "NULL"
        };
        let order = if prefer_latest_catalog && columns.contains("source_updated_at") {
            " ORDER BY source_updated_at DESC"
        } else {
            ""
        };
        for chunk in ids.chunks(400) {
            let query = format!(
                "SELECT \"{id_column}\", \"{name_column}\", {source_updated_at} FROM \"{table}\" WHERE \"{id_column}\" IN ({}){order}",
                vec!["?"; chunk.len()].join(",")
            );
            let Ok(mut statement) = conn.prepare(&query) else {
                continue;
            };
            let Ok(rows) = statement.query_map(params_from_iter(chunk.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<f64>>(2)
                        .ok()
                        .flatten()
                        .filter(|value| value.is_finite()),
                ))
            }) else {
                continue;
            };
            for (id, name, updated_at) in rows.flatten() {
                if let Some(name) = name.filter(|name| !name.trim().is_empty()) {
                    let replace = names.get(&id).is_none_or(|(_, previous_time)| {
                        prefer_latest_catalog
                            && updated_at.is_some_and(|time| {
                                previous_time.is_none_or(|previous| time > previous)
                            })
                    });
                    if replace {
                        names.insert(id, (name.trim().to_string(), updated_at));
                    }
                }
            }
        }
    }
    names
        .into_iter()
        .map(|(id, (name, _))| (id, name))
        .collect()
}

pub(super) fn apply_display_names(
    codex_dir: &Path,
    database_paths: &[PathBuf],
    sessions: &mut [SessionPreview],
) {
    let ids = sessions.iter().map(|session| session.id.clone()).collect();
    let mut names = database_names(
        database_paths,
        &ids,
        "local_thread_catalog",
        "thread_id",
        "display_title",
    );
    names.extend(index_names(codex_dir, &ids));
    // An explicit name in the current thread index is authoritative over an
    // older desktop catalog or CLI index entry.
    names.extend(database_names(
        database_paths,
        &ids,
        "threads",
        "id",
        "name",
    ));
    for session in sessions {
        if let Some(name) = names.remove(&session.id) {
            session.title = name;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_thread_name_then_latest_index_then_catalog_without_changing_first_message() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("state_5.sqlite");
        let conn = Connection::open(&database).unwrap();
        conn.execute_batch("CREATE TABLE threads(id TEXT, name TEXT, title TEXT, first_user_message TEXT); INSERT INTO threads VALUES ('named','Current name','Initial prompt','Initial prompt'),('indexed',NULL,'Initial prompt','Initial prompt'),('catalog',NULL,'Initial prompt','Initial prompt'); CREATE TABLE local_thread_catalog(thread_id TEXT, display_title TEXT, source_updated_at INTEGER); INSERT INTO local_thread_catalog VALUES ('named','Old catalog name',1),('indexed','Old catalog name',1),('catalog','Older name',1),('catalog','Catalog name',2);").unwrap();
        std::fs::write(directory.path().join("session_index.jsonl"), "{\"id\":\"indexed\",\"thread_name\":\"Old name\"}\ninvalid\n{\"id\":\"named\",\"thread_name\":\"Old index name\"}\n{\"id\":\"indexed\",\"thread_name\":\"Renamed session\"}\n").unwrap();
        let (mut sessions, _) = super::super::storage::list_session_previews_with_paths(
            std::slice::from_ref(&database),
            &super::super::types::RolloutScan::default(),
            "openai",
            100,
        )
        .unwrap();
        apply_display_names(directory.path(), &[database], &mut sessions);
        let sessions = sessions
            .into_iter()
            .map(|session| (session.id.clone(), session))
            .collect::<HashMap<_, _>>();
        assert_eq!(sessions["named"].title, "Current name");
        assert_eq!(sessions["indexed"].title, "Renamed session");
        assert_eq!(sessions["catalog"].title, "Catalog name");
        assert!(sessions
            .values()
            .all(|session| session.first_user_message.as_deref() == Some("Initial prompt")));
        assert_eq!(
            conn.query_row("SELECT title FROM threads WHERE id='indexed'", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
            "Initial prompt"
        );
    }

    #[test]
    fn oversized_or_malformed_index_entries_do_not_hide_later_renames() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session_index.jsonl");
        let mut content = vec![b'x'; MAX_INDEX_LINE_BYTES + 100];
        content.extend_from_slice(b"\n{\"id\":\"wanted\",\"thread_name\":\" Valid name \"}\n{\"id\":\"wanted\",\"thread_name\":\" \"}\n");
        std::fs::write(&path, content).unwrap();
        let names = index_names(directory.path(), &HashSet::from(["wanted".into()]));
        assert_eq!(names["wanted"], "Valid name");
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn catalogs_choose_the_newest_source_timestamp_across_databases_and_active_first_on_ties() {
        let directory = tempfile::tempdir().unwrap();
        let active = directory.path().join("state_5.sqlite");
        let legacy = directory.path().join("codex-dev.db");
        Connection::open(&active).unwrap().execute_batch("CREATE TABLE threads(id TEXT, title TEXT); INSERT INTO threads VALUES ('newest','Initial prompt'),('tied','Initial prompt'); CREATE TABLE local_thread_catalog(thread_id TEXT, display_title TEXT, source_updated_at INTEGER); INSERT INTO local_thread_catalog VALUES ('newest','Older active catalog',1),('tied','Active catalog wins tie',7);").unwrap();
        Connection::open(&legacy).unwrap().execute_batch("CREATE TABLE local_thread_catalog(thread_id TEXT, display_title TEXT, source_updated_at INTEGER); INSERT INTO local_thread_catalog VALUES ('newest','Latest catalog title',9),('tied','Legacy catalog loses tie',7);").unwrap();
        let discovery = super::super::storage::discover_sqlite_databases(directory.path());
        assert_eq!(discovery.active_paths, vec![active.clone()]);
        assert_eq!(discovery.active_first_related_paths()[0], active);
        let (mut sessions, _) = super::super::storage::list_session_previews_with_paths(
            &discovery.active_paths,
            &super::super::types::RolloutScan::default(),
            "openai",
            100,
        )
        .unwrap();
        apply_display_names(
            directory.path(),
            &discovery.active_first_related_paths(),
            &mut sessions,
        );
        let names = sessions
            .into_iter()
            .map(|session| (session.id, session.title))
            .collect::<HashMap<_, _>>();
        assert_eq!(names["newest"], "Latest catalog title");
        assert_eq!(names["tied"], "Active catalog wins tie");
    }
}
