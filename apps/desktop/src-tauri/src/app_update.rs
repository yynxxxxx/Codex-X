//! Keep downloads and verification in the signed Tauri updater, but own the
//! Windows handoff. The stock updater exits immediately and skips our routing
//! shutdown; it also cannot report installation progress after that exit.

use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{ipc::Channel, Manager, ResourceId, Webview};
use tauri_plugin_updater::Update;

#[cfg(any(test, target_os = "windows"))]
#[path = "windows_update.rs"]
mod windows;

static UPDATING: AtomicBool = AtomicBool::new(false);
// 0: not installing, 1: installer preparation/write, 2: Windows handoff.
static INSTALLATION_PHASE: AtomicU8 = AtomicU8::new(0);
const DOWNLOAD_TIMEOUT_MS: u64 = 10 * 60 * 1000;

struct InstallationExitGuard<'a>(&'a AtomicU8);
impl<'a> InstallationExitGuard<'a> {
    fn begin(phase: &'a AtomicU8) -> Self {
        phase.store(1, Ordering::Release);
        Self(phase)
    }
    #[cfg(any(test, target_os = "windows"))]
    fn handed_off(self) {
        self.0.store(2, Ordering::Release);
        std::mem::forget(self);
    }
}
impl Drop for InstallationExitGuard<'_> {
    fn drop(&mut self) {
        self.0.store(0, Ordering::Release);
    }
}

pub(crate) fn defer_exit_during_installation() -> bool {
    INSTALLATION_PHASE.load(Ordering::Acquire) == 1
}

#[derive(Clone, Serialize)]
#[serde(tag = "event", content = "data")]
pub(crate) enum AppUpdateEvent {
    #[serde(rename_all = "camelCase")]
    Started {
        content_length: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    Progress {
        chunk_length: usize,
    },
    Verifying,
    #[allow(dead_code)] // Emitted by the Windows handoff; other platforms install in place.
    Preparing,
    Installing,
    #[allow(dead_code)]
    HandedOff,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FailureStage {
    Download,
    Verify,
    Prepare,
    Install,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateFailure {
    stage: FailureStage,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    log_path: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstallResult {
    restart_required: bool,
}

struct UpdateLease<'a>(&'a AtomicBool);
impl<'a> UpdateLease<'a> {
    fn acquire(flag: &'a AtomicBool) -> Result<Self, UpdateFailure> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                failure(
                    FailureStage::Prepare,
                    "已有更新正在进行，请等待当前操作完成。",
                    None,
                )
            })?;
        Ok(Self(flag))
    }
    #[cfg(target_os = "windows")]
    fn handoff(self) {
        // Never permit a second installer while the original app is exiting.
        std::mem::forget(self);
    }
}
impl Drop for UpdateLease<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn failure(stage: FailureStage, message: &str, log: Option<&UpdateLog>) -> UpdateFailure {
    UpdateFailure {
        stage,
        message: message.into(),
        log_path: log.map(|log| log.path.to_string_lossy().into_owned()),
    }
}

/// Check before changing the installed application on every platform. The
/// converted supplier cannot resume through a raw Responses endpoint after exit.
fn install_with_route_preflight<T>(
    check: impl FnOnce() -> Result<(), ()>,
    install: impl FnOnce() -> Result<T, UpdateFailure>,
    log: &UpdateLog,
) -> Result<T, UpdateFailure> {
    if check().is_err() {
        log.record("routing_recovery_preflight_failed");
        return Err(failure(
            FailureStage::Prepare,
            "更新前未能确认连接配置可恢复，安装尚未开始。请检查路由配置后重试。",
            Some(log),
        ));
    }
    install()
}

/// This log contains only our fixed stage names and version, never URLs,
/// headers, auth/config contents or raw network/parser error messages.
struct UpdateLog {
    path: PathBuf,
    file: Mutex<File>,
}
impl UpdateLog {
    fn create(dir: &Path, version: &str) -> std::io::Result<Self> {
        let version = semver::Version::parse(version).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid version")
        })?;
        fs::create_dir_all(dir)?;
        let mut nonce = [0u8; 8];
        getrandom::getrandom(&mut nonce)
            .map_err(|_| std::io::Error::other("random source unavailable"))?;
        let nonce: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
        let path = dir.join(format!(
            "update-{}-{}-{nonce}.log",
            chrono::Utc::now().format("%Y%m%d-%H%M%S"),
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = Mutex::new(options.open(&path)?);
        let log = Self { path, file };
        log.record(&format!("target_version={version}"));
        Ok(log)
    }
    fn record(&self, stage: &str) {
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(file, "{} {stage}", chrono::Utc::now().to_rfc3339());
            let _ = file.flush();
        }
    }
}

fn timeout_ms(value: Option<u64>) -> u64 {
    value
        .unwrap_or(DOWNLOAD_TIMEOUT_MS)
        .clamp(30_000, DOWNLOAD_TIMEOUT_MS)
}

enum DownloadAttemptFailure {
    Transport,
    Verification,
}

async fn download_attempt(
    update: &Update,
    on_event: &Channel<AppUpdateEvent>,
    log: &UpdateLog,
) -> Result<Vec<u8>, DownloadAttemptFailure> {
    let finished = AtomicBool::new(false);
    let mut started = false;
    update
        .download(
            |chunk_length, content_length| {
                if !started {
                    started = true;
                    let _ = on_event.send(AppUpdateEvent::Started { content_length });
                }
                let _ = on_event.send(AppUpdateEvent::Progress { chunk_length });
            },
            || {
                finished.store(true, Ordering::Release);
                log.record("verifying_signature");
                let _ = on_event.send(AppUpdateEvent::Verifying);
            },
        )
        .await
        .map_err(|_| {
            if finished.load(Ordering::Acquire) {
                DownloadAttemptFailure::Verification
            } else {
                DownloadAttemptFailure::Transport
            }
        })
}

async fn download_with_recovery(
    update: &Update,
    mut candidates: Vec<reqwest::Url>,
    on_event: &Channel<AppUpdateEvent>,
    log: &UpdateLog,
) -> Result<Vec<u8>, UpdateFailure> {
    match download_attempt(update, on_event, log).await {
        Ok(bytes) => return Ok(bytes),
        Err(DownloadAttemptFailure::Verification) => {
            log.record("signature_verification_failed");
            return Err(failure(
                FailureStage::Verify,
                "更新包校验未通过，未启动安装。请重新检查在线更新。",
                Some(log),
            ));
        }
        Err(DownloadAttemptFailure::Transport) => log.record("primary_download_transport_failed"),
    }
    if candidates.is_empty() {
        match crate::app_update_channel::discover_download_candidates(update).await {
            Ok(found) => candidates = found,
            Err(_) => log.record("official_api_asset_lookup_failed"),
        }
    }
    for candidate in candidates {
        let alternate = match crate::app_update_channel::api_download_update(update, &candidate) {
            Ok(alternate) => alternate,
            Err(_) => continue,
        };
        log.record("retrying_download_via_official_api");
        match download_attempt(&alternate, on_event, log).await {
            Ok(bytes) => {
                log.record("official_api_download_verified");
                return Ok(bytes);
            }
            Err(DownloadAttemptFailure::Verification) => {
                log.record("signature_verification_failed");
                return Err(failure(
                    FailureStage::Verify,
                    "更新包校验未通过，未启动安装。请重新检查在线更新。",
                    Some(log),
                ));
            }
            Err(DownloadAttemptFailure::Transport) => {
                log.record("official_api_download_transport_failed")
            }
        }
    }
    Err(failure(
        FailureStage::Download,
        "在线更新的主通道和官方备用通道均未能完成下载。请检查网络或代理后重试。",
        Some(log),
    ))
}

#[cfg(test)]
pub(crate) async fn verify_download_recovery_for_test(
    update: &Update,
    candidates: Vec<reqwest::Url>,
) -> Result<Vec<u8>, UpdateFailure> {
    let temporary = tempfile::tempdir().expect("temporary updater test log");
    let log = UpdateLog::create(temporary.path(), &update.version).expect("updater test log");
    let channel = Channel::new(|_| Ok(()));
    let bytes = download_with_recovery(update, candidates, &channel, &log).await?;
    // Exercise the Windows staging handoff with verified fixture bytes only.
    // Dropping this private temporary file never executes an installer.
    let staged = windows::StagedInstaller::create(&bytes)
        .expect("verified synthetic Windows packet stages successfully");
    drop(staged);
    Ok(bytes)
}

/// No installer is launched until all preparation succeeds. If launch fails,
/// restore routing while the original app and single-instance lock still live.
#[cfg(any(test, target_os = "windows"))]
fn prepared_install<T>(
    prepare: impl FnOnce() -> Result<(), ()>,
    launch: impl FnOnce() -> Result<T, ()>,
    resume: impl FnOnce() -> Result<(), ()>,
    log: &UpdateLog,
) -> Result<T, UpdateFailure> {
    if prepare().is_err() {
        log.record("prepare_failed");
        let resumed = resume().is_ok();
        log.record(if resumed {
            "routing_resumed"
        } else {
            "routing_resume_failed"
        });
        return Err(failure(
            FailureStage::Prepare,
            if resumed {
                "更新前未能恢复路由配置，安装尚未启动。请检查配置文件后重试。"
            } else {
                "更新前的路由清理未完成，安装尚未启动。请检查路由设置后重试。"
            },
            Some(log),
        ));
    }
    match launch() {
        Ok(result) => Ok(result),
        Err(()) => {
            log.record("installer_launch_failed");
            let resumed = resume().is_ok();
            log.record(if resumed {
                "routing_resumed"
            } else {
                "routing_resume_failed"
            });
            Err(failure(
                FailureStage::Install,
                if resumed {
                    "无法启动更新安装器，原有路由已恢复。请重试或打开下载页安装。"
                } else {
                    "无法启动更新安装器。请检查路由设置；也可打开下载页安装。"
                },
                Some(log),
            ))
        }
    }
}

#[tauri::command]
pub(crate) async fn install_app_update(
    webview: Webview,
    update_rid: ResourceId,
    on_event: Channel<AppUpdateEvent>,
    timeout: Option<u64>,
    headers: Option<Vec<(String, String)>>,
) -> Result<InstallResult, UpdateFailure> {
    let _lease = UpdateLease::acquire(&UPDATING)?;
    let (mut update, download_candidates) = if let Ok(owned) =
        webview
            .resources_table()
            .get::<crate::app_update_channel::OwnedOnlineUpdate>(update_rid)
    {
        (owned.update.clone(), owned.download_candidates.clone())
    } else {
        let update = webview
            .resources_table()
            .get::<Update>(update_rid)
            .map_err(|_| {
                failure(
                    FailureStage::Prepare,
                    "更新信息已失效，请重新检查更新。",
                    None,
                )
            })?;
        ((*update).clone(), Vec::new())
    };
    // The metadata-check timeout must not become the package-download limit.
    update.timeout = Some(Duration::from_millis(timeout_ms(timeout)));
    if let Some(headers) = headers {
        update.headers.clear();
        for (name, value) in headers {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| failure(FailureStage::Prepare, "更新请求参数无效。", None))?;
            let value = reqwest::header::HeaderValue::from_str(&value)
                .map_err(|_| failure(FailureStage::Prepare, "更新请求参数无效。", None))?;
            update.headers.append(name, value);
        }
    }
    let dir = crate::paths::app_home()
        .map_err(|_| failure(FailureStage::Prepare, "无法读取更新日志目录。", None))?
        .join("update-logs");
    let log = UpdateLog::create(&dir, &update.version).map_err(|_| {
        failure(
            FailureStage::Prepare,
            "无法创建更新日志，请检查文件夹权限后重试。",
            None,
        )
    })?;
    log.record("download_started");
    let bytes = download_with_recovery(&update, download_candidates, &on_event, &log).await?;
    log.record("signature_verified");
    let exit_guard = InstallationExitGuard::begin(&INSTALLATION_PHASE);

    #[cfg(target_os = "windows")]
    {
        let app = webview.app_handle().clone();
        let result = install_windows(app, bytes, on_event, log, exit_guard).await;
        if result.is_ok() {
            _lease.handoff();
        }
        result
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = on_event.send(AppUpdateEvent::Preparing);
        tauri::async_runtime::spawn_blocking(move || {
            let _exit_guard = exit_guard;
            install_with_route_preflight(
                || crate::failover::begin_update_handoff().map_err(|_| ()),
                || {
                    let _ = on_event.send(AppUpdateEvent::Installing);
                    log.record("install_started");
                    update.install(bytes).map_err(|_| {
                        log.record("install_failed");
                        if crate::failover::resume_after_failed_update().is_err() {
                            log.record("routing_resume_failed");
                        }
                        failure(
                            FailureStage::Install,
                            "安装更新未完成，请重试或前往下载页安装。",
                            Some(&log),
                        )
                    })
                },
                &log,
            )?;
            // The application bytes are already installed. A recovery warning
            // must not turn this into an install retry or a second installer.
            if crate::failover::resume_after_failed_update().is_err() {
                log.record("routing_resume_failed_after_install_restart_required");
            }
            log.record("install_finished_restart_required");
            Ok(InstallResult {
                restart_required: true,
            })
        })
        .await
        .map_err(|_| {
            let _ = crate::failover::resume_after_failed_update();
            failure(
                FailureStage::Install,
                "安装更新未完成，请重新打开软件后检查更新。",
                None,
            )
        })?
    }
}

// Compile this path in host tests too: staging/Command/IPC use portable Rust,
// so Windows handoff type errors are caught before the Windows build starts.
#[cfg(any(test, target_os = "windows"))]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
async fn install_windows(
    app: tauri::AppHandle,
    bytes: Vec<u8>,
    on_event: Channel<AppUpdateEvent>,
    log: UpdateLog,
    exit_guard: InstallationExitGuard<'static>,
) -> Result<InstallResult, UpdateFailure> {
    tauri::async_runtime::spawn_blocking(move || {
        // Stage verified MSI/EXE bytes before changing live routes. The selected
        // updater platform keeps each existing installation on its own format.
        let installer = windows::StagedInstaller::create(&bytes).map_err(|_| {
            failure(
                FailureStage::Install,
                "无法准备 Windows 更新包，请检查磁盘空间或前往下载页安装。",
                Some(&log),
            )
        })?;
        let _ = on_event.send(AppUpdateEvent::Preparing);
        log.record("preparing_route_shutdown");
        install_with_route_preflight(
            || crate::failover::ensure_shutdown_allowed().map_err(|_| ()),
            || {
                prepared_install(
                    || crate::failover::shutdown_all().map_err(|_| ()),
                    || {
                        let _ = on_event.send(AppUpdateEvent::Installing);
                        log.record("launching_installer");
                        installer.launch(std::process::id()).map_err(|_| ())
                    },
                    || crate::failover::resume_after_failed_update().map_err(|_| ()),
                    &log,
                )
            },
            &log,
        )?;
        log.record("installer_handed_off");
        let _ = on_event.send(AppUpdateEvent::HandedOff);
        // Normal Tauri exit destroys windows/tray/single-instance lock. The
        // NSIS installer waits for this PID; MSI uses standard Restart Manager.
        exit_guard.handed_off();
        app.exit(0);
        Ok(InstallResult {
            restart_required: false,
        })
    })
    .await
    .map_err(|_| {
        failure(
            FailureStage::Install,
            "更新准备过程被中断，请重新打开 Codex-X 后检查更新。",
            None,
        )
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn installation_exit_guard_releases_on_failure_and_allows_windows_handoff() {
        let phase = AtomicU8::new(0);
        {
            let _guard = InstallationExitGuard::begin(&phase);
            assert_eq!(phase.load(Ordering::Acquire), 1);
        }
        assert_eq!(phase.load(Ordering::Acquire), 0);
        InstallationExitGuard::begin(&phase).handed_off();
        assert_eq!(phase.load(Ordering::Acquire), 2);
        assert_ne!(phase.load(Ordering::Acquire), 1);
    }

    #[test]
    fn invalid_recovery_preflight_blocks_install_before_application_bytes_change() {
        let temp = tempfile::tempdir().unwrap();
        let log = UpdateLog::create(temp.path(), "0.4.0").unwrap();
        let app = temp.path().join("fixture-app");
        fs::write(&app, b"original fixture").unwrap();
        let error = install_with_route_preflight(
            || Err(()),
            || {
                fs::write(&app, b"replacement fixture").unwrap();
                Ok(())
            },
            &log,
        )
        .unwrap_err();
        assert_eq!(error.stage, FailureStage::Prepare);
        assert!(error.message.contains("可恢复"));
        assert!(error.message.contains("安装尚未开始"));
        assert_eq!(fs::read(&app).unwrap(), b"original fixture");
        let stages = fs::read_to_string(&log.path).unwrap();
        assert!(stages.contains("routing_recovery_preflight_failed"));
        assert!(!stages.contains("install_started"));
    }

    #[test]
    fn native_route_preflight_installs_once_and_preserves_installer_errors() {
        let temp = tempfile::tempdir().unwrap();
        let log = UpdateLog::create(temp.path(), "0.4.0").unwrap();
        let calls = RefCell::new(vec![]);
        let error = install_with_route_preflight(
            || {
                calls.borrow_mut().push("check");
                Ok(())
            },
            || {
                calls.borrow_mut().push("install");
                Err::<(), _>(failure(
                    FailureStage::Install,
                    "fixture install error",
                    Some(&log),
                ))
            },
            &log,
        )
        .unwrap_err();
        assert_eq!(*calls.borrow(), vec!["check", "install"]);
        assert_eq!(error.stage, FailureStage::Install);
        assert_eq!(error.message, "fixture install error");
    }

    #[test]
    fn one_update_at_a_time_and_errors_release_the_guard() {
        let flag = AtomicBool::new(false);
        let first = UpdateLease::acquire(&flag).unwrap();
        assert!(UpdateLease::acquire(&flag).is_err());
        drop(first);
        assert!(UpdateLease::acquire(&flag).is_ok());
    }

    #[test]
    fn package_download_has_its_own_bounded_timeout() {
        assert_eq!(timeout_ms(None), 600_000);
        assert_eq!(timeout_ms(Some(15_000)), 30_000);
        assert_eq!(timeout_ms(Some(u64::MAX)), 600_000);
        assert_eq!(timeout_ms(Some(120_000)), 120_000);
    }

    #[test]
    fn handoff_never_launches_on_failed_prepare_and_resumes_on_launch_failure() {
        let temp = tempfile::tempdir().unwrap();
        let log = UpdateLog::create(temp.path(), "0.4.0").unwrap();
        let calls = RefCell::new(vec![]);
        let fail = prepared_install(
            || {
                calls.borrow_mut().push("prepare");
                Err(())
            },
            || {
                calls.borrow_mut().push("launch");
                Ok(())
            },
            || {
                calls.borrow_mut().push("resume");
                Ok(())
            },
            &log,
        )
        .unwrap_err();
        assert_eq!(fail.stage, FailureStage::Prepare);
        assert_eq!(*calls.borrow(), vec!["prepare", "resume"]);
        calls.borrow_mut().clear();
        let fail = prepared_install(
            || {
                calls.borrow_mut().push("prepare");
                Ok(())
            },
            || {
                calls.borrow_mut().push("launch");
                Err::<(), _>(())
            },
            || {
                calls.borrow_mut().push("resume");
                Err(())
            },
            &log,
        )
        .unwrap_err();
        assert_eq!(fail.stage, FailureStage::Install);
        assert_eq!(*calls.borrow(), vec!["prepare", "launch", "resume"]);
        assert!(fail.message.contains("检查路由设置"));
    }

    #[test]
    fn successful_handoff_does_not_resume_routing() {
        let temp = tempfile::tempdir().unwrap();
        let log = UpdateLog::create(temp.path(), "0.4.0").unwrap();
        assert_eq!(
            prepared_install(|| Ok(()), || Ok(42), || panic!("must not resume"), &log).unwrap(),
            42
        );
    }

    #[test]
    fn logs_are_unique_and_reject_untrusted_version_text() {
        let temp = tempfile::tempdir().unwrap();
        let first = UpdateLog::create(temp.path(), "0.4.0").unwrap();
        let second = UpdateLog::create(temp.path(), "0.4.0").unwrap();
        assert_ne!(first.path, second.path);
        assert!(UpdateLog::create(temp.path(), "0.4.0\nsecret").is_err());
        first.record("signature_verified");
        assert!(fs::read_to_string(&first.path)
            .unwrap()
            .contains("signature_verified"));
    }
}
