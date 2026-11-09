# Run only on an ephemeral GitHub Windows runner. Never run on a user's machine.
param([Parameter(Mandatory = $true)][string]$Installer)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows') {
  throw 'The installation smoke test is restricted to an ephemeral Windows Actions runner.'
}
$Installer = (Resolve-Path $Installer).Path
$UpgradeCode = '{1482A8E8-9217-517B-8528-93A6C90E0C2F}'
$LegacyProductCode = '{F71E3F3F-A463-4397-AB46-206D3FAC3FBD}'
$LegacySha256 = 'e0f2f172a31f860806a9714bab2e67eb38b85d525d7e5f0f569667cbeadfb152'
$InstallDir = Join-Path $env:LOCALAPPDATA 'Codex-X'
$UninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Codex-X'
$Work = Join-Path $env:RUNNER_TEMP 'codex-x-installer-smoke'
New-Item -ItemType Directory -Force $Work | Out-Null
Add-Type @'
using System.Runtime.InteropServices;
using System.Text;
public static class CodexXMsiSmoke {
  [DllImport("msi.dll", CharSet = CharSet.Unicode)]
  public static extern uint MsiEnumRelatedProducts(string code, uint reserved, uint index, StringBuilder product);
  [DllImport("msi.dll", CharSet = CharSet.Unicode)]
  public static extern uint MsiGetProductInfoEx(string code, System.IntPtr sid, uint context, string property, StringBuilder value, ref uint length);
}
'@
function Related-Products {
  $results = @()
  for ($index = 0; $index -lt 64; $index++) {
    $product = [Text.StringBuilder]::new(39)
    $result = [CodexXMsiSmoke]::MsiEnumRelatedProducts($UpgradeCode, 0, $index, $product)
    if ($result -eq 259) { return $results }
    if ($result -ne 0) { throw "MsiEnumRelatedProducts failed: $result" }
    $results += $product.ToString()
  }
  throw 'Unexpected number of MSI registrations.'
}
function Invoke-Installer([string]$File, [string]$Arguments, [int]$ExpectedExit = 0) {
  $watch = [Diagnostics.Stopwatch]::StartNew()
  $process = Start-Process -FilePath $File -ArgumentList $Arguments -PassThru
  if (-not $process.WaitForExit(600000)) {
    # Do not kill msiexec or an installer transaction. Fail the disposable job.
    throw "Installer has not finished after ten minutes (PID $($process.Id)); no forced termination was attempted."
  }
  $process.Refresh()
  if ($process.ExitCode -ne $ExpectedExit) { throw "Installer returned $($process.ExitCode), expected $ExpectedExit" }
  Write-Host "Installer completed in $([math]::Round($watch.Elapsed.TotalSeconds, 1)) seconds."
}
function Assert-Installed {
  if (-not (Test-Path (Join-Path $InstallDir 'codex-x.exe'))) { throw 'EXE not installed in original user LOCALAPPDATA.' }
  $registration = Get-ItemProperty $UninstallKey
  if ($registration.MainBinaryName -ne 'codex-x.exe') { throw 'Invalid NSIS registration.' }
  if (@(Related-Products).Count -ne 0) { throw 'Legacy MSI is still registered.' }
  foreach ($root in @('HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall', 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall', 'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall')) {
    $found = @(Get-ChildItem $root -ErrorAction SilentlyContinue | Get-ItemProperty | Where-Object { $_.DisplayName -eq 'Codex-X' })
    if ($root.StartsWith('HKCU:')) {
      if ($found.Count -ne 1) { throw 'Expected exactly one current-user uninstall entry.' }
    } elseif ($found.Count -ne 0) { throw 'A machine-wide Codex-X uninstall entry remains.' }
  }
  foreach ($entry in $Sentinels.GetEnumerator()) {
    if (-not (Test-Path $entry.Key) -or (Get-FileHash $entry.Key -Algorithm SHA256).Hash -ne $entry.Value) {
      throw "User configuration changed during installation: $($entry.Key)"
    }
  }
}
function Reset-FixtureInstallLocation {
  # Tauri intentionally preserves this install-location value when keeping
  # user data. The old MSI's AppSearch overrides command-line INSTALLDIR with
  # it. Remove only values produced by the previous synthetic install so this
  # fixture accurately represents a legacy default Program Files installation.
  $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\yynxxxxx\Codex-X', $true)
  if ($null -ne $key) {
    try {
      $key.DeleteValue('', $false)
      $key.DeleteValue('InstallDir', $false)
    } finally { $key.Dispose() }
  }
}
function Remove-TestNsis {
  # _?= avoids NSIS's detached temporary uninstaller process so WaitForExit
  # actually covers registry/files removal. This is NSIS's documented syntax.
  $uninstaller = Join-Path $InstallDir 'uninstall.exe'
  if (Test-Path $uninstaller) { Invoke-Installer $uninstaller "/S _?=$InstallDir" }
  if (Test-Path $UninstallKey) { throw 'NSIS uninstall registration was not removed.' }
}
if ((Test-Path $UninstallKey) -or @(Related-Products).Count -ne 0) { throw 'Runner already has a Codex-X installation.' }
$Sentinels = @{}
foreach ($relative in @('.codex\config.toml', '.codex\auth.json', '.codexx\installer-smoke-data.txt')) {
  $file = Join-Path $env:USERPROFILE $relative
  if (Test-Path $file) { throw "Refusing to overwrite an existing file: $file" }
  New-Item -ItemType Directory -Force (Split-Path $file) | Out-Null
  [IO.File]::WriteAllText($file, "Codex-X installer sentinel: $relative")
  $Sentinels[$file] = (Get-FileHash $file -Algorithm SHA256).Hash
}
try {
  Write-Host 'Scenario 1: clean current-user install and subsequent NSIS update.'
  Invoke-Installer $Installer '/S'
  Assert-Installed
  Invoke-Installer $Installer '/S /UPDATE'
  Assert-Installed
  Remove-TestNsis
  Reset-FixtureInstallLocation

  Write-Host 'Scenario 2: released v0.3.20 machine MSI -> current-user NSIS -> NSIS update.'
  $legacyAsset = 'Codex-X-0.3.20-windows-x64.msi'
  $legacyMsi = Join-Path $Work $legacyAsset
  for ($attempt = 1; $attempt -le 3; $attempt++) {
    try {
      if ($attempt -gt 1 -and $env:GH_TOKEN -and (Get-Command gh -ErrorAction SilentlyContinue)) {
        gh release download v0.3.20 --repo yynxxxxx/Codex-X --pattern $legacyAsset --dir $Work --clobber
        if ($LASTEXITCODE -ne 0) { throw 'GitHub API fixture download failed.' }
      } else {
        Invoke-WebRequest "https://github.com/yynxxxxx/Codex-X/releases/download/v0.3.20/$legacyAsset" -OutFile $legacyMsi -TimeoutSec 60
      }
      break
    } catch {
      if ($attempt -eq 3) { throw }
      Write-Host "Legacy fixture download attempt $attempt failed; retrying through GitHub's asset API when available."
      Start-Sleep -Seconds 3
    }
  }
  if ((Get-FileHash $legacyMsi -Algorithm SHA256).Hash.ToLowerInvariant() -ne $LegacySha256) { throw 'Released MSI hash mismatch.' }
  Invoke-Installer (Join-Path $env:SystemRoot 'System32\msiexec.exe') "/i `"$legacyMsi`" INSTALLDIR=`"$env:ProgramFiles\Codex-X`" /qn /norestart /L*v `"$Work\legacy-install.log`""
  if ($LegacyProductCode -notin @(Related-Products)) { throw 'The genuine legacy MSI did not register.' }
  $oldDirectory = [Text.StringBuilder]::new(1024)
  [uint32]$oldDirectoryLength = 1024
  $status = [CodexXMsiSmoke]::MsiGetProductInfoEx($LegacyProductCode, [IntPtr]::Zero, 4, 'InstallLocation', $oldDirectory, [ref]$oldDirectoryLength)
  if ($status -ne 0 -or $oldDirectory.Length -eq 0) { throw 'Could not read actual legacy MSI InstallLocation.' }
  $oldPath = $oldDirectory.ToString().TrimEnd('\')
  Write-Host "Actual legacy MSI InstallLocation: $oldPath"
  if ($oldPath -ne (Join-Path $env:ProgramFiles 'Codex-X')) { throw 'The legacy fixture was not installed to Program Files.' }
  $oldExe = Join-Path $oldPath 'codex-x.exe'
  $oldHash = (Get-FileHash $oldExe).Hash
  # Guard the old directory, a non-existent child, and a junction alias. None
  # may remove the registered MSI or overwrite its files.
  Invoke-Installer $Installer "/S /UPDATE /D=$oldPath" 1
  Invoke-Installer $Installer "/S /UPDATE /D=$oldPath\unsafe-child" 1
  $alias = Join-Path $Work 'legacy-junction'
  New-Item -ItemType Junction -Path $alias -Target $oldPath | Out-Null
  try {
    Invoke-Installer $Installer "/S /UPDATE /D=$alias\unsafe-child" 1
  } finally {
    [IO.Directory]::Delete($alias)
  }
  if ($LegacyProductCode -notin @(Related-Products) -or (Get-FileHash $oldExe).Hash -ne $oldHash) {
    throw 'A refused overlapping installation changed the legacy app.'
  }
  # Keep the real MSI's HKCU manufacturer path to verify that NSIS does not
  # inherit Program Files when the user accepts the default directory.
  Invoke-Installer $Installer '/S /UPDATE'
  Assert-Installed
  Invoke-Installer $Installer '/S /UPDATE'
  Assert-Installed
  Remove-TestNsis
  Reset-FixtureInstallLocation

  Write-Host 'Scenario 3: MSI returns 3010 after removal; separate NSIS install continues without reboot.'
  # A separate fixture copy of the hash-verified released MSI requests a reboot
  # only after REMOVE=ALL. /norestart prevents any OS reboot. This exercises the
  # genuine Windows Installer 3010 result, not a mocked installer exit code.
  $rebootMsi = Join-Path $Work 'legacy-deferred-cleanup-fixture.msi'
  Copy-Item $legacyMsi $rebootMsi
  $msiAutomation = New-Object -ComObject WindowsInstaller.Installer
  $database = $msiAutomation.OpenDatabase($rebootMsi, 1)
  $query = $database.OpenView('INSERT INTO `InstallExecuteSequence` (`Action`, `Condition`, `Sequence`) VALUES (''ScheduleReboot'', ''REMOVE="ALL"'', 6500)')
  $query.Execute()
  $query.Close()
  $summary = $database.GetType().InvokeMember('SummaryInformation', [Reflection.BindingFlags]::GetProperty, $null, $database, @(1))
  $newPackageCode = '{' + [guid]::NewGuid().ToString().ToUpperInvariant() + '}'
  [void]$summary.GetType().InvokeMember('Property', [Reflection.BindingFlags]::SetProperty, $null, $summary, @(9, $newPackageCode))
  $summary.Persist()
  $database.Commit()
  foreach ($comObject in @($query, $summary, $database, $msiAutomation)) {
    [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($comObject)
  }
  Invoke-Installer (Join-Path $env:SystemRoot 'System32\msiexec.exe') "/i `"$rebootMsi`" INSTALLDIR=`"$env:ProgramFiles\Codex-X`" /qn /norestart /L*v `"$Work\deferred-fixture-install.log`""
  if ($LegacyProductCode -notin @(Related-Products)) { throw 'The deferred-cleanup MSI fixture did not register.' }
  $beforeMigration = Get-Date
  Invoke-Installer $Installer '/S /UPDATE'
  Assert-Installed
  $migrationLogs = @(Get-ChildItem (Join-Path $env:LOCALAPPDATA 'Codex-X-updates\logs') -Filter 'install-*.log' |
    Where-Object { $_.LastWriteTime -ge $beforeMigration -and $_.Name -notlike '*-msi-*' })
  if (-not ($migrationLogs | Select-String -SimpleMatch 'MSI exit code: 3010')) { throw 'The deferred-cleanup scenario did not actually exercise MSI exit 3010.' }
  Invoke-Installer $Installer '/S /UPDATE'
  Assert-Installed
  Write-Host 'Windows installation smoke tests passed; no reboot needed, configuration sentinels unchanged.'
} finally {
  Get-ChildItem (Join-Path $env:LOCALAPPDATA 'Codex-X-updates\logs') -Filter 'install-*.log' -ErrorAction SilentlyContinue | ForEach-Object {
    Write-Host "--- $($_.Name) ---"
    Get-Content $_.FullName -Tail 100
  }
  Remove-TestNsis
}
