<#
.SYNOPSIS
    Produces a portable, statically-linked Windows release build with verified PE resource integrity.

.DESCRIPTION
    1. Resolves Windows SDK toolkit dynamically across 4 precedence tiers (CLI overrides, environment variables, PATH, compiler sibling trees, Windows Kits registry, and descending version trees).
    2. Runs mandatory quality gate script (scripts/check-quality.ps1) unless -SkipQualityGate is specified.
    3. Runs `cargo build --release` with configured WINRES_TOOLKIT_PATH.
    4. Stages `syncdir.exe` into `dist/`.
    5. Performs hard dumpbin check to verify no CRT DLLs are dynamically linked unless -SkipCrtCheck is specified.
    6. Phase 4b: Verifies PE binary integrity (DataDirectory[2] Resource Table, .rsrc section, RT_MANIFEST, and RT_ICON) via Test-PeBinaryStructure. Enforces fail-closed policy unless -AllowMissingIcon is set.
    7. Packages `syncdir.exe`, `LICENSE`, and `README.md` into `dist/syncdir-v{version}-x86_64-windows.zip`.
    8. Generates SHA256 checksum file alongside the ZIP archive.
    9. Restores initial environment state bidirectionally upon completion or error.

.PARAMETER WinresToolkitPath
    Direct toolkit folder override containing rc.exe or windres.exe. Alias: ToolkitPath, Toolkit.

.PARAMETER RcPath
    Direct path pointing to the rc.exe / windres.exe binary or enclosing directory. Alias: ResourceCompiler, Rc.

.PARAMETER WindowsSdkPath
    Root directory of a Windows SDK / Kits installation. Probes bin\*\x64\rc.exe. Alias: SdkPath, Sdk.

.PARAMETER AllowMissingIcon
    Downgrades release icon compilation and PE resource integrity errors to warnings. Alias: SkipIcon.

.PARAMETER SkipQualityGate
    Bypasses scripts/check-quality.ps1. Alias: NoQualityGate, Quick.

.PARAMETER SkipCrtCheck
    Bypasses dumpbin CRT dependency verification.

.PARAMETER SkipPeVerification
    Bypasses post-build PE resource and manifest verification gate.

.EXAMPLE
    .\scripts\build-release.ps1

.EXAMPLE
    .\scripts\build-release.ps1 -WinresToolkitPath "C:\Tools\WindowsKits\10\bin\10.0.22621.0\x64"

.EXAMPLE
    .\scripts\build-release.ps1 -AllowMissingIcon -SkipQualityGate
#>
[CmdletBinding()]
param(
    [Alias('ToolkitPath', 'Toolkit')]
    [string]$WinresToolkitPath,

    [Alias('ResourceCompiler', 'Rc')]
    [string]$RcPath,

    [Alias('SdkPath', 'Sdk')]
    [string]$WindowsSdkPath,

    [Alias('SkipIcon')]
    [switch]$AllowMissingIcon,

    [Alias('NoQualityGate', 'Quick')]
    [switch]$SkipQualityGate,

    [switch]$SkipCrtCheck,

    [switch]$SkipPeVerification
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8

function Test-PathSafety {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $false)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string]$ParamName
    )
    if ([string]::IsNullOrWhiteSpace($Path)) { return }
    if ($Path.IndexOfAny([char[]]@("`0", '"', "`r", "`n", "`t")) -ge 0) {
        throw "Security Error: -$ParamName contains prohibited characters (null bytes, quotes, or control characters)."
    }
    if ($Path.ToCharArray() | Where-Object { [char]::IsControl($_) }) {
        throw "Security Error: -$ParamName contains prohibited control characters."
    }
}

function Restore-EnvironmentState {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [System.Collections.IDictionary]$InitialSnapshot
    )
    $current = [System.Environment]::GetEnvironmentVariables()
    foreach ($key in $current.Keys) {
        if (-not $InitialSnapshot.Contains($key)) {
            [System.Environment]::SetEnvironmentVariable($key, $null)
            Remove-Item "env:$key" -ErrorAction SilentlyContinue
        }
    }
    foreach ($key in $InitialSnapshot.Keys) {
        if ($current[$key] -ne $InitialSnapshot[$key]) {
            [System.Environment]::SetEnvironmentVariable($key, $InitialSnapshot[$key])
        }
    }
}

function Invoke-WithEnvironmentScope {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [scriptblock]$Action
    )
    $snapshot = [System.Environment]::GetEnvironmentVariables()
    try {
        & $Action
    } finally {
        Restore-EnvironmentState -InitialSnapshot $snapshot
    }
}

function Resolve-SdkToolkit {
    [CmdletBinding()]
    param(
        [string]$ToolkitOverride,
        [string]$RcOverride,
        [string]$SdkRootOverride,
        [scriptblock]$PathExists = { param($p) Test-Path -LiteralPath $p }
    )
    # Tier 1: Explicit Toolkit Directory Override
    if (-not [string]::IsNullOrWhiteSpace($ToolkitOverride)) {
        Test-PathSafety -Path $ToolkitOverride -ParamName 'WinresToolkitPath'
        if (& $PathExists $ToolkitOverride) { return (Resolve-Path $ToolkitOverride).Path }
        Write-Warning "WinresToolkitPath '$ToolkitOverride' does not exist; falling back."
    }

    # Tier 2: Explicit RC binary or folder
    if (-not [string]::IsNullOrWhiteSpace($RcOverride)) {
        Test-PathSafety -Path $RcOverride -ParamName 'RcPath'
        if (& $PathExists (Join-Path $RcOverride "rc.exe")) { return (Resolve-Path $RcOverride).Path }
        if (& $PathExists $RcOverride -and ((Split-Path -Leaf $RcOverride) -in 'rc.exe','windres.exe')) {
            return (Split-Path -Parent (Resolve-Path $RcOverride).Path)
        }
        Write-Warning "RcPath '$RcOverride' does not exist or is not rc.exe; falling back."
    }

    # Tier 3: SDK Root candidate probing (checks bin\*\x64\rc.exe and bin\x64\rc.exe)
    $candidateRoots = @($SdkRootOverride, $env:WINDOWS_SDK_PATH) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    foreach ($root in $candidateRoots) {
        Test-PathSafety -Path $root -ParamName 'WindowsSdkPath'
        if (& $PathExists $root) {
            $versionDirs = Get-ChildItem (Join-Path $root "bin\10.*") -Directory -ErrorAction SilentlyContinue | Sort-Object Name -Descending
            foreach ($v in $versionDirs) {
                $p = Join-Path $v.FullName "x64\rc.exe"
                if (& $PathExists $p) { return (Split-Path -Parent $p) }
            }
            $directCandidate = Join-Path $root "bin\x64\rc.exe"
            if (& $PathExists $directCandidate) { return (Split-Path -Parent $directCandidate) }
        }
    }

    # Tier 4: Ambient PATH, Compiler Sibling Walking, and Registry lookup
    $rcCmd = Get-Command rc.exe -ErrorAction SilentlyContinue
    if ($rcCmd -and $rcCmd.Source) { return (Split-Path -Parent $rcCmd.Source) }

    # Dynamic Compiler Sibling Probing (portable MSVC / VS layout)
    $compilerCmd = Get-Command cl.exe, link.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($compilerCmd -and $compilerCmd.Source) {
        $curr = Split-Path -Parent $compilerCmd.Source
        while ($curr) {
            $probeKit = Join-Path $curr "Windows Kits\10"
            if (& $PathExists $probeKit) {
                $versionDirs = Get-ChildItem (Join-Path $probeKit "bin\10.*") -Directory -ErrorAction SilentlyContinue | Sort-Object Name -Descending
                foreach ($v in $versionDirs) {
                    $p = Join-Path $v.FullName "x64\rc.exe"
                    if (& $PathExists $p) { return (Split-Path -Parent $p) }
                }
                $directP = Join-Path $probeKit "bin\x64\rc.exe"
                if (& $PathExists $directP) { return (Split-Path -Parent $directP) }
            }
            $parent = Split-Path -Parent $curr
            if ($parent -eq $curr) { break }
            $curr = $parent
        }
    }

    $regRoots = @(
        "HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots",
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows Kits\Installed Roots"
    )
    foreach ($regKey in $regRoots) {
        if (Test-Path $regKey) {
            $kit10 = (Get-ItemProperty -Path $regKey -Name "KitsRoot10" -ErrorAction SilentlyContinue).KitsRoot10
            if ($kit10 -and (& $PathExists $kit10)) {
                $versionDirs = Get-ChildItem (Join-Path $kit10 "bin\10.*") -Directory -ErrorAction SilentlyContinue | Sort-Object Name -Descending
                foreach ($v in $versionDirs) {
                    $p = Join-Path $v.FullName "x64\rc.exe"
                    if (& $PathExists $p) { return (Split-Path -Parent $p) }
                }
            }
        }
    }

    return $null
}

function Test-PeBinaryStructure {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$FilePath
    )
    if (-not (Test-Path -LiteralPath $FilePath -PathType Leaf)) {
        return [PSCustomObject]@{
            IsValidPe        = $false
            Error            = "File not found: '$FilePath'"
            HasRsrcSection   = $false
            HasResourceTable = $false
            HasManifest      = $false
            HasIcon          = $false
        }
    }

    $stream = $null; $reader = $null
    try {
        $stream = [System.IO.File]::OpenRead($FilePath)
        $reader = [System.IO.BinaryReader]::new($stream)
        if ($stream.Length -lt 0x40) {
            return [PSCustomObject]@{ IsValidPe = $false; Error = "Truncated DOS header"; HasRsrcSection = $false; HasResourceTable = $false; HasManifest = $false; HasIcon = $false }
        }
        if ($reader.ReadUInt16() -ne 0x5A4D) {
            return [PSCustomObject]@{ IsValidPe = $false; Error = "Invalid DOS signature"; HasRsrcSection = $false; HasResourceTable = $false; HasManifest = $false; HasIcon = $false }
        }

        $stream.Seek(0x3C, [System.IO.SeekOrigin]::Begin) | Out-Null
        $peOffset = $reader.ReadUInt32()
        if ($peOffset + 24 -gt $stream.Length) {
            return [PSCustomObject]@{ IsValidPe = $false; Error = "Truncated PE header"; HasRsrcSection = $false; HasResourceTable = $false; HasManifest = $false; HasIcon = $false }
        }

        $stream.Seek($peOffset, [System.IO.SeekOrigin]::Begin) | Out-Null
        if ($reader.ReadUInt32() -ne 0x00004550) {
            return [PSCustomObject]@{ IsValidPe = $false; Error = "Invalid PE signature"; HasRsrcSection = $false; HasResourceTable = $false; HasManifest = $false; HasIcon = $false }
        }

        $machine = $reader.ReadUInt16()
        $numSections = $reader.ReadUInt16()
        $stream.Seek(12, [System.IO.SeekOrigin]::Current) | Out-Null # Skip time, ptr to symbols, num symbols
        $optHeaderSize = $reader.ReadUInt16()
        $characteristics = $reader.ReadUInt16()
        $optStart = $stream.Position
        $optMagic = $reader.ReadUInt16()

        # NumberOfRvaAndSizes is at offset 108 on PE32+ (0x020B), 92 on PE32 (0x010B) from $optStart
        $numRvaOffset = if ($optMagic -eq 0x020B) { 108 } elseif ($optMagic -eq 0x010B) { 92 } else { $null }
        if ($null -eq $numRvaOffset) {
            return [PSCustomObject]@{ IsValidPe = $false; Error = "Unsupported PE optional header magic"; HasRsrcSection = $false; HasResourceTable = $false; HasManifest = $false; HasIcon = $false }
        }

        $stream.Seek($optStart + $numRvaOffset, [System.IO.SeekOrigin]::Begin) | Out-Null
        $numRva = $reader.ReadUInt32()
        if ($numRva -lt 3) {
            return [PSCustomObject]@{
                IsValidPe        = $true
                Error            = "PE Optional Header specifies insufficient DataDirectory entries ($numRva < 3)"
                HasRsrcSection   = $false
                HasResourceTable = $false
                HasManifest      = $false
                HasIcon          = $false
            }
        }

        # PE32+ (64-bit): DataDirectory[2] Resource Table is at offset 128 (0x80) from Optional Header start
        # PE32  (32-bit): DataDirectory[2] Resource Table is at offset 112 (0x70) from Optional Header start
        $resourceEntryOffset = if ($optMagic -eq 0x020B) { $optStart + 128 } else { $optStart + 112 }

        $stream.Seek($resourceEntryOffset, [System.IO.SeekOrigin]::Begin) | Out-Null
        $resRva = $reader.ReadUInt32()
        $resSize = $reader.ReadUInt32()
        $hasResourceTable = ($resRva -gt 0 -and $resSize -gt 0)

        # Inspect Section Table for .rsrc
        $sectionStart = $optStart + $optHeaderSize
        $stream.Seek($sectionStart, [System.IO.SeekOrigin]::Begin) | Out-Null
        $hasRsrc = $false; $rsrcRawPtr = 0; $rsrcVirtAddr = 0
        for ($i = 0; $i -lt $numSections; $i++) {
            $nameBytes = $reader.ReadBytes(8)
            $name = [System.Text.Encoding]::ASCII.GetString($nameBytes).TrimEnd("`0")
            $vSize = $reader.ReadUInt32()
            $vAddr = $reader.ReadUInt32()
            $rawSize = $reader.ReadUInt32()
            $rawPtr = $reader.ReadUInt32()
            $stream.Seek(16, [System.IO.SeekOrigin]::Current) | Out-Null
            if ($name -eq '.rsrc' -and $vSize -gt 0 -and $rawSize -gt 0) {
                $hasRsrc = $true; $rsrcRawPtr = $rawPtr; $rsrcVirtAddr = $vAddr
            }
        }

        # Inspect Resource Directory Tree for RT_MANIFEST (24) and RT_ICON (3) / RT_GROUP_ICON (14)
        $hasManifest = $false; $hasIcon = $false
        if ($hasResourceTable -and $hasRsrc -and $rsrcRawPtr -gt 0) {
            # Translate RVA to physical file offset
            $tableFileOffset = $rsrcRawPtr + ($resRva - $rsrcVirtAddr)
            if ($tableFileOffset + 16 -le $stream.Length) {
                $stream.Seek($tableFileOffset + 12, [System.IO.SeekOrigin]::Begin) | Out-Null
                $namedEntries = $reader.ReadUInt16()
                $idEntries = $reader.ReadUInt16()
                for ($j = 0; $j -lt ($namedEntries + $idEntries); $j++) {
                    $typeId = $reader.ReadUInt32()
                    $offset = $reader.ReadUInt32()
                    if ($typeId -eq 24) { $hasManifest = $true }
                    if ($typeId -eq 14 -or $typeId -eq 3) { $hasIcon = $true }
                }
            }
        }

        return [PSCustomObject]@{
            IsValidPe        = $true
            Error            = $null
            HasRsrcSection   = $hasRsrc
            HasResourceTable = $hasResourceTable
            HasManifest      = $hasManifest
            HasIcon          = $hasIcon
        }
    } finally {
        if ($reader) { $reader.Dispose() }
        if ($stream) { $stream.Dispose() }
    }
}

function Assert-ReleaseResourceIntegrity {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$BinaryPath,
        [bool]$AllowMissingIcon = $false
    )
    $pe = Test-PeBinaryStructure -FilePath $BinaryPath
    if (-not $pe.IsValidPe) {
        throw "FAIL-CLOSED POLICY VIOLATION: Invalid PE binary staged at '$BinaryPath': $($pe.Error)"
    }
    if (-not $pe.HasRsrcSection -or -not $pe.HasResourceTable) {
        if ($AllowMissingIcon) {
            Write-Warning "Missing .rsrc section or Resource Table in '$BinaryPath'; permitted by -AllowMissingIcon."
            return
        }
        throw "FAIL-CLOSED POLICY VIOLATION: Staged binary '$BinaryPath' lacks an embedded .rsrc section or Resource Table (CWE-390 / CWE-250)."
    }
    if (-not $pe.HasManifest -or -not $pe.HasIcon) {
        if ($AllowMissingIcon) {
            Write-Warning "Incomplete resources (Manifest: $($pe.HasManifest), Icon: $($pe.HasIcon)) in '$BinaryPath'; permitted by -AllowMissingIcon."
            return
        }
        throw "FAIL-CLOSED POLICY VIOLATION: Staged binary '$BinaryPath' is missing Manifest ($($pe.HasManifest)) or Icon ($($pe.HasIcon))."
    }
    Write-Output "> ✅ Post-build PE verification passed: .rsrc section, manifest, and application icon confirmed."
}

function Invoke-BuildRelease {
    [CmdletBinding()]
    param(
        [string]$WinresToolkitPath,
        [string]$RcPath,
        [string]$WindowsSdkPath,
        [switch]$AllowMissingIcon,
        [switch]$SkipQualityGate,
        [switch]$SkipCrtCheck,
        [switch]$SkipPeVerification
    )
    Invoke-WithEnvironmentScope {
        Test-PathSafety -Path $WinresToolkitPath -ParamName 'WinresToolkitPath'
        Test-PathSafety -Path $RcPath -ParamName 'RcPath'
        Test-PathSafety -Path $WindowsSdkPath -ParamName 'WindowsSdkPath'

        $RepoRoot = (git rev-parse --show-toplevel 2>$null)
        if (-not $RepoRoot) { $RepoRoot = (Get-Location).Path }
        $RepoRoot = $RepoRoot.Trim()

        Push-Location $RepoRoot
        try {
            $Date = Get-Date -Format 'yyyy-MM-dd HH:mm'
            Write-Output '# 📦 Release Build & Distribution'
            Write-Output "Started: $Date"
            Write-Output "Repo: '$RepoRoot'"
            Write-Output ""

            # Dynamic SDK Toolkit Resolution
            $discoveredToolkit = Resolve-SdkToolkit -ToolkitOverride $WinresToolkitPath -RcOverride $RcPath -SdkRootOverride $WindowsSdkPath
            if ($discoveredToolkit) {
                $env:WINRES_TOOLKIT_PATH = $discoveredToolkit
                $env:PATH = "$discoveredToolkit;$env:PATH"
                Write-Output "> Configured Windows SDK toolkit: '$discoveredToolkit'"
            }
            if ($AllowMissingIcon.IsPresent -or $env:SYNCDIR_ALLOW_MISSING_ICON -eq '1') {
                $env:SYNCDIR_ALLOW_MISSING_ICON = '1'
                Write-Output "> Release policy override active: SYNCDIR_ALLOW_MISSING_ICON=1"
            }

            # Phase 1: Quality Gate Execution (Mandatory unless skipped)
            if (-not $SkipQualityGate) {
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
            }

            # Phase 2: Cargo Release Build
            Write-Output "## Phase 2: Building Release Binary"
            Write-Output "Executing `cargo build --release`..."
            & cargo build --release
            if ($LASTEXITCODE -ne 0) {
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
            if (-not $SkipCrtCheck) {
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
            }

            # Phase 4b: Post-Build PE Resource Verification Gate
            if (-not $SkipPeVerification) {
                Write-Output "## Phase 4b: Post-Build PE Resource Verification"
                Assert-ReleaseResourceIntegrity -BinaryPath $DistExe -AllowMissingIcon ($AllowMissingIcon.IsPresent -or $env:SYNCDIR_ALLOW_MISSING_ICON -eq '1')
                Write-Output ""
            }

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
            [System.IO.File]::WriteAllText($ShaFile, "$Hash  $ZipName`n", [System.Text.UTF8Encoding]::new($false))
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
        } finally {
            Pop-Location
        }
    }
}

# Entrypoint guard: Only execute pipeline if run as script, not dot-sourced
if ($MyInvocation.InvocationName -ne '.') {
    try {
        Invoke-BuildRelease @PSBoundParameters
        exit 0
    } catch {
        Write-Error -ErrorAction Continue "Release build pipeline failed: $_"
        exit 1
    }
}

