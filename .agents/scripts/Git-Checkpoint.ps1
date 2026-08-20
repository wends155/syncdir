<#
.SYNOPSIS
[DEPRECATED] This script has been archived in Phase 4 of the Skills Migration.
Its functionality is now handled natively by TARS workflows and .gemini/skills/.
Do not use this script for new development.
#>
<#
# ⚠️ REFERENCE ONLY — Workflows now use inline auto-runnable commands.
# This script is retained as documentation of the procedure it implements.
# See .agent/workflows/build.md for the current inline procedure.
.SYNOPSIS
    Atomic git add + commit in one step.

.DESCRIPTION
    Companion script for the TARS Act phase. Combines git status, add, and commit
    into a single tool call, reducing agent overhead from 2-3 calls to 1.

    Optionally runs Sync-TaskList.ps1 -Mode validate as a pre-commit gate
    to enforce task.md alignment before committing.

.PARAMETER Files
    Files to stage. Use "." to stage all changes.

.PARAMETER Message
    Commit message string.

.PARAMETER ValidateTask
    If present, runs Sync-TaskList.ps1 -Mode validate before committing.
    Aborts if validation fails (exit != 0).

.EXAMPLE
    .\.agent\scripts\Git-Checkpoint.ps1 -Files "src/main.rs","Cargo.toml" -Message "feat: add parser"

.EXAMPLE
    .\.agent\scripts\Git-Checkpoint.ps1 -Files "." -Message "fix: resolve lint" -ValidateTask
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string[]]$Files,

    [Parameter(Mandatory)]
    [string]$Message,

    [switch]$ValidateTask,

    [Parameter()]
    [string]$PlanFile,

    [Parameter()]
    [string]$TaskFile
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Continue'

# Prevent interactive prompts (defensive — /toolcheck sets these at session start)
$env:GIT_TERMINAL_PROMPT = '0'
$env:GCM_INTERACTIVE = 'never'

# --- Detect repo root ---
$RepoRoot = git rev-parse --show-toplevel 2>$null
if (-not $RepoRoot) {
    Write-Error "Not inside a git repository."
    exit 3
}
$RepoRoot = $RepoRoot.Trim()

# ============================================================
# Step 1: Show current status
# ============================================================
Write-Output "## Git Checkpoint"
Write-Output ""
Write-Output "### Status"
Write-Output ""
Write-Output '```'
$StatusOutput = git status --short 2>&1
if ($StatusOutput) {
    $StatusOutput
} else {
    Write-Output "(clean — nothing to commit)"
    Write-Output '```'
    Write-Output ""
    Write-Output "> ⚠️ Nothing to commit. Working tree is clean."
    exit 0
}
Write-Output '```'
Write-Output ""

# ============================================================
# Step 2: Stage files
# ============================================================
Write-Output "### Staging"
Write-Output ""

foreach ($f in $Files) {
    $addOutput = git add $f 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Output "❌ ``git add $f`` failed: $addOutput"
        exit 3
    }
    Write-Output "✅ Staged: ``$f``"
}

Write-Output ""

# ============================================================
# Step 3: Pre-commit validation (optional)
# ============================================================
if ($ValidateTask) {
    Write-Output "### Task Validation"
    Write-Output ""

    $ScriptPath = Join-Path $RepoRoot ".agent" "scripts" "Sync-TaskList.ps1"
    if (-not (Test-Path $ScriptPath)) {
        Write-Output "⚠️ Sync-TaskList.ps1 not found — skipping validation."
    }
    elseif (-not $PlanFile) {
        Write-Output "⚠️ ``-PlanFile`` not provided — skipping task validation."
        Write-Output "> Pass ``-PlanFile '<path>'`` to enable task.md alignment and progress checks."
    }
    else {
        $validateArgs = @('-Mode', 'validate', '-PlanFile', $PlanFile)
        if ($TaskFile) { $validateArgs += @('-TaskFile', $TaskFile) }
        $validateOutput = & pwsh -NonInteractive -File $ScriptPath @validateArgs 2>&1
        if ($LASTEXITCODE -ne 0) {
            Write-Output "❌ **task.md validation failed.** Aborting commit."
            Write-Output ""
            Write-Output '```'
            $validateOutput
            Write-Output '```'
            Write-Output ""
            Write-Output "> Fix task.md alignment and re-run."

            # Unstage to avoid stale staging
            git reset HEAD -- . >$null 2>&1
            exit 1
        }
        Write-Output "✅ task.md validation passed."
        # Show advisory output (progress warnings etc.)
        $advisoryLines = $validateOutput | Where-Object { $_ -match '⚠️|still unchecked|Update task' }
        if ($advisoryLines) {
            $advisoryLines | ForEach-Object { Write-Output $_ }
        }
    }
    Write-Output ""
}

# ============================================================
# Step 4: Commit
# ============================================================
Write-Output "### Commit"
Write-Output ""

$commitOutput = git commit -m $Message 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-Output "❌ ``git commit`` failed:"
    Write-Output ""
    Write-Output '```'
    $commitOutput
    Write-Output '```'
    exit 3
}

# Show result
$LogLine = git log -n 1 --oneline 2>&1
Write-Output "✅ **$LogLine**"
Write-Output ""

# Narsil best practice reminder
Write-Output "> **Narsil Best Practice:** Set ``path=""$RepoRoot""`` in Narsil tool calls to restrict analysis to this specific project."

exit 0
