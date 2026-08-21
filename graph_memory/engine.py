"""Graph Memory — 图引擎核心

NetworkX 有向图 + Embedding 语义检索 + Personalized PageRank 关联扩散

核心循环:
  写入: content → LLM/embedding 抽取实体 → 找已有图中的关联节点 → 建边
  读取: query → embedding 找种子 → PageRank 从种子扩散 → top-k 关联节点
"""
from __future__ import annotations

import hashlib
import json
import threading
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

import numpy as np
import networkx as nx

from .config import (
    GRAPH_FILE, GRAPH_DB, EMBEDDINGS_FILE, EMBEDDING_MODEL,
    PAGERANK_ALPHA, PAGERANK_TOL, RETRIEVAL_TOP_K, SEED_TOP_K, MIN_SIM_THRESHOLD,
)


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


def _node_id(content: str) -> str:
    """根据内容生成稳定 ID"""
    return hashlib.md5(content.encode()).hexdigest()[:12]


class GraphEngine:
    """图式记忆引擎

    单例,持有 NetworkX 图和 embedding 模型,负责所有 CRUD + 检索。
    """

    def __init__(self):
        self.graph = nx.DiGraph()
        self._embedder = None        # lazy load
        self._embeddings: dict[str, np.ndarray] = {}
        self._emb_matrix: np.ndarray | None = None   # 缓存矩阵
        self._emb_ids: list[str] | None = None       # 缓存 ID 顺序
        self._emb_dirty = True                        # 缓存是否过期
        self._save_dirty = False                      # 是否有待保存的变更
        # 并发模型说明(见 CONTRIBUTING.md):
        # 当前部署假定单进程 GraphEngine + FastAPI 同步路由跑在线程池里。
        # 同一进程内并发写会竞争 _embeddings / graph / emb 缓存,故加锁保护写路径。
        # 跨进程(多个 server 实例指向同一 data/)未做协调——不要那样部署,
        # 走 MCP 时 mcp_server 只做 HTTP 代理,不持有引擎。
        self._lock = threading.Lock()
        self._load()

    # ── Embedding ──────────────────────────────────────────────

    @property
    def embedder(self):
        if self._embedder is None:
            from sentence_transformers import SentenceTransformer
            self._embedder = SentenceTransformer(EMBEDDING_MODEL)
        return self._embedder

    def _embed(self, text: str) -> np.ndarray:
        return self.embedder.encode(text, normalize_embeddings=True)

    def _embed_batch(self, texts: list[str]) -> np.ndarray:
        return self.embedder.encode(texts, normalize_embeddings=True)

    def _get_emb_matrix(self) -> tuple[np.ndarray, list[str]]:
        """获取 embedding 矩阵(带缓存),返回 (matrix, ids)"""
        if self._emb_dirty or self._emb_matrix is None or self._emb_ids is None:
            self._emb_ids = list(self._embeddings.keys())
            if self._emb_ids:
                self._emb_matrix = np.array([self._embeddings[k] for k in self._emb_ids])
            else:
                self._emb_matrix = np.empty((0, self.embedder.get_sentence_embedding_dimension()))
            self._emb_dirty = False
        return self._emb_matrix, self._emb_ids

    def _invalidate_emb_cache(self):
        """embedding 变更时调用"""
        self._emb_dirty = True

    # ── 持久化 ────────────────────────────────────────────────

    def _init_db(self):
        """初始化 SQLite 表结构"""
        import sqlite3
        self._db = sqlite3.connect(str(GRAPH_DB), check_same_thread=False)
        self._db.row_factory = sqlite3.Row
        self._db.execute("PRAGMA journal_mode=WAL")  # 并发读写
        self._db.execute("""CREATE TABLE IF NOT EXISTS nodes (
            id TEXT PRIMARY KEY,
            content TEXT NOT NULL,
            title TEXT DEFAULT '',
            node_type TEXT DEFAULT 'knowledge',
            source TEXT DEFAULT 'manual',
            metadata TEXT DEFAULT '{}',
            created_at TEXT,
            updated_at TEXT
        )""")
        self._db.execute("""CREATE TABLE IF NOT EXISTS edges (
            source TEXT NOT NULL,
            target TEXT NOT NULL,
            relation TEXT DEFAULT 'related_to',
            weight REAL DEFAULT 1.0,
            metadata TEXT DEFAULT '{}',
            created_at TEXT,
            PRIMARY KEY (source, target)
        )""")
        # 索引加速查询
        self._db.execute("CREATE INDEX IF NOT EXISTS idx_node_type ON nodes(node_type)")
        self._db.execute("CREATE INDEX IF NOT EXISTS idx_node_source ON nodes(source)")
        self._db.execute("CREATE INDEX IF NOT EXISTS idx_node_created ON nodes(created_at)")
        self._db.commit()

    def _migrate_from_json(self):
        """从旧 graph.json 迁移到 SQLite(一次性)"""
        if not GRAPH_FILE.exists():
            return
        import json as _json
        print("检测到旧 graph.json,迁移到 SQLite...")
        data = _json.loads(GRAPH_FILE.read_text(encoding="utf-8"))
        nodes = data.get("nodes", {})
        edges = data.get("edges", [])
        for nid, attrs in nodes.items():
            self._db.execute(
                "INSERT OR REPLACE INTO nodes VALUES (?,?,?,?,?,?,?,?)",
                (nid, attrs.get("content",""), attrs.get("title",""),
                 attrs.get("node_type","knowledge"), attrs.get("source","manual"),
                 _json.dumps(attrs.get("metadata",{}), ensure_ascii=False),
                 attrs.get("created_at",""), attrs.get("updated_at",""))
            )
        for e in edges:
            self._db.execute(
                "INSERT OR REPLACE INTO edges VALUES (?,?,?,?,?,?)",
                (e["source"], e["target"], e.get("relation","related_to"),
                 e.get("weight",1.0),
                 _json.dumps(e.get("metadata",{}), ensure_ascii=False),
                 e.get("created_at",""))
            )
        self._db.commit()
        # 重命名旧文件(不删,留备份)
        GRAPH_FILE.rename(GRAPH_FILE.with_suffix(".json.bak"))
        print(f"✅ 迁移完成: {len(nodes)} 节点, {len(edges)} 边")

    def _load(self):
        self._init_db()
        self._migrate_from_json()

        # 从 SQLite 加载到 NetworkX 内存图
        for row in self._db.execute("SELECT * FROM nodes"):
            import json as _json
            attrs = dict(row)
            attrs["metadata"] = _json.loads(attrs.get("metadata", "{}"))
            self.graph.add_node(row["id"], **attrs)

        for row in self._db.execute("SELECT * FROM edges"):
            import json as _json
            attrs = {"relation": row["relation"], "weight": row["weight"],
                     "metadata": _json.loads(row["metadata"] or "{}"),
                     "created_at": row["created_at"]}
            self.graph.add_edge(row["source"], row["target"], **attrs)

        print(f"加载 {self.graph.number_of_nodes()} 节点, {self.graph.number_of_edges()} 边")

        # 加载 embedding 缓存
        emb_ok = False
        if EMBEDDINGS_FILE.exists():
            try:
                arch = np.load(EMBEDDINGS_FILE, allow_pickle=True)
                ids = arch["ids"].tolist()
                vecs = arch["vectors"]
                self._embeddings = {i: v for i, v in zip(ids, vecs)}
                if vecs.shape[1] != self.embedder.get_sentence_embedding_dimension():
                    print(f"⚠ Embedding 维度不匹配(旧={vecs.shape[1]}, 新={self.embedder.get_sentence_embedding_dimension()}),重建...")
                    self._embeddings = {}
                else:
                    emb_ok = True
            except Exception as e:
                print(f"⚠ Embedding 加载失败: {e},重建...")
                self._embeddings = {}

        if not emb_ok and self.graph.number_of_nodes() > 0:
            self._rebuild_embeddings()

    def _rebuild_embeddings(self):
        """为图中所有节点重新计算 embedding(批量)"""
        n = self.graph.number_of_nodes()
        print(f"重建 {n} 个节点的 embedding...")
        contents = []
        nids = []
        for nid, attrs in self.graph.nodes(data=True):
            contents.append(attrs.get("content", attrs.get("title", "")))
            nids.append(nid)
        # 批量编码
        vecs = self._embed_batch(contents)
        for nid, vec in zip(nids, vecs):
            self._embeddings[nid] = vec
        self._invalidate_emb_cache()
        self.save()
        print(f"✅ Embedding 重建完成 ({n} 个节点)")

    def save(self):
        """增量写入 SQLite(只写变化的节点/边)"""
        import json as _json

        # 找出内存图中有但 SQLite 中没有/变化的节点
        db_ids = {r[0] for r in self._db.execute("SELECT id FROM nodes")}
        for nid, attrs in self.graph.nodes(data=True):
            metadata = attrs.get("metadata", {})
            if isinstance(metadata, str):
                try: metadata = _json.loads(metadata)
                except: pass
            self._db.execute(
                "INSERT OR REPLACE INTO nodes VALUES (?,?,?,?,?,?,?,?)",
                (nid, attrs.get("content",""), attrs.get("title",""),
                 attrs.get("node_type","knowledge"), attrs.get("source","manual"),
                 _json.dumps(metadata, ensure_ascii=False),
                 attrs.get("created_at",""), attrs.get("updated_at",""))
            )
        # 删除 SQLite 中有但内存图中已删除的节点
        mem_ids = set(self.graph.nodes())
        for old_id in db_ids - mem_ids:
            self._db.execute("DELETE FROM nodes WHERE id=?", (old_id,))
            self._db.execute("DELETE FROM edges WHERE source=? OR target=?", (old_id, old_id))

        # 同步边
        for s, t, attrs in self.graph.edges(data=True):
            metadata = attrs.get("metadata", {})
            if isinstance(metadata, str):
                try: metadata = _json.loads(metadata)
                except: pass
            self._db.execute(
                "INSERT OR REPLACE INTO edges VALUES (?,?,?,?,?,?)",
                (s, t, attrs.get("relation","related_to"),
                 attrs.get("weight",1.0),
                 _json.dumps(metadata, ensure_ascii=False),
                 attrs.get("created_at",""))
            )
        # 删除已不存在的边
        db_edges = {(r[0], r[1]) for r in self._db.execute("SELECT source, target FROM edges")}
        mem_edges = set(self.graph.edges())
        for s, t in db_edges - mem_edges:
            self._db.execute("DELETE FROM edges WHERE source=? AND target=?", (s, t))

        self._db.commit()

        # 向量仍然用 npz(比 SQLite BLOB 更快)
        if self._embeddings:
            ids = list(self._embeddings.keys())
            vecs = np.array([self._embeddings[i] for i in ids])
            np.savez(EMBEDDINGS_FILE, ids=np.array(ids), vectors=vecs)

        self._save_dirty = False

    def flush(self):
        """如果有待保存的变更,执行保存"""
        if self._save_dirty:
            self.save()

    # ── 节点 CRUD ─────────────────────────────────────────────

    # 去重阈值: embedding 余弦相似度超过此值认为是同一知识
    DEDUP_THRESHOLD = 0.85

    # 合法的 6 种节点类型(importer 历史上有写入 history/session/memory 等非法类型,在此兜底)
    VALID_NODE_TYPES = {"knowledge", "preference", "project", "fact", "skill", "reference"}

    def _normalize_type(self, node_type: str) -> str:
        """非法类型回退到 knowledge,保留原类型到 metadata 供追溯"""
        if node_type in self.VALID_NODE_TYPES:
            return node_type
        return "knowledge"

    def add_node(self, content: str, title: str = "",
                 node_type: str = "knowledge", source: str = "manual",
                 metadata: dict = None) -> dict:
        """添加知识节点,返回节点信息

        去重策略(三层):
        1. 内容完全相同(MD5 hash) → 合并 metadata,返回已有节点
        2. embedding 相似度 > 0.85 → 合并到已有节点(更新来源标记)
        3. 都不匹配 → 新建节点
        """
        with self._lock:
            # 类型规范化:非法类型 → knowledge,原类型记入 metadata
            original_type = node_type
            node_type = self._normalize_type(node_type)
            if original_type != node_type and metadata is None:
                metadata = {}
            if original_type != node_type:
                metadata = dict(metadata or {})
                metadata.setdefault("original_type", original_type)

            # 第一层: MD5 完全匹配
            nid = _node_id(content)
            if nid in self.graph:
                self.graph.nodes[nid]["updated_at"] = _now()
                if metadata:
                    self.graph.nodes[nid].setdefault("metadata", {}).update(metadata)
                return self.get_node(nid)

            # 第二层: embedding 相似度去重(使用缓存矩阵)
            new_emb = self._embed(content)
            if self._embeddings:
                mat, ids = self._get_emb_matrix()
                sims = mat @ new_emb
                best_idx = int(np.argmax(sims))
                best_sim = float(sims[best_idx])
                if best_sim >= self.DEDUP_THRESHOLD:
                    existing_id = ids[best_idx]
                    existing = self.graph.nodes[existing_id]
                    existing["updated_at"] = _now()
                    existing.setdefault("metadata", {}).setdefault("merged_from", [])
                    if source not in existing["metadata"]["merged_from"]:
                        existing["metadata"]["merged_from"].append(source)
                    if metadata:
                        existing["metadata"].update(metadata)
                    self._save_dirty = True
                    return self.get_node(existing_id)

            # 第三层: 新建节点
            title = title or (content[:40] + "..." if len(content) > 40 else content)
            self.graph.add_node(nid,
                                content=content,
                                title=title,
                                node_type=node_type,
                                source=source,
                                metadata=metadata or {},
                                created_at=_now(),
                                updated_at=_now())
            self._embeddings[nid] = new_emb
            self._invalidate_emb_cache()
            self._save_dirty = True
            return self.get_node(nid)

    def update_node(self, nid: str, content: str = None, title: str = None,
                    node_type: str = None, metadata: dict = None) -> dict:
        with self._lock:
            if nid not in self.graph:
                raise KeyError(f"Node {nid} not found")
            node = self.graph.nodes[nid]
            if content is not None:
                node["content"] = content
                self._embeddings[nid] = self._embed(content)
                self._invalidate_emb_cache()
            if title is not None:
                node["title"] = title
            if node_type is not None:
                node["node_type"] = self._normalize_type(node_type)
            if metadata is not None:
                node.setdefault("metadata", {}).update(metadata)
            node["updated_at"] = _now()
            self._save_dirty = True
            return self.get_node(nid)

    def delete_node(self, nid: str) -> bool:
        with self._lock:
            if nid not in self.graph:
                return False
            self.graph.remove_node(nid)
            self._embeddings.pop(nid, None)
            self._invalidate_emb_cache()
            self._save_dirty = True
            return True

    def get_node(self, nid: str) -> dict:
        if nid not in self.graph:
            return None
        attrs = dict(self.graph.nodes[nid])
        attrs["id"] = nid
        # 邻居信息
        attrs["outgoing"] = list(self.graph.successors(nid))
        attrs["incoming"] = list(self.graph.predecessors(nid))
        return attrs

    # ── 边 CRUD ───────────────────────────────────────────────

    def add_edge(self, source: str, target: str, relation: str = "related_to",
                 weight: float = 1.0, metadata: dict = None) -> dict:
        with self._lock:
            if source not in self.graph or target not in self.graph:
                raise KeyError(f"Node(s) not found: {source} / {target}")
            self.graph.add_edge(source, target,
                                relation=relation,
                                weight=weight,
                                metadata=metadata or {},
                                created_at=_now())
            self._save_dirty = True
            return {"source": source, "target": target, "relation": relation, "weight": weight}

    def delete_edge(self, source: str, target: str) -> bool:
        with self._lock:
            if not self.graph.has_edge(source, target):
                return False
            self.graph.remove_edge(source, target)
            self._save_dirty = True
            return True

    # ── 自动建边(核心创新) ────────────────────────────────────

    def auto_link(self, nid: str, max_links: int = 5) -> list[dict]:
        """为新节点自动找关联并建边

        1. 用 embedding 找语义相似的已有节点
        2. 如果相似度超过阈值,建边
        """
        with self._lock:
            if nid not in self.graph or len(self._embeddings) <= 1:
                return []

            # 使用缓存矩阵(排除自己)
            mat, all_ids = self._get_emb_matrix()
            mask = [i for i, id_ in enumerate(all_ids) if id_ != nid]
            if not mask:
                return []

            other_ids = [all_ids[i] for i in mask]
            other_vecs = mat[mask]
            query_vec = self._embeddings[nid]

            # 余弦相似度(embedding 已归一化)
            sims = other_vecs @ query_vec
            ranked = np.argsort(sims)[::-1][:max_links]

            created = []
            for idx in ranked:
                sim = float(sims[idx])
                if sim < 0.3:  # 阈值:太低不建边
                    continue
                target_id = other_ids[idx]
                if self.graph.has_edge(nid, target_id):
                    continue
                relation = "related_to"
                if sim > 0.7:
                    relation = "strongly_related"
                self.graph.add_edge(nid, target_id,
                                    relation=relation,
                                    weight=sim,
                                    metadata={"auto_linked": True, "similarity": sim},
                                    created_at=_now())
                created.append({"source": nid, "target": target_id,
                                "relation": relation, "weight": round(sim, 3)})
            self._save_dirty = True
            return created

    # ── 检索(关键词 → 种子 → PageRank 扩散) ───────────────────

    def retrieve(self, query: str, top_k: int = RETRIEVAL_TOP_K,
                 spread: bool = True, spread_alpha: float = PAGERANK_ALPHA) -> list[dict]:
        """检索:embedding 找种子 → PageRank 扩散 → top-k 关联节点"""
        if not self._embeddings:
            return []

        query_emb = self._embed(query)
        mat, other_ids = self._get_emb_matrix()
        if len(other_ids) == 0:
            return []
        sims = mat @ query_emb

        # 种子节点 top-3
        seed_k = min(SEED_TOP_K, len(other_ids))
        seed_indices = np.argsort(sims)[::-1][:seed_k]
        seeds = {}
        for idx in seed_indices:
            seeds[other_ids[idx]] = float(sims[idx])

        if not spread or len(self.graph) <= seed_k:
            # 不扩散,直接返回语义最相似的(也要过滤低相似度)
            ranked = np.argsort(sims)[::-1][:top_k]
            results = []
            for idx in ranked:
                if float(sims[idx]) < MIN_SIM_THRESHOLD:
                    continue
                nid = other_ids[idx]
                node = self.get_node(nid)
                if node:
                    node["score"] = float(sims[idx])
                    node["match_type"] = "semantic"
                    results.append(node)
            return results

        # Personalized PageRank 从种子扩散
        personalization = {n: 0.0 for n in self.graph.nodes}
        for sid, weight in seeds.items():
            personalization[sid] = weight
        # 归一化
        total = sum(personalization.values())
        if total > 0:
            personalization = {k: v / total for k, v in personalization.items()}

        pr = nx.pagerank(self.graph, alpha=spread_alpha,
                         personalization=personalization,
                         tol=PAGERANK_TOL, max_iter=200, weight="weight")

        # 合并语义分数和 PageRank 分数
        # 归一化:用 sum 而非 max,避免单个高 PR 节点压缩其他分数
        pr_sum = sum(pr.values()) if pr else 1
        sem_max = max(sims) if len(sims) else 1

        scored = []
        for idx, nid in enumerate(other_ids):
            sem_score = float(sims[idx]) / max(sem_max, 1e-8)
            pr_score = pr.get(nid, 0) / max(pr_sum, 1e-8)
            # 加权融合:50% 语义 + 50% 图扩散
            fused = 0.5 * sem_score + 0.5 * pr_score
            scored.append((nid, fused, sem_score, pr_score))

        scored.sort(key=lambda x: -x[1])
        results = []
        # 类型优先级: knowledge > project > fact > reference > preference > skill
        # 让有实质内容的知识节点排在 skill 描述前面
        type_priority = {"knowledge": 0, "project": 1, "fact": 2,
                         "reference": 3, "preference": 4, "skill": 5}
        for nid, fused, sem, pr_s in scored[:top_k * 2]:  # 多取一倍用于重排
            # 过滤: 语义相似度太低的不返回(噪声)
            if sem < MIN_SIM_THRESHOLD:
                continue
            node = self.get_node(nid)
            if node:
                node["score"] = round(fused, 4)
                node["semantic_score"] = round(sem, 4)
                node["pagerank_score"] = round(pr_s, 4)
                node["match_type"] = "graph_spread" if pr_s > 0.01 else "semantic"
                # 类型优先级:分数差距 <0.05 时按类型优先
                node["_sort_key"] = (round(fused, 2) - 0.01 * type_priority.get(node.get("node_type", ""), 9))
                results.append(node)
        # 按调整后的 key 重排,取 top_k
        results.sort(key=lambda x: -x["_sort_key"])
        for r in results:
            r.pop("_sort_key", None)
        return results[:top_k]

    # ── 图统计 ────────────────────────────────────────────────

    def stats(self) -> dict:
        return {
            "node_count": self.graph.number_of_nodes(),
            "edge_count": self.graph.number_of_edges(),
            "density": round(nx.density(self.graph), 4) if self.graph.number_of_nodes() > 1 else 0,
            "avg_degree": round(2 * self.graph.number_of_edges() / max(self.graph.number_of_nodes(), 1), 2),
        }

    def snapshot(self) -> dict:
        """完整图快照给前端"""
        nodes = []
        for nid, attrs in self.graph.nodes(data=True):
            nodes.append({"id": nid, "data": dict(attrs)})
        edges = []
        for s, t, attrs in self.graph.edges(data=True):
            edges.append({"source": s, "target": t, "data": dict(attrs)})
        return {"nodes": nodes, "edges": edges, "stats": self.stats()}
