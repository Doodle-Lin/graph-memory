"""Graph Memory — FastAPI 后端

提供 REST API:
  GET  /api/graph          获取完整图(前端渲染)
  GET  /api/stats          图统计
  POST /api/nodes          添加节点
  PUT  /api/nodes/{id}     更新节点
  DELETE /api/nodes/{id}   删除节点
  POST /api/edges          添加边
  DELETE /api/edges        删除边
  POST /api/retrieve       检索记忆(关键词→扩散)
  POST /api/write          Agent 写入新知识(自动建边)
  POST /api/import         导入 agent 记忆
  GET  /api/search?q=      快速搜索
"""
from __future__ import annotations

from fastapi import FastAPI, HTTPException, Query, BackgroundTasks
from fastapi.middleware.cors import CORSMiddleware
from fastapi.staticfiles import StaticFiles
from pathlib import Path

from .config import API_HOST, API_PORT
from .engine import GraphEngine
from .models import NodeCreate, NodeUpdate, EdgeCreate, MemoryQuery, MemoryWrite
from .importer import (import_all, import_hermes, import_claude_code,
                       import_codex, import_hermes_cli, import_claude_sessions)
from .llm_extract import extract_and_import, batch_extract, LLM_CONFIG

app = FastAPI(title="Graph Memory", version="0.1.0")

app.add_middleware(
    CORSMiddleware, allow_origins=["*"], allow_methods=["*"], allow_headers=["*"],
)

engine = GraphEngine()


@app.get("/api/graph")
def get_graph():
    """完整图快照"""
    return engine.snapshot()


@app.get("/api/stats")
def get_stats():
    return engine.stats()


@app.get("/api/health")
def health_check():
    """健康检查端点(Docker/k8s 用)"""
    return {"status": "ok", "nodes": engine.graph.number_of_nodes()}


@app.get("/api/recent")
def get_recent(limit: int = Query(20)):
    """最近添加的节点(按 created_at 降序)"""
    nodes = []
    for nid, attrs in engine.graph.nodes(data=True):
        nodes.append({
            "id": nid,
            "title": attrs.get("title", ""),
            "content": attrs.get("content", ""),
            "node_type": attrs.get("node_type", ""),
            "source": attrs.get("source", ""),
            "created_at": attrs.get("created_at", ""),
        })
    nodes.sort(key=lambda x: x.get("created_at", ""), reverse=True)
    return nodes[:limit]


@app.post("/api/nodes")
def add_node(body: NodeCreate):
    node = engine.add_node(
        content=body.content, title=body.title,
        node_type=body.node_type, source=body.source,
        metadata=body.metadata,
    )
    engine.flush()
    return node


@app.put("/api/nodes/{nid}")
def update_node(nid: str, body: NodeUpdate):
    try:
        node = engine.update_node(nid, content=body.content, title=body.title,
                                  node_type=body.node_type, metadata=body.metadata)
        engine.flush()
        return node
    except KeyError:
        raise HTTPException(404, f"Node {nid} not found")


@app.delete("/api/nodes/{nid}")
def delete_node(nid: str):
    if engine.delete_node(nid):
        engine.flush()
        return {"ok": True}
    raise HTTPException(404, f"Node {nid} not found")


@app.post("/api/edges")
def add_edge(body: EdgeCreate):
    try:
        return engine.add_edge(body.source, body.target, body.relation, body.weight, body.metadata)
    except KeyError as e:
        raise HTTPException(404, str(e))
    finally:
        engine.flush()


@app.delete("/api/edges")
def delete_edge(source: str = Query(...), target: str = Query(...)):
    if engine.delete_edge(source, target):
        engine.flush()
        return {"ok": True}
    raise HTTPException(404, "Edge not found")


@app.post("/api/retrieve")
def retrieve(body: MemoryQuery):
    """检索记忆: query → embedding种子 → PageRank扩散 → top-k"""
    return engine.retrieve(body.query, top_k=body.top_k,
                           spread=body.spread, spread_alpha=body.spread_depth)


@app.get("/api/search")
def search(q: str = Query(...), top_k: int = Query(10)):
    """快速搜索(GET 版检索)"""
    return engine.retrieve(q, top_k=top_k)


@app.post("/api/write")
def write_memory(body: MemoryWrite):
    """Agent 写入新知识 + 自动建边"""
    node = engine.add_node(
        content=body.content, title=body.title,
        node_type=body.node_type, source=body.source,
    )
    links = []
    if body.auto_link:
        links = engine.auto_link(node["id"], max_links=body.max_links)
    engine.flush()  # 写入即落盘,避免进程崩溃丢失
    return {"node": node, "auto_links": links}


@app.post("/api/import")
def import_memories(source: str = Query("all")):
    """导入 agent 记忆构建初始图谱"""
    if source == "all":
        result = import_all(engine)
    elif source == "hermes":
        result = import_hermes(engine)
    elif source == "claude":
        result = import_claude_code(engine)
    elif source == "codex":
        result = import_codex(engine)
    elif source == "hermes_cli":
        result = import_hermes_cli(engine)
    elif source == "claude_sessions":
        result = import_claude_sessions(engine)
    else:
        raise HTTPException(400, f"Unknown source: {source}")
    engine.flush()
    return result


# ── LLM 提炼接口 ────────────────────────────────────────────

@app.post("/api/extract")
def extract_memory(body: dict):
    """用 LLM 从一段文本中提炼知识并导入图引擎

    Body: {"text": "原始文本", "source": "标识", "max_links": 5}
    """
    text = body.get("text", "")
    source = body.get("source", "manual")
    max_links = body.get("max_links", 5)
    if not text:
        raise HTTPException(400, "text is required")
    result = extract_and_import(engine, text, source, max_links=max_links)
    engine.flush()
    return result


@app.post("/api/extract/batch")
def extract_batch(body: dict):
    """批量提炼多段文本

    Body: {"texts": [{"text": "...", "source": "..."}], "batch_size": 1, "max_links": 5}
    """
    texts = body.get("texts", [])
    batch_size = body.get("batch_size", 1)
    max_links = body.get("max_links", 5)
    if not texts:
        raise HTTPException(400, "texts is required")
    text_tuples = [(t["text"], t.get("source", "manual")) for t in texts]
    result = batch_extract(engine, text_tuples, batch_size=batch_size, max_links=max_links)
    engine.flush()
    return result


@app.get("/api/llm/status")
def llm_status():
    """检查 LLM 配置状态"""
    return {
        "configured": bool(LLM_CONFIG.get("api_key")),
        "model": LLM_CONFIG.get("model", ""),
        "base_url": LLM_CONFIG.get("base_url", ""),
    }


@app.post("/api/update")
def update_memory(body: dict):
    """修正/更新已有知识节点

    Body: {"query": "查找关键词", "new_content": "更新后的内容", "new_title": "新标题", "node_type": "knowledge"}
    """
    query = body.get("query", "")
    new_content = body.get("new_content", "")
    new_title = body.get("new_title", "")
    node_type = body.get("node_type", "")
    if not query or not new_content:
        raise HTTPException(400, "query and new_content are required")

    results = engine.retrieve(query, top_k=1)
    if results:
        nid = results[0]["id"]
        engine.update_node(nid, content=new_content,
                         title=new_title if new_title else None,
                         node_type=node_type if node_type else None)
        engine.flush()
        return {"action": "updated", "node": results[0], "message": "已更新已有节点"}
    else:
        node = engine.add_node(content=new_content, title=new_title,
                              node_type=node_type or "knowledge", source="agent:update")
        links = engine.auto_link(node["id"], max_links=5)
        engine.flush()
        return {"action": "created", "node": node, "auto_links": links,
                "message": "未找到匹配节点,已创建新节点"}


@app.post("/api/extract/sessions")
async def extract_claude_sessions(background_tasks: BackgroundTasks):
    """用 LLM 提炼 Claude Code 所有 session JSONL → 导入图引擎

    异步处理,立即返回总批次数,后台逐个提炼。
    """
    import json as _json
    import os

    claude_proj = os.path.join(os.path.expanduser("~"), ".claude", "projects")
    jsonl_files = []
    for root, dirs, files in os.walk(claude_proj):
        for f in files:
            if f.endswith(".jsonl"):
                jsonl_files.append(os.path.join(root, f))

    texts = []
    for jf in jsonl_files:
        try:
            session_id = os.path.basename(jf).replace(".jsonl", "")
            user_msgs = []
            assistant_texts = []

            with open(jf, encoding="utf-8", errors="replace") as fh:
                for line in fh:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        obj = _json.loads(line)
                    except Exception:
                        continue

                    if obj.get("type") == "user":
                        msg = obj.get("message", {})
                        if isinstance(msg, dict):
                            content = msg.get("content", "")
                            if isinstance(content, str) and len(content) > 10:
                                if not any(content.startswith(p) for p in
                                           ["Launching skill:", "Exit code", "Base directory"]):
                                    user_msgs.append(content[:2000])
                            elif isinstance(content, list):
                                for block in content:
                                    if isinstance(block, dict) and block.get("type") == "text":
                                        t = block.get("text", "")
                                        if len(t) > 10:
                                            user_msgs.append(t[:2000])
                    elif obj.get("type") == "assistant":
                        msg = obj.get("message", {})
                        if isinstance(msg, dict):
                            content = msg.get("content", [])
                            if isinstance(content, list):
                                for block in content:
                                    if isinstance(block, dict) and block.get("type") == "text":
                                        t = block.get("text", "")
                                        if len(t) > 30:
                                            assistant_texts.append(t[:3000])

            combined = "\n---\n".join(user_msgs[:5] + assistant_texts[:5])
            if combined and len(combined) > 50:
                texts.append((combined, f"claude_session:{session_id}"))
        except Exception:
            continue

    # 后台批量提炼,立即返回
    background_tasks.add_task(batch_extract, engine, texts, 1, 5)
    return {"status": "started", "total_sessions": len(texts), "message": f"后台提炼 {len(texts)} 个 session"}


@app.get("/api/node/{nid}")
def get_node(nid: str):
    node = engine.get_node(nid)
    if node is None:
        raise HTTPException(404, f"Node {nid} not found")
    return node


@app.get("/api/neighbors/{nid}")
def get_neighbors(nid: str, depth: int = Query(1)):
    """获取节点的邻居(用于前端展开)"""
    if nid not in engine.graph:
        raise HTTPException(404, f"Node {nid} not found")
    result = {"node": engine.get_node(nid), "neighbors": []}
    # BFS 找 depth 跳邻居
    visited = {nid}
    frontier = [nid]
    for d in range(depth):
        next_frontier = []
        for n in frontier:
            for nb in list(engine.graph.successors(n)) + list(engine.graph.predecessors(n)):
                if nb not in visited:
                    visited.add(nb)
                    next_frontier.append(nb)
                    node = engine.get_node(nb)
                    if node:
                        result["neighbors"].append({**node, "depth": d + 1})
        frontier = next_frontier
    return result


# 静态文件(前端)
frontend_dir = Path(__file__).resolve().parent.parent / "frontend"
if frontend_dir.exists():
    app.mount("/", StaticFiles(directory=str(frontend_dir), html=True), name="frontend")


def main():
    import uvicorn
    import signal
    import sys

    # 优雅退出: SIGTERM/SIGINT 时先 flush 再退出
    def graceful_shutdown(signum, frame):
        print("\n正在保存图记忆...")
        engine.flush()
        print("✅ 已保存,退出")
        sys.exit(0)

    signal.signal(signal.SIGINT, graceful_shutdown)
    signal.signal(signal.SIGTERM, graceful_shutdown)

    print(f"Graph Memory API → http://{API_HOST}:{API_PORT}")
    print(f"可视化界面     → http://{API_HOST}:{API_PORT}/")
    uvicorn.run(app, host=API_HOST, port=API_PORT, log_level="info")


if __name__ == "__main__":
    main()
