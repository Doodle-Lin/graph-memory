# Graph Memory — 架构文档与路线

> 本文件是当前桌面应用形态的架构现状、下一步方向、约束的单一事实来源。
> 维护者:Claude Fable 5 + 作者。最近更新:2026-08-20。

---

## 1. 产品定位

一个**本地常驻的"记忆网关"**:各 agent(Claude Code / Codex / Hermes)通过统一接口读写同一张知识图谱,用户在桌面托盘点开能看/管。agent 不感知彼此,但共享同一份记忆。

类比:ccswitch 切换底层模型,Graph Memory 切换持久记忆层。轻、常驻、一键。

形态:**Tauri 2 桌面应用(Rust 后端 + Web 前端)**,单二进制,本地运行,数据不出本机。

---

## 2. 当前架构(B 方案:全 Rust 单引擎)

```
┌─────────────────────────────────────────────────────────┐
│  Tauri 桌面进程 (graph-memory.exe)                       │
│  ┌──────────────────────────────────────────────────┐   │
│  │  GraphEngine (src/engine.rs)                      │   │
│  │  - SQLite (graph.db, WAL) 持久化                  │   │
│  │  - petgraph 内存图 + fastembed (BGE-Small-ZH)     │   │
│  │  - add_node: MD5→embedding(>0.85)→新建 三层去重   │   │
│  │  - auto_link: 余弦相似度建边                       │   │
│  │  - retrieve: 语义种子 + PageRank 扩散融合          │   │
│  │  - enrich_all: 补 embedding + 批量建边            │   │
│  │  - delete_node / neighbors BFS                    │   │
│  └──────────────────────────────────────────────────┘   │
│           ▲                        ▲                    │
│           │                        │                    │
│  ┌────────┴───────┐    ┌───────────┴──────────┐         │
│  │ HTTP /api/*    │    │ Tauri invoke 命令     │         │
│  │ static_server  │    │ (lib.rs, 前端用)     │         │
│  │ .rs:9121       │    │                       │         │
│  └────────┬───────┘    └───────────┬──────────┘         │
│           │                        │                    │
│  ┌────────┴────────────────────────┴──────────┐         │
│  │  Webview (frontend/index.html)              │         │
│  │  Cytoscape.js (本地 vendor) 可视化          │         │
│  └────────────────────────────────────────────┘         │
└─────────────────────────────────────────────────────────┘
            ▲ HTTP (127.0.0.1:9121)
            │
┌───────────┴──────────────────────────────────────────────┐
│  MCP stdio server (mcp_server.exe)                        │
│  独立二进制,agent 通过 stdio 拉起,HTTP 代理到桌面进程     │
│  5 工具: retrieve/write/extract/update/recent            │
│  端口发现: 读 ~/.graph-memory/port 或 GM_API_URL 环境变量 │
└──────────────────────────────────────────────────────────┘
            ▲ stdio (JSON-RPC 2.0)
            │
┌───────────┴──────────────────────────────────────────────┐
│  Agent (Claude Code / Codex / Hermes / ...)              │
│  通过 MCP 配置接入,自动获得 5 个记忆工具                 │
└──────────────────────────────────────────────────────────┘
```

### 2.1 进程模型(关键决策)

**单一引擎实例原则**:只有一个 `GraphEngine`(在桌面进程里)。MCP server **不持有图**,只做 HTTP 转发。这避免了"两个进程各持一份图导致数据分叉"的炸弹(这是 Python 时代双引擎的教训,见历史决策 §8)。

- 桌面进程 = 常驻 daemon + GUI + 引擎
- MCP server = stateless proxy(可被多个 agent 各拉一份)
- 前端 = 纯 HTTP 客户端,不用 Tauri IPC(External URL 模式下 IPC 插件不注入,会报 Plugin not found)

### 2.2 模块职责

| 文件 | 职责 |
|---|---|
| `src-tauri/src/engine.rs` | 图引擎核心:SQLite + petgraph + fastembed + 检索/去重/建边 |
| `src-tauri/src/importer.rs` | 三源导入:hermes(MEMORY.md § 分隔 + skills)、claude(YAML frontmatter)、codex(history.jsonl) |
| `src-tauri/src/llm_extract.rs` | LLM 知识提炼:OpenAI 兼容 API,文本→结构化节点+关系→入库 |
| `src-tauri/src/static_server.rs` | 静态文件 + 引擎 HTTP API(`/api/*`)+ 模型下载状态 |
| `src-tauri/src/lib.rs` | Tauri 入口:invoke 命令、窗口创建、模型后台下载、自动 enrich |
| `src-tauri/src/bin/mcp_server.rs` | 独立 MCP stdio server,HTTP 代理到桌面进程 |
| `frontend/index.html` | 单文件 SPA,Cytoscape.js 可视化,纯 HTTP 调 `/api/*` |
| `frontend/cytoscape.min.js` | 本地 vendored,离线可用(不走 CDN) |
| `graph_memory/`(Python) | **legacy/参考实现**,不再演进,benchmark/regression 仍用它做评测 |

### 2.3 数据流(导入)

```
用户点"导入" → importer 读三源记忆文件 → add_node_raw 写 SQLite(快,无 embedding)
                                     ↓
            模型就绪? ──否──→ 等(后台下载完成后自动触发 enrich_all)
                     └─是──→ enrich_all:批量算 embedding + auto_link 建边
                              → 有边 → Cytoscape cose 力导向布局正常(非网格)
                              → embedding 就绪 → 三层去重生效(后续导入)
```

**两阶段导入**是当前的关键设计:导入立刻可见节点,模型就绪后自动补全边和去重能力。否则用户要干等模型下载。

### 2.4 数据流(agent 调用)

```
agent 对话 → MCP 客户端 → stdio 启动 mcp_server.exe
          → mcp_server 读 ~/.graph-memory/port 发现桌面进程端口
          → HTTP POST /api/retrieve (或 write/extract/update/recent)
          → 桌面进程 GraphEngine 处理 → 返回
          → mcp_server 格式化为 MCP TextContent → 回 agent
```

---

## 3. API 面(HTTP,127.0.0.1:9121)

| 端点 | 方法 | 说明 | 状态 |
|---|---|---|---|
| `/api/health` | GET | 健康检查 | ✅ |
| `/api/stats` | GET | 图统计 | ✅ |
| `/api/graph` | GET | 全图快照(可视化) | ✅ |
| `/api/recent` | GET | 最近节点 | ✅ |
| `/api/retrieve` | POST | 检索(embedding+PageRank) | ✅ |
| `/api/search` | GET | 关键词搜索(spread=false) | ✅ |
| `/api/write` | POST | 写入(自动建边+去重) | ✅ |
| `/api/update` | POST | 更新已有节点 | ✅ |
| `/api/import` | POST | 导入外部记忆(支持 enrich) | ✅ |
| `/api/enrich` | POST | 手动触发补 embedding+建边 | ✅ |
| `/api/extract` | POST | LLM 提炼文本→知识 | ✅(需 GM_LLM_*) |
| `/api/llm/status` | GET | LLM 配置状态 | ✅ |
| `/api/nodes/{id}` | DELETE | 删除节点 | ✅ |
| `/api/neighbors/{id}` | GET | BFS 邻居(展开) | ✅ |
| `/api/model_status` | GET | 模型下载进度 | ✅ |
| `/api/start_download` | POST | 触发模型下载 | ✅ |
| `/api/clientlog` | POST | 前端日志上报 | ✅ |

---

## 4. MCP 工具(agent 视角)

5 个工具,通过 stdio JSON-RPC 2.0 暴露:

| 工具 | 对应 HTTP | 说明 |
|---|---|---|
| `retrieve` | POST /api/retrieve | 检索知识(关键词→PageRank扩散) |
| `write` | POST /api/write | 写入新知识(自动建边+去重) |
| `extract` | POST /api/extract | LLM 提炼对话→知识 |
| `update` | POST /api/update | 修正过时知识 |
| `recent` | GET /api/recent | 查看最近添加 |

---

## 5. 配置项

| 环境变量 | 默认 | 说明 |
|---|---|---|
| `GM_LLM_API_KEY` | (无) | LLM API key,仅 extract 需要 |
| `GM_LLM_BASE_URL` | (无) | OpenAI 兼容 base url |
| `GM_LLM_MODEL` | (无) | 模型名 |
| `GM_EMBEDDING_MODEL` | BGE-Small-ZH | 本地嵌入模型(fastembed 内置) |
| `GM_HOST` | 127.0.0.1 | 服务监听地址 |
| `GM_PORT` | 9121 | 服务端口(占用则回退随机) |
| `GM_API_URL` | (端口发现) | MCP server 直连地址 |
| `HERMES_HOME` | `~/.hermes` | Hermes 记忆根 |
| `CLAUDE_HOME` | `~/.claude` | Claude Code 根 |
| `CODEX_HOME` | `~/.codex` | Codex 根 |

LLM 配置从环境变量读(或 `.env`),不耦合任何特定 Agent 的本地配置文件(已脱敏)。

---

## 6. 下一步方向(按优先级)

### P0 — 让桌面应用真正可用

**状态图例**:`✅实测` = harness 断言通过 / `⚠️已写未验` = 代码存在但无自动化验证 / `进行中`

| # | 任务 | 状态 | 说明 |
|---|---|---|---|
| 16 | 桌面应用启动验证 | ✅实测 | harness 23/23:窗口起、JS 执行、前端调 `/api/graph`、导入 68 节点、enrich 出 340 边 |
| — | E2E harness | ✅实测 | `scripts/harness.ps1`,隔离数据目录 + 退出码,23 断言覆盖全链路 |
| — | embedder 加载 | ✅实测 | 根因是 fastembed 未指定 `with_cache_dir()` → 找不到已下载模型 → 联网失败 → embedder=None → write 失败 + 无边。已 pin 到 `~/.cache/fastembed` |
| — | 默认二进制修复 | ✅实测 | `default-run = "graph-memory"`,harness 的 `cargo build` 通过 |
| — | 前端 UI 存活 | ✅实测 | 曾因 `const isTauri` 重复声明导致整个 script 块不执行(见 §8.2)。现 `no JS errors reported` 通过 |
| — | LLM 提炼接入 | ⚠️已写未验 | `llm_extract.rs` + `/api/extract` + MCP `extract`。需 `GM_LLM_*`,harness 未覆盖(无 key 时跳过) |
| — | 固定端口 + 发现文件 | ⚠️已写未验 | 默认 9121,写 `~/.graph-memory/port`。harness 验证了端口可发现,未验证 MCP 端读取 |
| — | 模型后台下载 + 自动 enrich | ⚠️已写未验 | 冷启动(无缓存)路径未测 —— harness 跑在模型已缓存的机器上 |

**下一个验证缺口**:冷启动(删掉 `~/.cache/fastembed` 后)的下载 → loading 窗口 → 自动 enrich 全链路,harness 目前不覆盖。

### P1 — 跨 agent 接入体验(MVP 核心)

| # | 任务 | 说明 |
|---|---|---|
| 15 | 接入器面板 | 检测本机装了哪些 agent,一键把 graph-memory MCP server 写进它们的配置。这是"跨 agent 无缝衔接"的真正落点——不是图算法,是配置衔接。状态灯显示接入情况。 |
| 19 | 桌面进程托盘常驻 | 关窗口不退进程,最小化到托盘,保证 agent 随时能调到 HTTP API。否则 agent 调用时引擎没在跑。 |

### P2 — 工程收尾

| # | 任务 | 说明 |
|---|---|---|
| 11 | CONTRIBUTING.md + 诚实并发文档 | 写明并发模型(单进程引擎,MCP 是无状态代理) |
| 12 | logging + API 一致性 | 替换 print 为 log,对齐 invoke 与 HTTP 两套 API |
| 14 | Rust/Python 引擎对齐 | **降级**:Python 只作 legacy/参考,Rust 是唯一引擎。无需对齐 ID 算法(不共享数据)。只需文档说明两边数据不互通。 |
| — | 效果回归验证 | Rust 引擎的检索效果(去重/auto_link/PageRank)未跑过 benchmark,需对照 Python baseline 验证无回退 |

### P3 — 远期

- 知识图谱膨胀管理(dedup/prune/merge,Python `manage.py` 移植到 Rust)
- 自动更新/分发
- 多用户/多图谱

---

## 7. 约束与红线

### 7.1 绝不违反

1. **单一引擎实例**:永远不要让第二个进程持有可写的 `GraphEngine`。MCP server 必须是无状态 HTTP 代理。违反 = 数据分叉炸弹。
2. **不重写已调好的算法**:Python 的检索参数(PageRank alpha、融合权重、类型优先级、去重阈值 0.85、auto_link 阈值 0.3)是 benchmark 调出来的。Rust 侧已照抄,未经验证前不要改数值。
3. **本地优先**:数据不出本机。embedding 本地(fastembed),LLM 仅用于提取且可选。不引入强外部依赖。
4. **脱敏**:代码/文档/示例不出现作者个人信息(内部 IP、项目名、用户名、内部 API 地址)。已脱敏,新增内容遵守。

### 7.4 多 agent 协作约定(2026-08-20 加,踩坑后)

本项目被多个 agent 协作编辑过,发生过"两个 agent 同时改同一批文件、互相静默覆盖"的事故:
一方在追一个"幽灵语法错误",反复 patch 都不生效或打到错位置,实际是另一方在同步改同一个文件。

约定:
1. **同一时刻只有一个 agent 编辑 `src-tauri/` 和 `frontend/`**。交接靠 commit,不靠口头。
2. **交接前必须 commit**(哪怕是 WIP),不留未跟踪文件。工作树脏着交接 = 下一个人不敢动。
3. **接手第一件事**:`git log -1` + `git status`,确认基线。
4. **复现症状前先确认磁盘文件 = 自己以为的版本**。patch 工具返回 `modified since you last read` 警告时,
   立刻停下重新读文件,不要继续打 patch。
5. **不靠肉眼判断行为**,跑 `scripts/harness.ps1`。它有隔离数据目录和确定的退出码,
   是唯一能跨 agent 复现的事实来源。

### 7.5 验证纪律

任何"修好了"的声明必须有 harness 输出支撑:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/harness.ps1
# exit 0 = 全绿; exit 1 = 有失败,清单打在 Summary 里
```

不要用 `Start-Process` + `sleep` + 肉眼看日志的方式验证 —— 不可复现,且容易把
"进程被 PowerShell 会话回收"误判成"应用崩溃"(踩过)。

### 7.2 已知技术债

- `importer.rs` 的 `now()`、`import_all()` 是 dead code(warning),可清理
- `static_server.rs` 的 `fastembed_cache_dir()` 未使用,可清理
- Rust 引擎的 `retrieve` PageRank 是**简化版**(不是 networkx 的完整 Personalized PageRank,而是"语义分 + 邻居传播"近似),效果未与 Python 对齐验证
- `add_node` 在 embedder 未就绪时会 panic(`.embed()` 内部 try_new 失败),导入流程靠 `embedder_ready()` 守卫,但 `write` API 没守卫——模型没就绪时 `/api/write` 会 500
- 前端 `favicon.ico` 404(无害)

### 7.3 不做的事(范围控制)

- 不做多用户/多图谱(单用户本机工具)
- 不做云端同步(隐私优先)
- 不在 Rust 里重写 Python 的 benchmark/regression(留 Python 做)
- 不引入数据库迁移框架(SQLite schema 简单,手动 `CREATE IF NOT EXISTS` 够)

---

## 8. 历史决策(为什么是这样)

### 8.1 为什么砍掉 Python 后端

Python 后端(`graph_memory/`)功能完整(引擎、MCP、benchmark、测试、脱敏都做过),但:
- 桌面应用要求"单二进制双击就跑",带 Python + pip 不轻量
- Python 和 Rust 两套引擎 = 双引擎炸弹(同一条记忆在两边算出不同 ID,数据不互通)
- 维护两套引擎成本 = 2x

决策:**Rust 为唯一引擎,Python 降级为 legacy/参考实现**。代价是 Rust 引擎未跑过 benchmark,需验证。

### 8.2 为什么 Tauri 不用 IPC

**实测根因(2026-08-20 修正)**:前端"点按钮没反应"的真凶是 `index.html` 里 `const isTauri` 被重复声明,`Uncaught SyntaxError` 导致**整个 `<script>` 块一行都不执行**。JS 从未运行 → 所有按钮无响应。这与 IPC 无关。

次生现象:在 JS 能跑但走 `invoke()` 分支的版本里,确实会报 `xxx not allowed. Plugin not found` —— External URL 模式(前端走 `http://127.0.0.1`)下 Tauri 不注入 IPC 插件。

决策:前端统一走 HTTP,不用 IPC。理由是 External URL 模式下 IPC 确实不可用,**但要记住:UI 完全无响应时先查 JS 语法错误,不要归因于 IPC**。invoke 命令保留仅作备用。

诊断手段:`/api/clientlog` 端点 + `index.html` 里的 `window.onerror` 上报,把前端 JS 错误打进后端日志。这是排查 webview 白屏/无响应的唯一可靠途径(webview 里看不到 DevTools console)。

### 8.3 为什么 Cytoscape 本地 vendor

CDN(`cdn.jsdelivr.net`)在桌面 webview 里加载不稳(CSP/网络/跨域),`cy` 永远 undefined → 节点画不出。决策:下载到 `frontend/cytoscape.min.js` 本地引用。

### 8.4 为什么两阶段导入

`add_node`(带 embedding)要模型就绪,但首次启动要下载 ~100MB 模型,用户干等体验差。决策:导入先 `add_node_raw`(无 embedding 快速入库),模型就绪后自动 `enrich_all` 补边。代价:导入到模型就绪之间,图是散点(无边)。

---

## 9. 启动方式

```bash
# 桌面应用(开发)
cd /e/workspace/graph-memory/src-tauri && cargo tauri dev

# 桌面应用(生产构建)
cd /e/workspace/graph-memory/src-tauri && cargo tauri build

# MCP server(独立二进制,agent 用)
cd /e/workspace/graph-memory/src-tauri && cargo build --bin mcp_server
# 产物:target/debug/mcp_server.exe

# Python 后端(legacy,仅用于 benchmark/regression)
cd /e/workspace/graph-memory && python -m graph_memory.server
```

agent 接入(Claude Code `~/.claude/config.json` 示例):
```json
{
  "mcpServers": {
    "graph-memory": {
      "command": "E:/workspace/graph-memory/src-tauri/target/debug/mcp_server.exe",
      "env": { "GM_API_URL": "http://127.0.0.1:9121" }
    }
  }
}
```
