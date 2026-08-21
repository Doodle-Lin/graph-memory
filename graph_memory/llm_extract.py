"""Graph Memory — LLM 提炼器

用 LLM 从原始对话/记忆中提炼知识节点和关系,替代机械规则过滤。

工作流:
  原始文本批次 → LLM 提炼 → JSON [{nodes: [...], edges: [...]}] → 写入图引擎

与直接调 CLI 相比:
  - 批量并发,速度快(10 个 session 一批)
  - 输出格式精确可控(JSON schema)
  - 不浪费 token 在工具定义/系统 prompt 上
"""
from __future__ import annotations

import json
import os
import re
import time
from typing import Any

import requests

from .engine import GraphEngine


# ── 配置 ────────────────────────────────────────────────────

def _load_llm_config() -> dict:
    """加载 LLM 配置: 仅从环境变量读取,不耦合任何特定 Agent 的本地配置文件。

    开源版只认 GM_LLM_* 三个环境变量(见 .env.example)。
    不设置则返回空 dict,extract 接口会返回 {"error": "no_api_key"} 而不影响检索/写入。
    """
    from .config import LLM_API_KEY, LLM_BASE_URL, LLM_MODEL
    if LLM_API_KEY and LLM_BASE_URL and LLM_MODEL:
        return {"api_key": LLM_API_KEY, "base_url": LLM_BASE_URL, "model": LLM_MODEL}
    return {}


LLM_CONFIG = _load_llm_config()

# 提炼 prompt — 要求 LLM 输出结构化 JSON
EXTRACT_PROMPT = """你是一个知识提炼专家。从下面的对话/记忆文本中提取有价值的知识节点和它们之间的关系。

## 提取规则

1. **只提取有信息量的知识** — 跳过寒暄("在吗""好的""收到")、命令("/model""/clear")、碎片("嗯""跳过")
2. **每条知识应能独立成立** — 不依赖对话上下文也能理解
3. **合并重复信息** — 同一个知识点在对话中反复出现,只提取一次
4. **保留技术细节** — 服务器地址、端口号、命令、路径、配置参数都是有价值的
5. **标注节点类型**(只能从以下 6 种中选择):
   - knowledge: 技术原理/概念(如"vLLM 用 PagedAttention 分页管理 KV Cache 提高 GPU 利用率")
   - preference: 用户偏好/习惯(如"用户偏好先跑通再优化")
   - project: 项目信息(如"某推理服务部署在 GPU 服务器,端口 8000")
   - fact: 环境/配置事实(如"内网无法访问 GitHub,改用镜像源")
   - skill: 技能/工具用法(如"ComfyUI fp8 运行时配置 bf16+fp8_e4m3fn")
   - reference: 参考资料/路径/经验教训(如"跨服务器传输镜像用 save|gzip 而非 build")

   注意:不要使用 session/history/feedback/user 等类型,只有上述 6 种。

6. **标注关系** — 如果两个节点有明显关联,标注 relation:
   - related_to: 一般关联
   - depends_on: 依赖
   - part_of: 部分
   - derived_from: 派生
   - same_topic: 同主题

## 输出格式(严格 JSON,不要 markdown 代码块)

{{
  "nodes": [
    {{"title": "简短标题", "content": "完整知识内容,保留所有技术细节(地址/端口/命令/参数/路径),不要省略", "type": "knowledge"}}
  ],
  "edges": [
    {{"source_title": "节点A标题", "target_title": "节点B标题", "relation": "related_to"}}
  ]
}}

如果文本没有有价值的知识,返回 {{"nodes": [], "edges": []}}

## 待提炼文本

来源: {source}
"""


def _call_llm(text: str, source: str, config: dict = None) -> dict:
    """调用 LLM 提炼知识,返回 {nodes, edges}"""
    cfg = config or LLM_CONFIG
    if not cfg.get("api_key"):
        return {"nodes": [], "edges": [], "error": "no_api_key"}

    prompt = EXTRACT_PROMPT.format(source=source) + "\n" + text[:8000]

    try:
        r = requests.post(
            f"{cfg['base_url']}/chat/completions",
            headers={
                "Authorization": f"Bearer {cfg['api_key']}",
                "Content-Type": "application/json",
            },
            json={
                "model": cfg["model"],
                "messages": [
                    {"role": "system", "content": "你是知识提炼专家,只输出JSON。"},
                    {"role": "user", "content": prompt},
                ],
                "max_tokens": 4096,
                "temperature": 0.3,
            },
            timeout=60,
        )
        r.raise_for_status()
        content = r.json()["choices"][0]["message"]["content"]

        # 某些推理模型会把正文留在 reasoning_content 而非 content,
        # 这里做兜底:content 为空时改读 reasoning_content(OpenAI 兼容字段)
        if not content:
            content = r.json()["choices"][0]["message"].get("reasoning_content", "")
        if not content:
            return {"nodes": [], "edges": [], "error": "empty_response"}

        # 去掉可能的 ```json ... ``` 包裹
        content = re.sub(r"```json\s*", "", content)
        content = re.sub(r"```\s*$", "", content)
        content = content.strip()

        # 找到第一个 { 和最后一个 }
        start = content.find("{")
        end = content.rfind("}")
        if start == -1 or end == -1:
            return {"nodes": [], "edges": [], "error": "no_json_in_response",
                    "raw": content[:200]}

        json_str = content[start:end + 1]
        # 修复 Python dict 单引号(如果 LLM 返回了 {'key': 'value'} 格式)
        if "'" in json_str and '"' not in json_str:
            json_str = json_str.replace("'", '"')

        result = json.loads(json_str)
        return result

    except Exception as e:
        return {"nodes": [], "edges": [], "error": str(e)}


def extract_and_import(engine: GraphEngine, text: str, source: str,
                        max_links: int = 5) -> dict:
    """提炼一段文本并导入图引擎

    Args:
        engine: 图引擎实例
        text: 原始文本(对话/记忆)
        source: 来源标识(如 "claude_session:abc123")
        max_links: 自动建边数量上限

    Returns:
        {"extracted": {nodes, edges}, "imported": {nodes_created, edges_created}}
    """
    # 1. LLM 提炼
    extracted = _call_llm(text, source)
    if extracted.get("error"):
        return {"extracted": extracted, "imported": {"nodes_created": 0, "edges_created": 0}}

    # 2. 写入节点
    title_to_id = {}
    nodes_created = 0
    edges_created = 0

    for node_data in extracted.get("nodes", []):
        title = node_data.get("title", "")
        content = node_data.get("content", "")
        ntype = node_data.get("type", "knowledge")

        if not content or len(content) < 10:
            continue

        node = engine.add_node(
            content=content,
            title=title,
            node_type=ntype,
            source=f"llm_extract:{source}",
        )
        if node:
            title_to_id[title] = node["id"]
            # 自动建边(语义相似度)
            links = engine.auto_link(node["id"], max_links=max_links)
            nodes_created += 1
            edges_created += len(links)

    # 3. 写入 LLM 标注的关系边
    for edge_data in extracted.get("edges", []):
        src_title = edge_data.get("source_title", "")
        tgt_title = edge_data.get("target_title", "")
        relation = edge_data.get("relation", "related_to")

        src_id = title_to_id.get(src_title)
        tgt_id = title_to_id.get(tgt_title)

        if src_id and tgt_id and not engine.graph.has_edge(src_id, tgt_id):
            engine.add_edge(src_id, tgt_id, relation=relation, weight=0.8)
            edges_created += 1

    engine.flush()
    return {
        "extracted": extracted,
        "imported": {"nodes_created": nodes_created, "edges_created": edges_created},
    }


def batch_extract(engine: GraphEngine, texts: list[tuple[str, str]],
                  batch_size: int = 1, max_links: int = 5) -> dict:
    """批量提炼多段文本

    Args:
        texts: [(text, source), ...]
        batch_size: 每批合并几段文本送给 LLM
        max_links: 每个节点自动建边上限

    Returns:
        {"total_nodes": N, "total_edges": M, "batches": N, "errors": [...]}
    """
    total_nodes = 0
    total_edges = 0
    errors = []
    batch_count = 0

    # 合并批次
    batches = []
    for i in range(0, len(texts), batch_size):
        batch_texts = texts[i:i + batch_size]
        combined = "\n\n---\n\n".join(t for t, _ in batch_texts)
        sources = ", ".join(s for _, s in batch_texts)
        batches.append((combined, sources))

    for text, source in batches:
        batch_count += 1
        result = extract_and_import(engine, text, source, max_links=max_links)
        imported = result.get("imported", {})
        total_nodes += imported.get("nodes_created", 0)
        total_edges += imported.get("edges_created", 0)

        if result.get("extracted", {}).get("error"):
            errors.append(result["extracted"]["error"])

        # 速率控制:避免 API 限流
        time.sleep(0.5)

    return {
        "total_nodes": total_nodes,
        "total_edges": total_edges,
        "batches": batch_count,
        "errors": errors,
    }
