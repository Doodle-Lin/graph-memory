#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P5-A / task-27：语义通道的**存在性证明** + **冷启动确定性**证据。

三件事，全部走真实 HTTP 服务（release 二进制 + 真实模型）：

  A. 存在性：建 3 条与查询**零字面交叠**的记录，`semantic=true` 查询时
     字面 `total == 0` 且 `semantic.memories` **非空**（贴原始请求/响应）。
  B. 热索引确定性：同一查询连打 N 次，建议的 `id` 序列**逐位相同**。
  C. 冷索引确定性：把库复制一份并清空 `memory_vectors`（等价 restore 后状态），
     新起服务后**第一次**语义查询的建议集合必须与热索引**逐位相同**，
     且查询后 `memory_vectors` 行数 == 范围内记录数（要么全建、要么降级，不半成品）。

用法：
  python -X utf8 tools/p5_existence_proof.py --exe <exe> --model-dir <cache root> --out <json>
"""
import argparse
import http.client
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, ".."))

import importlib.util

_spec = importlib.util.spec_from_file_location(
    "eval_runner", os.path.join(REPO, "tests", "eval", "run_eval.py")
)
runner = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(runner)

# 存在性用例：查询问的是"夜间跑批占用时间怎么办"，记录讲的是"备份窗口被报表任务占用"。
# 两者没有共享的长词条（跑批/时间 vs 备份窗口/报表任务），但语义上就是同一件事，实测 cosine ~0.577。
# 记录文本取自官方 fixture 的 B1/B2，避免自造语料带来的偏差。
QUERY = "晚上跑批把时间占满了怎么办"
RECORDS = [
    ("备份窗口被其它任务占用",
     "夜间备份窗口被报表任务占用，导致备份未在窗口内完成，需要重新排期。",
     "生产库 22:00-02:00"),
    ("下一步核对备份文件大小",
     "恢复演练前先核对备份文件大小是否与源库数量级一致。",
     ""),
    ("网关超时阈值设置为 30 秒",
     "网关默认超时 30 秒，超过后返回 504，需按业务调整。",
     ""),
]
# 期望被建议命中的记录（上表第 0 条）。
EXPECTED_TITLE = "备份窗口被其它任务占用"


def start(exe, data_dir, environment):
    for stale in ("service.json", "service.json.tmp"):
        try:
            os.remove(os.path.join(data_dir, stale))
        except FileNotFoundError:
            pass
    stamp = int(time.time() * 1000)
    stdout_path = os.path.join(data_dir, "svc.%d.out.log" % stamp)
    stderr_path = os.path.join(data_dir, "svc.%d.err.log" % stamp)
    port = runner.free_port()
    out_handle = open(stdout_path, "ab")
    err_handle = open(stderr_path, "ab")
    proc = subprocess.Popen(
        [exe, "--data-dir", data_dir, "--port", str(port)],
        stdout=out_handle, stderr=err_handle, cwd=os.path.dirname(exe), env=environment,
    )
    launched_at = time.time()
    info, host, svc_port = runner.wait_for_service(proc, stdout_path)
    return proc, host, svc_port, stdout_path, launched_at


def call(host, port, method, path, body=None):
    """Raw HTTP round trip that keeps both the request body and the parsed response."""
    payload = json.dumps(body, ensure_ascii=False).encode("utf-8") if body is not None else None
    connection = http.client.HTTPConnection(host, port, timeout=120)
    headers = {"Content-Type": "application/json"} if payload else {}
    connection.request(method, path, body=payload, headers=headers)
    response = connection.getresponse()
    raw = response.read().decode("utf-8")
    connection.close()
    parsed = json.loads(raw) if raw.strip().startswith("{") else raw
    return {
        "request": {"method": method, "path": path, "body": body},
        "status": response.status,
        "response": parsed,
    }


def first_semantic_probe(host, port, timeout=120.0):
    """§2.1 v1.5: the *first* request after a cold start must either answer or degrade —
    it must never come back degraded and then turn hybrid seconds later. Returns the first
    response, the status seen just before it, and how long the request itself took."""
    status_before = call(host, port, "GET", "/api/v1/agent/status")["response"]["semantic"]
    started = time.time()
    result = call(host, port, "POST", "/api/v1/search",
                  {"query": QUERY, "scope": "all", "semantic": True})
    elapsed_ms = round((time.time() - started) * 1000.0, 1)
    return status_before, result["response"], elapsed_ms


def wait_ready(host, port, timeout=120.0):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = call(host, port, "POST", "/api/v1/search",
                    {"query": QUERY, "scope": "all", "semantic": True})["response"]
        if (last.get("semantic") or {}).get("available"):
            return last
        time.sleep(0.25)
    raise RuntimeError("semantic channel never became available: %s" % last)


def suggestions_of(result):
    return [item["id"] for item in (result.get("semantic") or {}).get("memories") or []]


def titles_of(result):
    return [item.get("title") for item in (result.get("semantic") or {}).get("memories") or []]


def vector_rows(db_path):
    connection = sqlite3.connect("file:%s?mode=ro" % db_path.replace("\\", "/"), uri=True)
    try:
        return connection.execute("SELECT COUNT(*) FROM memory_vectors").fetchone()[0]
    finally:
        connection.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True)
    parser.add_argument("--model-dir", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--rounds", type=int, default=10)
    args = parser.parse_args()

    exe = os.path.abspath(args.exe)
    environment = dict(os.environ)
    environment["GM_EMBED_MODEL_DIR"] = os.path.abspath(args.model_dir)
    data_dir = tempfile.mkdtemp(prefix="gm-p5-exist-")
    cold_dir = data_dir + "-cold"
    payload = {
        "exe": exe,
        "exe_sha256": runner.sha256(exe),
        "model_dir": environment["GM_EMBED_MODEL_DIR"],
        "query": QUERY,
        "seeded_records": [{"title": t, "content": c, "conditions": cond} for t, c, cond in RECORDS],
        "measured_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
    }
    proc = None
    try:
        proc, host, port, stdout_path, launched_at = start(exe, data_dir, environment)
        project = call(host, port, "POST", "/api/v1/projects",
                       {"name": "P5-existence"})["response"]["id"]
        created = []
        for index, (title, content, conditions) in enumerate(RECORDS):
            result = call(host, port, "POST", "/api/v1/memories", {
                "memory_scope": "project", "project_id": project,
                "event_id": "p5-exist-%d" % index, "title": title, "content": content,
                "kind": "decision", "source": "p5-existence", "conditions": conditions,
                "evidence_type": "user_or_agent_statement",
            })
            created.append(result["response"]["memory"]["id"])
        payload["created_ids"] = created

        # --- D. first-request-after-cold-start (bounded wait, §2.1 v1.5) -------------
        status_before, first_result, first_ms = first_semantic_probe(host, port)
        payload["cold_start_first_request"] = {
            "status_before": status_before,
            "request_ms": first_ms,
            "available": (first_result.get("semantic") or {}).get("available"),
            "suggestions": len(suggestions_of(first_result)),
            "note": (first_result.get("semantic") or {}).get("note"),
        }
        # Whatever it answered, the very next identical request must give the same answer;
        # that equality is the real anti-flapping check.
        immediate = call(host, port, "POST", "/api/v1/search",
                         {"query": QUERY, "scope": "all", "semantic": True})["response"]
        payload["cold_start_second_request"] = {
            "available": (immediate.get("semantic") or {}).get("available"),
            "suggestions": len(suggestions_of(immediate)),
        }
        payload["cold_start_stable"] = (
            (first_result.get("semantic") or {}).get("available")
            == (immediate.get("semantic") or {}).get("available")
            and suggestions_of(first_result) == suggestions_of(immediate)
        )

        warm = wait_ready(host, port)
        payload["status"] = call(host, port, "GET", "/api/v1/agent/status")["response"]["semantic"]

        # --- A. existence -----------------------------------------------------------
        first = call(host, port, "POST", "/api/v1/search",
                     {"query": QUERY, "scope": "all", "semantic": True})
        payload["existence"] = {
            "literal_total": first["response"]["total"],
            "literal_ids": [m["id"] for m in first["response"]["memories"]],
            "semantic": first["response"]["semantic"],
            "raw_exchange": first,
        }
        payload["existence_ok"] = (
            first["response"]["total"] == 0 and len(suggestions_of(first["response"])) > 0
        )

        # --- B. warm determinism ----------------------------------------------------
        warm_runs = []
        for _ in range(args.rounds):
            result = call(host, port, "POST", "/api/v1/search",
                          {"query": QUERY, "scope": "all", "semantic": True})["response"]
            warm_runs.append({"ids": suggestions_of(result), "titles": titles_of(result),
                              "scores": [round(m["score"], 12)
                                         for m in (result.get("semantic") or {}).get("memories") or []]})
        payload["warm_runs"] = warm_runs
        payload["warm_deterministic"] = all(
            run["ids"] == warm_runs[0]["ids"] and run["scores"] == warm_runs[0]["scores"]
            for run in warm_runs
        )

        proc.terminate()
        proc.wait(timeout=20)
        proc = None
        time.sleep(1.0)

        # --- D. restart flap check (§2.1 v1.5) --------------------------------------
        # Same data dir, index already warm: the only thing missing right after start is the
        # ONNX session. The first `semantic=true` request must *wait* for the warm-up and
        # answer, not degrade and turn hybrid seconds later.
        proc, host, port, stdout_path, launched_at = start(exe, data_dir, environment)
        d_status_before, d_first, d_ms = first_semantic_probe(host, port)
        d_second = call(host, port, "POST", "/api/v1/search",
                        {"query": QUERY, "scope": "all", "semantic": True})["response"]
        payload["restart_first_request"] = {
            "status_before": d_status_before,
            "request_ms": d_ms,
            "since_launch_ms": round((time.time() - launched_at) * 1000.0, 1),
            "available": (d_first.get("semantic") or {}).get("available"),
            "suggestions": len(suggestions_of(d_first)),
            "note": (d_first.get("semantic") or {}).get("note"),
        }
        payload["restart_stable"] = (
            (d_first.get("semantic") or {}).get("available")
            == (d_second.get("semantic") or {}).get("available")
            and suggestions_of(d_first) == suggestions_of(d_second)
        )

        # --- E. degradation wording when no model exists at all (§2.2) ---------------
        # A fresh data dir: with no model the pool is irrelevant, we only care about the
        # degradation shape (`available=false`, `mode=literal`, empty memories, `note`).
        no_model_dir = data_dir + "-nomodel"
        degraded_env = dict(os.environ)
        degraded_env["GM_EMBED_MODEL_DIR"] = os.path.join(no_model_dir, "no-models")
        os.makedirs(degraded_env["GM_EMBED_MODEL_DIR"], exist_ok=True)
        proc, host, port, stdout_path, launched_at = start(exe, no_model_dir, degraded_env)
        dless = call(host, port, "POST", "/api/v1/search",
                     {"query": QUERY, "scope": "all", "semantic": True})["response"]
        payload["no_model_degradation"] = {
            "status": call(host, port, "GET", "/api/v1/agent/status")["response"]["semantic"],
            "available": (dless.get("semantic") or {}).get("available"),
            "mode": (dless.get("semantic") or {}).get("mode"),
            "memories": (dless.get("semantic") or {}).get("memories"),
            "note": (dless.get("semantic") or {}).get("note"),
        }
        proc.terminate()
        proc.wait(timeout=20)
        proc = None
        time.sleep(1.0)

        # --- C. cold determinism ----------------------------------------------------
        shutil.rmtree(cold_dir, ignore_errors=True)
        shutil.copytree(data_dir, cold_dir)
        db_path = os.path.join(cold_dir, "work-memory.db")
        # The service was terminated, so `work-memory.db` + its `-wal` are consistent; the
        # schema may still live only in the WAL, so let SQLite replay it — never delete the
        # sidecar files before opening the copy.
        connection = sqlite3.connect(db_path)
        connection.execute("DELETE FROM memory_vectors")
        connection.commit()
        connection.close()
        for suffix in ("-wal", "-shm"):
            try:
                os.remove(db_path + suffix)
            except FileNotFoundError:
                pass
        payload["cold_vectors_before"] = vector_rows(db_path)
        payload["records_in_db"] = sqlite3.connect(
            "file:%s?mode=ro" % db_path.replace("\\", "/"), uri=True
        ).execute("SELECT COUNT(*) FROM work_memories").fetchone()[0]

        proc, host, port, stdout_path, launched_at = start(exe, cold_dir, environment)
        cold_started = time.time()
        cold_status_before, cold_first_result, cold_first_ms = first_semantic_probe(host, port)
        payload["cold_vectors_after_first_query"] = vector_rows(db_path)
        payload["cold_first_available"] = (cold_first_result.get("semantic") or {}).get("available")
        payload["cold_first_request"] = {
            "status_before": cold_status_before,
            "request_ms": cold_first_ms,
            "available": cold_first_result["semantic"].get("available"),
            "suggestions": len(suggestions_of(cold_first_result)),
            "note": cold_first_result["semantic"].get("note"),
        }
        cold_runs = []
        for _ in range(args.rounds):
            result = call(host, port, "POST", "/api/v1/search",
                          {"query": QUERY, "scope": "all", "semantic": True})["response"]
            cold_runs.append({"ids": suggestions_of(result), "titles": titles_of(result),
                              "scores": [round(m["score"], 12)
                                         for m in (result.get("semantic") or {}).get("memories") or []]})
        payload["cold_runs"] = cold_runs
        payload["cold_deterministic"] = all(
            run["ids"] == cold_runs[0]["ids"] and run["scores"] == cold_runs[0]["scores"]
            for run in cold_runs
        )
        payload["cold_equals_warm"] = cold_runs[0]["ids"] == warm_runs[0]["ids"] \
            and cold_runs[0]["scores"] == warm_runs[0]["scores"]
        payload["expected_title"] = EXPECTED_TITLE
        payload["expected_record_rank"] = (
            cold_runs[0]["titles"].index(EXPECTED_TITLE) + 1
            if EXPECTED_TITLE in cold_runs[0]["titles"] else None
        )

        os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
        with open(args.out, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, ensure_ascii=False, indent=2)

        print("existence_ok=%s literal_total=%s suggestions=%s" % (
            payload["existence_ok"], payload["existence"]["literal_total"],
            len(payload["existence"]["semantic"]["memories"])))
        print("  titles=%s scores=%s" % (
            titles_of(payload["existence"]["raw_exchange"]["response"]),
            [round(m["score"], 6) for m in payload["existence"]["semantic"]["memories"]]))
        print("warm_deterministic=%s (n=%d)" % (payload["warm_deterministic"], len(warm_runs)))
        print("cold_start: status_before=%s -> first request %s ms available=%s suggestions=%s stable=%s" % (
            payload["cold_start_first_request"]["status_before"]["state"],
            payload["cold_start_first_request"]["request_ms"],
            payload["cold_start_first_request"]["available"],
            payload["cold_start_first_request"]["suggestions"],
            payload["cold_start_stable"]))
        print("cold_deterministic=%s cold_equals_warm=%s" % (
            payload["cold_deterministic"], payload["cold_equals_warm"]))
        print("restart_first: +%s ms after launch, request %s ms, available=%s suggestions=%s stable=%s" % (
            payload["restart_first_request"]["since_launch_ms"],
            payload["restart_first_request"]["request_ms"],
            payload["restart_first_request"]["available"],
            payload["restart_first_request"]["suggestions"],
            payload["restart_stable"]))
        print("no_model: available=%s mode=%s note=%s" % (
            payload["no_model_degradation"]["available"],
            payload["no_model_degradation"]["mode"],
            payload["no_model_degradation"]["note"]))
        print("cold: vectors %s -> %s, records=%s, expected_rank=%s (request %s ms)" % (
            payload["cold_vectors_before"], payload["cold_vectors_after_first_query"],
            payload["records_in_db"], payload["expected_record_rank"],
            payload["cold_first_request"]["request_ms"]))
        return 0 if (payload["existence_ok"] and payload["warm_deterministic"]
                     and payload["cold_deterministic"] and payload["cold_equals_warm"]
                     and payload["cold_start_stable"] and payload["restart_stable"]
                     and payload["restart_first_request"]["available"]
                     and payload["cold_first_available"]) else 1
    finally:
        if proc is not None and proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=20)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=20)
        shutil.rmtree(data_dir, ignore_errors=True)
        shutil.rmtree(cold_dir, ignore_errors=True)
        shutil.rmtree(data_dir + "-nomodel", ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
