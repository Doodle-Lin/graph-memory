"""Graph Memory 检索回归基线 — 引擎改动前后对比

为什么单独写、不复用 benchmark.py:
  benchmark.py 回答的是"图记忆 vs 只用 MEMORY.md 谁好",全程套 LLM judge,
  3 轮取平均仍有噪声,且要 180 次 LLM 调用/十几分钟。
  而引擎改动(auto_link 方向、PageRank 归一化、类型优先级)影响的是 retrieve()
  本身——它是确定性的(embedding→argsort→PageRank 全可复现),无需 LLM。
  所以这里用纯检索快照做尺子:固定 query → 录返回的节点 id/分数/排名 → diff。

用法(需先启动 server: python -m graph_memory.server):
  python regression.py snapshot baseline        # 录当前检索结果为 baseline
  python regression.py snapshot after-change    # 改完代码后再录一次
  python regression.py compare baseline after-change   # 对比

不动引擎、不动 benchmark.py。只读 /api/retrieve 和 /api/stats。
"""
from __future__ import annotations

import json
import hashlib
import os
import sys
import time
from pathlib import Path

import requests

API = "http://127.0.0.1:9121"
SNAPSHOT_DIR = Path(__file__).resolve().parent / "regression_snapshots"
SNAPSHOT_DIR.mkdir(exist_ok=True)

# 回归用 query 集。开源版用占位 query(用户应替换为自己的知识库问答对)。
# 与 benchmark.py 的题集保持同构:A=图里有答案, B=图里没有(防幻觉), C=部分信息需推理。
# 注:已脱敏为通用示例,你应当替换为针对自己图数据的真实 query,回归才有意义。
QUERIES = [
    # === A组: 图记忆中有明确答案 ===
    {"q": "示例项目部署在哪个服务器?IP和端口?", "ref": "(请替换)", "group": "A"},
    {"q": "示例设备的语音方案用什么?端口号?", "ref": "(请替换)", "group": "A"},
    {"q": "示例设备的三层架构是什么?", "ref": "(请替换)", "group": "A"},
    {"q": "Docker跨服务器部署怎么做?", "ref": "docker save|gzip导出→scp中转→docker load导入,不直接build(慢)", "group": "A"},
    {"q": "ComfyUI fp8怎么配置?省多少显存?", "ref": "bf16+weight_dtype=fp8_e4m3fn,省50%显存", "group": "A"},
    {"q": "示例推理服务在服务器上的路径和端口?", "ref": "(请替换)", "group": "A"},
    {"q": "示例API用的什么模型?", "ref": "(请替换)", "group": "A"},
    {"q": "示例训练环境的根目录?", "ref": "(请替换)", "group": "A"},
    {"q": "用户对文档生成有什么偏好?", "ref": "(请替换)", "group": "A"},
    {"q": "用户工作目录偏好是什么?", "ref": "(请替换)", "group": "A"},
    # === B组: 图里没有(防幻觉)——retrieve 仍会返回 top-k,记下看改动是否让它返回不同噪声 ===
    {"q": "2024年Nobel物理学奖得主是谁?", "ref": "(不在知识库)", "group": "B"},
    {"q": "Python 3.13新增了什么特性?", "ref": "(不在知识库)", "group": "B"},
    {"q": "上海今天天气怎么样?", "ref": "(不在知识库)", "group": "B"},
    {"q": "Rust语言的async运行时有哪些?", "ref": "(不在知识库)", "group": "B"},
    {"q": "CRISPR基因编辑技术的原理是什么?", "ref": "(不在知识库)", "group": "B"},
    {"q": "特斯拉2024年Q3财报营收多少?", "ref": "(不在知识库)", "group": "B"},
    {"q": "Kubernetes的Pod和Deployment有什么区别?", "ref": "(不在知识库)", "group": "B"},
    {"q": "长江的全长是多少公里?", "ref": "(不在知识库)", "group": "B"},
    {"q": "量子计算中的qubit和经典bit的本质区别?", "ref": "(不在知识库)", "group": "B"},
    {"q": "贝多芬第九交响曲创作于哪一年?", "ref": "(不在知识库)", "group": "B"},
    # === C组: 边缘(部分信息+推理) ===
    {"q": "如果我要在新服务器上部署服务,应该注意什么?", "ref": "(请替换)", "group": "C"},
    {"q": "面试被问到量化原理,我应该怎么准备?", "ref": "(请替换)", "group": "C"},
    {"q": "示例设备系统更新后出问题怎么办?", "ref": "(请替换)", "group": "C"},
    {"q": "在示例GPU上训练LoRA需要什么环境?", "ref": "(请替换)", "group": "C"},
    {"q": "用户说不懂某个技术概念时应该怎么做?", "ref": "(请替换)", "group": "C"},
    {"q": "Hermes和Claude Code的记忆文件分别在哪?", "ref": "Hermes:~/.hermes/memories/MEMORY.md,Claude:~/.claude/projects/*/memory/*.md", "group": "C"},
    {"q": "在内网开发有什么限制?", "ref": "(请替换)", "group": "C"},
    {"q": "vLLM和TensorRT-LLM有什么区别?", "ref": "vLLM更易用(Python原生),TRT-LLM更快(需编译,延迟更低)", "group": "C"},
    {"q": "示例教学项目有几个阶段?", "ref": "(请替换)", "group": "C"},
    {"q": "用户对实验代码的Git管理有什么要求?", "ref": "及时conventional commit,实验在feature分支,每步commit,确认收益后合入main", "group": "C"},
]

TOP_K = 5


def _graph_fingerprint(stats: dict, graph_snapshot: dict) -> str:
    """图指纹:节点 id 排序后哈希 + 边数。变了说明图数据本身变了,baseline 失效。"""
    node_ids = sorted(n["id"] for n in graph_snapshot.get("nodes", []))
    edge_count = stats.get("edge_count", 0)
    return hashlib.md5(("|".join(node_ids) + f"#{edge_count}").encode()).hexdigest()[:12]


def _retrieve(query: str) -> tuple[list[dict], float]:
    t0 = time.time()
    r = requests.post(f"{API}/api/retrieve",
                      json={"query": query, "top_k": TOP_K, "spread": True},
                      timeout=30)
    elapsed = time.time() - t0
    r.raise_for_status()
    results = r.json()
    # 只留回归需要的字段(去掉 content 避免快照过大)
    slim = []
    for i, res in enumerate(results):
        slim.append({
            "rank": i,
            "id": res.get("id"),
            "title": res.get("title", "")[:60],
            "node_type": res.get("node_type", ""),
            "score": res.get("score"),
            "semantic_score": res.get("semantic_score"),
            "pagerank_score": res.get("pagerank_score"),
            "match_type": res.get("match_type", ""),
        })
    return slim, elapsed


def snapshot(name: str) -> None:
    print(f"录制检索快照 → {name}")
    stats = requests.get(f"{API}/api/stats", timeout=10).json()
    graph_snapshot = requests.get(f"{API}/api/graph", timeout=30).json()
    fp = _graph_fingerprint(stats, graph_snapshot)

    per_query = []
    for i, q in enumerate(QUERIES):
        results, ret_time = _retrieve(q["q"])
        per_query.append({
            "q": q["q"], "group": q["group"], "ref": q["ref"],
            "results": results, "ret_time": round(ret_time, 4),
        })
        top1 = results[0]["title"] if results else "(空)"
        print(f"  [{q['group']}] Q{i+1:2d} {q['q'][:30]:32s} → {top1}")

    out = {
        "name": name,
        "timestamp": time.strftime("%Y-%m-%d %H:%M:%S"),
        "graph_fingerprint": fp,
        "graph_stats": stats,
        "top_k": TOP_K,
        "queries": per_query,
    }
    path = SNAPSHOT_DIR / f"{name}.json"
    path.write_text(json.dumps(out, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"\n图指纹: {fp}  节点={stats['node_count']} 边={stats['edge_count']}")
    print(f"已保存: {path}")


def _jaccard(a: list[str], b: list[str]) -> float:
    sa, sb = set(a), set(b)
    if not sa and not sb:
        return 1.0
    return len(sa & sb) / len(sa | sb)


def compare(name_a: str, name_b: str) -> None:
    pa = SNAPSHOT_DIR / f"{name_a}.json"
    pb = SNAPSHOT_DIR / f"{name_b}.json"
    if not pa.exists() or not pb.exists():
        print(f"缺少快照: {pa.exists()} {pb.exists()}")
        sys.exit(1)
    a = json.loads(pa.read_text(encoding="utf-8"))
    b = json.loads(pb.read_text(encoding="utf-8"))

    print("=" * 78)
    print(f"  检索回归对比: {name_a}  →  {name_b}")
    print("=" * 78)

    if a["graph_fingerprint"] != b["graph_fingerprint"]:
        print(f"  ⚠ 图指纹不同! {a['graph_fingerprint']} vs {b['graph_fingerprint']}")
        print(f"    ({a['graph_stats']['node_count']}节点/{a['graph_stats']['edge_count']}边"
              f"  →  {b['graph_stats']['node_count']}节点/{b['graph_stats']['edge_count']}边)")
        print(f"    图数据本身变了,diff 不完全等于引擎改动效果,需人工判断。")
    else:
        print(f"  图指纹一致 ({a['graph_fingerprint']}),diff 纯粹反映引擎改动。")

    qa = {q["q"]: q for q in a["queries"]}
    qb = {q["q"]: q for q in b["queries"]}

    top1_changes = 0
    jaccard_sum = 0.0
    score_drift_sum = 0.0
    moved_queries = []

    for q in QUERIES:
        ra = qa.get(q["q"], {}).get("results", [])
        rb = qb.get(q["q"], {}).get("results", [])
        ids_a = [r["id"] for r in ra]
        ids_b = [r["id"] for r in rb]
        jac = _jaccard(ids_a[:TOP_K], ids_b[:TOP_K])
        jaccard_sum += jac
        top1_changed = bool(ra and rb and ra[0]["id"] != rb[0]["id"])
        if top1_changed:
            top1_changes += 1
        # 分数漂移:同 id 节点的 |Δscore| 平均
        score_a = {r["id"]: r["score"] for r in ra}
        shared = [r for r in rb if r["id"] in score_a and r["score"] is not None]
        if shared:
            drift = sum(abs(r["score"] - score_a[r["id"]]) for r in shared) / len(shared)
            score_drift_sum += drift
        else:
            score_drift_sum += 0.0

        if jac < 1.0 or top1_changed:
            moved_queries.append((q, ra, rb, jac, top1_changed))

    n = len(QUERIES)
    print(f"\n  总览 ({n} 题):")
    print(f"    top-1 变化:        {top1_changes}/{n}")
    print(f"    top-5 平均 Jaccard: {jaccard_sum/n:.3f}  (1.0=完全一致)")
    print(f"    同节点平均分数漂移: {score_drift_sum/n:.4f}")

    if not moved_queries:
        print(f"\n  ✅ 检索结果与 baseline 完全一致(节点集合+top1 未变)。")
        return

    print(f"\n  发生变化的题 ({len(moved_queries)} 题):")
    print(f"  {'─' * 78}")
    for q, ra, rb, jac, t1c in moved_queries:
        flag = "🔴" if t1c else "🟡"
        print(f"  {flag} [{q['group']}] {q['q']}")
        print(f"     Jaccard={jac:.2f}  top1变化={'是' if t1c else '否'}")
        print(f"     旧: {_fmt_rank(ra)}")
        print(f"     新: {_fmt_rank(rb)}")
    print(f"  {'─' * 78}")
    print(f"  🔴=top1 换了节点  🟡=top1 没变但 top5 集合有出入")


def _fmt_rank(results: list[dict]) -> str:
    if not results:
        return "(空)"
    parts = []
    for r in results:
        sc = r.get("score")
        sc = f"{sc:.3f}" if isinstance(sc, (int, float)) else "?"
        parts.append(f"{r['rank']}·{r['title'][:18]}({sc})")
    return " | ".join(parts)


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return
    cmd = sys.argv[1]
    if cmd == "snapshot" and len(sys.argv) >= 3:
        snapshot(sys.argv[2])
    elif cmd == "compare" and len(sys.argv) >= 4:
        compare(sys.argv[2], sys.argv[3])
    else:
        print(__doc__)


if __name__ == "__main__":
    main()
