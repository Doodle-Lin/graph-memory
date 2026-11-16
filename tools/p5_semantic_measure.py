#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P5-A / task-27：语义通道端到端测量。

只用标准库，复用 `tests/eval/run_eval.py` 的播种与 HTTP 客户端，但**不改动**该文件
（它是官方评测运行器，冻结）。

行为：
  1. 用 `--exe` 起一个隔离服务（数据目录全新临时目录），经真实 v1 HTTP 接口播种官方
     `tests/eval/fixture.json`；
  2. **不传 `semantic` 字段**跑一遍全部 68 条官方查询 → 字面基线；
  3. **传 `semantic=true`**（阈值可覆盖）跑同一批查询 → 语义建议；
  4. 对照契约 §2.1 v1.2/§6.0 断言：
     - `semantic` 对象恒存在，字段齐全；
     - 字面结果集合为空时才有建议；字面有结果时 `semantic.memories == []`；
     - 建议**不进** `total`、不改变 `memories` 的顺序与内容；
     - 建议条目都带 `score`，且 `score >= min_similarity`；
     - `include_retracted=true` 时不产出建议（审计视图）；
     - 30 条官方同形探针在 `semantic=true` 下的建议数必须为 0；
     - 官方 `no_answer` 类查询在 `semantic=true` 下的建议数必须为 0；
  5. 输出原始 JSON（逐条 query/字面 total/建议/score/命中开关）与 stdout 摘要。

用法：
  python -X utf8 tools/p5_semantic_measure.py \
      --exe artifacts/p5-cargo-target/release/graph-memory-service.exe \
      --out artifacts/p5-semantic/measure-semantic.json
"""
import argparse
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, ".."))
EVAL = os.path.join(REPO, "tests", "eval")


def load_runner():
    spec = importlib.util.spec_from_file_location(
        "eval_runner", os.path.join(EVAL, "run_eval.py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def load_json(path):
    with open(path, encoding="utf-8") as handle:
        return json.load(handle)


def is_subsequence(needles, haystack):
    """`needles` must appear in `haystack` in order (ids only)."""
    iterator = iter(haystack)
    return all(any(item == needle for item in iterator) for needle in needles)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--probe-file", default=os.path.join(EVAL, "false_recall_probe.json"))
    parser.add_argument("--dataset", default=os.path.join(EVAL, "dataset.json"))
    parser.add_argument("--fixture", default=os.path.join(EVAL, "fixture.json"))
    parser.add_argument("--min-similarity", type=float, default=None)
    parser.add_argument("--model-dir", default=None)
    args = parser.parse_args()

    runner = load_runner()
    dataset = load_json(args.dataset)
    fixture = load_json(args.fixture)
    probes = load_json(args.probe_file)["probes"]

    exe = os.path.abspath(args.exe)
    data_dir = tempfile.mkdtemp(prefix="gm-p5-measure-")
    environment = dict(os.environ)
    if args.model_dir:
        environment["GM_EMBED_MODEL_DIR"] = os.path.abspath(args.model_dir)
    proc = None
    try:
        out_path = os.path.join(data_dir, "service.stdout.log")
        err_path = os.path.join(data_dir, "service.stderr.log")
        port = runner.free_port()
        with open(out_path, "wb") as stdout_handle, open(err_path, "wb") as stderr_handle:
            proc = subprocess.Popen(
                [exe, "--data-dir", data_dir, "--port", str(port)],
                stdout=stdout_handle,
                stderr=stderr_handle,
                cwd=os.path.dirname(exe),
                env=environment,
            )
        info, host, svc_port = runner.wait_for_service(proc, out_path)
        api = runner.Api(host, svc_port, log_path=os.path.join(data_dir, "http.jsonl"))
        manifest = {"data_dir": data_dir, "service": info, "exe": exe}
        evidence = []
        runner.seed(api, fixture, manifest, evidence)

        status = api.ok("GET", "/api/v1/agent/status")
        semantic_status = status.get("semantic")

        # Wait until the ONNX session is actually usable, otherwise "0 suggestions"
        # could just mean "the model was still loading".
        deadline = time.time() + 120.0
        while time.time() < deadline:
            probe = api.ok(
                "POST", "/api/v1/search",
                {"query": "语义就绪探测", "scope": "all", "semantic": True,
                 "semantic_min_similarity": 0.0},
            )
            if (probe.get("semantic") or {}).get("available"):
                break
            time.sleep(0.25)
        else:
            raise RuntimeError("semantic channel never became available")

        def scope_body(entry, semantic, min_similarity=None):
            """Same scope semantics as the official runner (`build_search_body`)."""
            body = runner.build_search_body(entry)
            if body.get("project_id"):
                body["project_id"] = manifest["projects"][body["project_id"]]
            if semantic:
                body["semantic"] = True
                if min_similarity is not None:
                    body["semantic_min_similarity"] = min_similarity
            return body

        rows = []
        violations = []
        for entry in dataset["queries"]:
            query = entry["query"]
            literal = api.ok("POST", "/api/v1/search", scope_body(entry, False))
            semantic = api.ok("POST", "/api/v1/search", scope_body(entry, True, args.min_similarity))
            block = semantic.get("semantic") or {}
            suggestions = block.get("memories") or []
            literal_ids = [item["id"] for item in literal["memories"]]
            semantic_literal_ids = [item["id"] for item in semantic["memories"]]
            suggestion_fixture_ids = [runner.fixture_id_of(manifest, item["id"]) for item in suggestions]
            expected = entry.get("expected_ids") or []
            row = {
                "id": entry.get("id"),
                "category": entry.get("category"),
                "query": query,
                "literal_total": literal["total"],
                "literal_ids": literal_ids,
                "semantic_total": semantic["total"],
                "semantic_literal_ids": semantic_literal_ids,
                "expected_ids": expected,
                "suggestion_fixture_ids": suggestion_fixture_ids,
                "expected_in_suggestions": bool(expected)
                and any(fid in suggestion_fixture_ids for fid in expected),
                "suggestions": [
                    {"id": item["id"], "title": item.get("title"), "score": item.get("score")}
                    for item in suggestions
                ],
                "available": block.get("available"),
                "mode": block.get("mode"),
                "min_similarity": block.get("min_similarity"),
                "note": block.get("note"),
            }
            rows.append(row)

            if not isinstance(block, dict):
                violations.append({"id": row["id"], "why": "semantic block missing"})
                continue
            if block.get("total") is not None:
                violations.append({"id": row["id"], "why": "semantic leaked into total"})
            if literal["total"] != semantic["total"]:
                violations.append({"id": row["id"], "why": "literal total changed"})
            if literal_ids != semantic_literal_ids:
                violations.append({"id": row["id"], "why": "literal ranking changed"})
            if literal["total"] > 0 and suggestions:
                violations.append({"id": row["id"], "why": "suggestions with non-empty literal"})
            for suggestion in suggestions:
                score = suggestion.get("score")
                if score is None:
                    violations.append({"id": row["id"], "why": "suggestion without score"})
                elif row["min_similarity"] is not None and score < row["min_similarity"] - 1e-9:
                    violations.append(
                        {"id": row["id"], "why": "score below min_similarity", "score": score}
                    )
            if row["category"] == "no_answer" and suggestions:
                violations.append(
                    {"id": row["id"], "why": "no_answer produced suggestions", "n": len(suggestions)}
                )

        probe_rows = []
        for query in probes:
            semantic = api.ok(
                "POST", "/api/v1/search",
                {"query": query, "limit": 5, "scope": "all", "semantic": True,
                 **({} if args.min_similarity is None
                    else {"semantic_min_similarity": args.min_similarity})},
            )
            block = semantic.get("semantic") or {}
            suggestions = block.get("memories") or []
            probe_rows.append(
                {
                    "query": query,
                    "literal_total": semantic["total"],
                    "suggestion_count": len(suggestions),
                    "suggestions": [
                        {"id": item["id"], "score": item.get("score")} for item in suggestions
                    ],
                }
            )
            if suggestions:
                violations.append({"probe": query, "why": "probe produced suggestions"})

        retracted = api.ok(
            "POST",
            "/api/v1/search",
            {**scope_body(dataset["queries"][0], True), "include_retracted": True},
        )
        retracted_block = retracted.get("semantic") or {}

        by_category = {}
        for row in rows:
            bucket = by_category.setdefault(
                row["category"], {"queries": 0, "literal_empty": 0, "suggestions": 0}
            )
            bucket["queries"] += 1
            if row["literal_total"] == 0:
                bucket["literal_empty"] += 1
            bucket["suggestions"] += len(row["suggestions"])

        payload = {
            "exe": exe,
            "exe_sha256": runner.sha256(exe),
            "model_dir": environment.get("GM_EMBED_MODEL_DIR"),
            "min_similarity_override": args.min_similarity,
            "semantic_status": semantic_status,
            "rows": rows,
            "probes": probe_rows,
            "probe_suggestion_total": sum(p["suggestion_count"] for p in probe_rows),
            "include_retracted_suggestions": len(retracted_block.get("memories") or []),
            "per_category": by_category,
            "violations": violations,
            "suggestion_total": sum(len(r["suggestions"]) for r in rows),
            "literal_empty_total": sum(1 for r in rows if r["literal_total"] == 0),
            # Contract §6: how often a literal-empty query gets its expected record as a
            # suggestion. Purely informational — suggestions never affect R@5.
            "empty_literal_queries": sum(
                1 for r in rows if r["literal_total"] == 0 and r["expected_ids"]
            ),
            "empty_literal_suggested_hits": sum(
                1
                for r in rows
                if r["literal_total"] == 0 and r["expected_in_suggestions"]
            ),
        }
        out_path = os.path.abspath(args.out)
        os.makedirs(os.path.dirname(out_path), exist_ok=True)
        with open(out_path, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, ensure_ascii=False, indent=2)

        print("exe=%s sha256=%s" % (exe, payload["exe_sha256"]))
        print("semantic_status=%s" % json.dumps(semantic_status, ensure_ascii=False))
        print("queries=%d literal_empty=%d suggestions=%d" % (
            len(rows), payload["literal_empty_total"], payload["suggestion_total"]))
        for name, bucket in sorted(by_category.items()):
            print("  %-18s queries=%-3d literal_empty=%-3d suggestions=%d" % (
                name, bucket["queries"], bucket["literal_empty"], bucket["suggestions"]))
        print("probes=%d probe_suggestions=%d" % (len(probe_rows), payload["probe_suggestion_total"]))
        print("include_retracted_suggestions=%d" % payload["include_retracted_suggestions"])
        print("empty_literal_queries=%d empty_literal_suggested_hits=%d" % (
            payload["empty_literal_queries"], payload["empty_literal_suggested_hits"]))
        for row in rows:
            if row["literal_total"] == 0 and row["expected_ids"]:
                print("  %-5s %-16s expected=%s suggested=%s top=%s" % (
                    row["id"], row["category"], row["expected_ids"],
                    row["expected_in_suggestions"],
                    [(s["title"], round(s["score"], 4)) for s in row["suggestions"][:2]]))
        print("violations=%d" % len(violations))
        for violation in violations[:20]:
            print("  VIOLATION %s" % json.dumps(violation, ensure_ascii=False))
        return 1 if violations else 0
    finally:
        if proc is not None and proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=10)
        shutil.rmtree(data_dir, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
