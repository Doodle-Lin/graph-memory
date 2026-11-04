param([string]$Dotnet = 'dotnet')
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
Push-Location $repo
try {
    if (-not (Get-Command $Dotnet -ErrorAction SilentlyContinue)) {
        $Dotnet = Join-Path $env:USERPROFILE '.dotnet/dotnet.exe'
    }
    $sdks = & $Dotnet --list-sdks 2>$null
    if (-not ($sdks -match '^10\.')) {
        $candidates = @(
            (Join-Path $env:USERPROFILE '.dotnet/dotnet.exe'),
            (Join-Path $env:USERPROFILE '.codex/tools/dotnet/dotnet.exe')
        )
        $Dotnet = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
        if (-not $Dotnet) { throw '.NET 10 SDK is required.' }
    }
    # Keep the resolved SDK discoverable for child processes (dotnet publish spawns MSBuild).
    $env:DOTNET_ROOT = Split-Path $Dotnet -Parent
    $env:PATH = "$env:DOTNET_ROOT;$env:PATH"
    cargo build --locked --manifest-path src-tauri/Cargo.toml --no-default-features --bin graph-memory-service --bin mcp_server --bin graph-memory-backup
    if ($LASTEXITCODE -ne 0) { throw 'Rust build failed' }
    & $Dotnet publish desktop/GraphMemory.Desktop/GraphMemory.Desktop.csproj -r win-x64 --self-contained true -o artifacts/desktop --nologo
    if ($LASTEXITCODE -ne 0) { throw 'Desktop publish failed' }
    Write-Host "Ready: $repo/artifacts/desktop/GraphMemory.Desktop.exe"
} finally { Pop-Location }
