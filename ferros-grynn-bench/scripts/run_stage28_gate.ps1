param(
 [Parameter(Mandatory=$true)][string]$BaselineBinary,
 [Parameter(Mandatory=$true)][string]$CommonBinary,
 [Parameter(Mandatory=$true)][string]$LiveBinary
)
$ErrorActionPreference='Stop'
if (Test-Path Env:FG28_BUDGET) {throw 'Default L11 matrix requires FG28_BUDGET absent; budgeted workloads use separate probes.'}
$taskWorkspace=(Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../..')).Path
Set-Location -LiteralPath $taskWorkspace
$taskBaseline=(Resolve-Path -LiteralPath $BaselineBinary).Path
$taskCommon=(Resolve-Path -LiteralPath $CommonBinary).Path
$taskLive=(Resolve-Path -LiteralPath $LiveBinary).Path
$taskRoot=Join-Path $taskWorkspace 'target/stage28-baseline/l11-attempts'
New-Item -ItemType Directory -Path $taskRoot -Force | Out-Null
# The exclusive lock covers ledger allocation, all measurements and final publication.
$taskLock=[System.IO.File]::Open((Join-Path $taskRoot 'runner.lock'),[System.IO.FileMode]::OpenOrCreate,[System.IO.FileAccess]::ReadWrite,[System.IO.FileShare]::None)
$taskLedger=Join-Path $taskRoot 'attempts.json'
$taskRun=$null
$taskRuns=@()
$taskCellMaximumSeconds=900
function Assert-Ledger([object[]]$Runs) {
 if ($Runs.Count -eq 0) {throw 'Invalid empty attempt ledger.'}
 for ($i=0;$i -lt $Runs.Count;$i++) {if ($Runs[$i].Number -ne $i+1) {throw 'Invalid attempt sequence.'}}
}
function Save-Ledger {
 $temporary=Join-Path $taskRoot ('ledger-'+[guid]::NewGuid().ToString('N')+'.tmp')
 $json=ConvertTo-Json -InputObject @($taskRuns) -Depth 12
 [System.IO.File]::WriteAllText($temporary,$json,[System.Text.UTF8Encoding]::new($false))
 if ([System.IO.File]::Exists($taskLedger)) { [System.IO.File]::Replace($temporary,$taskLedger,(Join-Path $taskRoot 'attempts.previous.json')) }
 else { [System.IO.File]::Move($temporary,$taskLedger) }
}
function Cell-Outcome([string]$Key,[string]$Text) {
 if ($Text -match 'status=(registration_resource_rejected|evaluation_resource_fault|resource_rejected)') {return 'CapacityGap'}
 if ($Key -match '-resource$' -and $Text -match 'status=expected_rejection') {return 'VerifiedResourceProbe'}
 if ($Key -match '-import_(on|off)$' -and $Text -match 'status=ok') {return 'VerifiedImportProbe'}
 if ($Text -match '\btime:\s*\[') {return 'Measured'}
 throw ('No validated measured/probe outcome for '+$Key)
}
function Run-Cell([string]$Key,[string]$Binary,[string[]]$Arguments) {
 $existing=@($taskRun.Cells | Where-Object { $_.Key -eq $Key })
 if ($existing.Count -gt 1) {throw ('Duplicate cell '+$Key)}
 if ($existing.Count -eq 1 -and $existing[0].FinishedUtc) {
  if ($existing[0].Outcome -eq 'TimedOut') {return}
  if ($existing[0].ExitCode -ne 0 -or $existing[0].Outcome -eq 'Invalid') {throw ('Recorded cell failure '+$Key)}
  return
 }
 $process=$null;$cell=$null;$owned=$false
 try {
 if ($existing.Count -eq 1) {
  $cell=$existing[0]
  if (-not $cell.Pid) {throw ('Unresolved launch; reconcile owned child before resuming '+$Key)}
  $process=Get-Process -Id $cell.Pid -ErrorAction SilentlyContinue
  if (-not $process) {throw ('Interrupted child outcome unknown; preserving evidence for '+$Key)}
  if ($process.Path -ne $Binary -or $process.StartTime.ToUniversalTime() -ne ([DateTime]$cell.ProcessStartedUtc).ToUniversalTime()) {throw ('In-flight PID ownership mismatch '+$Key)}
  # Acquire the handle before exit so its actual exit code remains available.
  $null=$process.Handle;$owned=$true
 } else {
  $cell=[pscustomobject]@{Key=$Key;StartedUtc=[DateTime]::UtcNow.ToString('o');ProcessStartedUtc=$null;Pid=$null;State='Starting';FinishedUtc=$null;ExitCode=$null;Outcome=$null;SampledProcessPeakRss=$null;Stdout=(Join-Path $taskOutput ($Key+'.log'));Stderr=(Join-Path $taskOutput ($Key+'.stderr.log'))}
  $taskRun.Cells+=@($cell);Save-Ledger
  $process=Start-Process -FilePath $Binary -ArgumentList $Arguments -WorkingDirectory $taskWorkspace -WindowStyle Hidden -PassThru -RedirectStandardOutput $cell.Stdout -RedirectStandardError $cell.Stderr
  $owned=$true
  $cell.Pid=$process.Id;$cell.ProcessStartedUtc=$process.StartTime.ToUniversalTime().ToString('o');$cell.State='Running';Save-Ledger
 }
 $peak=if ($null -eq $cell.SampledProcessPeakRss) {0L}else{[long]$cell.SampledProcessPeakRss}
  $timedOut=$false
  $initialAge=[Math]::Max(0,([DateTime]::UtcNow-([DateTime]$cell.StartedUtc).ToUniversalTime()).TotalSeconds)
  $elapsed=[System.Diagnostics.Stopwatch]::StartNew()
  while (-not $process.HasExited) {
   $process.Refresh();$peak=[Math]::Max($peak,$process.PeakWorkingSet64)
   if (-not $process.HasExited -and $initialAge+$elapsed.Elapsed.TotalSeconds -ge $taskCellMaximumSeconds) {
    $timedOut=$true
    try {$process.Kill($true)} catch {if (-not $process.HasExited) {throw}}
    break
   }
   Start-Sleep -Milliseconds 200
  }
  $process.WaitForExit();$cell.ExitCode=$process.ExitCode;$cell.FinishedUtc=[DateTime]::UtcNow.ToString('o');$cell.SampledProcessPeakRss=if ($peak -gt 0) {$peak}else{$null};$cell.State='Completed'
  if ($timedOut) {$cell.Outcome='TimedOut'}
  elseif ($cell.ExitCode -ne 0) {$cell.Outcome='Invalid';Save-Ledger;throw ('Gate cell failed '+$Key)}
  else {$cell.Outcome=Cell-Outcome $Key (Get-Content -LiteralPath $cell.Stdout -Raw)}
  Save-Ledger
  Write-Output ('L11 attempt '+$taskRun.Number+' '+$Key+' outcome='+$cell.Outcome+' sampled_peak_rss='+$cell.SampledProcessPeakRss)
 } finally {
  try {
   if ($owned -and $process -and -not $process.HasExited) {
    $process.Kill($true);$process.WaitForExit();$cell.State='Interrupted';$cell.Outcome='Invalid';$cell.ExitCode=$process.ExitCode;$cell.FinishedUtc=[DateTime]::UtcNow.ToString('o');Save-Ledger
   }
  } finally {if ($process) {$process.Dispose()}}
 }
}
try {
 if (Test-Path -LiteralPath $taskLedger) {
  $raw=Get-Content -LiteralPath $taskLedger -Raw
  if ([string]::IsNullOrWhiteSpace($raw)) {throw 'Empty attempt ledger; fail closed.'}
  $taskRuns=@($raw | ConvertFrom-Json)
  Assert-Ledger $taskRuns
 }
 $taskDigests=@((Get-FileHash -LiteralPath $taskBaseline).Hash,(Get-FileHash -LiteralPath $taskCommon).Hash,(Get-FileHash -LiteralPath $taskLive).Hash)
 $active=@($taskRuns | Where-Object {$_.State -eq 'Running'})
 if ($active.Count -gt 1) {throw 'Multiple active gate attempts.'}
 if ($active.Count -eq 1) {
  $taskRun=$active[0]
  if (($taskRun.Digests -join '|') -ne ($taskDigests -join '|')) {throw 'Cannot resume with changed binaries.'}
 } else {
  $taskRun=[pscustomobject]@{Number=$taskRuns.Count+1;State='Running';StartedUtc=[DateTime]::UtcNow.ToString('o');FinishedUtc=$null;Digests=$taskDigests;Cells=@();Comparison=@();CommonVerdict=$null;CapacityGaps=@();TimedOutCells=@();CellMaximumSeconds=$taskCellMaximumSeconds;Verdict=$null}
  $taskRuns+=@($taskRun)
 }
 $taskOutput=Join-Path $taskRoot ('attempt-'+$taskRun.Number)
 New-Item -ItemType Directory -Path $taskOutput -Force | Out-Null
 $env:CRITERION_HOME=Join-Path $taskOutput 'criterion'
 Save-Ledger
 Remove-Item Env:FG28_SMOKE -ErrorAction SilentlyContinue
 $env:FG27_LONG='1'
 $expected=@()
 foreach ($rows in @(10000,100000,1000000)) {
  $env:FG27_ROWS=[string]$rows
  foreach ($variant in @('base','current')) {
   $key='common-'+$variant+'-'+$rows;$expected+=@($key)
   $binary=if ($variant -eq 'base') {$taskBaseline}else{$taskCommon}
   Run-Cell $key $binary @('--bench','--save-baseline',('l11-a'+$taskRun.Number+'-'+$variant))
  }
 }
 foreach ($local in @(0,1)) {
  $env:FG28_LOCAL=[string]$local
  foreach ($rows in @(10000,100000,1000000)) {
   $env:FG28_ROWS=[string]$rows
   foreach ($case in @('scan_1','scan_10','scan_100','fetch','let_data','let_param','replay','ack','import_on','import_off','resource')) {
    $env:FG28_CASE=$case;$key='live-'+$local+'-'+$rows+'-'+$case;$expected+=@($key)
    Run-Cell $key $taskLive @('--bench','--save-baseline',('l11-a'+$taskRun.Number+'-live-'+$local))
   }
  }
 }
 if ($taskRun.Cells.Count -ne $expected.Count -or @($expected | Where-Object {$_ -notin $taskRun.Cells.Key}).Count -ne 0) {throw 'Incomplete gate cell matrix.'}
 $comparison=@()
 foreach ($rows in @(10000,100000,1000000)) {
  foreach ($name in @('return_literal','point_read_16','mutate_64')) {
   if (@($taskRun.Cells | Where-Object {$_.Key -in @(('common-base-'+$rows),('common-current-'+$rows)) -and $_.Outcome -eq 'TimedOut'}).Count -gt 0) {
    $comparison+=@([pscustomobject]@{Rows=$rows;Workload=$name;MeanRatio=$null;ConservativeUpperRatio=$null;Pass=$false})
    continue
   }
   $path=Join-Path $env:CRITERION_HOME ('gql_stage27_common/'+$name+'/'+$rows)
   $before=Get-Content -LiteralPath (Join-Path $path ('l11-a'+$taskRun.Number+'-base/estimates.json')) -Raw | ConvertFrom-Json
   $after=Get-Content -LiteralPath (Join-Path $path ('l11-a'+$taskRun.Number+'-current/estimates.json')) -Raw | ConvertFrom-Json
   $ratio=$after.mean.point_estimate/$before.mean.point_estimate
   $upper=$after.mean.confidence_interval.upper_bound/$before.mean.confidence_interval.lower_bound
   $comparison+=@([pscustomobject]@{Rows=$rows;Workload=$name;MeanRatio=$ratio;ConservativeUpperRatio=$upper;Pass=($ratio -le 1.05 -and $upper -le 1.05)})
  }
 }
 $taskRun.Comparison=$comparison
 $taskRun.CommonVerdict=if (@($comparison | Where-Object {-not $_.Pass}).Count -eq 0) {'PASS'}else{'FAIL_OR_INCONCLUSIVE'}
 $taskRun.CapacityGaps=@($taskRun.Cells | Where-Object {$_.Outcome -eq 'CapacityGap'} | ForEach-Object {$_.Key})
 $timedOutCells=@($taskRun.Cells | Where-Object {$_.Outcome -eq 'TimedOut'} | ForEach-Object {$_.Key})
 if ($taskRun.PSObject.Properties.Name -notcontains 'TimedOutCells') {$taskRun | Add-Member -NotePropertyName TimedOutCells -NotePropertyValue @()}
 $taskRun.TimedOutCells=$timedOutCells
 $taskRun.Verdict=if ($taskRun.CommonVerdict -ne 'PASS') {'FAIL_OR_INCONCLUSIVE'}elseif ($timedOutCells.Count -gt 0) {'COMMON_PASS_LIVE_TIMEOUT_GAPS'}elseif ($taskRun.CapacityGaps.Count -gt 0) {'COMMON_PASS_LIVE_CAPACITY_GAPS'}else{'PASS'}
 $taskRun.State='Completed'
} catch {
 if ($taskRun) {$taskRun.State='Failed';$taskRun.Verdict=$_.Exception.Message;Save-Ledger}
 throw
} finally {
 try {if ($taskRun -and $taskRun.State -ne 'Running') {$taskRun.FinishedUtc=[DateTime]::UtcNow.ToString('o');Save-Ledger}}
 finally {$taskLock.Dispose()}
}
Write-Output ('L11 attempt '+$taskRun.Number+' verdict='+$taskRun.Verdict)
