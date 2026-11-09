# Destructive installation checks run only on an ephemeral Windows Actions runner.
# This verifies package/upgrade/startup behavior, not Windows Smart App Control trust.
param([Parameter(Mandatory = $true)][string]$Installer)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows') {
  throw 'MSI installation checks are restricted to an ephemeral Windows Actions runner.'
}
$Installer = (Resolve-Path -LiteralPath $Installer).Path
if ([IO.Path]::GetExtension($Installer) -ne '.msi') { throw 'An MSI installer is required.' }
$UpgradeCode = '{1482A8E8-9217-517B-8528-93A6C90E0C2F}'
$LegacyProductCode = '{F71E3F3F-A463-4397-AB46-206D3FAC3FBD}'
$LegacySha256 = 'e0f2f172a31f860806a9714bab2e67eb38b85d525d7e5f0f569667cbeadfb152'
$Work = Join-Path $env:RUNNER_TEMP ('codex-x-msi-smoke-' + [guid]::NewGuid().ToString('N'))
$Msiexec = Join-Path $env:SystemRoot 'System32\msiexec.exe'
$NsisSubKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\Codex-X'
New-Item -ItemType Directory -Force -Path $Work | Out-Null
Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class CodexXMsiVerification {
  [DllImport("msi.dll", CharSet=CharSet.Unicode, ExactSpelling=true)]
  public static extern uint MsiEnumRelatedProductsW(string code, uint reserved, uint index, StringBuilder product);
  [DllImport("msi.dll", CharSet=CharSet.Unicode, ExactSpelling=true)]
  public static extern uint MsiGetProductInfoExW(string code, IntPtr sid, uint context, string property, StringBuilder value, ref uint length);
  [DllImport("msi.dll", CharSet=CharSet.Unicode, ExactSpelling=true)]
  public static extern uint MsiOpenDatabaseW(string path, IntPtr persist, out uint database);
  [DllImport("msi.dll", CharSet=CharSet.Unicode, ExactSpelling=true)]
  public static extern uint MsiDatabaseOpenViewW(uint database, string query, out uint view);
  [DllImport("msi.dll", ExactSpelling=true)]
  public static extern uint MsiViewExecute(uint view, uint record);
  [DllImport("msi.dll", ExactSpelling=true)]
  public static extern uint MsiViewFetch(uint view, out uint record);
  [DllImport("msi.dll", CharSet=CharSet.Unicode, ExactSpelling=true)]
  public static extern uint MsiRecordGetStringW(uint record, uint field, StringBuilder value, ref uint length);
  [DllImport("msi.dll", ExactSpelling=true)]
  public static extern uint MsiCloseHandle(uint handle);
}
'@
function Related-Products {
  $items = @()
  for ($index = 0; $index -lt 64; $index++) {
    $product = [Text.StringBuilder]::new(39)
    $result = [CodexXMsiVerification]::MsiEnumRelatedProductsW($UpgradeCode, 0, $index, $product)
    if ($result -eq 259) { return $items }
    if ($result -ne 0) { throw "MSI enumeration failed: $result" }
    $items += $product.ToString()
  }
  throw 'Unexpected MSI registration count.'
}
function Product-Info([string]$Product, [string]$Property) {
  $value = [Text.StringBuilder]::new(4096)
  [uint32]$length = 4096
  $result = [CodexXMsiVerification]::MsiGetProductInfoExW($Product, [IntPtr]::Zero, 4, $Property, $value, [ref]$length)
  if ($result -ne 0) { throw "MSI $Property lookup failed: $result" }
  return $value.ToString()
}
function Read-MsiProperty([string]$Path, [string]$Name) {
  if ($Name -notmatch '^[A-Za-z0-9_]+$') { throw 'Invalid MSI property name.' }
  [uint32]$database = 0
  [uint32]$view = 0
  [uint32]$record = 0
  try {
    $result = [CodexXMsiVerification]::MsiOpenDatabaseW($Path, [IntPtr]::Zero, [ref]$database)
    if ($result -ne 0) { throw "Cannot open MSI read-only: $result" }
    $result = [CodexXMsiVerification]::MsiDatabaseOpenViewW($database, "SELECT ``Value`` FROM ``Property`` WHERE ``Property``='$Name'", [ref]$view)
    if ($result -ne 0) { throw "Cannot query MSI property $Name`: $result" }
    $result = [CodexXMsiVerification]::MsiViewExecute($view, 0)
    if ($result -ne 0) { throw "Cannot execute MSI property query: $result" }
    $result = [CodexXMsiVerification]::MsiViewFetch($view, [ref]$record)
    if ($result -ne 0) { throw "MSI property $Name is missing: $result" }
    $value = [Text.StringBuilder]::new(4096)
    [uint32]$length = 4096
    $result = [CodexXMsiVerification]::MsiRecordGetStringW($record, 1, $value, [ref]$length)
    if ($result -ne 0) { throw "Cannot read MSI property $Name`: $result" }
    return $value.ToString()
  } finally {
    foreach ($handle in @($record, $view, $database)) {
      if ($handle -ne 0) { [void][CodexXMsiVerification]::MsiCloseHandle($handle) }
    }
  }
}
function Start-Msi([string]$Arguments) {
  return Start-Process -FilePath $Msiexec -ArgumentList $Arguments -PassThru
}
function Wait-Msi($Process, [int]$Expected = 0) {
  if (-not $Process.WaitForExit(600000)) {
    throw 'MSI exceeded ten minutes; the Windows Installer transaction was not terminated.'
  }
  $Process.Refresh()
  if ($Process.ExitCode -ne $Expected) { throw "MSI returned $($Process.ExitCode), expected $Expected." }
}
function Invoke-Msi([string]$Arguments, [int]$Expected = 0) {
  Wait-Msi (Start-Msi $Arguments) $Expected
}
function Reset-FixtureInstallLocation {
  # Prior NSIS checks intentionally preserve these user-preference values.
  # Only clear the synthetic runner fixture's installation location.
  $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\yynxxxxx\Codex-X', $true)
  if ($null -ne $key) {
    try { $key.DeleteValue('', $false); $key.DeleteValue('InstallDir', $false) } finally { $key.Dispose() }
  }
}
function Assert-Sentinels {
  foreach ($item in $Sentinels.GetEnumerator()) {
    if (-not (Test-Path -LiteralPath $item.Key) -or (Get-FileHash -LiteralPath $item.Key -Algorithm SHA256).Hash -ne $item.Value) {
      throw "Installation changed user data: $($item.Key)"
    }
  }
}
function Assert-Installed {
  $products = @(Related-Products)
  if ($products.Count -ne 1 -or $products[0] -ne $ProductCode) { throw 'Expected exactly the newly built MSI registration.' }
  if ((Product-Info $ProductCode 'VersionString') -ne $Version) { throw 'Installed MSI version mismatch.' }
  $location = (Product-Info $ProductCode 'InstallLocation').TrimEnd('\')
  $exe = Join-Path $location 'codex-x.exe'
  if (-not (Test-Path -LiteralPath $exe)) { throw 'Installed application is missing.' }
  if ((Get-Item -LiteralPath $exe).VersionInfo.ProductVersion -notlike "$Version*") { throw 'Installed executable version mismatch.' }
  Assert-Sentinels
  return $exe
}
function Wait-AppWindow($Process) {
  $deadline = [DateTime]::UtcNow.AddSeconds(45)
  do {
    $Process.Refresh()
    if ($Process.HasExited) { throw "Installed app exited before a window appeared (exit $($Process.ExitCode))." }
    if ($Process.MainWindowHandle -ne [IntPtr]::Zero) {
      Start-Sleep -Seconds 2
      $Process.Refresh()
      if ($Process.HasExited) { throw 'Installed app exited immediately after opening.' }
      return $Process
    }
    Start-Sleep -Milliseconds 250
  } while ([DateTime]::UtcNow -lt $deadline)
  throw 'The installed application did not create a window within 45 seconds.'
}
function Start-App([string]$Exe) {
  $start = [Diagnostics.ProcessStartInfo]::new($Exe)
  $start.UseShellExecute = $false
  $start.WorkingDirectory = $Work
  $start.Environment['CODEX_HOME'] = Join-Path $Work 'codex'
  $start.Environment['CODEXX_HOME'] = Join-Path $Work 'codexx'
  $process = [Diagnostics.Process]::Start($start)
  $script:StartedApps += $process
  return Wait-AppWindow $process
}
function Stop-FixtureApp($Process) {
  # This package test simulates the updater process exiting after handoff.
  # It does not exercise the application's actual app.exit callback: WM_CLOSE
  # hides to tray, and WM_QUIT is not a supported Tauri shutdown interface.
  # Only a GUI process explicitly started/identified by this disposable test
  # is stopped. Windows Installer is a sibling process and is never stopped.
  if (-not ($StartedApps | Where-Object { $_.Id -eq $Process.Id })) {
    throw 'Refusing to stop an untracked application process.'
  }
  $Process.Refresh()
  if ($Process.HasExited) { return }
  $Process.Kill()
  if (-not $Process.WaitForExit(20000)) { throw 'The disposable test application did not exit.' }
}
function Wait-AutolaunchedApp([string]$Exe) {
  $deadline = [DateTime]::UtcNow.AddSeconds(45)
  do {
    $found = @(Get-Process -Name 'codex-x' -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $Exe })
    if ($found.Count -gt 1) { throw 'Multiple application instances started after MSI update.' }
    if ($found.Count -eq 1) {
      $script:StartedApps += $found[0]
      return Wait-AppWindow $found[0]
    }
    Start-Sleep -Milliseconds 250
  } while ([DateTime]::UtcNow -lt $deadline)
  throw 'MSI online-update arguments did not relaunch the application.'
}
if (@(Related-Products).Count -ne 0) { throw 'Runner already has an MSI installation.' }
if (Get-Process -Name 'codex-x' -ErrorAction SilentlyContinue) { throw 'Runner already has a running Codex-X process.' }
$ProductCode = Read-MsiProperty $Installer 'ProductCode'
$Version = Read-MsiProperty $Installer 'ProductVersion'
$PackageUpgradeCode = Read-MsiProperty $Installer 'UpgradeCode'
Write-Host "MSI identity: product=$ProductCode version=$Version upgrade=$PackageUpgradeCode"
if ([guid]$PackageUpgradeCode -ne [guid]$UpgradeCode) { throw 'MSI does not preserve the historical upgrade identity.' }
$Sentinels = @{}
# Existing NSIS checks leave synthetic data in these locations. Preserve and
# hash it, rather than replacing another check's data.
foreach ($relative in @('.codex\config.toml', '.codex\auth.json', '.codexx\msi-smoke-data.txt')) {
  $file = Join-Path $env:USERPROFILE $relative
  if (-not (Test-Path -LiteralPath $file)) {
    New-Item -ItemType Directory -Force -Path (Split-Path $file) | Out-Null
    [IO.File]::WriteAllText($file, "MSI preservation sentinel: $relative")
  }
  $Sentinels[$file] = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash
}
$StartedApps = @()
try {
  Reset-FixtureInstallLocation
  Write-Host 'Scenario 1: an existing EXE registration blocks MSI initial installation without invoking its uninstaller.'
  foreach ($hive in @([Microsoft.Win32.RegistryHive]::CurrentUser, [Microsoft.Win32.RegistryHive]::LocalMachine)) {
    foreach ($registryView in @([Microsoft.Win32.RegistryView]::Registry32, [Microsoft.Win32.RegistryView]::Registry64)) {
      $root = [Microsoft.Win32.RegistryKey]::OpenBaseKey($hive, $registryView)
      if ($root.OpenSubKey($NsisSubKey)) { $root.Dispose(); throw 'Runner already has an EXE registration.' }
      $fixture = $root.CreateSubKey($NsisSubKey)
      $fixture.SetValue('UninstallString', 'DoNotExecuteRegistryCommands')
      $fixture.Dispose()
      try {
        $log = Join-Path $Work "guard-$hive-$registryView.log"
        Invoke-Msi "/i `"$Installer`" /qn /norestart /L*v `"$log`"" 1603
        if (@(Related-Products).Count -ne 0) { throw 'A refused MSI installation registered a product.' }
        if (-not (Select-String -LiteralPath $log -SimpleMatch 'The EXE installation of Codex-X is already present' -Quiet)) { throw 'MSI failed for an unrelated reason instead of the EXE guard.' }
        Assert-Sentinels
      } finally { $root.DeleteSubKey($NsisSubKey); $root.Dispose() }
    }
  }

  Write-Host 'Scenario 2: clean MSI installation, native app startup, and repeated MSI installation.'
  Invoke-Msi "/i `"$Installer`" /qn /norestart /L*v `"$Work\clean.log`""
  $exe = Assert-Installed
  $app = Start-App $exe
  Stop-FixtureApp $app
  Invoke-Msi "/i `"$Installer`" /passive /norestart /L*v `"$Work\repeat.log`""
  $exe = Assert-Installed
  $app = Start-App $exe
  Stop-FixtureApp $app
  Invoke-Msi "/x $ProductCode /qn /norestart /L*v `"$Work\remove-clean.log`""
  if (@(Related-Products).Count -ne 0) { throw 'Clean MSI uninstall retained product registration.' }
  Assert-Sentinels
  Reset-FixtureInstallLocation

  Write-Host 'Scenario 3: released v0.3.20 MSI -> current MSI using online-update arguments with the old app initially running.'
  $legacyAsset = 'Codex-X-0.3.20-windows-x64.msi'
  $legacyMsi = Join-Path $Work $legacyAsset
  for ($attempt = 1; $attempt -le 3; $attempt++) {
    try {
      if ($attempt -gt 1 -and $env:GH_TOKEN -and (Get-Command gh -ErrorAction SilentlyContinue)) {
        gh release download v0.3.20 --repo yynxxxxx/Codex-X --pattern $legacyAsset --dir $Work --clobber
        if ($LASTEXITCODE -ne 0) { throw 'Historical MSI download through GitHub API failed.' }
      } else {
        Invoke-WebRequest "https://github.com/yynxxxxx/Codex-X/releases/download/v0.3.20/$legacyAsset" -OutFile $legacyMsi -TimeoutSec 60
      }
      break
    } catch { if ($attempt -eq 3) { throw }; Start-Sleep -Seconds 3 }
  }
  if ((Get-FileHash -LiteralPath $legacyMsi -Algorithm SHA256).Hash.ToLowerInvariant() -ne $LegacySha256) { throw 'Historical MSI checksum mismatch.' }
  Invoke-Msi "/i `"$legacyMsi`" /qn /norestart /L*v `"$Work\legacy.log`""
  if ($LegacyProductCode -notin @(Related-Products)) { throw 'Historical MSI did not register.' }
  $oldExe = Join-Path (Product-Info $LegacyProductCode 'InstallLocation') 'codex-x.exe'
  $oldApp = Start-App $oldExe
  $update = Start-Msi "/i `"$Installer`" /passive /norestart AUTOLAUNCHAPP=True /L*v `"$Work\upgrade.log`""
  # Simulate the existing updater process exiting after the installation handoff.
  Stop-FixtureApp $oldApp
  Wait-Msi $update
  $exe = Assert-Installed
  $newApp = Wait-AutolaunchedApp $exe
  Stop-FixtureApp $newApp
  Assert-Sentinels
  Write-Host "MSI $Version checks passed: registration guard, installation, repeat installation, historical upgrade, native startup and configuration preservation. Application exit callback and Smart App Control trust were not assessed."
} finally {
  foreach ($process in $StartedApps) {
    $process.Refresh()
    if (-not $process.HasExited) {
      Stop-FixtureApp $process
    }
  }
  foreach ($product in @(Related-Products)) {
    if ($product -in @($ProductCode, $LegacyProductCode)) {
      Invoke-Msi "/x $product /qn /norestart /L*v `"$Work\cleanup-$product.log`""
    }
  }
  Get-ChildItem -LiteralPath $Work -Filter '*.log' | ForEach-Object {
    Write-Host "MSI check log: $($_.Name)"
    Get-Content -LiteralPath $_.FullName -Tail 30
  }
}
