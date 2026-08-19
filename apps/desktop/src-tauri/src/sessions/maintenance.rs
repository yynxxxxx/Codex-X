use crate::error::{CodexxError, Result};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct DesktopLifecycleStatus {
    pub(super) was_running: bool,
    pub(super) restarted: bool,
    pub(super) warning: Option<String>,
}

pub(super) fn run_with_stopped_desktop<T, S>(
    stop: impl FnOnce() -> std::result::Result<S, String>,
    was_running: impl FnOnce(&S) -> bool,
    operation: impl FnOnce() -> Result<T>,
    restore: impl FnOnce(S) -> std::result::Result<(), String>,
) -> Result<(T, DesktopLifecycleStatus)> {
    let stop_state = stop().map_err(|error| {
        CodexxError::Config(format!(
            "无法安全关闭 Codex Desktop，会话操作已取消: {error}"
        ))
    })?;
    let desktop_was_running = was_running(&stop_state);
    let operation_result = operation();
    let restore_result = restore(stop_state);

    match operation_result {
        Ok(result) => {
            let warning = restore_result
                .err()
                .map(|error| format!("会话操作已完成，但 Codex Desktop 自动重新启动失败: {error}"));
            Ok((
                result,
                DesktopLifecycleStatus {
                    was_running: desktop_was_running,
                    restarted: desktop_was_running && warning.is_none(),
                    warning,
                },
            ))
        }
        Err(operation_error) => match restore_result {
            Ok(()) => Err(operation_error),
            Err(restart_error) => Err(CodexxError::Config(format!(
                "{operation_error}; Codex Desktop 也未能自动重新启动: {restart_error}"
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn stopped_desktop_is_restored_after_success() {
        let events = RefCell::new(Vec::new());
        let (value, status) = run_with_stopped_desktop(
            || {
                events.borrow_mut().push("stop");
                Ok(true)
            },
            |running| *running,
            || {
                events.borrow_mut().push("sync");
                Ok(7)
            },
            |running| {
                assert!(running);
                events.borrow_mut().push("start");
                Ok(())
            },
        )
        .expect("run maintenance");
        assert_eq!(value, 7);
        assert_eq!(events.into_inner(), ["stop", "sync", "start"]);
        assert_eq!(
            status,
            DesktopLifecycleStatus {
                was_running: true,
                restarted: true,
                warning: None,
            }
        );
    }

    #[test]
    fn desktop_that_was_not_running_is_not_reported_as_restarted() {
        let starts = RefCell::new(0);
        let (_, status) = run_with_stopped_desktop(
            || Ok(false),
            |running| *running,
            || Ok(()),
            |running| {
                assert!(!running);
                *starts.borrow_mut() += usize::from(running);
                Ok(())
            },
        )
        .expect("run maintenance");
        assert_eq!(*starts.borrow(), 0);
        assert!(!status.was_running);
        assert!(!status.restarted);
    }

    #[test]
    fn stop_failure_prevents_mutation() {
        let mutated = RefCell::new(false);
        let error = run_with_stopped_desktop::<(), ()>(
            || Err("close timeout".to_string()),
            |_| false,
            || {
                *mutated.borrow_mut() = true;
                Ok(())
            },
            |_| Ok(()),
        )
        .expect_err("stop must fail");
        assert!(!*mutated.borrow());
        assert!(error.to_string().contains("会话操作已取消"));
    }

    #[test]
    fn failed_sync_or_delete_still_attempts_restore() {
        for operation in ["sync", "delete"] {
            let restored = RefCell::new(false);
            let error = run_with_stopped_desktop(
                || Ok(true),
                |running| *running,
                || Err::<(), _>(CodexxError::Config(format!("{operation} failed"))),
                |_| {
                    *restored.borrow_mut() = true;
                    Ok(())
                },
            )
            .expect_err("operation must fail");
            assert!(*restored.borrow());
            assert!(error.to_string().contains(&format!("{operation} failed")));
        }
    }

    #[test]
    fn successful_mutation_keeps_success_when_restart_fails() {
        let (value, status) = run_with_stopped_desktop(
            || Ok(true),
            |running| *running,
            || Ok("mutated"),
            |_| Err("launch timeout".to_string()),
        )
        .expect("mutation remains successful");
        assert_eq!(value, "mutated");
        assert!(status.was_running);
        assert!(!status.restarted);
        assert!(status.warning.unwrap().contains("自动重新启动失败"));
    }

    #[test]
    fn operation_error_is_not_overwritten_by_restart_error() {
        let error = run_with_stopped_desktop(
            || Ok(true),
            |running| *running,
            || Err::<(), _>(CodexxError::Database("primary failure".to_string())),
            |_| Err("secondary failure".to_string()),
        )
        .expect_err("both stages fail");
        let message = error.to_string();
        assert!(message.contains("primary failure"));
        assert!(message.contains("secondary failure"));
        assert!(message.find("primary failure") < message.find("secondary failure"));
    }
}
