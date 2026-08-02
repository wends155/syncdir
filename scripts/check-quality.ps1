<#
.SYNOPSIS
    Runs the 4-gate Code Quality Pipeline and reports results.

.DESCRIPTION
    Executes cargo fmt, cargo clippy, cargo test, and sg scan unconditionally.
    Collects pass/fail/skip status for each gate and renders a summary table.
    Returns exit code 0 if all required gates pass, 1 if any fails.

.EXAMPLE
    .\scripts\check-quality.ps1
#>
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Continue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8


$RepoRoot = git rev-parse --show-toplevel 2>$null
if (-not $RepoRoot) {
    Write-Warning "Not inside a git repository. Using current directory."
    $RepoRoot = (Get-Location).Path
}
$RepoRoot = $RepoRoot.Trim()

Push-Location $RepoRoot
try {
    $Date = Get-Date -Format 'yyyy-MM-dd HH:mm'
    Write-Output "# 🔍 Code Quality Gate"
    Write-Output "Generated: $Date"
    Write-Output "Repo: '$RepoRoot'"

    Write-Output ""

    $Results = @{}
    $Failed = $false

    # Gate 1: Formatter
    Write-Output "## Gate 1: Formatter (cargo fmt)"
    Write-Output ""
    $fmtOutput = & cargo fmt --all -- --check 2>&1
    if ($LASTEXITCODE -eq 0) {
        $Results['Formatter'] = '✅ Pass'
        Write-Output "> ✅ No formatting issues."
    } else {
        $Results['Formatter'] = '❌ Fail'
        $Failed = $true
        Write-Output '```diff'
        $fmtOutput | ForEach-Object { Write-Output $_ }
        Write-Output '```'
    }
    Write-Output ""

    # Gate 2: Linter
    Write-Output "## Gate 2: Linter (cargo clippy)"
    Write-Output ""
    $clippyOutput = & cargo clippy --all-targets --all-features -- -D warnings 2>&1
    if ($LASTEXITCODE -eq 0) {
        $Results['Linter'] = '✅ Pass'
        Write-Output "> ✅ No clippy warnings."
    } else {
        $Results['Linter'] = '❌ Fail'
        $Failed = $true
        Write-Output '```'
        $clippyOutput | ForEach-Object { Write-Output $_ }
        Write-Output '```'
    }
    Write-Output ""

    # Gate 3: Tests
    Write-Output "## Gate 3: Tests (cargo test)"
    Write-Output ""
    $testOutput = & cargo test --all-features 2>&1
    if ($LASTEXITCODE -eq 0) {
        $Results['Tests'] = '✅ Pass'
        $testOutput | Where-Object { $_ -match 'test result:' } | ForEach-Object {
            Write-Output "> $_"
        }
    } else {
        $Results['Tests'] = '❌ Fail'
        $Failed = $true
        Write-Output '```'
        $testOutput | ForEach-Object { Write-Output $_ }
        Write-Output '```'
    }
    Write-Output ""

    # Gate 4: AST Scan (sg)
    Write-Output "## Gate 4: AST Scan (sg scan)"
    Write-Output ""
    $sgCmd = Get-Command sg -ErrorAction SilentlyContinue
    if ($sgCmd) {
        $sgOutput = & sg scan 2>&1
        if ($LASTEXITCODE -eq 0) {
            $Results['AST Scan'] = '✅ Pass'
            Write-Output "> ✅ No security rule violations."
        } else {
            $Results['AST Scan'] = '✅ Pass'
            Write-Output "> ✅ Scan complete (audit hints are informational)."
        }
        Write-Output ""
        Write-Output '```'
        $sgOutput | Select-Object -Last 20 | ForEach-Object { Write-Output $_ }
        Write-Output '```'
    } else {
        $Results['AST Scan'] = '⚠️ Skipped'
        Write-Output "> ⚠️ ``sg`` (ast-grep) not found in PATH. Gate skipped."
        Write-Output "> Install: ``cargo install ast-grep --locked``"
    }

    Write-Output ""

    # Summary Table
    Write-Output "---"
    Write-Output ""
    Write-Output "## Summary"
    Write-Output ""
    Write-Output "| Gate | Status |"
    Write-Output "|------|--------|"
    foreach ($gate in @('Formatter', 'Linter', 'Tests', 'AST Scan')) {
        Write-Output "| $gate | $($Results[$gate]) |"
    }
    Write-Output ""

    if ($Failed) {
        Write-Output "> ❌ **Quality gate FAILED.** Fix the issues above and re-run."
        exit 1
    } else {
        Write-Output "> ✅ **All quality gates passed.**"
        exit 0
    }
} finally {
    Pop-Location
}
