use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{mpsc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

#[path = "codex_desktop_windows.rs"]
pub(crate) mod desktop_windows;

#[cfg(target_os = "windows")]
use std::env;

#[cfg(any(target_os = "windows", test))]
const WINDOWS_CODEX_PACKAGE_IDENTITIES: &[&str] =
    &["OpenAI.Codex", "OpenAI.CodexBeta", "OpenAI.ChatGPT-Desktop"];
#[cfg(target_os = "windows")]
const WINDOWS_CODEX_EXECUTABLES: &[&str] = &["ChatGPT.exe", "Codex.exe", "codex.exe"];
#[cfg(any(target_os = "macos", test))]
const MACOS_CODEX_APP_NAMES: &[&str] = &[
    "Codex.app",
    "OpenAI Codex.app",
    "OpenAI.Codex.app",
    "ChatGPT Codex.app",
    "ChatGPT.app",
];
#[cfg(any(target_os = "macos", test))]
const MACOS_CODEX_BUNDLE_IDS: &[&str] = &["com.openai.codex", "com.openai.codex.beta"];

const CODEX_VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const PROGRAM_TIMEOUT: Duration = Duration::from_secs(2);
const PROGRAM_POLL_INTERVAL: Duration = Duration::from_millis(10);
const CHILD_TERMINATION_GRACE: Duration = Duration::from_millis(250);
#[cfg(any(target_os = "macos", target_os = "windows"))]
const CODEX_DESKTOP_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(any(target_os = "macos", target_os = "windows"))]
const CODEX_DESKTOP_QUIT_TIMEOUT: Duration = Duration::from_secs(8);
#[cfg(target_os = "macos")]
const CODEX_DESKTOP_POLL_INTERVAL: Duration = Duration::from_millis(100);
#[cfg(test)]
const WINDOWS_RESTART_RESULT_PREFIX: &str = "CODEX_X_DESKTOP_RESTART";

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct MacosAppMetadata {
    bundle_id: String,
    executable: String,
    display_name: Option<String>,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
struct MacosCodexApp {
    bundle_id: String,
    bundle_path: PathBuf,
    executable_path: PathBuf,
    app_name: String,
}

static CODEX_VERSION: OnceLock<String> = OnceLock::new();

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

/// Catalog probes own their helper process tree and must not leave an npm/cmd
/// wrapper's native child holding stdout open after the bounded read finishes.
pub(crate) fn official_model_command(program: &Path, args: &[&str]) -> Command {
    #[allow(unused_mut)]
    let mut command = program_command(program, args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

/// Only pass a child spawned using `official_model_command`; its process group
/// (Unix) or PID subtree (Windows) belongs to this short-lived catalog probe.
pub(crate) fn terminate_official_model_child(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(2);
    #[cfg(unix)]
    {
        let group = format!("-{}", child.id());
        if let Ok(mut killer) = Command::new("/bin/kill")
            .args(["-KILL", "--", group.as_str()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            wait_for_child_exit(&mut killer, deadline);
            let _ = killer.kill();
            wait_for_child_exit(&mut killer, Instant::now() + CHILD_TERMINATION_GRACE);
        }
    }
    #[cfg(target_os = "windows")]
    {
        let pid = child.id().to_string();
        if let Ok(mut killer) = program_command(
            Path::new("taskkill.exe"),
            &["/PID", pid.as_str(), "/T", "/F"],
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        {
            wait_for_child_exit(&mut killer, deadline);
            let _ = killer.kill();
            wait_for_child_exit(&mut killer, Instant::now() + CHILD_TERMINATION_GRACE);
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
    run_command_with_timeout(program_command(program, args), deadline, maximum)
        .map(|(_, output)| output)
}

fn run_command_with_timeout(
    mut command: Command,
    deadline: Option<Instant>,
    maximum: Duration,
) -> Option<(u32, Output)> {
    let timeout = remaining_timeout(deadline, maximum)?;
    let command_deadline = Instant::now().checked_add(timeout)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().ok()?;
    let child_id = child.id();

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
    Some((
        child_id,
        Output {
            status,
            stdout,
            stderr,
        },
    ))
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
fn macos_bundled_codex_candidates(bundle: &Path) -> Vec<PathBuf> {
    let resources = bundle.join("Contents/Resources");
    vec![
        resources.join("codex-cli/bin/codex"),
        resources.join("codex-cli/CodexCLI.app/Contents/MacOS/codex"),
        resources.join("codex"),
    ]
}

#[cfg(target_os = "macos")]
fn visit_platform_candidates<F>(home: &Path, deadline: Option<Instant>, visit: &mut F) -> bool
where
    F: FnMut(PathBuf) -> bool,
{
    // Newer desktop builds package the CLI in codex-cli/ instead of directly
    // in Resources/. Keep both layouts and the existing PATH-first callers.
    for bundle in macos_codex_app_paths(home) {
        for candidate in macos_bundled_codex_candidates(&bundle).into_iter().take(2) {
            if deadline_expired(deadline) || !visit(candidate) {
                return false;
            }
        }
    }
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

#[cfg(any(target_os = "macos", test))]
fn macos_codex_app_paths(home: &Path) -> Vec<PathBuf> {
    [PathBuf::from("/Applications"), home.join("Applications")]
        .into_iter()
        .flat_map(|root| {
            MACOS_CODEX_APP_NAMES
                .iter()
                .map(move |name| root.join(name))
        })
        .collect()
}

#[cfg(any(target_os = "macos", test))]
fn macos_app_metadata_from_plist(plist: &str) -> Option<MacosAppMetadata> {
    Some(MacosAppMetadata {
        bundle_id: plist_string_value(plist, "CFBundleIdentifier")?,
        executable: plist_string_value(plist, "CFBundleExecutable")?,
        display_name: plist_string_value(plist, "CFBundleDisplayName")
            .or_else(|| plist_string_value(plist, "CFBundleName")),
    })
}

#[cfg(any(target_os = "macos", test))]
fn is_supported_macos_codex_bundle_id(bundle_id: &str) -> bool {
    MACOS_CODEX_BUNDLE_IDS
        .iter()
        .any(|supported| bundle_id.eq_ignore_ascii_case(supported))
}

#[cfg(any(target_os = "macos", test))]
fn parse_macos_main_process_ids(output: &str, executable_path: &Path) -> Vec<u32> {
    let executable = executable_path.to_string_lossy();
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let pid_end = line.find(char::is_whitespace)?;
            let pid = line[..pid_end].parse::<u32>().ok()?;
            let command = line[pid_end..].trim_start();
            let matches = command == executable
                || command
                    .strip_prefix(executable.as_ref())
                    .is_some_and(|rest| rest.chars().next().is_some_and(char::is_whitespace));
            matches.then_some(pid)
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn macos_app_version(deadline: Option<Instant>) -> Option<String> {
    let home = dirs::home_dir().unwrap_or_default();
    for app in macos_codex_app_paths(&home) {
        if deadline_expired(deadline) {
            return None;
        }
        if !app.is_dir() {
            continue;
        }
        let app_name = if app
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "ChatGPT.app")
        {
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

#[cfg(target_os = "macos")]
fn macos_plist_raw_value(app: &Path, key: &str, deadline: Option<Instant>) -> Option<String> {
    let plist_path = app.join("Contents/Info.plist");
    if let Ok(plist) = fs::read_to_string(&plist_path) {
        if let Some(value) = plist_string_value(&plist, key) {
            return Some(value);
        }
    }

    let plist_path = plist_path.to_str()?;
    let output = run_program(
        Path::new("/usr/bin/plutil"),
        &["-extract", key, "raw", "-o", "-", plist_path],
        deadline,
    )?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

#[cfg(target_os = "macos")]
fn macos_codex_apps(deadline: Option<Instant>) -> Result<Vec<MacosCodexApp>, String> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut apps = Vec::new();
    for app in macos_codex_app_paths(&home) {
        if deadline_expired(deadline) {
            return Err("检测 Codex/ChatGPT 客户端超时".to_string());
        }
        if !app.is_dir() {
            continue;
        }

        let plist = fs::read_to_string(app.join("Contents/Info.plist")).ok();
        let metadata = plist.as_deref().and_then(macos_app_metadata_from_plist);
        let bundle_id = metadata
            .as_ref()
            .map(|metadata| metadata.bundle_id.clone())
            .or_else(|| macos_plist_raw_value(&app, "CFBundleIdentifier", deadline));
        let executable = metadata
            .as_ref()
            .map(|metadata| metadata.executable.clone())
            .or_else(|| macos_plist_raw_value(&app, "CFBundleExecutable", deadline));
        let Some(bundle_id) = bundle_id.filter(|value| is_supported_macos_codex_bundle_id(value))
        else {
            continue;
        };
        let Some(executable) = executable.filter(|value| {
            !value.is_empty()
                && !value.contains('/')
                && !value.contains('\\')
                && !value.contains('\0')
        }) else {
            continue;
        };
        let executable_path = app.join("Contents/MacOS").join(executable);
        if !executable_path.is_file() {
            continue;
        }
        let executable_path = executable_path.canonicalize().unwrap_or(executable_path);
        let display_name = metadata
            .and_then(|metadata| metadata.display_name)
            .or_else(|| macos_plist_raw_value(&app, "CFBundleDisplayName", deadline))
            .or_else(|| macos_plist_raw_value(&app, "CFBundleName", deadline));
        let app_name = if display_name
            .as_deref()
            .is_some_and(|name| name.to_ascii_lowercase().contains("chatgpt"))
            || app
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.to_ascii_lowercase().contains("chatgpt"))
        {
            "ChatGPT"
        } else {
            "Codex"
        };
        let bundle_path = app.canonicalize().unwrap_or(app);
        apps.push(MacosCodexApp {
            bundle_id,
            bundle_path,
            executable_path,
            app_name: app_name.to_string(),
        });
    }
    if apps.is_empty() {
        Err("未找到可重启的 Codex/ChatGPT 桌面客户端".to_string())
    } else {
        Ok(apps)
    }
}

#[cfg(target_os = "macos")]
fn macos_main_process_ids(
    executable_path: &Path,
    deadline: Option<Instant>,
) -> Result<Vec<u32>, String> {
    let output = run_program_with_timeout(
        Path::new("/bin/ps"),
        &["-axo", "pid=,command="],
        deadline,
        CODEX_DESKTOP_COMMAND_TIMEOUT,
    )
    .ok_or_else(|| "读取 Codex/ChatGPT 客户端进程失败或超时".to_string())?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if message.is_empty() {
            "读取 Codex/ChatGPT 客户端进程失败".to_string()
        } else {
            format!("读取 Codex/ChatGPT 客户端进程失败: {message}")
        });
    }
    Ok(parse_macos_main_process_ids(
        &String::from_utf8_lossy(&output.stdout),
        executable_path,
    ))
}

pub(crate) fn desktop_command_output(
    program: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<Output, String> {
    run_program_with_timeout(program, args, Some(Instant::now() + timeout), timeout)
        .ok_or_else(|| "客户端系统操作超时，未继续重启或连接客户端。".into())
}

// A failed process query is neither proof that the old app is still running nor
// proof that it has stopped. Keep confirmation bounded and require consecutive
// successful observations before proceeding with the next lifecycle step.
#[cfg(any(target_os = "macos", test))]
struct DesktopProcessObservation {
    running: bool,
    settle: Duration,
    stable: Option<(Vec<u32>, Instant)>,
    last: Result<Vec<u32>, String>,
}

#[cfg(any(target_os = "macos", test))]
impl DesktopProcessObservation {
    fn new(running: bool, settle: Duration) -> Self {
        Self {
            running,
            settle,
            stable: None,
            last: Err("尚未确认客户端进程状态".into()),
        }
    }

    fn observe(&mut self, sample: Result<Vec<u32>, String>, now: Instant) -> Option<Vec<u32>> {
        self.last = sample.map(|mut pids| {
            pids.sort_unstable();
            pids.dedup();
            pids
        });
        let Ok(pids) = &self.last else {
            self.stable = None;
            return None;
        };
        if self.running == pids.is_empty() {
            self.stable = None;
            return None;
        }
        let (previous, since) = self.stable.get_or_insert_with(|| (pids.clone(), now));
        if previous != pids {
            *previous = pids.clone();
            *since = now;
        }
        (now.saturating_duration_since(*since) >= self.settle).then(|| pids.clone())
    }

    fn confirmed_absent(&self) -> bool {
        matches!(&self.last, Ok(pids) if pids.is_empty())
    }

    fn error(&self, context: &str) -> String {
        match &self.last {
            Err(error) => format!("{context}；进程状态暂时无法确认：{error}"),
            _ => context.to_owned(),
        }
    }
}

#[cfg(target_os = "macos")]
fn observe_macos_processes(
    executable: &Path,
    deadline: Instant,
    observation: &mut DesktopProcessObservation,
) -> Option<Vec<u32>> {
    while Instant::now() < deadline {
        let sample = macos_main_process_ids(executable, Some(deadline));
        if let Some(pids) = observation.observe(sample, Instant::now()) {
            return Some(pids);
        }
        thread::sleep(CODEX_DESKTOP_POLL_INTERVAL);
    }
    None
}

#[cfg(target_os = "macos")]
fn wait_macos_stopped(executable: &Path) -> Result<(), String> {
    let mut observation = DesktopProcessObservation::new(false, Duration::from_millis(500));
    if observe_macos_processes(
        executable,
        Instant::now() + CODEX_DESKTOP_QUIT_TIMEOUT,
        &mut observation,
    )
    .is_some()
    {
        return Ok(());
    }
    Err(observation.error("未能确认 Codex/ChatGPT 已完全退出，未启动重复实例；请稍后重试"))
}

#[cfg(any(target_os = "macos", test))]
fn macos_launch_arguments(bundle: &str, home: &str, new_instance: bool) -> Vec<String> {
    let mut arguments = Vec::new();
    if new_instance {
        arguments.push("-n".into());
    }
    arguments.extend([
        "-a".to_owned(),
        bundle.to_owned(),
        "--env".to_owned(),
        format!("CODEX_HOME={home}"),
        "--stdin".to_owned(),
        "/dev/null".to_owned(),
        "--stdout".to_owned(),
        "/dev/null".to_owned(),
        "--stderr".to_owned(),
        "/dev/null".to_owned(),
    ]);
    arguments
}

#[cfg(any(target_os = "macos", test))]
fn launch_macos_with_observation(
    mut launch: impl FnMut(bool) -> Result<bool, String>,
    mut observe: impl FnMut(bool) -> Result<(), DesktopProcessObservation>,
) -> Result<(), String> {
    let mut last_error = "未确认 Codex 客户端已启动".to_owned();
    for attempt in 0..2 {
        let accepted = match launch(attempt == 0) {
            Ok(accepted) => accepted,
            Err(error) => {
                last_error = error;
                false
            }
        };
        if accepted {
            last_error = "系统已收到启动请求，但尚未确认客户端已持续运行".into();
        }
        match observe(accepted) {
            Ok(()) => return Ok(()),
            Err(observation) => {
                last_error = observation.error(&last_error);
                // A successful open receipt alone does not prove the GUI is
                // running. Recover once only after a successful empty query.
                // Recovery omits -n, so a late first launch is activated rather
                // than opening a second copy of the same app.
                if !observation.confirmed_absent() || attempt == 1 {
                    break;
                }
            }
        }
    }
    Err(format!("启动 Codex/ChatGPT 未完成：{last_error}"))
}

#[cfg(target_os = "macos")]
fn launch_macos_codex_app(app: &MacosCodexApp, home: &Path) -> Result<(), String> {
    let bundle = app
        .bundle_path
        .to_str()
        .ok_or("Codex 客户端路径不是有效文本")?;
    let home = home.to_str().ok_or("Codex 配置目录不是有效文本")?;
    launch_macos_with_observation(
        |new_instance| {
            let output = run_command_with_timeout(
                macos_open_command(&macos_launch_arguments(bundle, home, new_instance), true),
                None,
                CODEX_DESKTOP_COMMAND_TIMEOUT,
            )
            .map(|(_, output)| output)
            .ok_or_else(|| "LaunchServices 启动请求超时".to_owned())?;
            if output.status.success() {
                Ok(true)
            } else {
                let details = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                Err(if details.is_empty() {
                    "LaunchServices 未接受启动请求".into()
                } else {
                    details
                })
            }
        },
        |accepted| {
            let deadline = Instant::now()
                + if accepted {
                    CODEX_DESKTOP_QUIT_TIMEOUT
                } else {
                    Duration::from_millis(750)
                };
            let mut observation = DesktopProcessObservation::new(true, Duration::from_millis(200));
            if observe_macos_processes(&app.executable_path, deadline, &mut observation).is_some() {
                Ok(())
            } else {
                Err(observation)
            }
        },
    )
}

// LaunchServices helpers for explicit desktop restart and locale apply.
// Capability/status inspection never launches or quits an application.
#[cfg(target_os = "macos")]
fn macos_open_command(arguments: &[String], explicit_home: bool) -> Command {
    use std::os::unix::process::CommandExt;

    let mut command = Command::new("/usr/bin/open");
    command.args(arguments).process_group(0);
    // LaunchServices inherits `open`'s environment. Keep arbitrary provider
    // env_key/env_http_headers and proxies, but remove the manager's private
    // launch identity and Electron/dev injection state. Values are not inspected.
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(|name| {
            matches!(
                name,
                "__CFBundleIdentifier"
                    | "XPC_SERVICE_NAME"
                    | "XPC_FLAGS"
                    | "CODEX_DESKTOP_RELAUNCH_OPEN_EVENTS"
                    | "ELECTRON_RUN_AS_NODE"
                    | "NODE_OPTIONS"
            ) || (explicit_home && name == "CODEX_HOME")
                || ["CODEX_ELECTRON_", "TAURI_", "CODEXX_"]
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
        }) {
            command.env_remove(name);
        }
    }
    command
}

#[cfg(target_os = "macos")]
fn macos_open_launch_capabilities(require_environment: bool) -> Result<(), String> {
    static HELP: OnceLock<Result<String, String>> = OnceLock::new();
    let help = HELP
        .get_or_init(|| {
            // `open -h` exits nonzero when showing usage. Only inspect help;
            // this never opens an application or changes LaunchServices state.
            let output = desktop_command_output(
                Path::new("/usr/bin/open"),
                &["-h"],
                CODEX_DESKTOP_COMMAND_TIMEOUT,
            )?;
            Ok(format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ))
        })
        .as_ref()
        .map_err(Clone::clone)?;
    if !["--stdin", "--stdout", "--stderr"]
        .iter()
        .all(|option| help.contains(option))
    {
        return Err(
            "当前 macOS 的 open 不支持独立标准流；客户端尚未退出，请升级系统后重试。".into(),
        );
    }
    if require_environment && !help.contains("--env") {
        return Err(
            "当前 macOS 的 open 不支持独立传入配置目录；客户端尚未退出，请升级系统后重试。".into(),
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
struct MacosProcessMetadata {
    parent: u32,
    group: u32,
    command: String,
}

#[cfg(target_os = "macos")]
fn macos_process_metadata(
    pid: u32,
    deadline: Option<Instant>,
) -> Result<MacosProcessMetadata, String> {
    let pid = pid.to_string();
    let output = run_program_with_timeout(
        Path::new("/bin/ps"),
        &["-ww", "-p", &pid, "-o", "ppid=,pgid=,command="],
        deadline,
        CODEX_DESKTOP_COMMAND_TIMEOUT,
    )
    .filter(|output| output.status.success())
    .ok_or("无法确认客户端进程的独立启动身份")?;
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    let (parent, rest) = text
        .split_once(char::is_whitespace)
        .ok_or("客户端进程身份返回无效")?;
    let (group, command) = rest
        .trim_start()
        .split_once(char::is_whitespace)
        .ok_or("客户端进程组返回无效")?;
    Ok(MacosProcessMetadata {
        parent: parent.parse().map_err(|_| "客户端父进程标识无效")?,
        group: group.parse().map_err(|_| "客户端进程组标识无效")?,
        command: command.trim_start().to_owned(),
    })
}

#[cfg(target_os = "macos")]
pub(crate) fn locale_validate_launch() -> Result<(), String> {
    macos_open_launch_capabilities(true)?;
    let own = macos_process_metadata(std::process::id(), None)?;
    if own.group == 0 {
        return Err("无法确认 Codex-X 进程组，客户端尚未退出。".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn locale_launch_desktop(
    bundle_path: &Path,
    executable: &Path,
    home: &Path,
    port: Option<u16>,
) -> Result<u32, String> {
    macos_open_launch_capabilities(true)?;
    let bundle = bundle_path.to_str().ok_or("Codex 客户端路径无效")?;
    let home = home.to_str().ok_or("Codex 配置目录不是有效文本")?;
    let own_pid = std::process::id();
    let own_group = macos_process_metadata(own_pid, None)?.group;
    if own_group == 0 {
        return Err("无法确认 Codex-X 进程组，未启动客户端。".into());
    }
    if !macos_main_process_ids(executable, None)?.is_empty() {
        return Err("官方客户端已在运行，未另起实例；请完全退出后重试。".into());
    }
    let mut arguments = vec![
        "-n".to_owned(),
        "-a".to_owned(),
        bundle.to_owned(),
        "--env".to_owned(),
        format!("CODEX_HOME={home}"),
        "--stdin".to_owned(),
        "/dev/null".to_owned(),
        "--stdout".to_owned(),
        "/dev/null".to_owned(),
        "--stderr".to_owned(),
        "/dev/null".to_owned(),
    ];
    if let Some(port) = port {
        arguments.extend([
            "--args".to_owned(),
            format!("--remote-debugging-port={port}"),
            "--remote-debugging-address=127.0.0.1".to_owned(),
        ]);
    }
    // Own and reap only the short-lived `open` client. The GUI is launched by
    // LaunchServices, never kept as a Child or tied to Codex-X's process group.
    let (open_pid, output) = run_command_with_timeout(
        macos_open_command(&arguments, true),
        None,
        CODEX_DESKTOP_COMMAND_TIMEOUT,
    )
    .ok_or("LaunchServices 启动未完成；未回退到直接运行客户端可执行文件。")?;
    let mut last_error = if output.status.success() {
        "LaunchServices 已收到启动请求，尚未确认新的官方主进程。".to_owned()
    } else {
        format!(
            "LaunchServices 未确认启动：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    };
    let deadline = Instant::now() + CODEX_DESKTOP_QUIT_TIMEOUT;
    let mut observed = None;
    while Instant::now() < deadline {
        let pids = match macos_main_process_ids(executable, Some(deadline)) {
            Ok(pids) => pids,
            Err(error) => {
                last_error = error;
                observed = None;
                thread::sleep(CODEX_DESKTOP_POLL_INTERVAL);
                continue;
            }
        };
        if pids.len() > 1 {
            return Err("发现多个官方主进程，未连接任何实例；请保留一个后重试。".into());
        }
        if let Some(&pid) = pids.first() {
            let identity = match macos_process_metadata(pid, Some(deadline)) {
                Ok(identity) => identity,
                Err(error) => {
                    last_error = error;
                    observed = None;
                    thread::sleep(CODEX_DESKTOP_POLL_INTERVAL);
                    continue;
                }
            };
            if pid == own_pid
                || pid == open_pid
                || identity.parent == own_pid
                || identity.parent == open_pid
                || identity.group == 0
                || identity.group == own_group
            {
                return Err(
                    "官方客户端未独立于 Codex-X 启动，未执行语言兼容连接。请手动打开客户端。"
                        .into(),
                );
            }
            let matches_executable =
                parse_macos_main_process_ids(&format!("{pid} {}", identity.command), executable)
                    .contains(&pid);
            let matches_port = port.is_none_or(|port| {
                let flag = format!("--remote-debugging-port={port}");
                identity.command.split_whitespace().any(|arg| arg == flag)
            });
            if matches_executable && matches_port {
                let (previous, since) = observed.get_or_insert_with(|| (pid, Instant::now()));
                if *previous == pid && since.elapsed() >= Duration::from_millis(200) {
                    return Ok(pid);
                }
                if *previous != pid {
                    *previous = pid;
                    *since = Instant::now();
                }
            } else {
                observed = None;
                last_error = "新的官方进程未确认所请求的启动参数，未连接该实例。".into();
            }
        } else {
            observed = None;
        }
        thread::sleep(CODEX_DESKTOP_POLL_INTERVAL);
    }
    Err(last_error)
}

#[cfg(target_os = "macos")]
pub(crate) fn locale_platform_command(program: &Path, args: &[&str]) -> Result<Output, String> {
    let deadline = Instant::now() + CODEX_DESKTOP_COMMAND_TIMEOUT;
    let output =
        run_program_with_timeout(program, args, Some(deadline), CODEX_DESKTOP_COMMAND_TIMEOUT)
            .ok_or("客户端操作超时，未继续应用语言兼容。")?;
    if !output.status.success() {
        return Err("客户端操作未完成，请检查官方客户端状态后重试。".into());
    }
    Ok(output)
}

#[cfg(target_os = "macos")]
pub(crate) fn locale_desktop_app() -> Result<(PathBuf, PathBuf, String, String, String), String> {
    locale_desktop_app_until(Some(Instant::now() + CODEX_DESKTOP_COMMAND_TIMEOUT))
}

#[cfg(target_os = "macos")]
fn locale_desktop_app_until(
    deadline: Option<Instant>,
) -> Result<(PathBuf, PathBuf, String, String, String), String> {
    let apps: Vec<_> = macos_codex_apps(deadline)?
        .into_iter()
        .filter(|app| {
            matches!(
                app.bundle_id.as_str(),
                "com.openai.codex" | "com.openai.codex.beta"
            )
        })
        .collect();
    let mut running = Vec::new();
    for app in &apps {
        if !macos_main_process_ids(&app.executable_path, deadline)?.is_empty() {
            running.push(app);
        }
    }
    if running.len() > 1 {
        return Err("检测到多个官方客户端实例，请先保留一个实例再应用界面语言。".into());
    }
    let app = running
        .first()
        .copied()
        .or_else(|| apps.first())
        .ok_or("未找到官方 Codex 客户端")?;
    let version = macos_info_plist_version(&app.bundle_path).unwrap_or_else(|| "unknown".into());
    Ok((
        app.bundle_path.clone(),
        app.executable_path.clone(),
        app.bundle_id.clone(),
        app.app_name.clone(),
        version,
    ))
}

#[cfg(target_os = "macos")]
pub(crate) fn locale_desktop_pids(executable: &Path) -> Result<Vec<u32>, String> {
    macos_main_process_ids(
        executable,
        Some(Instant::now() + CODEX_DESKTOP_COMMAND_TIMEOUT),
    )
}

#[cfg(target_os = "macos")]
pub(crate) fn locale_quit_desktop(executable: &Path, bundle_id: &str) -> Result<(), String> {
    if !MACOS_CODEX_BUNDLE_IDS.contains(&bundle_id) {
        return Err("不支持的客户端身份".into());
    }
    if locale_desktop_pids(executable)?.is_empty() {
        return wait_macos_stopped(executable);
    }
    let requested = locale_platform_command(
        Path::new("/usr/bin/osascript"),
        &[
            "-e",
            &format!("tell application id \"{bundle_id}\" to quit"),
        ],
    );
    // Apple Events may lose the connection while the application exits. An
    // error from osascript is not proof that the exact selected process lives.
    match wait_macos_stopped(executable) {
        Ok(()) => Ok(()),
        Err(error) => Err(requested
            .err()
            .map(|request| format!("{request}；{error}"))
            .unwrap_or(error)),
    }
}

#[cfg(target_os = "macos")]
pub fn restart_codex_desktop(home: &Path) -> Result<(String, bool), String> {
    let detection_deadline = Some(Instant::now() + CODEX_DESKTOP_COMMAND_TIMEOUT);
    let apps = macos_codex_apps(detection_deadline)?;
    let mut first_installed = None;
    let mut running = Vec::new();
    for app in apps {
        if !macos_main_process_ids(&app.executable_path, detection_deadline)?.is_empty() {
            running.push(app);
        } else if first_installed.is_none() {
            first_installed = Some(app);
        }
    }
    if running.len() > 1 {
        return Err("检测到多个官方 Codex 客户端，请先保留一个再重启。".into());
    }
    let was_running = !running.is_empty();
    let app = running
        .pop()
        .or(first_installed)
        .ok_or("未找到可重启的 Codex 客户端")?;
    // Validate the selected home and LaunchServices environment support before
    // quitting; a plain restart must not silently open a different config scope.
    home.to_str().ok_or("Codex 配置目录不是有效文本")?;
    macos_open_launch_capabilities(true)?;
    locale_quit_desktop(&app.executable_path, &app.bundle_id)?;
    launch_macos_codex_app(&app, home)?;
    Ok((app.app_name, was_running))
}

#[cfg(not(target_os = "macos"))]
fn macos_app_version(_deadline: Option<Instant>) -> Option<String> {
    None
}

#[cfg(test)]
fn parse_windows_restart_result(output: &str) -> Option<(String, bool)> {
    output.lines().rev().find_map(|line| {
        let payload = line
            .trim()
            .strip_prefix(WINDOWS_RESTART_RESULT_PREFIX)?
            .strip_prefix('\t')?;
        let mut fields = payload.split('\t');
        let app_name = fields.next()?.trim();
        let was_running = match fields.next()?.trim() {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        if fields.next().is_some() || !matches!(app_name, "Codex" | "ChatGPT") {
            return None;
        }
        Some((app_name.to_string(), was_running))
    })
}

#[cfg(test)]
fn windows_restart_script() -> String {
    let identities = WINDOWS_CODEX_PACKAGE_IDENTITIES
        .iter()
        .map(|identity| format!("'{}'", identity.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    r#"
$ErrorActionPreference = 'Stop'
$utf8 = [System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = $utf8
$OutputEncoding = $utf8
$identities = @(__IDENTITIES__)
$packages = @(Get-AppxPackage |
  Where-Object { $identities -contains $_.Name } |
  Sort-Object -Property Version -Descending |
  Select-Object -Unique)
if ($packages.Count -eq 0) { throw 'Supported Codex/ChatGPT Appx package was not found.' }

function Get-CodexDesktopProcesses([string]$root) {
  $appRoot = $root + 'app\'
  $resourcesRoot = $appRoot + 'resources\'
  @(Get-Process -ErrorAction SilentlyContinue | Where-Object {
    try {
      $processPath = [IO.Path]::GetFullPath($_.MainModule.FileName)
      $executableName = [IO.Path]::GetFileName($processPath)
      ($executableName -ieq 'Codex.exe' -or $executableName -ieq 'ChatGPT.exe') -and
        $processPath.StartsWith($appRoot, [StringComparison]::OrdinalIgnoreCase) -and
        -not $processPath.StartsWith($resourcesRoot, [StringComparison]::OrdinalIgnoreCase)
    } catch {
      $false
    }
  })
}

$package = $null
$installRoot = $null
$processes = @()
foreach ($candidate in $packages) {
  $candidateRoot = [IO.Path]::GetFullPath([string]$candidate.InstallLocation).TrimEnd('\') + '\'
  $candidateProcesses = @(Get-CodexDesktopProcesses $candidateRoot)
  if ($candidateProcesses.Count -gt 0) {
    $package = $candidate
    $installRoot = $candidateRoot
    $processes = $candidateProcesses
    break
  }
}
if ($null -eq $package) {
  $package = $packages[0]
  $installRoot = [IO.Path]::GetFullPath([string]$package.InstallLocation).TrimEnd('\') + '\'
  $processes = @(Get-CodexDesktopProcesses $installRoot)
}

$manifest = Get-AppxPackageManifest -Package $package.PackageFullName
$application = @($manifest.Package.Applications.Application) | Select-Object -First 1
$applicationId = [string]$application.Id
$packageFamily = [string]$package.PackageFamilyName
if ([string]::IsNullOrWhiteSpace($applicationId) -or [string]::IsNullOrWhiteSpace($packageFamily)) {
  throw 'Codex/ChatGPT Appx package does not expose a launchable application id.'
}

$wasRunning = $processes.Count -gt 0
if ($wasRunning) {
  foreach ($process in $processes) {
    Stop-Process -InputObject $process -Force -ErrorAction SilentlyContinue
  }

  $quitDeadline = [DateTime]::UtcNow.AddSeconds(8)
  do {
    $remaining = @(Get-CodexDesktopProcesses $installRoot)
    if ($remaining.Count -eq 0) { break }
    Start-Sleep -Milliseconds 100
  } while ([DateTime]::UtcNow -lt $quitDeadline)
  $remaining = @(Get-CodexDesktopProcesses $installRoot)
  if ($remaining.Count -gt 0) {
    throw 'Codex/ChatGPT background processes could not be stopped before the timeout; relaunch was cancelled.'
  }
}

$aumid = $packageFamily + '!' + $applicationId
Start-Process -FilePath 'explorer.exe' -ArgumentList ('shell:AppsFolder\' + $aumid) | Out-Null
$appName = if ($package.Name -eq 'OpenAI.ChatGPT-Desktop') { 'ChatGPT' } else { 'Codex' }
$runningFlag = if ($wasRunning) { '1' } else { '0' }
[Console]::Out.WriteLine('__RESULT_PREFIX__' + "`t" + $appName + "`t" + $runningFlag)
"#
    .replace("__IDENTITIES__", &identities)
    .replace("__RESULT_PREFIX__", WINDOWS_RESTART_RESULT_PREFIX)
}

#[cfg(target_os = "windows")]
pub fn restart_codex_desktop(home: &Path) -> Result<(String, bool), String> {
    desktop_windows::restart(home)
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

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
pub fn restart_codex_desktop(_home: &Path) -> Result<(String, bool), String> {
    Err("当前平台暂不支持重启 Codex/ChatGPT 桌面客户端".to_string())
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

/// Prefer the installed desktop's own catalog over a potentially older PATH
/// CLI. Other callers retain the original executable-candidate ordering.
pub(crate) fn official_model_executable_candidates() -> Vec<PathBuf> {
    // This path runs before a picker can return even a cached catalog. Bound
    // discovery across desktop detection and extension/profile traversal.
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    #[cfg(target_os = "macos")]
    {
        if let Ok((bundle, _, _, _, _)) = locale_desktop_app_until(Some(deadline)) {
            append_unique_candidates(
                &mut candidates,
                &mut seen,
                macos_bundled_codex_candidates(&bundle),
            );
        }
        let home = dirs::home_dir().unwrap_or_default();
        for bundle in macos_codex_app_paths(&home) {
            append_unique_candidates(
                &mut candidates,
                &mut seen,
                macos_bundled_codex_candidates(&bundle),
            );
        }
    }
    append_unique_candidates(
        &mut candidates,
        &mut seen,
        codex_executable_candidates_until(Some(deadline)),
    );
    candidates
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
    use super::{
        cached_codex_version, is_supported_macos_codex_bundle_id, latest_windows_package_version,
        macos_app_metadata_from_plist, macos_codex_app_paths, parse_macos_main_process_ids,
        parse_windows_restart_result, plist_string_value, run_program, version_line,
        visit_named_files, windows_restart_script,
    };
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::OnceLock;
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
    fn windows_restart_result_parser_ignores_noise_and_reads_state() {
        let output = "PowerShell startup text\nCODEX_X_DESKTOP_RESTART\tChatGPT\t1\n";
        assert_eq!(
            parse_windows_restart_result(output),
            Some(("ChatGPT".to_string(), true))
        );
        assert_eq!(
            parse_windows_restart_result("CODEX_X_DESKTOP_RESTART\tCodex\t0\n"),
            Some(("Codex".to_string(), false))
        );
        assert_eq!(
            parse_windows_restart_result("CODEX_X_DESKTOP_RESTART\tOther\t1\n"),
            None
        );
    }

    #[test]
    fn windows_restart_script_only_stops_processes_in_the_selected_package_directory() {
        let script = windows_restart_script();
        assert!(script.contains("$processPath.StartsWith($appRoot"));
        assert!(script.contains("$executableName -ieq 'Codex.exe'"));
        assert!(script.contains("$executableName -ieq 'ChatGPT.exe'"));
        assert!(script.contains("-not $processPath.StartsWith($resourcesRoot"));
        assert!(script.contains("Stop-Process -InputObject $process -Force"));
        assert!(script.contains("[Console]::OutputEncoding = $utf8"));
        assert!(script.contains("shell:AppsFolder\\"));
        assert!(!script.contains("CloseMainWindow"));
        assert!(!script.contains("Get-Process -Name"));
        assert!(!script.contains("Stop-Process -Name"));
        assert!(!script.contains("taskkill"));
        assert!(!script.contains("ApplicationFrameHost"));
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

    #[test]
    fn macos_app_metadata_parser_reads_exact_desktop_identity() {
        let plist = r#"<plist><dict>
<key>CFBundleDisplayName</key>
<string>ChatGPT</string>
<key>CFBundleExecutable</key>
<string>ChatGPT</string>
<key>CFBundleIdentifier</key>
<string>com.openai.codex</string>
</dict></plist>"#;
        let metadata = macos_app_metadata_from_plist(plist).expect("parse app metadata");
        assert_eq!(metadata.bundle_id, "com.openai.codex");
        assert_eq!(metadata.executable, "ChatGPT");
        assert_eq!(metadata.display_name.as_deref(), Some("ChatGPT"));
        assert!(is_supported_macos_codex_bundle_id(&metadata.bundle_id));
        assert!(!is_supported_macos_codex_bundle_id("com.example.codex"));
        assert!(macos_app_metadata_from_plist(
            "<plist><dict><key>CFBundleIdentifier</key><string>com.openai.codex</string></dict></plist>"
        )
        .is_none());
    }

    #[test]
    fn macos_app_candidates_reuse_supported_names_in_stable_order() {
        let paths = macos_codex_app_paths(Path::new("/Users/tester"));
        assert_eq!(
            paths.first(),
            Some(&Path::new("/Applications/Codex.app").to_path_buf())
        );
        assert_eq!(
            paths.last(),
            Some(&Path::new("/Users/tester/Applications/ChatGPT.app").to_path_buf())
        );
    }

    #[test]
    fn macos_process_confirmation_survives_read_errors_without_treating_them_as_exit() {
        use super::DesktopProcessObservation;
        let start = Instant::now();
        let mut stopped = DesktopProcessObservation::new(false, Duration::from_millis(500));
        assert!(stopped.observe(Ok(vec![]), start).is_none());
        assert!(stopped
            .observe(
                Err("temporary ps failure".into()),
                start + Duration::from_millis(400)
            )
            .is_none());
        assert!(!stopped.confirmed_absent());
        assert!(stopped
            .observe(Ok(vec![]), start + Duration::from_millis(500))
            .is_none());
        assert!(stopped
            .observe(Ok(vec![]), start + Duration::from_millis(999))
            .is_none());
        assert_eq!(
            stopped.observe(Ok(vec![]), start + Duration::from_millis(1000)),
            Some(vec![])
        );

        let mut started = DesktopProcessObservation::new(true, Duration::from_millis(200));
        assert!(started
            .observe(Err("temporary ps failure".into()), start)
            .is_none());
        assert!(started
            .observe(Ok(vec![42]), start + Duration::from_millis(10))
            .is_none());
        assert_eq!(
            started.observe(Ok(vec![42]), start + Duration::from_millis(210)),
            Some(vec![42])
        );
    }

    #[test]
    fn macos_successful_launch_receipt_without_gui_recovers_once_without_new_instance() {
        use super::{launch_macos_with_observation, DesktopProcessObservation};
        let mut launches = Vec::new();
        let mut checks = 0;
        let result = launch_macos_with_observation(
            |new_instance| {
                launches.push(new_instance);
                Ok(true)
            },
            |accepted| {
                assert!(accepted);
                checks += 1;
                if checks == 1 {
                    let mut empty = DesktopProcessObservation::new(true, Duration::ZERO);
                    empty.observe(Ok(vec![]), Instant::now());
                    Err(empty)
                } else {
                    Ok(())
                }
            },
        );
        assert!(result.is_ok());
        assert_eq!(launches, vec![true, false]);
    }

    #[test]
    fn macos_unknown_or_starting_process_does_not_trigger_another_launch() {
        use super::{launch_macos_with_observation, DesktopProcessObservation};
        for sample in [Err("temporary process query failure".into()), Ok(vec![42])] {
            let mut launches = 0;
            let result = launch_macos_with_observation(
                |_| {
                    launches += 1;
                    Ok(true)
                },
                |_| {
                    let mut uncertain =
                        DesktopProcessObservation::new(true, Duration::from_secs(1));
                    uncertain.observe(sample.clone(), Instant::now());
                    Err(uncertain)
                },
            );
            assert!(result.is_err());
            assert_eq!(launches, 1);
        }
    }

    #[test]
    fn macos_launch_error_with_a_confirmed_gui_is_success_and_recovery_is_bounded() {
        use super::{launch_macos_with_observation, DesktopProcessObservation};
        let mut launches = 0;
        assert!(launch_macos_with_observation(
            |_| {
                launches += 1;
                Err("late open failure".into())
            },
            |accepted| {
                assert!(!accepted);
                Ok(())
            },
        )
        .is_ok());
        assert_eq!(launches, 1);
        launches = 0;
        assert!(launch_macos_with_observation(
            |_| {
                launches += 1;
                Ok(true)
            },
            |_| {
                let mut empty = DesktopProcessObservation::new(true, Duration::ZERO);
                empty.observe(Ok(vec![]), Instant::now());
                Err(empty)
            },
        )
        .is_err());
        assert_eq!(launches, 2);
    }

    #[test]
    fn macos_recovery_keeps_configuration_scope_and_detached_streams() {
        use super::macos_launch_arguments;
        let first = macos_launch_arguments("/Applications/ChatGPT.app", "/tmp/配置 空间", true);
        let retry = macos_launch_arguments("/Applications/ChatGPT.app", "/tmp/配置 空间", false);
        assert_eq!(first[0], "-n");
        assert_eq!(&first[1..], retry.as_slice());
        assert!(retry
            .windows(2)
            .any(|args| args == ["--env", "CODEX_HOME=/tmp/配置 空间"]));
        for stream in ["--stdin", "--stdout", "--stderr"] {
            assert!(retry.windows(2).any(|args| args == [stream, "/dev/null"]));
        }
        assert!(!retry
            .iter()
            .any(|arg| arg.starts_with("--remote-debugging")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "manually exercises LaunchServices with a disposable test app, never an official client"]
    fn macos_launch_services_fixture_survives_launcher_exit_and_restarts() {
        use super::{
            launch_macos_codex_app, macos_main_process_ids, macos_process_metadata,
            run_command_with_timeout, wait_macos_stopped, MacosCodexApp,
        };
        use serde_json::{json, Value};
        use std::process::Command;
        const TEST: &str =
            "platform::tests::macos_launch_services_fixture_survives_launcher_exit_and_restarts";
        let worker = std::env::var_os("CODEXX_FIXTURE_ROOT").map(std::path::PathBuf::from);
        let temporary = worker.is_none().then(|| tempfile::tempdir().unwrap());
        let root = worker
            .clone()
            .unwrap_or_else(|| temporary.as_ref().unwrap().path().to_owned())
            .canonicalize()
            .unwrap();
        let app = MacosCodexApp {
            bundle_path: root.join("RestartFixture.app"),
            executable_path: root.join("RestartFixture.app/Contents/MacOS/CodexXRestartFixture"),
            bundle_id: "dev.codexx.restart-fixture".into(),
            app_name: "Codex-X restart fixture".into(),
        };
        let home = root.join("配置 空间");
        if worker.is_some() {
            assert_eq!(
                fs::read_to_string(root.join("fixture-marker")).unwrap(),
                "disposable restart fixture"
            );
            let own = macos_process_metadata(std::process::id(), None).unwrap();
            launch_macos_codex_app(&app, &home).unwrap();
            fs::write(
                root.join("launcher.json"),
                json!({"pid":std::process::id(),"group":own.group}).to_string(),
            )
            .unwrap();
            return;
        }
        fs::create_dir_all(app.executable_path.parent().unwrap()).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(root.join("fixture-marker"), "disposable restart fixture").unwrap();
        fs::write(app.bundle_path.join("Contents/Info.plist"), format!(
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>dev.codexx.restart-fixture.p{}</string><key>CFBundleExecutable</key><string>CodexXRestartFixture</string><key>CFBundleName</key><string>Codex-X restart fixture</string><key>CFBundlePackageType</key><string>APPL</string><key>LSUIElement</key><true/></dict></plist>", std::process::id())).unwrap();
        let source = root.join("fixture.m");
        fs::write(&source, include_str!("testdata/macos-restart-fixture.m")).unwrap();
        let built = Command::new("/usr/bin/xcrun")
            .args(["clang", "-fobjc-arc", "-framework", "AppKit"])
            .arg(&source)
            .arg("-o")
            .arg(&app.executable_path)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        let result = std::panic::catch_unwind(|| {
            let mut launcher = Command::new(std::env::current_exe().unwrap());
            launcher
                .args([TEST, "--ignored", "--exact", "--nocapture"])
                .env("CODEXX_FIXTURE_ROOT", &root)
                .env("__CFBundleIdentifier", "dev.codexx.fake-manager")
                .env("TAURI_FIXTURE_STATE", "must-not-leak")
                .env("NODE_OPTIONS", "--fixture-must-not-leak")
                .env("RESTART_FIXTURE_PROVIDER", "synthetic-value")
                .env("CODEX_HOME", root.join("wrong-config-scope"));
            let (_, output) = run_command_with_timeout(launcher, None, Duration::from_secs(30))
                .expect("disposable launcher completes and releases its pipes");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let launched: Value =
                serde_json::from_slice(&fs::read(root.join("launcher.json")).unwrap()).unwrap();
            let state: Value =
                serde_json::from_slice(&fs::read(home.join("started.json")).unwrap()).unwrap();
            let first_pid = state["pid"].as_u64().unwrap() as u32;
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!(
                macos_main_process_ids(&app.executable_path, None).unwrap(),
                vec![first_pid]
            );
            let actual = macos_process_metadata(first_pid, None).unwrap();
            assert_ne!(actual.parent as u64, launched["pid"].as_u64().unwrap());
            assert_ne!(actual.group as u64, launched["group"].as_u64().unwrap());
            assert_eq!(state["home"].as_str(), home.to_str());
            for key in [
                "providerPreserved",
                "managerIdentityAbsent",
                "managerStateAbsent",
                "nodeOptionsAbsent",
            ] {
                assert_eq!(state[key], true, "{key}");
            }
            fs::write(home.join("quit"), "exit only this fixture").unwrap();
            wait_macos_stopped(&app.executable_path).unwrap();
            fs::remove_file(home.join("quit")).unwrap();
            fs::remove_file(home.join("started.json")).unwrap();
            launch_macos_codex_app(&app, &home).unwrap();
            let second: Value =
                serde_json::from_slice(&fs::read(home.join("started.json")).unwrap()).unwrap();
            let second_pid = second["pid"].as_u64().unwrap() as u32;
            assert_ne!(first_pid, second_pid);
            assert_eq!(
                macos_main_process_ids(&app.executable_path, None).unwrap(),
                vec![second_pid]
            );
            assert_eq!(second["home"].as_str(), home.to_str());
            println!("LaunchServices fixture: launcher exited, client remained independent; config scope and environment verified; quit/relaunch confirmed with a new PID.");
        });
        // Cleanup is restricted to this generated executable and bundle path.
        let _ = fs::write(home.join("quit"), "exit only this fixture");
        if wait_macos_stopped(&app.executable_path).is_err() {
            for pid in macos_main_process_ids(&app.executable_path, None).unwrap_or_default() {
                let _ = Command::new("/bin/kill")
                    .args(["-TERM", &pid.to_string()])
                    .status();
            }
            let _ = wait_macos_stopped(&app.executable_path);
        }
        let _ = Command::new("/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister")
            .arg("-u").arg(&app.bundle_path).output();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn macos_process_parser_only_matches_the_exact_main_executable() {
        let executable = Path::new("/Applications/ChatGPT.app/Contents/MacOS/ChatGPT");
        let output = "  101 /Applications/ChatGPT.app/Contents/MacOS/ChatGPT\n\
  102 /Applications/ChatGPT.app/Contents/MacOS/ChatGPT --started-from-launch-services\n\
  103 /Applications/ChatGPT.app/Contents/Frameworks/Codex (Renderer).app/Contents/MacOS/Codex (Renderer)\n\
  104 /Applications/ChatGPT.app/Contents/MacOS/ChatGPT-helper\n\
  105 /Applications/Other.app/Contents/MacOS/ChatGPT\n";
        assert_eq!(
            parse_macos_main_process_ids(output, executable),
            vec![101, 102]
        );
    }
}
