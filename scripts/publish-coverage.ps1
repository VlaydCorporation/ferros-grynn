param([Parameter(Mandatory)][ValidateSet('success', 'failure', 'cancelled', 'skipped')][string]$State)

# Publish only generated badge data on an isolated branch, never on the source branch.
$ErrorActionPreference = 'Stop'
function Invoke-Git {
    param([string[]]$Arguments)
    $result = & git @Arguments
    if ($LASTEXITCODE -ne 0) { throw "git $($Arguments -join ' ') failed." }
    return $result
}

# Re-running an old workflow must not replace the current branch's measurement.
$sourceHead = Invoke-Git -Arguments @('ls-remote', '--heads', 'origin', 'refs/heads/mvp')
if (-not $env:GITHUB_SHA -or -not $sourceHead) { throw 'Cannot identify the source revision for publication.' }
if (($sourceHead -split '\s+')[0] -ne $env:GITHUB_SHA) {
    Write-Host 'Skipped badge publication: this run no longer represents the tip of mvp.'
    exit 0
}
$badgePath = 'target/coverage/coverage.json'
if ($State -eq 'success') {
    $badge = Get-Content -LiteralPath $badgePath -Raw | ConvertFrom-Json
    if ($badge.schemaVersion -ne 1 -or $badge.commit -ne $env:GITHUB_SHA -or $badge.lines -le 0 -or $badge.covered -lt 0 -or $badge.covered -gt $badge.lines) {
        throw 'Refusing a missing, stale or invalid coverage badge.'
    }
} else {
    New-Item -ItemType Directory -Path 'target/coverage' -Force | Out-Null
    @{ schemaVersion = 1; label = 'coverage'; message = 'unavailable'; color = 'lightgrey'; commit = $env:GITHUB_SHA } |
        ConvertTo-Json | Set-Content -LiteralPath $badgePath -Encoding utf8NoBOM
}

$branch = 'refs/heads/badges'
$remoteHead = Invoke-Git -Arguments @('ls-remote', '--heads', 'origin', $branch)
$parent = $null
if ($remoteHead) {
    Invoke-Git -Arguments @('fetch', '--no-tags', 'origin', $branch) | Out-Null
    $parent = Invoke-Git -Arguments @('rev-parse', 'FETCH_HEAD')
}
$env:GIT_AUTHOR_NAME = 'github-actions[bot]'
$env:GIT_AUTHOR_EMAIL = '41898282+github-actions[bot]@users.noreply.github.com'
$env:GIT_COMMITTER_NAME = $env:GIT_AUTHOR_NAME
$env:GIT_COMMITTER_EMAIL = $env:GIT_AUTHOR_EMAIL
# An independent index preserves the working tree and any other badge files.
$env:GIT_INDEX_FILE = Join-Path ([IO.Path]::GetTempPath()) ('fg-badge-' + [Guid]::NewGuid().ToString('N') + '.index')
if ($parent) { Invoke-Git -Arguments @('read-tree', $parent) | Out-Null } else { Invoke-Git -Arguments @('read-tree', '--empty') | Out-Null }
$blob = Invoke-Git -Arguments @('hash-object', '-w', '--', $badgePath)
Invoke-Git -Arguments @('update-index', '--add', '--cacheinfo', "100644,$blob,coverage.json") | Out-Null
$tree = Invoke-Git -Arguments @('write-tree')
if ($parent -and $tree -eq (Invoke-Git -Arguments @('rev-parse', "$parent`^{tree}"))) { exit 0 }
$commitArgs = @('commit-tree', $tree, '-m', "Update coverage for $env:GITHUB_SHA")
if ($parent) { $commitArgs += @('-p', $parent) }
$commit = Invoke-Git -Arguments $commitArgs
$sourceHead = Invoke-Git -Arguments @('ls-remote', '--heads', 'origin', 'refs/heads/mvp')
if (($sourceHead -split '\s+')[0] -ne $env:GITHUB_SHA) {
    Write-Host 'Skipped badge publication: mvp advanced while preparing the update.'
    exit 0
}
Invoke-Git -Arguments @('push', 'origin', "${commit}:${branch}")
