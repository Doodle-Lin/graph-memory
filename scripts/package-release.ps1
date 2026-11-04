#requires -Version 5.1
<#
.SYNOPSIS
    Builds the Graph Memory release package for Windows x64.

.DESCRIPTION
    Single source of truth for the version is src-tauri/Cargo.toml [package] version.
    The script fails when desktop/GraphMemory.Desktop/GraphMemory.Desktop.csproj
    <Version> or src-tauri/src/bin/mcp_server.rs serverInfo.version disagree, and
    prints every value it compared.

    Output:
      artifacts/release/graph-memory-<version>-win-x64/   (4 exe + README.md + VERSION.txt + SHA256SUMS.txt + runtime files)
      artifacts/release/graph-memory-<version>-win-x64.zip

.PARAMETER SkipRust
    Reuse existing src-tauri/target/release binaries instead of rebuilding.

.PARAMETER SkipDesktop
    Do not publish the desktop app (produces a partial, Rust-only package).

.PARAMETER DesktopPublishDir
    Reuse an existing `dotnet publish` output directory instead of publishing.

.PARAMETER NoZip
    Skip ZIP creation (directory only).
#>
[CmdletBinding()]
param(
    [switch]$SkipRust,
    [switch]$SkipDesktop,
    [string]$DesktopPublishDir,
    [switch]$NoZip
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RepoRoot = Split-Path -Parent $PSScriptRoot
$CargoToml = Join-Path $RepoRoot 'src-tauri/Cargo.toml'
$Csproj = Join-Path $RepoRoot 'desktop/GraphMemory.Desktop/GraphMemory.Desktop.csproj'
$McpSource = Join-Path $RepoRoot 'src-tauri/src/bin/mcp_server.rs'
$ReleaseRoot = Join-Path $RepoRoot 'artifacts/release'
$RustRelease = Join-Path $RepoRoot 'src-tauri/target/release'
$RustBins = @('graph-memory-service.exe', 'mcp_server.exe', 'graph-memory-backup.exe')

function Write-Step([string]$Message) {
    Write-Host ""
    Write-Host "==> $Message"
}

function Get-CargoVersion {
    $text = Get-Content -Path $CargoToml -Raw -Encoding UTF8
    $match = [regex]::Match($text, '(?ms)^\[package\]\s*$(.*?)(?=^\[)')
    if (-not $match.Success) { throw "cannot find the [package] section in $CargoToml" }
    $version = [regex]::Match($match.Groups[1].Value, '(?m)^\s*version\s*=\s*"([^"]+)"')
    if (-not $version.Success) { throw "cannot find [package] version in $CargoToml" }
    $version.Groups[1].Value
}

function Get-CsprojVersion {
    [xml]$xml = Get-Content -Path $Csproj -Raw -Encoding UTF8
    $versions = @($xml.Project.PropertyGroup | ForEach-Object { $_.Version } | Where-Object { $_ })
    if ($versions.Count -eq 0) { throw "$Csproj has no <Version> element" }
    if ($versions.Count -gt 1) { throw "$Csproj declares <Version> more than once: $($versions -join ', ')" }
    [string]$versions[0]
}

function Get-McpVersion {
    $text = Get-Content -Path $McpSource -Raw -Encoding UTF8
    $match = [regex]::Match($text, '"serverInfo"\s*:\s*\{\s*"name"\s*:\s*"graph-memory"\s*,\s*"version"\s*:\s*"([^"]+)"')
    if (-not $match.Success) { throw "cannot find serverInfo.version in $McpSource" }
    $match.Groups[1].Value
}

function Assert-VersionConsistency {
    $cargo = Get-CargoVersion
    $csprojVersion = Get-CsprojVersion
    $mcp = Get-McpVersion
    $observed = "Cargo.toml=[$cargo] csproj=[$csprojVersion] mcp_server.rs=[$mcp]"
    Write-Host "version sources: $observed"
    if ($cargo -ne $csprojVersion) {
        throw "release version mismatch: Cargo.toml=[$cargo] csproj=[$csprojVersion] ($observed)"
    }
    if ($cargo -ne $mcp) {
        throw "release version mismatch: Cargo.toml=[$cargo] mcp_server.rs=[$mcp] ($observed)"
    }
    $cargo
}

function Resolve-Dotnet {
    # A `dotnet` on PATH may be a runtime-only host with no SDK; require a 10.x SDK
    # (the project targets net10.0) and otherwise fall back to the user-local SDK.
    $candidates = New-Object System.Collections.ArrayList
    $command = Get-Command dotnet -ErrorAction SilentlyContinue
    if ($command) { [void]$candidates.Add($command.Source) }
    [void]$candidates.Add((Join-Path $env:USERPROFILE '.dotnet/dotnet.exe'))
    [void]$candidates.Add((Join-Path $env:USERPROFILE '.codex/tools/dotnet/dotnet.exe'))
    $probed = New-Object System.Collections.ArrayList
    foreach ($candidate in $candidates) {
        if (-not (Test-Path -LiteralPath $candidate)) { continue }
        $sdks = @(& $candidate --list-sdks 2>$null)
        $hasSdk = @($sdks | Where-Object { $_ -match '^10\.' }).Count -gt 0
        [void]$probed.Add("$candidate -> $(if ($hasSdk) { 'net10 SDK' } else { 'no net10 SDK' })")
        if ($hasSdk) {
            Write-Host "dotnet: $candidate [probes: $($probed -join '; ')]"
            return $candidate
        }
    }
    throw ".NET 10 SDK not found. probes: $($probed -join '; ')"
}

function Get-FileSizeText([long]$Bytes) {
    if ($Bytes -ge 1MB) { return ('{0:N2} MB' -f ($Bytes / 1MB)) }
    if ($Bytes -ge 1KB) { return ('{0:N1} KB' -f ($Bytes / 1KB)) }
    "$Bytes bytes"
}

$version = Assert-VersionConsistency
$name = "graph-memory-$version-win-x64"
$staging = Join-Path $ReleaseRoot $name
$zipPath = Join-Path $ReleaseRoot "$name.zip"

Write-Host "release version : $version"
Write-Host "package         : $name"

if (-not $SkipRust) {
    Write-Step "cargo build --release --no-default-features ($($RustBins -join ', '))"
    Push-Location (Join-Path $RepoRoot 'src-tauri')
    try {
        $cargoArgs = @('build', '--release', '--no-default-features')
        foreach ($bin in $RustBins) { $cargoArgs += @('--bin', [IO.Path]::GetFileNameWithoutExtension($bin)) }
        Write-Host ("cargo " + ($cargoArgs -join ' '))
        & cargo @cargoArgs
        if ($LASTEXITCODE -ne 0) { throw "cargo release build failed with exit code $LASTEXITCODE" }
    } finally {
        Pop-Location
    }
} else {
    Write-Step "reusing existing release binaries (-SkipRust)"
}

$rustSources = @{}
foreach ($bin in $RustBins) {
    $path = Join-Path $RustRelease $bin
    if (-not (Test-Path -LiteralPath $path)) {
        throw "-SkipRust was given but $path does not exist; run without -SkipRust first"
    }
    $rustSources[$bin] = $path
}
Write-Host "rust binaries:"
foreach ($bin in $RustBins) {
    $item = Get-Item -LiteralPath $rustSources[$bin]
    Write-Host ("  {0,-28} {1}  {2}" -f $bin, $item.LastWriteTime.ToString('yyyy-MM-dd HH:mm:ss'), (Get-FileSizeText $item.Length))
}

$publishDir = $null
if ($SkipDesktop) {
    Write-Step "skipping desktop publish (-SkipDesktop): the package will contain only the Rust binaries"
} elseif ($DesktopPublishDir) {
    if (-not (Test-Path -LiteralPath (Join-Path $DesktopPublishDir 'GraphMemory.Desktop.exe'))) {
        throw "-DesktopPublishDir '$DesktopPublishDir' has no GraphMemory.Desktop.exe"
    }
    $publishDir = (Resolve-Path -LiteralPath $DesktopPublishDir).Path
    Write-Step "reusing desktop publish output: $publishDir"
} else {
    Write-Step "dotnet publish desktop/GraphMemory.Desktop -c Release -r win-x64 --self-contained true"
    $dotnet = Resolve-Dotnet
    $env:DOTNET_ROOT = Split-Path $dotnet -Parent
    $env:PATH = "$env:DOTNET_ROOT;$env:PATH"
    $publishDir = Join-Path $env:TEMP ("gm-publish-" + [guid]::NewGuid().ToString('N'))
    & $dotnet publish (Join-Path $RepoRoot 'desktop/GraphMemory.Desktop/GraphMemory.Desktop.csproj') `
        -c Release -r win-x64 --self-contained true -o $publishDir --nologo
    if ($LASTEXITCODE -ne 0) { throw "dotnet publish failed with exit code $LASTEXITCODE" }
}

Write-Step "assembling $staging"
if (Test-Path -LiteralPath $staging) { Remove-Item -LiteralPath $staging -Recurse -Force }
New-Item -ItemType Directory -Path $staging -Force | Out-Null

if ($publishDir) {
    Copy-Item -Path (Join-Path $publishDir '*') -Destination $staging -Recurse -Force
}
foreach ($bin in $RustBins) {
    Copy-Item -LiteralPath $rustSources[$bin] -Destination (Join-Path $staging $bin) -Force
}

# README.md: copy the repository README and append the release notes.
$readme = Join-Path $staging 'README.md'
Copy-Item -LiteralPath (Join-Path $RepoRoot 'README.md') -Destination $readme -Force
$readmeTail = @'

---

## 发布包说明 (release __VERSION__, win-x64)

- 分发目录：__NAME__（解压 __NAME__.zip 即得）。整个目录一起复制，不要只复制 exe：
  自包含的 .NET 运行时文件必须与 GraphMemory.Desktop.exe 同目录。
- GraphMemory.Desktop.exe：桌面入口（Avalonia，Windows x64，自包含运行时）。
- graph-memory-service.exe：本地 v1 HTTP 服务，数据目录 --data-dir，端口 --port。
- mcp_server.exe：MCP stdio 服务（serverInfo.version = __VERSION__）。
- graph-memory-backup.exe：export / verify / restore 备份 CLI。恢复只写入空目录，绝不覆盖现有工作库。
- VERSION.txt：本包版本；SHA256SUMS.txt：目录内所有文件的 sha256，可逐项校验。
- 自检：scripts/verify-release.ps1 会校验完整性、哈希、版本一致性、解压后 smoke 测试与一次真实导出→校验→恢复往返。
'@
$readmeTail = $readmeTail.Replace('__VERSION__', $version).Replace('__NAME__', $name)
Add-Content -Path $readme -Value $readmeTail -Encoding UTF8

Set-Content -LiteralPath (Join-Path $staging 'VERSION.txt') -Value $version -Encoding ASCII -NoNewline
Write-Host "wrote VERSION.txt = $version"

Write-Step "computing SHA256SUMS.txt"
$sumsPath = Join-Path $staging 'SHA256SUMS.txt'
$files = Get-ChildItem -LiteralPath $staging -Recurse -File | Where-Object { $_.FullName -ne $sumsPath } | Sort-Object FullName
$sums = foreach ($file in $files) {
    $relative = $file.FullName.Substring($staging.Length + 1).Replace('\', '/')
    $hash = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $relative"
}
[IO.File]::WriteAllLines($sumsPath, [string[]]$sums, (New-Object Text.UTF8Encoding($false)))
Write-Host ("SHA256SUMS.txt: {0} entries" -f $sums.Count)

if (-not $NoZip) {
    Write-Step "creating $zipPath"
    if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath -Force }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    # includeBaseDirectory = true: the ZIP carries exactly one top-level folder
    # named after the package, so extracting and running needs no extra nesting.
    [IO.Compression.ZipFile]::CreateFromDirectory($staging, $zipPath, [IO.Compression.CompressionLevel]::Optimal, $true)
}

Write-Step "release package summary"
$keyFiles = @('GraphMemory.Desktop.exe', 'graph-memory-service.exe', 'mcp_server.exe', 'graph-memory-backup.exe', 'README.md', 'VERSION.txt', 'SHA256SUMS.txt')
foreach ($key in $keyFiles) {
    $path = Join-Path $staging $key
    if (Test-Path -LiteralPath $path) {
        $item = Get-Item -LiteralPath $path
        Write-Host ("  {0,-28} {1,12}" -f $key, (Get-FileSizeText $item.Length))
    } else {
        Write-Host ("  {0,-28} {1,12}" -f $key, 'MISSING')
    }
}
$all = Get-ChildItem -LiteralPath $staging -Recurse -File
$total = ($all | Measure-Object -Property Length -Sum).Sum
Write-Host ("  directory: {0} files, {1}" -f $all.Count, (Get-FileSizeText $total))
Write-Host "  package dir : $staging"
if (-not $NoZip) {
    $zipItem = Get-Item -LiteralPath $zipPath
    Write-Host ("  package zip : {0} ({1})" -f $zipPath, (Get-FileSizeText $zipItem.Length))
}
if ($publishDir -and -not $DesktopPublishDir -and -not $SkipDesktop) {
    Remove-Item -LiteralPath $publishDir -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host ""
Write-Host "PACKAGE-RELEASE-OK version=$version dir=$staging"
