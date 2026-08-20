<#
.SYNOPSIS
[DEPRECATED] This script has been archived in Phase 4 of the Skills Migration.
Its functionality is now handled natively by TARS workflows and .gemini/skills/.
Do not use this script for new development.
#>
<#
# ⚠️ REFERENCE ONLY — Workflows now use inline auto-runnable commands.
# This script is retained as documentation of the procedure it implements.
# See .agent/workflows/plan-making.md, build.md for the current inline procedures.
.SYNOPSIS
    Synchronizes task.md with plan artifacts and generates doc update targets.

.DESCRIPTION
    Companion script for the /plan-making workflow. Parses implementation plan
    artifacts to generate task checklists, validate alignment, or produce
    file lists for /update-doc.

    Modes:
      generate  - Parse plan's [NEW]/[MODIFY]/[DELETE] tags → create task.md checklist.
      validate  - Compare existing task.md against plan → report mismatches.
      doc-list  - Extract affected source files → list for /update-doc workflow.
      preflight - Pre-approval gate: checks task.md exists, has items, and aligns with plan.

.PARAMETER Mode
    The operation mode: 'generate', 'validate', 'doc-list', or 'preflight'.

.PARAMETER PlanFile
    Path to the implementation plan artifact (markdown file).

.PARAMETER TaskFile
    Path to task.md. Defaults to task.md in the same directory as PlanFile.

.EXAMPLE
    .\.agent\scripts\Sync-TaskList.ps1 -Mode generate -PlanFile plan.md

.EXAMPLE
    .\.agent\scripts\Sync-TaskList.ps1 -Mode validate -PlanFile plan.md -TaskFile task.md

.EXAMPLE
    .\.agent\scripts\Sync-TaskList.ps1 -Mode doc-list -PlanFile plan.md
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('generate', 'validate', 'doc-list', 'preflight')]
    [string]$Mode,

    [Parameter()]
    [string]$PlanFile,

    [Parameter()]
    [string]$TaskFile
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# --- Validate PlanFile ---
if (-not $PlanFile) {
    Write-Error "PlanFile is required. Usage: Sync-TaskList.ps1 -Mode <mode> -PlanFile <path>"
    exit 1
}
if (-not (Test-Path $PlanFile)) {
    Write-Error "Plan file not found: $PlanFile"
    exit 1
}

$PlanContent = Get-Content $PlanFile
$PlanDir = Split-Path $PlanFile -Parent

# Default TaskFile
if (-not $TaskFile) {
    $TaskFile = Join-Path $PlanDir "task.md"
}

# --- Extract plan title ---
$PlanTitle = "Implementation Plan"
foreach ($line in $PlanContent) {
    if ($line -match '^#\s+(.+)') {
        $PlanTitle = $Matches[1].Trim()
        break
    }
}

# --- Extract file entries: [NEW], [MODIFY], [DELETE] ---
# Matches patterns like: #### [NEW] [filename](path) or ### [MODIFY] filename
$FileEntries = @()
$CurrentComponent = "General"

foreach ($line in $PlanContent) {
    # Detect component headings (### Component Name)
    if ($line -match '^###\s+(?!\[)(.+)') {
        $CurrentComponent = $Matches[1].Trim()
    }

    # Detect file entries with tags
    # Detect file entries with tags — only match heading lines (#### [ACTION])
    if ($line -match '^#{2,4}\s+\[(NEW|MODIFY|DELETE|TEST)\]\s*\[?([^\]\(]+)\]?\s*\(?(file:///)?([^\)\s]*)') {
        $Action = $Matches[1]
        $FileName = $Matches[2].Trim()
        $FilePath = if ($Matches[4]) { $Matches[4].Trim() } else { $FileName }

        $FileEntries += [PSCustomObject]@{
            Action    = $Action
            FileName  = $FileName
            FilePath  = $FilePath
            Component = $CurrentComponent
        }
    }
}

# ============================================================
# Generate Mode
# ============================================================
if ($Mode -eq 'generate') {
    $Lines = @()
    $Lines += "# Task: $PlanTitle"
    $Lines += ""
    $Lines += "## Objectives"

    # Group by component
    $Components = $FileEntries | Group-Object -Property Component

    foreach ($group in $Components) {
        $Lines += "- [ ] $($group.Name)"

        foreach ($entry in $group.Group) {
            $Lines += "  - [ ] [$($entry.Action)] $($entry.FileName)"
        }
    }

    # Standard trailing items
    $Lines += "- [ ] Run verification pipeline"
    $Lines += "- [ ] Update docs (run ``Sync-TaskList.ps1 -Mode doc-list`` then ``/update-doc``)"
    $Lines += "- [ ] Update ``context.md``"
    $Lines += "- [ ] Commit"
    $Lines += ""
    $Lines += "---"
    $Lines += "> Generated from: $PlanFile"
    $Lines += "> Files detected: $($FileEntries.Count)"

    Set-Content -Path $TaskFile -Value ($Lines -join "`n") -Encoding UTF8
    Write-Output "✅ task.md written to: $TaskFile ($($FileEntries.Count) file entries)"

    exit 0
}

# ============================================================
# Validate Mode
# ============================================================
if ($Mode -eq 'validate') {
    if (-not (Test-Path $TaskFile)) {
        Write-Output "❌ task.md not found at: $TaskFile"
        Write-Output "Run ``Sync-TaskList.ps1 -Mode generate`` to create it."
        exit 1
    }

    $TaskContent = Get-Content $TaskFile -Raw
    $Mismatches = @()

    foreach ($entry in $FileEntries) {
        # Check if the file appears in task.md
        $EscapedName = [regex]::Escape($entry.FileName)
        if ($TaskContent -notmatch $EscapedName) {
            $Mismatches += [PSCustomObject]@{
                Type     = "In plan, missing from task.md"
                Action   = $entry.Action
                FileName = $entry.FileName
            }
        }
    }

    # Check for task items not in plan (basic: look for [NEW/MODIFY/DELETE/TEST] patterns in task)
    $TaskFileRefs = [regex]::Matches($TaskContent, '\[(NEW|MODIFY|DELETE|TEST)\]\s*(\S+)')
    foreach ($match in $TaskFileRefs) {
        $TaskFileName = $match.Groups[2].Value
        $InPlan = $FileEntries | Where-Object { $_.FileName -eq $TaskFileName }
        if (-not $InPlan) {
            $Mismatches += [PSCustomObject]@{
                Type     = "In task.md, missing from plan"
                Action   = $match.Groups[1].Value
                FileName = $TaskFileName
            }
        }
    }

    Write-Output "# Task Sync Validation"
    Write-Output "Plan: $PlanFile"
    Write-Output "Task: $TaskFile"
    Write-Output ""

    if ($Mismatches.Count -eq 0) {
        Write-Output "✅ **task.md is aligned with the plan.** ($($FileEntries.Count) file(s) matched)"
    }
    else {
        Write-Output "❌ **$($Mismatches.Count) mismatch(es) found:**"
        Write-Output ""
        Write-Output "| Type | Action | File |"
        Write-Output "|------|--------|------|"
        foreach ($m in $Mismatches) {
            Write-Output "| $($m.Type) | $($m.Action) | $($m.FileName) |"
        }
        Write-Output ""
        Write-Output "> Re-run ``Sync-TaskList.ps1 -Mode generate`` to regenerate task.md."
        exit 1
    }

    # --- Progress check (advisory) ---
    $TaskLines = $TaskContent -split "`n"
    $StaleItems = @()
    foreach ($entry in $FileEntries) {
        $EscapedName = [regex]::Escape($entry.FileName)
        $matchLine = $TaskLines | Where-Object { $_ -match $EscapedName } | Select-Object -First 1
        if ($matchLine -and $matchLine -match '^\s*-\s*\[ \]') {
            $StaleItems += "[$($entry.Action)] $($entry.FileName)"
        }
    }

    if ($StaleItems.Count -gt 0) {
        Write-Output ""
        Write-Output "⚠️ **$($StaleItems.Count) item(s) still unchecked:**"
        Write-Output ""
        foreach ($s in $StaleItems) {
            Write-Output "- $s"
        }
        Write-Output ""
        Write-Output "> Update task.md: mark in-progress items ``[/]`` and completed items ``[x]``."
    }

    exit 0
}

# ============================================================
# Preflight Mode (gate before approval)
# ============================================================
if ($Mode -eq 'preflight') {
    Write-Output "# Pre-flight Gate"
    Write-Output "Plan: $PlanFile"
    Write-Output ""

    $Errors = @()

    # --- Check 1: task.md exists ---
    if (-not (Test-Path $TaskFile)) {
        $Errors += "task.md not found at: $TaskFile"
    }

    # --- Check 2: task.md has checklist items ---
    if ($Errors.Count -eq 0) {
        $TaskContent = Get-Content $TaskFile -Raw
        if ($TaskContent -notmatch '\[[ xX/]\]') {
            $Errors += "task.md has no checklist items (expected [ ], [x], or [/])"
        }
    }

    # --- Check 3: alignment with plan ---
    if ($Errors.Count -eq 0 -and $FileEntries.Count -gt 0) {
        foreach ($entry in $FileEntries) {
            $EscapedName = [regex]::Escape($entry.FileName)
            if ($TaskContent -notmatch $EscapedName) {
                $Errors += "Plan entry missing from task.md: [$($entry.Action)] $($entry.FileName)"
            }
        }
    }

    # --- Report ---
    if ($Errors.Count -eq 0) {
        Write-Output "✅ **Pre-flight passed.** task.md exists, has items, and aligns with plan."
        Write-Output "> Files: $($FileEntries.Count) | Task: $TaskFile"
        exit 0
    }
    else {
        Write-Output "❌ **Pre-flight FAILED** ($($Errors.Count) issue(s)):"
        Write-Output ""
        foreach ($e in $Errors) {
            Write-Output "- $e"
        }
        Write-Output ""
        Write-Output "> Fix: Run ``Sync-TaskList.ps1 -Mode generate -PlanFile $PlanFile`` then retry."
        exit 1
    }
}

# ============================================================
# Doc-List Mode
# ============================================================
if ($Mode -eq 'doc-list') {
    Write-Output "# Files Requiring Documentation Update"
    Write-Output "Generated from: $PlanFile"
    Write-Output ""

    if ($FileEntries.Count -eq 0) {
        Write-Output "> No file entries found in plan. Check that [NEW]/[MODIFY]/[DELETE] tags are used."
        exit 0
    }

    # Filter to source files only (not configs, docs, scripts)
    $SourceExtensions = @('.rs', '.go', '.ts', '.tsx', '.js', '.jsx', '.svelte', '.py')
    $SourceFiles = $FileEntries | Where-Object {
        $ext = [System.IO.Path]::GetExtension($_.FileName)
        $ext -in $SourceExtensions
    }
    $DocFiles = $FileEntries | Where-Object {
        $ext = [System.IO.Path]::GetExtension($_.FileName)
        $ext -notin $SourceExtensions
    }

    # Group by action, NEW first
    $NewFiles = $SourceFiles | Where-Object { $_.Action -eq 'NEW' }
    $ModFiles = $SourceFiles | Where-Object { $_.Action -eq 'MODIFY' }
    $DelFiles = $SourceFiles | Where-Object { $_.Action -eq 'DELETE' }

    Write-Output "## Affected Source Files"
    Write-Output ""

    if ($NewFiles) {
        Write-Output "### 🟢 NEW — Needs fresh documentation"
        foreach ($f in $NewFiles) {
            Write-Output "- ``$($f.FileName)`` — generate rustdoc/doc comments (no docs exist yet)"
        }
        Write-Output ""
    }

    if ($ModFiles) {
        Write-Output "### 🟡 MODIFY — Check for doc drift"
        foreach ($f in $ModFiles) {
            Write-Output "- ``$($f.FileName)`` — verify signatures/behavior unchanged or update docs"
        }
        Write-Output ""
    }

    if ($DelFiles) {
        Write-Output "### 🔴 DELETE — Clean up references"
        foreach ($f in $DelFiles) {
            Write-Output "- ``$($f.FileName)`` — remove from architecture.md, spec.md references"
        }
        Write-Output ""
    }

    if (-not $SourceFiles) {
        Write-Output "> No source files affected — only config/doc files changed."
        Write-Output ""
    }

    Write-Output "## Recommended Handoff"
    Write-Output ""
    Write-Output "1. Run ``/update-doc`` after implementation and ``/audit`` pass."
    Write-Output "2. Scope ``/update-doc`` to the files listed above."
    Write-Output "3. Prioritize 🟢 NEW files (no docs exist yet)."
    Write-Output "4. Check 🟡 MODIFY files for changed interfaces or behavior."
    Write-Output "5. Clean up 🔴 DELETE references from ``architecture.md`` and ``spec.md``."
    Write-Output "6. Also check if ``architecture.md`` or ``spec.md`` sections need structural updates."
    Write-Output ""
    Write-Output "---"
    Write-Output "> Source files: $(@($SourceFiles).Count) | Doc/config files: $(@($DocFiles).Count) | Total: $($FileEntries.Count)"

    exit 0
}
