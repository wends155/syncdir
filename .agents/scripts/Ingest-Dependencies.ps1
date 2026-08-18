<#
.SYNOPSIS
    Fetch dependency READMEs from crates.io across multiple repos for knowledge-rag ingestion.
.DESCRIPTION
    Reads Cargo.toml files from specified repo paths, extracts dependency names,
    fetches READMEs from crates.io API, saves them as tagged markdown files,
    and invokes ingest_helper.py to index them into knowledge-rag.
.PARAMETER Repos
    Array of repository paths containing Cargo.toml files. Defaults to current directory.
.PARAMETER OutputDir
    Directory to save fetched READMEs. Defaults to $env:APPDATA/knowledge-rag/staging/deps/rust.
.EXAMPLE
    .\Ingest-Dependencies.ps1 -Repos "C:\Users\WSALIGAN\code\syncdir", "C:\Users\WSALIGAN\code\ctapi-rs"
#>
[CmdletBinding()]
param(
    [string[]]$Repos = @("."),
    [string]$OutputDir = "$env:APPDATA\knowledge-rag\staging\deps\rust"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Create output directory
if (-not (Test-Path $OutputDir)) {
    New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
}

$allDeps = [System.Collections.Generic.HashSet[string]]::new()

foreach ($repoPath in $Repos) {
    $resolvedPath = Resolve-Path $repoPath -ErrorAction SilentlyContinue
    if (-not $resolvedPath) {
        Write-Warning "Repository path not found: $repoPath"
        continue
    }

    $cargoPath = Join-Path $resolvedPath.Path "Cargo.toml"
    if (-not (Test-Path $cargoPath)) {
        Write-Warning "Cargo.toml not found at: $cargoPath"
        continue
    }

    Write-Host "Scanning Cargo.toml at: $cargoPath" -ForegroundColor Cyan
    $content = Get-Content $cargoPath -Raw
    $inDeps = $false

    foreach ($line in ($content -split "`n")) {
        $trimmed = $line.Trim()
        if ($trimmed -match '^\[dependencies\]' -or $trimmed -match '^\[dev-dependencies\]') {
            $inDeps = $true
            continue
        }
        if ($trimmed -match '^\[' -and $inDeps) {
            $inDeps = $false
            continue
        }
        if ($inDeps -and $trimmed -match '^([a-zA-Z0-9_-]+)\s*=') {
            $crateName = $Matches[1]
            # Ignore workspace/path/git deps without crates.io names if needed
            [void]$allDeps.Add($crateName)
        }
    }
}

Write-Host "`nFound $($allDeps.Count) unique dependencies across all repositories:" -ForegroundColor Green
Write-Host ($allDeps -join ', ') "`n"

# Fetch READMEs from crates.io
$headers = @{
    'User-Agent' = 'syncdir-knowledge-rag-ingester/0.1 (contact: wsaligan)'
    'Accept'     = 'application/json'
}

$fetchedCount = 0

foreach ($crate in $allDeps) {
    $outFile = Join-Path $OutputDir "$crate.md"
    if (Test-Path $outFile) {
        Write-Host "Skipping $crate (already exists in staging)" -ForegroundColor Gray
        continue
    }

    Write-Host "Fetching README for: $crate ..." -NoNewline
    try {
        $apiUrl = "https://crates.io/api/v1/crates/$crate"
        $response = Invoke-RestMethod -Uri $apiUrl -Headers $headers
        $version = $response.crate.max_stable_version
        if (-not $version) { $version = $response.crate.max_version }

        # Fetch the README
        $readmeUrl = "https://crates.io/api/v1/crates/$crate/$version/readme"
        $readme = Invoke-WebRequest -Uri $readmeUrl -Headers $headers -UseBasicParsing
        $readmeContent = $readme.Content

        if (-not $readmeContent -or $readmeContent.Trim().Length -eq 0) {
            Write-Host " NO README AVAILABLE ($version)" -ForegroundColor Yellow
            continue
        }

        # Save with YAML frontmatter header
        $header = @"
---
crate: $crate
version: $version
source: crates.io
fetched: $(Get-Date -Format 'yyyy-MM-dd')
namespace: deps/rust
---

"@
        Set-Content -Path $outFile -Value ($header + $readmeContent) -Encoding UTF8
        Write-Host " OK ($version)" -ForegroundColor Green
        $fetchedCount++
    }
    catch {
        Write-Host " FAILED: $($_.Exception.Message)" -ForegroundColor Red
    }

    # Rate limiting — crates.io policy asks for max 1 req/sec
    Start-Sleep -Seconds 1
}

Write-Host "`nFetch complete. $fetchedCount new README(s) saved to: $OutputDir" -ForegroundColor Green

# Trigger Python helper for auto-ingestion
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ingestHelper = Join-Path $scriptDir "ingest_helper.py"

if (Test-Path $ingestHelper) {
    Write-Host "Triggering auto-ingestion via ingest_helper.py ..." -ForegroundColor Cyan
    python $ingestHelper $OutputDir
} else {
    Write-Warning "ingest_helper.py not found at $ingestHelper. Manual ingestion required."
}
