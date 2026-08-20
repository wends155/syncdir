<#
.SYNOPSIS
[DEPRECATED] This script has been archived in Phase 4 of the Skills Migration.
Its functionality is now handled natively by TARS workflows and .gemini/skills/.
Do not use this script for new development.
#>
<#
# ⚠️ REFERENCE ONLY — Workflows now use inline auto-runnable commands.
# This script is retained as documentation of the procedure it implements.
# See .agent/workflows/log-audit-generic.md for the current inline procedure.
.SYNOPSIS
    Generalized log file analysis with ripgrep fast-path.

.DESCRIPTION
    Companion script for the /log-audit workflow. Analyzes structured log files
    across any project with observability/tracing. Auto-detects log format
    (tracing-subscriber, JSON/slog, log4j, generic).

    Modes:
      Basic scan    - Severity breakdown + tail of matching entries.
      -Lifecycle    - Extract thread lifecycle, connection pool, and timing events.
      -DeepAnalysis - Statistical analysis: timing stats, churn, repetition, spans.

    Uses ripgrep (rg) when available for ~5x faster file scanning,
    with Select-String fallback for environments without rg.

.PARAMETER LogDir
    Directory containing log files. Default: 'logs' (relative to repo root).

.PARAMETER Lines
    Number of matching tail lines to display. Default: 50.

.PARAMETER Level
    Minimum severity filter: TRACE, DEBUG, INFO, WARN, or ERROR. Default: WARN.

.PARAMETER All
    Scan all log files, not just the latest.

.PARAMETER Lifecycle
    Extract and display thread lifecycle, connection pool, and timing events.

.PARAMETER DeepAnalysis
    Statistical analysis: timing stats, connection churn, repetition, span integrity.

.EXAMPLE
    .\.agent\scripts\Check-Logs.ps1 -Level TRACE

.EXAMPLE
    .\.agent\scripts\Check-Logs.ps1 -LogDir "target/logs" -Level DEBUG -Lifecycle

.EXAMPLE
    .\.agent\scripts\Check-Logs.ps1 -Level TRACE -DeepAnalysis
#>
[CmdletBinding()]
param(
    [string]$LogDir = 'logs',

    [int]$Lines = 50,

    [ValidateSet('TRACE', 'DEBUG', 'INFO', 'WARN', 'ERROR')]
    [string]$Level = 'WARN',

    [switch]$All,
    [switch]$Lifecycle,
    [switch]$DeepAnalysis
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Continue'

# --- Detect repo root ---
$RepoRoot = git rev-parse --show-toplevel 2>$null
if (-not $RepoRoot) { $RepoRoot = (Get-Location).Path }
$RepoRoot = $RepoRoot.Trim()

# --- Ripgrep availability ---
$RgAvailable = [bool](Get-Command rg -ErrorAction SilentlyContinue)

# ============================================================
# Log Format Auto-Detection
# ============================================================

# Supported formats and their severity anchor patterns
$Formats = @{
    'tracing' = @{
        # Rust tracing-subscriber: 2026-02-22T03:13:24.527Z  INFO module::path: message
        Detect  = '^\d{4}-\d{2}-\d{2}T[\d:.]+Z\s+(TRACE|DEBUG|INFO|WARN|ERROR)'
        TRACE   = '\d{4}-\d{2}-\d{2}T[\d:.]+Z\s+TRACE'
        DEBUG   = '\d{4}-\d{2}-\d{2}T[\d:.]+Z\s+DEBUG'
        INFO    = '\d{4}-\d{2}-\d{2}T[\d:.]+Z\s+INFO'
        WARN    = '\d{4}-\d{2}-\d{2}T[\d:.]+Z\s+WARN'
        ERROR   = '\d{4}-\d{2}-\d{2}T[\d:.]+Z\s+ERROR'
    }
    'json' = @{
        # JSON structured: {"level":"info","msg":"...","time":"..."}
        Detect  = '^\s*\{.*"level"\s*:'
        TRACE   = '"level"\s*:\s*"(trace|TRACE)"'
        DEBUG   = '"level"\s*:\s*"(debug|DEBUG)"'
        INFO    = '"level"\s*:\s*"(info|INFO)"'
        WARN    = '"level"\s*:\s*"(warn|WARN|warning|WARNING)"'
        ERROR   = '"level"\s*:\s*"(error|ERROR)"'
    }
    'log4j' = @{
        # log4j/logback: 2026-02-22 03:13:24 INFO [main] com.app.Main - message
        Detect  = '^\d{4}-\d{2}-\d{2}\s+[\d:]+\s+(TRACE|DEBUG|INFO|WARN|ERROR)'
        TRACE   = '^\d{4}-\d{2}-\d{2}\s+[\d:]+\s+TRACE'
        DEBUG   = '^\d{4}-\d{2}-\d{2}\s+[\d:]+\s+DEBUG'
        INFO    = '^\d{4}-\d{2}-\d{2}\s+[\d:]+\s+INFO'
        WARN    = '^\d{4}-\d{2}-\d{2}\s+[\d:]+\s+WARN'
        ERROR   = '^\d{4}-\d{2}-\d{2}\s+[\d:]+\s+ERROR'
    }
    'generic' = @{
        # Fallback: any line containing [LEVEL] or LEVEL:
        Detect  = '(TRACE|DEBUG|INFO|WARN|ERROR)'
        TRACE   = '\bTRACE\b'
        DEBUG   = '\bDEBUG\b'
        INFO    = '\bINFO\b'
        WARN    = '\bWARN\b'
        ERROR   = '\bERROR\b'
    }
}

# --- Generic lifecycle patterns (works across any framework) ---
$LifecyclePatterns = @(
    'thread spawned', 'thread started', 'thread exiting',
    'worker spawned', 'worker started', 'worker exiting',
    'initialized', 'initializing', 'init complete',
    'shutting down', 'shutdown complete', 'shut down',
    'dropping', 'teardown', 'cleanup',
    'panicked', 'failed to initialize'
)

$PoolPatterns = @(
    'connection established', 'connection closed', 'connection lost',
    'reconnect', 'reconnection', 'reconnecting',
    'evicting', 'evict', 'pool created', 'pool drained',
    'cache hit', 'cache miss', 'cache expired'
)

# Configurable timing/span patterns
$TimingPattern = 'elapsed_ms=|duration_ms=|took \d+ms|latency_ms=|elapsed=\d+'
$SpanPattern   = '\w+\.\w+\{'

# ============================================================
# Discovery
# ============================================================

# Resolve log directory
if (-not [System.IO.Path]::IsPathRooted($LogDir)) {
    $LogDir = Join-Path $RepoRoot $LogDir
}

if (-not (Test-Path $LogDir)) {
    Write-Output "# Log Audit"
    Write-Output ""
    Write-Output "> ❌ **Log directory not found:** ``$LogDir``"
    Write-Output "> Create a ``logs/`` directory or use ``-LogDir <path>``."
    exit 1
}

$logFiles = @(Get-ChildItem $LogDir -File | Sort-Object LastWriteTime -Descending)
if ($logFiles.Count -eq 0) {
    Write-Output "# Log Audit"
    Write-Output ""
    Write-Output "> ❌ **No log files found in** ``$LogDir``"
    exit 1
}

if (-not $All) {
    $logFiles = @($logFiles[0])
}

# --- Detect format from first file ---
$DetectedFormat = 'generic'
$SampleLines = Get-Content $logFiles[0].FullName -TotalCount 20 -ErrorAction SilentlyContinue
foreach ($fmtName in @('tracing', 'json', 'log4j')) {
    $detectPattern = $Formats[$fmtName].Detect
    if ($SampleLines | Select-String -Pattern $detectPattern -Quiet) {
        $DetectedFormat = $fmtName
        break
    }
}
$Fmt = $Formats[$DetectedFormat]

# Build level filter pattern (includes current level and above)
$severityOrder = @{ 'TRACE' = 0; 'DEBUG' = 1; 'INFO' = 2; 'WARN' = 3; 'ERROR' = 4 }
$minSeverity = $severityOrder[$Level]
$includedLevels = $severityOrder.Keys | Where-Object { $severityOrder[$_] -ge $minSeverity }
$levelPatterns = $includedLevels | ForEach-Object { $Fmt[$_] }
$combinedLevelPattern = ($levelPatterns -join '|')

# ============================================================
# Helper: Count pattern matches (rg fast-path)
# ============================================================
function Get-MatchCount {
    param([string]$Pattern, [string]$FilePath, [string[]]$Content)
    if ($RgAvailable) {
        $result = rg -c $Pattern $FilePath 2>$null
        if ($result) { return [int]$result } else { return 0 }
    } else {
        return ($Content | Select-String -Pattern $Pattern).Count
    }
}

function Get-MatchLines {
    param([string]$Pattern, [string]$FilePath, [string[]]$Content)
    if ($RgAvailable) {
        return @(rg -n $Pattern $FilePath 2>$null)
    } else {
        return @($Content | Select-String -Pattern $Pattern | ForEach-Object { $_.Line })
    }
}

# ============================================================
# Header
# ============================================================
Write-Output "# Log Audit Report"
Write-Output "Generated: $(Get-Date -Format 'yyyy-MM-dd HH:mm')"
Write-Output "Format: **$DetectedFormat** | Level: **$Level** | Files: **$($logFiles.Count)**"
Write-Output ""

# ============================================================
# Per-file scan
# ============================================================
$totalLines = 0; $totalTrace = 0; $totalDebug = 0
$totalInfo = 0; $totalWarn = 0; $totalError = 0
$hasErrors = $false
$allMatchedLines = @()

foreach ($file in $logFiles) {
    $filePath = $file.FullName
    $content = if (-not $RgAvailable) { Get-Content $filePath } else { @() }
    $fileLines = if ($RgAvailable) {
        $wc = rg -c '.' $filePath 2>$null; if ($wc) { [int]$wc } else { 0 }
    } else { $content.Count }

    $totalLines += $fileLines

    # Severity counts
    $tc = Get-MatchCount -Pattern $Fmt.TRACE -FilePath $filePath -Content $content
    $dc = Get-MatchCount -Pattern $Fmt.DEBUG -FilePath $filePath -Content $content
    $ic = Get-MatchCount -Pattern $Fmt.INFO  -FilePath $filePath -Content $content
    $wc2 = Get-MatchCount -Pattern $Fmt.WARN  -FilePath $filePath -Content $content
    $ec = Get-MatchCount -Pattern $Fmt.ERROR -FilePath $filePath -Content $content

    $totalTrace += $tc; $totalDebug += $dc; $totalInfo += $ic
    $totalWarn += $wc2; $totalError += $ec
    if ($ec -gt 0) { $hasErrors = $true }

    # Collect matching lines for tail display
    $matched = Get-MatchLines -Pattern $combinedLevelPattern -FilePath $filePath -Content $content
    $allMatchedLines += $matched | Select-Object -Last $Lines

    Write-Output "## $($file.Name)"
    Write-Output ""
    Write-Output "| Metric | Value |"
    Write-Output "|--------|-------|"
    Write-Output "| **Size** | $([math]::Round($file.Length / 1KB, 1)) KB |"
    Write-Output "| **Lines** | $fileLines |"
    Write-Output "| **TRACE** | $tc |"
    Write-Output "| **DEBUG** | $dc |"
    Write-Output "| **INFO** | $ic |"
    Write-Output "| **WARN** | $wc2 |"
    Write-Output "| **ERROR** | $ec |"
    Write-Output ""
}

# --- Tail display ---
Write-Output "## Last $Lines $Level+ Entries"
Write-Output ""
$tail = $allMatchedLines | Select-Object -Last $Lines
if ($tail.Count -eq 0) {
    Write-Output "> No entries at $Level level or above."
} else {
    Write-Output '```'
    $tail
    Write-Output '```'
}
Write-Output ""

# ============================================================
# Lifecycle Extraction
# ============================================================
if ($Lifecycle) {
    Write-Output "---"
    Write-Output ""
    Write-Output "## Lifecycle Events"
    Write-Output ""

    # Collect all content
    $lifecycleRegex = ($LifecyclePatterns | ForEach-Object { [regex]::Escape($_) }) -join '|'
    $poolRegex = ($PoolPatterns | ForEach-Object { [regex]::Escape($_) }) -join '|'

    $allLifecycle = @()
    $allPool = @()
    $allTiming = @()

    foreach ($file in $logFiles) {
        $fp = $file.FullName
        $ct = if (-not $RgAvailable) { Get-Content $fp } else { @() }
        $allLifecycle += Get-MatchLines -Pattern $lifecycleRegex -FilePath $fp -Content $ct
        $allPool += Get-MatchLines -Pattern $poolRegex -FilePath $fp -Content $ct
        $allTiming += Get-MatchLines -Pattern $TimingPattern -FilePath $fp -Content $ct
    }

    Write-Output "### Thread Lifecycle ($($allLifecycle.Count) events)"
    Write-Output ""
    if ($allLifecycle.Count -eq 0) {
        Write-Output "> No lifecycle events found. Run with ``-Level TRACE`` to capture."
    } else {
        Write-Output '```'
        $allLifecycle | Select-Object -First 30
        Write-Output '```'
    }
    Write-Output ""

    Write-Output "### Connection Pool ($($allPool.Count) events)"
    Write-Output ""
    if ($allPool.Count -eq 0) {
        Write-Output "> No pool events found."
    } else {
        Write-Output '```'
        $allPool | Select-Object -First 30
        Write-Output '```'
    }
    Write-Output ""

    Write-Output "### Operation Timings ($($allTiming.Count) events)"
    Write-Output ""
    if ($allTiming.Count -eq 0) {
        Write-Output "> No timing events found."
    } else {
        Write-Output '```'
        $allTiming | Select-Object -First 30
        Write-Output '```'
    }
    Write-Output ""
}

# ============================================================
# Deep Analysis
# ============================================================
if ($DeepAnalysis) {
    Write-Output "---"
    Write-Output ""
    Write-Output "## Deep Analysis"
    Write-Output ""

    # Collect all content with ANSI stripping
    $deepContent = foreach ($file in $logFiles) {
        Get-Content $file.FullName | ForEach-Object { $_ -replace '\x1B\[[0-9;]*m', '' }
    }

    # --- §A. Timing Statistics ---
    $timingMatches = @($deepContent | Select-String -Pattern '(?:elapsed_ms|duration_ms|latency_ms)=(\d+)|took (\d+)ms')
    Write-Output "### §A. Timing Statistics ($($timingMatches.Count) ops)"
    Write-Output ""
    if ($timingMatches.Count -eq 0) {
        Write-Output "> No timed operations found."
    } else {
        $nums = $timingMatches | ForEach-Object {
            $g1 = $_.Matches[0].Groups[1].Value
            $g2 = $_.Matches[0].Groups[2].Value
            if ($g1) { [int]$g1 } elseif ($g2) { [int]$g2 }
        }
        $stats = $nums | Measure-Object -Minimum -Maximum -Average
        $avg = [math]::Round($stats.Average, 1)

        Write-Output "| Metric | Value |"
        Write-Output "|--------|-------|"
        Write-Output "| **Min** | $($stats.Minimum)ms |"
        Write-Output "| **Max** | $($stats.Maximum)ms |"
        Write-Output "| **Avg** | ${avg}ms |"
        Write-Output "| **Count** | $($stats.Count) |"

        $outliers = @($nums | Where-Object { $_ -gt 100 } | Sort-Object -Descending)
        if ($outliers.Count -gt 0) {
            $pct = [math]::Round(($outliers.Count / $stats.Count) * 100, 1)
            Write-Output "| **Outliers (>100ms)** | $($outliers.Count) (${pct}%) |"
        }
        Write-Output ""
    }

    # --- §B. Connection Churn ---
    $connCount = @($deepContent | Select-String -Pattern 'connection established' -CaseSensitive:$false).Count
    $reconnCount = @($deepContent | Select-String -Pattern 'reconnect' -CaseSensitive:$false).Count
    $evictions = @($deepContent | Select-String -Pattern 'evict' -CaseSensitive:$false).Count
    $cacheHits = @($deepContent | Select-String -Pattern 'cache hit' -CaseSensitive:$false).Count

    Write-Output "### §B. Connection Churn"
    Write-Output ""
    Write-Output "| Metric | Value |"
    Write-Output "|--------|-------|"
    Write-Output "| **Connections** | $connCount |"
    Write-Output "| **Reconnects** | $reconnCount |"
    Write-Output "| **Evictions** | $evictions |"
    Write-Output "| **Cache hits** | $cacheHits |"
    Write-Output ""

    # --- §C. Repetition Analysis ---
    Write-Output "### §C. Top Repeated Messages"
    Write-Output ""
    $grouped = $deepContent |
        ForEach-Object { $_ -replace '^\d{4}-\d{2}-\d{2}T[\d:.]+Z?\s+\w+\s+', '' } |
        Where-Object { $_.Trim() -ne '' } |
        Group-Object |
        Sort-Object Count -Descending |
        Select-Object -First 10

    if ($grouped.Count -eq 0) {
        Write-Output "> No repeated messages found."
    } else {
        Write-Output "| Count | Message |"
        Write-Output "|-------|---------|"
        foreach ($g in $grouped) {
            $msg = $g.Name
            if ($msg.Length -gt 80) { $msg = $msg.Substring(0, 80) + '...' }
            Write-Output "| $($g.Count) | ``$msg`` |"
        }
        Write-Output ""
    }

    # --- §D. Span Integrity ---
    Write-Output "### §D. Span Integrity"
    Write-Output ""
    $spanMatches = @($deepContent | Select-String -Pattern $SpanPattern)
    if ($spanMatches.Count -eq 0) {
        Write-Output "> No tracing spans found."
    } else {
        $spans = $spanMatches | ForEach-Object {
            if ($_.Matches[0].Value -match '(\w+\.\w+)\{') { $Matches[1] }
        } | Group-Object | Sort-Object Count -Descending

        Write-Output "| Count | Span |"
        Write-Output "|-------|------|"
        foreach ($s in $spans) {
            Write-Output "| $($s.Count) | ``$($s.Name)`` |"
        }
        Write-Output ""
    }
}

# ============================================================
# Summary
# ============================================================
Write-Output "---"
Write-Output ""
Write-Output "## Summary"
Write-Output ""
Write-Output "| Metric | Value |"
Write-Output "|--------|-------|"
Write-Output "| **Files** | $($logFiles.Count) |"
Write-Output "| **Total lines** | $totalLines |"
Write-Output "| **Format** | $DetectedFormat |"
Write-Output "| **TRACE** | $totalTrace |"
Write-Output "| **DEBUG** | $totalDebug |"
Write-Output "| **INFO** | $totalInfo |"
Write-Output "| **WARN** | $totalWarn |"
Write-Output "| **ERROR** | $totalError |"
Write-Output "| **rg available** | $RgAvailable |"
Write-Output ""

if ($hasErrors) {
    Write-Output "> ❌ **Errors detected.** Use the /log-audit workflow for deep analysis."
} else {
    Write-Output "> ✅ **No errors found.**"
}
Write-Output ""
Write-Output "> **Narsil Best Practice:** Set ``path=""$RepoRoot""`` in Narsil tool calls to restrict analysis to this specific project."

exit $(if ($hasErrors) { 1 } else { 0 })
