#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P5-A / task-27：真实模型下的余弦分布扫描（阈值选择的一手证据）。

在官方 fixture 上播种，然后把 `semantic_min_similarity` 压到 0、`semantic_limit` 拉到 10，
观察**字面为空**的查询到底能拿到多高的余弦：

  * 15 条 paraphrase_hard 中字面为空的那些（本批次真正要救的）；
  * 10 条官方 no_answer（同形探针，建议必须为 0）；
  * 30 条同形探针；
  * 4 条 retracted 中字面为空的。

用法：
  python -X utf8 tools/p5_threshold_scan.py --exe <path> --out <json>
"""
import argparse
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile

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


def wait_ready(runner, api, timeout=120.0):
    """Block until the semantic channel can actually answer (`available == true`)."""
    import time

    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = api.ok(
            "POST", "/api/v1/search",
            {"query": "语义就绪探测", "scope": "all", "semantic": True,
             "semantic_min_similarity": 0.0},
        ).get("semantic")
        if (last or {}).get("available"):
            return last
        time.sleep(0.25)
    raise RuntimeError("semantic channel never became available; last=%s" % last)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--model-dir", default=None)
    args = parser.parse_args()

    runner = load_runner()
    dataset = json.load(open(os.path.join(EVAL, "dataset.json"), encoding="utf-8"))
    fixture = json.load(open(os.path.join(EVAL, "fixture.json"), encoding="utf-8"))
    probes = json.load(open(os.path.join(EVAL, "false_recall_probe.json"), encoding="utf-8"))["probes"]

    exe = os.path.abspath(args.exe)
    data_dir = tempfile.mkdtemp(prefix="gm-p5-scan-")
    environment = dict(os.environ)
    if args.model_dir:
        environment["GM_EMBED_MODEL_DIR"] = os.path.abspath(args.model_dir)
    proc = None
    try:
        stdout_path = os.path.join(data_dir, "service.stdout.log")
        stderr_path = os.path.join(data_dir, "service.stderr.log")
        port = runner.free_port()
        with open(stdout_path, "wb") as out_handle, open(stderr_path, "wb") as err_handle:
            proc = subprocess.Popen(
                [exe, "--data-dir", data_dir, "--port", str(port)],
                stdout=out_handle, stderr=err_handle, cwd=os.path.dirname(exe), env=environment,
            )
        info, host, svc_port = runner.wait_for_service(proc, stdout_path)
        api = runner.Api(host, svc_port, log_path=os.path.join(data_dir, "http.jsonl"))
        manifest = {"data_dir": data_dir, "service": info, "exe": exe}
        runner.seed(api, fixture, manifest, [])
        ready = wait_ready(runner, api)

        def scan(query):
            result = api.ok(
                "POST", "/api/v1/search",
                {
                    "query": query, "scope": "all", "limit": 5,
                    "semantic": True, "semantic_min_similarity": 0.0, "semantic_limit": 10,
                },
            )
            block = result.get("semantic") or {}
            return {
                "literal_total": result["total"],
                "suggestions": [
                    {"id": item["id"], "title": item["title"], "score": item["score"]}
                    for item in (block.get("memories") or [])
                ],
            }

        rows = []
        for entry in dataset["queries"]:
            if entry["category"] not in ("paraphrase_hard", "no_answer", "retracted"):
                continue
            row = scan(entry["query"])
            row.update({"id": entry["id"], "category": entry["category"], "query": entry["query"]})
            row["max_score"] = max((s["score"] for s in row["suggestions"]), default=None)
            target = [s for s in row["suggestions"] if s["title"] == entry.get("expect_title")]
            row["target_score"] = target[0]["score"] if target else None
            rows.append(row)

        probe_rows = []
        for query in probes:
            row = scan(query)
            row.update({"query": query})
            row["max_score"] = max((s["score"] for s in row["suggestions"]), default=None)
            probe_rows.append(row)

        payload = {
            "exe": exe,
            "exe_sha256": runner.sha256(exe),
            "model_dir": environment.get("GM_EMBED_MODEL_DIR"),
            "ready_status": ready,
            "scan_min_similarity": 0.0,
            "scan_limit": 10,
            "rows": rows,
            "probes": probe_rows,
            "probe_max": max((r["max_score"] or 0.0 for r in probe_rows), default=0.0),
            "no_answer_max": max(
                (r["max_score"] or 0.0 for r in rows if r["category"] == "no_answer"), default=0.0
            ),
            "empty_literal_answer_scores": [
                {"id": r["id"], "query": r["query"], "max_score": r["max_score"],
                 "target_score": r["target_score"]}
                for r in rows
                if r["category"] == "paraphrase_hard" and r["literal_total"] == 0
            ],
        }
        os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
        with open(args.out, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, ensure_ascii=False, indent=2)

        print("exe_sha256=%s" % payload["exe_sha256"])
        print("no_answer_max=%s probe_max=%s" % (payload["no_answer_max"], payload["probe_max"]))
        for row in rows:
            print("  %-6s %-16s literal=%-2d max=%s target=%s" % (
                row["id"], row["category"], row["literal_total"],
                None if row["max_score"] is None else round(row["max_score"], 4),
                None if row["target_score"] is None else round(row["target_score"], 4)))
        print("probe maxima: %s" % [None if r["max_score"] is None else round(r["max_score"], 4)
                                    for r in probe_rows])
        return 0
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
