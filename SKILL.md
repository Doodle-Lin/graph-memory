---
name: graph-memory
description: 图式记忆系统 — 基于知识图谱的关联记忆检索与写入。提供检索(关键词→PageRank扩散)、写入(自动建边+embedding去重)、LLM提炼、导入(从Hermes/Claude Code/Codex记忆构建初始图谱)、可视化(Web界面)。agent回答用户后应主动提取知识写入图。
---

# Graph Memory Skill

## 系统概述

图式记忆系统,用知识图谱替代传统的"按会话分割"记忆。核心思想模仿人类海马体:
- **写入时**:新知识自动找已有知识中的关联节点,建立边(像人学习时联想)
- **检索时**:关键词找种子节点 → Personalized PageRank 沿图扩散 → 返回关联知识(像人回忆时联想)
- **去重**:三层去重(MD5完全匹配 → embedding相似度>0.85合并 → 新建)

## 启动服务

```bash
# 在项目根目录
python -m graph_memory.server
```
- API: http://127.0.0.1:9121
- 可视化: http://127.0.0.1:9121/

## ⚡ Agent 自动知识提取规则

**作为 agent,在以下场景应主动调用 Graph Memory API:**

### 1. 回答用户问题后 → 提取知识

当你在对话中产生了有价值的技术知识(解释了原理、给了方案、解决了问题),在回复用户的同时,调 `/api/extract` 提取知识:

```bash
curl -X POST http://127.0.0.1:9121/api/extract \
  -H "Content-Type: application/json" \
  -d '{"text":"你的回答中有价值的内容","source":"hermes_session:<topic>","max_links":5}'
```

**判断标准** — 以下内容应提取:
- ✅ 技术原理/概念解释(如"vLLM 的 PagedAttention 把 KV Cache 分页管理")
- ✅ 项目信息/部署方案(如"某服务部署在 GPU 服务器,端口 8000")
- ✅ 用户偏好/习惯(如"用户偏好先跑通再优化")
- ✅ 经验教训/踩坑(如"跨服务器传镜像用 save|gzip 而非 build")
- ✅ 工具/命令用法(如"ComfyUI fp8 配置: bf16+fp8_e4m3fn")
- ❌ 寒暄/确认/简单指令("好的""收到""请继续")→ 不提取

### 2. 遇到新话题 → 先检索已有知识

在开始回答用户问题前,先检索图中已有的相关知识:

```bash
curl -X POST http://127.0.0.1:9121/api/retrieve \
  -H "Content-Type: application/json" \
  -d '{"query":"用户问题的关键词","top_k":5,"spread":true}'
```

检索结果会告诉你:
- 图中已有哪些相关知识(避免重复解释)
- 通过图扩散能联想到哪些相关概念(可以补充到回答中)

### 3. 修正过时/错误记忆

当发现图中的知识过时或错误时（如端口变了、路径换了），调 `/api/update` 更新：

```bash
curl -X POST http://127.0.0.1:9121/api/update \
  -H "Content-Type: application/json" \
  -d '{"query":"某服务端口","new_content":"某服务已迁移到新端口(全量运行)","node_type":"fact"}'
```

系统会:
1. 用 query 检索最匹配的节点
2. 找到 → 更新内容(修正/更新)
3. 没找到 → 创建新节点

### 4. 无需用户指令

**不需要用户说"记一下"或"存到知识图谱"** — agent 应自动判断内容是否有价值,有则提取。去重机制会自动处理重复内容(embedding相似度>0.85的会合并)。

## API 接口

### 检索记忆
```bash
curl -X POST http://127.0.0.1:9121/api/retrieve \
  -H "Content-Type: application/json" \
  -d '{"query":"示例关键词 部署","top_k":10,"spread":true}'
```

### LLM 提炼(推荐 agent 使用)
```bash
curl -X POST http://127.0.0.1:9121/api/extract \
  -H "Content-Type: application/json" \
  -d '{"text":"对话内容...","source":"hermes_session:topic","max_links":5}'
```

### 写入新知识(直接写入,不走 LLM)
```bash
curl -X POST http://127.0.0.1:9121/api/write \
  -H "Content-Type: application/json" \
  -d '{"content":"新知识内容","title":"标题","node_type":"knowledge","source":"agent","auto_link":true,"max_links":5}'
```

### 导入 agent 记忆
```bash
curl -X POST "http://127.0.0.1:9121/api/import?source=all"
```

### 检查 LLM 配置
```bash
curl http://127.0.0.1:9121/api/llm/status
```

## 节点类型(只有 6 种)

| 类型 | 说明 | 颜色 |
|------|------|------|
| knowledge | 技术原理/概念 | 蓝 |
| preference | 用户偏好/习惯 | 绿 |
| project | 项目信息 | 橙 |
| fact | 环境/配置事实 | 红 |
| skill | 技能/工具用法 | 紫 |
| reference | 参考资料/经验教训 | 橙黄 |

## 关系类型

- `related_to`: 一般关联
- `same_topic`: 同主题
- `part_of`: 部分
- `depends_on`: 依赖
- `derived_from`: 派生

## 技术架构

- 后端: FastAPI + NetworkX + sentence-transformers(默认 BAAI/bge-base-zh-v1.5)
- 前端: Cytoscape.js(单文件 SPA,支持类型/来源筛选)
- 存储: JSON(graph.json) + NPZ(embeddings.npz)
- 检索: Embedding 语义检索 + Personalized PageRank 图扩散
- 去重: MD5 完全匹配 → embedding 相似度 > 0.85 合并 → 新建
- LLM: 配置见 .env.example(GM_LLM_*,OpenAI 兼容 API)
