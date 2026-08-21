"""Graph Memory MCP Server

作为 Hermes MCP server 运行,提供两个工具:
  - mcp_graph_memory_retrieve: 检索知识(关键词→PageRank扩散)
  - mcp_graph_memory_write: 写入新知识(自动建边+去重)

Hermes config.yaml 配置:
  mcp_servers:
    graph-memory:
      command: "python"
      args: ["-m", "graph_memory.mcp_server"]
      env:
        GM_PORT: "9121"

启动后 agent 自动获得这两个工具,无需手动调 HTTP API。
"""
from __future__ import annotations

import json
import sys
import os

# 确保能 import graph_memory 包
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from mcp.server import Server
from mcp.server.stdio import stdio_server
from mcp.types import Tool, TextContent

# MCP server 通过 HTTP API 调用后端,不自己加载引擎
# 这样避免两个进程各自持有 GraphEngine 导致数据不一致
API_BASE = os.environ.get("GM_API_URL", "http://127.0.0.1:9121")

server = Server("graph-memory")


@server.list_tools()
async def list_tools() -> list[Tool]:
    return [
        Tool(
            name="retrieve",
            description="检索知识图谱中的关联知识。输入关键词或问题,返回通过 embedding 语义匹配 + Personalized PageRank 图扩散找到的关联知识节点。能跨概念联想——从一个技术点沿图扩散到相关的部署细节、经验教训或用户偏好。",
            inputSchema={
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "检索关键词或问题"
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "返回结果数(默认5)",
                        "default": 5
                    }
                },
                "required": ["query"]
            }
        ),
        Tool(
            name="write",
            description="将新知识写入知识图谱。自动找已有知识中的关联节点并建边(像人学习时联想)。三层去重:完全相同→合并,embedding相似度>0.85→合并,否则新建。",
            inputSchema={
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "知识内容(保留技术细节:地址/端口/命令/参数)"
                    },
                    "title": {
                        "type": "string",
                        "description": "简短标题(可选)"
                    },
                    "node_type": {
                        "type": "string",
                        "description": "节点类型: knowledge/preference/project/fact/skill/reference",
                        "default": "knowledge"
                    },
                    "source": {
                        "type": "string",
                        "description": "来源标识(如 hermes_session:topic)",
                        "default": "agent"
                    }
                },
                "required": ["content"]
            }
        ),
        Tool(
            name="extract",
            description="用 LLM 从一段对话/文本中提炼知识并写入图。自动过滤噪声,提取结构化知识节点+关系,适合处理对话记录。",
            inputSchema={
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "待提炼的文本(对话/记忆)"
                    },
                    "source": {
                        "type": "string",
                        "description": "来源标识",
                        "default": "agent"
                    }
                },
                "required": ["text"]
            }
        ),
        Tool(
            name="recent",
            description="获取最近添加的知识节点。不传参数时返回最近20条。",
            inputSchema={
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "返回条数(默认20)",
                        "default": 20
                    }
                }
            }
        ),
        Tool(
            name="update",
            description="修正/更新已有知识节点。当发现图中的知识过时或错误时使用。通过关键词找到匹配节点,更新其内容。如果找不到匹配节点,则创建新节点。",
            inputSchema={
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "用来查找要更新的节点的关键词"
                    },
                    "new_content": {
                        "type": "string",
                        "description": "更新后的完整内容(保留所有技术细节)"
                    },
                    "new_title": {
                        "type": "string",
                        "description": "更新后的标题(可选)"
                    },
                    "node_type": {
                        "type": "string",
                        "description": "节点类型(可选): knowledge/preference/project/fact/skill/reference",
                    }
                },
                "required": ["query", "new_content"]
            }
        ),
    ]


@server.call_tool()
async def call_tool(name: str, arguments: dict) -> list[TextContent]:
    import urllib.request
    import json as _json

    def api_post(path, body):
        data = _json.dumps(body).encode("utf-8")
        req = urllib.request.Request(f"{API_BASE}{path}", data=data, method="POST",
                                    headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=30) as resp:
            return _json.loads(resp.read().decode("utf-8"))

    def api_get(path):
        req = urllib.request.Request(f"{API_BASE}{path}")
        with urllib.request.urlopen(req, timeout=10) as resp:
            return _json.loads(resp.read().decode("utf-8"))

    if name == "retrieve":
        query = arguments.get("query", "")
        top_k = arguments.get("top_k", 5)
        result = api_post("/api/retrieve", {"query": query, "top_k": top_k, "spread": True})
        lines = [f"检索 '{query}' 返回 {len(result)} 条关联知识:\n"]
        for i, r in enumerate(result):
            lines.append(f"--- {i+1} ---")
            lines.append(f"标题: {r.get('title', '?')}")
            lines.append(f"类型: {r.get('node_type', '?')}  分数: {r.get('score', 0):.3f}")
            lines.append(f"内容: {r.get('content', '')}")
            lines.append("")
        return [TextContent(type="text", text="\n".join(lines))]

    elif name == "write":
        content = arguments.get("content", "")
        if not content:
            return [TextContent(type="text", text="错误: content 不能为空")]
        result = api_post("/api/write", {
            "content": content,
            "title": arguments.get("title", ""),
            "node_type": arguments.get("node_type", "knowledge"),
            "source": arguments.get("source", "agent"),
            "auto_link": True,
            "max_links": 5,
        })
        return [TextContent(type="text",
            text=f"已写入知识节点: {result.get('node', {}).get('title', '?')}\n自动建立 {len(result.get('auto_links', []))} 条关联")]

    elif name == "extract":
        text = arguments.get("text", "")
        if not text:
            return [TextContent(type="text", text="错误: text 不能为空")]
        result = api_post("/api/extract", {"text": text, "source": arguments.get("source", "agent")})
        return [TextContent(type="text",
            text=f"LLM 提炼完成: {result.get('total_nodes', 0)} 个知识节点, {result.get('total_edges', 0)} 条关系")]

    elif name == "recent":
        limit = arguments.get("limit", 20)
        result = api_get(f"/api/recent?limit={limit}")
        lines = [f"最近 {len(result)} 条知识:\n"]
        for n in result:
            lines.append(f"[{n.get('node_type', '?')}] {n.get('title', '?')}")
            lines.append(f"  {n.get('content', '')[:100]}")
            lines.append("")
        return [TextContent(type="text", text="\n".join(lines))]

    elif name == "update":
        query = arguments.get("query", "")
        new_content = arguments.get("new_content", "")
        if not query or not new_content:
            return [TextContent(type="text", text="错误: query 和 new_content 不能为空")]
        result = api_post("/api/update", {
            "query": query,
            "new_content": new_content,
            "new_title": arguments.get("new_title", ""),
            "node_type": arguments.get("node_type", ""),
        })
        return [TextContent(type="text",
            text=f"已{result.get('action', '?')}知识节点: {result.get('message', '')}")]

    return [TextContent(type="text", text=f"未知工具: {name}")]


def main():
    import asyncio

    async def run():
        print("Graph Memory MCP Server starting...", file=sys.stderr)
        async with stdio_server() as (read, write):
            await server.run(read, write, server.create_initialization_options())

    asyncio.run(run())


if __name__ == "__main__":
    main()
