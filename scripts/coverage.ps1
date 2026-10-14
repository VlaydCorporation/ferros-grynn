param([string]$Toolchain = 'stable')

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path $PSScriptRoot -Parent
Set-Location $repoRoot
$llvmTools = & rustup component list --toolchain $Toolchain --installed
if ($LASTEXITCODE -ne 0 -or -not ($llvmTools -match '^llvm-tools')) {
    throw "Install llvm-tools-preview for $Toolchain before measuring coverage."
}
& cargo "+$Toolchain" llvm-cov --version
if ($LASTEXITCODE -ne 0) { throw 'cargo-llvm-cov is required.' }
New-Item -ItemType Directory -Path 'target/coverage' -Force | Out-Null
# --no-report retains profiles, so clear the coverage workspace before this run.
& cargo "+$Toolchain" llvm-cov clean --workspace
if ($LASTEXITCODE -ne 0) { throw 'Coverage profile cleanup failed.' }
& cargo "+$Toolchain" llvm-cov --locked -p ferros-grynn --no-default-features --lib --tests --no-report -- --test-threads=1
if ($LASTEXITCODE -ne 0) { throw 'Coverage test execution failed; no percentage will be published.' }
# Dedicated tests and other workspace packages are outside this root-source metric.
$exclude = '[/\\](tests|crates|ferros-grynn-bench)[/\\]|[/\\]tests\.rs$'
& cargo "+$Toolchain" llvm-cov report --json --summary-only --ignore-filename-regex $exclude --output-path target/coverage/summary.json
if ($LASTEXITCODE -ne 0) { throw 'JSON coverage export failed.' }
& cargo "+$Toolchain" llvm-cov report --html --ignore-filename-regex $exclude --output-dir target/coverage/html
if ($LASTEXITCODE -ne 0) { throw 'HTML coverage export failed.' }
& (Join-Path $PSScriptRoot 'coverage-badge.ps1') -Report target/coverage/summary.json -Output target/coverage/coverage.json
if ($env:GITHUB_STEP_SUMMARY) {
    $badge = Get-Content -LiteralPath target/coverage/coverage.json -Raw | ConvertFrom-Json
    "### Windows core coverage`n`n$($badge.message) of lines ($($badge.covered)/$($badge.lines)).`n`nScope: root sources; no default features; unit and integration tests; dedicated test files and other workspace packages excluded. Inline unit-test sections can contribute to the line metric. Doctests and optional features are not part of this percentage." |
        Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY
}
