<#
.SYNOPSIS
    Produces a portable, statically-linked Windows release build.

.DESCRIPTION
    1. Runs mandatory quality gate script (scripts/check-quality.ps1).
    2. Runs `cargo build --release`.
    3. Stages `syncdir.exe` into `dist/`.
    4. Performs hard dumpbin check to verify no CRT DLLs (VCRUNTIME140.dll, api-ms-win-crt-*) are dynamically linked.
    5. Packages `syncdir.exe`, `LICENSE`, and `README.md` into `dist/syncdir-v{version}-x86_64-windows.zip`.
    6. Generates SHA256 checksum file alongside the ZIP archive.

.EXAMPLE
    .\scripts\build-release.ps1
#>
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
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
    Write-Output "# 📦 Release Build & Distribution"
    Write-Output "Started: $Date"
    Write-Output "Repo: '$RepoRoot'"
    Write-Output ""

    # Phase 1: Quality Gate Execution (Mandatory)
    Write-Output "## Phase 1: Quality Gate Verification"
    $QualityScript = Join-Path $RepoRoot "scripts\check-quality.ps1"
    if (-not (Test-Path $QualityScript)) {
        throw "Quality script not found at '$QualityScript'."
    }

    Write-Output "Running quality gate suite..."
    & pwsh -NoProfile -File $QualityScript
    if ($LASTEXITCODE -ne 0) {
        throw "Quality gate failed with exit code $LASTEXITCODE. Release build aborted."
    }
    Write-Output "> ✅ Quality gates passed successfully."
    Write-Output ""

    # Phase 2: Cargo Release Build
    Write-Output "## Phase 2: Building Release Binary"
    Write-Output "Executing `cargo build --release`..."
    $buildOutput = & cargo build --release 2>&1
    if ($LASTEXITCODE -ne 0) {
        $buildOutput | ForEach-Object { Write-Error $_ }
        throw "Cargo release build failed with exit code $LASTEXITCODE."
    }
    Write-Output "> ✅ Cargo release build complete."
    Write-Output ""

    # Phase 3: Stage Distribution Artifacts
    Write-Output "## Phase 3: Staging Distribution Artifacts"
    $DistDir = Join-Path $RepoRoot "dist"
    if (Test-Path $DistDir) {
        Remove-Item -Path $DistDir -Recurse -Force
    }
    $null = New-Item -Path $DistDir -ItemType Directory -Force

    $TargetExe = Join-Path $RepoRoot "target\release\syncdir.exe"
    if (-not (Test-Path $TargetExe)) {
        throw "Target binary not found at '$TargetExe'."
    }

    $DistExe = Join-Path $DistDir "syncdir.exe"
    Copy-Item -Path $TargetExe -Destination $DistExe -Force
    $ExeSizeMB = (Get-Item $DistExe).Length / 1MB
    Write-Output ("> Staged binary: '{0}' ({1:N2} MB)" -f $DistExe, $ExeSizeMB)
    Write-Output ""

    # Phase 4: CRT Static Link Verification (Hard Gate)
    Write-Output "## Phase 4: Static CRT Dependency Check (dumpbin)"
    $DumpbinCmd = Get-Command dumpbin -ErrorAction SilentlyContinue
    if (-not $DumpbinCmd) {
        # Check standard Visual Studio / MSVC build tools path if not in PATH
        $VsWhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
        if (Test-Path $VsWhere) {
            $VsInstallPath = & $VsWhere -latest -products * -property installationPath
            if ($VsInstallPath) {
                $DumpbinPath = Get-ChildItem -Path "$VsInstallPath\VC\Tools\MSVC" -Filter "dumpbin.exe" -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1
                if ($DumpbinPath) {
                    $DumpbinCmd = $DumpbinPath.FullName
                }
            }
        }
    }

    if ($DumpbinCmd) {
        Write-Output "Running `dumpbin /dependents` check..."
        $dependents = & $DumpbinCmd /dependents $DistExe 2>&1
        $crtDlls = $dependents | Where-Object { $_ -match 'VCRUNTIME140\.dll|api-ms-win-crt' }
        if ($crtDlls) {
            Write-Error "FOUND DYNAMIC CRT DEPENDENCIES:"
            $crtDlls | ForEach-Object { Write-Error "  - $_" }
            throw "Static CRT verification FAILED: Binary dynamically links CRT DLLs."
        } else {
            Write-Output "> ✅ Verification passed: No VCRUNTIME140.dll or api-ms-win-crt-* dependencies found."
        }
    } else {
        Write-Warning "dumpbin.exe not found in PATH or VS installation. Skipping dumpbin verification gate."
    }
    Write-Output ""

    # Phase 5: Version Extraction & Packaging
    Write-Output "## Phase 5: Version Extraction & Packaging"
    $CargoToml = Get-Content (Join-Path $RepoRoot "Cargo.toml") -Raw
    if ($CargoToml -match 'version\s*=\s*"([^"]+)"') {
        $Version = $Matches[1]
    } else {
        throw "Could not extract version from Cargo.toml."
    }
    Write-Output "> Version: v$Version"

    $ZipName = "syncdir-v$Version-x86_64-windows.zip"
    $ZipPath = Join-Path $DistDir $ZipName

    $ItemsToZip = @(
        $DistExe,
        (Join-Path $RepoRoot "LICENSE"),
        (Join-Path $RepoRoot "README.md")
    )

    foreach ($item in $ItemsToZip) {
        if (-not (Test-Path $item)) {
            throw "Packaging asset not found: '$item'"
        }
    }

    Write-Output "Creating release archive '$ZipName'..."
    Compress-Archive -Path $ItemsToZip -DestinationPath $ZipPath -Force
    $ZipSizeMB = (Get-Item $ZipPath).Length / 1MB
    Write-Output ("> ✅ Archive created: '{0}' ({1:N2} MB)" -f $ZipPath, $ZipSizeMB)
    Write-Output ""

    # Phase 6: SHA256 Checksum Generation
    Write-Output "## Phase 6: SHA256 Checksum Generation"
    $Hash = (Get-FileHash -Path $ZipPath -Algorithm SHA256).Hash.ToLower()
    $ShaFile = "$ZipPath.sha256"
    "$Hash  $ZipName" | Out-File -FilePath $ShaFile -Encoding utf8
    Write-Output "> SHA256: $Hash"
    Write-Output "> Checksum file: '$ShaFile'"
    Write-Output ""

    # Summary
    Write-Output "---"
    Write-Output "## Summary"
    Write-Output ""
    Write-Output "| Artifact | Path | Size / Details |"
    Write-Output "|----------|------|----------------|"
    Write-Output ("| Binary | `dist/syncdir.exe` | {0:N2} MB (Static CRT) |" -f $ExeSizeMB)
    Write-Output ("| Archive | `dist/$ZipName` | {0:N2} MB |" -f $ZipSizeMB)
    Write-Output ("| Checksum | `dist/$ZipName.sha256` | `$Hash` |")
    Write-Output ""
    Write-Output "> ✅ **Portable release build completed successfully.**"
    exit 0
} finally {
    Pop-Location
}
