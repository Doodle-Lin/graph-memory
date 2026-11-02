#!/usr/bin/env pwsh
[CmdletBinding()]
param(
    [string]$Dotnet = 'dotnet',
    [string]$Python = 'python'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = Split-Path $PSScriptRoot -Parent

# Console test harnesses. These are plain console apps invoked with "dotnet run";
# they are deliberately not run with "dotnet test". Adjust the project paths here if
# the repository layout changes.
$setupTestProject  = 'tests/Desktop.Setup.Tests'
$editorTestProject = 'tests/Desktop.Editor.Tests'

$publishDir = Join-Path $repo 'artifacts/ci-desktop'
$desktopExe = Join-Path $publishDir 'GraphMemory.Desktop.exe'

function Invoke-Native {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Label,
        [Parameter(Mandatory = $true)][string]$FilePath,
        [string[]]$CommandArgs = @()
    )

    Write-Host "==> $Label"
    Write-Host "    $FilePath $($CommandArgs -join ' ')"
    & $FilePath @CommandArgs
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0) {
        throw "$Label failed with exit code $exitCode"
    }
}

foreach ($tool in @($Dotnet, $Python)) {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        throw "Required command not found on PATH: $tool"
    }
}

# Snapshot and clear inherited GM_* configuration so the contract can never pick up a
# developer machine's settings. Everything is restored in the finally block below.
$savedEnvironment = @{}
$processEnvironment = [System.Environment]::GetEnvironmentVariables([System.EnvironmentVariableTarget]::Process)
foreach ($name in @($processEnvironment.Keys)) {
    if ($name -like 'GM_*') {
        $savedEnvironment[$name] = [string]$processEnvironment[$name]
        Remove-Item -LiteralPath "Env:$name"
    }
}
Write-Host "Cleared $($savedEnvironment.Count) inherited GM_* environment variable(s)."

Push-Location $repo
try {
    Write-Host "Repository: $repo"

    Invoke-Native -Label 'Rust: build headless binaries' -FilePath 'cargo' -CommandArgs @(
        'build',
        '--manifest-path', 'src-tauri/Cargo.toml',
        '--locked',
        '--no-default-features',
        '--bin', 'graph-memory-service',
        '--bin', 'mcp_server'
    )

    Invoke-Native -Label 'Rust: library tests' -FilePath 'cargo' -CommandArgs @(
        'test',
        '--manifest-path', 'src-tauri/Cargo.toml',
        '--locked',
        '--no-default-features',
        '--lib'
    )

    Invoke-Native -Label 'Desktop: publish self-contained win-x64' -FilePath $Dotnet -CommandArgs @(
        'publish',
        'desktop/GraphMemory.Desktop/GraphMemory.Desktop.csproj',
        '-r', 'win-x64',
        '--self-contained', 'true',
        '-o', $publishDir,
        '--nologo'
    )

    Invoke-Native -Label 'Python: headless service tests' -FilePath $Python -CommandArgs @(
        '-X', 'utf8', 'tests/test_headless_service.py'
    )

    Invoke-Native -Label 'Setup console tests' -FilePath $Dotnet -CommandArgs @(
        'run', '--project', $setupTestProject, '-c', 'Release'
    )

    Invoke-Native -Label 'Editor console tests' -FilePath $Dotnet -CommandArgs @(
        'run', '--project', $editorTestProject, '-c', 'Release'
    )

    if (-not (Test-Path -LiteralPath $desktopExe -PathType Leaf)) {
        throw "Published desktop executable not found: $desktopExe"
    }

    $smokeDataDir = Join-Path ([System.IO.Path]::GetTempPath()) ('gm-desktop-smoke-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $smokeDataDir -Force | Out-Null
    Write-Host "==> Desktop smoke test (data dir: $smokeDataDir)"

    # WinExe: wait for the process and check its exit code explicitly.
    $smokeOut = Join-Path $publishDir 'smoke.stdout.log'
    $smokeError = Join-Path $publishDir 'smoke.stderr.log'
    $smoke = Start-Process -FilePath $desktopExe -ArgumentList @('--smoke-test', '--data-dir', ('"{0}"' -f $smokeDataDir)) -Wait -PassThru -WindowStyle Hidden -RedirectStandardOutput $smokeOut -RedirectStandardError $smokeError
    Get-Content -LiteralPath $smokeOut, $smokeError
    if ($smoke.ExitCode -ne 0) {
        throw "Desktop smoke test failed with exit code $($smoke.ExitCode)"
    }

    Write-Host 'Windows v1 contract: PASS'
}
finally {
    Pop-Location
    foreach ($name in @($savedEnvironment.Keys)) {
        Set-Item -LiteralPath "Env:$name" -Value $savedEnvironment[$name]
    }
    if ($savedEnvironment.Count -gt 0) {
        Write-Host "Restored $($savedEnvironment.Count) GM_* environment variable(s)."
    }
}
