# P5 契约：本机离线语义召回（semantic recall）

状态：**草案 v1（lead 定稿，2026-11-05）**。本文件是本批次的**唯一权威契约**；实现、测试、脚本断言、发布物都必须与它一致。任何偏离都要先改本文件并说明理由。

## 1. 背景与目标

P4 之后字面检索（词条 + 中文 bigram）已到天花板区间：`同义改写（不照抄）` Recall@5 = 0.5333，**完全不共享字面**的改写（例如"怎么防止改了代码忘了跑测试" ↔ 记录里的"提交前必须先跑回归"）必然漏召回。目标：增加一条**本机、离线、可选**的语义通道，让这类查询能召回，同时**不破坏**既有字面契约。

成功定义：跨字面改写召回可用；没有模型时不比现在差；模型的引入不带来新的假召回类别。

## 2. 关键设计决定

### 2.1 "bounded semantic"（重新招标式语义召回）

语义相似度是**不对称**的：对"具体目标"高，对"仓库里任意一条记录"低。若把语义分数直接混进既有排序，长查询会整体抬高、偏离目标记录，破坏 AND 契约与"多主题查询必须 0 召回"。

因此规定：

1. **字面路径不变**：`match_terms` 的匹配与评分语义、排序规则、AND 契约（P0-A1）**一字不改**。
2. 语义通道**只在字面命中集合为空或不足以回答时**产出候选（下称 bounded 语义候选）：
   - `query_embedding = embed(query)`
   - 候选 = `top_k`（默认 5，上限 10）个 **scope 内且未撤回**（`include_retracted=false` 时）的记录，按 `cosine(query, memory)` 倒序；
   - 只保留 `cosine >= semantic_min_similarity`（默认 0.42，可配置）的记录；
   - `semantic` 候选**不得**混入字面命中集合的排序权重；字面结果优先，语义结果作为补充段落（`semantic.memories`）返回。
3. 每条记录参与语义比较的文本向量固定为 `title + "\n" + content + "\n" + conditions` 的拼接（空白归一化后），并由 `memory_vectors.content_sha256` 绑定；内容/标题/条件任一变化都必须重算。

### 2.2 降级必须是最小惊讶

- 模型未就绪（未下载/校验失败/加载失败/关闭）时：**行为与 P4-D 逐位一致**（指标、返回条数、排序、`semantic.available=false`、`semantic.mode="literal"`）。
- 模型下载/校验失败不得导致服务启动失败、不得让已有功能退化。
- 语义通道**不得**改变字面命中集合内的顺序；不得放行被撤回记录（`include_retracted=false` 时）；不得跨项目泄漏（`scope` 过滤先于语义比较）。

## 3. HTTP 契约

### 3.1 请求

`POST /api/v1/search` 新增可选字段（缺省 = 当前行为，向后兼容）：

| 字段 | 类型 | 默认 | 语义 |
|---|---|---|---|
| `semantic` | bool | `false` | 是否请求语义通道；模型未就绪时按 §2.2 降级 |
| `semantic_min_similarity` | number | `0.42` | 允许区间 `0.0..=1.0`，越界 → 400 |
| `semantic_limit` | integer | `5` | 允许区间 `1..=10`，越界 → 400 |

`POST /api/v1/context` 也接受 `semantic`（默认 `false`），用于"想不起来关键词时给个方向"。

### 3.2 响应

新增对象（**任何情况下都存在**，便于客户端判断）：

```json
"semantic": {
  "available": true,
  "mode": "hybrid" | "literal",
  "model": "bge-small-zh-v1.5",
  "min_similarity": 0.42,
  "memories": [ { "id": "...", "score": 0.71, ... } ],
  "note": "模型未就绪：语义通道已跳过"     // 仅降级时出现
}
```

- `available=false` 时 `memories` 必须为空数组。
- `mode="hybrid"` 表示模型就绪且本次真的跑了语义比较。
- `memories` 每项**必须**含 `id`（memory id）与 `score`（cosine，0..1）；允许附带其它 memory 字段。语义结果的分数**不得**与字面 `memories` 的分数混在同一排序里比较。
- `POST /api/v1/search` 的响应顶层必须**始终**含 `semantic` 对象（即使未开启语义），`total` 仍只统计字面结果；语义结果条数另由 `semantic.memories` 长度表达。
- MCP `memory_search` 的 arguments 直接透传，因此 `{"semantic": true}` 通过 MCP 也生效；MCP 五个工具的**集合与 schema 不变**。

### 3.3 模型状态接口

`GET /api/v1/agent/status` 增加字段：`"semantic": {"state":"absent|downloading|verifying|ready|failed","model":"bge-small-zh-v1.5","detail":"..."}`。**不得**新增需要授权的写接口；模型下载触发只允许 `POST /api/v1/models/prepare`（本机、无参数、幂等）或在桌面端触发。

**进度字段（v1.1 修订，desktop-ux 2026-11-05 提出）**：`state="downloading"`/`"verifying"` 时**可以**附带：

| 字段 | 类型 | 语义 |
|---|---|---|
| `progress` | number `0.0..=1.0` | 已完成比例；**只在能真实测量时出现**，无法测量则字段缺省 |
| `bytes_downloaded` | integer | 已下载字节数 |
| `bytes_total` | integer | 已知总字节数；未知则缺省 |

三条硬规定：(1) **数值必须来自真实测量**，禁止用时间/次数伪造百分比；(2) 字段**全部可选**，客户端必须在字段缺省时仍能正确显示（不确定进度）；(3) 下载结束（`ready`/`failed`）后这些字段必须消失或归零，不得残留误导。

## 4. 存储契约

新表（`work-memory.db`，与 `work_memories` 同库）：

```sql
CREATE TABLE IF NOT EXISTS memory_vectors (
  id TEXT PRIMARY KEY,            -- memory id
  model TEXT NOT NULL,            -- 模型标识（含版本）
  dim INTEGER NOT NULL,
  content_sha256 TEXT NOT NULL,   -- 参与向量的文本哈希
  vec BLOB NOT NULL,              -- f32 小端序紧凑存储
  updated_at TEXT NOT NULL
);
```

- 老库（无此表）必须能直接打开并工作，不做破坏性迁移。
- 备份/导出：`memory_vectors` **不进导出包**（可重建的派生数据）；`restore` 后首次检索时按需重建，且重建不得改变答案（复核者需验证"恢复后指标一致"）。
- 重建过程必须幂等：同一内容重复计算得到相同向量（同模型）。

## 5. 模型获取契约

- 位置：`%LOCALAPPDATA%\GraphMemory\models\<model-dir>\`，可用 `GM_EMBED_MODEL_DIR` 覆盖（测试用）。
- 校验：下载完成后校验 **每个文件的大小与 SHA256**（清单内置于代码/文档），校验失败 → 删除临时目录、状态 `failed`、降级，不半途可用。
- 下载：只在显式触发时发生（桌面按钮 / `POST /api/v1/models/prepare`）；**不得**在服务启动或首次检索时静默联网。
- 无网络时：状态停在 `absent`/`failed`，产品其余部分完全可用。
- 不做代理/镜像/账号要求；不使用任何密钥。

## 6. 验收门槛（硬门槛，independent 复核必须用**自建、与仓库评测集零重叠**的数据集）

| 项 | 门槛 |
|---|---|
| 跨字面语义集 Recall@5（复核者自建，≥40 条，字面命中≈0） | **≥ 0.80**，且相对同一集上的字面基线提升 **≥ 0.25 绝对** |
| 无答案误召回（semantic 打开，官方 `no_answer` + 复核者自建对抗集） | **= 0** |
| 30 条同形多主题探针（`tests/eval/false_recall_probe.json`，semantic 打开） | **0/30** |
| 跨项目污染 / 撤回泄漏 / 版本不匹配 | **0 / 0 / 0** |
| 未就绪降级：官方评测集指标 | 与 P4-D **逐位相同** |
| 检索 p95（1200 条 × 16 查询，模型已加载） | ≤ 字面基线 **+30 ms** |
| MCP | 五工具集合、`inputSchema`、`serverInfo.version` 语义不变（版本号见 §7） |
| 契约 | P0-A1 的 AND 断言（`S-A1-04`）仍 **PASS**；`context.text` 结构不变 |
| 依赖与网络 | 新增依赖必须已在离线缓存可构建；检索路径**零网络调用**（可断网复跑验证） |
| 索引重建 | 全量 1200 条记录重建向量 ≤ 60 s（本机 CPU），且幂等 |

任何一项不达标 → **不合入**。

## 7. 版本与兼容

- 版本统一升到 **0.4.0**（新增 API 字段、新表、新能力）：`Cargo.toml [package]`、csproj `<Version>`、`mcp_server serverInfo.version` 三处必须一致，`scripts/package-release.ps1` 的版本一致性检查必须通过。
- 旧客户端（不传 `semantic`）行为必须与 0.3.0 一致。

## 8. 非目标（本批次明确不做）

- 不做云端 embedding API、不做多模态、不做重排序 LLM。
- 不做向量数据库/ANN（1200 条级别暴力 cosine 足够；>10 万条再议）。
- 不改桌面端主检索交互（只在桌面端增加"启用语义检索"的模型获取/状态入口）。
- 不自动下载模型、不静默联网。
