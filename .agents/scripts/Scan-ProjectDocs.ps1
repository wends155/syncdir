<#
.SYNOPSIS
[DEPRECATED] This script has been archived in Phase 4 of the Skills Migration.
Its functionality is now handled natively by TARS workflows and .gemini/skills/.
Do not use this script for new development.
#>
<#
# ⚠️ REFERENCE ONLY — Workflows now use inline auto-runnable commands.
# This script is retained as documentation of the procedure it implements.
# See .agent/workflows/update-doc.md, architecture.md for the current inline procedures.
.SYNOPSIS
    Scans a project for documentation state and completeness.

.DESCRIPTION
    Companion script for the /update-doc workflow. Extracts project metadata,
    doc coverage, and audits spec.md against doc-rules.md required sections.

    Modes:
      scan     - Extract current project state as a structured report.
      validate - Check completeness and exit with code 1 if required sections are missing.

.PARAMETER Mode
    The operation mode: 'scan' or 'validate'.

.PARAMETER RepoRoot
    Path to the project root. Defaults to the git repo root or current directory.

.EXAMPLE
    .\.agent\scripts\Scan-ProjectDocs.ps1 -Mode scan

.EXAMPLE
    .\.agent\scripts\Scan-ProjectDocs.ps1 -Mode validate
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('scan', 'validate')]
    [string]$Mode,

    [Parameter()]
    [string]$RepoRoot,

    [Parameter()]
    [ValidateSet('doc', 'arch', 'all')]
    [string]$Scope = 'doc'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Continue'

# --- Detect repo root ---
if (-not $RepoRoot) {
    $RepoRoot = git rev-parse --show-toplevel 2>$null
    if (-not $RepoRoot) {
        $RepoRoot = (Get-Location).Path
    }
}
$RepoRoot = $RepoRoot.Trim()

# --- Tracking for validate mode ---
$script:ValidationFailures = @()

function Add-ValidationResult {
    param([string]$Section, [bool]$Present, [string]$DocFile)
    if (-not $Present) {
        $script:ValidationFailures += "$DocFile § $Section"
    }
}

# ============================================================
# Header
# ============================================================
$Date = Get-Date -Format 'yyyy-MM-dd'
Write-Output "# Documentation Scan Report"
Write-Output "Generated: $Date"
Write-Output "Mode: $Mode"
Write-Output "Root: $RepoRoot"

# ============================================================
# Project Detection
# ============================================================
Write-Output ""
Write-Output "## Project Detection"
Write-Output ""

$Language = "Unknown"
$Edition = "N/A"
$ToolchainVersion = "N/A"
$ManifestFile = "None"

# Rust
$CargoToml = Join-Path $RepoRoot "Cargo.toml"
if (Test-Path $CargoToml) {
    $Language = "Rust"
    $ManifestFile = "Cargo.toml"
    $CargoContent = Get-Content $CargoToml -Raw

    # Extract edition
    if ($CargoContent -match 'edition\s*=\s*"(\d+)"') {
        $Edition = $Matches[1]
    }

    # Extract rust-version if specified
    if ($CargoContent -match 'rust-version\s*=\s*"([^"]+)"') {
        $ToolchainVersion = $Matches[1]
    }
    else {
        # Try rustc --version
        $RustcVersion = rustc --version 2>$null
        if ($RustcVersion -match 'rustc\s+(\S+)') {
            $ToolchainVersion = $Matches[1]
        }
    }
}

# Node.js / TypeScript
$PackageJson = Join-Path $RepoRoot "package.json"
if (Test-Path $PackageJson) {
    $Language = "JavaScript/TypeScript"
    $ManifestFile = "package.json"

    $TsConfig = Join-Path $RepoRoot "tsconfig.json"
    if (Test-Path $TsConfig) {
        $Language = "TypeScript"
        $TsContent = Get-Content $TsConfig -Raw
        if ($TsContent -match '"target"\s*:\s*"([^"]+)"') {
            $Edition = $Matches[1]
        }
    }

    $NodeVersion = node --version 2>$null
    if ($NodeVersion) { $ToolchainVersion = $NodeVersion.Trim() }
}

# Go
$GoMod = Join-Path $RepoRoot "go.mod"
if (Test-Path $GoMod) {
    $Language = "Go"
    $ManifestFile = "go.mod"
    $GoContent = Get-Content $GoMod -Raw

    if ($GoContent -match 'go\s+(\S+)') {
        $Edition = $Matches[1]
    }

    $GoVersion = go version 2>$null
    if ($GoVersion -match 'go(\S+)') {
        $ToolchainVersion = $Matches[1]
    }
}

Write-Output "| Field | Value |"
Write-Output "|-------|-------|"
Write-Output "| **Language** | $Language |"
Write-Output "| **Edition** | $Edition |"
Write-Output "| **Toolchain** | $ToolchainVersion |"
Write-Output "| **Manifest** | $ManifestFile |"

# ============================================================
# Project Layout
# ============================================================
Write-Output ""
Write-Output "## Project Layout"
Write-Output ""
Write-Output '```'

# Use tree if available, else fallback to Get-ChildItem
$TreeCmd = Get-Command tree -ErrorAction SilentlyContinue
if ($TreeCmd) {
    tree /F /A $RepoRoot 2>$null | Select-Object -First 60
}
else {
    Get-ChildItem -Path $RepoRoot -Recurse -Depth 3 -Name -ErrorAction SilentlyContinue |
        Where-Object { $_ -notmatch '(\.git|node_modules|target|\.agent)' } |
        Select-Object -First 60
}

Write-Output '```'

# ============================================================
# Dependencies
# ============================================================
Write-Output ""
Write-Output "## Dependencies"
Write-Output ""

if ($Language -eq "Rust" -and (Test-Path $CargoToml)) {
    Write-Output '```toml'
    $InDeps = $false
    foreach ($line in (Get-Content $CargoToml)) {
        if ($line -match '^\[.*dependencies') { $InDeps = $true; Write-Output $line; continue }
        if ($line -match '^\[' -and $InDeps) { $InDeps = $false }
        if ($InDeps -and $line.Trim()) { Write-Output $line }
    }
    Write-Output '```'
}
elseif (Test-Path $PackageJson) {
    Write-Output '```json'
    $PkgContent = Get-Content $PackageJson -Raw | ConvertFrom-Json
    if ($PkgContent.dependencies) {
        Write-Output '"dependencies":'
        $PkgContent.dependencies | ConvertTo-Json -Depth 1
    }
    if ($PkgContent.devDependencies) {
        Write-Output '"devDependencies":'
        $PkgContent.devDependencies | ConvertTo-Json -Depth 1
    }
    Write-Output '```'
}
elseif (Test-Path $GoMod) {
    Write-Output '```'
    Get-Content $GoMod
    Write-Output '```'
}
else {
    Write-Output "> No manifest file detected."
}

# ============================================================
# Toolchain Config
# ============================================================
Write-Output ""
Write-Output "## Toolchain Config"
Write-Output ""

$ConfigFiles = @(
    'rustfmt.toml', '.rustfmt.toml',
    'clippy.toml', '.clippy.toml',
    '.cargo/config.toml',
    '.eslintrc', '.eslintrc.json', '.eslintrc.js', 'eslint.config.js',
    '.prettierrc', '.prettierrc.json',
    'biome.json',
    'golangci.yml', '.golangci.yml'
)

$FoundAny = $false
foreach ($cf in $ConfigFiles) {
    $cfPath = Join-Path $RepoRoot $cf
    if (Test-Path $cfPath) {
        $FoundAny = $true
        Write-Output "### $cf"
        Write-Output ""
        Write-Output '```'
        Get-Content $cfPath
        Write-Output '```'
        Write-Output ""
    }
}
if (-not $FoundAny) {
    Write-Output "> No toolchain config files found."
}

# ============================================================
# Doc Coverage (Rust-specific)
# ============================================================
Write-Output ""
Write-Output "## Doc Coverage"
Write-Output ""

if ($Language -eq "Rust") {
    # Find all public items
    $RgAvailable = Get-Command rg -ErrorAction SilentlyContinue
    $SrcDir = Join-Path $RepoRoot "src"

    if (-not (Test-Path $SrcDir)) { $SrcDir = $RepoRoot }

    if ($RgAvailable) {
        $PublicItems = rg -n "^\s*pub\s+(fn|struct|enum|trait|type|mod|const|static)\s+" $SrcDir --glob "*.rs" 2>$null
        $DocComments = rg -c "^\s*///" $SrcDir --glob "*.rs" 2>$null
    }
    else {
        $PublicItems = Get-ChildItem -Path $SrcDir -Filter "*.rs" -Recurse -File -ErrorAction SilentlyContinue |
            Select-String -Pattern "^\s*pub\s+(fn|struct|enum|trait|type|mod|const|static)\s+" -ErrorAction SilentlyContinue
        $DocComments = @()
    }

    $PubCount = if ($PublicItems) { @($PublicItems).Count } else { 0 }
    $DocCount = 0
    if ($DocComments) {
        foreach ($dc in $DocComments) {
            if ($dc -match ':(\d+)$') { $DocCount += [int]$Matches[1] }
        }
    }

    $Coverage = if ($PubCount -gt 0) { [math]::Round(($DocCount / $PubCount) * 100, 1) } else { 0 }

    Write-Output "| Metric | Value |"
    Write-Output "|--------|-------|"
    Write-Output "| **Public items** | $PubCount |"
    Write-Output "| **Doc comment lines** | $DocCount |"
    Write-Output "| **Estimated coverage** | ~$Coverage% |"

    # List undocumented public items (heuristic: pub item without /// on preceding line)
    if ($PublicItems -and $PubCount -gt 0) {
        Write-Output ""
        Write-Output "### Undocumented Public Items (sample)"
        Write-Output ""
        Write-Output '```'
        $PublicItems | Select-Object -First 20
        Write-Output '```'
    }
}
else {
    Write-Output "> Doc coverage scanning is currently Rust-focused."
    Write-Output "> For other languages, use Narsil MCP ``find_symbols`` in the workflow."
}

# ============================================================
# architecture.md Section Audit (arch scope)
# ============================================================
if ($Scope -in @('arch', 'all')) {
    Write-Output ""
    Write-Output "## architecture.md Section Audit"
    Write-Output ""

    $ArchFile = Join-Path $RepoRoot "architecture.md"
    $ArchSections = @(
        'Project Overview',
        'Project Objectives',
        'Language & Runtime',
        'Project Layout',
        'Module Boundaries',
        'Dependency Direction',
        'Toolchain',
        'Error Handling',
        'Observability',
        'Testing Strategy',
        'Documentation Conventions',
        'Dependencies',
        'Architecture Diagrams',
        'Known Constraints',
        'Data Model'
    )

    if (Test-Path $ArchFile) {
        $ArchContent = Get-Content $ArchFile -Raw

        Write-Output "| # | Section | Status |"
        Write-Output "|---|---------|--------|"

        $i = 1
        foreach ($section in $ArchSections) {
            $Pattern = [regex]::Escape($section)
            $Found = $ArchContent -match "(?i)$Pattern"
            $Status = if ($Found) { "✅ Present" } else { "❌ Missing" }
            Write-Output "| $i | $section | $Status |"
            Add-ValidationResult -Section $section -Present $Found -DocFile "architecture.md"
            $i++
        }
    }
    else {
        Write-Output "> **architecture.md not found.** All 15 sections need to be created."
        foreach ($section in $ArchSections) {
            Add-ValidationResult -Section $section -Present $false -DocFile "architecture.md"
        }
    }
}

# ============================================================
# spec.md Drift Detection (doc scope)
# ============================================================
if ($Scope -in @('doc', 'all')) {
    Write-Output ""
    Write-Output "## spec.md Drift Detection"
    Write-Output ""

    $SpecFile = Join-Path $RepoRoot "spec.md"
    if (Test-Path $SpecFile) {
        $SpecRaw = Get-Content $SpecFile -Raw
        if ($SpecRaw -match '(?m)^>\s*Last verified against:\s*(\S+)') {
            $VerifiedHash = $Matches[1]
            $SrcDir = Join-Path $RepoRoot "src"
            if (-not (Test-Path $SrcDir)) { $SrcDir = $RepoRoot }

            $CommitsBehind = git rev-list --count "$VerifiedHash..HEAD" -- $SrcDir 2>$null
            if ($LASTEXITCODE -eq 0 -and $CommitsBehind) {
                $CommitsBehind = [int]$CommitsBehind
                if ($CommitsBehind -eq 0) {
                    Write-Output "✅ spec.md is current (verified against ``$VerifiedHash``, 0 source commits since)"
                } else {
                    Write-Output "⚠️ spec.md may be stale (**$CommitsBehind source commit(s)** since last verification at ``$VerifiedHash``)"
                    if ($Mode -eq 'validate') {
                        Write-Output "> Run ``/update-doc`` to re-verify and update the hash."
                    }
                }
            } else {
                Write-Output "⚠️ Could not resolve hash ``$VerifiedHash`` — git history may be shallow or hash is invalid."
            }
        } else {
            Write-Output "⚠️ No ``Last verified against:`` metadata found in spec.md."
            Write-Output "> Run ``/update-doc`` to add drift tracking."
        }
    } else {
        Write-Output "> spec.md not found — drift detection skipped."
    }
}

# ============================================================
# spec.md Section Audit (doc scope)
# ============================================================
if ($Scope -in @('doc', 'all')) {
    Write-Output ""
    Write-Output "## spec.md Section Audit"
    Write-Output ""

    $SpecFile = Join-Path $RepoRoot "spec.md"
    $SpecSections = @(
        'Module.*Contract',
        'Data Model',
        'State Machine',
        'Command.*CLI',
        'Integration Point'
    )
    $SpecSectionNames = @(
        'Module/Component Contracts',
        'Data Models',
        'State Machines',
        'Command/CLI Contracts',
        'Integration Points'
    )

    if (Test-Path $SpecFile) {
        $SpecContent = Get-Content $SpecFile -Raw

        Write-Output "| # | Section | Status |"
        Write-Output "|---|---------|--------|"

        for ($i = 0; $i -lt $SpecSections.Count; $i++) {
            $Found = $SpecContent -match "(?i)$($SpecSections[$i])"
            $Status = if ($Found) { "✅ Present" } else { "❌ Missing" }
            Write-Output "| $($i+1) | $($SpecSectionNames[$i]) | $Status |"
            Add-ValidationResult -Section $SpecSectionNames[$i] -Present $Found -DocFile "spec.md"
        }
    }
    else {
        Write-Output "> **spec.md not found.** All sections need to be created."
        foreach ($name in $SpecSectionNames) {
            Add-ValidationResult -Section $name -Present $false -DocFile "spec.md"
        }
    }
}

# ============================================================
# Validation Summary (validate mode only)
# ============================================================
if ($Mode -eq 'validate') {
    Write-Output ""
    Write-Output "---"
    Write-Output ""
    Write-Output "## Validation Summary"
    Write-Output ""

    if ($script:ValidationFailures.Count -eq 0) {
        Write-Output "✅ **All required sections are present.**"
        exit 0
    }
    else {
        Write-Output "❌ **$($script:ValidationFailures.Count) required section(s) missing:**"
        Write-Output ""
        foreach ($f in $script:ValidationFailures) {
            Write-Output "- $f"
        }
        Write-Output ""
        $ScopeLabel = switch ($Scope) { 'doc' { '/update-doc' }; 'arch' { '/architecture' }; 'all' { '/update-doc and /architecture' } }
        Write-Output "> Run ``$ScopeLabel`` to generate the missing sections."
        exit 1
    }
}
else {
    Write-Output ""
    Write-Output "---"
    Write-Output "> **Narsil Best Practice:** Set ``path=""$RepoRoot""`` in Narsil tool calls to restrict analysis to this specific project."
    Write-Output "> Scan complete. Use this data in the /update-doc workflow Steps 2-4."
}
