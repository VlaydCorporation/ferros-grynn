# Installs only this repository's pre-commit hook; other hooks remain in place.
$ErrorActionPreference = 'Stop'
$repoRoot = (Split-Path $PSScriptRoot -Parent).Replace('\', '/')
$source = Join-Path $repoRoot '.githooks/pre-commit'
$hookPath = & git -c "safe.directory=$repoRoot" -C $repoRoot rev-parse --git-path hooks/pre-commit
if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve the Git hook directory.' }
if (-not [IO.Path]::IsPathRooted($hookPath)) { $hookPath = Join-Path $repoRoot $hookPath }
if (Test-Path -LiteralPath $hookPath) {
    if ((Get-FileHash -LiteralPath $hookPath).Hash -ne (Get-FileHash -LiteralPath $source).Hash) {
        throw "An existing pre-commit hook differs from this one. Integrate 'just pre-commit' into $hookPath before retrying."
    }
} else {
    New-Item -ItemType Directory -Path (Split-Path $hookPath -Parent) -Force | Out-Null
    Copy-Item -LiteralPath $source -Destination $hookPath
}
if (-not $IsWindows) {
    & chmod +x $hookPath
    if ($LASTEXITCODE -ne 0) { throw 'Cannot make the hook executable.' }
}
Write-Host "Installed pre-commit hook: $hookPath"
