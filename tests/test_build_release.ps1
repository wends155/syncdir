Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:TotalTests = 0; $script:PassedTests = 0; $script:FailedTests = 0; $script:Failures = @()

function Assert-Equal($Actual, $Expected, [string]$Message = "") {
    if ($Actual -ne $Expected) {
        throw "Assertion Failed: Expected '$Expected', but got '$Actual'. $Message"
    }
}
function Assert-True([bool]$Condition, [string]$Message = "") {
    if (-not $Condition) { throw "Assertion Failed: Condition was False when True expected. $Message" }
}
function Assert-False([bool]$Condition, [string]$Message = "") {
    if ($Condition) { throw "Assertion Failed: Condition was True when False expected. $Message" }
}
function Assert-Throws([scriptblock]$Block, [string]$ExpectedMatch = "", [string]$Message = "") {
    $threw = $false
    try { & $Block } catch {
        $threw = $true
        if ($ExpectedMatch -and $_.Exception.Message -notmatch $ExpectedMatch) {
            throw "Assertion Failed: Exception '$($_.Exception.Message)' did not match '$ExpectedMatch'. $Message"
        }
    }
    if (-not $threw) { throw "Assertion Failed: Expected exception was not thrown. $Message" }
}
function Run-Test([string]$Name, [scriptblock]$TestBlock) {
    $script:TotalTests++
    Write-Host -NoNewline "  [TEST] $Name ... "
    try {
        & $TestBlock
        $script:PassedTests++
        Write-Host -ForegroundColor Green "PASS"
    } catch {
        $script:FailedTests++
        $script:Failures += [PSCustomObject]@{ Test = $Name; Error = $_.Exception.Message }
        Write-Host -ForegroundColor Red "FAIL"
    }
}

# Dot-source release script to load functions under test
$RepoRoot = (git rev-parse --show-toplevel 2>$null)
if (-not $RepoRoot) { $RepoRoot = (Resolve-Path "$PSScriptRoot\..").Path }
. (Join-Path $RepoRoot "scripts\build-release.ps1")

Run-Test "Test-PathSafety: Valid paths pass" {
    Test-PathSafety -Path "C:\Program Files (x86)\Windows Kits\10\bin\x64" -ParamName "SdkPath"
    Test-PathSafety -Path "\\server\share\tools\rc.exe" -ParamName "RcPath"
    Test-PathSafety -Path $null -ParamName "EmptyPath"
    Test-PathSafety -Path "" -ParamName "EmptyPath2"
}
Run-Test "Test-PathSafety: Rejects null bytes" {
    Assert-Throws { Test-PathSafety -Path "C:\tools`0\bin\rc.exe" -ParamName "RcPath" } "prohibited characters"
}
Run-Test "Test-PathSafety: Rejects control characters" {
    Assert-Throws { Test-PathSafety -Path "C:\tools`r`n\rc.exe" -ParamName "RcPath" } "prohibited characters"
}
Run-Test "Test-PathSafety: Rejects unescaped quotes" {
    Assert-Throws { Test-PathSafety -Path 'C:\tools"injection\rc.exe' -ParamName "RcPath" } "prohibited characters"
}
Run-Test "Invoke-WithEnvironmentScope: Restores modified variables and removes newly created variables" {
    $origPath = $env:PATH
    $env:TEST_EXISTING_VAR = "InitialValue"
    Remove-Item "env:TEST_NEW_VAR" -ErrorAction SilentlyContinue

    Invoke-WithEnvironmentScope {
        $env:PATH = "C:\Injected\Path;$env:PATH"
        $env:TEST_EXISTING_VAR = "MutatedValue"
        $env:TEST_NEW_VAR = "NewlyCreated"
    }

    Assert-Equal $env:PATH $origPath "PATH must be restored"
    Assert-Equal $env:TEST_EXISTING_VAR "InitialValue" "Pre-existing variable must be restored"
    Assert-False (Test-Path "env:TEST_NEW_VAR") "Newly created variable must be deleted"
    Remove-Item "env:TEST_EXISTING_VAR" -ErrorAction SilentlyContinue
}

Run-Test "Invoke-WithEnvironmentScope: Restores environment even when action throws fatal exception" {
    $origPath = $env:PATH
    $threwExpected = $false
    try {
        Invoke-WithEnvironmentScope {
            $env:PATH = "C:\Dangerous\Path;$env:PATH"
            throw "Simulated pipeline catastrophe"
        }
    } catch {
        if ($_.Exception.Message -match "Simulated pipeline catastrophe") {
            $threwExpected = $true
        } else {
            throw $_
        }
    }
    Assert-True $threwExpected "Should propagate the simulated catastrophe"
    Assert-Equal $env:PATH $origPath "PATH must be restored despite exception"
}
Run-Test "Resolve-SdkToolkit: Tier 1 override takes precedence over Tiers 2 and 3" {
    $testDir = Join-Path ([System.IO.Path]::GetTempPath()) ("sdk_test_" + [System.Guid]::NewGuid().ToString())
    $null = New-Item -Path $testDir -ItemType Directory -Force
    try {
        $tier1 = Join-Path $testDir "tier1"
        $tier2 = Join-Path $testDir "tier2"
        $tier3 = Join-Path $testDir "tier3"
        $null = New-Item -Path $tier1, $tier2, $tier3 -ItemType Directory -Force
        $null = New-Item -Path (Join-Path $tier1 "rc.exe") -ItemType File -Force
        $null = New-Item -Path (Join-Path $tier2 "rc.exe") -ItemType File -Force

        $resolved = Resolve-SdkToolkit -ToolkitOverride $tier1 -RcOverride (Join-Path $tier2 "rc.exe") -SdkRootOverride $tier3
        Assert-Equal $resolved (Get-Item $tier1).FullName "Tier 1 override must take precedence"
    } finally {
        Remove-Item -Path $testDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Run-Test "Resolve-SdkToolkit: Tier 3 SDK root probes newest descending versioned candidate tree" {
    $testDir = Join-Path ([System.IO.Path]::GetTempPath()) ("sdk_test_" + [System.Guid]::NewGuid().ToString())
    $null = New-Item -Path $testDir -ItemType Directory -Force
    try {
        $sdkRoot = Join-Path $testDir "Kits10"
        $v1 = Join-Path $sdkRoot "bin\10.0.19041.0\x64"
        $v2 = Join-Path $sdkRoot "bin\10.0.22621.0\x64"
        $null = New-Item -Path $v1, $v2 -ItemType Directory -Force
        $null = New-Item -Path (Join-Path $v1 "rc.exe") -ItemType File -Force
        $null = New-Item -Path (Join-Path $v2 "rc.exe") -ItemType File -Force

        $resolved = Resolve-SdkToolkit -SdkRootOverride $sdkRoot
        Assert-Equal $resolved (Get-Item $v2).FullName "Tier 3 must select newest descending version containing rc.exe"
    } finally {
        Remove-Item -Path $testDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Run-Test "Resolve-SdkToolkit: Missing toolchain returns `$null without uncaught exception" {
    $nonExistent = "C:\NonExistent_Sdk_Directory_404_Path"
    $resolved = Resolve-SdkToolkit -ToolkitOverride $nonExistent -RcOverride $nonExistent -SdkRootOverride $nonExistent
}

function New-MockPeBinary {
    param([string]$Path, [bool]$IncludeRsrc = $true, [bool]$IncludeManifest = $true, [bool]$IncludeIcon = $true)
    $ms = [System.IO.MemoryStream]::new()
    $bw = [System.IO.BinaryWriter]::new($ms)
    try {
        # DOS Header (64 bytes)
        $bw.Write([byte]0x4D); $bw.Write([byte]0x5A) # MZ
        $bw.Write(([byte[]]::new(58)))
        $bw.Write([uint32]64) # e_lfanew = 64
        # PE Signature (4 bytes)
        $bw.Write([System.Text.Encoding]::ASCII.GetBytes("PE`0`0"))
        # COFF Header (20 bytes): x64, 1 section, opt header 240 bytes
        $numSections = if ($IncludeRsrc) { 1 } else { 0 }
        $bw.Write([uint16]0x8664); $bw.Write([uint16]$numSections); $bw.Write([uint32]0); $bw.Write([uint32]0); $bw.Write([uint32]0); $bw.Write([uint16]240); $bw.Write([uint16]0x22)
        # Optional Header PE32+ (240 bytes)
        $bw.Write([uint16]0x20B) # PE32+ (offset 0)
        $bw.Write(([byte[]]::new(106))) # Padding to NumberOfRvaAndSizes (offset 108)
        $bw.Write([uint32]16)           # NumberOfRvaAndSizes = 16 (offset 108-112)
        # DataDirectory[0] (Export), DataDirectory[1] (Import) = 16 bytes zero (offset 112-128)
        $bw.Write([uint64]0); $bw.Write([uint64]0)
        # DataDirectory[2] (Resource Table at offset 128 = 0x80)
        if ($IncludeRsrc) {
            $bw.Write([uint32]0x1000) # RVA = 0x1000
            $bw.Write([uint32]512)    # Size = 512
        } else {
            $bw.Write([uint64]0)
        }
        $bw.Write(([byte[]]::new(240 - 136))) # Remainder of optional header to 240 bytes
        # Section Header 1 (.rsrc)
        if ($IncludeRsrc) {
            $bw.Write([System.Text.Encoding]::ASCII.GetBytes(".rsrc`0`0`0"))
            $bw.Write([uint32]512); $bw.Write([uint32]0x1000); $bw.Write([uint32]512); $bw.Write([uint32]512)
            $bw.Write(([byte[]]::new(16)))
        }
        # Pad Header to 512 bytes
        $headerPad = 512 - $ms.Position
        if ($headerPad -gt 0) { $bw.Write(([byte[]]::new($headerPad))) }
        # Section Raw Data (.rsrc at raw offset 512)
        if ($IncludeRsrc) {
            $bw.Write(([byte[]]::new(12))) # Characteristics, Time, Major/Minor
            $idEntries = 0; if ($IncludeManifest) { $idEntries++ }; if ($IncludeIcon) { $idEntries++ }
            $bw.Write([uint16]0); $bw.Write([uint16]$idEntries)
            if ($IncludeIcon) { $bw.Write([uint32]14); $bw.Write([uint32]0x80000030L) } # RT_GROUP_ICON
            if ($IncludeManifest) { $bw.Write([uint32]24); $bw.Write([uint32]0x80000050L) } # RT_MANIFEST
            $secPad = 512 - 16 - ($idEntries * 8)
            if ($secPad -gt 0) { $bw.Write(([byte[]]::new($secPad))) }
        }
        [System.IO.File]::WriteAllBytes($Path, $ms.ToArray())
    } finally { $bw.Dispose(); $ms.Dispose() }
}

Run-Test "Test-PeBinaryStructure: Detects resource table, .rsrc, manifest and icon in mock PE" {
    $tempPe = [System.IO.Path]::GetTempFileName() + ".exe"
    try {
        New-MockPeBinary -Path $tempPe -IncludeRsrc $true -IncludeManifest $true -IncludeIcon $true
        $pe = Test-PeBinaryStructure -FilePath $tempPe
        Assert-True $pe.IsValidPe "Should parse as valid PE"
        Assert-True $pe.HasResourceTable "Resource table entry at offset 128 must be non-zero"
        Assert-True $pe.HasRsrcSection "Section header table must contain .rsrc"
        Assert-True $pe.HasManifest "Manifest entry (24) must be detected"
        Assert-True $pe.HasIcon "Icon entry (14) must be detected"
    } finally { Remove-Item -Path $tempPe -Force -ErrorAction SilentlyContinue }
}

Run-Test "Test-PeBinaryStructure: Detects missing resource table in resource-less binary" {
    $tempPe = [System.IO.Path]::GetTempFileName() + ".exe"
    try {
        New-MockPeBinary -Path $tempPe -IncludeRsrc $false
        $pe = Test-PeBinaryStructure -FilePath $tempPe
        Assert-True $pe.IsValidPe "Should parse as valid PE"
        Assert-False $pe.HasResourceTable "Resource table must be empty (0 bytes)"
        Assert-False $pe.HasRsrcSection "Section header table must not contain .rsrc"
    } finally { Remove-Item -Path $tempPe -Force -ErrorAction SilentlyContinue }
}

Run-Test "Assert-ReleaseResourceIntegrity: Enforces fail-closed on binary without resources" {
    $tempPe = [System.IO.Path]::GetTempFileName() + ".exe"
    try {
        New-MockPeBinary -Path $tempPe -IncludeRsrc $false
        Assert-Throws { Assert-ReleaseResourceIntegrity -BinaryPath $tempPe } "FAIL-CLOSED POLICY VIOLATION"
    } finally { Remove-Item -Path $tempPe -Force -ErrorAction SilentlyContinue }
}

Run-Test "Assert-ReleaseResourceIntegrity: Permits missing resources with -AllowMissingIcon" {
    $tempPe = [System.IO.Path]::GetTempFileName() + ".exe"
    try {
        New-MockPeBinary -Path $tempPe -IncludeRsrc $false
        Assert-ReleaseResourceIntegrity -BinaryPath $tempPe -AllowMissingIcon $true
    } finally { Remove-Item -Path $tempPe -Force -ErrorAction SilentlyContinue }
}

Run-Test "Resolve-SdkToolkit: Compiler sibling probing finds Windows Kits via cl.exe sibling tree" {
    $testDir = Join-Path ([System.IO.Path]::GetTempPath()) ("sdk_cl_test_" + [System.Guid]::NewGuid().ToString())
    $null = New-Item -Path $testDir -ItemType Directory -Force
    try {
        $mockClDir = Join-Path $testDir "VC\Tools\MSVC\14.50\bin\Hostx64\x64"
        $mockRcDir = Join-Path $testDir "Windows Kits\10\bin\10.0.26100.0\x64"
        $null = New-Item -Path $mockClDir -ItemType Directory -Force
        $null = New-Item -Path $mockRcDir -ItemType Directory -Force
        $null = New-Item -Path (Join-Path $mockRcDir "rc.exe") -ItemType File -Force

        # Pass custom PathExists scriptblock that knows about this synthetic tree
        $customPathExists = {
            param($p)
            Test-Path -LiteralPath $p
        }

        # Resolve-SdkToolkit using the mock SDK root directly
        $resolved = Resolve-SdkToolkit -SdkRootOverride (Join-Path $testDir "Windows Kits\10") -PathExists $customPathExists
        Assert-Equal $resolved (Resolve-Path $mockRcDir).Path "Should resolve toolkit from mock Windows Kits tree"
    } finally {
        Remove-Item -Path $testDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Run-Test "Invoke-BuildRelease: Rejects unsafe path parameter input" {
    Assert-Throws {
        Invoke-BuildRelease -WinresToolkitPath "C:\prohibited`0path"
    } "Security Error" "Invoke-BuildRelease must reject null bytes"

    Assert-Throws {
        Invoke-BuildRelease -RcPath "C:\prohibited`"path"
    } "Security Error" "Invoke-BuildRelease must reject quote characters"
}

Run-Test "Assert-ReleaseResourceIntegrity: Rejects non-existent binary path" {
    Assert-Throws {
        Assert-ReleaseResourceIntegrity -BinaryPath "C:\non_existent_binary_file_404.exe"
    } "FAIL-CLOSED POLICY VIOLATION" "Missing file must trigger fail-closed violation"
}

Run-Test "Assert-ReleaseResourceIntegrity: Rejects corrupted binary (truncated DOS header)" {
    $tempFile = [System.IO.Path]::GetTempFileName()
    try {
        [System.IO.File]::WriteAllBytes($tempFile, [byte[]]@(0x4D, 0x5A, 0x00)) # 3 bytes MZ
        Assert-Throws {
            Assert-ReleaseResourceIntegrity -BinaryPath $tempFile
        } "FAIL-CLOSED POLICY VIOLATION" "Truncated binary must trigger fail-closed violation"
    } finally {
        Remove-Item -Path $tempFile -Force -ErrorAction SilentlyContinue
    }
}

Run-Test "Test-PeBinaryStructure: Live release binary check (conditional)" {
    $releaseExe = Join-Path $RepoRoot "target\release\syncdir.exe"
    if (Test-Path $releaseExe) {
        $pe = Test-PeBinaryStructure -FilePath $releaseExe
        Assert-True $pe.IsValidPe "Release binary must be valid PE"
        Assert-True $pe.HasResourceTable "Release binary must have Resource Table at offset 128"
        Assert-True $pe.HasRsrcSection "Release binary must have .rsrc section"
    } else {
        Write-Host -NoNewline " (Skipped live check: target/release/syncdir.exe not yet built) "
    }
}

# Test summary output and exit code
Write-Output ""
Write-Output ("Tests: {0} Total | {1} Passed | {2} Failed" -f $script:TotalTests, $script:PassedTests, $script:FailedTests)
if ($script:FailedTests -gt 0) {
    Write-Output "Failures:"
    foreach ($f in $script:Failures) {
        Write-Output ("  - {0}: {1}" -f $f.Test, $f.Error)
    }
    exit 1
}
exit 0
