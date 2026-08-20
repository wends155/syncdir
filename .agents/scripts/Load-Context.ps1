<#
.SYNOPSIS
[DEPRECATED] This script has been archived in Phase 4 of the Skills Migration.
Its functionality is now handled natively by TARS workflows and .gemini/skills/.
Do not use this script for new development.
#>
<#
# ⚠️ REFERENCE ONLY — Workflows now use inline auto-runnable commands.
# This script is retained as documentation of the procedure it implements.
# See .agent/workflows/issue.md, feature.md, plan-making.md, toolcheck.md for the current inline procedures.
.SYNOPSIS
    Loads project context for /issue and /plan-making workflows.

.DESCRIPTION
    Gathers prerequisite context docs, git history, and TODO markers into a single
    structured markdown dump. Saves agent turns by replacing 5-7 repetitive tool calls
    with one script invocation.

    Modes:
      issue - Loads architecture.md, context.md, git log, TODOs, issue report template.
      plan  - All of the above + .agent/rules/coding-standard.md, .agent/rules/ipr.md, plan header template.

.PARAMETER Mode
    The workflow mode: 'issue' or 'plan'.

.EXAMPLE
    .\.agent\scripts\Load-Context.ps1 -Mode issue

.EXAMPLE
    .\.agent\scripts\Load-Context.ps1 -Mode plan
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('issue', 'plan')]
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

# --- Helper: Print a doc file if it exists ---
function Show-DocIfExists {
    param([string]$Name)

    $FilePath = Join-Path $RepoRoot $Name
    Write-Output ""
    Write-Output "## $Name"
    Write-Output ""
    if (Test-Path $FilePath) {
        Get-Content $FilePath -Raw
    }
    else {
        Write-Output "> **Not found:** ``$Name`` does not exist in ``$RepoRoot``"
    }
}

# --- Header ---
$Date = Get-Date -Format 'yyyy-MM-dd'
Write-Output "# Context Dump ($Mode mode)"
Write-Output "Generated: $Date"

# --- Common context (both modes) ---
Show-DocIfExists 'architecture.md'
Show-DocIfExists 'context.md'

# --- Plan-only extras ---
if ($Mode -eq 'plan') {
    Show-DocIfExists '.agent/rules/coding-standard.md'
    Show-DocIfExists '.agent/rules/ipr.md'
}

# --- Recent commits ---
Write-Output ""
Write-Output "## Recent Commits (last 20)"
Write-Output ""

$GitAvailable = Get-Command git -ErrorAction SilentlyContinue
if ($GitAvailable) {
    Write-Output '```'
    git log -n 20 --oneline 2>$null
    if ($LASTEXITCODE -ne 0) {
        Write-Output "(no git history available)"
    }
    Write-Output '```'
}
else {
    Write-Output "> **git** not found on PATH."
}

# --- TODO/FIXME/HACK markers ---
Write-Output ""
Write-Output "## TODO/FIXME/HACK Markers"
Write-Output ""

$RgAvailable = Get-Command rg -ErrorAction SilentlyContinue
if ($RgAvailable) {
    $RgOutput = rg -n "TODO|FIXME|HACK" --type-add "code:*.{rs,go,ts,js,svelte,py,toml,yaml,yml}" --type code $RepoRoot 2>$null
    if ($RgOutput) {
        Write-Output '```'
        $RgOutput
        Write-Output '```'
    }
    else {
        Write-Output "> None found."
    }
}
else {
    Write-Warning "ripgrep (rg) not found. Falling back to Select-String."
    $Patterns = @('TODO', 'FIXME', 'HACK')
    $Extensions = @('*.rs', '*.go', '*.ts', '*.js', '*.svelte', '*.py')
    $Results = Get-ChildItem -Path $RepoRoot -Recurse -Include $Extensions -File -ErrorAction SilentlyContinue |
        Select-String -Pattern $Patterns -ErrorAction SilentlyContinue
    if ($Results) {
        Write-Output '```'
        $Results | ForEach-Object { "$($_.RelativePath($RepoRoot)):$($_.LineNumber): $($_.Line.Trim())" }
        Write-Output '```'
    }
    else {
        Write-Output "> None found."
    }
}

# --- Pre-filled template ---
Write-Output ""
Write-Output "---"
Write-Output ""

if ($Mode -eq 'issue') {
    Write-Output "## Issue Report Template"
    Write-Output ""
    Write-Output @"
``````markdown
## 🐛 Issue Report

| Field          | Value                        |
|----------------|------------------------------|
| **Type**       | bug / feature / chore / docs |
| **Component**  | [affected area]              |
| **Severity**   | critical / high / med / low  |
| **Filed**      | $Date                        |

### Description
[Clear restatement of the issue in the user's own words]

### Investigation Findings
- **Affected files:** [list of files with links]
- **Root cause analysis:** [diagnosis based on investigation]
- **Impact scope:** narrow (single function) / module / cross-cutting
- **Related history:** [anything from context.md or git log]
- **Recent changes:** [relevant commits, if any]
- **Test coverage:** [existing tests in this area, pass/fail status]

### Open Questions
- [Any ambiguities or unknowns that need user clarification]

### Recommended Severity
[Confirm or adjust the initial severity estimate with reasoning]
``````
"@
}
elseif ($Mode -eq 'plan') {
    Write-Output "## Plan Header Template"
    Write-Output ""
    Write-Output @"
``````markdown
| Field | Value |
|-------|-------|
| **Role** | Architect |
| **Date** | $Date |
| **Scope** | [One-line summary of what changes] |
| **Tier** | S / M / L |
``````
"@
}

Write-Output ""
Write-Output "---"
Write-Output "> **Narsil Best Practice:** Set ``path=""$RepoRoot""`` in Narsil tool calls to restrict analysis to this specific project."
Write-Output "> Context loading complete. Proceed with the workflow."
