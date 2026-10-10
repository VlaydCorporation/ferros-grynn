# Exercise original runner helpers in an isolated ledger, without benchmarks.
$ErrorActionPreference='Stop'
$taskWorkspace=(Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../..')).Path
$taskRoot=Join-Path $taskWorkspace ('target/stage28-baseline/runner-timeout-selfcheck-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $taskRoot | Out-Null
$taskOutput=$taskRoot
$taskLedger=Join-Path $taskRoot 'attempts.json'
$taskCellMaximumSeconds=1
$tokens=$null;$errors=$null
$ast=[System.Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot 'run_stage28_gate.ps1'),[ref]$tokens,[ref]$errors)
if ($errors.Count) {throw 'Runner syntax errors'}
foreach ($name in @('Assert-Ledger','Save-Ledger','Cell-Outcome','Run-Cell')) {
 $definition=$ast.Find({param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name},$true)
 if (-not $definition) {throw ('Missing original helper '+$name)}
 Invoke-Expression $definition.Extent.Text
}
$taskRun=[pscustomobject]@{Number=99;Cells=@();CommonVerdict='PASS';CapacityGaps=@();Verdict=$null}
$taskRuns=@($taskRun)
$binary=(Get-Command pwsh -ErrorAction Stop).Source
Run-Cell 'live-timeout' $binary @('-NoLogo','-NoProfile','-Command','Start-Sleep -Seconds 10')
if ($taskRun.Cells.Count -ne 1 -or $taskRun.Cells[0].Outcome -ne 'TimedOut') {throw 'Timeout not recorded'}
$ended=Get-Process -Id $taskRun.Cells[0].Pid -ErrorAction SilentlyContinue
if ($ended) {throw 'Timed out child survived'}
Run-Cell 'live-timeout' $binary @('-NoLogo','-NoProfile','-Command','Start-Sleep -Seconds 10')
if ($taskRun.Cells.Count -ne 1) {throw 'Timeout replay launched a second child'}
$timedOutCells=@('live-timeout')
$verdict=$ast.Find({param($node) $node -is [System.Management.Automation.Language.AssignmentStatementAst] -and $node.Extent.Text.StartsWith('$taskRun.Verdict=if')},$true)
if (-not $verdict) {throw 'Verdict expression missing'}
Invoke-Expression $verdict.Extent.Text
if ($taskRun.Verdict -ne 'COMMON_PASS_LIVE_TIMEOUT_GAPS') {throw 'Timeout became a passing gate'}
$condition=$ast.Find({param($node) $node -is [System.Management.Automation.Language.IfStatementAst] -and $node.Extent.Text.StartsWith('if (@($taskRun.Cells | Where-Object {$_.Key -in')},$true)
if (-not $condition) {throw 'Common timeout guard missing'}
$taskRun.Cells[0].Key='common-base-10000'
foreach ($rows in @(10000,100000)) {
 $actual=Invoke-Expression $condition.Clauses[0].Item1.Extent.Text
 if ($actual -ne ($rows -eq 10000)) {throw 'Common timeout row binding changed'}
}
# Resume checks must accept JSON-parsed dates and reject a reused PID identity.
$child=Start-Process -FilePath $binary -ArgumentList @('-NoLogo','-NoProfile','-Command','Start-Sleep -Seconds 10') -WorkingDirectory $taskWorkspace -WindowStyle Hidden -PassThru
try {
 $null=$child.Handle
 $cell=[pscustomobject]@{Key='resume';StartedUtc=[DateTime]::UtcNow.AddSeconds(-2).ToString('o');ProcessStartedUtc=$child.StartTime.ToUniversalTime().AddSeconds(1).ToString('o');Pid=$child.Id;State='Running';FinishedUtc=$null;ExitCode=$null;Outcome=$null;SampledProcessPeakRss=$null;Stdout=(Join-Path $taskRoot 'resume.log');Stderr=(Join-Path $taskRoot 'resume.stderr.log')}
 $taskRun.Cells=@($cell | ConvertTo-Json | ConvertFrom-Json)
 $rejected=$false
 try {Run-Cell 'resume' $binary @('unused')} catch {
  if ($_.Exception.Message -notmatch 'ownership mismatch') {throw}
  $rejected=$true
 }
 if (-not $rejected -or $child.HasExited) {throw 'Mismatched identity was admitted or killed'}
 $taskRun.Cells[0].ProcessStartedUtc=$child.StartTime.ToUniversalTime()
 Run-Cell 'resume' $binary @('unused')
 if ($taskRun.Cells[0].Outcome -ne 'TimedOut') {throw 'Parsed-date resume failed'}
} finally {
 if (-not $child.HasExited) {$child.Kill($true);$child.WaitForExit()}
 $child.Dispose()
}
$taskRun.Cells[0].Outcome='Invalid'
$taskRun.Cells[0].ExitCode=1
$rejected=$false
try {Run-Cell 'resume' $binary @('unused')} catch {
 if ($_.Exception.Message -notmatch 'Recorded cell failure') {throw}
 $rejected=$true
}
if (-not $rejected) {throw 'Invalid completed cell was skipped'}
$sequential=@(1..13 | ForEach-Object {[pscustomobject]@{Number=$_}})
Assert-Ledger $sequential
$sequential[6].Number=99
$rejected=$false
try {Assert-Ledger $sequential} catch {$rejected=$true}
if (-not $rejected) {throw 'Invalid sequence accepted'}
$rejected=$false
try {Assert-Ledger @()} catch {$rejected=$true}
if (-not $rejected) {throw 'Empty ledger accepted'}
# The default runner must reject inherited experimental budgets before creating its ledger.
$budgetGuard=$ast.Find({param($node) $node -is [System.Management.Automation.Language.IfStatementAst] -and $node.Extent.Text.StartsWith('if (Test-Path Env:FG28_BUDGET)')},$true)
if (-not $budgetGuard) {throw 'Default budget guard missing'}
$priorBudget=[System.Environment]::GetEnvironmentVariable('FG28_BUDGET')
try {
 Remove-Item Env:FG28_BUDGET -ErrorAction SilentlyContinue
 Invoke-Expression $budgetGuard.Extent.Text
 $env:FG28_BUDGET='large-work'
 $rejected=$false
 try {Invoke-Expression $budgetGuard.Extent.Text} catch {$rejected=$true}
 if (-not $rejected) {throw 'Budgeted workload entered default gate'}
} finally {
 if ($null -eq $priorBudget) {Remove-Item Env:FG28_BUDGET -ErrorAction SilentlyContinue}
 else {$env:FG28_BUDGET=$priorBudget}
}
'PASS: timeout/negative verdict, owned cleanup, parsed-date resume, PID mismatch, failed-cell rejection, unlimited sequential ledger, default budget guard.'
