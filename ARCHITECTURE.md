# Graph Memory — 架构文档

> 本文件是桌面应用形态的架构现状、设计决策、约束的单一事实来源。
> 最近更新：2026-09-08（阶段 1-4 优化完成后）。

---

## 1. 产品定位

一个**本地常驻的"记忆网关"**：各 agent（Claude Code / Codex / Hermes）通过统一接口读写同一张知识图谱，用户在桌面托盘点开能看/管。agent 不感知彼此，但共享同一份记忆。

形态：**Tauri 2 桌面应用（Rust 后端 + Web 前端）**，单二进制，本地运行，数据不出本机。

---

## 2. 当前架构

```
┌─────────────────────────────────────────────────────────┐
│  Tauri 桌面进程 (graph-memory.exe)                       │
│  ┌──────────────────────────────────────────────────┐   │
│  │  GraphEngine (src/engine.rs)                      │   │
│  │  - SQLite (graph.db, WAL) 持久化                  │   │
│  │  - FTS5 trigram 全文检索 (nodes_fts)              │   │
│  │  - embeddings 表 (BLOB, model 标签)               │   │
│  │  - node_history 表 (更新历史)                     │   │
│  │  - petgraph 内存图 + fastembed (BGE-Small-ZH)     │   │
│  │  - BM25 + Embedding → RRF → PPR 多跳 → MMR        │   │
│  │  - UUID id + content_hash 精确去重                │   │
│  │  - embedding 相似度 0.85 近似去重                 │   │
│  │  - auto_link 阈值 0.5 + 边类型权重                │   │
│  │  - 时间衰减(90天) + 访问计数 + MMR多样性           │   │
│  │  - consolidate / forget_stale / dedup_scan       │   │
│  └──────────────────────────────────────────────────┘   │
│           ▲                        ▲                    │
│  ┌────────┴───────┐    ┌───────────┴──────────┐         │
│  │ HTTP /api/*    │    │ Tauri invoke 命令     │         │
│  │ static_server  │    │ (lib.rs, 备用)        │         │
│  │ .rs:9121       │    │                       │         │
│  └────────┬───────┘    └───────────┬──────────┘         │
│  ┌────────┴────────────────────────┴──────────┐         │
│  │  Webview (frontend/index.html)              │         │
│  │  检索列表 + 节点详情/编辑 + 图谱 tab        │         │
│  └────────────────────────────────────────────┘         │
│  系统托盘常驻 + 后台维护线程(每60min)                    │
└─────────────────────────────────────────────────────────┘
            ▲ HTTP (127.0.0.1:9121)
┌───────────┴──────────────────────────────────────────────┐
│  MCP stdio server (mcp_server.exe)                        │
│  7 工具: retrieve/write/extract/update/recent/          │
│         consolidate/forget                               │
│  端口发现: 读 ~/.graph-memory/port 或 GM_API_URL         │
└──────────────────────────────────────────────────────────┘
            ▲ stdio (JSON-RPC 2.0)
┌───────────┴──────────────────────────────────────────────┐
│  Agent (Claude Code / Codex / Hermes / ...)              │
│  接入时自动部署 SKILL.md(行为指令:自动检索/写入)          │
└──────────────────────────────────────────────────────────┘
```

### 2.1 模块职责

| 文件 | 职责 |
|---|---|
| `src-tauri/src/engine.rs` | 图引擎核心：SQLite+FTS5+petgraph+fastembed+检索/去重/PPR/MMR/consolidate/forget |
| `src-tauri/src/importer.rs` | 三源导入：hermes(MEMORY.md+skills,跳过graph-memory自身)、claude(YAML)、codex(jsonl) |
| `src-tauri/src/llm_extract.rs` | LLM 提炼：OpenAI兼容API，URL自动补/v1/，HTTP状态检查，信息保留守卫 |
| `src-tauri/src/static_server.rs` | HTTP API + 静态文件 + SSE流式refine + LLM配置 + agent接入 + dedup_scan |
| `src-tauri/src/agent_connector.rs` | Agent接入：detect/connect/disconnect + YAML(Hermes)/JSON(Claude/Codex) + deploy_skill + 原子写 |
| `src-tauri/src/lib.rs` | Tauri入口：托盘、窗口、模型加载、后台维护线程、.env加载 |
| `src-tauri/src/bin/mcp_server.rs` | 独立MCP stdio server，Content-Length输入兼容+换行输出 |
| `frontend/index.html` | 单文件SPA：检索列表+节点详情/编辑+图谱tab+LLM配置弹窗+提炼进度面板 |
| `frontend/cytoscape.min.js` | 本地vendored，离线可用 |
| `SKILL.md` | Agent行为指令：回答前检索/回答后写入/发现过时更新/无需用户指令 |

### 2.2 检索算法（详细）

```
1. 实体抽取：query → ASCII标识符(8397/A800) → 分路BM25
2. BM25：FTS5 trigram，split_query_terms拆成ASCII+CJK 3-gram，OR连接
3. Embedding：BGE-Small-ZH，O(N)遍历cosine
4. RRF(k=60)：1/(60+rank) 融合BM25和Embedding排名
5. 准入门槛：原始cosine≥0.2 OR BM25命中（RRF只排序不做门槛）
6. PPR迭代(3轮,α=0.5)：p = 0.5·seed + 0.5·W^T·p
   - 双向扩散(Out+In)
   - 边类型权重(depends_on×1.2, same_topic×0.5)
7. 时间衰减：exp(-Δt/90天)，最多减半不归零
8. 访问计数：log(1+access_count)×0.05
9. MMR多样性：贪心选，与已选embedding>0.85的跳过
10. access_count+1, last_accessed=now (命中节点)
```

### 2.3 数据流

```
导入 → add_node_raw(无embedding快入库) → 模型就绪 → enrich_all(补embedding+建边)
                                          ↓
检索 → BM25+Embedding → RRF → PPR → 时间衰减 → MMR → 返回 + 更新access_count
                                          ↓
写入 → content_hash去重 → embedding去重(0.85) → 新建(UUID id) → auto_link(0.5)
                                          ↓
提炼 → 跳过refined:true → LLM调用 → mark_refined → SSE流式进度
                                          ↓
后台(每60min) → forget_stale(>180天,access<2,degree<2保留) → dedup_scan(只读)
```

---

## 3. 数据模型

### SQLite 表

```sql
nodes(id, content, title, node_type, source, metadata, created_at, updated_at, content_hash, access_count, last_accessed)
edges(source, target, relation, weight, metadata, created_at)
embeddings(node_id, model, vector BLOB)  -- 持久化embedding，重启不重算
node_history(node_id, old_title, old_content, changed_at)  -- 更新历史
nodes_fts(node_id, title, content)  -- FTS5 trigram虚拟表
```

### 节点 id

- UUID v4（不再依赖 content hash）
- content_hash 列做精确去重（与 id 解耦，update 后 id 稳定）

### 边

- auto_link：cosine > 0.5 自动建（max 3 条）
- relation 类型：related_to / strongly_related / depends_on / part_of / derived_from / same_topic
- PPR 扩散时按 relation 加权

---

## 4. MCP 工具

| 工具 | HTTP | 说明 |
|---|---|---|
| retrieve | POST /api/retrieve | 检索（混合+PPR+MMR） |
| write | POST /api/write | 写入（去重+建边） |
| extract | POST /api/extract | LLM提炼对话→知识 |
| update | POST /api/update | 修正节点（存历史） |
| recent | GET /api/recent | 最近添加 |
| consolidate | POST /api/consolidate | 合并节点（边迁移） |
| forget | POST /api/forget | 遗忘陈旧节点 |

---

## 5. 已完成的优化（阶段 1-4）

| 阶段 | 修复 | 状态 |
|---|---|---|
| 1 数据完整性 | UUID id + content_hash去重不丢内容 + update存历史 + 阈值用原始cosine | ✅ |
| 2 检索算法 | 真 PPR 多跳 + auto_link 0.5 + 边类型权重 + 双向扩散 | ✅ |
| 3 记忆生命周期 | embedding持久化 + 时间衰减 + 访问计数 + MMR + consolidate/forget | ✅ |
| 4 查询理解 | 实体抽取分路检索 + extract跨批次边解析 + 批次内去重 | ✅ |
| 自动化 | 后台维护(每60min) + refine标记防重复 + SKILL自动部署 | ✅ |
| UI | 检索优先布局 + 编辑 + SSE提炼进度 + 正式图标 | ✅ |

## 6. 待完成

| 任务 | 优先级 | 说明 |
|---|---|---|
| 正式构建 | P0 | cargo tauri build 出 .msi/.exe 安装包 |
| 开机自启 | P1 | Tauri autostart 或注册表 |
| 崩溃恢复 | P1 | watchdog 或守护进程 |
| ANN 索引 | P2 | 1000+节点时需要(HNSW / sqlite-vec) |
| RwLock | P2 | retrieve读锁/write写锁，refine释放锁 |
| 事务 | P2 | nodes+fts写入包事务 |

## 7. 约束与红线

1. **单一引擎实例**：MCP server 是无状态 HTTP 代理
2. **本地优先**：数据不出本机，LLM 仅用于提取且可选
3. **不重写已调好的算法**：PPR 参数、auto_link 阈值、MMR 阈值已验证
4. **脱敏**：代码/文档不出现个人信息

## 8. 启动方式

```bash
# 开发
cd src-tauri && cargo tauri dev

# 生产构建
cd src-tauri && cargo tauri build

# MCP server（独立二进制）
cd src-tauri && cargo build --bin mcp_server
```

Agent 接入示例（Claude Code `~/.claude.json`）：
```json
{
  "mcpServers": {
    "graph-memory": {
      "command": "path/to/mcp_server.exe",
      "env": { "GM_API_URL": "http://127.0.0.1:9121" }
    }
  }
}
```
