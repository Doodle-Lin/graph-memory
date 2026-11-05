#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P4-C / task-24 永久回归：30 条同形探针不得误召回。

读取 `tests/eval/false_recall_probe.json`，在冻结的 `tests/eval/fixture.json` 上起一个
隔离服务，逐条执行查询并断言 `total == 0`（`expect: no_recall`）。

探针 = 3 个互不相关的短语拼接（与官方 no_answer 类同形）：库里没有任何一条记录同时覆盖
全部主题，正确行为是**一条都不返回**。基线冻结二进制与任何后续实现都必须 0/30。

用法：
    python -X utf8 tests/eval/check_false_recall.py \
        --exe artifacts/p4-cargo-target/debug/graph-memory-service.exe \
        [--out artifacts/p4-recall/false-recall-p4b.json]

退出码 0 表示 0/30，非 0 表示出现误召回（并打印是哪些查询、召回了什么）。
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
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))


def load_runner():
    spec = importlib.util.spec_from_file_location("eval_runner", os.path.join(HERE, "run_eval.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True)
    parser.add_argument("--probe-file", default=os.path.join(HERE, "false_recall_probe.json"))
    parser.add_argument("--fixture", default=os.path.join(HERE, "fixture.json"))
    parser.add_argument("--out", default=None)
    args = parser.parse_args()

    runner = load_runner()
    with open(args.probe_file, encoding="utf-8") as handle:
        spec = json.load(handle)
    probes = spec["probes"]
    with open(args.fixture, encoding="utf-8") as handle:
        fixture = json.load(handle)

    exe = os.path.abspath(args.exe)
    data_dir = tempfile.mkdtemp(prefix="gm-probe-")
    out_dir = os.path.dirname(exe)
    proc = None
    try:
        out_path = os.path.join(data_dir, "service.stdout.log")
        err_path = os.path.join(data_dir, "service.stderr.log")
        port = runner.free_port()
        with open(out_path, "wb") as stdout_handle, open(err_path, "wb") as stderr_handle:
            proc = subprocess.Popen([exe, "--data-dir", data_dir, "--port", str(port)],
                                    stdout=stdout_handle, stderr=stderr_handle, cwd=out_dir)
        info, host, svc_port = runner.wait_for_service(proc, out_path)
        api = runner.Api(host, svc_port, log_path=os.path.join(data_dir, "http.jsonl"))
        manifest = {"data_dir": data_dir, "service": info, "exe": exe}
        evidence = []
        runner.seed(api, fixture, manifest, evidence)
        rows = []
        for query in probes:
            result = api.ok("POST", "/api/v1/search", {"query": query, "limit": 5, "scope": "all"})
            rows.append({
                "query": query,
                "total": result["total"],
                "returned": [item.get("title") for item in result["memories"]],
            })
        false_recall = [row for row in rows if row["total"] > 0]
        payload = {
            "exe": exe,
            "exe_sha256": runner.sha256(exe),
            "probes": len(rows),
            "false_recall_queries": len(false_recall),
            "false_recall_rate": len(false_recall) / len(rows) if rows else 0.0,
            "returned_total": sum(row["total"] for row in rows),
            "rows": rows,
        }
        if args.out:
            with open(os.path.abspath(args.out), "w", encoding="utf-8") as handle:
                json.dump(payload, handle, ensure_ascii=False, indent=2)
        print("%s: %d/%d 误召回 returned_total=%d sha256=%s" % (
            os.path.basename(exe), len(false_recall), len(rows), payload["returned_total"],
            payload["exe_sha256"]))
        for row in false_recall:
            print("  FALSE RECALL %s -> %s" % (row["query"], row["returned"]))
        return 1 if false_recall else 0
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
