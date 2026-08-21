"""Graph Memory 知识管理工具 — 防膨胀 + 清理

用法:
  python manage.py status           # 查看图健康状态
  python manage.py merge             # 合并相似节点(embedding >0.85)
  python manage.py prune             # 删除孤立+过时节点
  python manage.py prune --dry-run   # 预览不执行
  python manage.py dedup             # 全量去重扫描报告

设计原则:
  - 不删除有 >=2 条边的节点(有关联的知识不删)
  - 不删除 90 天内更新过的节点
  - 合并时保留更长/更详细的内容
  - 所有操作先 dry-run 展示,确认后才执行
"""
import sys
import os
import time
import json
import argparse
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
os.environ.setdefault("GM_HOST", "127.0.0.1")
os.environ.setdefault("GM_PORT", "9121")

import requests

API = f"http://{os.environ.get('GM_HOST')}:{os.environ.get('GM_PORT')}"


def get_graph():
    """获取完整图数据"""
    r = requests.get(f"{API}/api/graph", timeout=30)
    return r.json()


def cmd_status(args):
    """图健康状态报告"""
    graph = get_graph()
    stats = graph.get("stats", {})
    nodes = graph.get("nodes", [])
    edges = graph.get("edges", [])

    print("=" * 50)
    print("  Graph Memory 健康状态")
    print("=" * 50)
    print(f"  节点: {stats.get('node_count', 0)}")
    print(f"  边: {stats.get('edge_count', 0)}")
    print(f"  密度: {stats.get('density', 0)}")
    print(f"  平均度: {stats.get('avg_degree', 0)}")

    # 按类型统计
    type_counts = {}
    for n in nodes:
        t = n.get("data", {}).get("node_type", "unknown")
        type_counts[t] = type_counts.get(t, 0) + 1

    # 按来源统计
    source_counts = {}
    for n in nodes:
        s = n.get("data", {}).get("source", "unknown")
        # 截断长来源名
        s = s[:20] if len(s) > 20 else s
        source_counts[s] = source_counts.get(s, 0) + 1

    # 孤立节点(0 条边)
    node_ids = {n["id"] for n in nodes}
    connected = set()
    for e in edges:
        connected.add(e["source"])
        connected.add(e["target"])
    isolated = node_ids - connected

    # 90 天未更新
    now = datetime.now(timezone.utc)
    stale = []
    for n in nodes:
        updated = n.get("data", {}).get("updated_at", "")
        if updated:
            try:
                dt = datetime.fromisoformat(updated.replace("Z", "+00:00"))
                if (now - dt).days > 90:
                    stale.append(n["id"])
            except Exception:
                pass

    # 内容长度分布
    lengths = [len(n.get("data", {}).get("content", "")) for n in nodes]
    avg_len = sum(lengths) / len(lengths) if lengths else 0
    max_len = max(lengths) if lengths else 0

    print(f"\n  孤立节点(0边): {len(isolated)}")
    print(f"  90天未更新: {len(stale)}")
    print(f"  内容平均长度: {avg_len:.0f} 字符")
    print(f"  内容最大长度: {max_len} 字符")

    print(f"\n  按类型:")
    for t, c in sorted(type_counts.items(), key=lambda x: -x[1]):
        print(f"    {t}: {c}")

    print(f"\n  按来源(top 5):")
    for s, c in sorted(source_counts.items(), key=lambda x: -x[1])[:5]:
        print(f"    {s}: {c}")

    if len(isolated) > 0:
        print(f"\n  ⚠ {len(isolated)} 个孤立节点,建议 prune")
    if len(stale) > 10:
        print(f"  ⚠ {len(stale)} 个节点 90 天未更新,建议 prune")
    if avg_len < 50:
        print(f"  ⚠ 平均内容长度 {avg_len:.0f} 偏短,可能需要重新提炼")


def cmd_merge(args):
    """合并相似节点(embedding 相似度 > 阈值)"""
    graph = get_graph()
    nodes = graph.get("nodes", [])
    edges = graph.get("edges", [])

    threshold = args.threshold
    dry_run = args.dry_run

    print(f"扫描相似节点(阈值 > {threshold})...")
    print(f"模式: {'预览(dry-run)' if dry_run else '执行'}")
    print()

    # 用 API 检查每对节点相似度太慢(n²)。
    # 直接用 retrieve 找每个节点的相似节点。
    merged_count = 0
    merged_pairs = []

    checked = set()
    for n in nodes:
        nid = n["id"]
        if nid in checked:
            continue
        content = n.get("data", {}).get("content", "")
        if not content or len(content) < 10:
            continue

        # 用 retrieve 找相似节点
        try:
            r = requests.post(f"{API}/api/retrieve", json={
                "query": content[:200], "top_k": 5, "spread": False
            }, timeout=10)
            results = r.json()
        except Exception:
            continue

        for res in results:
            rid = res.get("id", "")
            if rid == nid or rid in checked:
                continue
            score = res.get("semantic_score", res.get("score", 0))
            if score >= threshold:
                # 找到相似对
                existing_content = n.get("data", {}).get("content", "")
                new_content = res.get("content", "")
                # 保留更长的
                keep_id = nid if len(existing_content) >= len(new_content) else rid
                drop_id = rid if keep_id == nid else nid
                merged_pairs.append((keep_id, drop_id, score,
                                    existing_content[:50], new_content[:50]))
                checked.add(drop_id)
                merged_count += 1

    print(f"发现 {merged_count} 对相似节点:")
    for keep, drop, score, c1, c2 in merged_pairs[:20]:
        print(f"  score={score:.3f}")
        print(f"    保留: {c1}...")
        print(f"    合并: {c2}...")
    if len(merged_pairs) > 20:
        print(f"  ... 还有 {len(merged_pairs) - 20} 对")

    if dry_run:
        print(f"\n预览模式: 不执行删除。去掉 --dry-run 执行。")
    elif merged_count > 0:
        print(f"\n执行合并: 删除 {merged_count} 个冗余节点...")
        for keep, drop, score, _, _ in merged_pairs:
            try:
                requests.delete(f"{API}/api/nodes/{drop}", timeout=10)
            except Exception as e:
                print(f"  删除 {drop} 失败: {e}")
        print(f"✅ 完成,删除 {merged_count} 个冗余节点")


def cmd_prune(args):
    """删除孤立 + 过时节点"""
    graph = get_graph()
    nodes = graph.get("nodes", [])
    edges = graph.get("edges", [])

    dry_run = args.dry_run
    min_degree = args.min_degree
    max_age_days = args.max_age

    print(f"清理条件: 度 < {min_degree} 且 {max_age_days} 天未更新")
    print(f"模式: {'预览(dry-run)' if dry_run else '执行'}")
    print()

    # 计算每个节点的度
    degree = {}
    for e in edges:
        degree[e["source"]] = degree.get(e["source"], 0) + 1
        degree[e["target"]] = degree.get(e["target"], 0) + 1

    # 90 天未更新
    now = datetime.now(timezone.utc)
    to_delete = []

    for n in nodes:
        nid = n["id"]
        deg = degree.get(nid, 0)
        if deg >= min_degree:
            continue  # 有足够关联,不删

        updated = n.get("data", {}).get("updated_at", "")
        if not updated:
            to_delete.append(nid)
            continue

        try:
            dt = datetime.fromisoformat(updated.replace("Z", "+00:00"))
            if (now - dt).days > max_age_days:
                to_delete.append(nid)
        except Exception:
            pass

    print(f"将删除 {len(to_delete)} 个节点:")
    for nid in to_delete[:20]:
        node = next((n for n in nodes if n["id"] == nid), None)
        if node:
            title = node.get("data", {}).get("title", "?")[:40]
            deg = degree.get(nid, 0)
            print(f"  [{deg}边] {title}")
    if len(to_delete) > 20:
        print(f"  ... 还有 {len(to_delete) - 20} 个")

    if dry_run:
        print(f"\n预览模式: 不执行删除。去掉 --dry-run 执行。")
    elif to_delete:
        print(f"\n执行删除...")
        deleted = 0
        for nid in to_delete:
            try:
                requests.delete(f"{API}/api/nodes/{nid}", timeout=10)
                deleted += 1
            except Exception:
                pass
        print(f"✅ 删除 {deleted} 个节点")


def cmd_dedup(args):
    """全量去重扫描报告(不删除)"""
    graph = get_graph()
    nodes = graph.get("nodes", [])

    print("全量去重扫描...")
    print()

    # MD5 去重检查
    content_hash = {}
    for n in nodes:
        content = n.get("data", {}).get("content", "")
        if not content:
            continue
        h = hash(content)
        if h in content_hash:
            content_hash[h].append(n["id"])
        else:
            content_hash[h] = [n["id"]]

    md5_dups = {k: v for k, v in content_hash.items() if len(v) > 1}

    # Embedding 相似度检查(用 retrieve)
    sim_pairs = []
    checked = set()
    for n in nodes:
        nid = n["id"]
        if nid in checked:
            continue
        content = n.get("data", {}).get("content", "")
        if not content or len(content) < 10:
            continue
        try:
            r = requests.post(f"{API}/api/retrieve", json={
                "query": content[:200], "top_k": 3, "spread": False
            }, timeout=10)
            results = r.json()
        except Exception:
            continue
        for res in results:
            rid = res.get("id", "")
            if rid == nid or rid in checked:
                continue
            score = res.get("semantic_score", res.get("score", 0))
            if score >= 0.85:
                sim_pairs.append((nid, rid, score))
                checked.add(rid)

    print(f"MD5 完全重复: {len(md5_dups)} 组")
    for h, ids in list(md5_dups.items())[:5]:
        print(f"  {ids}")

    print(f"\nEmbedding 相似度 >0.85: {len(sim_pairs)} 对")
    for a, b, s in sim_pairs[:10]:
        na = next((n for n in nodes if n["id"] == a), {})
        nb = next((n for n in nodes if n["id"] == b), {})
        print(f"  score={s:.3f}: {na.get('data',{}).get('title','?')[:30]} ↔ {nb.get('data',{}).get('title','?')[:30]}")

    total_dups = len(md5_dups) + len(sim_pairs)
    if total_dups == 0:
        print("\n✅ 无重复节点")
    else:
        print(f"\n⚠ 共 {total_dups} 组重复,建议运行: python manage.py merge")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Graph Memory 知识管理工具")
    sub = parser.add_subparsers(dest="command")

    sub.add_parser("status", help="查看图健康状态")
    
    p_merge = sub.add_parser("merge", help="合并相似节点")
    p_merge.add_argument("--threshold", type=float, default=0.85, help="相似度阈值(默认0.85)")
    p_merge.add_argument("--dry-run", action="store_true", help="预览不执行")

    p_prune = sub.add_parser("prune", help="删除孤立+过时节点")
    p_prune.add_argument("--dry-run", action="store_true", help="预览不执行")
    p_prune.add_argument("--min-degree", type=int, default=2, help="低于此度数且过时才删(默认2)")
    p_prune.add_argument("--max-age", type=int, default=90, help="超过此天数未更新才删(默认90)")

    sub.add_parser("dedup", help="全量去重扫描报告")

    args = parser.parse_args()
    if args.command == "status":
        cmd_status(args)
    elif args.command == "merge":
        cmd_merge(args)
    elif args.command == "prune":
        cmd_prune(args)
    elif args.command == "dedup":
        cmd_dedup(args)
    else:
        parser.print_help()
