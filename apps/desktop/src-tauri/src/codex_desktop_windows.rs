//! Windows desktop discovery and activation. This module deliberately uses
//! Windows' registered package/signature/process APIs, never executable names
//! alone, before stopping a process or attaching to its debugging endpoint.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WindowsDesktopApp {
    pub(crate) path: PathBuf,
    pub(crate) executable: PathBuf,
    pub(crate) bundle_id: String,
    pub(crate) name: String,
    pub(crate) version: String,
}

fn stamps() -> &'static Mutex<HashMap<u32, String>> {
    static STAMPS: OnceLock<Mutex<HashMap<u32, String>>> = OnceLock::new();
    STAMPS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn powershell(script: &str, data: Value, timeout: Duration) -> Result<Value, String> {
    let system_root = std::env::var_os("SystemRoot").ok_or("无法定位 Windows 系统目录")?;
    let executable =
        PathBuf::from(system_root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let encoded = base64::engine::general_purpose::STANDARD
        .encode(serde_json::to_vec(&data).map_err(|_| "无法编码 Windows 客户端操作参数")?);
    // All variable input is JSON data. Paths and config values are never
    // interpolated as PowerShell source or passed through a command shell.
    let source = format!(
        "$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; \
         [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); \
         $data=([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{encoded}')) | ConvertFrom-Json);\n\
         {COMMON}\ntry {{ & {{ {script} }} }} catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 1 }}"
    );
    let output = crate::platform::desktop_command_output(
        &executable,
        &[
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &source,
        ],
        timeout,
    )?;
    if !output.status.success() {
        return Err(operation_error(&String::from_utf8_lossy(&output.stderr)));
    }
    serde_json::from_slice(&output.stdout).map_err(|_| "Windows 未返回有效的客户端操作结果".into())
}

fn operation_error(message: &str) -> String {
    if message.contains("CODEXX_STORE_ELEVATION_REQUIRED") {
        return "Codex-X 正以管理员身份运行，无法直接启动 Microsoft Store 版 Codex。请使用普通权限重新打开 Codex-X。".into();
    }
    if message.contains("CODEXX_STORE_BROKER_UNAVAILABLE") {
        return "Windows 未找到同一用户的普通权限桌面，无法代为启动 Microsoft Store 版 Codex。"
            .into();
    }
    if message.contains("CODEXX_PROCESS_ACCESS_REQUIRED") {
        return "无法读取 Codex/ChatGPT 进程身份，请确认它与 Codex-X 使用相同用户和权限运行。"
            .into();
    }
    format!(
        "Windows Codex 操作未完成：{}",
        message.trim().chars().take(1800).collect::<String>()
    )
}

const COMMON: &str = r#"
function FullPath($path) { [IO.Path]::GetFullPath([string]$path).TrimEnd('\') }
function SamePath($left, $right) {
  [string]::Equals((FullPath $left), (FullPath $right), [StringComparison]::OrdinalIgnoreCase)
}
function ProcessStamp($process) {
  if ($null -eq $process.CreationDate) { throw 'Cannot verify the process creation time.' }
  $process.CreationDate.ToUniversalTime().Ticks.ToString([Globalization.CultureInfo]::InvariantCulture)
}
function EnsureProcessApi {
  if ('CodexDesktopProcess' -as [type]) { return }
  Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text;
public sealed class CodexDesktopProcess : IDisposable {
  [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool CloseHandle(IntPtr handle);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool QueryFullProcessImageName(IntPtr handle, uint flags, StringBuilder path, ref uint length);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetProcessTimes(IntPtr handle, out long created, out long exit, out long kernel, out long user);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetExitCodeProcess(IntPtr handle, out uint code);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool ProcessIdToSessionId(uint pid, out uint session);
  [DllImport("kernel32.dll", SetLastError=true)] static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool TerminateProcess(IntPtr handle, uint code);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool OpenProcessToken(IntPtr handle, uint access, out IntPtr token);
  IntPtr handle;
  public uint ProcessId { get; private set; }
  public string ExecutablePath { get; private set; }
  public DateTime CreationDate { get; private set; }
  public uint SessionId { get; private set; }
  public bool HasExited {
    get { uint result=WaitForSingleObject(handle,0); if(result==0xffffffff) throw new Win32Exception(Marshal.GetLastWin32Error()); return result==0; }
  }
  static bool Exited(IntPtr h) { uint code; return GetExitCodeProcess(h,out code) && code!=259; }
  static CodexDesktopProcess Read(uint pid, uint access) {
    IntPtr h=OpenProcess(access,false,pid);
    if(h==IntPtr.Zero) { int error=Marshal.GetLastWin32Error(); if(error==87 || error==1168) return null; throw new Win32Exception(error); }
    try {
      if(Exited(h)) return null;
      uint length=32768; var path=new StringBuilder((int)length);
      long created,exit,kernel,user; uint session;
      if(!QueryFullProcessImageName(h,0,path,ref length) || !GetProcessTimes(h,out created,out exit,out kernel,out user) || !ProcessIdToSessionId(pid,out session)) {
        int error=Marshal.GetLastWin32Error(); if(Exited(h)) return null; throw new Win32Exception(error);
      }
      var result=new CodexDesktopProcess { handle=h,ProcessId=pid,ExecutablePath=path.ToString(),CreationDate=DateTime.FromFileTimeUtc(created),SessionId=session };
      h=IntPtr.Zero; return result;
    } finally { if(h!=IntPtr.Zero) CloseHandle(h); }
  }
  public static CodexDesktopProcess Inspect(uint pid) { return Read(pid,0x1000); }
  public static CodexDesktopProcess Bind(uint pid, string path, long created, bool forStop) {
    // Request only query/terminate/synchronize, not PROCESS_ALL_ACCESS. The
    // retained handle pins identity even if a PID is recycled after exit.
    var process=Read(pid,forStop ? 0x101001u : 0x101000u); if(process==null) return null;
    try {
      if(!String.Equals(process.ExecutablePath,path,StringComparison.OrdinalIgnoreCase) || Math.Abs(process.CreationDate.Ticks-created)>=10) return null;
      if(process.SessionId!=Process.GetCurrentProcess().SessionId) throw new InvalidOperationException("The desktop belongs to another Windows session.");
      IntPtr token;
      if(!OpenProcessToken(process.handle,8,out token)) throw new Win32Exception(Marshal.GetLastWin32Error());
      try {
        using(var owner=new WindowsIdentity(token)) using(var current=WindowsIdentity.GetCurrent()) {
          if(!owner.User.Equals(current.User)) throw new InvalidOperationException("The desktop belongs to another Windows user.");
        }
      } finally { CloseHandle(token); }
      var result=process; process=null; return result;
    } finally { if(process!=null) process.Dispose(); }
  }
  public void CloseMainWindow() { if(!HasExited) using(var process=Process.GetProcessById((int)ProcessId)) process.CloseMainWindow(); }
  public void Kill() { if(!HasExited && !TerminateProcess(handle,1) && !HasExited) throw new Win32Exception(Marshal.GetLastWin32Error()); }
  public void Dispose() { if(handle!=IntPtr.Zero) { CloseHandle(handle); handle=IntPtr.Zero; } }
}
'@ | Out-Null
}
function InspectDesktopProcess($processId) {
  EnsureProcessApi
  [CodexDesktopProcess]::Inspect([uint32]$processId)
}
function AppProcesses($exe, [bool]$ignoreInaccessible=$false) {
  $session=[Diagnostics.Process]::GetCurrentProcess().SessionId
  $processes=@(Get-CimInstance Win32_Process -Filter "Name='Codex.exe' OR Name='ChatGPT.exe'")
  foreach ($entry in $processes) {
    if ($entry.SessionId -ne $session) { continue }
    if ($entry.ExecutablePath -and -not (SamePath $entry.ExecutablePath $exe)) { continue }
    # CIM can retain an exiting process with a null path. Query the live PID
    # with limited rights; never interpret missing CIM metadata as a new app.
    $live=$null
    try { $live=InspectDesktopProcess $entry.ProcessId }
    catch {
      if ($ignoreInaccessible) { continue }
      throw ('CODEXX_PROCESS_ACCESS_REQUIRED: PID '+[string]$entry.ProcessId)
    }
    if ($null -eq $live) { continue }
    try {
      if ($live.SessionId -ne $session -or -not (SamePath $live.ExecutablePath $exe)) { continue }
      if ($null -ne $entry.CreationDate -and [Math]::Abs($live.CreationDate.Ticks-[long](ProcessStamp $entry)) -ge 10) { continue }
      [pscustomobject]@{ProcessId=$entry.ProcessId;ExecutablePath=$live.ExecutablePath;
        CreationDate=$live.CreationDate;SessionId=$live.SessionId;
        CommandLine=$entry.CommandLine;ParentProcessId=$entry.ParentProcessId}
    } finally { $live.Dispose() }
  }
}
function SelectMainProcesses($entries, [bool]$ignoreInaccessible=$false) {
  foreach ($entry in @($entries)) {
    if ($null -eq $entry.CommandLine) {
      # A sandboxed Electron helper can hide its command line. An observed
      # parent in the same verified installation identifies it as a child.
      if (@($entries | Where-Object { $_.ProcessId -eq $entry.ParentProcessId -and $_.CreationDate -le $entry.CreationDate }).Count -gt 0) { continue }
      if ($ignoreInaccessible) { continue }
      throw 'Cannot verify the desktop main process command line.'
    }
    if ($entry.CommandLine -notmatch '(?i)(?:^|\s)--type(?:=|\s)' -and
        $entry.CommandLine -notmatch '(?i)(?:^|\s)--crashpad-handler(?:\s|$)') { $entry }
  }
}
function MainProcesses($exe, [bool]$ignoreInaccessible=$false) {
  SelectMainProcesses @(AppProcesses $exe $ignoreInaccessible) $ignoreInaccessible
}
function BoundProcess($entry, $exe, [bool]$forStop=$false) {
  EnsureProcessApi
  [CodexDesktopProcess]::Bind([uint32]$entry.ProcessId,(FullPath $exe),[long](ProcessStamp $entry),$forStop)
}
function MainIdentity($app) {
  $entries=@(MainProcesses $app.executable $true)
  if ($entries.Count -gt 1) { throw 'Multiple verified desktop main processes are running.' }
  if ($entries.Count -eq 0) { return $null }
  $process=BoundProcess $entries[0] $app.executable
  if ($null -eq $process) { return $null }
  try { @{pid=[uint32]$entries[0].ProcessId;created=(ProcessStamp $entries[0])} }
  finally { $process.Dispose() }
}
function IsDesktopExe($exe) {
  $leaf=[IO.Path]::GetFileName([string]$exe)
  ($leaf -ieq 'Codex.exe' -or $leaf -ieq 'ChatGPT.exe') -and
    (Test-Path -LiteralPath $exe -PathType Leaf) -and
    (Test-Path -LiteralPath (Join-Path ([IO.Path]::GetDirectoryName($exe)) 'resources\app.asar') -PathType Leaf)
}
function TrustedStandalone($exe) {
  if (-not (IsDesktopExe $exe)) { return $false }
  $signature=Get-AuthenticodeSignature -LiteralPath $exe
  if ($signature.Status -ne 'Valid' -or $null -eq $signature.SignerCertificate) { return $false }
  $publisher=$signature.SignerCertificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName,$false)
  @('OpenAI OpCo, LLC','OpenAI, L.L.C.') -ccontains $publisher
}
function TrustedPackage($package) {
  $null -ne $package -and
    @('OpenAI.Codex','OpenAI.CodexBeta','OpenAI.ChatGPT-Desktop') -ccontains [string]$package.Name -and
    [string]$package.PublisherId -ceq '2p2nqsd0c76g0' -and
    [string]$package.PackageFamilyName -ceq ($package.Name+'_2p2nqsd0c76g0') -and
    [string]$package.Status -eq 'Ok' -and [string]$package.SignatureKind -eq 'Store' -and
    -not $package.IsDevelopmentMode
}
function PackageApp($package) {
  $root=FullPath $package.InstallLocation
  $manifest=Get-AppxPackageManifest -Package $package.PackageFullName
  foreach ($entry in @($manifest.Package.Applications.Application)) {
    if (-not $entry.Executable -or -not $entry.Id) { continue }
    $exe=FullPath (Join-Path $root ([string]$entry.Executable))
    if (-not $exe.StartsWith($root+'\',[StringComparison]::OrdinalIgnoreCase)) { continue }
    if (-not (IsDesktopExe $exe)) { continue }
    return [pscustomobject]@{path=$root;executable=$exe;bundleId=($package.PackageFamilyName+'!'+$entry.Id);
      name=$(if($package.Name -eq 'OpenAI.ChatGPT-Desktop'){'ChatGPT'}else{'Codex'});version=[string]$package.Version}
  }
  return $null
}
function IsElevated {
  $principal=[Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
  $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}
function ValidateApp($app, [bool]$allowElevated=$false) {
  if (-not (IsDesktopExe $app.executable)) { throw 'The selected path is not the official Electron desktop application.' }
  if ([string]$app.bundleId -eq 'OpenAI.Codex.Standalone') {
    if (-not (SamePath ([IO.Path]::GetDirectoryName($app.executable)) $app.path) -or
        -not (TrustedStandalone $app.executable)) { throw 'The desktop executable has no valid OpenAI publisher signature.' }
    return
  }
  if (-not $allowElevated -and (IsElevated)) {
    throw 'CODEXX_STORE_ELEVATION_REQUIRED'
  }
  $family=([string]$app.bundleId).Split('!')[0]
  $package=@(Get-AppxPackage | Where-Object { [string]$_.PackageFamilyName -ceq $family }) | Select-Object -First 1
  if (-not (TrustedPackage $package)) { throw 'The official signed Microsoft Store package is no longer registered.' }
  $registered=PackageApp $package
  if ($null -eq $registered -or $registered.bundleId -cne $app.bundleId -or
      -not (SamePath $registered.path $app.path) -or
      -not (SamePath $registered.executable $app.executable)) { throw 'The registered Store application identity has changed.' }
}
function Emit($result) { ConvertTo-Json -InputObject $result -Depth 6 -Compress }
"#;

// Ask the actual desktop Explorer to activate the registered Store app. Creating
// a new Shell.Application object and launching directly can inherit elevation.
// Bind to the desktop view, and require its token to belong to this same user
// and session without elevation before any existing Codex process is closed.
const DESKTOP_SHELL: &str = r#"
function DesktopShell {
  Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Security.Principal;
public static class CodexDesktopShellIdentity {
  [DllImport("user32.dll")] static extern IntPtr GetShellWindow();
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool GetTokenInformation(IntPtr token, int kind, out uint value, uint size, out uint needed);
  [DllImport("advapi32.dll", EntryPoint="GetTokenInformation", SetLastError=true)] static extern bool GetTokenBuffer(IntPtr token, int kind, IntPtr value, uint size, out uint needed);
  [DllImport("advapi32.dll")] static extern IntPtr GetSidSubAuthorityCount(IntPtr sid);
  [DllImport("advapi32.dll")] static extern IntPtr GetSidSubAuthority(IntPtr sid, uint index);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
  public static IntPtr Verify() {
    IntPtr window=GetShellWindow(); uint pid;
    if (window==IntPtr.Zero || GetWindowThreadProcessId(window,out pid)==0 || pid==0)
      throw new InvalidOperationException("No interactive Windows desktop shell is available.");
    using (Process process=Process.GetProcessById((int)pid)) {
      if (process.SessionId!=Process.GetCurrentProcess().SessionId)
        throw new InvalidOperationException("The desktop shell belongs to another Windows session.");
      IntPtr token;
      if (!OpenProcessToken(process.Handle,8,out token)) throw new Win32Exception(Marshal.GetLastWin32Error());
      try {
        using (WindowsIdentity identity=new WindowsIdentity(token))
        using (WindowsIdentity current=WindowsIdentity.GetCurrent()) {
          if (!identity.User.Equals(current.User))
            throw new InvalidOperationException("The desktop shell belongs to another Windows user.");
        }
        uint elevated,needed;
        if (!GetTokenInformation(token,20,out elevated,4,out needed)) throw new Win32Exception(Marshal.GetLastWin32Error());
        if (elevated!=0) throw new InvalidOperationException("The desktop shell is elevated.");
        GetTokenBuffer(token,25,IntPtr.Zero,0,out needed);
        if (needed==0) throw new Win32Exception(Marshal.GetLastWin32Error());
        IntPtr label=Marshal.AllocHGlobal((int)needed);
        try {
          if (!GetTokenBuffer(token,25,label,needed,out needed)) throw new Win32Exception(Marshal.GetLastWin32Error());
          IntPtr sid=Marshal.ReadIntPtr(label);
          byte count=Marshal.ReadByte(GetSidSubAuthorityCount(sid));
          if (count==0) throw new InvalidOperationException("The desktop shell has no integrity level.");
          int integrity=Marshal.ReadInt32(GetSidSubAuthority(sid,(uint)(count-1)));
          if (integrity<0x2000 || integrity>=0x3000)
            throw new InvalidOperationException("The desktop shell does not have medium integrity.");
        } finally { Marshal.FreeHGlobal(label); }
      } finally { CloseHandle(token); }
    }
    return window;
  }
}
'@ | Out-Null
  $expected=[CodexDesktopShellIdentity]::Verify()
  $windows=(New-Object -ComObject Shell.Application).Windows()
  $handle=0
  $desktop=$windows.FindWindowSW(0,0,8,[ref]$handle,1)
  if ($null -eq $desktop -or ([long]$handle -band 0xffffffffL) -ne ($expected.ToInt64() -band 0xffffffffL)) {
    throw 'The interactive desktop shell could not be verified.'
  }
  $shell=$desktop.Document.Application
  if ($null -eq $shell) { throw 'The interactive desktop has no activation interface.' }
  if ([CodexDesktopShellIdentity]::Verify() -ne $expected) { throw 'The interactive desktop changed during activation preflight.' }
  return $shell
}
"#;

const STOP_DESKTOP: &str = r#"
function StopDesktop($app, [bool]$requireCurrentUser=$false) {
  $script:CodexCloseStarted=$false
  $entries=@(AppProcesses $app.executable)
  $main=@(SelectMainProcesses $entries)
  if ($main.Count -gt 1) { throw 'Multiple desktop instances are running; none was stopped.' }
  $bound=New-Object 'System.Collections.Generic.List[object]'
  try {
    # Validate every process and acquire identity-pinned handles before the
    # first close. Bind always checks current user/session, including non-admin
    # restarts; inaccessible selected processes cannot cause a partial stop.
    foreach ($entry in $entries) {
      $process=BoundProcess $entry $app.executable $true
      if ($null -ne $process) { $bound.Add($process) }
    }
    foreach ($process in $bound) {
      if ($process.ProcessId -in @($main | ForEach-Object { $_.ProcessId })) {
        try { $script:CodexCloseStarted=$true; $process.CloseMainWindow() } catch { if (-not $process.HasExited) { throw } }
      }
    }
    $grace=[DateTime]::UtcNow.AddSeconds(2)
    while ([DateTime]::UtcNow -lt $grace -and @($bound | Where-Object { -not $_.HasExited }).Count -gt 0) { Start-Sleep -Milliseconds 100 }
    foreach ($process in $bound) { if (-not $process.HasExited) { $script:CodexCloseStarted=$true; $process.Kill() } }
    $deadline=[DateTime]::UtcNow.AddSeconds(8)
    do {
      # Once closing starts, wait on pinned handles. Re-enumerating global CIM
      # rows here used to throw on stale, pathless rows after Codex had exited.
      if (@($bound | Where-Object { -not $_.HasExited }).Count -eq 0) { return }
      Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'The selected desktop processes did not stop; relaunch was cancelled.'
  } finally { foreach ($process in $bound) { $process.Dispose() } }
}

"#;

const START_WITH_DESKTOP_SHELL: &str = r#"
function StartWithDesktopShell($app,$shell) {
  $shell.ShellExecute(('shell:AppsFolder\'+[string]$app.bundleId),'','','open',1) | Out-Null
  $deadline=[DateTime]::UtcNow.AddSeconds(8)
  do {
    $main=@(MainProcesses $app.executable $true)
    if ($main.Count -gt 1) { throw 'Multiple desktop instances started; no debugging connection was made.' }
    if ($main.Count -eq 1) {
      $process=BoundProcess $main[0] $app.executable
      if ($null -ne $process) {
        try { return @{pid=[uint32]$main[0].ProcessId} }
        finally { $process.Dispose() }
      }
    }
    Start-Sleep -Milliseconds 100
  } while ([DateTime]::UtcNow -lt $deadline)
  throw 'The desktop shell accepted activation, but the verified Codex application did not start.'
}
"#;

const ELEVATED_STORE_RESTART: &str = r#"
if (-not (IsElevated)) { Emit @{handled=$false}; return }
$script:CodexCloseStarted=$false
$script:CodexStartAttempted=$false
try {
  ValidateApp $data.app $true
  # Acquire and validate the unelevated broker before stopping anything.
  try { $shell=DesktopShell } catch { throw ('CODEXX_STORE_BROKER_UNAVAILABLE: '+$_.Exception.Message) }
  $wasRunning=@(AppProcesses $data.app.executable).Count -gt 0
  StopDesktop $data.app $true
  $script:CodexStartAttempted=$true
  $started=StartWithDesktopShell $data.app $shell
  Emit @{handled=$true;ok=$true;wasRunning=$wasRunning;pid=$started.pid}
} catch {
  Emit @{handled=$true;ok=$false;afterAttempt=($script:CodexCloseStarted -or $script:CodexStartAttempted);error=$_.Exception.Message}
}
"#;

pub(crate) fn detect() -> Result<WindowsDesktopApp, String> {
    type Cached = (Instant, Result<WindowsDesktopApp, String>);
    static CACHE: OnceLock<Mutex<Option<Cached>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "Windows 客户端检测状态忙")?;
    if let Some((recorded, value)) = cache.as_ref() {
        let ttl = if value.is_ok() { 30 } else { 10 };
        if recorded.elapsed() < Duration::from_secs(ttl) {
            return value.clone();
        }
    }
    let value = detect_fresh();
    *cache = Some((Instant::now(), value.clone()));
    value
}

pub(crate) fn detect_fresh() -> Result<WindowsDesktopApp, String> {
    let result = powershell(
        r#"
$candidates=New-Object 'System.Collections.Generic.List[object]'
foreach ($package in @(Get-AppxPackage | Sort-Object -Property Version -Descending)) {
  if (-not (TrustedPackage $package)) { continue }
  try { $app=PackageApp $package } catch { continue }
  if ($null -ne $app) { $candidates.Add($app) }
}
if ($env:LOCALAPPDATA) {
  $roots=@('OpenAI\Codex\bin','OpenAI\Codex','Programs\OpenAI\Codex','Programs\Codex')
  $seen=@{}
  foreach ($relative in $roots) {
    $root=Join-Path $env:LOCALAPPDATA $relative
    $directories=@($root,(Join-Path $root 'app'))
    if (Test-Path -LiteralPath $root -PathType Container) {
      foreach ($child in @(Get-ChildItem -LiteralPath $root -Directory -ErrorAction SilentlyContinue | Select-Object -First 32)) {
        $directories+=@($child.FullName,(Join-Path $child.FullName 'app'))
      }
    }
    foreach ($directory in $directories) {
      foreach ($leaf in @('Codex.exe','ChatGPT.exe')) {
        $exe=Join-Path $directory $leaf
        $key=$exe.ToLowerInvariant()
        if ($seen.ContainsKey($key)) { continue }; $seen[$key]=$true
        if (-not (TrustedStandalone $exe)) { continue }
        $version=(Get-Item -LiteralPath $exe).VersionInfo.ProductVersion
        $candidates.Add([pscustomobject]@{path=(FullPath $directory);executable=(FullPath $exe);
          bundleId='OpenAI.Codex.Standalone';name='Codex';version=[string]$version})
      }
    }
  }
}
$running=@()
foreach ($app in $candidates) {
  $main=@(MainProcesses $app.executable)
  if ($main.Count -gt 1) { throw 'Multiple desktop instances are running. Keep one instance before restarting.' }
  if ($main.Count -eq 1) { $running+=,$app }
}
if ($running.Count -gt 1) { throw 'Multiple official desktop installations are running. Keep one before restarting.' }
if ($running.Count -eq 1) { Emit $running[0]; exit }
if ($candidates.Count -eq 0) { throw 'No verified OpenAI Codex Store package or signed standalone installation was found.' }
Emit $candidates[0]
"#,
        Value::Null,
        Duration::from_secs(25),
    )?;
    serde_json::from_value(result).map_err(|_| "Windows 客户端安装信息无效".into())
}

pub(crate) fn validate(app: &WindowsDesktopApp) -> Result<(), String> {
    powershell(
        "ValidateApp $data.app; Emit $true",
        json!({"app":app}),
        Duration::from_secs(15),
    )
    .map(|_| ())
}

pub(crate) fn validate_launch(app: &WindowsDesktopApp, home: &Path) -> Result<(), String> {
    if app.bundle_id != "OpenAI.Codex.Standalone" {
        let default = dirs::home_dir()
            .ok_or("无法确认 Windows 默认 Codex 配置目录")?
            .join(".codex");
        let requested = crate::paths::normalized_path_scope(home);
        let expected = crate::paths::normalized_path_scope(&default);
        if requested != expected {
            return Err("Windows Store 版无法通过安全激活传入自定义 CODEX_HOME；请使用默认 .codex 目录或官方独立安装版。客户端尚未退出。".into());
        }
    }
    Ok(())
}

pub(crate) fn pids(app: &WindowsDesktopApp) -> Result<Vec<u32>, String> {
    let value = powershell(
        "Emit @(AppProcesses $data.app.executable | ForEach-Object { [uint32]$_.ProcessId })",
        json!({"app":app}),
        Duration::from_secs(8),
    )?;
    serde_json::from_value(value).map_err(|_| "无法确认 Windows 客户端进程".into())
}

struct StopFailure {
    // false: no close/activation was attempted; true: either was attempted;
    // None: the command outcome is unknown (for example a runner timeout).
    after_attempt: Option<bool>,
    message: String,
}

fn stop_desktop(app: &WindowsDesktopApp) -> Result<(), StopFailure> {
    let result = powershell(
        &format!("{STOP_DESKTOP}\nValidateApp $data.app; try {{ StopDesktop $data.app; Emit @{{ok=$true}} }} catch {{ Emit @{{ok=$false;afterAttempt=$script:CodexCloseStarted;error=$_.Exception.Message}} }}"),
        json!({"app":app}),
        Duration::from_secs(20),
    ).map_err(|message| StopFailure { after_attempt: None, message })?;
    if result["ok"].as_bool() == Some(true) {
        return Ok(());
    }
    Err(StopFailure {
        after_attempt: result["afterAttempt"].as_bool(),
        message: operation_error(
            result["error"]
                .as_str()
                .unwrap_or("无法确认 Windows 客户端退出状态"),
        ),
    })
}

pub(crate) fn quit(app: &WindowsDesktopApp) -> Result<(), String> {
    stop_desktop(app).map_err(|error| error.message)
}

fn existing_main(app: &WindowsDesktopApp) -> Result<Option<u32>, String> {
    let value = powershell(
        "Emit (MainIdentity $data.app)",
        json!({"app":app}),
        Duration::from_secs(10),
    )?;
    if value.is_null() {
        return Ok(None);
    }
    remember_started(value).map(Some)
}

pub(crate) fn launch(
    app: &WindowsDesktopApp,
    home: Option<&Path>,
    port: Option<u16>,
) -> Result<u32, String> {
    if let Some(home) = home {
        validate_launch(app, home)?;
    }
    if app.bundle_id == "OpenAI.Codex.Standalone" {
        return launch_standalone(app, home, port);
    }
    let result = powershell(
        r#"
ValidateApp $data.app
if ($null -eq $data.port) {
  $existing=MainIdentity $data.app
  if ($null -ne $existing) { Emit $existing; return }
} elseif (@(AppProcesses $data.app.executable).Count -gt 0) { throw 'The desktop application is still stopping; no duplicate instance was launched.' }
$arguments=''
if ($null -ne $data.port) { $arguments='--remote-debugging-port='+[string]$data.port+' --remote-debugging-address=127.0.0.1' }
  Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
[ComImport, Guid("2e941141-7f97-4756-ba1d-9decde894a3d"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface ICodexActivationManager {
  [PreserveSig] int ActivateApplication([MarshalAs(UnmanagedType.LPWStr)] string appId,
    [MarshalAs(UnmanagedType.LPWStr)] string arguments, uint options, out uint processId);
}
public static class CodexPackageActivation {
  public static uint Start(string appId, string arguments) {
    var manager=(ICodexActivationManager)Activator.CreateInstance(Type.GetTypeFromCLSID(new Guid("45ba127d-10a8-46ea-8ab7-56ea9078943c")));
    try { uint processId; int result=manager.ActivateApplication(appId, arguments, 0, out processId);
      Marshal.ThrowExceptionForHR(result); return processId; }
    finally { Marshal.FinalReleaseComObject(manager); }
  }
}
'@ | Out-Null
  $started=[CodexPackageActivation]::Start([string]$data.app.bundleId,$arguments)
if ($started -eq 0) { throw 'Windows did not return an activated process id.' }
$created=''; $deadline=[DateTime]::UtcNow.AddSeconds(8)
do {
  $process=$null
  try { $process=InspectDesktopProcess $started } catch { }
  if ($null -ne $process) {
    try {
      if (-not (SamePath $process.ExecutablePath $data.app.executable)) { throw 'Windows activated a different executable; no debugging connection was made.' }
      $confirmed=BoundProcess $process $data.app.executable
      if ($null -ne $confirmed) {
        try { $created=ProcessStamp $process; break } finally { $confirmed.Dispose() }
      }
    } finally { $process.Dispose() }
  }
  Start-Sleep -Milliseconds 100
} while ([DateTime]::UtcNow -lt $deadline)
if (-not $created) { throw 'Windows accepted activation, but its process identity could not be confirmed. The launched application was not terminated.' }
Emit @{pid=$started;created=$created}
"#,
        json!({"app":app,"home":home,"port":port}),
        Duration::from_secs(20),
    )?;
    remember_started(result)
}

fn remember_started(result: Value) -> Result<u32, String> {
    let pid = result["pid"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .filter(|pid| *pid != 0)
        .ok_or("Windows 未确认已启动的客户端进程")?;
    let created = result["created"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or("Windows 未确认客户端进程身份")?;
    let mut tracked = stamps().lock().map_err(|_| "Windows 进程状态忙")?;
    if tracked.len() >= 16 {
        tracked.clear();
    }
    tracked.insert(pid, created.to_owned());
    Ok(pid)
}

fn launch_standalone(
    app: &WindowsDesktopApp,
    home: Option<&Path>,
    port: Option<u16>,
) -> Result<u32, String> {
    let prepared = powershell(
        r#"
ValidateApp $data.app
if ($null -eq $data.port) {
  $existing=MainIdentity $data.app
  if ($null -ne $existing) { Emit $existing; return }
} elseif (@(AppProcesses $data.app.executable).Count -gt 0) { throw 'The desktop is still stopping; no duplicate was launched.' }
Emit $null
"#,
        json!({"app":app,"port":port}),
        Duration::from_secs(15),
    )?;
    if !prepared.is_null() {
        return remember_started(prepared);
    }
    let arguments = port
        .map(|port| {
            vec![
                format!("--remote-debugging-port={port}"),
                "--remote-debugging-address=127.0.0.1".to_owned(),
            ]
        })
        .unwrap_or_default();
    let args: Vec<_> = arguments.iter().map(String::as_str).collect();
    let mut command = crate::platform::program_command(&app.executable, &args);
    command
        .current_dir(&app.path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(home) = home {
        command.env("CODEX_HOME", home);
    }
    let mut child = command
        .spawn()
        .map_err(|_| "无法启动已验证的 Windows Codex 客户端")?;
    let pid = child.id();
    let result = powershell(
        r#"
$deadline=[DateTime]::UtcNow.AddSeconds(8)
do {
  $process=$null
  try { $process=InspectDesktopProcess ([uint32]$data.pid) } catch { }
  if ($null -ne $process) {
    try {
      if (-not (SamePath $process.ExecutablePath $data.app.executable)) { throw 'The newly started process has an unexpected executable.' }
      $confirmed=BoundProcess $process $data.app.executable
      if ($null -ne $confirmed) {
        try { Emit @{pid=[uint32]$data.pid;created=(ProcessStamp $process)}; return } finally { $confirmed.Dispose() }
      }
    } finally { $process.Dispose() }
  }
  Start-Sleep -Milliseconds 100
} while ([DateTime]::UtcNow -lt $deadline)
if ($null -eq $data.port) {
  $existing=MainIdentity $data.app
  if ($null -ne $existing) { Emit $existing; return }
}
throw 'The desktop was launched, but its process identity could not be confirmed.'

"#,
        json!({"app":app,"pid":pid,"port":port}),
        Duration::from_secs(15),
    ).and_then(remember_started);
    if result.is_err() && port.is_some() {
        // Only a failed controlled debugging launch is cleaned up. An ordinary
        // launch must not kill a healthy app because observation was delayed.
        // Child owns the original Windows process handle, so cleanup cannot
        // terminate an unrelated process after a recycled PID.
        let _ = child.kill();
        let _ = child.try_wait();
    }
    // Windows has no zombie-process wait requirement. Dropping Child releases
    // our process/thread handles while the GUI keeps its own lifetime.
    drop(child);
    result
}

pub(crate) fn verify_process(app: &WindowsDesktopApp, pid: u32) -> Result<(), String> {
    let created = stamps()
        .lock()
        .map_err(|_| "Windows 进程状态忙")?
        .get(&pid)
        .cloned()
        .ok_or("该客户端进程不属于本次受控启动")?;
    powershell(
        r#"
$process=Get-CimInstance Win32_Process -Filter ('ProcessId='+[uint32]$data.pid)
if ($null -eq $process -or -not $process.ExecutablePath -or
    -not (SamePath $process.ExecutablePath $data.app.executable) -or
    (ProcessStamp $process) -cne [string]$data.created) { throw 'The activated desktop process identity has changed.' }
Emit $true
"#,
        json!({"app":app,"pid":pid,"created":created}),
        Duration::from_secs(8),
    )
    .map(|_| ())
}

pub(crate) fn verify_listener(app: &WindowsDesktopApp, pid: u32, port: u16) -> Result<(), String> {
    let created = stamps()
        .lock()
        .map_err(|_| "Windows 进程状态忙")?
        .get(&pid)
        .cloned()
        .ok_or("该客户端进程不属于本次受控启动")?;
    powershell(
        r#"
$process=Get-CimInstance Win32_Process -Filter ('ProcessId='+[uint32]$data.pid)
if ($null -eq $process -or -not $process.ExecutablePath -or
    -not (SamePath $process.ExecutablePath $data.app.executable) -or
    (ProcessStamp $process) -cne [string]$data.created) { throw 'The activated desktop process identity has changed.' }
$listeners=@(Get-NetTCPConnection -State Listen -LocalPort ([uint16]$data.port) -ErrorAction SilentlyContinue)
if ($listeners.Count -eq 0) { throw 'The official desktop has not opened its debugging port.' }
foreach ($listener in $listeners) {
  if ($listener.LocalAddress -cne '127.0.0.1' -or [uint32]$listener.OwningProcess -ne [uint32]$data.pid) {
    throw 'The debugging port is not exclusively owned by the activated process on 127.0.0.1.'
  }
}
$after=Get-CimInstance Win32_Process -Filter ('ProcessId='+[uint32]$data.pid)
if ($null -eq $after -or -not $after.ExecutablePath -or
    -not (SamePath $after.ExecutablePath $data.app.executable) -or
    (ProcessStamp $after) -cne [string]$data.created) { throw 'The desktop process changed while its port was being verified.' }
Emit $true
"#,
        json!({"app":app,"pid":pid,"port":port,"created":created}),
        Duration::from_secs(8),
    )
    .map(|_| ())
}

fn restore_store(app: &WindowsDesktopApp) -> Result<u32, String> {
    // Recovery never closes anything. Revalidate the ordinary desktop broker
    // and activate only after confirming that the selected installation has
    // no remaining process. Do not fall back to an elevated direct launch.
    let result = powershell(
        &format!(
            r#"{DESKTOP_SHELL}
{START_WITH_DESKTOP_SHELL}
ValidateApp $data.app $true
$existing=MainIdentity $data.app
if ($null -ne $existing) {{ Emit $existing; return }}
if (@(AppProcesses $data.app.executable).Count -gt 0) {{ throw 'Client processes remain; recovery did not create a duplicate instance.' }}
$shell=DesktopShell
Emit (StartWithDesktopShell $data.app $shell)
"#
        ),
        json!({"app":app}),
        Duration::from_secs(25),
    )?;
    result["pid"]
        .as_u64()
        .and_then(|id| u32::try_from(id).ok())
        .filter(|id| *id != 0)
        .ok_or("Windows 未确认恢复启动的客户端进程".into())
}

fn recover_restart(
    original_pids: &[u32],
    failure: StopFailure,
    mut current_main: impl FnMut() -> Result<Option<u32>, String>,
    mut no_processes: impl FnMut() -> Result<bool, String>,
    mut start_once: impl FnMut() -> Result<u32, String>,
) -> Result<(), String> {
    if failure.after_attempt == Some(false) {
        return Err(format!("重启准备失败，客户端未被关闭：{}", failure.message));
    }
    match current_main() {
        Ok(Some(pid)) if !original_pids.contains(&pid) => return Ok(()),
        Ok(Some(_)) => return Err(format!("重启未完成，原客户端仍在运行：{}", failure.message)),
        Err(observation) => {
            return Err(format!(
                "无法确认重启后的客户端状态，未重复启动。{}；{observation}",
                failure.message
            ))
        }
        Ok(None) => {}
    }
    match no_processes() {
        Ok(true) => {}
        Ok(false) => {
            return Err(format!(
                "客户端仍有后台进程，未重复启动。{}",
                failure.message
            ))
        }
        Err(observation) => {
            return Err(format!(
                "无法确认客户端是否已退出，未重复启动。{}；{observation}",
                failure.message
            ))
        }
    }
    match start_once() {
        Ok(_) => Ok(()),
        Err(recovery) => {
            // A timed-out observation can follow a successful activation.
            // Check once, but never issue a second recovery launch.
            if let Ok(Some(pid)) = current_main() {
                if !original_pids.contains(&pid) {
                    return Ok(());
                }
            }
            Err(format!(
                "客户端重新打开未完成。{}；恢复启动：{recovery}",
                failure.message
            ))
        }
    }
}

pub(crate) fn restart(home: &Path) -> Result<(String, bool), String> {
    let app = detect_fresh()?;
    validate_launch(&app, home)?;
    let original_pids = pids(&app)?;
    let was_running = !original_pids.is_empty();
    if app.bundle_id != "OpenAI.Codex.Standalone" {
        let result = powershell(
            &format!("{DESKTOP_SHELL}\n{STOP_DESKTOP}\n{START_WITH_DESKTOP_SHELL}\n{ELEVATED_STORE_RESTART}"),
            json!({"app":app}),
            Duration::from_secs(35),
        );
        match result {
            Ok(value) if value["handled"].as_bool() == Some(false) => {}
            Ok(value) if value["ok"].as_bool() == Some(true) => return Ok((app.name, was_running)),
            other => {
                let failure = match other {
                    Ok(value) => StopFailure {
                        after_attempt: value["afterAttempt"].as_bool(),
                        message: operation_error(
                            value["error"].as_str().unwrap_or("未收到客户端重启结果"),
                        ),
                    },
                    Err(message) => StopFailure {
                        after_attempt: None,
                        message,
                    },
                };
                recover_restart(
                    &original_pids,
                    failure,
                    || existing_main(&app),
                    || pids(&app).map(|ids| ids.is_empty()),
                    || restore_store(&app),
                )?;
                return Ok((app.name, was_running));
            }
        }
    }
    validate(&app)?;
    let failure = match stop_desktop(&app) {
        Ok(()) => match launch(&app, Some(home), None) {
            Ok(_) => return Ok((app.name, was_running)),
            Err(message) => StopFailure {
                after_attempt: Some(true),
                message,
            },
        },
        Err(error) => error,
    };
    recover_restart(
        &original_pids,
        failure,
        || existing_main(&app),
        || pids(&app).map(|ids| ids.is_empty()),
        || launch(&app, Some(home), None),
    )?;
    Ok((app.name, was_running))
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use std::cell::Cell;

    fn failure(after_attempt: Option<bool>) -> StopFailure {
        StopFailure {
            after_attempt,
            message: "fixture failure".into(),
        }
    }

    #[test]
    fn preflight_failure_never_starts_or_claims_a_client_was_closed() {
        let result = recover_restart(
            &[10],
            failure(Some(false)),
            || panic!("no observation required"),
            || panic!("no observation required"),
            || panic!("must not launch"),
        );
        assert!(result.unwrap_err().contains("客户端未被关闭"));
    }

    #[test]
    fn timed_out_runner_recovers_once_when_fresh_observation_confirms_exit() {
        let calls = Cell::new(0);
        let result = recover_restart(
            &[10],
            failure(None),
            || Ok(None),
            || Ok(true),
            || {
                calls.set(calls.get() + 1);
                Ok(20)
            },
        );
        assert!(result.is_ok());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn timed_out_runner_preserves_existing_original_main_without_relaunch() {
        let result = recover_restart(
            &[10],
            failure(None),
            || Ok(Some(10)),
            || panic!("main already observed"),
            || panic!("must not relaunch"),
        );
        assert!(result.unwrap_err().contains("原客户端仍在运行"));
    }

    #[test]
    fn already_relaunched_verified_main_is_accepted_without_a_second_start() {
        assert!(recover_restart(
            &[10],
            failure(Some(true)),
            || Ok(Some(20)),
            || panic!("main already observed"),
            || panic!("must not relaunch")
        )
        .is_ok());
    }

    #[test]
    fn unverified_remaining_processes_prevent_blind_recovery_launch() {
        let result = recover_restart(
            &[10],
            failure(None),
            || Ok(None),
            || Err("identity unavailable".into()),
            || panic!("must not relaunch"),
        );
        assert!(result.unwrap_err().contains("无法确认客户端是否已退出"));
    }

    #[test]
    fn failed_recovery_is_not_retried_in_a_loop() {
        let calls = Cell::new(0);
        let result = recover_restart(
            &[10],
            failure(Some(true)),
            || Ok(None),
            || Ok(true),
            || {
                calls.set(calls.get() + 1);
                Err("activation denied".into())
            },
        );
        assert!(result.unwrap_err().contains("重新打开未完成"));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn recovery_observation_timeout_does_not_kill_a_successfully_started_main() {
        let observations = Cell::new(0);
        let calls = Cell::new(0);
        let result = recover_restart(
            &[10],
            failure(Some(true)),
            || {
                observations.set(observations.get() + 1);
                Ok(if observations.get() == 1 {
                    None
                } else {
                    Some(20)
                })
            },
            || Ok(true),
            || {
                calls.set(calls.get() + 1);
                Err("observation timeout".into())
            },
        );
        assert!(result.is_ok());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn delayed_store_start_with_no_original_process_accepts_observed_main() {
        assert!(recover_restart(
            &[],
            failure(Some(true)),
            || Ok(Some(20)),
            || panic!("already started"),
            || panic!("must not duplicate activation")
        )
        .is_ok());
    }

    #[test]
    fn access_errors_are_normalized_for_both_structured_and_stderr_failures() {
        let message = operation_error("CODEXX_PROCESS_ACCESS_REQUIRED: PID 123");
        assert!(message.contains("相同用户和权限"));
        assert!(!message.contains("CODEXX_PROCESS_ACCESS_REQUIRED"));
        assert!(!message.contains("未被关闭"));
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    const PROCESS_FIXTURES: &str = r#"
$global:fixtureSession=[Diagnostics.Process]::GetCurrentProcess().SessionId
$global:fixtureCreated=[DateTime]::UtcNow
$global:queries=0; $global:closed=$false; $global:boundDisposed=0
function Entry($id,$path,$command='Codex.exe',$parent=0) {
  [pscustomobject]@{ProcessId=$id;ExecutablePath=$path;CommandLine=$command;
    ParentProcessId=$parent;CreationDate=$global:fixtureCreated;SessionId=$global:fixtureSession}
}
function Live($id,$path='C:\fixture\Codex.exe') {
  $value=Entry $id $path
  $value | Add-Member -MemberType ScriptMethod -Name Dispose -Value {}
  $value
}
function Get-CimInstance { $global:queries++; $global:rows }
function InspectDesktopProcess($processId) { Live $processId }
function BoundProcess($entry,$exe,$forStop) {
  if (-not $forStop) { throw 'Fixture stop must request only its required control rights.' }
  $value=[pscustomobject]@{ProcessId=$entry.ProcessId;HasExited=$false}
  $value | Add-Member -MemberType ScriptMethod -Name CloseMainWindow -Value { $global:closed=$true; $this.HasExited=$true }
  $value | Add-Member -MemberType ScriptMethod -Name Kill -Value { $this.HasExited=$true }
  $value | Add-Member -MemberType ScriptMethod -Name Dispose -Value { $global:boundDisposed++ }
  $value
}
"#;

    fn process_fixture(script: &str, stop: bool) -> Value {
        powershell(
            &format!(
                "{}\n{PROCESS_FIXTURES}\n{script}",
                if stop { STOP_DESKTOP } else { "" }
            ),
            json!({"app":{"executable":"C:\\fixture\\Codex.exe"}}),
            Duration::from_secs(15),
        )
        .unwrap()
    }

    #[test]
    fn cim_missing_path_uses_live_identity_and_ignores_exited_foreign_and_recycled_pids() {
        let value = process_fixture(
            r#"
$foreign=Entry 103 $null; $foreign.SessionId=$global:fixtureSession+1
$global:rows=@((Entry 101 $null),(Entry 102 $null),$foreign,(Entry 104 'C:\other\Codex.exe'),(Entry 105 $null),(Entry 106 $null))
$global:probed=New-Object 'System.Collections.Generic.List[int]'
function InspectDesktopProcess($processId) {
  $global:probed.Add($processId)
  if ($processId -eq 102) { return $null }
  if ($processId -eq 105) { return (Live $processId 'C:\other\Codex.exe') }
  $value=Live $processId
  if ($processId -eq 106) { $value.CreationDate=$value.CreationDate.AddSeconds(1) }
  $value
}
Emit @{ids=@(AppProcesses $data.app.executable | ForEach-Object { $_.ProcessId });probed=$global:probed.ToArray()}
"#,
            false,
        );
        assert_eq!(value["ids"], json!([101]));
        assert_eq!(value["probed"], json!([101, 102, 105, 106]));
    }

    #[test]
    fn inaccessible_live_process_aborts_before_any_client_is_closed() {
        let value = process_fixture(
            r#"
$global:rows=@(Entry 101 $null)
function InspectDesktopProcess { throw 'fixture: access denied' }
try { StopDesktop $data.app; throw 'unexpected success' }
catch { Emit @{error=$_.Exception.Message;closed=$script:CodexCloseStarted} }
"#,
            true,
        );
        assert_eq!(value["closed"], false);
        assert!(value["error"]
            .as_str()
            .unwrap()
            .contains("CODEXX_PROCESS_ACCESS_REQUIRED"));
    }

    #[test]
    fn stop_waits_on_bound_handles_instead_of_pathless_cim_rows_after_close() {
        let value = process_fixture(
            r#"
function Get-CimInstance {
  $global:queries++
  if ($global:closed) { return (Entry 101 $null) }
  Entry 101 'C:\fixture\Codex.exe'
}
function InspectDesktopProcess($processId) {
  if ($global:closed) { throw 'fixture: stale CIM process has lost its path' }
  Live $processId
}
StopDesktop $data.app
Emit @{closed=$global:closed;queries=$global:queries;disposed=$global:boundDisposed}
"#,
            true,
        );
        assert_eq!(value["closed"], true);
        assert_eq!(value["queries"], 1);
        assert_eq!(value["disposed"], 1);
    }

    #[test]
    fn stop_binds_all_processes_before_closing_and_releases_handles_on_preflight_failure() {
        let value = process_fixture(
            r#"
$global:rows=@((Entry 101 'C:\fixture\Codex.exe'),(Entry 102 'C:\fixture\Codex.exe' 'Codex.exe --type=renderer' 101))
$originalBound=${function:BoundProcess}
function BoundProcess($entry,$exe,$forStop) {
  if ($entry.ProcessId -eq 102) { throw 'fixture: selected helper cannot be controlled' }
  & $originalBound $entry $exe $forStop
}
try { StopDesktop $data.app; throw 'unexpected success' }
catch { Emit @{error=$_.Exception.Message;closed=$global:closed;disposed=$global:boundDisposed} }
"#,
            true,
        );
        assert_eq!(value["closed"], false);
        assert_eq!(value["disposed"], 1);
        assert_eq!(
            value["error"],
            "fixture: selected helper cannot be controlled"
        );
    }

    #[test]
    fn native_process_inspection_and_read_only_binding_check_path_and_creation_time() {
        let value = powershell(r#"
$live=InspectDesktopProcess $PID
try {
  EnsureProcessApi
  $bound=[CodexDesktopProcess]::Bind([uint32]$PID,$live.ExecutablePath,$live.CreationDate.Ticks,$false)
  try {
    $wrongPath=[CodexDesktopProcess]::Bind([uint32]$PID,($live.ExecutablePath+'.wrong'),$live.CreationDate.Ticks,$false)
    $wrongTime=[CodexDesktopProcess]::Bind([uint32]$PID,$live.ExecutablePath,($live.CreationDate.Ticks+10000),$false)
    Emit @{bound=($null -ne $bound -and -not $bound.HasExited);wrongPath=($null -eq $wrongPath);wrongTime=($null -eq $wrongTime)}
  } finally { if ($null -ne $bound) { $bound.Dispose() } }
} finally { $live.Dispose() }
"#, Value::Null, Duration::from_secs(15)).unwrap();
        assert_eq!(
            value,
            json!({"bound":true,"wrongPath":true,"wrongTime":true})
        );
    }

    fn restart_fixture(before: &str, catch: bool) -> Value {
        let flow = if catch {
            format!(
                "try {{ & {{ {ELEVATED_STORE_RESTART} }} }} catch {{ Emit @{{error=$_.Exception.Message;stopped=$global:stopped}} }}"
            )
        } else {
            ELEVATED_STORE_RESTART.to_owned()
        };
        powershell(
            &format!("{START_WITH_DESKTOP_SHELL}\n{before}\n{flow}"),
            json!({"app":{"executable":"fixture.exe","bundleId":"OpenAI.Codex_2p2nqsd0c76g0!App"}}),
            Duration::from_secs(15),
        )
        .unwrap()
    }

    #[test]
    fn elevated_store_restart_keeps_client_running_when_broker_preflight_fails() {
        let result = restart_fixture(
            r#"
$global:stopped=$false
function IsElevated { $true }
function ValidateApp($app, $allowElevated) { if (-not $allowElevated) { throw 'wrong validation mode' } }
function DesktopShell { throw 'fixture: no same-user unelevated shell' }
function StopDesktop($app) { $global:stopped=$true }
"#,
            true,
        );
        assert_eq!(result["afterAttempt"], false);
        assert_eq!(result["ok"], false);
        assert_eq!(
            result["error"],
            "CODEXX_STORE_BROKER_UNAVAILABLE: fixture: no same-user unelevated shell"
        );
    }

    #[test]
    fn elevated_store_restart_activates_verified_identity_after_preflight_and_stop() {
        let result = restart_fixture(
            r#"
$global:stopped=$false; $global:brokered=$false
function IsElevated { $true }
function ValidateApp($app, $allowElevated) { if (-not $allowElevated) { throw 'wrong validation mode' } }
function DesktopShell {
  $global:brokered=$true
  $shell=[pscustomobject]@{}
  $shell | Add-Member -MemberType ScriptMethod -Name ShellExecute -Value {
    param($target,$arguments,$directory,$verb,$show)
    if (-not $global:stopped -or $target -cne 'shell:AppsFolder\OpenAI.Codex_2p2nqsd0c76g0!App' -or
        $arguments -cne '' -or $directory -cne '' -or $verb -cne 'open' -or $show -ne 1) { throw 'unexpected broker activation' }
  }
  $shell
}
function AppProcesses($exe) { [pscustomobject]@{ProcessId=123} }
function StopDesktop($app, $requireCurrentUser) {
  if (-not $global:brokered) { throw 'stop before broker preflight' }
  if (-not $requireCurrentUser) { throw 'missing process owner validation' }
  $global:stopped=$true
}
function MainProcesses($exe) { [pscustomobject]@{ProcessId=4242} }
function BoundProcess($entry,$exe) {
  if ($exe -cne 'fixture.exe' -or $entry.ProcessId -ne 4242) { throw 'unexpected process identity' }
  $process=[pscustomobject]@{}
  $process | Add-Member -MemberType ScriptMethod -Name Dispose -Value {}
  $process
}
"#,
            false,
        );
        assert_eq!(result["handled"], true);
        assert_eq!(result["wasRunning"], true);
        assert_eq!(result["pid"], 4242);
    }

    #[test]
    fn elevated_store_activation_failure_reports_that_close_already_started() {
        let result = restart_fixture(
            r#"
function IsElevated { $true }
function ValidateApp { }
function DesktopShell { [pscustomobject]@{} }
function AppProcesses { [pscustomobject]@{ProcessId=123} }
function StopDesktop { $script:CodexCloseStarted=$true }
function StartWithDesktopShell { throw 'fixture activation failed after stop' }
"#,
            false,
        );
        assert_eq!(result["handled"], true);
        assert_eq!(result["ok"], false);
        assert_eq!(result["afterAttempt"], true);
        assert_eq!(result["error"], "fixture activation failed after stop");
    }

    #[test]
    fn elevated_store_start_failure_without_an_old_process_still_requires_observation() {
        let result = restart_fixture(
            r#"
function IsElevated { $true }
function ValidateApp { }
function DesktopShell { [pscustomobject]@{} }
function AppProcesses { }
function StopDesktop { $script:CodexCloseStarted=$false }
function StartWithDesktopShell { throw 'fixture activation confirmation was delayed' }
"#,
            false,
        );
        assert_eq!(result["handled"], true);
        assert_eq!(result["ok"], false);
        assert_eq!(result["afterAttempt"], true);
        assert_eq!(
            result["error"],
            "fixture activation confirmation was delayed"
        );
    }

    #[test]
    fn unelevated_store_restart_uses_existing_activation_path() {
        let result = restart_fixture(
            r#"
function IsElevated { $false }
function ValidateApp { throw 'unexpected validation' }
function DesktopShell { throw 'unexpected broker' }
function StopDesktop { throw 'unexpected stop' }
"#,
            false,
        );
        assert_eq!(result["handled"], false);
    }

    #[test]
    fn desktop_shell_broker_compiles_without_requiring_an_interactive_desktop() {
        let result = powershell(
            &format!(
                "{DESKTOP_SHELL}\ntry {{ DesktopShell | Out-Null }} catch {{ }}; Emit ($null -ne ('CodexDesktopShellIdentity' -as [type]))"
            ),
            Value::Null,
            Duration::from_secs(15),
        )
        .unwrap();
        assert_eq!(result, true);
    }
}
