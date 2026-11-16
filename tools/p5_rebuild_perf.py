#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P5-A / task-27：1200 条规模下的重建耗时与检索延迟（真实模型、release 二进制）。

三个阶段，全部记录墙钟时间：

  A. **降级播种**：不设置 `GM_EMBED_MODEL_DIR`（模型不在默认位置）→ 语义通道不可用，
     经真实 v1 HTTP 接口写入 1200 条记录；
  B. **重建**：用同一个数据目录、带上模型重新起服务，反复执行语义检索，直到所有向量
     都补齐（每次检索最多补 `REFRESH_BUDGET` 条），记录总耗时；随后**再跑一整轮**同样的
     检索，验证幂等（第二轮不应再做嵌入）；
  C. **检索延迟**：16 条代表查询 × 若干轮，分别在 `semantic=false` 与 `semantic=true`
     下测 p50/p95。

用法：
  python -X utf8 tools/p5_rebuild_perf.py --exe <release exe> --records 1200 \
      --model-dir <hf-hub cache root> --out artifacts/p5-semantic/rebuild-perf.json
"""
import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, ".."))
EVAL = os.path.join(REPO, "tests", "eval")

import importlib.util

_spec = importlib.util.spec_from_file_location("eval_runner", os.path.join(EVAL, "run_eval.py"))
runner = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(runner)

TOPICS = [
    "端口", "证书", "备份", "日志", "队列", "阈值", "巡检", "排班", "网关", "缓存",
    "发布", "回滚", "索引", "编译", "镜像", "连接池",
]


def start(exe, data_dir, environment):
    # The discovery file from a previous run must go, otherwise `wait_for_service`
    # returns the old port instantly and every request is refused.
    for stale in ("service.json", "service.json.tmp"):
        try:
            os.remove(os.path.join(data_dir, stale))
        except FileNotFoundError:
            pass
    stdout_path = os.path.join(data_dir, "service.%d.stdout.log" % int(time.time() * 1000))
    stderr_path = os.path.join(data_dir, "service.%d.stderr.log" % int(time.time() * 1000))
    port = runner.free_port()
    out_handle = open(stdout_path, "ab")
    err_handle = open(stderr_path, "ab")
    proc = subprocess.Popen(
        [exe, "--data-dir", data_dir, "--port", str(port)],
        stdout=out_handle, stderr=err_handle, cwd=os.path.dirname(exe), env=environment,
    )
    info, host, svc_port = runner.wait_for_service(proc, stdout_path)
    if proc.poll() is not None:
        tail = open(stdout_path, encoding="utf-8", errors="replace").read()[-2000:]
        raise RuntimeError("service exited right after startup: %s" % tail)
    printed = open(stdout_path, encoding="utf-8", errors="replace").read().strip().splitlines()
    print("started pid=%s api=%s:%s (discovery=%s) data_dir=%s" % (
        proc.pid, host, svc_port, info.get("api_url"), data_dir))
    print("  service stdout tail: %s" % (printed[-1] if printed else ""))
    return proc, runner.Api(host, svc_port, log_path=os.path.join(data_dir, "http.jsonl")), stdout_path, stdout_path


def wait_no_handle(runner, db_path, timeout=20.0):
    """The service is force-terminated, so a handle can lag; wait before touching SQLite."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            os.remove(db_path + "-shm")
            os.remove(db_path + "-wal")
            return True
        except FileNotFoundError:
            return True
        except OSError:
            time.sleep(0.25)
    return False


def count_vectors(data_dir):
    """Contract §4: `memory_vectors` lives in the workspace DB, one row per record."""
    db_path = os.path.join(data_dir, "work-memory.db")
    wait_no_handle(None, db_path)
    import sqlite3

    try:
        connection = sqlite3.connect("file:%s?mode=ro" % db_path.replace("\\", "/"), uri=True)
        try:
            return connection.execute("SELECT COUNT(*) FROM memory_vectors").fetchone()[0]
        finally:
            connection.close()
    except sqlite3.Error as error:
        return "error: %s" % error


def count_records(data_dir):
    db_path = os.path.join(data_dir, "work-memory.db")
    import sqlite3

    try:
        connection = sqlite3.connect("file:%s?mode=ro" % db_path.replace("\\", "/"), uri=True)
        try:
            return connection.execute("SELECT COUNT(*) FROM work_memories").fetchone()[0]
        finally:
            connection.close()
    except sqlite3.Error as error:
        return "error: %s" % error


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True)
    parser.add_argument("--records", type=int, default=1200)
    parser.add_argument("--model-dir", default=None)
    parser.add_argument("--out", required=True)
    parser.add_argument("--rounds", type=int, default=8)
    args = parser.parse_args()

    exe = os.path.abspath(args.exe)
    data_dir = tempfile.mkdtemp(prefix="gm-p5-perf-")
    degraded_env = dict(os.environ)
    degraded_env.pop("GM_EMBED_MODEL_DIR", None)
    # Point the default location at an empty directory so "model absent" is deterministic
    # even on a machine that already has the model in %LOCALAPPDATA%.
    absent_root = os.path.join(data_dir, "absent-models")
    os.makedirs(absent_root, exist_ok=True)
    degraded_env["GM_EMBED_MODEL_DIR"] = absent_root
    ready_env = dict(os.environ)
    if args.model_dir:
        ready_env["GM_EMBED_MODEL_DIR"] = os.path.abspath(args.model_dir)

    payload = {
        "exe": exe,
        "exe_sha256": runner.sha256(exe),
        "records": args.records,
        "degraded_model_dir": absent_root,
        "ready_model_dir": ready_env.get("GM_EMBED_MODEL_DIR"),
    }
    proc = None
    try:
        # ---------------------------------------------------------------- A. degraded seed
        proc, api, stdout_path, _ = start(exe, data_dir, degraded_env)
        status = api.ok("GET", "/api/v1/agent/status")["semantic"]
        payload["degraded_status"] = status
        projects = {}
        for key in ("alpha", "beta"):
            projects[key] = api.ok(
                "POST", "/api/v1/projects", {"name": "P5-%s" % key, "directory": "/p5/%s" % key}
            )["id"]
        seed_started = time.time()
        for index in range(args.records):
            topic = TOPICS[index % len(TOPICS)]
            body = {
                "memory_scope": "project",
                "project_id": projects["alpha" if index % 2 == 0 else "beta"],
                "event_id": "p5-perf-%d" % index,
                "title": "%s 记录 %d" % (topic, index),
                "content": "关于%s的第 %d 条运维记录：%s 相关处理步骤与结论。" % (topic, index, topic),
                "kind": "decision" if index % 3 == 0 else "next_step",
                "source": "perf-seed",
                "conditions": "仅 %s 项目" % ("alpha" if index % 2 == 0 else "beta"),
                "evidence_type": "tool_result",
            }
            api.ok("POST", "/api/v1/memories", body)
        payload["degraded_seed_seconds"] = round(time.time() - seed_started, 2)
        payload["degraded_write_ms_per_record"] = round(
            payload["degraded_seed_seconds"] * 1000.0 / args.records, 3
        )
        proc.terminate()
        proc.wait(timeout=20)
        proc = None
        # Wait for the exclusive data-directory lock to be released before restarting.
        time.sleep(1.0)

        # ---------------------------------------------------------------- B. rebuild
        proc, api, stdout_path, _ = start(exe, data_dir, ready_env)
        queries = ["%s 处理步骤" % TOPICS[i % len(TOPICS)] for i in range(16)]
        # §2.1: the semantic channel only runs when the literal hit set is empty, so the
        # rebuild must be measured with a query that shares nothing with the seeded records.
        probe_query = "彗星 咖啡 陶艺"
        # §2.1 v1.5: the first `semantic=true` request *is* the measurement — it must build
        # the whole index inside the budget and answer, not degrade and recover later.
        rebuild_started = time.time()
        first = api.ok("POST", "/api/v1/search",
                       {"query": probe_query, "scope": "all", "semantic": True,
                        "semantic_min_similarity": 0.0, "semantic_limit": 10})
        payload["first_semantic_request_seconds"] = round(time.time() - rebuild_started, 2)
        payload["first_semantic_request"] = {
            "available": (first.get("semantic") or {}).get("available"),
            "suggestions": len((first.get("semantic") or {}).get("memories") or []),
            "note": (first.get("semantic") or {}).get("note"),
        }
        payload["vectors_after_first_request"] = count_vectors(data_dir)

        def semantic_round():
            started = time.time()
            latencies = []
            for query in queries + [probe_query, probe_query]:
                one = time.time()
                api.ok("POST", "/api/v1/search",
                       {"query": query, "scope": "all", "semantic": True,
                        "semantic_min_similarity": 0.0, "semantic_limit": 10})
                latencies.append((time.time() - one) * 1000.0)
            return round((time.time() - started) * 1000.0, 1), [round(x, 2) for x in latencies]

        payload["rebuild_seconds"] = payload["first_semantic_request_seconds"]
        payload["rebuild_passes"] = [{
            "round_ms": round(payload["first_semantic_request_seconds"] * 1000.0, 1),
            "latencies_ms": [round(payload["first_semantic_request_seconds"] * 1000.0, 1)],
            "kind": "cold first request (whole index built before answering)",
        }]
        payload["records_in_db"] = count_records(data_dir)

        # Idempotence: a second identical sweep must do no embedding work, and the answers
        # must be identical to the cold pass (§2.1 v1.4 order-independence).
        warm_first = api.ok("POST", "/api/v1/search",
                            {"query": probe_query, "scope": "all", "semantic": True,
                             "semantic_min_similarity": 0.0, "semantic_limit": 10})
        payload["warm_first_suggestions"] = [
            item["id"] for item in (warm_first.get("semantic") or {}).get("memories") or []
        ]
        payload["cold_first_suggestions"] = [
            item["id"] for item in (first.get("semantic") or {}).get("memories") or []
        ]
        payload["order_independent"] = (
            payload["warm_first_suggestions"] == payload["cold_first_suggestions"]
        )
        second_started = time.time()
        second = []
        for _ in range(max(1, args.records // 200 + 1)):
            total_ms, latencies = semantic_round()
            second.append({"round_ms": total_ms, "latencies_ms": latencies})
        payload["second_sweep_seconds"] = round(time.time() - second_started, 2)
        payload["second_sweep"] = second
        payload["vectors_after_second_sweep"] = count_vectors(data_dir)

        # ---------------------------------------------------------------- C. retrieval p95
        def measure(semantic):
            samples = []
            for _ in range(args.rounds):
                for query in queries:
                    one = time.time()
                    api.ok("POST", "/api/v1/search",
                           {**{"query": query, "scope": "all"},
                            **({"semantic": True, "semantic_min_similarity": 0.0,
                                "semantic_limit": 10} if semantic else {})})
                    samples.append((time.time() - one) * 1000.0)
            samples.sort()
            return {
                "n": len(samples),
                "p50_ms": round(statistics.median(samples), 2),
                "p95_ms": round(samples[int(len(samples) * 0.95) - 1], 2),
                "max_ms": round(samples[-1], 2),
            }

        literal = measure(False)
        hybrid = measure(True)
        payload["retrieval_literal"] = literal
        payload["retrieval_semantic"] = hybrid
        payload["p95_delta_ms"] = round(hybrid["p95_ms"] - literal["p95_ms"], 2)
        payload["p50_delta_ms"] = round(hybrid["p50_ms"] - literal["p50_ms"], 2)

        os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
        with open(args.out, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, ensure_ascii=False, indent=2)

        print("degraded_seed_seconds=%s (%.3f ms/record)" % (
            payload["degraded_seed_seconds"], payload["degraded_write_ms_per_record"]))
        print("rebuild(first semantic request)=%s s | available=%s suggestions=%s | vectors=%s / records=%s" % (
            payload["rebuild_seconds"], payload["first_semantic_request"]["available"],
            payload["first_semantic_request"]["suggestions"],
            payload["vectors_after_first_request"], payload["records_in_db"]))
        print("order_independent=%s | second_sweep_seconds=%s | vectors_after=%s" % (
            payload["order_independent"], payload["second_sweep_seconds"],
            payload["vectors_after_second_sweep"]))
        print("literal p50=%s p95=%s | semantic p50=%s p95=%s | delta p95=%s ms" % (
            literal["p50_ms"], literal["p95_ms"], hybrid["p50_ms"], hybrid["p95_ms"],
            payload["p95_delta_ms"]))
        return 0
    finally:
        if proc is not None and proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=20)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=20)
        shutil.rmtree(data_dir, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
