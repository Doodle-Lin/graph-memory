# Graph Memory

> 本地知识图谱记忆系统 — 一个桌面常驻的记忆网关，让 AI agent（Claude Code / Codex / Hermes）跨会话检索和写入同一张知识图谱，不用从头交代。

**English:** Graph Memory is a local knowledge-graph memory gateway. Agents read/write a shared graph via MCP tools, so knowledge captured in one session is reachable in every other. Runs locally — embeddings are a local BGE-Small-ZH model, the graph is SQLite + FTS5, and the LLM is only used (optionally) for knowledge extraction.

## 特性

- **混合检索** — BM25 (FTS5 trigram) + Embedding (BGE-Small-ZH) → RRF 融合 → PPR 多跳图扩散 → MMR 多样性
- **多实体分路检索** — "8397板子和A800的区别" 能同时覆盖两个实体邻域
- **跨 agent 接入** — 一键接入 Claude Code / Hermes / Codex，自动部署 SKILL.md 行为指令
- **知识生命周期** — 写入（auto_link 建边 + 三层去重）→ 提炼（LLM，标记防重复花 token）→ 更新（存历史）→ 合并 → 遗忘（后台自动）
- **embedding 持久化** — SQLite 存储向量，重启不重算
- **时间衰减 + 访问计数** — 常用的、近期更新的节点优先返回
- **后台维护** — 每 60 分钟自动遗忘陈旧节点 + 去重扫描，零 LLM token
- **系统托盘常驻** — 关窗不退进程，保证 agent 随时能调到 HTTP API
- **本地运行** — 数据不出本机，embedding 模型本地加载

## 快速开始

### 1. 构建

```bash
git clone https://github.com/Doodle-Lin/graph-memory.git
cd graph-memory
cd src-tauri
cargo tauri build
```

产物：`target/release/bundle/nsis/Graph Memory_0.2.0_x64-setup.exe`

### 2. 启动

双击安装后启动，或开发模式：

```bash
cd src-tauri
cargo tauri dev
```

首次启动会下载 embedding 模型（BGE-Small-ZH-v1.5，~100MB），之后缓存到本地。

### 3. 导入已有记忆

在应用界面点"导入"按钮，或：

```bash
curl -X POST http://127.0.0.1:9121/api/import?source=all
```

支持 Hermes（MEMORY.md / USER.md / skills）、Claude Code（memory/*.md）、Codex（history.jsonl）。

### 4. 接入 Agent

在应用左侧"Agent 接入"面板，点"接入"按钮。会自动：
- 写 MCP 配置到 agent 的配置文件（Claude: `.claude.json` / Hermes: `config.yaml`）
- 部署 SKILL.md 到 agent 的 skills 目录（教 agent 自动检索/写入记忆）

重启 agent 后，自动获得 7 个记忆工具。

### 5. 配置 LLM 提炼（可选）

在应用界面点齿轮按钮，填入 Base URL / API Key / 模型名。提炼功能立即可用，检索/写入不需要 LLM。

## 在 Agent 中使用

Agent 加载 SKILL.md 后会自动执行：
- **回答前**：静默调 `retrieve` 检索已有知识
- **回答后**：如果有有价值知识，自动调 `write` 写入
- **发现过时**：调 `update` 修正
- **无需用户指令** — agent 自动判断内容是否有价值

### MCP 工具（7 个）

| 工具 | 说明 |
|---|---|
| `retrieve` | 检索记忆（BM25+Embedding+RRF → PPR 多跳 → MMR） |
| `write` | 写入新知识（auto_link 建边 + 三层去重） |
| `extract` | LLM 提炼对话→知识（批量，跨批次边解析） |
| `update` | 修正已有节点（存历史，content_hash 同步） |
| `recent` | 查看最近添加 |
| `consolidate` | 合并两个近似节点（边迁移，内容存历史） |
| `forget` | 遗忘陈旧节点（>180天 + access<2 + 保留桥节点） |

## 架构

```
┌─────────────────────────────────────────────────┐
│  Agent (Claude Code / Codex / Hermes)          │
│  ┌─────────────┐  ┌──────────────────┐        │
│  │ MCP Client  │  │ SKILL.md (行为指令) │        │
│  └──────┬──────┘  └──────────────────┘        │
│         │ stdio (JSON-RPC 2.0)                │
│  ┌──────▼──────┐                             │
│  │ MCP Server  │  (轻量 HTTP 代理)            │
│  └──────┬──────┘                             │
└─────────┼─────────────────────────────────────┘
          │ HTTP (127.0.0.1:9121)
┌─────────▼─────────────────────────────────────┐
│  Tauri 2 桌面进程 (graph-memory.exe)           │
│  ┌──────────────┐  ┌────────────┐  ┌────────┐ │
│  │ GraphEngine  │  │ LLM 提炼   │  │ 导入器 │ │
│  │ SQLite+FTS5  │  │ OpenAI兼容 │  │        │ │
│  │ BGE-Small-ZH │  └────────────┘  └────────┘ │
│  │ PPR 多跳     │                              │
│  │ MMR 去重     │  ┌────────────────────────┐ │
│  └──────┬───────┘  │ 后台维护线程           │ │
│         │          │ (每60min: forget+dedup)│ │
│  ┌──────▼────┐     └────────────────────────┘ │
│  │ SQLite    │  ┌──────────────────────────┐ │
│  │ graph.db  │  │ Webview (前端 SPA)       │ │
│  │ + FTS5    │  │ 检索列表 + 详情 + 图谱    │ │
│  │ + embeddings│ └──────────────────────────┘ │
│  └───────────┘  系统托盘常驻                  │
└─────────────────────────────────────────────────┘
```

## 检索算法

```
query → 实体抽取(ASCII标识符分路)
  → BM25 (FTS5 trigram, OR 连接)  ─┐
  → Embedding (BGE-Small-ZH 语义)  ─┤→ RRF(k=60) 融合 → 归一化
  → 准入门槛(cosine≥0.2 OR BM25命中)  │
                                     ↓
                              PPR 迭代(3轮, α=0.5)
                              双向扩散(Out+In)
                              边类型加权(depends_on>same_topic)
                                     ↓
                              时间衰减(90天半衰期)
                              访问计数加成(log(1+n))
                              MMR 多样性(>0.85 跳过)
                                     ↓
                              top_k 结果
```

## API

| 端点 | 方法 | 说明 |
|---|---|---|
| `/api/retrieve` | POST | 检索知识 |
| `/api/write` | POST | 写入新知识 |
| `/api/update` | POST | 更新已有节点 |
| `/api/extract` | POST | LLM 提炼对话→知识 |
| `/api/refine` | POST | SSE 流式批量提炼 |
| `/api/recent` | GET | 最近添加 |
| `/api/graph` | GET | 全图快照 |
| `/api/stats` | GET | 图统计 + 类型/来源分布 |
| `/api/search` | GET | 关键词搜索 |
| `/api/neighbors/{id}` | GET | BFS 邻居 |
| `/api/nodes/{id}` | DELETE | 删除节点 |
| `/api/import` | POST | 导入外部记忆 |
| `/api/enrich` | POST | 补全 embedding + 建边 |
| `/api/consolidate` | POST | 合并两个节点 |
| `/api/forget` | POST | 遗忘陈旧节点 |
| `/api/dedup/scan` | GET | 近似对候选(只读) |
| `/api/llm/status` | GET | LLM 配置状态 |
| `/api/llm/config` | POST | 写 LLM 配置到 .env |
| `/api/agents` | GET | 检测已安装的 agent |
| `/api/agents/connect` | POST | 接入 agent(写配置+部署SKILL) |
| `/api/agents/disconnect` | POST | 断开 agent(清配置+删SKILL) |
| `/api/health` | GET | 健康检查 |

## 配置项

| 环境变量 | 默认 | 说明 |
|---|---|---|
| `GM_LLM_API_KEY` | (无) | LLM API key，仅 extract/refine 需要 |
| `GM_LLM_BASE_URL` | (无) | OpenAI 兼容 base url |
| `GM_LLM_MODEL` | (无) | 模型名 |
| `GM_EMBEDDING_MODEL` | BGE-Small-ZH-v1.5 | 本地嵌入模型 |
| `GM_PORT` | 9121 | 服务端口 |
| `HERMES_HOME` | %LOCALAPPDATA%\hermes | Hermes 记忆根 |
| `CLAUDE_HOME` | ~/.claude | Claude Code 根 |
| `CODEX_HOME` | ~/.codex | Codex 根 |

## License

MIT
