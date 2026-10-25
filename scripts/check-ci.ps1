# Small contract checks for scripts, coverage data, documentation and publication.
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path $PSScriptRoot -Parent
Set-Location $repoRoot
$fixtureRoot = Join-Path $repoRoot ('target/ci-check-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fixtureRoot -Force | Out-Null

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

foreach ($script in Get-ChildItem -LiteralPath $PSScriptRoot -Filter '*.ps1') {
    $parseErrors = $null
    [Management.Automation.Language.Parser]::ParseFile($script.FullName, [ref]$null, [ref]$parseErrors) | Out-Null
    Assert-True ($parseErrors.Count -eq 0) "PowerShell parse error in $($script.Name): $parseErrors"
}

# Dedicated test files must not inflate either production line metric.
$coverageScript = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'coverage.ps1') -Raw
$exclusions = @([regex]::Matches($coverageScript, '(?m)^\s*\$exclude = ''([^'']+)''') | ForEach-Object { $_.Groups[1].Value })
Assert-True ($exclusions.Count -eq 2) 'Expected separate workspace and core coverage scopes.'
foreach ($pattern in $exclusions) {
    foreach ($testFile in @('crates/fg-engine/src/gql/analysis/layout_tests.rs', 'crates/fg-engine/src/storage/changefeed/tests.rs', 'crates/fg-runtime/src/values/runtime/tests.rs', 'tests/system.rs', 'crates\fg-engine\src\gql\analysis\layout_tests.rs')) {
        Assert-True ($testFile -match $pattern) "Test code enters production coverage: $testFile"
    }
    foreach ($productionFile in @('src/lib.rs', 'crates/fg-memory/src/lib.rs', 'crates/fg-syntax/src/syn/mod.rs', 'crates/fg-runtime/src/graph/mod.rs', 'crates/fg-value-codec/src/values/storage/codec/mod.rs', 'crates/fg-engine/src/gql/analysis/mod.rs')) {
        Assert-True (Test-Path -LiteralPath $productionFile) "Missing core owner fixture: $productionFile"
        Assert-True ($productionFile -notmatch $pattern) "Production code was excluded from coverage: $productionFile"
        Assert-True ($productionFile.Replace('/', '\') -notmatch $pattern) "Windows production path was excluded: $productionFile"
    }
    Assert-True ('crates/fg-test-support/src/lib.rs' -match $pattern) 'Test helpers enter production coverage.'
}
Assert-True ('crates/ferros-grynn-macros/src/lib.rs' -notmatch $exclusions[0]) 'Workspace coverage excludes a workspace package.'
Assert-True ('crates/ferros-grynn-macros/src/lib.rs' -match $exclusions[1]) 'Core coverage includes a separate workspace package.'
foreach ($package in @('fg-columnar', 'fg-gql-status', 'fg-storage-engine-prototypes')) {
    $source = "crates/$package/src/lib.rs"
    Assert-True ($source -notmatch $exclusions[0]) "Workspace coverage excludes $package."
    Assert-True ($source -match $exclusions[1]) "Core coverage includes $package."
}
$argumentArrays = @([regex]::Matches($coverageScript, '\$testArguments = @\(([^)]*)\)'))
Assert-True ($argumentArrays.Count -eq 2) 'Expected separate workspace and core test selections.'
$corePackages = @([regex]::Matches($argumentArrays[1].Groups[1].Value, '''-p'',\s*''([^'']+)''') | ForEach-Object { $_.Groups[1].Value } | Sort-Object)
$expectedCorePackages = @('ferros-grynn', 'fg-memory', 'fg-syntax', 'fg-runtime', 'fg-value-codec', 'fg-engine') | Sort-Object
Assert-True (($corePackages -join ',') -eq ($expectedCorePackages -join ',')) 'Core coverage must test every moved production owner.'

$report = Join-Path $fixtureRoot 'summary.json'
$badgePath = Join-Path $fixtureRoot 'coverage.json'
$generator = Join-Path $PSScriptRoot 'coverage-badge.ps1'
$originalCulture = [Threading.Thread]::CurrentThread.CurrentCulture
try {
    [Threading.Thread]::CurrentThread.CurrentCulture = [Globalization.CultureInfo]::GetCultureInfo('ru-RU')
    foreach ($case in @(
        @{ count = 10; covered = 8; expected = '80.0%'; color = 'brightgreen' },
        @{ count = 3; covered = 2; expected = '66.7%'; color = 'yellow' },
        @{ count = 10; covered = 0; expected = '0.0%'; color = 'orange' }
    )) {
        @{ type = 'llvm.coverage.json.export'; data = @(@{ totals = @{ lines = @{ count = $case.count; covered = $case.covered } } }) } |
            ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $report
        & $generator -Report $report -Output $badgePath -Commit 'fixture'
        $badge = Get-Content -LiteralPath $badgePath -Raw | ConvertFrom-Json
        Assert-True ($badge.message -eq $case.expected -and $badge.color -eq $case.color -and $badge.commit -eq 'fixture') 'Badge counts, rounding, culture or provenance mismatch.'
    }
    $workspaceScope = 'Windows; workspace; default features; library, binary and integration tests'
    & $generator -Report $report -Output $badgePath -Commit 'fixture' -Scope $workspaceScope
    $badge = Get-Content -LiteralPath $badgePath -Raw | ConvertFrom-Json
    Assert-True ($badge.scope -eq $workspaceScope -and $badge.message -eq '0.0%') 'Workspace badge scope or totals mismatch.'
    foreach ($badLines in @(@{ count = 0; covered = 0 }, @{ count = 2; covered = 3 }, @{ count = 2; covered = -1 }, @{ count = 2.5; covered = 1 }, @{})) {
        @{ type = 'llvm.coverage.json.export'; data = @(@{ totals = @{ lines = $badLines } }) } |
            ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $report
        $rejected = $false
        try { & $generator -Report $report -Output $badgePath -Commit 'fixture' } catch { $rejected = $true }
        Assert-True $rejected 'Invalid coverage totals were accepted.'
    }
    '{"type":"other","data":[]}' | Set-Content -LiteralPath $report
    $rejected = $false
    try { & $generator -Report $report -Output $badgePath -Commit 'fixture' } catch { $rejected = $true }
    Assert-True $rejected 'Invalid LLVM report type was accepted.'
} finally { [Threading.Thread]::CurrentThread.CurrentCulture = $originalCulture }

$query = (Get-Content -LiteralPath examples/quick_look.gql -Raw).Replace("`r`n", "`n").Trim()
$versionMatch = [regex]::Match((Get-Content Cargo.toml -Raw), '(?ms)^\[package\].*?^version\s*=\s*"([^"]+)"')
Assert-True $versionMatch.Success 'Package version not found.'
foreach ($readme in @('README.md', 'README.ru.md')) {
    $content = (Get-Content -LiteralPath $readme -Raw).Replace("`r`n", "`n")
    $sql = [regex]::Match($content, '(?s)```sql\n(.*?)\n```').Groups[1].Value.Trim()
    Assert-True ($sql -eq $query) "SQL example drift in $readme."
    Assert-True ($content.Contains('version-' + $versionMatch.Groups[1].Value + '-')) "Version badge drift in $readme."
}
foreach ($document in @('README.md', 'README.ru.md', 'CONTRIBUTING.md', 'CONTRIBUTING.ru.md', 'CLA.md', 'AGENTS.md')) {
    $content = Get-Content -LiteralPath $document -Raw
    $links = @([regex]::Matches($content, '(?:href|src|srcset)="([^"#]+)"')) + @([regex]::Matches($content, '\]\(([^)#]+)(?:#[^)]*)?\)'))
    foreach ($match in $links) {
        $link = $match.Groups[1].Value
        if ($link -match '^https?://') { continue }
        Assert-True (Test-Path -LiteralPath (Join-Path $repoRoot $link)) "Broken local link in ${document}: $link"
    }
}
foreach ($svg in Get-ChildItem -LiteralPath img/readme -Filter '*.svg') { [xml](Get-Content -LiteralPath $svg.FullName -Raw) | Out-Null }

# Use an entirely local origin to exercise publication without touching GitHub.
$origin = Join-Path $fixtureRoot 'origin.git'
$checkout = Join-Path $fixtureRoot 'checkout'
function Test-Git([string[]]$Arguments) {
    $result = & git -c "safe.directory=$checkout" @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Fixture git failed: $($Arguments -join ' ')" }
    return $result
}
Test-Git -Arguments @('init', '--bare', $origin) | Out-Null
Test-Git -Arguments @('init', '--initial-branch=mvp', $checkout) | Out-Null
Test-Git -Arguments @('-C', $checkout, 'config', 'user.name', 'CI fixture') | Out-Null
Test-Git -Arguments @('-C', $checkout, 'config', 'user.email', 'fixture@example.invalid') | Out-Null
Test-Git -Arguments @('-C', $checkout, 'remote', 'add', 'origin', $origin) | Out-Null
'source' | Set-Content -LiteralPath (Join-Path $checkout 'source.txt')
Test-Git -Arguments @('-C', $checkout, 'add', 'source.txt') | Out-Null
Test-Git -Arguments @('-C', $checkout, 'commit', '-m', 'Source fixture') | Out-Null
Test-Git -Arguments @('-C', $checkout, 'push', 'origin', 'mvp') | Out-Null
$head = Test-Git -Arguments @('-C', $checkout, 'rev-parse', 'HEAD')
# Check idempotent hook installation and refusal to overwrite a different hook.
New-Item -ItemType Directory -Path (Join-Path $checkout 'scripts'), (Join-Path $checkout '.githooks') -Force | Out-Null
$fixtureInstaller = Join-Path $checkout 'scripts/install-hooks.ps1'
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'install-hooks.ps1') -Destination $fixtureInstaller
Copy-Item -LiteralPath (Join-Path $repoRoot '.githooks/pre-commit') -Destination (Join-Path $checkout '.githooks/pre-commit')
foreach ($attempt in 1..2) {
    & pwsh -NoLogo -NoProfile -File $fixtureInstaller
    Assert-True ($LASTEXITCODE -eq 0) 'Hook installation or repeat installation failed.'
}
$installedHook = Join-Path $checkout '.git/hooks/pre-commit'
Assert-True ((Get-FileHash -LiteralPath $installedHook).Hash -eq (Get-FileHash -LiteralPath (Join-Path $repoRoot '.githooks/pre-commit')).Hash) 'Installed hook differs from tracked hook.'
'existing custom hook' | Set-Content -LiteralPath $installedHook
$hookBefore = (Get-FileHash -LiteralPath $installedHook).Hash
& pwsh -NoLogo -NoProfile -File $fixtureInstaller 2> (Join-Path $fixtureRoot 'expected-hook-rejection.log')
Assert-True ($LASTEXITCODE -ne 0 -and (Get-FileHash -LiteralPath $installedHook).Hash -eq $hookBefore) 'A custom pre-commit hook was overwritten.'
New-Item -ItemType Directory -Path (Join-Path $checkout 'target/coverage') -Force | Out-Null
$fixtureBadge = Join-Path $checkout 'target/coverage/coverage.json'
@{ schemaVersion = 1; label = 'coverage'; message = '80.0%'; color = 'brightgreen'; lines = 10; covered = 8; commit = $head } |
    ConvertTo-Json | Set-Content -LiteralPath $fixtureBadge
'dirty source' | Set-Content -LiteralPath (Join-Path $checkout 'source.txt')
$statusBefore = Test-Git -Arguments @('-C', $checkout, 'status', '--porcelain')
$publisher = Join-Path $PSScriptRoot 'publish-coverage.ps1'
$oldSha = $env:GITHUB_SHA
try {
    $env:GITHUB_SHA = $head
    Push-Location $checkout
    try {
        & pwsh -NoLogo -NoProfile -File $publisher -State success
        Assert-True ($LASTEXITCODE -eq 0) 'Initial badge publication failed.'
        $published = (Test-Git -Arguments @('show', 'refs/remotes/origin/badges:coverage.json')) | ConvertFrom-Json
        Assert-True ($published.message -eq '80.0%' -and $published.commit -eq $head) 'Published badge mismatch.'
        $badgeHead = Test-Git -Arguments @('rev-parse', 'refs/remotes/origin/badges')
        & pwsh -NoLogo -NoProfile -File $publisher -State success
        Assert-True ($LASTEXITCODE -eq 0) 'Repeat badge publication failed.'
        Assert-True ((Test-Git -Arguments @('rev-parse', 'refs/remotes/origin/badges')) -eq $badgeHead) 'Identical badge created an unnecessary commit.'
        $stale = Get-Content -LiteralPath $fixtureBadge -Raw | ConvertFrom-Json
        $stale.commit = 'stale-fixture'
        $stale | ConvertTo-Json | Set-Content -LiteralPath $fixtureBadge
        & pwsh -NoLogo -NoProfile -File $publisher -State success 2> (Join-Path $fixtureRoot 'expected-rejection.log')
        Assert-True ($LASTEXITCODE -ne 0) 'Stale badge was accepted.'
        $env:GITHUB_SHA = 'old-run-fixture'
        & pwsh -NoLogo -NoProfile -File $publisher -State failure
        Assert-True ($LASTEXITCODE -eq 0) 'Obsolete workflow was not skipped.'
        Assert-True ((Test-Git -Arguments @('rev-parse', 'refs/remotes/origin/badges')) -eq $badgeHead) 'Obsolete workflow overwrote the current badge.'
        $env:GITHUB_SHA = $head
        & pwsh -NoLogo -NoProfile -File $publisher -State failure
        Assert-True ($LASTEXITCODE -eq 0) 'Failure badge publication failed.'
        $failed = (Test-Git -Arguments @('show', 'refs/remotes/origin/badges:coverage.json')) | ConvertFrom-Json
        Assert-True ($failed.message -eq 'unavailable') 'Failure retained a stale percentage.'
        Assert-True ((Test-Git -Arguments @('rev-parse', 'HEAD')) -eq $head) 'Publisher changed source HEAD.'
        Assert-True (((Test-Git -Arguments @('status', '--porcelain')) -join "`n") -eq ($statusBefore -join "`n")) 'Publisher changed source files or index.'
    } finally { Pop-Location }
} finally { $env:GITHUB_SHA = $oldSha }
Write-Host 'CI scripts, coverage boundary cases, documentation and isolated badge publication: PASS.'
