<#
.SYNOPSIS
[DEPRECATED] This script has been archived in Phase 4 of the Skills Migration.
Its functionality is now handled natively by TARS workflows and .gemini/skills/.
Do not use this script for new development.
#>
<#
# ⚠️ REFERENCE ONLY — Workflows now use inline auto-runnable commands.
# This script is retained as documentation of the procedure it implements.
# See .agent/workflows/toolcheck.md for the current inline procedure.
.SYNOPSIS
    Scans the development environment for session readiness.

.DESCRIPTION
    Companion script for the /toolcheck workflow. Performs mechanical checks
    across 6 categories: shell tools, Rust toolchain, linkers, workflow files,
    script files, and project detection. Outputs a structured markdown report.

    The LLM uses this output to diagnose failures, attempt fixes, and produce
    a Session Readiness Report.

.PARAMETER Mode
    The operation mode: 'scan' (only mode, extensible).

.EXAMPLE
    .\.agent\scripts\Check-Environment.ps1 -Mode scan
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('scan')]
    [string]$Mode
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

$Date = Get-Date -Format 'yyyy-MM-dd HH:mm'

Write-Output "# 🔧 Environment Scan"
Write-Output "Generated: $Date"
Write-Output "Repo: ``$RepoRoot``"
Write-Output ""

# ============================================================
# Helper: Check if a command exists and get version
# ============================================================
function Test-Tool {
    param(
        [string]$Name,
        [string]$VersionCmd
    )
    $cmd = Get-Command $Name -ErrorAction SilentlyContinue
    if ($cmd) {
        $version = ""
        if ($VersionCmd) {
            try {
                $version = Invoke-Expression $VersionCmd 2>$null
                if ($version) {
                    # Take first line only
                    $version = ($version -split "`n")[0].Trim()
                }
            } catch {
                $version = "(version unknown)"
            }
        }
        $path = $cmd.Source
        return [PSCustomObject]@{
            Found   = $true
            Version = $version
            Path    = $path
        }
    }
    return [PSCustomObject]@{
        Found   = $false
        Version = ""
        Path    = ""
    }
}

# ============================================================
# Category 1: Shell Tools
# ============================================================
Write-Output "## 1. Shell Tools"
Write-Output ""
Write-Output "| Tool | Status | Version | Path |"
Write-Output "|------|--------|---------|------|"

$ShellTools = @(
    @{ Name = 'git';  VersionCmd = 'git --version' }
    @{ Name = 'rg';   VersionCmd = 'rg --version' }
)

# PowerShell version (special case — already running)
$PwshVersion = $PSVersionTable.PSVersion.ToString()
$PwshEdition = $PSVersionTable.PSEdition
Write-Output "| pwsh | ✅ | $PwshVersion ($PwshEdition) | (current shell) |"

foreach ($tool in $ShellTools) {
    $result = Test-Tool -Name $tool.Name -VersionCmd $tool.VersionCmd
    if ($result.Found) {
        Write-Output "| $($tool.Name) | ✅ | $($result.Version) | ``$($result.Path)`` |"
    } else {
        Write-Output "| $($tool.Name) | ❌ | — | not found |"
    }
}

Write-Output ""

# --- Git non-interactive safety ---
Write-Output "### Git Non-Interactive Safety"
Write-Output ""

$CredHelper = git config credential.helper 2>$null
$GpgSign = git config commit.gpgsign 2>$null
$FixesApplied = @()

# Detect and fix credential manager prompts
if ($CredHelper -match 'manager|wincred|osxkeychain') {
    $env:GCM_INTERACTIVE = 'never'
    $env:GIT_TERMINAL_PROMPT = '0'
    $FixesApplied += "GCM_INTERACTIVE=never (was: ``$CredHelper``)"
    $FixesApplied += "GIT_TERMINAL_PROMPT=0"
}

# Detect GPG signing (can't auto-fix, just warn)
if ($GpgSign -eq 'true') {
    Write-Output "> ⚠️ **GPG signing enabled.** Ensure gpg-agent is running or commits may hang."
}

if ($FixesApplied.Count -gt 0) {
    Write-Output "| Blocker | Fix Applied |"
    Write-Output "|---------|-------------|"
    foreach ($fix in $FixesApplied) {
        Write-Output "| Interactive prompt | $fix |"
    }
    Write-Output ""
    Write-Output "> ✅ Non-interactive env vars set **for this session**."
} else {
    Write-Output "> ✅ No interactive blockers detected."
}

Write-Output ""

# ============================================================
# Category 2: Rust Toolchain
# ============================================================
Write-Output "## 2. Rust Toolchain"
Write-Output ""
Write-Output "| Tool | Status | Version |"
Write-Output "|------|--------|---------|"

$RustTools = @(
    @{ Name = 'rustc';       VersionCmd = 'rustc --version' }
    @{ Name = 'cargo';       VersionCmd = 'cargo --version' }
    @{ Name = 'cargo-clippy'; VersionCmd = 'cargo clippy --version' }
    @{ Name = 'rustfmt';     VersionCmd = 'rustfmt --version' }
    @{ Name = 'rustup';      VersionCmd = 'rustup --version' }
)

$RustAvailable = $false
foreach ($tool in $RustTools) {
    $result = Test-Tool -Name $tool.Name -VersionCmd $tool.VersionCmd
    if ($result.Found) {
        Write-Output "| $($tool.Name) | ✅ | $($result.Version) |"
        if ($tool.Name -eq 'rustc') { $RustAvailable = $true }
    } else {
        Write-Output "| $($tool.Name) | ❌ | — |"
    }
}

# Rustup show (active toolchain + targets)
if ($RustAvailable) {
    Write-Output ""
    Write-Output "### Active Toolchain"
    Write-Output ""
    Write-Output '```'
    rustup show 2>$null | Select-Object -First 15
    Write-Output '```'

    # Check Cargo.toml for edition
    $CargoToml = Join-Path $RepoRoot "Cargo.toml"
    if (Test-Path $CargoToml) {
        $Edition = Select-String -Path $CargoToml -Pattern 'edition\s*=\s*"(\d{4})"' -ErrorAction SilentlyContinue
        if ($Edition) {
            Write-Output ""
            Write-Output "> Rust edition: **$($Edition.Matches[0].Groups[1].Value)**"
        }
    }
}

Write-Output ""

# ============================================================
# Category 3: Linkers (Portable MSVC)
# ============================================================
Write-Output "## 3. Linkers"
Write-Output ""
Write-Output "| Linker | Status | Path | Notes |"
Write-Output "|--------|--------|------|-------|"

$Linkers = @('link.exe', 'cl.exe', 'gcc')

foreach ($linker in $Linkers) {
    $cmd = Get-Command $linker -ErrorAction SilentlyContinue
    if ($cmd) {
        $path = $cmd.Source
        $notes = ""

        # Scoop conflict check for link.exe
        if ($linker -eq 'link.exe') {
            if ($path -match '(scoop|shims|busybox|coreutils|usr\\bin)') {
                $notes = "⚠️ **CONFLICT**: This is scoop/coreutils ``link``, NOT MSVC ``link.exe``"
            } elseif ($path -match '(MSVC|Microsoft|VC|Visual)') {
                $notes = "✅ MSVC linker"
            } else {
                $notes = "⚠️ Unknown origin — verify this is MSVC"
            }
        }

        Write-Output "| $linker | ✅ | ``$path`` | $notes |"
    } else {
        Write-Output "| $linker | ❌ | not found | — |"
    }
}

# Check for all link.exe instances on PATH (conflict detection)
$AllLinks = @(Get-Command link.exe -All -ErrorAction SilentlyContinue)
if ($AllLinks.Count -gt 1) {
    Write-Output ""
    Write-Output "> ⚠️ **Multiple ``link.exe`` found on PATH** ($($AllLinks.Count) instances):"
    foreach ($l in $AllLinks) {
        Write-Output ">   - ``$($l.Source)``"
    }
    Write-Output "> First one wins. Ensure MSVC ``link.exe`` appears before scoop's."
}

Write-Output ""

# ============================================================
# Category 4: Workflow Files
# ============================================================
Write-Output "## 4. Workflow Files"
Write-Output ""

$WorkflowDir = Join-Path $RepoRoot ".agent" "workflows"

# Required workflows (must exist)
$RequiredWorkflows = @(
    'ago.md', 'architecture.md', 'audit.md', 'brainstorm.md', 'build.md',
    'design.md', 'feature.md', 'issue.md',
    'plan-making.md', 'spec.md', 'toolcheck.md', 'update-doc.md'
)
# Alternative workflows (at least one per group must exist)
$AlternativeWorkflows = @{
    'log-audit' = @('log-audit-generic.md', 'log-audit.md')
}

Write-Output "| Workflow | Status |"
Write-Output "|----------|--------|"

$WorkflowMissing = 0
$WorkflowTotal = $RequiredWorkflows.Count + $AlternativeWorkflows.Count

# Check required workflows
foreach ($wf in $RequiredWorkflows) {
    $wfPath = Join-Path $WorkflowDir $wf
    if (Test-Path $wfPath) {
        Write-Output "| $wf | ✅ |"
    } else {
        Write-Output "| $wf | ❌ |"
        $WorkflowMissing++
    }
}

# Check alternative workflows (any-of per group)
foreach ($group in $AlternativeWorkflows.GetEnumerator()) {
    $found = $null
    foreach ($alt in $group.Value) {
        $altPath = Join-Path $WorkflowDir $alt
        if (Test-Path $altPath) {
            $found = $alt
            break
        }
    }
    if ($found) {
        Write-Output "| $found | ✅ |"
    } else {
        $names = ($group.Value -join ' or ')
        Write-Output "| $names | ❌ |"
        $WorkflowMissing++
    }
}

# Check for extra workflows
$AllExpected = $RequiredWorkflows + ($AlternativeWorkflows.Values | ForEach-Object { $_ } | ForEach-Object { $_ })
$ActualWorkflows = Get-ChildItem -Path $WorkflowDir -Filter "*.md" -ErrorAction SilentlyContinue
$ExtraWorkflows = $ActualWorkflows | Where-Object { $_.Name -notin $AllExpected }
if ($ExtraWorkflows) {
    Write-Output ""
    Write-Output "> Extra workflows found:"
    foreach ($ex in $ExtraWorkflows) {
        Write-Output ">   - ``$($ex.Name)``"
    }
}

Write-Output ""

# ============================================================
# Category 5: Script Files
# ============================================================
Write-Output "## 5. Script Files"
Write-Output ""

$ScriptDir = Join-Path $RepoRoot ".agent" "scripts"
$ExpectedScripts = @(
    'Audit-Codebase.ps1', 'Check-Environment.ps1', 'Check-Logs.ps1',
    'Git-Checkpoint.ps1', 'Load-Context.ps1', 'Scan-ProjectDocs.ps1',
    'Sync-TaskList.ps1'
)

Write-Output "### .agent/scripts/"
Write-Output ""
Write-Output "| Script | Status |"
Write-Output "|--------|--------|"

$ScriptMissing = 0
foreach ($sc in $ExpectedScripts) {
    $scPath = Join-Path $ScriptDir $sc
    if (Test-Path $scPath) {
        Write-Output "| $sc | ✅ |"
    } else {
        Write-Output "| $sc | ❌ |"
        $ScriptMissing++
    }
}

# Check repo root for scripts
Write-Output ""
Write-Output "### Repo Root Scripts"
Write-Output ""

$RootScripts = @()
$RootScripts += Get-ChildItem -Path $RepoRoot -Filter "*.ps1" -Depth 0 -ErrorAction SilentlyContinue
$RootScripts += Get-ChildItem -Path $RepoRoot -Filter "*.sh" -Depth 0 -ErrorAction SilentlyContinue
$RootScripts += Get-ChildItem -Path $RepoRoot -Filter "Makefile" -Depth 0 -ErrorAction SilentlyContinue
$RootScripts += Get-ChildItem -Path $RepoRoot -Filter "justfile" -Depth 0 -ErrorAction SilentlyContinue

if ($RootScripts) {
    Write-Output "| File | Type |"
    Write-Output "|------|------|"
    foreach ($rs in $RootScripts) {
        $ext = $rs.Extension
        Write-Output "| ``$($rs.Name)`` | $ext |"
    }
} else {
    Write-Output "> No scripts found in repo root."
}

Write-Output ""

# ============================================================
# Category 6: Project Detection
# ============================================================
Write-Output "## 6. Project Detection"
Write-Output ""

$ProjectFiles = @(
    @{ File = 'Cargo.toml';    Type = 'Rust' }
    @{ File = 'package.json';  Type = 'Node.js' }
    @{ File = 'go.mod';        Type = 'Go' }
    @{ File = 'pyproject.toml'; Type = 'Python' }
)

$DetectedProject = $null
foreach ($pf in $ProjectFiles) {
    $pfPath = Join-Path $RepoRoot $pf.File
    if (Test-Path $pfPath) {
        $DetectedProject = $pf.Type
        Write-Output "| Field | Value |"
        Write-Output "|-------|-------|"
        Write-Output "| **Type** | $($pf.Type) |"
        Write-Output "| **Manifest** | ``$($pf.File)`` |"

        # Parse project name from manifest
        if ($pf.Type -eq 'Rust') {
            $Name = Select-String -Path $pfPath -Pattern '^\s*name\s*=\s*"([^"]+)"' | Select-Object -First 1
            if ($Name) {
                Write-Output "| **Name** | $($Name.Matches[0].Groups[1].Value) |"
            }
            $Edition = Select-String -Path $pfPath -Pattern 'edition\s*=\s*"(\d{4})"' | Select-Object -First 1
            if ($Edition) {
                Write-Output "| **Edition** | $($Edition.Matches[0].Groups[1].Value) |"
            }
        }
        elseif ($pf.Type -eq 'Node.js') {
            $PkgJson = Get-Content $pfPath -Raw | ConvertFrom-Json -ErrorAction SilentlyContinue
            if ($PkgJson -and $PkgJson.name) {
                Write-Output "| **Name** | $($PkgJson.name) |"
            }
        }
        elseif ($pf.Type -eq 'Go') {
            $Module = Select-String -Path $pfPath -Pattern '^module\s+(\S+)' | Select-Object -First 1
            if ($Module) {
                Write-Output "| **Module** | $($Module.Matches[0].Groups[1].Value) |"
            }
        }
        break
    }
}

if (-not $DetectedProject) {
    Write-Output "> No recognized project manifest found."
}

Write-Output ""

# ============================================================
# Summary
# ============================================================
Write-Output "---"
Write-Output ""
Write-Output "## Summary"
Write-Output ""

$TotalChecks = $ShellTools.Count + 1 + $RustTools.Count + $Linkers.Count + $WorkflowTotal + $ExpectedScripts.Count
$Issues = $WorkflowMissing + $ScriptMissing
# Note: tool/linker failures are counted by the LLM from the table above

Write-Output "| Metric | Value |"
Write-Output "|--------|-------|"
Write-Output "| **Repo** | ``$RepoRoot`` |"
Write-Output "| **Project** | $( if ($DetectedProject) { $DetectedProject } else { 'unknown' } ) |"
Write-Output "| **Workflow files** | $($WorkflowTotal - $WorkflowMissing) / $WorkflowTotal |"
Write-Output "| **Script files** | $($ExpectedScripts.Count - $ScriptMissing) / $($ExpectedScripts.Count) |"
Write-Output "| **Rust** | $( if ($RustAvailable) { '✅' } else { '❌' } ) |"

Write-Output ""
Write-Output "> **Narsil Best Practice:** Set ``path=""$RepoRoot""`` in Narsil tool calls to restrict analysis to this specific project."
Write-Output "> Scan complete. LLM should now diagnose any ❌ items and attempt fixes."

exit 0
