<#
.SYNOPSIS
[DEPRECATED] This script has been archived in Phase 4 of the Skills Migration.
Its functionality is now handled natively by TARS workflows and .gemini/skills/.
Do not use this script for new development.
#>
<#
# ⚠️ REFERENCE ONLY — Workflows now use inline auto-runnable commands.
# This script is retained as documentation of the procedure it implements.
# See .agent/workflows/audit.md for the current inline procedure.
.SYNOPSIS
    Automates mechanical audit checks for the /audit workflow.

.DESCRIPTION
    Companion script for the /audit workflow. Gathers context, runs verification
    gates, and performs mechanical compliance checks. Outputs a structured report
    with pre-filled checklist items.

    Modes:
      scan - Full scan: gather context + mechanical checks + pre-filled checklist.
      gate - Verification gate only: run fmt/lint/test and report pass/fail.

.PARAMETER Mode
    The operation mode: 'scan' or 'gate'.

.PARAMETER Scope
    Audit scope: 'changed' (git diff, default) or 'full' (entire codebase).

.EXAMPLE
    .\.agent\scripts\Audit-Codebase.ps1 -Mode scan

.EXAMPLE
    .\.agent\scripts\Audit-Codebase.ps1 -Mode scan -Scope full

.EXAMPLE
    .\.agent\scripts\Audit-Codebase.ps1 -Mode gate
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('scan', 'gate')]
    [string]$Mode,

    [Parameter()]
    [ValidateSet('changed', 'full')]
    [string]$Scope = 'changed'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Continue'

# --- Detect repo root ---
$RepoRoot = git rev-parse --show-toplevel 2>$null
if (-not $RepoRoot) {
    Write-Warning "Not inside a git repository. Using current directory."
    $RepoRoot = (Get-Location).Path
}
$RepoRoot = $RepoRoot.Trim()

# --- Detect language ---
$Language = "Unknown"
$CargoToml = Join-Path $RepoRoot "Cargo.toml"
$PackageJson = Join-Path $RepoRoot "package.json"
$GoMod = Join-Path $RepoRoot "go.mod"

if (Test-Path $CargoToml) { $Language = "Rust" }
elseif (Test-Path $PackageJson) { $Language = "JavaScript/TypeScript" }
elseif (Test-Path $GoMod) { $Language = "Go" }

# --- Detect toolchain commands from architecture.md ---
$ArchFile = Join-Path $RepoRoot "architecture.md"
$FmtCmd = $null
$LintCmd = $null
$TestCmd = $null

if (Test-Path $ArchFile) {
    $ArchContent = Get-Content $ArchFile -Raw
    # Try to extract commands from Toolchain section
    if ($Language -eq "Rust") {
        $FmtCmd = "cargo fmt --all -- --check"
        $LintCmd = "cargo clippy --all-targets --all-features -- -D warnings"
        $TestCmd = "cargo test --all-features"
    }
}
else {
    # Fallback defaults by language
    if ($Language -eq "Rust") {
        $FmtCmd = "cargo fmt --all -- --check"
        $LintCmd = "cargo clippy --all-targets -- -D warnings"
        $TestCmd = "cargo test"
    }
    elseif ($Language -eq "JavaScript/TypeScript") {
        $FmtCmd = "npx prettier --check ."
        $LintCmd = "npx eslint ."
        $TestCmd = "npm test"
    }
    elseif ($Language -eq "Go") {
        $FmtCmd = "gofmt -l ."
        $LintCmd = "golangci-lint run"
        $TestCmd = "go test ./..."
    }
}

# --- Helper: Run a command and report pass/fail ---
function Invoke-GateCheck {
    param([string]$Name, [string]$Command)

    if (-not $Command) {
        Write-Output "| **$Name** | ``N/A`` | ⏭️ Skipped (no command) |"
        return $true
    }

    Write-Output "| **$Name** | ``$Command`` | $(
        try {
            $output = Invoke-Expression $Command 2>&1
            if ($LASTEXITCODE -eq 0) { "✅ Pass" } else { "❌ Fail" }
        }
        catch {
            "❌ Error: $($_.Exception.Message)"
        }
    ) |"

    return ($LASTEXITCODE -eq 0)
}

# --- Helpers ---
$RgAvailable = [bool](Get-Command rg -ErrorAction SilentlyContinue)

function Get-TargetFiles {
    if ($Scope -eq 'changed') {
        $files = git diff --name-only HEAD~1 2>$null
        if (-not $files) {
            $files = git diff --name-only --cached 2>$null
        }
        if (-not $files) {
            $files = git diff --name-only 2>$null
        }
        return $files
    }
    else {
        # Full codebase
        if ($Language -eq "Rust") {
            return Get-ChildItem -Path $RepoRoot -Filter "*.rs" -Recurse -File -Name -ErrorAction SilentlyContinue |
                Where-Object { $_ -notmatch '(target|\.git)' }
        }
        elseif ($Language -match "Script") {
            return Get-ChildItem -Path $RepoRoot -Include "*.ts","*.js","*.tsx","*.jsx" -Recurse -File -Name -ErrorAction SilentlyContinue |
                Where-Object { $_ -notmatch '(node_modules|\.git|dist|build)' }
        }
        elseif ($Language -eq "Go") {
            return Get-ChildItem -Path $RepoRoot -Filter "*.go" -Recurse -File -Name -ErrorAction SilentlyContinue |
                Where-Object { $_ -notmatch '(vendor|\.git)' }
        }
        else {
            return Get-ChildItem -Path $RepoRoot -Recurse -File -Name -ErrorAction SilentlyContinue |
                Where-Object { $_ -notmatch '(\.git|node_modules|target)' } |
                Select-Object -First 100
        }
    }
}

# ============================================================
# Header
# ============================================================
$Date = Get-Date -Format 'yyyy-MM-dd'
Write-Output "# Audit Scan Report"
Write-Output "Generated: $Date"
Write-Output "Mode: $Mode | Scope: $Scope | Language: $Language"
Write-Output "Root: $RepoRoot"

# ============================================================
# Changed / Target Files
# ============================================================
if ($Mode -eq 'scan') {
    Write-Output ""
    Write-Output "## Target Files"
    Write-Output ""

    $TargetFiles = Get-TargetFiles
    if ($TargetFiles) {
        Write-Output '```'
        $TargetFiles
        Write-Output '```'
        Write-Output ""
        Write-Output "**Total:** $(@($TargetFiles).Count) file(s)"
    }
    else {
        Write-Output "> No changed files detected. Consider using ``-Scope full`` for a compliance audit."
    }
}

# ============================================================
# Verification Gate
# ============================================================
Write-Output ""
Write-Output "## Verification Gate"
Write-Output ""
Write-Output "| Check | Command | Status |"
Write-Output "|-------|---------|--------|"

$GatePass = $true

if ($FmtCmd) {
    Push-Location $RepoRoot
    Invoke-GateCheck -Name "Formatter" -Command $FmtCmd
    if ($LASTEXITCODE -ne 0) { $GatePass = $false }
    Pop-Location
}
else {
    Write-Output "| **Formatter** | N/A | ⏭️ Skipped |"
}

if ($LintCmd) {
    Push-Location $RepoRoot
    Invoke-GateCheck -Name "Linter" -Command $LintCmd
    if ($LASTEXITCODE -ne 0) { $GatePass = $false }
    Pop-Location
}
else {
    Write-Output "| **Linter** | N/A | ⏭️ Skipped |"
}

if ($TestCmd) {
    Push-Location $RepoRoot
    Invoke-GateCheck -Name "Tests" -Command $TestCmd
    if ($LASTEXITCODE -ne 0) { $GatePass = $false }
    Pop-Location
}
else {
    Write-Output "| **Tests** | N/A | ⏭️ Skipped |"
}

Write-Output ""
if ($GatePass) {
    Write-Output "✅ **Verification gate passed.**"
}
else {
    Write-Output "❌ **Verification gate failed.** See details above."
}

# Gate mode stops here
if ($Mode -eq 'gate') {
    if ($GatePass) { exit 0 } else { exit 1 }
}

# ============================================================
# Mechanical Checks (scan mode only)
# ============================================================
Write-Output ""
Write-Output "---"
Write-Output ""
Write-Output "## Mechanical Checks"

# --- .unwrap() in Production Code (Rust) ---
if ($Language -eq "Rust") {
    Write-Output ""
    Write-Output "### .unwrap() in Production Code"
    Write-Output ""

    $SrcDir = Join-Path $RepoRoot "src"
    if (-not (Test-Path $SrcDir)) { $SrcDir = $RepoRoot }

    if ($RgAvailable) {
        $UnwrapHits = rg -n "\.unwrap\(\)" $SrcDir --glob "*.rs" --glob "!*test*" --glob "!*bench*" 2>$null
    }
    else {
        $UnwrapHits = Get-ChildItem -Path $SrcDir -Filter "*.rs" -Recurse -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -notmatch 'test' } |
            Select-String -Pattern "\.unwrap\(\)" -ErrorAction SilentlyContinue
    }

    if ($UnwrapHits) {
        Write-Output "❌ **Found $(@($UnwrapHits).Count) instance(s):**"
        Write-Output ""
        Write-Output '```'
        $UnwrapHits | Select-Object -First 20
        Write-Output '```'
    }
    else {
        Write-Output "✅ No ``.unwrap()`` found in production code."
    }
}

# --- Hardcoded Secret Patterns ---
Write-Output ""
Write-Output "### Hardcoded Secret Patterns"
Write-Output ""

$SecretPatterns = @(
    'API_KEY\s*=',
    'SECRET\s*=',
    'PASSWORD\s*=',
    'TOKEN\s*=',
    'PRIVATE_KEY',
    'BEGIN RSA',
    'BEGIN OPENSSH'
)
$SecretRegex = ($SecretPatterns -join '|')

if ($RgAvailable) {
    $SecretHits = rg -n -i $SecretRegex $RepoRoot --glob "!.git" --glob "!target" --glob "!node_modules" --glob "!*.lock" 2>$null
}
else {
    $SecretHits = Get-ChildItem -Path $RepoRoot -Recurse -File -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -notmatch '(\.git|target|node_modules)' -and $_.Extension -notmatch '\.lock' } |
        Select-String -Pattern $SecretPatterns -ErrorAction SilentlyContinue
}

if ($SecretHits) {
    Write-Output "⚠️ **Found $(@($SecretHits).Count) potential secret(s):**"
    Write-Output ""
    Write-Output '```'
    $SecretHits | Select-Object -First 15
    Write-Output '```'
    Write-Output ""
    Write-Output "> Review manually — these may be false positives (e.g., config templates, documentation)."
}
else {
    Write-Output "✅ No hardcoded secret patterns detected."
}

# --- Doc Coverage on Target Files ---
if ($Language -eq "Rust") {
    Write-Output ""
    Write-Output "### Doc Coverage"
    Write-Output ""

    $SrcDir = Join-Path $RepoRoot "src"
    if (-not (Test-Path $SrcDir)) { $SrcDir = $RepoRoot }

    if ($RgAvailable) {
        $PubItems = rg -c "^\s*pub\s+(fn|struct|enum|trait|type)\s+" $SrcDir --glob "*.rs" 2>$null
        $DocLines = rg -c "^\s*///" $SrcDir --glob "*.rs" 2>$null
    }
    else {
        $PubItems = Get-ChildItem -Path $SrcDir -Filter "*.rs" -Recurse -File -ErrorAction SilentlyContinue |
            Select-String -Pattern "^\s*pub\s+(fn|struct|enum|trait|type)\s+" -ErrorAction SilentlyContinue
        $DocLines = @()
    }

    $PubCount = 0
    if ($PubItems) {
        foreach ($p in $PubItems) {
            if ($p -match ':(\d+)$') { $PubCount += [int]$Matches[1] }
        }
    }

    $DocCount = 0
    if ($DocLines) {
        foreach ($d in $DocLines) {
            if ($d -match ':(\d+)$') { $DocCount += [int]$Matches[1] }
        }
    }

    $Coverage = if ($PubCount -gt 0) { [math]::Round(($DocCount / $PubCount) * 100, 1) } else { 0 }

    Write-Output "| Metric | Value |"
    Write-Output "|--------|-------|"
    Write-Output "| Public items | $PubCount |"
    Write-Output "| Doc comment lines | $DocCount |"
    Write-Output "| Estimated coverage | ~$Coverage% |"
}

# --- TODO/FIXME/HACK ---
Write-Output ""
Write-Output "### TODO/FIXME/HACK Markers"
Write-Output ""

if ($RgAvailable) {
    $TodoHits = rg -n "TODO|FIXME|HACK" $RepoRoot --glob "!.git" --glob "!target" --glob "!node_modules" --type-add "code:*.{rs,go,ts,js,svelte,py}" --type code 2>$null
}
else {
    $Extensions = @('*.rs', '*.go', '*.ts', '*.js', '*.svelte', '*.py')
    $TodoHits = Get-ChildItem -Path $RepoRoot -Recurse -Include $Extensions -File -ErrorAction SilentlyContinue |
        Select-String -Pattern 'TODO|FIXME|HACK' -ErrorAction SilentlyContinue
}

if ($TodoHits) {
    Write-Output "**Found $(@($TodoHits).Count) marker(s):**"
    Write-Output ""
    Write-Output '```'
    $TodoHits | Select-Object -First 20
    Write-Output '```'
}
else {
    Write-Output "✅ No TODO/FIXME/HACK markers found."
}

# ============================================================
# Pre-filled Checklist
# ============================================================
Write-Output ""
Write-Output "---"
Write-Output ""
Write-Output "## Pre-filled Audit Checklist"
Write-Output ""
Write-Output "Items marked with 🤖 were checked automatically. Items marked with 🧠 require LLM reasoning."
Write-Output ""

# Gate results
$GateStatus = if ($GatePass) { "✅" } else { "❌" }
Write-Output "### Verification Gate"
Write-Output "- [$GateStatus] 🤖 Formatter, Linter, Tests pass"
Write-Output ""

# Mechanical results
$UnwrapStatus = if ($Language -ne "Rust") { "N/A" } elseif (-not $UnwrapHits) { "✅" } else { "❌" }
$SecretStatus = if (-not $SecretHits) { "✅" } else { "⚠️" }

Write-Output "### Code Quality (2e)"
Write-Output "- [$UnwrapStatus] 🤖 No ``.unwrap()`` in production code"
Write-Output "- [$SecretStatus] 🤖 No hardcoded secrets"
Write-Output "- [ ] 🧠 Code is idiomatic"
Write-Output "- [ ] 🧠 No dead code or unused imports"
Write-Output "- [ ] 🧠 Clear variable/function names"
Write-Output ""

Write-Output "### GEMINI.md Compliance (2b)"
Write-Output "- [ ] 🧠 Error handling: no silent failures"
Write-Output "- [ ] 🧠 Observability: structured logging present"
Write-Output "- [ ] 🧠 Documentation: public items documented"
Write-Output ""

Write-Output "### Testing (2c)"
Write-Output "- [ ] 🧠 Tests exist for new/changed logic"
Write-Output "- [ ] 🧠 Edge cases covered"
Write-Output "- [ ] 🧠 Testable design (injectable deps)"
Write-Output ""

Write-Output "---"
Write-Output "> **Narsil Best Practice:** Set ``path=""$RepoRoot""`` in Narsil tool calls to restrict analysis to this specific project."
Write-Output "> Scan complete. Use this data in the /audit workflow Steps 2-4."
Write-Output "> Items marked 🧠 require the Architect's judgment (or Narsil MCP)."
