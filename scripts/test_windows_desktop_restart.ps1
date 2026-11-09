# Execute the production PowerShell helpers against isolated fixture processes.
# No official Codex/ChatGPT process is inspected or stopped by these fixtures.
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'This check requires Windows.' }
$source = Get-Content (Join-Path $PSScriptRoot '../apps/desktop/src-tauri/src/codex_desktop_windows.rs') -Raw -Encoding UTF8
function Constant([string]$Name) {
  $match = [regex]::Match($source, '(?s)const ' + [regex]::Escape($Name) + ': &str = r#"(.*?)"#;')
  if (-not $match.Success) { throw "Missing production/test constant: $Name" }
  $match.Groups[1].Value
}
$common = Constant 'COMMON'
$stop = Constant 'STOP_DESKTOP'
$fixtures = Constant 'PROCESS_FIXTURES'
$cases = @(
  @{Name='cim_missing_path_uses_live_identity_and_ignores_exited_foreign_and_recycled_pids';Expected=@{ids=@(101);probed=@(101,102,105,106)}},
  @{Name='inaccessible_live_process_aborts_before_any_client_is_closed';Expected=@{closed=$false;error='CODEXX_PROCESS_ACCESS_REQUIRED: PID 101'}},
  @{Name='stop_waits_on_bound_handles_instead_of_pathless_cim_rows_after_close';Expected=@{closed=$true;queries=1;disposed=1}},
  @{Name='stop_binds_all_processes_before_closing_and_releases_handles_on_preflight_failure';Expected=@{closed=$false;disposed=1;error='fixture: selected helper cannot be controlled'}}
)
foreach ($case in $cases) {
  $start=$source.IndexOf('fn '+$case.Name+'(')
  if ($start -lt 0) { throw "Missing fixture: $($case.Name)" }
  $match=[regex]::Match($source.Substring($start), '(?s)process_fixture\(\s*r#"(.*?)"#,\s*(true|false)\s*,?\s*\)')
  if (-not $match.Success) { throw "Cannot extract fixture: $($case.Name)" }
  $result = & {
    $data=[pscustomobject]@{app=[pscustomobject]@{executable='C:\fixture\Codex.exe'}}
    . ([scriptblock]::Create($common))
    . ([scriptblock]::Create($stop))
    . ([scriptblock]::Create($fixtures))
    . ([scriptblock]::Create($match.Groups[1].Value))
  }
  $actual = ($result -join "`n") | ConvertFrom-Json
  foreach ($key in $case.Expected.Keys) {
    $left=ConvertTo-Json -InputObject $actual.$key -Compress
    $right=ConvertTo-Json -InputObject $case.Expected[$key] -Compress
    if ($left -cne $right) { throw "$($case.Name): $key returned $left, expected $right" }
  }
  Write-Host "Passed: $($case.Name)"
}
# Exercise actual limited-query Windows APIs only against this test host itself.
. ([scriptblock]::Create($common))
EnsureProcessApi
$live=InspectDesktopProcess $PID
try {
  $bound=[CodexDesktopProcess]::Bind([uint32]$PID,$live.ExecutablePath,$live.CreationDate.Ticks,$false)
  try {
    if ($null -eq $bound -or $bound.HasExited) { throw 'Read-only native identity binding failed.' }
    if ($null -ne [CodexDesktopProcess]::Bind([uint32]$PID,($live.ExecutablePath+'.wrong'),$live.CreationDate.Ticks,$false)) { throw 'Path mismatch was accepted.' }
    if ($null -ne [CodexDesktopProcess]::Bind([uint32]$PID,$live.ExecutablePath,($live.CreationDate.Ticks+10000),$false)) { throw 'Creation time mismatch was accepted.' }
  } finally { if ($null -ne $bound) { $bound.Dispose() } }
} finally { $live.Dispose() }
Write-Host 'Passed: native query-only process binding; no application was terminated.'
