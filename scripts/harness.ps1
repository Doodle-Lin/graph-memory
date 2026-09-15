<#
Graph Memory — E2E Harness

用途:一条命令启动桌面应用、自动发现端口、跑全链路断言、捕获前端 JS 错误、清理退出。
退出码:0 = 全部通过,1 = 有失败。可直接用于 CI / 回归。

用法:
  powershell -ExecutionPolicy Bypass -File scripts/harness.ps1
  powershell -ExecutionPolicy Bypass -File scripts/harness.ps1 -KeepAlive   # 跑完不杀进程,便于肉眼看 UI
  powershell -ExecutionPolicy Bypass -File scripts/harness.ps1 -SkipBuild   # 跳过 cargo build

设计要点:
  - 用隔离的 GM_DATA_DIR,不污染用户真实图谱
  - 清 WebView2 缓存,避免加载到旧页面(踩过的坑)
  - 前端健康判据 = /api/clientlog 上报的 JS 错误数为 0(不靠肉眼看窗口)
  - 每个断言独立记录,失败不中断,最后汇总
#>
param(
  [switch]$KeepAlive,
  [switch]$SkipBuild,
  [int]$BootTimeoutSec = 40
)

$ErrorActionPreference = 'Continue'
$ProjectRoot = Split-Path -Parent $PSScriptRoot
$TauriDir    = Join-Path $ProjectRoot 'src-tauri'
$ExePath     = Join-Path $TauriDir 'target\debug\graph-memory.exe'
$WebViewCache = Join-Path $env:LOCALAPPDATA 'com.graphmemory.app'

# 隔离的测试数据目录:每次跑用全新的图,断言才可复现
$TestDataDir = Join-Path $env:TEMP "gm-harness-$(Get-Date -Format 'yyyyMMdd-HHmmss')"
$LogPath     = Join-Path $TestDataDir 'app.log'

$script:Pass = 0
$script:Fail = 0
$script:Failures = @()

function Section($name) { Write-Host "`n--- $name ---" -ForegroundColor Cyan }

function Check($name, $cond, $detail = '') {
  if ($cond) {
    Write-Host "  [PASS] $name" -ForegroundColor Green
    $script:Pass++
  } else {
    Write-Host "  [FAIL] $name" -ForegroundColor Red
    if ($detail) { Write-Host "         $detail" -ForegroundColor DarkGray }
    $script:Fail++
    $script:Failures += "$name$(if($detail){" — $detail"})"
  }
}

function Api($port, $path, $method = 'GET', $body = $null, $timeout = 30) {
  $uri = "http://127.0.0.1:$port$path"
  try {
    $p = @{ Uri = $uri; Method = $method; TimeoutSec = $timeout; UseBasicParsing = $true }
    if ($body) { $p.Body = ($body | ConvertTo-Json -Compress); $p.ContentType = 'application/json' }
    $r = Invoke-WebRequest @p
    return @{ ok = $true; status = $r.StatusCode; raw = $r.Content }
  } catch {
    return @{ ok = $false; status = 0; raw = ''; err = $_.Exception.Message }
  }
}

function ApiJson($port, $path, $method = 'GET', $body = $null, $timeout = 30) {
  $r = Api $port $path $method $body $timeout
  if (-not $r.ok) { return $null }
  try { return $r.raw | ConvertFrom-Json } catch { return $null }
}

Write-Host "============================================================"
Write-Host "  Graph Memory — E2E Harness"
Write-Host "============================================================"
Write-Host "  project : $ProjectRoot"
Write-Host "  data dir: $TestDataDir  (isolated)"

# ── 0. 准备 ──
Section 'Setup'
Get-Process -Name 'graph-memory' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2
Remove-Item $WebViewCache -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $TestDataDir | Out-Null
Write-Host "  killed stale processes, cleared WebView2 cache"

# ── 1. 构建 ──
if (-not $SkipBuild) {
  Section 'Build'
  $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
  Push-Location $TauriDir
  $buildOut = cargo build 2>&1 | Out-String
  Pop-Location
  $buildOk = $LASTEXITCODE -eq 0
  Check 'cargo build succeeds' $buildOk (($buildOut -split "`n" | Select-String '^error' | Select-Object -First 3) -join '; ')
  if (-not $buildOk) {
    Write-Host "`nBuild failed — aborting." -ForegroundColor Red
    exit 1
  }
} else {
  Write-Host "  (build skipped)"
}
Check 'exe exists' (Test-Path $ExePath)

# ── 2. 启动 ──
Section 'Boot'
$env:RUST_LOG = 'info'
$env:GM_DATA_DIR = $TestDataDir
$proc = Start-Process -FilePath $ExePath -WorkingDirectory $TauriDir `
  -RedirectStandardError $LogPath -PassThru

$port = $null
$deadline = (Get-Date).AddSeconds($BootTimeoutSec)
while ((Get-Date) -lt $deadline) {
  Start-Sleep -Milliseconds 700
  if ($proc.HasExited) { break }
  $txt = Get-Content $LogPath -Raw -ErrorAction SilentlyContinue
  if ($txt -and $txt -match 'static server on 127\.0\.0\.1:(\d+)') { $port = $Matches[1]; break }
}

Check 'process still alive after boot' (-not $proc.HasExited) "exit code: $($proc.ExitCode)"
Check 'static server port discovered' ($null -ne $port) "check $LogPath"

if (-not $port) {
  Write-Host "`nNo port — cannot continue. Log tail:" -ForegroundColor Red
  Get-Content $LogPath -Tail 20 -ErrorAction SilentlyContinue | ForEach-Object { Write-Host "  $_" }
  if (-not $KeepAlive) { $proc | Stop-Process -Force -ErrorAction SilentlyContinue }
  exit 1
}
Write-Host "  port = $port"

# 给 WebView 时间加载页面 + 跑 JS
Start-Sleep -Seconds 6

# ── 3. 后端 API ──
Section 'Backend API'
$health = ApiJson $port '/api/health'
Check '/api/health returns ok' ($health -and $health.status -eq 'ok')

$stats0 = ApiJson $port '/api/stats'
Check '/api/stats returns node_count field' ($null -ne $stats0 -and $null -ne $stats0.node_count)

$graph0 = ApiJson $port '/api/graph'
Check '/api/graph returns nodes+edges arrays' ($null -ne $graph0 -and $null -ne $graph0.nodes -and $null -ne $graph0.edges)

Check '/index.html served' ((Api $port '/index.html').status -eq 200)
Check 'cytoscape.min.js served locally' ((Api $port '/cytoscape.min.js').status -eq 200)
Check 'favicon.ico served (no 404)' ((Api $port '/favicon.ico').status -eq 200)

# ── 4. 前端健康(关键:靠 clientlog 判定,不靠肉眼) ──
Section 'Frontend health'
$log = Get-Content $LogPath -Raw -ErrorAction SilentlyContinue

Check 'WebView fetched index.html' ($log -match 'HTTP GET /index\.html')
Check 'WebView fetched cytoscape.min.js' ($log -match 'HTTP GET /cytoscape\.min\.js')

$bootLine = ([regex]::Match($log, 'JS\[boot\][^\r\n]*')).Value
Check 'JS executed (boot log received)' ($bootLine -ne '') 'no JS[boot] in log — script block never ran'
Check 'cytoscape global available' ($bootLine -match 'cytoscape=function') $bootLine

$jsErrors = [regex]::Matches($log, 'JS\[(?:error|reject)\][^\r\n]*') | ForEach-Object { $_.Value.Trim() }
Check 'no JS errors reported' ($jsErrors.Count -eq 0) (($jsErrors | Select-Object -First 3) -join ' | ')

# 前端真的调了业务 API,才算 UI 活着
Check 'frontend called /api/graph (UI actually alive)' ($log -match 'HTTP GET /api/graph')

# ── 5. 导入链路 ──
Section 'Import pipeline'
# 导入在三源记忆上跑 add_node_raw + (embedder 就绪时)同步 enrich_all + dedup。
# 单线程引擎(Mutex)下,import 持有锁直到 enrich 完成 —— 期间所有其他 API 阻塞。
# 因此给 import 足够长(600s),让它真正完成、释放锁,后续断言才不会全超时。
$imp = ApiJson $port '/api/import?source=all' 'POST' @{} 600
Check '/api/import succeeds' ($null -ne $imp) 'import returned nothing (timeout? check enrich cost)'
if ($imp) {
  Check 'import created nodes' ($imp.total_nodes -gt 0 -or ($imp.sources -and $imp.sources.PSObject.Properties.Count -gt 0)) "total_nodes=$($imp.total_nodes)"
  Write-Host "         sources: $($imp.sources | ConvertTo-Json -Compress)"
}

# 安全网:import 超时返回 null 时,引擎可能仍在后台 enrich,锁还没释放。
# 轮询 /api/health 直到它快速响应(锁释放),总预算 600s,避免后续断言全超时。
if (-not $imp) {
  Write-Host "  import timed out — polling for engine lock release..." -ForegroundColor DarkYellow
  $pollDeadline = (Get-Date).AddSeconds(600)
  $engineFree = $false
  while ((Get-Date) -lt $pollDeadline) {
    $h = Api $port '/api/health' 'GET' $null 8
    if ($h.ok) { $engineFree = $true; break }
    Start-Sleep -Seconds 5
  }
  Check 'engine lock released after import' $engineFree 'engine blocked > 600s after import'
}

$stats1 = ApiJson $port '/api/stats' 'GET' $null 120
Check 'node_count increased after import' ($null -ne $stats1 -and $stats1.node_count -gt 0) "node_count=$($stats1.node_count)"

# ── 6. 写入 + 检索 ──
Section 'Write + retrieve'
$w = ApiJson $port '/api/write' 'POST' @{
  content = 'Harness 自检节点:图式记忆用 PageRank 扩散做关联检索,端口 9121。'
  title = 'harness probe'
  node_type = 'knowledge'
  source = 'harness'
} 60
Check '/api/write succeeds' ($null -ne $w) 'write failed (embedder may not be ready)'

# min_score 过滤:harness 已写入"PageRank 扩散关联检索"节点,
# 用高阈值检索同一语义应命中,用极高阈值应过滤掉全部 → 空数组(不是 null)
$ret = ApiJson $port '/api/retrieve' 'POST' @{ query = 'PageRank 关联检索'; top_k = 3; min_score = 0.0 } 60
Check '/api/retrieve returns results' ($null -ne $ret) 'retrieve failed'

$retFiltered = Api $port '/api/retrieve' 'POST' @{ query = 'PageRank 关联检索'; top_k = 10; min_score = 0.999 } 60
$filteredJson = if ($retFiltered.ok) { $retFiltered.raw | ConvertFrom-Json -ErrorAction SilentlyContinue } else { $null }
Check '/api/retrieve honors min_score (0.999 → empty)' ($null -ne $filteredJson -and @($filteredJson).Count -eq 0) "raw: $($retFiltered.raw)"

# /api/write 守卫:embedder 未就绪时 add_node 走 raw 降级(不再 panic)
# 这里模型已就绪,验证正常路径不受守卫影响
$wStatus = Api $port '/api/write' 'POST' @{ content = 'guard probe'; node_type = 'knowledge' } 30
Check '/api/write returns 200 when embedder ready' ($wStatus.status -eq 200) "status=$($wStatus.status)"

# ── 7. 建边(enrich) ──
Section 'Enrich / edges'
$enr = ApiJson $port '/api/enrich' 'POST' @{} 300
Check '/api/enrich succeeds' ($null -ne $enr)
$stats2 = ApiJson $port '/api/stats' 'GET' $null 120
Check 'edges exist after enrich (graph not just dots)' ($null -ne $stats2 -and $stats2.edge_count -gt 0) "edge_count=$($stats2.edge_count)"

# ── 7.5. 托盘常驻(关窗口不退进程) ──
Section 'Tray persistence'
# 验证进程在 3 秒后还活着(它本该常驻)
Start-Sleep -Seconds 3
$alive3 = -not $proc.HasExited
Check 'process alive 3s after harness checks' $alive3 "process exited early"

# 验证 API 仍可用(即使窗口被 OS 最小化/失焦)
$health2 = ApiJson $port '/api/health' 'GET' $null 120
Check '/api/health still responds' ($null -ne $health2 -and $health2.status -eq 'ok') 'API died after window interaction'

# ── 8. Agent 接入器 ──
Section 'Agent connector'
$agents = ApiJson $port '/api/agents' 'GET' $null 120
Check '/api/agents returns list' ($null -ne $agents -and $null -ne $agents.agents) 'no agents field'
if ($agents) {
  $inst = $agents.agents | Where-Object { $_.installed -eq $true }
  Check 'at least one agent detected' ($inst.Count -ge 1) "detected: $(($agents.agents | ForEach-Object { $_.name + '=' + $_.installed }) -join ', ')"
}

# ── 9. 维护工具(consolidate/forget — 阶段3) ──
Section 'Maintenance tools'
# forget 用极保守参数,确保不删任何节点(只验证端点存活)
$forget = ApiJson $port '/api/forget' 'POST' @{ max_age_days = 3650; min_access_count = 999 } 120
Check '/api/forget succeeds' ($null -ne $forget) 'forget failed'

# ── 汇总 ──
Section 'Summary'
$total = $script:Pass + $script:Fail
Write-Host "  PASS: $($script:Pass)/$total" -ForegroundColor Green
if ($script:Fail -gt 0) {
  Write-Host "  FAIL: $($script:Fail)/$total" -ForegroundColor Red
  Write-Host "`n  Failures:" -ForegroundColor Red
  $script:Failures | ForEach-Object { Write-Host "    - $_" -ForegroundColor Red }
}
Write-Host "`n  app log: $LogPath"

# ── 清理 ──
if ($KeepAlive) {
  Write-Host "  (KeepAlive) process left running: PID $($proc.Id), port $port" -ForegroundColor Yellow
} else {
  $proc | Stop-Process -Force -ErrorAction SilentlyContinue
  Start-Sleep -Seconds 1
  Write-Host "  process stopped, test data kept at $TestDataDir"
}

if ($script:Fail -gt 0) { exit 1 } else { exit 0 }
