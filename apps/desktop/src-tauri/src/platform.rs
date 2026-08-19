use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{mpsc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use std::env;

#[cfg(any(target_os = "windows", test))]
const WINDOWS_CODEX_PACKAGE_IDENTITIES: &[&str] =
    &["OpenAI.Codex", "OpenAI.CodexBeta", "OpenAI.ChatGPT-Desktop"];
#[cfg(any(target_os = "windows", test))]
const WINDOWS_CODEX_EXECUTABLES: &[&str] = &["ChatGPT.exe", "Codex.exe", "codex.exe"];

#[cfg(all(target_os = "windows", not(test)))]
const CODEX_RESTART_TIMEOUT: Duration = Duration::from_secs(30);
const CODEX_VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const PROGRAM_TIMEOUT: Duration = Duration::from_secs(2);
const PROGRAM_POLL_INTERVAL: Duration = Duration::from_millis(10);
const CHILD_TERMINATION_GRACE: Duration = Duration::from_millis(250);

static CODEX_VERSION: OnceLock<String> = OnceLock::new();
static CODEX_RESTART_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexRestartResult {
    pub success: bool,
    pub was_running: bool,
    pub restarted: bool,
    pub platform: String,
    pub message: String,
}

fn version_line(stdout: &str, stderr: &str, success: bool) -> Option<String> {
    let lines = stdout.lines().chain(stderr.lines()).map(str::trim);
    let preferred = lines.clone().find(|line| {
        let lower = line.to_ascii_lowercase();
        !line.is_empty()
            && !lower.starts_with("warning:")
            && (lower.contains("codex-cli")
                || lower.contains("@openai/codex")
                || lower.starts_with("codex "))
            && line.chars().any(|ch| ch.is_ascii_digit())
    });
    if preferred.is_some() {
        return preferred.map(ToString::to_string);
    }
    if !success {
        return None;
    }
    lines
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            !line.is_empty()
                && !lower.starts_with("warning:")
                && !lower.starts_with("error:")
                && line.chars().any(|ch| ch.is_ascii_digit())
        })
        .map(ToString::to_string)
        .next()
}

fn version_from_output(output: Output) -> Option<String> {
    version_line(
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
        output.status.success(),
    )
}

#[cfg(target_os = "windows")]
pub fn program_command(program: &Path, args: &[&str]) -> Command {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let is_script = program
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
    let mut command = if is_script {
        let mut shell = Command::new("cmd.exe");
        let command_line = format!("\"\"{}\" {}\"", program.display(), args.join(" "));
        shell.args(["/D", "/S", "/C"]).arg(command_line);
        shell
    } else {
        let mut direct = Command::new(program);
        direct.args(args);
        direct
    };
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(not(target_os = "windows"))]
pub fn program_command(program: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(program);
    command.args(args);
    command
}

fn remaining_timeout(deadline: Option<Instant>, maximum: Duration) -> Option<Duration> {
    let remaining = deadline
        .map(|deadline| deadline.saturating_duration_since(Instant::now()))
        .unwrap_or(maximum)
        .min(maximum);
    (!remaining.is_zero()).then_some(remaining)
}

fn deadline_expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

fn wait_for_child_exit(child: &mut Child, deadline: Instant) {
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) if Instant::now() < deadline => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                thread::sleep(remaining.min(PROGRAM_POLL_INTERVAL));
            }
            Ok(None) => return,
        }
    }
}

fn terminate_child(child: &mut Child) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let pid = child.id().to_string();
        let mut taskkill = Command::new("taskkill.exe");
        taskkill
            .args(["/PID", pid.as_str(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        if let Ok(mut killer) = taskkill.spawn() {
            let deadline = Instant::now() + CHILD_TERMINATION_GRACE;
            wait_for_child_exit(&mut killer, deadline);
            let _ = killer.kill();
        }
    }

    let _ = child.kill();
    wait_for_child_exit(child, Instant::now() + CHILD_TERMINATION_GRACE);
}

fn output_reader<R>(mut stream: R) -> Option<mpsc::Receiver<Option<Vec<u8>>>>
where
    R: Read + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("codex-version-output".to_string())
        .spawn(move || {
            let mut output = Vec::new();
            let result = stream.read_to_end(&mut output).ok().map(|_| output);
            let _ = sender.send(result);
        })
        .ok()?;
    Some(receiver)
}

fn receive_output(
    receiver: &mpsc::Receiver<Option<Vec<u8>>>,
    deadline: Instant,
) -> Option<Vec<u8>> {
    match receiver.try_recv() {
        Ok(output) => output,
        Err(mpsc::TryRecvError::Disconnected) => None,
        Err(mpsc::TryRecvError::Empty) => {
            let remaining = deadline.saturating_duration_since(Instant::now());
            (!remaining.is_zero())
                .then(|| receiver.recv_timeout(remaining).ok().flatten())
                .flatten()
        }
    }
}

fn run_program_with_timeout(
    program: &Path,
    args: &[&str],
    deadline: Option<Instant>,
    maximum: Duration,
) -> Option<Output> {
    let timeout = remaining_timeout(deadline, maximum)?;
    let command_deadline = Instant::now().checked_add(timeout)?;
    let mut command = program_command(program, args);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().ok()?;

    let Some(stdout) = child.stdout.take() else {
        terminate_child(&mut child);
        return None;
    };
    let Some(stderr) = child.stderr.take() else {
        terminate_child(&mut child);
        return None;
    };
    // Drain both pipes while polling so verbose commands cannot block on a full pipe buffer.
    let Some(stdout_receiver) = output_reader(stdout) else {
        terminate_child(&mut child);
        return None;
    };
    let Some(stderr_receiver) = output_reader(stderr) else {
        terminate_child(&mut child);
        return None;
    };

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < command_deadline => {
                let remaining = command_deadline.saturating_duration_since(Instant::now());
                thread::sleep(remaining.min(PROGRAM_POLL_INTERVAL));
            }
            Ok(None) | Err(_) => {
                terminate_child(&mut child);
                return None;
            }
        }
    };
    let stdout = receive_output(&stdout_receiver, command_deadline)?;
    let stderr = receive_output(&stderr_receiver, command_deadline)?;
    Some(Output {
        status,
        stdout,
        stderr,
    })
}

fn run_program(program: &Path, args: &[&str], deadline: Option<Instant>) -> Option<Output> {
    run_program_with_timeout(program, args, deadline, PROGRAM_TIMEOUT)
}

fn command_version(program: &Path, deadline: Option<Instant>) -> Option<String> {
    run_program(program, &["--version"], deadline)
        .and_then(version_from_output)
        .or_else(|| run_program(program, &["-V"], deadline).and_then(version_from_output))
}

fn candidate_key(path: &Path) -> String {
    let value = path.to_string_lossy().to_string();
    if cfg!(target_os = "windows") {
        value.to_ascii_lowercase()
    } else {
        value
    }
}

fn push_candidate(candidates: &mut Vec<PathBuf>, seen: &mut HashSet<String>, path: PathBuf) {
    if seen.insert(candidate_key(&path)) {
        candidates.push(path);
    }
}

#[cfg(any(target_os = "windows", test))]
fn numeric_version(value: &str) -> Option<Vec<u32>> {
    let parts = value
        .split('.')
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .ok()?;
    (parts.len() >= 2).then_some(parts)
}

#[cfg(any(target_os = "windows", test))]
fn windows_package_version(package_name: &str) -> Option<(Vec<u32>, String)> {
    for identity in WINDOWS_CODEX_PACKAGE_IDENTITIES {
        let prefix_len = identity.len();
        if !package_name
            .get(..prefix_len)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(identity))
            || package_name.as_bytes().get(prefix_len) != Some(&b'_')
        {
            continue;
        }
        let version = package_name.get(prefix_len + 1..)?.split('_').next()?;
        return Some((numeric_version(version)?, version.to_string()));
    }
    None
}

#[cfg(any(target_os = "windows", test))]
fn latest_windows_package_version<'a>(
    package_names: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    package_names
        .into_iter()
        .filter_map(windows_package_version)
        .max_by(|left, right| left.0.cmp(&right.0))
        .map(|(_, version)| version)
}

#[cfg(target_os = "windows")]
fn windows_store_app_version_from_roots(
    roots: &[PathBuf],
    deadline: Option<Instant>,
) -> Option<String> {
    let mut package_names = Vec::new();
    for root in roots {
        if deadline_expired(deadline) {
            break;
        }
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            if deadline_expired(deadline) {
                break;
            }
            if entry.path().is_dir() {
                package_names.push(entry.file_name().to_string_lossy().to_string());
            }
        }
    }
    latest_windows_package_version(package_names.iter().map(String::as_str))
        .map(|version| format!("Codex app {version}"))
}

fn visit_named_files<F>(
    root: &Path,
    names: &[&str],
    depth: usize,
    deadline: Option<Instant>,
    visit: &mut F,
) -> bool
where
    F: FnMut(PathBuf) -> bool,
{
    if depth == 0 || deadline_expired(deadline) {
        return !deadline_expired(deadline);
    }
    if !root.is_dir() {
        return !deadline_expired(deadline);
    }
    let Ok(entries) = fs::read_dir(root) else {
        return !deadline_expired(deadline);
    };
    for entry in entries.flatten() {
        if deadline_expired(deadline) {
            return false;
        }
        let path = entry.path();
        if path.is_dir() {
            if !visit_named_files(&path, names, depth - 1, deadline, visit) {
                return false;
            }
        } else if path.is_file()
            && path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| {
                    names
                        .iter()
                        .any(|candidate| name.eq_ignore_ascii_case(candidate))
                })
            && !visit(path)
        {
            return false;
        }
    }
    !deadline_expired(deadline)
}

fn visit_extension_codex_candidates<F>(
    home: &Path,
    deadline: Option<Instant>,
    visit: &mut F,
) -> bool
where
    F: FnMut(PathBuf) -> bool,
{
    let roots = [
        home.join(".cursor").join("extensions"),
        home.join(".vscode").join("extensions"),
        home.join(".vscode-insiders").join("extensions"),
        home.join(".windsurf").join("extensions"),
    ];
    for root in roots {
        if deadline_expired(deadline) {
            return false;
        }
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        let mut extension_dirs = Vec::new();
        for entry in entries.flatten() {
            if deadline_expired(deadline) {
                break;
            }
            let path = entry.path();
            if path.is_dir()
                && path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(|name| {
                        let lower = name.to_ascii_lowercase();
                        lower.starts_with("openai.chatgpt-") || lower.starts_with("openai.codex-")
                    })
            {
                extension_dirs.push(path);
            }
        }
        if deadline_expired(deadline) {
            return false;
        }
        extension_dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        for extension_dir in extension_dirs {
            if deadline_expired(deadline) {
                return false;
            }
            if !visit_named_files(
                &extension_dir,
                &["codex", "codex.exe", "codex.cmd"],
                5,
                deadline,
                visit,
            ) {
                return false;
            }
        }
    }
    true
}

#[cfg(target_os = "macos")]
fn visit_platform_candidates<F>(home: &Path, deadline: Option<Instant>, visit: &mut F) -> bool
where
    F: FnMut(PathBuf) -> bool,
{
    let candidates = [
        PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/codex"),
        home.join("Applications/ChatGPT.app/Contents/Resources/codex"),
        PathBuf::from("/Applications/Codex.app/Contents/Resources/codex"),
        home.join("Applications/Codex.app/Contents/Resources/codex"),
        PathBuf::from("/Applications/OpenAI Codex.app/Contents/Resources/codex"),
        home.join("Applications/OpenAI Codex.app/Contents/Resources/codex"),
        PathBuf::from("/Applications/OpenAI.Codex.app/Contents/Resources/codex"),
        home.join("Applications/OpenAI.Codex.app/Contents/Resources/codex"),
        PathBuf::from("/Applications/ChatGPT Codex.app/Contents/Resources/codex"),
        home.join("Applications/ChatGPT Codex.app/Contents/Resources/codex"),
        PathBuf::from("/opt/homebrew/bin/codex"),
        PathBuf::from("/usr/local/bin/codex"),
        home.join(".local/bin/codex"),
        home.join(".npm-global/bin/codex"),
        home.join("Library/pnpm/codex"),
    ];
    for candidate in candidates {
        if deadline_expired(deadline) || !visit(candidate) {
            return false;
        }
    }
    visit_extension_codex_candidates(home, deadline, visit)
}

#[cfg(target_os = "windows")]
fn visit_platform_candidates<F>(home: &Path, deadline: Option<Instant>, visit: &mut F) -> bool
where
    F: FnMut(PathBuf) -> bool,
{
    if let Ok(appdata) = env::var("APPDATA") {
        let appdata = PathBuf::from(appdata);
        for candidate in [
            appdata.join("npm").join("codex.cmd"),
            appdata.join("npm").join("codex.exe"),
        ] {
            if deadline_expired(deadline) || !visit(candidate) {
                return false;
            }
        }
        for target in ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"] {
            let candidate = appdata
                .join("npm/node_modules/@openai/codex/vendor")
                .join(target)
                .join("codex/codex.exe");
            if deadline_expired(deadline) || !visit(candidate) {
                return false;
            }
        }
    }
    if let Ok(localappdata) = env::var("LOCALAPPDATA") {
        let localappdata = PathBuf::from(localappdata);
        for candidate in [
            localappdata.join("Microsoft/WindowsApps/codex.exe"),
            localappdata.join("Microsoft/WindowsApps/codex.cmd"),
        ] {
            if deadline_expired(deadline) || !visit(candidate) {
                return false;
            }
        }
        for root in [
            localappdata.join("Programs/ChatGPT"),
            localappdata.join("Programs/Codex"),
            localappdata.join("Programs/OpenAI/Codex"),
            localappdata.join("OpenAI/ChatGPT"),
            localappdata.join("OpenAI/Codex"),
        ] {
            if !visit_named_files(&root, &["codex.exe", "codex.cmd"], 7, deadline, visit) {
                return false;
            }
        }
    }
    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(program_files) = env::var(variable) {
            for app in ["ChatGPT", "Codex"] {
                if !visit_named_files(
                    &PathBuf::from(&program_files).join(app),
                    &["codex.exe", "codex.cmd"],
                    7,
                    deadline,
                    visit,
                ) {
                    return false;
                }
            }
        }
    }
    visit_extension_codex_candidates(home, deadline, visit)
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn visit_platform_candidates<F>(home: &Path, deadline: Option<Instant>, visit: &mut F) -> bool
where
    F: FnMut(PathBuf) -> bool,
{
    let candidates = [
        PathBuf::from("/usr/local/bin/codex"),
        PathBuf::from("/usr/bin/codex"),
        PathBuf::from("/snap/bin/codex"),
        home.join(".local/bin/codex"),
        home.join(".npm-global/bin/codex"),
        home.join(".local/share/pnpm/codex"),
    ];
    for candidate in candidates {
        if deadline_expired(deadline) || !visit(candidate) {
            return false;
        }
    }
    visit_extension_codex_candidates(home, deadline, visit)
}

#[cfg(target_os = "windows")]
fn windows_where_candidates(deadline: Option<Instant>) -> Vec<PathBuf> {
    let Some(output) = run_program(Path::new("where.exe"), &["codex"], deadline) else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

#[cfg(not(target_os = "windows"))]
fn windows_where_candidates(_deadline: Option<Instant>) -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(target_os = "macos")]
fn macos_app_version(deadline: Option<Instant>) -> Option<String> {
    let home = dirs::home_dir().unwrap_or_default();
    for root in [PathBuf::from("/Applications"), home.join("Applications")] {
        for name in [
            "Codex.app",
            "OpenAI Codex.app",
            "OpenAI.Codex.app",
            "ChatGPT Codex.app",
            "ChatGPT.app",
        ] {
            if deadline_expired(deadline) {
                return None;
            }
            let app = root.join(name);
            if !app.is_dir() {
                continue;
            }
            let app_name = if name == "ChatGPT.app" {
                "ChatGPT app"
            } else {
                "Codex app"
            };
            if let Some(version) = macos_info_plist_version(&app).or_else(|| {
                let app = app.to_str()?;
                let output = run_program(
                    Path::new("mdls"),
                    &["-name", "kMDItemVersion", "-raw", app],
                    deadline,
                )?;
                output
                    .status
                    .success()
                    .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
            }) {
                if !version.is_empty() && version != "(null)" {
                    return Some(format!("{app_name} {version}"));
                }
            }
            return Some(format!("{app_name} installed"));
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn macos_info_plist_version(app: &Path) -> Option<String> {
    let plist = fs::read_to_string(app.join("Contents/Info.plist")).ok()?;
    plist_string_value(&plist, "CFBundleShortVersionString")
        .or_else(|| plist_string_value(&plist, "CFBundleVersion"))
}

#[cfg(any(target_os = "macos", test))]
fn plist_string_value(plist: &str, key: &str) -> Option<String> {
    let (_, after_key) = plist.split_once(&format!("<key>{key}</key>"))?;
    let (_, after_open) = after_key.split_once("<string>")?;
    let (value, _) = after_open.split_once("</string>")?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(not(target_os = "macos"))]
fn macos_app_version(_deadline: Option<Instant>) -> Option<String> {
    None
}

#[cfg(target_os = "windows")]
fn windows_app_version(deadline: Option<Instant>) -> Option<String> {
    let mut roots = Vec::new();
    for variable in ["ProgramFiles", "ProgramW6432"] {
        if let Ok(program_files) = env::var(variable) {
            roots.push(PathBuf::from(program_files).join("WindowsApps"));
        }
    }
    roots.push(PathBuf::from(r"C:\Program Files\WindowsApps"));
    roots.sort();
    roots.dedup();
    if let Some(version) = windows_store_app_version_from_roots(&roots, deadline) {
        return Some(version);
    }

    if deadline_expired(deadline) {
        return None;
    }

    let script = "Get-AppxPackage | Where-Object { $_.Name -in @('OpenAI.Codex','OpenAI.CodexBeta','OpenAI.ChatGPT-Desktop') } | ForEach-Object { $_.Version.ToString() }";
    if let Some(output) = run_program(
        Path::new("powershell.exe"),
        &["-NoProfile", "-NonInteractive", "-Command", script],
        deadline,
    ) {
        if output.status.success() {
            let versions = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .filter_map(|version| {
                    numeric_version(version).map(|parsed| (parsed, version.to_string()))
                })
                .max_by(|left, right| left.0.cmp(&right.0));
            if let Some((_, version)) = versions {
                return Some(format!("Codex app {version}"));
            }
        }
    }

    let local_appdata = env::var("LOCALAPPDATA").ok().map(PathBuf::from)?;
    for directory in [
        local_appdata.join("OpenAI/Codex/bin"),
        local_appdata.join("OpenAI/Codex"),
        local_appdata.join("Programs/OpenAI/Codex"),
        local_appdata.join("Programs/Codex"),
    ] {
        if deadline_expired(deadline) {
            return None;
        }
        if WINDOWS_CODEX_EXECUTABLES.iter().any(|name| {
            directory.join(name).is_file() || directory.join("app").join(name).is_file()
        }) {
            return Some("Codex app installed".to_string());
        }
    }
    None
}

#[cfg(not(target_os = "windows"))]
fn windows_app_version(_deadline: Option<Instant>) -> Option<String> {
    None
}

#[cfg(any(target_os = "windows", test))]
fn is_allowed_windows_package_identity(identity: &str) -> bool {
    WINDOWS_CODEX_PACKAGE_IDENTITIES
        .iter()
        .any(|allowed| identity.eq_ignore_ascii_case(allowed))
}

#[cfg(any(target_os = "windows", test))]
#[allow(dead_code)]
fn windows_path_is_within(path: &str, root: &str) -> bool {
    let path = path.trim().replace('/', "\\").to_ascii_lowercase();
    let root = root
        .trim()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase();
    !root.is_empty()
        && path
            .strip_prefix(&root)
            .is_some_and(|suffix| suffix.starts_with('\\'))
}

#[cfg(any(target_os = "windows", test))]
#[allow(dead_code)]
fn is_safe_windows_desktop_process(
    executable_name: &str,
    executable_path: &str,
    package_locations: &[&str],
) -> bool {
    let path_name = executable_path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or_default();
    WINDOWS_CODEX_EXECUTABLES.iter().any(|allowed| {
        executable_name.eq_ignore_ascii_case(allowed) && path_name.eq_ignore_ascii_case(allowed)
    }) && package_locations
        .iter()
        .any(|location| windows_path_is_within(executable_path, location))
}

#[allow(dead_code)]
fn unsupported_restart_result(platform: &str) -> CodexRestartResult {
    CodexRestartResult {
        success: false,
        was_running: false,
        restarted: false,
        platform: platform.to_string(),
        message: "当前平台暂不支持 Codex Desktop 重启".to_string(),
    }
}

#[derive(Debug)]
pub(crate) struct CodexDesktopStopState {
    was_running: bool,
    #[cfg_attr(test, allow(dead_code))]
    package_identities: Vec<String>,
    _lifecycle_guard: MutexGuard<'static, ()>,
}

impl CodexDesktopStopState {
    pub(crate) fn was_running(&self) -> bool {
        self.was_running
    }
}

#[cfg(all(target_os = "windows", not(test)))]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WindowsStopOutput {
    success: bool,
    was_running: bool,
    package_identities: Vec<String>,
    code: String,
}

#[cfg(all(target_os = "windows", not(test)))]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WindowsStartOutput {
    success: bool,
    code: String,
}

fn acquire_desktop_lifecycle_lock() -> std::result::Result<MutexGuard<'static, ()>, String> {
    let lock = CODEX_RESTART_LOCK.get_or_init(|| Mutex::new(()));
    #[cfg(test)]
    return Ok(lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
    #[cfg(not(test))]
    lock.try_lock()
        .map_err(|_| "Codex Desktop 生命周期操作正在进行，请稍后重试".to_string())
}

#[cfg(any(target_os = "windows", test))]
fn windows_stop_script() -> String {
    let packages = WINDOWS_CODEX_PACKAGE_IDENTITIES.join("','");
    let executables = WINDOWS_CODEX_EXECUTABLES.join("','");
    WINDOWS_STOP_SCRIPT_TEMPLATE
        .replace("__PACKAGES__", &packages)
        .replace("__EXECUTABLES__", &executables)
}

#[cfg(any(target_os = "windows", test))]
const WINDOWS_STOP_SCRIPT_TEMPLATE: &str = r#"
$ErrorActionPreference = 'Stop'
$allowedPackages = @('__PACKAGES__')
$allowedExecutables = @('__EXECUTABLES__')
$wasRunning = $false

function Write-Result([bool]$success, [bool]$running, [string[]]$identities, [string]$code) {
  [pscustomobject]@{
    success = $success
    wasRunning = $running
    packageIdentities = @($identities)
    code = $code
  } | ConvertTo-Json -Compress
}

function Get-SafeDesktopProcesses($packages) {
  $safe = @()
  $processes = @(Get-CimInstance Win32_Process | Where-Object { $allowedExecutables -contains $_.Name })
  foreach ($process in $processes) {
    $path = [string]$process.ExecutablePath
    if ([string]::IsNullOrWhiteSpace($path)) { throw 'process_unverified' }
    foreach ($package in $packages) {
      $root = ([string]$package.InstallLocation).TrimEnd('\')
      if ([string]::IsNullOrWhiteSpace($root)) { continue }
      if ($path.StartsWith($root + '\', [System.StringComparison]::OrdinalIgnoreCase)) {
        $safe += [pscustomobject]@{ Process = $process; Package = $package }
        break
      }
    }
  }
  return @($safe)
}

try {
  $packages = @(Get-AppxPackage | Where-Object { $allowedPackages -contains $_.Name })
  $targets = @(Get-SafeDesktopProcesses $packages)
  $wasRunning = $targets.Count -gt 0
  if (-not $wasRunning) {
    Write-Result $true $false @() 'ok'
    exit 0
  }
  $runningPackages = @($targets | ForEach-Object { [string]$_.Package.Name } | Sort-Object -Unique)
  $targetPids = @($targets | ForEach-Object { [int]$_.Process.ProcessId })

  foreach ($target in $targets) {
    $pidValue = [int]$target.Process.ProcessId
    $current = @(Get-SafeDesktopProcesses $packages | Where-Object { [int]$_.Process.ProcessId -eq $pidValue })
    if ($current.Count -eq 0) { continue }
    $process = Get-Process -Id $pidValue -ErrorAction SilentlyContinue
    if ($null -ne $process) { [void]$process.CloseMainWindow() }
  }

  $graceDeadline = [DateTime]::UtcNow.AddSeconds(4)
  do {
    $remaining = @(Get-SafeDesktopProcesses $packages | Where-Object {
      $targetPids -contains [int]$_.Process.ProcessId
    })
    if ($remaining.Count -eq 0) { break }
    Start-Sleep -Milliseconds 150
  } while ([DateTime]::UtcNow -lt $graceDeadline)

  foreach ($target in $remaining) {
    $pidValue = [int]$target.Process.ProcessId
    $verified = @(Get-SafeDesktopProcesses $packages | Where-Object { [int]$_.Process.ProcessId -eq $pidValue })
    if ($verified.Count -gt 0) {
      Stop-Process -Id $pidValue -Force -ErrorAction SilentlyContinue
    }
  }

  $forceDeadline = [DateTime]::UtcNow.AddSeconds(3)
  do {
    $remaining = @(Get-SafeDesktopProcesses $packages | Where-Object {
      $targetPids -contains [int]$_.Process.ProcessId
    })
    if ($remaining.Count -eq 0) { break }
    Start-Sleep -Milliseconds 150
  } while ([DateTime]::UtcNow -lt $forceDeadline)
  if ($remaining.Count -gt 0) {
    Write-Result $false $true $runningPackages 'close_timeout'
    exit 0
  }
  Write-Result $true $true $runningPackages 'ok'
} catch {
  $code = if ($_.Exception.Message -eq 'process_unverified') { 'process_unverified' } else { 'operation_failed' }
  Write-Result $false $wasRunning @() $code
}
"#;

#[cfg(any(target_os = "windows", test))]
fn windows_start_script(package_identities: &[String]) -> std::result::Result<String, String> {
    if package_identities
        .iter()
        .any(|identity| !is_allowed_windows_package_identity(identity))
    {
        return Err("拒绝启动未经允许的 Codex Desktop 包身份".to_string());
    }
    let requested = package_identities.join("','");
    let packages = WINDOWS_CODEX_PACKAGE_IDENTITIES.join("','");
    let executables = WINDOWS_CODEX_EXECUTABLES.join("','");
    Ok(WINDOWS_START_SCRIPT_TEMPLATE
        .replace("__REQUESTED__", &requested)
        .replace("__PACKAGES__", &packages)
        .replace("__EXECUTABLES__", &executables))
}

#[cfg(any(target_os = "windows", test))]
const WINDOWS_START_SCRIPT_TEMPLATE: &str = r#"
$ErrorActionPreference = 'Stop'
$allowedPackages = @('__PACKAGES__')
$allowedExecutables = @('__EXECUTABLES__')
$requestedPackages = @('__REQUESTED__') | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }

function Write-Result([bool]$success, [string]$code) {
  [pscustomobject]@{ success = $success; code = $code } | ConvertTo-Json -Compress
}
function Get-SafeDesktopProcesses($packages) {
  $safe = @()
  $processes = @(Get-CimInstance Win32_Process | Where-Object { $allowedExecutables -contains $_.Name })
  foreach ($process in $processes) {
    $path = [string]$process.ExecutablePath
    if ([string]::IsNullOrWhiteSpace($path)) { continue }
    foreach ($package in $packages) {
      $root = ([string]$package.InstallLocation).TrimEnd('\')
      if (-not [string]::IsNullOrWhiteSpace($root) -and $path.StartsWith($root + '\', [System.StringComparison]::OrdinalIgnoreCase)) {
        $safe += [pscustomobject]@{ Process = $process; Package = $package }
        break
      }
    }
  }
  return @($safe)
}

try {
  $installed = @(Get-AppxPackage | Where-Object { $allowedPackages -contains $_.Name })
  if ($requestedPackages.Count -eq 0) {
    foreach ($identity in $allowedPackages) {
      $candidate = $installed | Where-Object { $_.Name -eq $identity } | Select-Object -First 1
      if ($null -ne $candidate) { $requestedPackages = @($identity); break }
    }
  }
  $selected = @()
  foreach ($identity in $requestedPackages) {
    if ($allowedPackages -notcontains $identity) { Write-Result $false 'package_not_allowed'; exit 0 }
    $package = $installed | Where-Object { $_.Name -eq $identity } | Select-Object -First 1
    if ($null -eq $package) { Write-Result $false 'package_not_found'; exit 0 }
    $selected += $package
  }
  if ($selected.Count -eq 0) { Write-Result $false 'package_not_found'; exit 0 }

  foreach ($package in $selected) {
    $manifest = Get-AppxPackageManifest -Package $package
    $applications = @($manifest.Package.Applications.Application)
    $application = $applications | Where-Object {
      $allowedExecutables -contains [System.IO.Path]::GetFileName([string]$_.Executable)
    } | Select-Object -First 1
    if ($null -eq $application) { $application = $applications | Select-Object -First 1 }
    $applicationId = [string]$application.Id
    $familyName = [string]$package.PackageFamilyName
    if ([string]::IsNullOrWhiteSpace($applicationId) -or [string]::IsNullOrWhiteSpace($familyName)) {
      Write-Result $false 'launch_target_missing'
      exit 0
    }
    $aumid = 'shell:AppsFolder\' + $familyName + '!' + $applicationId
    Start-Process -FilePath 'explorer.exe' -ArgumentList $aumid
  }

  $deadline = [DateTime]::UtcNow.AddSeconds(10)
  do {
    Start-Sleep -Milliseconds 200
    $runningNames = @(Get-SafeDesktopProcesses $selected | ForEach-Object { [string]$_.Package.Name } | Sort-Object -Unique)
    $missing = @($selected | Where-Object { $runningNames -notcontains [string]$_.Name })
    if ($missing.Count -eq 0) { Write-Result $true 'ok'; exit 0 }
  } while ([DateTime]::UtcNow -lt $deadline)
  Write-Result $false 'launch_timeout'
} catch {
  Write-Result $false 'operation_failed'
}
"#;

#[cfg(all(target_os = "windows", not(test)))]
fn run_windows_lifecycle_script<T: for<'de> Deserialize<'de>>(script: &str) -> Option<T> {
    let deadline = Instant::now() + CODEX_RESTART_TIMEOUT;
    run_program_with_timeout(
        Path::new("powershell.exe"),
        &["-NoProfile", "-NonInteractive", "-Command", script],
        Some(deadline),
        CODEX_RESTART_TIMEOUT,
    )
    .filter(|output| output.status.success())
    .and_then(|output| serde_json::from_slice(&output.stdout).ok())
}

#[cfg(all(target_os = "windows", not(test)))]
fn stop_codex_desktop_locked() -> std::result::Result<(bool, Vec<String>), String> {
    let parsed = run_windows_lifecycle_script::<WindowsStopOutput>(&windows_stop_script())
        .ok_or_else(|| "无法完成安全的 Desktop 进程检测".to_string())?;
    if !parsed.success {
        return Err(match parsed.code.as_str() {
            "close_timeout" => "Codex Desktop 未能在限定时间内完全退出".to_string(),
            "process_unverified" => "无法可靠验证 Codex Desktop 进程，已取消会话操作".to_string(),
            _ => "Codex Desktop 安全关闭失败".to_string(),
        });
    }
    if parsed
        .package_identities
        .iter()
        .any(|identity| !is_allowed_windows_package_identity(identity))
    {
        return Err("Desktop 检测返回了未经允许的包身份".to_string());
    }
    Ok((parsed.was_running, parsed.package_identities))
}

#[cfg(all(target_os = "windows", not(test)))]
fn start_codex_desktop_locked(package_identities: &[String]) -> std::result::Result<(), String> {
    let script = windows_start_script(package_identities)?;
    let parsed = run_windows_lifecycle_script::<WindowsStartOutput>(&script)
        .ok_or_else(|| "无法确认 Codex Desktop 启动结果".to_string())?;
    if parsed.success {
        Ok(())
    } else {
        Err(match parsed.code.as_str() {
            "package_not_found" => "未找到原先运行的 Codex Desktop 包".to_string(),
            "launch_target_missing" => "未找到可安全启动的 Codex Desktop 应用入口".to_string(),
            "launch_timeout" => "Codex Desktop 启动后未在限定时间内出现".to_string(),
            _ => "Codex Desktop 安全启动失败".to_string(),
        })
    }
}

pub(crate) fn stop_codex_desktop() -> std::result::Result<CodexDesktopStopState, String> {
    let lifecycle_guard = acquire_desktop_lifecycle_lock()?;
    #[cfg(test)]
    let (was_running, package_identities) = (false, Vec::new());
    #[cfg(all(not(test), target_os = "windows"))]
    let (was_running, package_identities) = stop_codex_desktop_locked()?;
    #[cfg(all(not(test), not(target_os = "windows")))]
    let (was_running, package_identities) = (false, Vec::new());
    Ok(CodexDesktopStopState {
        was_running,
        package_identities,
        _lifecycle_guard: lifecycle_guard,
    })
}

pub(crate) fn start_codex_desktop(
    state: CodexDesktopStopState,
    launch_when_stopped: bool,
) -> std::result::Result<(), String> {
    if !state.was_running && !launch_when_stopped {
        return Ok(());
    }
    #[cfg(test)]
    return Ok(());
    #[cfg(all(not(test), target_os = "windows"))]
    return start_codex_desktop_locked(&state.package_identities);
    #[cfg(all(not(test), not(target_os = "windows")))]
    Err("当前平台暂不支持 Codex Desktop 启动".to_string())
}

pub fn restart_codex_desktop() -> CodexRestartResult {
    let platform = std::env::consts::OS;
    #[cfg(target_os = "windows")]
    {
        let state = match stop_codex_desktop() {
            Ok(state) => state,
            Err(message) => {
                return CodexRestartResult {
                    success: false,
                    was_running: false,
                    restarted: false,
                    platform: platform.to_string(),
                    message,
                }
            }
        };
        let was_running = state.was_running();
        return match start_codex_desktop(state, true) {
            Ok(()) => CodexRestartResult {
                success: true,
                was_running,
                restarted: was_running,
                platform: platform.to_string(),
                message: if was_running {
                    "Codex 已重新启动".to_string()
                } else {
                    "Codex 已启动".to_string()
                },
            },
            Err(message) => CodexRestartResult {
                success: false,
                was_running,
                restarted: false,
                platform: platform.to_string(),
                message,
            },
        };
    }
    #[cfg(target_os = "macos")]
    {
        unsupported_restart_result("macos")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        unsupported_restart_result(platform)
    }
}

fn path_codex_candidates(deadline: Option<Instant>) -> Vec<PathBuf> {
    let mut candidates = ["codex", "codex.exe", "codex.cmd"]
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if !deadline_expired(deadline) {
        candidates.extend(windows_where_candidates(deadline));
    }
    candidates
}

fn append_unique_candidates(
    unique: &mut Vec<PathBuf>,
    seen: &mut HashSet<String>,
    candidates: impl IntoIterator<Item = PathBuf>,
) {
    for candidate in candidates {
        push_candidate(unique, seen, candidate);
    }
}

fn codex_executable_candidates_until(deadline: Option<Instant>) -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    append_unique_candidates(&mut unique, &mut seen, path_codex_candidates(deadline));
    if !deadline_expired(deadline) {
        let mut collect = |candidate| {
            if deadline_expired(deadline) {
                return false;
            }
            push_candidate(&mut unique, &mut seen, candidate);
            true
        };
        let _ = visit_platform_candidates(&home, deadline, &mut collect);
    }
    unique
}

pub fn codex_executable_candidates() -> Vec<PathBuf> {
    codex_executable_candidates_until(None)
}

fn version_from_candidates(
    candidates: impl IntoIterator<Item = PathBuf>,
    seen: &mut HashSet<String>,
    deadline: Instant,
) -> Option<String> {
    for candidate in candidates {
        if deadline_expired(Some(deadline)) {
            return None;
        }
        if !seen.insert(candidate_key(&candidate)) {
            continue;
        }
        let is_bare_command = candidate.components().count() == 1;
        if is_bare_command || candidate.is_file() {
            if let Some(version) = command_version(&candidate, Some(deadline)) {
                return Some(version);
            }
        }
    }
    None
}

fn version_from_platform_candidates(
    home: &Path,
    seen: &mut HashSet<String>,
    deadline: Instant,
) -> Option<String> {
    let mut detected = None;
    let mut probe = |candidate| {
        if deadline_expired(Some(deadline)) {
            return false;
        }
        detected = version_from_candidates([candidate], seen, deadline);
        detected.is_none() && !deadline_expired(Some(deadline))
    };
    let _ = visit_platform_candidates(home, Some(deadline), &mut probe);
    detected
}

fn detect_codex_version_uncached() -> Option<String> {
    let deadline = Instant::now().checked_add(CODEX_VERSION_PROBE_TIMEOUT)?;
    let mut seen = HashSet::new();

    // Probe cheap PATH/where.exe results before walking redirected profiles or slow disks.
    if let Some(version) =
        version_from_candidates(path_codex_candidates(Some(deadline)), &mut seen, deadline)
    {
        return Some(version);
    }
    if deadline_expired(Some(deadline)) {
        return None;
    }

    let home = dirs::home_dir().unwrap_or_default();
    if let Some(version) = version_from_platform_candidates(&home, &mut seen, deadline) {
        return Some(version);
    }
    if deadline_expired(Some(deadline)) {
        return None;
    }
    macos_app_version(Some(deadline)).or_else(|| windows_app_version(Some(deadline)))
}

fn cached_codex_version(
    cache: &OnceLock<String>,
    detect: impl FnOnce() -> Option<String>,
) -> Option<String> {
    if let Some(version) = cache.get() {
        return Some(version.clone());
    }
    let detected = detect()?;
    let _ = cache.set(detected.clone());
    cache.get().cloned().or(Some(detected))
}

pub fn detect_codex_version() -> Option<String> {
    cached_codex_version(&CODEX_VERSION, detect_codex_version_uncached)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::run_program;
    use super::{
        cached_codex_version, is_allowed_windows_package_identity, is_safe_windows_desktop_process,
        latest_windows_package_version, plist_string_value, unsupported_restart_result,
        version_line, visit_named_files, windows_start_script, windows_stop_script,
        CodexRestartResult,
    };
    use std::fs;
    #[cfg(unix)]
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::OnceLock;
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    #[test]
    fn version_parser_prefers_codex_line_over_warning() {
        assert_eq!(
            version_line(
                "codex-cli 0.144.0-alpha.4\n",
                "WARNING: could not create PATH aliases\n",
                true,
            )
            .as_deref(),
            Some("codex-cli 0.144.0-alpha.4")
        );
    }

    #[test]
    fn version_parser_accepts_successful_plain_version() {
        assert_eq!(
            version_line("0.42.0\n", "", true).as_deref(),
            Some("0.42.0")
        );
    }

    #[test]
    fn version_parser_rejects_failed_error_output() {
        assert_eq!(
            version_line("", "error: command not found 127\n", false),
            None
        );
    }

    #[test]
    fn codex_version_cache_runs_probe_once() {
        let cache = OnceLock::new();
        let calls = AtomicUsize::new(0);

        let first = cached_codex_version(&cache, || {
            calls.fetch_add(1, Ordering::Relaxed);
            Some("codex-cli 1.2.3".to_string())
        });
        let second = cached_codex_version(&cache, || {
            calls.fetch_add(1, Ordering::Relaxed);
            Some("codex-cli 9.9.9".to_string())
        });

        assert_eq!(first.as_deref(), Some("codex-cli 1.2.3"));
        assert_eq!(second, first);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn codex_version_cache_retries_after_a_missing_result() {
        let cache = OnceLock::new();
        let calls = AtomicUsize::new(0);

        assert_eq!(
            cached_codex_version(&cache, || {
                calls.fetch_add(1, Ordering::Relaxed);
                None
            }),
            None
        );
        assert_eq!(
            cached_codex_version(&cache, || {
                calls.fetch_add(1, Ordering::Relaxed);
                Some("codex-cli 1.2.3".to_string())
            }),
            Some("codex-cli 1.2.3".to_string())
        );
        assert_eq!(
            cached_codex_version(&cache, || {
                calls.fetch_add(1, Ordering::Relaxed);
                Some("codex-cli 9.9.9".to_string())
            }),
            Some("codex-cli 1.2.3".to_string())
        );
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn recursive_candidate_scan_stops_immediately_after_visitor_finishes() {
        static TEMP_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

        let root = std::env::temp_dir().join(format!(
            "codex-x-platform-test-{}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("first")).expect("create first candidate directory");
        fs::create_dir_all(root.join("second")).expect("create second candidate directory");
        fs::write(root.join("first/codex.exe"), b"").expect("create first candidate");
        fs::write(root.join("second/codex.exe"), b"").expect("create second candidate");

        let mut visits = 0;
        let completed = visit_named_files(&root, &["codex.exe"], 3, None, &mut |_| {
            visits += 1;
            false
        });
        fs::remove_dir_all(&root).expect("remove candidate test directory");

        assert!(!completed);
        assert_eq!(visits, 1);
    }

    #[cfg(unix)]
    #[test]
    fn program_runner_stops_hung_process_at_deadline() {
        let started = Instant::now();
        let deadline = started + Duration::from_millis(100);

        assert!(run_program(
            Path::new("/bin/sh"),
            &["-c", "while :; do :; done"],
            Some(deadline),
        )
        .is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn windows_package_identity_allowlist_is_exact() {
        for identity in super::WINDOWS_CODEX_PACKAGE_IDENTITIES {
            assert!(is_allowed_windows_package_identity(identity));
        }
        assert!(!is_allowed_windows_package_identity(
            "OpenAI.Codex.Untrusted"
        ));
        assert!(!is_allowed_windows_package_identity("Other.Codex"));
    }

    #[test]
    fn arbitrary_windows_package_cannot_be_a_restart_target() {
        assert!(!is_allowed_windows_package_identity(
            "Contoso.ChatGPT-Desktop"
        ));
        assert!(!is_allowed_windows_package_identity("OpenAI.Codex_Evil"));
    }

    #[test]
    fn lifecycle_scripts_are_built_only_from_the_official_allowlists() {
        let stop_script = windows_stop_script();
        let start_script = windows_start_script(&["OpenAI.CodexBeta".to_string()])
            .expect("allowed package can be a start target");
        for identity in super::WINDOWS_CODEX_PACKAGE_IDENTITIES {
            assert!(stop_script.contains(identity));
            assert!(start_script.contains(identity));
        }
        for executable in super::WINDOWS_CODEX_EXECUTABLES {
            assert!(stop_script.contains(executable));
            assert!(start_script.contains(executable));
        }
        assert!(!stop_script.contains("Contoso.ChatGPT-Desktop"));
        assert!(!start_script.contains("Contoso.ChatGPT-Desktop"));
        assert!(stop_script.contains("throw 'process_unverified'"));
        assert!(stop_script.contains("$targetPids -contains"));
    }

    #[test]
    fn desktop_process_matching_rejects_obvious_cli_paths() {
        let package = r"C:\Program Files\WindowsApps\OpenAI.Codex_1.2.3.4_x64__publisher";
        assert!(is_safe_windows_desktop_process(
            "Codex.exe",
            r"C:\Program Files\WindowsApps\OpenAI.Codex_1.2.3.4_x64__publisher\app\Codex.exe",
            &[package],
        ));
        assert!(!is_safe_windows_desktop_process(
            "codex.exe",
            r"C:\Users\tester\AppData\Roaming\npm\node_modules\@openai\codex\codex.exe",
            &[package],
        ));
        assert!(!is_safe_windows_desktop_process(
            "codex.exe",
            r"C:\Tools\codex.exe",
            &[package],
        ));
    }

    #[test]
    fn restart_result_serializes_with_camel_case_fields() {
        let value = serde_json::to_value(CodexRestartResult {
            success: true,
            was_running: true,
            restarted: true,
            platform: "windows".to_string(),
            message: "Codex 已重新启动".to_string(),
        })
        .expect("serialize restart result");
        assert_eq!(value["wasRunning"], true);
        assert_eq!(value["restarted"], true);
        assert!(value.get("was_running").is_none());
    }

    #[test]
    fn start_script_rejects_untrusted_package_identity() {
        assert!(windows_start_script(&["Contoso.ChatGPT-Desktop".to_string()]).is_err());
    }

    #[test]
    fn unsupported_platform_returns_safely() {
        let result = unsupported_restart_result("linux");
        assert!(!result.success);
        assert_eq!(result.platform, "linux");
        assert!(!result.was_running);
        assert!(!result.restarted);
    }

    #[test]
    fn windows_package_detection_accepts_supported_codex_packages() {
        assert_eq!(
            latest_windows_package_version([
                "OpenAI.Codex_1.2.3.4_x64__publisher",
                "OpenAI.CodexBeta_1.3.0.0_x64__publisher",
                "Other.App_99.0.0.0_x64__publisher",
            ]),
            Some("1.3.0.0".to_string())
        );
    }

    #[test]
    fn plist_parser_reads_codex_bundle_version() {
        let plist = r#"<plist><dict>
<key>CFBundleShortVersionString</key>
<string>1.2026.204</string>
</dict></plist>"#;
        assert_eq!(
            plist_string_value(plist, "CFBundleShortVersionString").as_deref(),
            Some("1.2026.204")
        );
    }
}
