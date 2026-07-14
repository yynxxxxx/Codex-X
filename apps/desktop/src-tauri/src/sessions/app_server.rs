use crate::error::Result;
use crate::platform;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub(super) struct OfficialSessionDeleteOutcome {
    pub(super) deleted_ids: HashSet<String>,
    pub(super) completed_roots: HashSet<String>,
    pub(super) failed_roots: Vec<(String, String)>,
}

#[derive(Debug)]
enum AppServerDeleteAttempt {
    Success(OfficialSessionDeleteOutcome),
    Unsupported(String),
}

fn send_app_server_message(
    stdin: &mut impl Write,
    value: &Value,
) -> std::result::Result<(), String> {
    let mut line = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    line.push(b'\n');
    stdin.write_all(&line).map_err(|error| error.to_string())?;
    stdin.flush().map_err(|error| error.to_string())
}

fn recv_app_server_message(
    receiver: &mpsc::Receiver<std::io::Result<String>>,
    deadline: Instant,
) -> std::result::Result<Value, String> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| "Codex App Server 响应超时".to_string())?;
        let line = receiver
            .recv_timeout(remaining)
            .map_err(|error| format!("Codex App Server 响应失败: {error}"))?
            .map_err(|error| format!("读取 Codex App Server 输出失败: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }
        return serde_json::from_str(&line)
            .map_err(|error| format!("解析 Codex App Server 输出失败: {error}"));
    }
}

fn app_server_error(value: &Value) -> Option<(i64, String)> {
    let error = value.get("error")?;
    Some((
        error
            .get("code")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Codex App Server 返回未知错误")
            .to_string(),
    ))
}

fn stop_app_server_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn run_app_server_delete_attempt(
    mut child: Child,
    codex_dir: &Path,
    roots: &[String],
) -> AppServerDeleteAttempt {
    let Some(mut stdin) = child.stdin.take() else {
        stop_app_server_child(&mut child);
        return AppServerDeleteAttempt::Unsupported("Codex App Server stdin 不可用".to_string());
    };
    let Some(stdout) = child.stdout.take() else {
        stop_app_server_child(&mut child);
        return AppServerDeleteAttempt::Unsupported("Codex App Server stdout 不可用".to_string());
    };
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let result = (|| -> AppServerDeleteAttempt {
        let initialize_id = 1i64;
        let initialize = json!({
            "id": initialize_id,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "codex_x",
                    "title": "Codex-X",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": null
            }
        });
        if let Err(error) = send_app_server_message(&mut stdin, &initialize) {
            return AppServerDeleteAttempt::Unsupported(error);
        }
        let initialize_deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let message = match recv_app_server_message(&receiver, initialize_deadline) {
                Ok(message) => message,
                Err(error) => return AppServerDeleteAttempt::Unsupported(error),
            };
            if message.get("id").and_then(Value::as_i64) != Some(initialize_id) {
                continue;
            }
            if let Some((_, message)) = app_server_error(&message) {
                return AppServerDeleteAttempt::Unsupported(message);
            }
            let Some(server_home) = message
                .get("result")
                .and_then(|value| value.get("codexHome"))
                .and_then(Value::as_str)
            else {
                return AppServerDeleteAttempt::Unsupported(
                    "Codex App Server 未返回 CODEX_HOME".to_string(),
                );
            };
            let requested_home = codex_dir
                .canonicalize()
                .unwrap_or_else(|_| codex_dir.to_path_buf());
            let returned_home = PathBuf::from(server_home)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(server_home));
            if requested_home != returned_home {
                return AppServerDeleteAttempt::Unsupported(format!(
                    "Codex App Server 使用了不同的 CODEX_HOME: {}",
                    returned_home.display()
                ));
            }
            break;
        }
        if let Err(error) = send_app_server_message(&mut stdin, &json!({"method": "initialized"})) {
            return AppServerDeleteAttempt::Unsupported(error);
        }

        let mut outcome = OfficialSessionDeleteOutcome::default();
        'delete_roots: for (index, root) in roots.iter().enumerate() {
            let request_id = 1000 + index as i64;
            if let Err(error) = send_app_server_message(
                &mut stdin,
                &json!({
                    "id": request_id,
                    "method": "thread/delete",
                    "params": { "threadId": root }
                }),
            ) {
                for pending in &roots[index..] {
                    outcome.failed_roots.push((pending.clone(), error.clone()));
                }
                break 'delete_roots;
            }
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                let message = match recv_app_server_message(&receiver, deadline) {
                    Ok(message) => message,
                    Err(error) => {
                        for pending in &roots[index..] {
                            outcome.failed_roots.push((pending.clone(), error.clone()));
                        }
                        break 'delete_roots;
                    }
                };
                if message.get("method").and_then(Value::as_str) == Some("thread/deleted") {
                    if let Some(id) = message
                        .get("params")
                        .and_then(|value| value.get("threadId"))
                        .and_then(Value::as_str)
                    {
                        outcome.deleted_ids.insert(id.to_string());
                    }
                }
                if message.get("id").and_then(Value::as_i64) == Some(request_id) {
                    if let Some((code, error)) = app_server_error(&message) {
                        let lower = error.to_ascii_lowercase();
                        if code == -32601 {
                            return AppServerDeleteAttempt::Unsupported(error);
                        }
                        if code == -32600
                            && (lower.contains("no rollout")
                                || lower.contains("not found")
                                || lower.contains("does not exist"))
                        {
                            outcome.completed_roots.insert(root.clone());
                            break;
                        }
                        outcome.failed_roots.push((root.clone(), error));
                        break;
                    }
                    outcome.completed_roots.insert(root.clone());
                    break;
                }
            }
        }
        AppServerDeleteAttempt::Success(outcome)
    })();

    drop(stdin);
    stop_app_server_child(&mut child);
    let _ = reader.join();
    result
}

pub(super) fn delete_sessions_via_codex_app_server(
    codex_dir: &Path,
    roots: &[String],
) -> Result<Option<OfficialSessionDeleteOutcome>> {
    let mut unsupported_messages = Vec::new();
    for program in platform::codex_executable_candidates() {
        let is_bare_command = program.components().count() == 1;
        if !is_bare_command && !program.is_file() {
            continue;
        }
        let mut command = platform::program_command(&program, &["app-server", "--stdio"]);
        command
            .env("CODEX_HOME", codex_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                unsupported_messages.push(format!("{}: {error}", program.display()));
                continue;
            }
        };
        match run_app_server_delete_attempt(child, codex_dir, roots) {
            AppServerDeleteAttempt::Success(outcome) => return Ok(Some(outcome)),
            AppServerDeleteAttempt::Unsupported(message) => {
                unsupported_messages.push(format!("{}: {message}", program.display()));
            }
        }
    }
    let _ = unsupported_messages;
    Ok(None)
}
