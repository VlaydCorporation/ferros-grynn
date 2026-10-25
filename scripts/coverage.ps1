param(
    [string]$Toolchain = 'stable',
    [ValidateSet('Core', 'Workspace')][string]$Scope = 'Core'
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path $PSScriptRoot -Parent
Set-Location $repoRoot
$llvmTools = & rustup component list --toolchain $Toolchain --installed
if ($LASTEXITCODE -ne 0 -or -not ($llvmTools -match '^llvm-tools')) {
    throw "Install llvm-tools-preview for $Toolchain before measuring coverage."
}
& cargo "+$Toolchain" llvm-cov --version
if ($LASTEXITCODE -ne 0) { throw 'cargo-llvm-cov is required.' }
if ($Scope -eq 'Workspace') {
    $reportRoot = 'target/coverage-workspace'
    $testArguments = @('--workspace', '--lib', '--bins', '--tests')
    $exclude = '(^|[/\\])tests[/\\]|(^|[/\\])([^/\\]*_)?tests\.rs$|(^|[/\\])crates[/\\]fg-test-support[/\\]'
    $description = 'Windows; workspace; default features; library, binary and integration tests'
} else {
    $reportRoot = 'target/coverage'
    $testArguments = @('-p', 'ferros-grynn', '-p', 'fg-memory', '-p', 'fg-syntax', '-p', 'fg-runtime', '-p', 'fg-value-codec', '-p', 'fg-engine', '--no-default-features', '--lib', '--tests')
    $exclude = '(^|[/\\])(tests|ferros-grynn-bench)[/\\]|(^|[/\\])([^/\\]*_)?tests\.rs$|(^|[/\\])crates[/\\](?!(?:fg-memory|fg-syntax|fg-runtime|fg-value-codec|fg-engine)[/\\])'
    $description = 'Windows; facade and core owners; no-default-features; lib and integration tests'
}
New-Item -ItemType Directory -Path $reportRoot -Force | Out-Null
# --no-report retains profiles, so clear the coverage workspace before this run.
& cargo "+$Toolchain" llvm-cov clean --workspace
if ($LASTEXITCODE -ne 0) { throw 'Coverage profile cleanup failed.' }
& cargo "+$Toolchain" llvm-cov --locked @testArguments --no-report -- --test-threads=1
if ($LASTEXITCODE -ne 0) { throw 'Coverage test execution failed; no percentage will be published.' }
# Dedicated test files are excluded; inline unit tests can contribute to totals.
& cargo "+$Toolchain" llvm-cov report --json --summary-only --ignore-filename-regex $exclude --output-path (Join-Path $reportRoot 'summary.json')
if ($LASTEXITCODE -ne 0) { throw 'JSON coverage export failed.' }
& cargo "+$Toolchain" llvm-cov report --html --ignore-filename-regex $exclude --output-dir (Join-Path $reportRoot 'html')
if ($LASTEXITCODE -ne 0) { throw 'HTML coverage export failed.' }
& (Join-Path $PSScriptRoot 'coverage-badge.ps1') -Report (Join-Path $reportRoot 'summary.json') -Output (Join-Path $reportRoot 'coverage.json') -Scope $description
if ($env:GITHUB_STEP_SUMMARY) {
    $badge = Get-Content -LiteralPath (Join-Path $reportRoot 'coverage.json') -Raw | ConvertFrom-Json
    "### Windows $Scope coverage`n`n$($badge.message) of lines ($($badge.covered)/$($badge.lines)).`n`nScope: $description. Dedicated test files are excluded; inline unit tests can contribute to the line metric. Doctests and additional optional features are outside this percentage." |
        Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY
}
