param(
    [Parameter(Mandatory)][string]$Report,
    [Parameter(Mandatory)][string]$Output,
    [string]$Commit = $env:GITHUB_SHA,
    [string]$Scope = 'Windows; ferros-grynn; no-default-features; lib and integration tests'
)

# Reject empty or inconsistent reports rather than presenting fabricated coverage.
$ErrorActionPreference = 'Stop'
$reportData = Get-Content -LiteralPath $Report -Raw | ConvertFrom-Json
if ($reportData.type -ne 'llvm.coverage.json.export' -or @($reportData.data).Count -ne 1) {
    throw 'Expected a single LLVM coverage export.'
}
$lines = $reportData.data[0].totals.lines
if ($null -eq $lines.count -or $null -eq $lines.covered) { throw 'Missing LLVM line totals.' }
$count = [long]$lines.count
$covered = [long]$lines.covered
if ($count -ne $lines.count -or $covered -ne $lines.covered -or $count -le 0 -or $covered -lt 0 -or $covered -gt $count) { throw 'Invalid LLVM line totals.' }
$percent = 100.0 * $covered / $count
$color = if ($percent -ge 80) { 'brightgreen' } elseif ($percent -ge 60) { 'yellow' } else { 'orange' }
$message = $percent.ToString('F1', [Globalization.CultureInfo]::InvariantCulture) + '%'
$badge = [ordered]@{
    schemaVersion = 1
    label = 'coverage'
    message = $message
    color = $color
    lines = $count
    covered = $covered
    commit = $Commit
    scope = $Scope
}
New-Item -ItemType Directory -Path (Split-Path ([IO.Path]::GetFullPath($Output)) -Parent) -Force | Out-Null
$badge | ConvertTo-Json | Set-Content -LiteralPath $Output -Encoding utf8NoBOM
Write-Host "Line coverage: $message ($covered / $count)"
