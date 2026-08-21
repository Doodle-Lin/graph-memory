"""Graph Memory 引擎层单元测试

不依赖真实 embedding 模型:用确定性假 embedder 注入,保证测试快且离线可复现。
用 tmp_path 隔离数据文件,不触碰真实 data/。

运行: pytest tests/test_engine.py -v
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import numpy as np
import pytest

# 让 tests 能 import graph_memory(项目根在父目录)
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from graph_memory import config
from graph_memory.engine import GraphEngine, _node_id


class FakeEmbedder:
    """确定性假 embedder: bag-of-words 哈希向量,归一化。

    贴近真实 embedder 的语义特性:
    - 相同文本 → 相同向量(MD5 与 embedding 去重可测)
    - 共享较多词元的文本 → 高余弦相似(auto_link 阈值可测)
    - 词元完全不重合 → 接近正交
    不依赖任何外部模型,毫秒级,可离线复现。
    """

    def __init__(self, dim: int = 64):
        self.dim = dim

    def _tokens(self, text: str) -> list[str]:
        # 按非字母数字切分,中文字符按单字拆;统一小写
        import re
        parts = re.findall(r"[a-zA-Z0-9]+|[一-鿿]", text)
        return [p.lower() for p in parts if p]

    def _vec(self, text: str) -> np.ndarray:
        v = np.zeros(self.dim, dtype=np.float32)
        for tok in self._tokens(text):
            h = hash(tok) % self.dim
            v[h] += 1.0
        n = np.linalg.norm(v)
        return v / n if n > 0 else v

    def encode(self, texts, normalize_embeddings=True):
        if isinstance(texts, str):
            return self._vec(texts)
        return np.array([self._vec(t) for t in texts])

    def get_sentence_embedding_dimension(self):
        return self.dim


@pytest.fixture
def engine(tmp_path, monkeypatch):
    """一个用临时数据目录、注入假 embedder 的空图引擎。"""
    monkeypatch.setattr(config, "GRAPH_FILE", tmp_path / "graph.json")
    monkeypatch.setattr(config, "GRAPH_DB", tmp_path / "graph.db")
    monkeypatch.setattr(config, "EMBEDDINGS_FILE", tmp_path / "embeddings.npz")
    # GraphEngine.__init__ 里 from .config import 的是模块属性引用,
    # 但 engine.py 顶部已 `from .config import GRAPH_FILE` 把值绑定到 engine 模块。
    # 所以需要同时 patch engine 模块里的名字。
    from graph_memory import engine as engine_mod
    monkeypatch.setattr(engine_mod, "GRAPH_FILE", tmp_path / "graph.json")
    monkeypatch.setattr(engine_mod, "GRAPH_DB", tmp_path / "graph.db")
    monkeypatch.setattr(engine_mod, "EMBEDDINGS_FILE", tmp_path / "embeddings.npz")

    eng = GraphEngine()
    eng._embedder = FakeEmbedder(dim=64)  # 注入假 embedder,跳过真实模型加载
    assert eng.graph.number_of_nodes() == 0
    return eng


# ── add_node 三层去重 ──────────────────────────────────────

class TestDedup:
    def test_new_node_created(self, engine):
        node = engine.add_node("vLLM uses PagedAttention for KV cache", title="paged")
        assert node is not None
        assert node["content"] == "vLLM uses PagedAttention for KV cache"
        assert engine.graph.number_of_nodes() == 1

    def test_identical_content_merges_md5(self, engine):
        """完全相同内容 → 合并,节点数不增。"""
        engine.add_node("identical content here")
        n1 = engine.graph.number_of_nodes()
        again = engine.add_node("identical content here")
        assert engine.graph.number_of_nodes() == n1  # 没新增
        assert again["id"] == _node_id("identical content here")

    def test_metadata_merged_on_dedup(self, engine):
        engine.add_node("shared fact", metadata={"a": 1})
        engine.add_node("shared fact", metadata={"b": 2})
        nid = _node_id("shared fact")
        meta = engine.graph.nodes[nid].get("metadata", {})
        assert meta.get("a") == 1 and meta.get("b") == 2

    def test_type_normalization(self, engine):
        """非法 node_type 应回退到 knowledge,原类型记入 metadata。"""
        node = engine.add_node("some fact", node_type="history")
        assert node["node_type"] == "knowledge"
        assert engine.graph.nodes[node["id"]]["metadata"].get("original_type") == "history"

    def test_valid_type_preserved(self, engine):
        node = engine.add_node("pref content", node_type="preference")
        assert node["node_type"] == "preference"
        assert "original_type" not in engine.graph.nodes[node["id"]].get("metadata", {})


# ── auto_link ─────────────────────────────────────────────

class TestAutoLink:
    def test_no_links_on_single_node(self, engine):
        node = engine.add_node("only one node")
        links = engine.auto_link(node["id"])
        assert links == []  # 没有别的节点可连

    def test_links_created_for_similar(self, engine):
        """两个共享较多词元的节点应能自动建边。

        注:add_node 本身不做 auto_link(由上层 /api/write 或 importer 显式调用),
        这里显式调用 auto_link 来测它是否建边。内容过近会被 embedding 去重(>0.85 合并),
        所以用同主题但不同表述的节点,确保它们都作为独立节点存在。
        """
        a = engine.add_node("vLLM 用 PagedAttention 分页管理 KV cache 提升并发")
        b = engine.add_node("vLLM 的 PagedAttention 把 KV cache 分页 管理显存碎片")
        assert engine.graph.number_of_nodes() == 2
        links = engine.auto_link(b["id"], max_links=5)
        assert len(links) >= 1
        assert engine.graph.number_of_edges() >= 1

    def test_self_not_linked(self, engine):
        node = engine.add_node("lonely node uniquexyz")
        links = engine.auto_link(node["id"])
        for l in links:
            assert l["target"] != node["id"]


# ── retrieve ──────────────────────────────────────────────

class TestRetrieve:
    def test_empty_graph_returns_empty(self, engine):
        assert engine.retrieve("anything") == []

    def test_retrieve_returns_nodes(self, engine):
        engine.add_node("vLLM PagedAttention 分页管理 KV cache")
        results = engine.retrieve("vLLM PagedAttention", top_k=5)
        assert len(results) >= 1
        assert all("score" in r for r in results)
        assert all("content" in r for r in results)

    def test_retrieve_deterministic(self, engine):
        """同一 query 多次检索结果一致(确定性)。"""
        engine.add_node("deterministic retrieval test node alpha")
        engine.add_node("deterministic retrieval test node beta")
        r1 = [r["id"] for r in engine.retrieve("deterministic retrieval", top_k=5)]
        r2 = [r["id"] for r in engine.retrieve("deterministic retrieval", top_k=5)]
        assert r1 == r2

    def test_spread_off_returns_semantic(self, engine):
        engine.add_node("semantic only retrieval node")
        results = engine.retrieve("semantic only", top_k=5, spread=False)
        assert len(results) >= 1


# ── update / delete ────────────────────────────────────────

class TestUpdateDelete:
    def test_update_content_changes_embedding(self, engine):
        node = engine.add_node("original content")
        nid = node["id"]
        old_emb = engine._embeddings[nid].copy()
        engine.update_node(nid, content="completely new content")
        assert engine.graph.nodes[nid]["content"] == "completely new content"
        assert not np.allclose(engine._embeddings[nid], old_emb)

    def test_update_missing_raises(self, engine):
        with pytest.raises(KeyError):
            engine.update_node("nonexistent", content="x")

    def test_delete_removes_node_and_edges(self, engine):
        a = engine.add_node("node A for delete test")
        b = engine.add_node("node B for delete test related")
        engine.auto_link(a["id"])
        # 删 A 后,A 的边也应消失
        engine.delete_node(a["id"])
        assert a["id"] not in engine.graph
        # B 还在
        assert b["id"] in engine.graph

    def test_delete_missing_returns_false(self, engine):
        assert engine.delete_node("does-not-exist") is False


# ── 持久化 ─────────────────────────────────────────────────

class TestPersistence:
    def test_save_then_reload(self, engine, tmp_path):
        engine.add_node("persisted knowledge node")
        engine.add_node("another persisted node")
        engine.save()

        # 重新加载(同一文件)
        from graph_memory import engine as engine_mod
        eng2 = GraphEngine()
        eng2._embedder = FakeEmbedder(dim=64)
        assert eng2.graph.number_of_nodes() == 2
        contents = {d.get("content") for _, d in eng2.graph.nodes(data=True)}
        assert "persisted knowledge node" in contents

    def test_flush_only_writes_when_dirty(self, engine, tmp_path):
        engine.add_node("dirty test node")
        engine.flush()
        assert engine._save_dirty is False
        # 再次 flush 不应报错
        engine.flush()
