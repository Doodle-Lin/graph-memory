#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P5-A / task-27：模型加载与单次写路径耗时诊断（原始证据）。

做四件事，全部打印真实测量值：
  1. 起服务（`GM_EMBED_MODEL_DIR` 由调用方给定），等 `/api/health`；
  2. 轮询 `GET /api/v1/agent/status` 直到 `semantic.state == ready`，记录**启动到就绪**耗时；
  3. 建项目 + 单条 `POST /api/v1/memories`，记录**真实墙钟耗时**（模型已加载）；
  4. 单条 `POST /api/v1/search`（`semantic=true`，字面为空）记录耗时，并打印建议。

用法：
  python -X utf8 tools/p5_load_probe.py --exe <path> --model-dir <cache root> --out <json>
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, ".."))
sys.path.insert(0, os.path.join(REPO, "tests", "eval"))

import importlib.util

spec = importlib.util.spec_from_file_location(
    "eval_runner", os.path.join(REPO, "tests", "eval", "run_eval.py")
)
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True)
    parser.add_argument("--model-dir", default=None)
    parser.add_argument("--out", required=True)
    parser.add_argument("--ready-timeout", type=float, default=120.0)
    parser.add_argument("--rounds", type=int, default=10,
                        help="空字面语义检索的采样轮数（p50/p95 用）")
    args = parser.parse_args()

    exe = os.path.abspath(args.exe)
    data_dir = tempfile.mkdtemp(prefix="gm-p5-load-")
    environment = dict(os.environ)
    if args.model_dir:
        environment["GM_EMBED_MODEL_DIR"] = os.path.abspath(args.model_dir)
    proc = None
    try:
        stdout_path = os.path.join(data_dir, "service.stdout.log")
        stderr_path = os.path.join(data_dir, "service.stderr.log")
        port = runner.free_port()
        started = time.time()
        with open(stdout_path, "wb") as out_handle, open(stderr_path, "wb") as err_handle:
            proc = subprocess.Popen(
                [exe, "--data-dir", data_dir, "--port", str(port)],
                stdout=out_handle, stderr=err_handle, cwd=os.path.dirname(exe), env=environment,
            )
        info, host, svc_port = runner.wait_for_service(proc, stdout_path)
        api = runner.Api(host, svc_port, log_path=os.path.join(data_dir, "http.jsonl"))

        first_status = api.ok("GET", "/api/v1/agent/status")["semantic"]
        ready_at = None
        first_ready = None
        deadline = time.time() + args.ready_timeout
        while time.time() < deadline:
            status = api.ok("GET", "/api/v1/agent/status")["semantic"]
            if status.get("state") == "ready" and status.get("available_hint") is None:
                # `state` flips to ready as soon as files are present; the session may
                # still be building, so probe a semantic search instead.
                probe = api.ok(
                    "POST", "/api/v1/search",
                    {"query": "模型就绪探测", "scope": "all", "semantic": True},
                )
                if (probe.get("semantic") or {}).get("available"):
                    first_ready = status
                    ready_at = time.time()
                    break
            time.sleep(0.25)
        if ready_at is None:
            raise RuntimeError("model never became usable within %.0fs" % args.ready_timeout)

        project = api.ok("POST", "/api/v1/projects", {"name": "P5-load"})["id"]
        write_started = time.time()
        created = api.ok(
            "POST", "/api/v1/memories",
            {
                "memory_scope": "project",
                "project_id": project,
                "event_id": "p5-load-1",
                "title": "部署窗口",
                "content": "生产环境只允许在每周三 22:00 之后发布",
                "kind": "decision",
                "source": "ops",
                "conditions": "仅生产环境",
                "evidence_type": "user_or_agent_statement",
            },
        )["memory"]
        write_ms = (time.time() - write_started) * 1000.0

        empty_started = time.time()
        empty_search = api.ok(
            "POST", "/api/v1/search",
            {"query": "怎么给多肉植物换盆", "scope": "all", "semantic": True},
        )
        search_ms = (time.time() - empty_started) * 1000.0

        # Contract §4: `include_retracted=true` must also close the semantic channel.
        retracted_search = api.ok(
            "POST", "/api/v1/search",
            {"query": "怎么给多肉植物换盆", "scope": "all", "semantic": True,
             "include_retracted": True},
        )

        samples = []
        for _ in range(args.rounds):
            one = time.time()
            api.ok("POST", "/api/v1/search",
                   {"query": "怎么给多肉植物换盆", "scope": "all", "semantic": True})
            samples.append((time.time() - one) * 1000.0)
        samples.sort()
        import statistics

        payload = {
            "exe": exe,
            "exe_sha256": runner.sha256(exe),
            "model_dir": environment.get("GM_EMBED_MODEL_DIR"),
            "service_start_to_ready_ms": round((ready_at - started) * 1000.0, 1),
            "first_status": first_status,
            "ready_status": first_ready,
            "single_write_ms": round(write_ms, 1),
            "created_id": created["id"],
            "empty_literal_search_ms": round(search_ms, 1),
            "empty_literal_search": {
                "total": empty_search["total"],
                "semantic": empty_search.get("semantic"),
            },
            "include_retracted_semantic": {
                "total": retracted_search["total"],
                "suggestions": len(
                    (retracted_search.get("semantic") or {}).get("memories") or []
                ),
            },
            "empty_literal_semantic_ms": {
                "n": len(samples),
                "p50": round(statistics.median(samples), 2),
                "p95": round(samples[int(len(samples) * 0.95) - 1], 2),
                "max": round(samples[-1], 2),
            },
            "stderr": open(stderr_path, encoding="utf-8", errors="replace").read()[-4000:],
        }
        os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
        with open(args.out, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, ensure_ascii=False, indent=2)
        print("start_to_ready_ms=%s" % payload["service_start_to_ready_ms"])
        print("single_write_ms=%s" % payload["single_write_ms"])
        print("empty_literal_search_ms=%s  p50=%s p95=%s" % (
            payload["empty_literal_search_ms"],
            payload["empty_literal_semantic_ms"]["p50"],
            payload["empty_literal_semantic_ms"]["p95"]))
        print("include_retracted_suggestions=%s" % (
            payload["include_retracted_semantic"]["suggestions"]))
        print("ready_status=%s" % json.dumps(first_ready, ensure_ascii=False))
        print("empty_search_semantic=%s" % json.dumps(empty_search.get("semantic"), ensure_ascii=False))
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
