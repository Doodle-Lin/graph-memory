#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P3-B (task-17 / L4-01) 中文检索评测运行器。

只用 Python 标准库。行为：
  1. 随机端口启动 frozen debug 服务（不自行编译），在全新临时目录播种；
  2. 全部记录经真实 v1 HTTP 接口写入（带 event_id），写入后逐条回读校验；
  3. 跑完数据集全部查询，计算 Recall@5 / MRR / 跨项目污染率 / 无答案误召回率 / 撤回泄漏数；
  4. 写出 report.json（逐条 query/期望/实际/名次）、report.md（分类别汇总 + 最差 10 条）、
     seed_manifest.json（fixture_id -> 真实 UUID/版本）。

用法：
  python tests/eval/run_eval.py [--exe artifacts/eval/frozen/graph-memory-service.exe]
                                [--out-dir artifacts/eval] [--label baseline]
"""

import argparse
import hashlib
import http.client
import json
import os
import platform
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
TOP_K = 5
CATEGORIES = [
    "synonym",
    "project_conflict",
    "long_question",
    "paraphrase_hard",
    "retracted",
    "no_answer",
    "latest_version",
    "stale_outdated",
]
CATEGORY_LABEL = {
    "synonym": "同义改写",
    "project_conflict": "项目冲突",
    "long_question": "中文长句(>=25字)",
    "paraphrase_hard": "同义改写(不照抄)",
    "retracted": "过期/已撤回",
    "no_answer": "无结果",
    "latest_version": "最新版本",
    "stale_outdated": "过期内容(信息性)",
}


# --------------------------------------------------------------------------- HTTP

class Api:
    """显式 UTF-8 的极简 HTTP 客户端（PS/默认解码不可靠，这里自己解码字节）。"""

    def __init__(self, base_host, base_port, log_path=None):
        self.host = base_host
        self.port = base_port
        self.log = []
        self.log_path = log_path

    def _record(self, method, path, body, status, raw):
        self.log.append(
            {"method": method, "path": path, "body": body, "status": status,
             "response": raw.decode("utf-8", "replace")}
        )
        if self.log_path:
            with open(self.log_path, "a", encoding="utf-8") as handle:
                handle.write(json.dumps(self.log[-1], ensure_ascii=False) + "\n")

    def call(self, method, path, body=None):
        payload = None
        if body is not None:
            payload = json.dumps(body, ensure_ascii=False).encode("utf-8")
        conn = http.client.HTTPConnection(self.host, self.port, timeout=30)
        headers = {"Content-Type": "application/json; charset=utf-8"}
        conn.request(method, path, body=payload, headers=headers)
        response = conn.getresponse()
        raw = response.read()
        status = response.status
        conn.close()
        self._record(method, path, body, status, raw)
        parsed = json.loads(raw.decode("utf-8"))
        return status, parsed

    def ok(self, method, path, body=None):
        status, parsed = self.call(method, path, body)
        if status != 200:
            raise RuntimeError("%s %s -> HTTP %s: %s" % (method, path, status, parsed))
        return parsed


# --------------------------------------------------------------------------- service

def wait_for_service(proc, out_path, timeout=40.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if proc.poll() is not None:
            raise RuntimeError("service exited early rc=%s" % proc.returncode)
        try:
            with open(out_path, "r", encoding="utf-8", errors="replace") as handle:
                text = handle.read()
        except OSError:
            text = ""
        for line in text.splitlines():
            line = line.strip()
            if line.startswith("{"):
                try:
                    info = json.loads(line)
                except ValueError:
                    continue
                if "api_url" in info:
                    url = info["api_url"]
                    return info, url.rsplit(":", 1)[0].replace("http://", ""), int(url.rsplit(":", 1)[1])
        time.sleep(0.25)
    raise RuntimeError("service did not report api_url within %.0fs" % timeout)


def free_port():
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


# --------------------------------------------------------------------------- seeding

def seed(api, fixture, manifest, evidence):
    projects = {}
    for project in fixture["projects"]:
        created = api.ok("POST", "/api/v1/projects",
                         {"name": project["name"], "directory": project["directory"]})
        projects[project["key"]] = created["id"]
        if created.get("existing"):
            raise RuntimeError("project dir reused: %s" % project["directory"])
    manifest["projects"] = projects

    # 建立 project key -> fixture_id 的固定顺序，保证排序并列时的确定性。
    records = []
    for entry in fixture["memories"]:
        body = {
            "memory_scope": entry["scope"],
            "event_id": entry["event_id"],
            "title": entry["title"],
            "content": entry["content"],
            "kind": entry["kind"],
            "source": entry["source"],
            "conditions": entry["conditions"],
            "evidence_type": entry["evidence_type"],
        }
        if entry.get("project"):
            body["project_id"] = projects[entry["project"]]
        created = api.ok("POST", "/api/v1/memories", body)["memory"]
        if created["version"] != entry["expect_version"]:
            raise RuntimeError("unexpected version for %s: %s" % (entry["fixture_id"], created["version"]))
        records.append({"fixture": entry, "memory": created})
        evidence.append({"step": "create", "fixture_id": entry["fixture_id"],
                         "request": body, "response": created})

    for update in fixture["updates"]:
        match = next(r for r in records if r["fixture"]["fixture_id"] == update["fixture_id"])
        body = {"id": match["memory"]["id"], "expected_version": update["expected_version"],
                "content": update["content"], "conditions": update["conditions"]}
        updated = api.ok("POST", "/api/v1/memories/update", body)["memory"]
        if updated["version"] != update["expected_version"] + 1:
            raise RuntimeError("update produced version %s for %s"
                               % (updated["version"], update["fixture_id"]))
        match["memory"] = updated
        evidence.append({"step": "update", "fixture_id": update["fixture_id"],
                         "request": body, "response": updated})

    for record in records:
        retraction = record["fixture"].get("retract")
        if not retraction:
            continue
        body = {"id": record["memory"]["id"], "expected_version": record["memory"]["version"],
                "reason": retraction["reason"]}
        retracted = api.ok("POST", "/api/v1/memories/retract", body)["memory"]
        if retracted["version"] != record["memory"]["version"] + 1:
            raise RuntimeError("retract produced version %s for %s"
                               % (retracted["version"], record["fixture"]["fixture_id"]))
        if not retracted["retracted_at"]:
            raise RuntimeError("retract did not set retracted_at for %s"
                               % record["fixture"]["fixture_id"])
        record["memory"] = retracted
        evidence.append({"step": "retract", "fixture_id": record["fixture"]["fixture_id"],
                         "request": body, "response": retracted})

    manifest["memories"] = {
        r["fixture"]["fixture_id"]: {
            "id": r["memory"]["id"],
            "version": r["memory"]["version"],
            "created_at": r["memory"]["created_at"],
            "title": r["fixture"]["title"],
            "project": r["memory"]["project_id"],
            "scope": r["memory"]["memory_scope"],
            "retracted_at": r["memory"]["retracted_at"],
        }
        for r in records
    }
    manifest["seed_order"] = [r["fixture"]["fixture_id"] for r in records]
    return records


def verify_seed(api, fixture, records):
    """逐条回读：用该记录自己的标题作为查询（include_retracted=true），断言能取到自己的 id。"""
    failures = []
    for record in records:
        fixture_id = record["fixture"]["fixture_id"]
        found = api.ok("POST", "/api/v1/search",
                       {"scope": "all", "query": record["fixture"]["title"],
                        "include_retracted": True, "limit": 200})["memories"]
        ids = [item["id"] for item in found]
        if record["memory"]["id"] not in ids:
            failures.append({"fixture_id": fixture_id, "title": record["fixture"]["title"],
                             "returned": ids[:10]})
    return failures


# --------------------------------------------------------------------------- queries

def build_search_body(query):
    body = {"query": query["query"], "limit": TOP_K}
    if query["scope"] == "project":
        body["scope"] = "project"
        body["project_id"] = query["project"]
    elif query["scope"] == "personal":
        body["scope"] = "personal"
    else:
        body["scope"] = "all"
    return body


def run_queries(api, dataset, manifest):
    rows = []
    for query in dataset["queries"]:
        body = build_search_body(query)
        resolved_body = dict(body)
        if resolved_body.get("project_id"):
            resolved_body["project_id"] = manifest["projects"][resolved_body["project_id"]]
        result = api.ok("POST", "/api/v1/search", resolved_body)

        expected_project = None
        if query["scope"] == "project":
            expected_project = manifest["projects"][query["project"]]

        returned = []
        for rank, item in enumerate(result["memories"], start=1):
            returned.append({
                "rank": rank,
                "id": item["id"],
                "fixture_id": fixture_id_of(manifest, item["id"]),
                "score": item.get("score"),
                "memory_scope": item["memory_scope"],
                "project_id": item["project_id"],
                "version": item["version"],
                "retracted_at": item.get("retracted_at"),
            })

        expected = query.get("expected_ids") or []
        hits = [r for r in returned if r["fixture_id"] in expected] if expected else []
        rank = hits[0]["rank"] if hits else None
        row = {
            "id": query["id"],
            "category": query["category"],
            "query": query["query"],
            "scope": query["scope"],
            "project_key": query.get("project"),
            "project_id": expected_project,
            "expected_fixture_ids": expected,
            "expected_versions": query.get("expected_versions", {}),
            "actual": returned,
            "actual_fixture_ids": [r["fixture_id"] for r in returned],
            "rank": rank,
            "hit": bool(hits),
            "recall_at_5": 1.0 if hits else 0.0,
            "reciprocal_rank": (1.0 / rank) if rank else 0.0,
            "retracted_returned": sum(1 for r in returned if r["retracted_at"]),
            "cross_project_returned": sum(
                1 for r in returned
                if expected_project and r["memory_scope"] == "project"
                and r["project_id"] != expected_project
            ),
            "returned_total": result["total"],
            "retracted_excluded": result.get("retracted_excluded", 0),
            "version_violations": [
                r["fixture_id"] for r in returned
                if r["fixture_id"] in query.get("expected_versions", {})
                and r["version"] != query["expected_versions"][r["fixture_id"]]
            ],
        }

        if query["category"] == "retracted":
            include_body = dict(resolved_body)
            include_body["include_retracted"] = True
            include_result = api.ok("POST", "/api/v1/search", include_body)
            include_returned = [{
                "rank": i,
                "id": item["id"],
                "fixture_id": fixture_id_of(manifest, item["id"]),
                "retracted_at": item.get("retracted_at"),
            } for i, item in enumerate(include_result["memories"], start=1)]
            include_expected = query.get("include_retracted_expected") or []
            include_rank = next(
                (r["rank"] for r in include_returned if r["fixture_id"] in include_expected), None
            )
            row["include_retracted"] = {
                "expected_fixture_ids": include_expected,
                "actual_fixture_ids": [r["fixture_id"] for r in include_returned],
                "rank": include_rank,
                "hit": bool(include_rank),
                "retracted_excluded": include_result.get("retracted_excluded", 0),
            }
        rows.append(row)
    return rows


def fixture_id_of(manifest, memory_id):
    for fixture_id, info in manifest["memories"].items():
        if info["id"] == memory_id:
            return fixture_id
    return memory_id


# --------------------------------------------------------------------------- metrics

def summarize(rows):
    answered = [r for r in rows if r["expected_fixture_ids"]]
    no_answer = [r for r in rows if r["category"] == "no_answer"]
    stale = [r for r in rows if r["category"] == "stale_outdated"]
    total_returned = sum(r["returned_total"] for r in rows)
    cross_project = sum(r["cross_project_returned"] for r in rows)
    leaked = sum(r["retracted_returned"] for r in rows)

    def mean(values):
        return (sum(values) / len(values)) if values else 0.0

    metrics = {
        "queries_total": len(rows),
        "queries_with_answer": len(answered),
        "recall_at_5": mean([r["recall_at_5"] for r in answered]),
        "mrr": mean([r["reciprocal_rank"] for r in answered]),
        "cross_project_returned": cross_project,
        "returned_total": total_returned,
        "cross_project_pollution_rate": (cross_project / total_returned) if total_returned else 0.0,
        "no_answer_queries": len(no_answer),
        "no_answer_false_recall": sum(1 for r in no_answer if r["returned_total"] > 0),
        "no_answer_false_recall_rate": mean([1.0 if r["returned_total"] > 0 else 0.0 for r in no_answer]),
        "stale_outdated_queries": len(stale),
        "stale_outdated_false_recall": sum(1 for r in stale if r["returned_total"] > 0),
        "retracted_leaks_default_search": leaked,
        "version_violation_count": sum(len(r["version_violations"]) for r in rows),
    }
    per_category = {}
    for category in CATEGORIES:
        subset = [r for r in rows if r["category"] == category]
        if not subset:
            continue
        with_answer = [r for r in subset if r["expected_fixture_ids"]]
        per_category[category] = {
            "label": CATEGORY_LABEL[category],
            "queries": len(subset),
            "queries_with_answer": len(with_answer),
            "recall_at_5": mean([r["recall_at_5"] for r in with_answer]),
            "mrr": mean([r["reciprocal_rank"] for r in with_answer]),
            "hits": sum(1 for r in with_answer if r["hit"]),
            "misses": sum(1 for r in with_answer if not r["hit"]),
            "false_recall": sum(1 for r in subset if not r["expected_fixture_ids"] and r["returned_total"] > 0),
            "retracted_returned": sum(r["retracted_returned"] for r in subset),
        }
        if category == "retracted":
            include_hits = [r["include_retracted"]["hit"] for r in subset if r.get("include_retracted")]
            per_category[category]["include_retracted_recall"] = mean(
                [1.0 if h else 0.0 for h in include_hits]
            )
    return metrics, per_category


def worst(rows, count=10):
    """最差优先：漏召回 > 无答案误召回 > 过期信息性命中/正确结果。"""

    def sort_key(row):
        if row["expected_fixture_ids"]:
            bucket = 0 if not row["hit"] else 2
        elif row["category"] == "no_answer":
            bucket = 1 if row["returned_total"] > 0 else 3
        else:
            # stale_outdated 为信息性类别，仅排在命中之后展示。
            bucket = 2 if row["returned_total"] > 0 else 3
        return (bucket, row["reciprocal_rank"], row["id"])

    return sorted(rows, key=sort_key)[:count]


# --------------------------------------------------------------------------- report

def write_report(out_dir, metrics, per_category, rows, meta):
    report = {"meta": meta, "metrics": metrics, "per_category": per_category,
              "worst": [r["id"] for r in worst(rows)], "queries": rows}
    with open(os.path.join(out_dir, "report.json"), "w", encoding="utf-8") as handle:
        json.dump(report, handle, ensure_ascii=False, indent=2)

    lines = []
    lines.append("# P3-B 中文检索评测报告（task-17 / L4-01）")
    lines.append("")
    lines.append("- 运行时间：%s" % meta["started_at"])
    lines.append("- 冻结二进制：`%s`" % meta["exe"])
    lines.append("- SHA256：`%s`" % meta["exe_sha256"])
    lines.append("- 服务：%s" % meta["api_url"])
    lines.append("- 数据集：`tests/eval/dataset.json`（%d 条查询）" % metrics["queries_total"])
    lines.append("- 夹具：`tests/eval/fixture.json`（%d 条记录）" % meta["seeded_records"])
    lines.append("")
    lines.append("## 总体指标")
    lines.append("")
    lines.append("| 指标 | 数值 |")
    lines.append("| --- | --- |")
    lines.append("| Recall@5（有答案 %d 条） | %.4f |" % (metrics["queries_with_answer"], metrics["recall_at_5"]))
    lines.append("| MRR（有答案 %d 条） | %.4f |" % (metrics["queries_with_answer"], metrics["mrr"]))
    lines.append("| 跨项目污染率 | %.4f（%d/%d） |"
                 % (metrics["cross_project_pollution_rate"], metrics["cross_project_returned"],
                    metrics["returned_total"]))
    lines.append("| 无答案误召回率 | %.4f（%d/%d） |"
                 % (metrics["no_answer_false_recall_rate"], metrics["no_answer_false_recall"],
                    metrics["no_answer_queries"]))
    lines.append("| 撤回泄漏数（默认检索） | %d |" % metrics["retracted_leaks_default_search"])
    lines.append("| 过期内容误召回 | %d/%d |"
                 % (metrics["stale_outdated_false_recall"], metrics["stale_outdated_queries"]))
    lines.append("| 版本不匹配 | %d |" % metrics["version_violation_count"])
    lines.append("")
    lines.append("## 分类别汇总")
    lines.append("")
    lines.append("| 类别 | 查询数 | 有答案 | Recall@5 | MRR | 命中 | 未命中 | 误召回 | 撤回泄漏 |")
    lines.append("| --- | --- | --- | --- | --- | --- | --- | --- | --- |")
    for category in CATEGORIES:
        stats = per_category.get(category)
        if not stats:
            continue
        lines.append("| %s | %d | %d | %.4f | %.4f | %d | %d | %d | %d |"
                     % (stats["label"], stats["queries"], stats["queries_with_answer"],
                        stats["recall_at_5"], stats["mrr"], stats["hits"], stats["misses"],
                        stats["false_recall"], stats["retracted_returned"]))
        if category == "retracted":
            lines.append("| ↳ include_retracted=true 召回率 | — | — | %.4f | — | — | — | — | — |"
                         % stats.get("include_retracted_recall", 0.0))
    lines.append("")
    lines.append("## 最差 %d 条" % len(report["worst"]))
    lines.append("")
    lines.append("| 查询 id | 类别 | query | 期望 | 实际（前5） | 名次 | 说明 |")
    lines.append("| --- | --- | --- | --- | --- | --- | --- |")
    for row in worst(rows):
        if row["expected_fixture_ids"]:
            note = "命中" if row["hit"] else "未命中（Recall@5=0）"
        else:
            note = "误召回 %d 条" % row["returned_total"] if row["returned_total"] else "正确无结果"
        lines.append("| %s | %s | %s | %s | %s | %s | %s |"
                     % (row["id"], CATEGORY_LABEL[row["category"]], row["query"],
                        ",".join(row["expected_fixture_ids"]) or "（无答案）",
                        ",".join(row["actual_fixture_ids"][:5]) or "（空）",
                        row["rank"] if row["rank"] else "-", note))
    lines.append("")
    lines.append("逐条明细见 `report.json`（含 query/期望/实际/名次与 include_retracted 复核）。")
    with open(os.path.join(out_dir, "report.md"), "w", encoding="utf-8") as handle:
        handle.write("\n".join(lines) + "\n")
    return report


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest().upper()


# --------------------------------------------------------------------------- main

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", default=os.path.join(REPO, "artifacts", "eval", "frozen",
                                                      "graph-memory-service.exe"))
    parser.add_argument("--out-dir", default=os.path.join(REPO, "artifacts", "eval"))
    parser.add_argument("--label", default=None)
    parser.add_argument("--keep-data", action="store_true")
    parser.add_argument("--check-p4", action="store_true", help="Fail below accepted P4 quality floors or on scope/retraction leakage")
    args = parser.parse_args()

    exe = os.path.abspath(args.exe)
    if not os.path.exists(exe):
        raise SystemExit("frozen service binary not found: %s" % exe)
    exe_sha = sha256(exe)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    label = args.label or ("run-" + stamp)
    out_dir = os.path.join(os.path.abspath(args.out_dir), label)
    if os.path.exists(out_dir):
        raise SystemExit("output dir already exists: %s" % out_dir)
    os.makedirs(out_dir)

    with open(os.path.join(HERE, "fixture.json"), "r", encoding="utf-8") as handle:
        fixture = json.load(handle)
    with open(os.path.join(HERE, "dataset.json"), "r", encoding="utf-8") as handle:
        dataset = json.load(handle)

    data_dir = tempfile.mkdtemp(prefix="gm-eval-")
    proc = None
    started_at = time.strftime("%Y-%m-%dT%H:%M:%S%z")
    try:
        out_path = os.path.join(out_dir, "service.stdout.log")
        err_path = os.path.join(out_dir, "service.stderr.log")
        port = free_port()
        with open(out_path, "wb") as stdout_handle, open(err_path, "wb") as stderr_handle:
            proc = subprocess.Popen(
                [exe, "--data-dir", data_dir, "--port", str(port)],
                stdout=stdout_handle, stderr=stderr_handle, cwd=REPO,
            )
        info, host, svc_port = wait_for_service(proc, out_path)

        api = Api(host, svc_port, log_path=os.path.join(out_dir, "http.jsonl"))
        manifest = {"data_dir": data_dir, "service": info, "exe": exe, "exe_sha256": exe_sha}
        evidence = []
        records = seed(api, fixture, manifest, evidence)
        seed_failures = verify_seed(api, fixture, records)
        manifest["seed_verification_failures"] = seed_failures
        with open(os.path.join(out_dir, "seed_manifest.json"), "w", encoding="utf-8") as handle:
            json.dump(manifest, handle, ensure_ascii=False, indent=2)
        with open(os.path.join(out_dir, "seed_evidence.json"), "w", encoding="utf-8") as handle:
            json.dump(evidence, handle, ensure_ascii=False, indent=2)
        if seed_failures:
            raise RuntimeError("seed verification failed: %s" % json.dumps(seed_failures, ensure_ascii=False))

        rows = run_queries(api, dataset, manifest)
        metrics, per_category = summarize(rows)
        meta = {
            "task": "task-17 / L4-01",
            "started_at": started_at,
            "finished_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
            "exe": exe,
            "exe_sha256": exe_sha,
            "exe_size": os.path.getsize(exe),
            "api_url": info.get("api_url"),
            "seeded_records": len(records),
            "data_dir": data_dir,
            "host": platform.node(),
            "platform": platform.platform(),
            "python": sys.version.split()[0],
            "top_k": TOP_K,
            "dataset_sha256": sha256(os.path.join(HERE, "dataset.json")),
            "fixture_sha256": sha256(os.path.join(HERE, "fixture.json")),
        }
        write_report(out_dir, metrics, per_category, rows, meta)
        summary = {"out_dir": out_dir, "metrics": metrics, "seed_failures": seed_failures,
                   "exe_sha256": exe_sha, "per_category": per_category}
        print(json.dumps(summary, ensure_ascii=False, indent=2))
        if args.check_p4:
            failures = []
            for key, minimum in {"recall_at_5": 0.8571428571, "mrr": 0.8469387755}.items():
                if metrics[key] + 1e-9 < minimum:
                    failures.append(f"{key}: {metrics[key]} < {minimum}")
            for key in ("cross_project_returned", "no_answer_false_recall", "retracted_leaks_default_search"):
                if metrics[key] != 0:
                    failures.append(f"{key}: expected 0, got {metrics[key]}")
            if failures:
                raise RuntimeError("P4 quality gate failed: " + "; ".join(failures))

    finally:
        if proc is not None and proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=10)
        if not args.keep_data:
            shutil.rmtree(data_dir, ignore_errors=True)


if __name__ == "__main__":
    main()
