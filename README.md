# Graph Memory

> A local knowledge-graph memory for AI coding agents — one cross-project graph that any conversation can query, so an agent in one session reaches knowledge captured in every other.

本地知识图谱记忆系统,替代传统 agent 按会话分割的记忆方式。用知识图谱 + PageRank 扩散检索实现跨概念关联,让 agent 在一个会话内访问所有项目的知识。

**English summary:** Graph Memory replaces per-session memory with a single local knowledge graph. Retrieval seeds from semantic nearest-neighbors and spreads via Personalized PageRank over auto-built edges, fusing 50% semantic + 50% graph-diffusion scores. It exposes 5 tools to agents over MCP and ships a Cytoscape.js visualization. Everything runs locally — embeddings are a local sentence-transformers model, the graph is a JSON file, and the LLM is only used (optionally) for knowledge extraction.

## 特性 / Features

- **图式关联检索** — PageRank 沿图扩散,从一个技术点带出相关的部署细节、经验教训或用户偏好
- **LLM 知识提炼** — 从对话/记忆中自动提取结构化知识节点
- **三层去重** — MD5 → embedding 相似度 >0.85 → 新建
- **记忆修正** — 发现过时信息可更新已有节点
- **MCP 集成** — 通过 MCP 自动为各 Agent 暴露 5 个工具
- **可视化** — Cytoscape.js 暗色主题,筛选/增删改查
- **本地运行** — 数据不出本机,embedding 模型本地加载

## 快速开始

### 1. 安装

```bash
git clone https://github.com/yourname/graph-memory.git
cd graph-memory
pip install -e ".[mcp,dotenv]"
```

### 2. 配置

```bash
cp .env.example .env
# 编辑 .env 填入 LLM API key 和 base_url(检索/写入不需要 LLM,只有 extract 需要)
```

### 3. 启动

```bash
python -m graph_memory.server
```

打开 http://127.0.0.1:9121/ 看可视化界面。

> 首次启动会下载 embedding 模型(默认 `BAAI/bge-base-zh-v1.5`,约 400MB),之后缓存到本地。

### 3a. 演示数据（可选）

首次体验时,可种入一组通用技术知识样例,让空白项目开箱即用:

```bash
python seed_demo.py
```

随后在 http://127.0.0.1:9121/ 即可看到一张小图。清空演示数据:删除 `data/graph.json` 和 `data/embeddings.npz` 后重启 server。

### 3b. Docker 一键运行

```bash
docker build -t graph-memory .
docker run -p 9121:9121 -v gm_data:/app/data -v gm_models:/root/.cache/huggingface graph-memory
```

### 4. 导入已有记忆

首次使用时,从 Hermes / Claude Code / Codex 导入已有记忆:

```bash
curl -X POST http://127.0.0.1:9121/api/import?source=all
```

也可以批量提炼 Claude Code session 历史:

```bash
curl -X POST http://127.0.0.1:9121/api/extract/sessions
```

> 导入路径可通过环境变量覆盖(`HERMES_HOME` / `CLAUDE_HOME` / `CODEX_HOME`),默认指向各 Agent 在用户主目录下的标准位置。

## 在 Agent 中使用

### MCP Server（自动可用）

在你的 Agent 的 MCP 配置中加入 `graph-memory` MCP server,重启后自动获得 5 个工具:

| 工具 | 说明 |
|---|---|
| `mcp_graph_memory_retrieve` | 检索知识(关键词→PageRank扩散) |
| `mcp_graph_memory_write` | 写入新知识(自动建边+去重) |
| `mcp_graph_memory_extract` | LLM 提炼对话→知识 |
| `mcp_graph_memory_update` | 修正过时知识 |
| `mcp_graph_memory_recent` | 查看最近添加 |

agent 在对话中可以直接调用这些工具,无需手动操作。

> MCP server 通过 stdio 运行,以 HTTP 客户端身份代理到 FastAPI 后端,本身不加载模型,避免与后端持有两份不一致的图数据。

### Skill（agent 指导）

`SKILL.md` 是给 agent 的使用指导,agent 加载后会按规则:
- 回答前先检索图记忆
- 回答后提取有价值的新知识写入
- 发现过时信息时主动更新

### 使用方式

**方式 1：直接跟 agent 对话**

> "帮我看看某服务器上的推理项目"

agent 会自动调 `retrieve` 检索相关知识,拿到项目路径/端口/分支信息后回答。

**方式 2：让 agent 记住新知识**

> "记住,vLLM 0.25 新增了 speculative decoding 支持"

agent 会调 `write` 写入知识图谱,自动关联已有节点。

**方式 3：修正过时信息**

> "某服务的端口已经改了,不是 8000 了"

agent 会调 `update` 更新已有节点。

**方式 4：可视化浏览**

打开 http://127.0.0.1:9121/ 搜索、筛选、增删改查。

## 架构

```
┌──────────────────────────────────────────┐
│  Agent (Hermes / Claude Code / ...)       │
│  ┌─────────────┐  ┌──────────────────┐   │
│  │ MCP Client  │  │ Skill (指导)     │   │
│  └──────┬──────┘  └──────────────────┘   │
│         │ stdio                           │
│  ┌──────▼──────┐                          │
│  │ MCP Server  │  (轻量, 不加载模型)       │
│  │ mcp_server  │                          │
│  └──────┬──────┘                          │
└─────────┼─────────────────────────────────┘
          │ HTTP
┌─────────▼─────────────────────────────────┐
│  FastAPI Server (port 9121)              │
│  ┌───────────┐  ┌──────────┐  ┌────────┐ │
│  │ GraphEngine│  │ LLM提取  │  │ 导入器 │ │
│  │ NetworkX  │  │ OpenAI   │  │        │ │
│  │ PageRank  │  │ 兼容API  │  └────────┘ │
│  │ bge embed │  └──────────┘              │
│  └───────────┘                            │
│       │                                   │
│  ┌────▼────┐  ┌────────────┐              │
│  │ graph   │  │ embeddings │              │
│  │ .json   │  │ .npz       │              │
│  └─────────┘  └────────────┘              │
└─────────────────────────────────────────────┘
```

MCP server 是轻量 HTTP 客户端,不加载 embedding 模型。所有计算在 FastAPI server 中完成,避免两个进程各自持有引擎导致数据不一致。

## API

| 端点 | 方法 | 说明 |
|---|---|---|
| `/api/retrieve` | POST | 检索知识 (embedding + PageRank) |
| `/api/write` | POST | 写入新知识 (自动建边 + 去重) |
| `/api/update` | POST | 修正/更新已有知识 |
| `/api/extract` | POST | LLM 提炼对话→知识 |
| `/api/recent` | GET | 最近添加的节点 |
| `/api/graph` | GET | 全图数据 (可视化) |
| `/api/stats` | GET | 图统计 |
| `/api/search` | GET | 关键词搜索 |
| `/api/import` | POST | 导入外部记忆 |
| `/api/extract/sessions` | POST | 批量提炼 session |
| `/api/health` | GET | 健康检查 (Docker) |

## 评测

```bash
python benchmark.py
```

30 道题 × 3 轮 × LLM 评分,对比"只靠 MEMORY.md"vs"加图记忆"的回答质量。

> 题集需针对你自己的知识库定制(见 `benchmark.py` 顶部注释)。检索行为本身的回归用 `regression.py`(确定性快照对比,不依赖 LLM):

```bash
python regression.py snapshot baseline      # 改代码前
python regression.py snapshot after-change  # 改代码后
python regression.py compare baseline after-change
```

## 测试

```bash
pip install -e ".[test]"
pytest tests/ -q
```

引擎层测试用确定性假 embedder,不下载真实模型,离线可跑。

## 知识管理（防膨胀）

日常使用久了图会膨胀。定期运行管理工具:

```bash
python manage.py status        # 查看图健康状态
python manage.py dedup         # 扫描重复节点报告
python manage.py merge          # 合并相似节点(embedding >0.85)
python manage.py prune --dry-run   # 预览孤立+过时节点
python manage.py prune              # 执行清理
```

清理规则:
- 度 <2 且 90 天未更新的节点被删除(有关联的保留)
- 合并相似节点时保留更长/更详细的内容
- 所有操作支持 `--dry-run` 预览

## 配置项

| 环境变量 | 默认 | 说明 |
|---|---|---|
| `GM_LLM_API_KEY` | (无) | LLM API key,仅 extract 接口需要 |
| `GM_LLM_BASE_URL` | (无) | OpenAI 兼容 base url |
| `GM_LLM_MODEL` | (无) | 模型名 |
| `GM_EMBEDDING_MODEL` | `BAAI/bge-base-zh-v1.5` | 本地 embedding 模型 |
| `GM_HOST` | `127.0.0.1` | 服务监听地址 |
| `GM_PORT` | `9121` | 服务端口 |
| `HERMES_HOME` | `~/.hermes` | Hermes 记忆根目录 |
| `CLAUDE_HOME` | `~/.claude` | Claude Code 根目录 |
| `CODEX_HOME` | `~/.codex` | Codex 根目录 |

## License

MIT
