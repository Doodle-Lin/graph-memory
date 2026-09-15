# Graph Memory — E2E Harness 文档

> 单一事实来源:唯一能跨 agent 复现的验证手段。
> 对应脚本:[scripts/harness.ps1](../scripts/harness.ps1)

---

## 1. 它是什么

一条命令把桌面应用拉起来、跑全链路断言、捕获前端 JS 错误、干净退出。
退出码是机器可读的判定:`0` = 全绿,`1` = 有失败。可直接挂在 CI / 回归门禁上。

它存在的理由记录在 [ARCHITECTURE.md §7.4–7.5](../ARCHITECTURE.md):本项目被多个 agent 协作编辑过,
发生过"两个 agent 同时改同一批文件、互相静默覆盖"和"把进程被 PowerShell 回收误判成应用崩溃"的事故。
harness 用隔离数据目录 + 确定退出码 + 不靠肉眼看窗口,堵住这两类误判。

**不要用** `Start-Process` + `sleep` + 肉眼看日志的方式替代它 —— 不可复现,且容易把
"进程被 PowerShell 会话回收"误判成"应用崩溃"(踩过)。

---

## 2. 怎么跑

```powershell
# 标准(构建 + 启动 + 断言 + 清理)
powershell -ExecutionPolicy Bypass -File scripts/harness.ps1

# 跑完不杀进程,便于肉眼对照 UI
powershell -ExecutionPolicy Bypass -File scripts/harness.ps1 -KeepAlive

# 跳过 cargo build(已构建好,迭代断言时用)
powershell -ExecutionPolicy Bypass -File scripts/harness.ps1 -SkipBuild

# 启动慢的机器上调大超时
powershell -ExecutionPolicy Bypass -File scripts/harness.ps1 -BootTimeoutSec 90
```

退出码:
- `0` —— 全部断言通过
- `1` —— 有失败,清单打在 Summary 里(每条失败带 detail)

---

## 3. 设计要点

| 要点 | 为什么 |
|---|---|
| **隔离数据目录** | 每次跑用 `$env:TEMP\gm-harness-<时间戳>` 全新图,断言可复现,不污染用户真实图谱。设 `GM_DATA_DIR` 指过去。 |
| **清 WebView2 缓存** | 加载到旧页面会让断言全部假绿(踩过)。跑前删 `~\AppData\Local\com.graphmemory.app`。 |
| **前端健康靠 `/api/clientlog`** | webview 里看不到 DevTools console。前端 `window.onerror` 把 JS 错误打进后端日志,harness 扫日志里的 `JS[error]/JS[reject]`,错误数 = 0 才算通过。**不靠肉眼看窗口。** |
| **端口自动发现** | 不硬编码 9121(可能被占回退随机)。扫 app.log 里的 `static server on 127.0.0.1:(\d+)`。 |
| **断言独立不中断** | 每个 `Check` 独立记录,失败不 break,最后汇总。一次跑能看到所有问题,不是修一个才知道下一个。 |
| **两条健康线** | 后端 API 返回对 + 前端真的调了业务 API(`HTTP GET /api/graph` 在日志里)。后者是"UI 真活着"的硬证据,避免"后端通了但 JS 全挂"的假绿。 |

---

## 4. 断言清单(当前 29 条,按 section)

### Setup / Build
- `cargo build succeeds`(无 `-SkipBuild` 时)
- `exe exists`

### Boot
- `process still alive after boot`
- `static server port discovered`

### Backend API
- `/api/health returns ok`
- `/api/stats returns node_count field`
- `/api/graph returns nodes+edges arrays`
- `/index.html served`
- `cytoscape.min.js served locally`
- `favicon.ico served (no 404)`

### Frontend health
- `WebView fetched index.html`
- `WebView fetched cytoscape.min.js`
- `JS executed (boot log received)` —— `JS[boot]` 在日志里,证明 script 块跑了
- `cytoscape global available` —— boot 日志含 `cytoscape=function`,证明库加载了
- `no JS errors reported` —— `JS[error]/JS[reject]` 数 = 0
- `frontend called /api/graph (UI actually alive)` —— 业务 API 被调,不是只加载了壳

### Import pipeline
- `/api/import succeeds`
- `import created nodes`(total_nodes > 0 或有 sources)
- `node_count increased after import`

### Write + retrieve
- `/api/write succeeds`
- `/api/retrieve returns results`
- `/api/retrieve honors min_score (0.999 → empty)` —— 高阈值过滤掉全部,返回空数组(不是 null)
- `/api/write returns 200 when embedder ready` —— 守卫不误伤就绪路径

### Enrich / edges
- `/api/enrich succeeds`
- `edges exist after enrich (graph not just dots)`

### Tray persistence
- `process alive 3s after harness checks`
- `/api/health still responds`

### Agent connector
- `/api/agents returns list`
- `at least one agent detected`

### Maintenance tools
- `/api/forget succeeds` —— 阶段3 维护工具端点存活(用极保守参数,不删任何节点)

### 已知并发行为(harness 设计依据)
- 后台 auto-refine 线程启动后等 30s,先让 harness 的 import 完成,避免抢锁。
- auto-refine 的 LLM 调用在 Mutex **外**完成,只在写回时短暂持锁,不会饿死 agent API。
- 单次 `/api/import?source=all` 在 embedder 就绪时同步 enrich,期间持锁;harness 给 600s + 锁释放轮询兜底。

---

## 5. 加一条新断言

1. 想清楚它验证的是**哪条**健康线(后端 / 前端 / 数据流 / 持久化)。
2. 用 `Api` / `ApiJson` 调对应端点,在合适 section 里加一行 `Check`。
3. 失败的 `Check` 带上 `detail`(实际值),别只留一个布尔。
4. **不要**改成"失败就 break"——会掩盖后续问题。
5. 跑一遍,确认退出码符合预期(0 或 1)。

例(加一个 update 端点断言):
```powershell
$up = ApiJson $port '/api/update' 'POST' @{ query = 'harness probe'; new_content = 'updated' } 30
Check '/api/update succeeds' ($null -ne $up -and $up.action -eq 'updated') "raw: $($up | ConvertTo-Json -Compress)"
```

---

## 6. 已知不覆盖的缺口

harness 是"模型已缓存"机器上的热路径测试,**不覆盖**:

| 缺口 | 说明 | 建议 |
|---|---|---|
| **冷启动** | 删掉 `~/.cache/fastembed` 后的下载 → loading 窗口 → 自动 enrich 全链路 | 加 `-ColdStart` 模式:跑前清缓存,放宽超时,验证 `/api/model_status` 从 downloading → ready、enrich 后边数 > 0。**破坏性,默认关闭。** |
| **LLM 提炼** | `/api/extract` + MCP `extract` 需 `GM_LLM_*` | harness 无 key 时跳过;有 key 时设环境变量加断言 |
| **MCP 端端口读取** | harness 验证了端口可发现,没验证 `mcp_server.exe` 真的读 `~/.graph-memory/port` | 单独拉 `mcp_server.exe` 发 `initialize` + `tools/call retrieve`,看 HTTP 代理是否通 |
| **Rust vs Python 效果回归** | Rust retrieve 是简化 PageRank,未对照 Python baseline | 留 Python `benchmark.py` 做基准,Rust 侧跑相同 query 比对 recall |

加冷启动模式时,务必默认关闭、用独立隔离目录,且**不删用户真实缓存**——清 `$env:TEMP` 下的副本。

---

## 7. 故障排查

| 现象 | 先查 |
|---|---|
| `No port — cannot continue` | 看 `$TestDataDir\app.log` 尾 20 行。常见:端口被占(应回退但没回退)、模型加载失败、SQLite 锁。 |
| `no JS errors reported` 失败 | 日志里有 `JS[error]` 行,是前端真实崩溃。先查 [ARCHITECTURE.md §8.2] —— 历史上是 `const isTauri` 重复声明让整个 script 块不执行。 |
| `frontend called /api/graph` 失败但后端 API 全过 | JS 加载了壳但没跑业务逻辑。查 cytoscape 是否加载成功(`cytoscape global available`)。 |
| 写入相关断言失败 | `embedder may not be ready`。查 `/api/model_status`,embedder 未就绪时 `/api/write` 现在返回 503(不是 500)。等 enrich 完成或先跑 `/api/enrich`。 |
| `cargo build succeeds` 失败 | 编译错误。harness 会打印前 3 条 `error:`。修完再跑。 |
| 全绿但功能不对 | harness 覆盖的是链路存活,不是效果。效果回归另见 Python `benchmark.py`。 |

---

## 8. 在 CI 里用

```yaml
# .github/workflows/ci.yml(Windows runner,因为这是 Tauri Windows 桌面应用)
- name: E2E harness
  run: powershell -ExecutionPolicy Bypass -File scripts/harness.ps1 -SkipBuild
  # -SkipBuild:把 build 拆成上一步单独跑,日志不混在一起
```

注意:
- harness 需要 Windows + WebView2(Tauri 桌面应用)。Linux runner 只能跑 Python 引擎层 pytest,跑不了 harness。
- CI 里用 `-SkipBuild` + 前置 `cargo build` 步骤,便于在 build 失败时直接 fail fast。
- 冷启动模式(若加上)绝不能进 CI 默认路径——会拉模型下载,慢且依赖网络。
