#requires -Version 5.1
<#
.SYNOPSIS
    Verifies a Graph Memory release package end to end.

.DESCRIPTION
    Checks, in order:
      1. package directory completeness (4 exe + VERSION.txt + SHA256SUMS.txt);
      2. version consistency across Cargo.toml, csproj, mcp_server.rs, VERSION.txt;
      3. every SHA256SUMS.txt entry re-hashed, and every packaged file listed;
      4. the ZIP extracts into a fresh temp directory and the extracted
         GraphMemory.Desktop.exe --smoke-test --data-dir <fresh> exits 0;
      5. an MCP initialize handshake against the extracted mcp_server.exe
         reports the expected version;
      6. a real round trip with the extracted binaries: service on a random
         port -> create project/memory -> retract -> export -> verify ->
         restore into an empty directory -> counts equal -> re-export gives the
         same payload_sha256.

    Prints a JSON verdict and exits 0 only when every check passed.

.PARAMETER PackageDir
    Release directory. Defaults to artifacts/release/graph-memory-<version>-win-x64.

.PARAMETER ZipPath
    Release ZIP. Defaults to the ZIP beside the default package directory.

.PARAMETER DesktopPublishDir
    For partial packages (-SkipDesktop) this directory supplies the desktop
    binary that the smoke test and MCP handshake need, without being packaged.

.PARAMETER KeepTemp
    Keep the temporary extraction/round-trip directories for inspection.
#>
[CmdletBinding()]
param(
    [string]$PackageDir,
    [string]$ZipPath,
    [string]$DesktopPublishDir,
    [switch]$KeepTemp
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RepoRoot = Split-Path -Parent $PSScriptRoot
$CargoToml = Join-Path $RepoRoot 'src-tauri/Cargo.toml'
$Csproj = Join-Path $RepoRoot 'desktop/GraphMemory.Desktop/GraphMemory.Desktop.csproj'
$McpSource = Join-Path $RepoRoot 'src-tauri/src/bin/mcp_server.rs'
$RequiredExes = @('GraphMemory.Desktop.exe', 'graph-memory-service.exe', 'mcp_server.exe', 'graph-memory-backup.exe')

$script:Issues = New-Object System.Collections.ArrayList
$script:Checks = New-Object System.Collections.ArrayList

function Add-Check([string]$Name, [bool]$Ok, [string]$Detail) {
    [void]$script:Checks.Add([ordered]@{ check = $Name; ok = $Ok; detail = $Detail })
    if (-not $Ok) { [void]$script:Issues.Add("$Name`: $Detail") }
    $tag = if ($Ok) { 'PASS' } else { 'FAIL' }
    Write-Host ("[{0}] {1} - {2}" -f $tag, $Name, $Detail)
}

function Get-CargoVersion {
    $text = Get-Content -Path $CargoToml -Raw -Encoding UTF8
    $section = [regex]::Match($text, '(?ms)^\[package\]\s*$(.*?)(?=^\[)')
    if (-not $section.Success) { throw "cannot find the [package] section in $CargoToml" }
    $version = [regex]::Match($section.Groups[1].Value, '(?m)^\s*version\s*=\s*"([^"]+)"')
    if (-not $version.Success) { throw "cannot find [package] version in $CargoToml" }
    $version.Groups[1].Value
}

function Get-CsprojVersion {
    [xml]$xml = Get-Content -Path $Csproj -Raw -Encoding UTF8
    $versions = @($xml.Project.PropertyGroup | ForEach-Object { $_.Version } | Where-Object { $_ })
    if ($versions.Count -eq 0) { throw "$Csproj has no <Version> element" }
    [string]$versions[0]
}

function Get-McpVersion {
    $text = Get-Content -Path $McpSource -Raw -Encoding UTF8
    $match = [regex]::Match($text, '"serverInfo"\s*:\s*\{\s*"name"\s*:\s*"graph-memory"\s*,\s*"version"\s*:\s*"([^"]+)"')
    if (-not $match.Success) { throw "cannot find serverInfo.version in $McpSource" }
    $match.Groups[1].Value
}

function Get-Prop($Object, [string]$Name) {
    if ($null -eq $Object) { return $null }
    $property = $Object.PSObject.Properties[$Name]
    if ($property) { return $property.Value }
    return $null
}

function Invoke-Api([string]$Base, [string]$Path, $Body) {
    $file = Join-Path $env:TEMP ("gm-api-" + [guid]::NewGuid().ToString('N') + '.json')
    try {
        [IO.File]::WriteAllText($file, ($Body | ConvertTo-Json -Compress -Depth 8), (New-Object Text.UTF8Encoding($false)))
        $bytes = [IO.File]::ReadAllBytes($file)
        Invoke-RestMethod -Uri ($Base.TrimEnd('/') + $Path) -Method Post -ContentType 'application/json; charset=utf-8' -Body $bytes
    } finally {
        Remove-Item -LiteralPath $file -Force -ErrorAction SilentlyContinue
    }
}

function Invoke-Exe([string]$Exe, [string[]]$Arguments, [string]$WorkDir) {
    $out = Join-Path $env:TEMP ("gm-exe-" + [guid]::NewGuid().ToString('N') + '.out')
    $err = Join-Path $env:TEMP ("gm-exe-" + [guid]::NewGuid().ToString('N') + '.err')
    try {
        $process = Start-Process -FilePath $Exe -ArgumentList $Arguments -WorkingDirectory $WorkDir -Wait -PassThru `
            -RedirectStandardOutput $out -RedirectStandardError $err -WindowStyle Hidden
        [ordered]@{
            exit_code = $process.ExitCode
            stdout = ([IO.File]::ReadAllText($out, [Text.Encoding]::UTF8)).Trim()
            stderr = ([IO.File]::ReadAllText($err, [Text.Encoding]::UTF8)).Trim()
        }
    } finally {
        Remove-Item -LiteralPath $out, $err -Force -ErrorAction SilentlyContinue
    }
}

function Start-ServiceAt([string]$Exe, [string]$DataDir) {
    New-Item -ItemType Directory -Path $DataDir -Force | Out-Null
    $process = Start-Process -FilePath $Exe -ArgumentList @('--data-dir', $DataDir, '--port', '0') -PassThru `
        -RedirectStandardOutput (Join-Path $DataDir 'service.out') -RedirectStandardError (Join-Path $DataDir 'service.err') -WindowStyle Hidden
    $manifest = Join-Path $DataDir 'service.json'
    for ($i = 0; $i -lt 200; $i++) {
        if (Test-Path -LiteralPath $manifest) {
            try {
                $info = [IO.File]::ReadAllText($manifest, [Text.Encoding]::UTF8) | ConvertFrom-Json
                if ($info.api_url) { return [ordered]@{ process = $process; api_url = [string]$info.api_url } }
            } catch { }
        }
        if ($process.HasExited) { throw "graph-memory-service.exe exited early (code $($process.ExitCode))" }
        Start-Sleep -Milliseconds 100
    }
    throw 'graph-memory-service.exe did not publish service.json in time'
}

function Stop-OwnedService($Handle) {
    if ($null -eq $Handle) { return }
    try { if (-not $Handle.process.HasExited) { Stop-Process -Id $Handle.process.Id -Force; $Handle.process.WaitForExit(5000) } } catch { }
    try { $Handle.process.Dispose() } catch { }
}

function Get-FileSizeText([long]$Bytes) {
    if ($Bytes -ge 1MB) { return ('{0:N2} MB' -f ($Bytes / 1MB)) }
    if ($Bytes -ge 1KB) { return ('{0:N1} KB' -f ($Bytes / 1KB)) }
    "$Bytes bytes"
}

$tempRoots = New-Object System.Collections.ArrayList
function New-TempRoot([string]$Label) {
    $path = Join-Path $env:TEMP ("gm-verify-$Label-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $path -Force | Out-Null
    [void]$tempRoots.Add($path)
    $path
}

$serviceHandle = $null
$extractRoot = $null
$version = $null
$verdict = $null
try {
    # ---------------------------------------------------------------- versions
    $cargoVersion = Get-CargoVersion
    $csprojVersion = Get-CsprojVersion
    $mcpVersion = Get-McpVersion
    Write-Host "version sources: Cargo.toml=[$cargoVersion] csproj=[$csprojVersion] mcp_server.rs=[$mcpVersion]"
    Add-Check 'version_cargo_vs_csproj' ($cargoVersion -eq $csprojVersion) "Cargo.toml=[$cargoVersion] csproj=[$csprojVersion]"
    Add-Check 'version_cargo_vs_mcp' ($cargoVersion -eq $mcpVersion) "Cargo.toml=[$cargoVersion] mcp_server.rs=[$mcpVersion]"
    $version = $cargoVersion

    if (-not $PackageDir) { $PackageDir = Join-Path $RepoRoot "artifacts/release/graph-memory-$version-win-x64" }
    if (-not $ZipPath) { $ZipPath = Join-Path $RepoRoot "artifacts/release/graph-memory-$version-win-x64.zip" }

    # ------------------------------------------------------------ completeness
    Write-Host ""
    Write-Host "==> package completeness: $PackageDir"
    $dirOk = Test-Path -LiteralPath $PackageDir
    Add-Check 'package_dir_exists' $dirOk $PackageDir
    if (-not $dirOk) { throw "package directory missing: $PackageDir" }

    $missing = @()
    foreach ($name in ($RequiredExes + @('VERSION.txt', 'SHA256SUMS.txt'))) {
        if (-not (Test-Path -LiteralPath (Join-Path $PackageDir $name))) { $missing += $name }
    }
    Add-Check 'package_required_files' ($missing.Count -eq 0) $(if ($missing.Count -eq 0) { ($RequiredExes + @('VERSION.txt', 'SHA256SUMS.txt')) -join ', ' } else { 'missing: ' + ($missing -join ', ') })

    $versionFile = Join-Path $PackageDir 'VERSION.txt'
    if (Test-Path -LiteralPath $versionFile) {
        $packagedVersion = ([IO.File]::ReadAllText($versionFile, [Text.Encoding]::UTF8)).Trim()
        Add-Check 'version_txt_matches_cargo' ($packagedVersion -eq $version) "VERSION.txt=[$packagedVersion] Cargo.toml=[$version]"
    }

    $zipOk = Test-Path -LiteralPath $ZipPath
    Add-Check 'package_zip_exists' $zipOk $ZipPath

    # --------------------------------------------------------------- SHA256SUMS
    Write-Host ""
    Write-Host "==> SHA256SUMS.txt"
    $sumsFile = Join-Path $PackageDir 'SHA256SUMS.txt'
    $sumProblems = New-Object System.Collections.ArrayList
    $listed = New-Object System.Collections.ArrayList
    if (Test-Path -LiteralPath $sumsFile) {
        $lines = @(Get-Content -LiteralPath $sumsFile -Encoding UTF8 | Where-Object { $_.Trim() })
        foreach ($line in $lines) {
            $match = [regex]::Match($line.Trim(), '^([0-9a-fA-F]{64})\s+\*?(.+)$')
            if (-not $match.Success) { [void]$sumProblems.Add("malformed line: $line"); continue }
            $relative = $match.Groups[2].Value.Trim().Replace('/', '\')
            if ($listed -notcontains $relative) { [void]$listed.Add($relative) }
            $target = Join-Path $PackageDir $relative
            if (-not (Test-Path -LiteralPath $target)) { [void]$sumProblems.Add("missing file: $relative"); continue }
            $actual = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($actual -ne $match.Groups[1].Value.ToLowerInvariant()) { [void]$sumProblems.Add("hash mismatch: $relative") }
        }
        Add-Check 'sha256sums_entries' ($sumProblems.Count -eq 0) $(if ($sumProblems.Count -eq 0) { "$($lines.Count) entries verified" } else { ($sumProblems -join '; ') })

        $unlisted = @(Get-ChildItem -LiteralPath $PackageDir -Recurse -File |
            ForEach-Object { $_.FullName.Substring($PackageDir.Length + 1).Replace('/', '\') } |
            Where-Object { $_ -ne 'SHA256SUMS.txt' -and ($listed -notcontains $_) })
        Add-Check 'sha256sums_covers_every_file' ($unlisted.Count -eq 0) $(if ($unlisted.Count -eq 0) { 'all package files are listed' } else { 'unlisted: ' + ($unlisted -join ', ') })
    } else {
        Add-Check 'sha256sums_entries' $false 'SHA256SUMS.txt is missing'
    }

    # ------------------------------------------------- ZIP extract + smoke test
    Write-Host ""
    Write-Host "==> ZIP extraction and desktop smoke test"
    $extractRoot = $null
    if ($zipOk) {
        $extractRoot = New-TempRoot 'extract'
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        # The archive must carry exactly one top-level folder, named after the
        # package directory, so "unzip and run" works without extra nesting.
        $expectedZipRoot = Split-Path $PackageDir -Leaf
        $archive = [IO.Compression.ZipFile]::OpenRead($ZipPath)
        try {
            $topLevels = @($archive.Entries |
                ForEach-Object { ($_.FullName -split '[\\/]')[0] } |
                Where-Object { $_ } |
                Select-Object -Unique)
        } finally { $archive.Dispose() }
        $topLevelText = if ($topLevels.Count -le 3) { $topLevels -join ', ' } else { "$($topLevels.Count) entries: " + (($topLevels | Select-Object -First 3) -join ', ') + ', ...' }
        Add-Check 'zip_single_top_level_dir' ($topLevels.Count -eq 1 -and $topLevels[0] -eq $expectedZipRoot) "top-level entries=[$topLevelText] expected=[$expectedZipRoot]"

        [IO.Compression.ZipFile]::ExtractToDirectory($ZipPath, $extractRoot)
        $extractedDir = Join-Path $extractRoot $expectedZipRoot
        if (-not (Test-Path -LiteralPath $extractedDir) -and $topLevels.Count -eq 1) {
            $extractedDir = Join-Path $extractRoot $topLevels[0]
        }
        $extractedMissing = @($RequiredExes | Where-Object { -not (Test-Path -LiteralPath (Join-Path $extractedDir $_)) })
        Add-Check 'zip_has_all_exes' ($extractedMissing.Count -eq 0) $(if ($extractedMissing.Count -eq 0) { "$extractedDir -> $($RequiredExes -join ', ')" } else { 'missing: ' + ($extractedMissing -join ', ') })
    } else {
        $extractedDir = $PackageDir
    }

    $desktopExe = Join-Path $extractedDir 'GraphMemory.Desktop.exe'
    if (-not (Test-Path -LiteralPath $desktopExe) -and $DesktopPublishDir) {
        $desktopExe = Join-Path $DesktopPublishDir 'GraphMemory.Desktop.exe'
    }
    $desktopAvailable = Test-Path -LiteralPath $desktopExe
    if (-not $desktopAvailable) {
        Add-Check 'desktop_smoke_test' $false "GraphMemory.Desktop.exe not found in the package or -DesktopPublishDir"
    } else {
        $smokeData = New-TempRoot 'smoke-data'
        $smoke = Invoke-Exe $desktopExe @('--smoke-test', '--data-dir', $smokeData) (Split-Path $desktopExe -Parent)
        $smokeOk = $smoke.exit_code -eq 0 -and $smoke.stdout -match 'PASS desktop client'
        Add-Check 'desktop_smoke_test' $smokeOk "exit=$($smoke.exit_code) stdout=[$($smoke.stdout)] stderr=[$($smoke.stderr)]"
    }

    # ----------------------------------------------------------- MCP handshake
    Write-Host ""
    Write-Host "==> mcp_server.exe initialize handshake"
    $mcpExe = Join-Path $extractedDir 'mcp_server.exe'
    if (-not (Test-Path -LiteralPath $mcpExe) -and $extractedDir -ne $PackageDir) { $mcpExe = Join-Path $PackageDir 'mcp_server.exe' }
    if (-not (Test-Path -LiteralPath $mcpExe)) {
        Add-Check 'mcp_server_version' $false 'mcp_server.exe not found'
    } else {
        $request = '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"verify-release","version":"1"}}}'
        $requestFile = Join-Path $env:TEMP ("gm-mcp-" + [guid]::NewGuid().ToString('N') + '.jsonl')
        $mcpOut = Join-Path $env:TEMP ("gm-mcp-" + [guid]::NewGuid().ToString('N') + '.out')
        $mcpErr = Join-Path $env:TEMP ("gm-mcp-" + [guid]::NewGuid().ToString('N') + '.err')
        try {
            $requestBytes = [Text.Encoding]::UTF8.GetBytes($request + "`n")
            $mcpData = New-TempRoot 'mcp-data'
            $mcpExeDir = Split-Path $mcpExe -Parent
            $start = New-Object Diagnostics.ProcessStartInfo
            $start.FileName = $mcpExe
            $start.WorkingDirectory = $mcpExeDir
            $start.UseShellExecute = $false
            $start.CreateNoWindow = $true
            $start.RedirectStandardInput = $true
            $start.RedirectStandardOutput = $true
            $start.RedirectStandardError = $true
            # .NET 4.6.2+ string Arguments keeps every path atomic; nothing here
            # needs the 8.3 short names that ArgumentList would require.
            $start.Arguments = "--data-dir `"$mcpData`""
            $mcpProcess = [Diagnostics.Process]::Start($start)
            $mcpProcess.StandardInput.BaseStream.Write($requestBytes, 0, $requestBytes.Length)
            $mcpProcess.StandardInput.BaseStream.Flush()
            $mcpProcess.StandardInput.Close()
            if (-not $mcpProcess.WaitForExit(20000)) { try { $mcpProcess.Kill() } catch { } ; throw 'mcp_server.exe did not exit after stdin EOF' }
            $handshake = $mcpProcess.StandardOutput.ReadToEnd()
            $handshakeErr = $mcpProcess.StandardError.ReadToEnd()
            $mcpProcess.Dispose()
            $reported = $null
            foreach ($candidate in ($handshake -split "`n")) {
                $line = $candidate.Trim()
                if (-not $line) { continue }
                $stripped = $line -replace '^Content-Length:\s*\d+\s*$', ''
                if (-not $stripped) { continue }
                try { $parsed = $stripped | ConvertFrom-Json } catch { continue }
                $serverInfo = Get-Prop (Get-Prop $parsed 'result') 'serverInfo'
                $reported = Get-Prop $serverInfo 'version'
                if ($reported) { break }
            }
            Add-Check 'mcp_server_version' ($reported -eq $version) "handshake serverInfo.version=[$reported] expected=[$version] stderr=[$($handshakeErr.Trim())]"
        } finally {
            Remove-Item -LiteralPath $requestFile, $mcpOut, $mcpErr -Force -ErrorAction SilentlyContinue
        }
    }

    # ------------------------------------------------------------ real roundtrip
    Write-Host ""
    Write-Host "==> real round trip with the packaged binaries"
    $serviceExe = Join-Path $extractedDir 'graph-memory-service.exe'
    $backupExe = Join-Path $extractedDir 'graph-memory-backup.exe'
    if (-not (Test-Path -LiteralPath $serviceExe)) { $serviceExe = Join-Path $PackageDir 'graph-memory-service.exe' }
    if (-not (Test-Path -LiteralPath $backupExe)) { $backupExe = Join-Path $PackageDir 'graph-memory-backup.exe' }
    $roundTrip = New-TempRoot 'roundtrip'
    $liveDir = Join-Path $roundTrip 'live'
    $restoreDir = Join-Path $roundTrip 'restored'
    $backupFile = Join-Path $roundTrip 'release.gmbackup'
    $reexportFile = Join-Path $roundTrip 'reexport.gmbackup'
    $binDir = Split-Path $serviceExe -Parent

    $serviceHandle = Start-ServiceAt $serviceExe $liveDir
    $api = $serviceHandle.api_url
    Write-Host "service: $serviceExe pid=$($serviceHandle.process.Id) api_url=$api"
    Add-Check 'service_started' $true "api_url=$api"

    $project = Invoke-Api $api '/api/v1/projects' @{ name = '发布物验证项目'; directory = 'C:\workspace\release-verify' }
    $created = Invoke-Api $api '/api/v1/memories' @{ memory_scope = 'personal'; event_id = 'release-roundtrip'; content = '发布物往返验证记录 v1'; source = 'verify-release'; conditions = 'release verification only' }
    $memoryId = $created.memory.id
    $retracted = Invoke-Api $api '/api/v1/memories/retract' @{ id = $memoryId; expected_version = 1; reason = '发布物验证撤回' }
    Add-Check 'retract_creates_new_version' ($retracted.memory.version -eq 2 -and $retracted.memory.retracted_at) "version=$($retracted.memory.version) retracted_at=$($retracted.memory.retracted_at) reason=$($retracted.memory.retract_reason)"

    # service is still running: export must work and must not change the library
    $export = Invoke-Exe $backupExe @('export', '--data-dir', $liveDir, '--output', $backupFile) $binDir
    $exportJson = $null
    try { $exportJson = $export.stdout | ConvertFrom-Json } catch { }
    Add-Check 'export_with_service_running' ($export.exit_code -eq 0 -and $exportJson.status -eq 'ok') "exit=$($export.exit_code) stdout=[$($export.stdout)] stderr=[$($export.stderr)]"

    Stop-OwnedService $serviceHandle
    $serviceHandle = $null

    $verify = Invoke-Exe $backupExe @('verify', $backupFile) $binDir
    $verifyJson = $null
    try { $verifyJson = $verify.stdout | ConvertFrom-Json } catch { }
    Add-Check 'verify_exported_backup' ($verify.exit_code -eq 0 -and $verifyJson.status -eq 'ok') "exit=$($verify.exit_code) stdout=[$($verify.stdout)]"

    New-Item -ItemType Directory -Path $restoreDir -Force | Out-Null
    $restore = Invoke-Exe $backupExe @('restore', '--input', $backupFile, '--data-dir', $restoreDir) $binDir
    $restoreJson = $null
    try { $restoreJson = $restore.stdout | ConvertFrom-Json } catch { }
    Add-Check 'restore_into_empty_dir' ($restore.exit_code -eq 0 -and $restoreJson.status -eq 'ok') "exit=$($restore.exit_code) stdout=[$($restore.stdout)] stderr=[$($restore.stderr)]"
    Add-Check 'restored_db_present' (Test-Path -LiteralPath (Join-Path $restoreDir 'work-memory.db')) (Join-Path $restoreDir 'work-memory.db')

    $countsEqual = $false
    if ($verifyJson -and $restoreJson) {
        $countsEqual = (($verifyJson.counts | ConvertTo-Json -Compress) -eq ($restoreJson.counts | ConvertTo-Json -Compress))
    }
    Add-Check 'restored_counts_match' $countsEqual "verify.counts=$(if ($verifyJson) { $verifyJson.counts | ConvertTo-Json -Compress } else { 'n/a' }) restore.counts=$(if ($restoreJson) { $restoreJson.counts | ConvertTo-Json -Compress } else { 'n/a' })"

    $reexport = Invoke-Exe $backupExe @('export', '--data-dir', $restoreDir, '--output', $reexportFile) $binDir
    $digestEqual = $false
    if ($reexport.exit_code -eq 0) {
        $original = [IO.File]::ReadAllText($backupFile, [Text.Encoding]::UTF8) | ConvertFrom-Json
        $again = [IO.File]::ReadAllText($reexportFile, [Text.Encoding]::UTF8) | ConvertFrom-Json
        $digestEqual = $original.manifest.payload_sha256 -eq $again.manifest.payload_sha256
        Add-Check 'reexport_payload_sha256' $digestEqual "original=$($original.manifest.payload_sha256) reexport=$($again.manifest.payload_sha256)"
    } else {
        Add-Check 'reexport_payload_sha256' $false "re-export exit=$($reexport.exit_code) stderr=[$($reexport.stderr)]"
    }

    # ------------------------------------------------------------- file summary
    Write-Host ""
    Write-Host "==> packaged files"
    foreach ($file in (Get-ChildItem -LiteralPath $PackageDir -File | Sort-Object Name)) {
        Write-Host ("  {0,-28} {1,12}" -f $file.Name, (Get-FileSizeText $file.Length))
    }
    $total = (Get-ChildItem -LiteralPath $PackageDir -Recurse -File | Measure-Object -Property Length -Sum).Sum
    Write-Host ("  directory total: {0}" -f (Get-FileSizeText $total))
} catch {
    Add-Check 'verify_release_run' $false $_.Exception.Message
} finally {
    Stop-OwnedService $serviceHandle
    if (-not $KeepTemp) {
        foreach ($path in $tempRoots) { Remove-Item -LiteralPath $path -Recurse -Force -ErrorAction SilentlyContinue }
        if ($extractRoot) { Remove-Item -LiteralPath $extractRoot -Recurse -Force -ErrorAction SilentlyContinue }
    }
}

$ok = $script:Issues.Count -eq 0
$verdict = [ordered]@{
    status = if ($ok) { 'ok' } else { 'failed' }
    version = $version
    package_dir = $PackageDir
    package_zip = $ZipPath
    checks = $script:Checks
    failures = @($script:Issues)
    exit_code = if ($ok) { 0 } else { 1 }
}
$json = $verdict | ConvertTo-Json -Depth 6
Write-Host ""
Write-Host $json
Write-Host ""
Write-Host ("VERIFY-RELEASE " + $verdict.status + " version=" + $version + " failures=" + $script:Issues.Count)
exit $verdict.exit_code
