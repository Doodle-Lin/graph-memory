param([string]$Dotnet = 'dotnet')
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
Push-Location $repo
try {
    if (-not (Get-Command $Dotnet -ErrorAction SilentlyContinue)) {
        $Dotnet = Join-Path $env:USERPROFILE '.codex/tools/dotnet/dotnet.exe'
    }
    $sdks = & $Dotnet --list-sdks
    if (-not ($sdks -match '^10\.')) {
        $localSdk = Join-Path $env:USERPROFILE '.codex/tools/dotnet/dotnet.exe'
        if (Test-Path $localSdk) { $Dotnet = $localSdk }
        else { throw '.NET 10 SDK is required.' }
    }
    cargo build --locked --manifest-path src-tauri/Cargo.toml --no-default-features --bin graph-memory-service --bin mcp_server
    if ($LASTEXITCODE -ne 0) { throw 'Rust build failed' }
    & $Dotnet publish desktop/GraphMemory.Desktop/GraphMemory.Desktop.csproj -r win-x64 --self-contained true -o artifacts/desktop --nologo
    if ($LASTEXITCODE -ne 0) { throw 'Desktop publish failed' }
    Write-Host "Ready: $repo/artifacts/desktop/GraphMemory.Desktop.exe"
} finally { Pop-Location }
