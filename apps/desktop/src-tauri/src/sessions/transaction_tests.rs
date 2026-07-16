use super::*;
use crate::sessions::backup::provider_sync_backup_root;
use crate::sessions::sync::sync_sessions_provider_with_hook;
use rusqlite::{Connection, OpenFlags};
use std::fs;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn temp_dir(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "codex-x-session-sync-{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create test directory");
    path
}

fn write_rollout(path: &Path, id: &str) -> Vec<u8> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create rollout parent");
    }
    let content = format!(
        "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"model_provider\":\"openai\",\"cwd\":\"/tmp/project\"}}}}\n"
    );
    fs::write(path, content.as_bytes()).expect("write rollout");
    content.into_bytes()
}

fn create_thread_database(path: &Path, id: &str, rollout: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create sqlite parent");
    }
    let conn = Connection::open(path).expect("create thread database");
    conn.execute_batch(
        "CREATE TABLE threads (
            id TEXT PRIMARY KEY,
            model_provider TEXT NOT NULL,
            rollout_path TEXT
         );",
    )
    .expect("create threads table");
    conn.execute(
        "INSERT INTO threads (id, model_provider, rollout_path)
         VALUES (?1, 'openai', ?2)",
        (id, rollout.display().to_string()),
    )
    .expect("insert thread");
}

fn thread_provider(path: &Path, id: &str) -> String {
    Connection::open(path)
        .expect("open thread database")
        .query_row(
            "SELECT model_provider FROM threads WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .expect("read thread provider")
}

fn sqlite_quick_check(path: &Path) -> String {
    Connection::open(path)
        .expect("open sqlite for quick check")
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .expect("run quick check")
}

#[test]
fn sqlite_update_failure_rolls_back_every_open_database_and_jsonl() {
    let codex_dir = temp_dir("multi-sqlite-update-failure");
    let id = "019f6000-0000-7000-8000-000000000401";
    let rollout = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
    let original_rollout = write_rollout(&rollout, id);
    let first = codex_dir.join("sqlite/custom.db");
    let second = codex_dir.join("state_10.sqlite");
    create_thread_database(&first, id, &rollout);
    create_thread_database(&second, id, &rollout);
    Connection::open(&second)
        .expect("open failing database")
        .execute_batch(
            "CREATE TRIGGER reject_provider_update
             BEFORE UPDATE OF model_provider ON threads
             BEGIN SELECT RAISE(ABORT, 'provider update blocked'); END;",
        )
        .expect("install rejecting trigger");

    let error = sync_sessions_provider_with_hook(
        Some(codex_dir.display().to_string()),
        Some("custom".to_string()),
        |_| Ok(()),
    )
    .expect_err("second database update must fail");
    assert!(error.to_string().contains("provider update blocked"));
    assert_eq!(thread_provider(&first, id), "openai");
    assert_eq!(thread_provider(&second, id), "openai");
    assert_eq!(
        fs::read(&rollout).expect("read restored rollout"),
        original_rollout
    );

    fs::remove_dir_all(codex_dir).expect("remove test directory");
}

#[test]
fn sqlite_commit_failpoint_restores_committed_and_uncommitted_databases() {
    let codex_dir = temp_dir("sqlite-commit-failpoint");
    let id = "019f6000-0000-7000-8000-000000000406";
    let rollout = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
    let original_rollout = write_rollout(&rollout, id);
    let first = codex_dir.join("sqlite/custom.db");
    let second = codex_dir.join("state_10.sqlite");
    create_thread_database(&first, id, &rollout);
    create_thread_database(&second, id, &rollout);

    let error = sync_sessions_provider_with_hook(
        Some(codex_dir.display().to_string()),
        Some("custom".to_string()),
        |point| match point {
            MutationPoint::AfterSqliteCommit(0) => {
                Err(CodexxError::Config("提交后注入失败".to_string()))
            }
            _ => Ok(()),
        },
    )
    .expect_err("fail after first sqlite commit");

    assert_eq!(error.to_string(), "配置错误: 提交后注入失败");
    assert_eq!(thread_provider(&first, id), "openai");
    assert_eq!(thread_provider(&second, id), "openai");
    assert_eq!(
        fs::read(&rollout).expect("read restored rollout"),
        original_rollout
    );

    fs::remove_dir_all(codex_dir).expect("remove test directory");
}

#[test]
fn failed_sqlite_commit_rolls_back_without_snapshot_restore() {
    let codex_dir = temp_dir("failed-sqlite-commit");
    let id = "019f6000-0000-7000-8000-000000000407";
    let rollout = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
    write_rollout(&rollout, id);
    let database = codex_dir.join("state_10.sqlite");
    create_thread_database(&database, id, &rollout);
    Connection::open(&database)
        .expect("open database for deferred constraint")
        .execute_batch(
            "CREATE TABLE parent (id TEXT PRIMARY KEY);
             CREATE TABLE child (
                parent_id TEXT REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED
             );",
        )
        .expect("create deferred foreign key");

    let conn = Connection::open(&database).expect("open writer");
    conn.pragma_update(None, "foreign_keys", true)
        .expect("enable foreign keys");
    conn.execute_batch(
        "BEGIN IMMEDIATE;
         INSERT INTO child (parent_id) VALUES ('missing');",
    )
    .expect("defer invalid foreign key until commit");
    let observer = Connection::open_with_flags(
        &database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("open observer");
    let mut pending = vec![PendingSqliteUpdate {
        path: database.clone(),
        conn,
        observer,
        columns: HashSet::new(),
        counts: SqliteUpdateCounts::default(),
        transaction_open: true,
    }];
    let mut journal = MutationJournal::default();

    let error = commit_sqlite_updates(&mut pending, &mut journal, &mut |_| Ok(()))
        .expect_err("deferred foreign key must reject commit");

    assert!(error.to_string().contains("FOREIGN KEY constraint failed"));
    assert!(journal.sqlite_restore_attempts.is_empty());
    assert!(!pending[0].transaction_open);
    let child_rows: i64 = Connection::open(&database)
        .expect("reopen database")
        .query_row("SELECT COUNT(*) FROM child", [], |row| row.get(0))
        .expect("count rolled-back child rows");
    assert_eq!(child_rows, 0);

    drop(pending);
    fs::remove_dir_all(codex_dir).expect("remove test directory");
}

#[test]
fn sqlite_restore_does_not_overwrite_new_codex_writes() {
    let codex_dir = temp_dir("sqlite-concurrent-write");
    let id = "019f6000-0000-7000-8000-000000000409";
    let rollout = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
    let original_rollout = write_rollout(&rollout, id);
    let database = codex_dir.join("state_10.sqlite");
    create_thread_database(&database, id, &rollout);

    let error = sync_sessions_provider_with_hook(
        Some(codex_dir.display().to_string()),
        Some("custom".to_string()),
        |point| match point {
            MutationPoint::AfterSqliteCommit(0) => {
                Connection::open(&database)
                    .expect("open concurrent Codex writer")
                    .execute_batch(
                        "CREATE TABLE concurrent_marker (value TEXT NOT NULL);
                         INSERT INTO concurrent_marker (value) VALUES ('keep-new-write');",
                    )
                    .expect("write after provider sync commit");
                Err(CodexxError::Config("并发写入后注入失败".to_string()))
            }
            _ => Ok(()),
        },
    )
    .expect_err("recovery must detect the concurrent write");

    assert!(error.to_string().contains("会话数据库已发生变化"));
    assert_eq!(thread_provider(&database, id), "custom");
    let marker: String = Connection::open(&database)
        .expect("open database with concurrent write")
        .query_row("SELECT value FROM concurrent_marker", [], |row| row.get(0))
        .expect("read concurrent marker");
    assert_eq!(marker, "keep-new-write");
    assert_eq!(
        fs::read(&rollout).expect("read restored rollout"),
        original_rollout
    );
    assert!(fs::read_dir(provider_sync_backup_root(&codex_dir))
        .expect("read retained provider sync backup")
        .next()
        .is_some());

    fs::remove_dir_all(codex_dir).expect("remove test directory");
}

#[test]
fn sqlite_restore_rechecks_after_waiting_for_a_writer() {
    let codex_dir = temp_dir("sqlite-writer-during-restore");
    let id = "019f6000-0000-7000-8000-000000000412";
    let rollout = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
    write_rollout(&rollout, id);
    let database = codex_dir.join("state_10.sqlite");
    create_thread_database(&database, id, &rollout);
    let mut writer = None;

    let error = sync_sessions_provider_with_hook(
        Some(codex_dir.display().to_string()),
        Some("custom".to_string()),
        |point| match point {
            MutationPoint::AfterSqliteCommit(0) => {
                let writer_database = database.clone();
                let (ready_tx, ready_rx) = mpsc::channel();
                writer = Some(thread::spawn(move || {
                    let conn = Connection::open(writer_database).expect("open concurrent writer");
                    conn.execute_batch(
                        "BEGIN IMMEDIATE;
                         CREATE TABLE concurrent_during_restore (value TEXT NOT NULL);
                         INSERT INTO concurrent_during_restore (value)
                         VALUES ('keep-writer-commit');",
                    )
                    .expect("stage concurrent write");
                    ready_tx.send(()).expect("signal writer lock");
                    thread::sleep(Duration::from_millis(150));
                    conn.execute_batch("COMMIT")
                        .expect("commit while recovery waits");
                }));
                ready_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("wait for concurrent writer lock");
                Err(CodexxError::Config("持锁写入期间注入失败".to_string()))
            }
            _ => Ok(()),
        },
    )
    .expect_err("recovery must recheck after waiting for the writer");

    writer
        .take()
        .expect("writer handle")
        .join()
        .expect("join concurrent writer");
    assert!(error.to_string().contains("会话数据库已发生变化"));
    assert_eq!(thread_provider(&database, id), "custom");
    let marker: String = Connection::open(&database)
        .expect("open database after concurrent commit")
        .query_row("SELECT value FROM concurrent_during_restore", [], |row| {
            row.get(0)
        })
        .expect("read concurrent marker");
    assert_eq!(marker, "keep-writer-commit");

    fs::remove_dir_all(codex_dir).expect("remove test directory");
}

#[cfg(unix)]
#[test]
fn sqlite_prepare_deduplicates_symlink_aliases() {
    use std::os::unix::fs::symlink;

    let codex_dir = temp_dir("sqlite-symlink-alias");
    let id = "019f6000-0000-7000-8000-000000000410";
    let rollout = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
    write_rollout(&rollout, id);
    let database = codex_dir.join("state_10.sqlite");
    let alias = codex_dir.join("state-alias.sqlite");
    create_thread_database(&database, id, &rollout);
    symlink(&database, &alias).expect("create database symlink");

    let mut pending =
        prepare_sqlite_updates(&[database.clone(), alias]).expect("prepare aliased database once");

    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].path,
        database.canonicalize().expect("canonical db")
    );
    rollback_open_transactions(&mut pending);

    fs::remove_dir_all(codex_dir).expect("remove test directory");
}

#[test]
fn global_main_failpoint_restores_prior_mutations_without_touching_backup() {
    let codex_dir = temp_dir("global-main-failpoint");
    let id = "019f6000-0000-7000-8000-000000000408";
    let rollout = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
    let original_rollout = write_rollout(&rollout, id);
    let database = codex_dir.join("state_10.sqlite");
    create_thread_database(&database, id, &rollout);
    let global = codex_dir.join(".codex-global-state.json");
    let global_backup = codex_dir.join(".codex-global-state.json.bak");
    let original_global = br#"{"electron-saved-workspace-roots":"/tmp/project"}"#;
    fs::write(&global, original_global).expect("write original global state");
    assert!(!global_backup.exists());

    let error = sync_sessions_provider_with_hook(
        Some(codex_dir.display().to_string()),
        Some("custom".to_string()),
        |point| match point {
            MutationPoint::AfterGlobalMainWrite => {
                assert_eq!(thread_provider(&database, id), "openai");
                assert!(fs::read_to_string(&rollout)
                    .expect("read mutated rollout")
                    .contains("\"model_provider\":\"custom\""));
                assert!(!global_backup.exists());
                Err(CodexxError::Config("主状态写入后注入失败".to_string()))
            }
            _ => Ok(()),
        },
    )
    .expect_err("fail after global main write");

    assert_eq!(error.to_string(), "配置错误: 主状态写入后注入失败");
    assert_eq!(thread_provider(&database, id), "openai");
    assert_eq!(
        fs::read(&rollout).expect("read restored rollout"),
        original_rollout
    );
    assert_eq!(
        fs::read(&global).expect("read restored global"),
        original_global
    );
    assert!(!global_backup.exists());

    fs::remove_dir_all(codex_dir).expect("remove test directory");
}

#[test]
fn injected_failure_restores_sqlite_jsonl_and_global_state() {
    let codex_dir = temp_dir("full-mutation-rollback");
    let id = "019f6000-0000-7000-8000-000000000411";
    let rollout = codex_dir.join(format!("sessions/rollout-test-{id}.jsonl"));
    write_rollout(&rollout, id);
    let mut rollout_file = fs::OpenOptions::new()
        .append(true)
        .open(&rollout)
        .expect("open rollout for user event");
    writeln!(
        rollout_file,
        "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"hello\"}}}}"
    )
    .expect("append user event");
    drop(rollout_file);
    let original_rollout = fs::read(&rollout).expect("read original rollout");
    let database = codex_dir.join("state_10.sqlite");
    create_thread_database(&database, id, &rollout);
    let wal_guard = Connection::open(&database).expect("open database for WAL mode");
    wal_guard
        .pragma_update(None, "journal_mode", "WAL")
        .expect("enable WAL mode");
    wal_guard
        .execute_batch(
            "ALTER TABLE threads ADD COLUMN has_user_event INTEGER DEFAULT 0;
             ALTER TABLE threads ADD COLUMN cwd TEXT;
             UPDATE threads SET cwd = '/tmp/wrong';
             CREATE TABLE rollback_marker (value TEXT NOT NULL);
             INSERT INTO rollback_marker (value) VALUES ('keep-me');",
        )
        .expect("write WAL marker");
    let wal_path = PathBuf::from(format!("{}-wal", database.display()));
    assert!(wal_path.exists());
    let global = codex_dir.join(".codex-global-state.json");
    let global_backup = codex_dir.join(".codex-global-state.json.bak");
    let original_global = br#"{
  "electron-saved-workspace-roots": "/tmp/project",
  "unrelated-setting": true
}"#;
    fs::write(&global, original_global).expect("write original global state");
    assert!(!global_backup.exists());

    let mut hook_called = false;
    let error = sync_sessions_provider_with_hook(
        Some(codex_dir.display().to_string()),
        Some("custom".to_string()),
        |point| match point {
            MutationPoint::AfterSqliteCommit(0) => {
                hook_called = true;
                assert_eq!(thread_provider(&database, id), "custom");
                let user_event_flag: i64 = Connection::open(&database)
                    .expect("open committed session metadata")
                    .query_row(
                        "SELECT has_user_event FROM threads WHERE id = ?1",
                        [id],
                        |row| row.get(0),
                    )
                    .expect("read committed user event flag");
                assert_eq!(user_event_flag, 0);
                assert!(fs::read_to_string(&rollout)
                    .expect("read mutated rollout")
                    .contains("\"model_provider\":\"custom\""));
                let main = fs::read_to_string(&global).expect("read mutated global state");
                let backup =
                    fs::read_to_string(&global_backup).expect("read mutated global backup");
                assert_eq!(main, backup);
                Err(CodexxError::Config("测试注入失败".to_string()))
            }
            _ => Ok(()),
        },
    )
    .expect_err("hook must fail after all writes");

    assert!(hook_called);
    assert_eq!(error.to_string(), "配置错误: 测试注入失败");
    assert_eq!(thread_provider(&database, id), "openai");
    let restored_index: (i64, String) = Connection::open(&database)
        .expect("open restored session metadata")
        .query_row(
            "SELECT has_user_event, cwd FROM threads WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read restored session metadata");
    assert_eq!(restored_index, (0, "/tmp/wrong".to_string()));
    assert_eq!(sqlite_quick_check(&database), "ok");
    let marker: String = Connection::open(&database)
        .expect("open restored database")
        .query_row("SELECT value FROM rollback_marker", [], |row| row.get(0))
        .expect("read restored WAL marker");
    assert_eq!(marker, "keep-me");
    assert_eq!(
        fs::read(&rollout).expect("read restored rollout"),
        original_rollout
    );
    assert_eq!(
        fs::read(&global).expect("read restored global"),
        original_global
    );
    assert!(!global_backup.exists());
    assert!(fs::read_dir(provider_sync_backup_root(&codex_dir))
        .expect("read retained provider sync backup")
        .next()
        .is_some());

    drop(wal_guard);
    fs::remove_dir_all(codex_dir).expect("remove test directory");
}
