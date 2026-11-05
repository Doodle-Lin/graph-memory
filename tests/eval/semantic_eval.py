#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P5 (task-31) semantic-recall evaluator.

Drives a real, isolated `graph-memory-service` over real HTTP and measures both
channels separately, so a reported "semantic gain" can be checked against the
literal baseline on exactly the same corpus:

  * literal channel  -> response `memories`
  * semantic channel -> response `semantic.memories`

Modes:
  --mode full    run the corpus with semantic off and on, compare, assert gates
  --mode scan    sweep --threshold-values and print the recall/false-recall curve
  --mode probe   run only the no-recall set at one threshold

Corpus JSON (built by verifier3, never derived from tests/eval):

  {
    "projects": [{"key": "p1", "name": "项目甲"}],
    "records":  [{"key": "r1", "scope": "project", "project": "p1",
                  "title": "...", "content": "...", "conditions": "..."}],
    "queries":  [{"id": "q1", "query": "...", "scope": "project", "project": "p1",
                  "expected": ["r1"]}],
    "no_recall":[{"id": "n1", "query": "...", "scope": "all"}]
  }

Exit code 0 only when the gates hold; otherwise 1 with the offending rows printed.
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
DEFAULT_EXE = os.path.join(REPO, "src-tauri", "target", "debug", "graph-memory-service.exe")
DEFAULT_MODEL_DIR = os.path.join(os.environ.get("LOCALAPPDATA", ""), "GraphMemory", "models")


def load_runner():
    spec = importlib.util.spec_from_file_location("eval_runner", os.path.join(HERE, "run_eval.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Service(object):
    """One isolated service instance over one temporary data directory."""

    def __init__(self, runner, exe, model_dir):
        self.runner = runner
        self.exe = exe
        self.model_dir = model_dir
        self.data_dir = tempfile.mkdtemp(prefix="gm-semantic-")
        self.proc = None
        self.api = None
        self.info = None
        self.stdout = os.path.join(self.data_dir, "service.stdout.log")
        self.stderr = os.path.join(self.data_dir, "service.stderr.log")

    def start(self):
        port = self.runner.free_port()
        env = dict(os.environ)
        if self.model_dir:
            env["GM_EMBED_MODEL_DIR"] = self.model_dir
        with open(self.stdout, "wb") as out, open(self.stderr, "wb") as err:
            self.proc = subprocess.Popen(
                [self.exe, "--data-dir", self.data_dir, "--port", str(port)],
                stdout=out, stderr=err, cwd=os.path.dirname(self.exe), env=env,
            )
        self.info, host, svc_port = self.runner.wait_for_service(self.proc, self.stdout)
        self.api = self.runner.Api(host, svc_port, log_path=os.path.join(self.data_dir, "http.jsonl"))
        return self.info

    def stop(self):
        if self.proc is not None and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=10)

    def cleanup(self):
        self.stop()
        shutil.rmtree(self.data_dir, ignore_errors=True)

    # ---------------------------------------------------------------- api calls
    def status(self):
        try:
            return self.api.ok("GET", "/api/v1/agent/status", None)
        except Exception:
            return {}

    def prepare_model(self):
        """Explicitly ask the service to fetch the model. 404 means the build has
        no endpoint yet; that is reported, not hidden."""
        try:
            return self.api.ok("POST", "/api/v1/models/prepare", {})
        except Exception as exc:  # noqa: BLE001 - surface any failure verbatim
            return {"error": str(exc)}

    def create_project(self, name):
        return self.api.ok("POST", "/api/v1/projects", {"name": name})

    def write_memory(self, record, project_id, index):
        body = {
            "memory_scope": "project" if record.get("scope") == "project" else "personal",
            "event_id": "p5-%s-%d" % (record.get("key", index), index),
            "title": record.get("title", ""),
            "content": record.get("content", ""),
            "kind": record.get("kind", "note"),
        }
        if record.get("conditions"):
            body["conditions"] = record["conditions"]
        if body["memory_scope"] == "project":
            body["project_id"] = project_id
        return self.api.ok("POST", "/api/v1/memories", body)["memory"]["id"]

    def search(self, query, semantic, threshold=None, limit=5, scope_override=None):
        body = {"query": query.get("query", ""), "limit": limit, "semantic": bool(semantic)}
        scope = scope_override or query.get("scope", "all")
        if scope == "project":
            body["scope"] = "project"
            body["project_id"] = query.get("resolved_project")
        elif scope == "personal":
            body["scope"] = "personal"
        else:
            body["scope"] = "all"
        if threshold is not None:
            body["semantic_min_similarity"] = float(threshold)
        body = {k: v for k, v in body.items() if v is not None}
        return self.api.ok("POST", "/api/v1/search", body)


def seed(service, corpus):
    projects = {}
    for project in corpus.get("projects", []):
        projects[project["key"]] = service.create_project(project["name"])["id"]
    ids = {}
    for index, record in enumerate(corpus.get("records", []), start=1):
        project_id = projects.get(record.get("project")) if record.get("scope") == "project" else None
        ids[record["key"]] = service.write_memory(record, project_id, index)
    for query in corpus.get("queries", []):
        query["resolved_project"] = projects.get(query.get("project"))
    return projects, ids


def evaluate(service, corpus, ids, semantic, threshold=None):
    rows = []
    for query in corpus.get("queries", []):
        result = service.search(query, semantic, threshold=threshold)
        literal_ids = [m["id"] for m in result.get("memories", [])]
        sem = result.get("semantic") or {}
        semantic_pairs = [(m.get("id"), m.get("score")) for m in sem.get("memories", [])]
        semantic_ids = [pair[0] for pair in semantic_pairs]
        expected = [ids[key] for key in query.get("expected", []) if key in ids]
        literal_hit = any(mid in literal_ids[:5] for mid in expected)
        semantic_hit = any(mid in semantic_ids for mid in expected)
        rows.append({
            "id": query.get("id"),
            "query": query.get("query"),
            "expected": expected,
            "literal_ids": literal_ids,
            "semantic_ids": semantic_ids,
            "semantic_scores": semantic_pairs,
            "literal_hit": literal_hit,
            "semantic_hit": semantic_hit,
            "combined_hit": literal_hit or semantic_hit,
            "semantic_available": bool(sem.get("available")),
            "semantic_mode": sem.get("mode"),
        })
    return rows


def evaluate_no_recall(service, corpus, semantic, threshold=None):
    rows = []
    for query in corpus.get("no_recall", []):
        query.setdefault("scope", "all")
        result = service.search(query, semantic, threshold=threshold)
        sem = result.get("semantic") or {}
        rows.append({
            "id": query.get("id"),
            "query": query.get("query"),
            "literal_total": result.get("total", 0),
            "literal_titles": [m.get("title") for m in result.get("memories", [])],
            "semantic_total": len(sem.get("memories", [])),
            "semantic_titles": [(m.get("id"), m.get("score")) for m in sem.get("memories", [])],
        })
    return rows


def summarize(rows, no_recall_rows=None):
    total = len(rows) or 1
    literal = sum(1 for r in rows if r["literal_hit"]) / float(total)
    semantic = sum(1 for r in rows if r["semantic_hit"]) / float(total)
    combined = sum(1 for r in rows if r["combined_hit"]) / float(total)
    gains = sum(1 for r in rows if r["semantic_hit"] and not r["literal_hit"])
    summary = {
        "queries": len(rows),
        "literal_recall_at_5": literal,
        "semantic_channel_recall": semantic,
        "combined_recall": combined,
        "semantic_only_gains": gains,
    }
    if no_recall_rows is not None:
        summary["no_recall_queries"] = len(no_recall_rows)
        summary["no_recall_literal_false"] = sum(1 for r in no_recall_rows if r["literal_total"] > 0)
        summary["no_recall_semantic_false"] = sum(1 for r in no_recall_rows if r["semantic_total"] > 0)
    return summary


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", choices=["full", "scan", "probe"], default="full")
    parser.add_argument("--exe", default=DEFAULT_EXE)
    parser.add_argument("--model-dir", default=DEFAULT_MODEL_DIR)
    parser.add_argument("--corpus", required=True)
    parser.add_argument("--out", default=None)
    parser.add_argument("--threshold", type=float, default=0.42)
    parser.add_argument("--threshold-values", default="0.30,0.35,0.40,0.45,0.50,0.55,0.60,0.65,0.70")
    parser.add_argument("--prepare", action="store_true", help="call POST /api/v1/models/prepare first")
    parser.add_argument("--keep-data", action="store_true")
    args = parser.parse_args()

    runner = load_runner()
    with open(args.corpus, encoding="utf-8") as handle:
        corpus = json.load(handle)
    model_dir = os.path.abspath(args.model_dir) if args.model_dir else None

    service = Service(runner, os.path.abspath(args.exe), model_dir)
    payload = {"exe": service.exe, "exe_sha256": runner.sha256(service.exe),
               "corpus": os.path.abspath(args.corpus), "mode": args.mode,
               "model_dir": model_dir}
    status_code = 0
    try:
        service.start()
        if args.prepare:
            payload["prepare"] = service.prepare_model()
        payload["status_semantic"] = (service.status() or {}).get("semantic")
        _, ids = seed(service, corpus)

        if args.mode == "full":
            off_rows = evaluate(service, corpus, ids, semantic=False)
            off_no_recall = evaluate_no_recall(service, corpus, semantic=False)
            on_rows = evaluate(service, corpus, ids, semantic=True, threshold=args.threshold)
            on_no_recall = evaluate_no_recall(service, corpus, semantic=True, threshold=args.threshold)
            off = summarize(off_rows, off_no_recall)
            on = summarize(on_rows, on_no_recall)
            payload.update({"off": off, "on": on, "threshold": args.threshold,
                            "off_rows": off_rows, "on_rows": on_rows,
                            "on_no_recall": on_no_recall})
            gates = {
                "cross_literal_recall_ge_0.80": on["combined_recall"] >= 0.80,
                "gain_over_literal_ge_0.25": (on["combined_recall"] - off["literal_recall_at_5"]) >= 0.25,
                "no_recall_semantic_zero": on["no_recall_semantic_false"] == 0,
                "no_recall_literal_zero": on["no_recall_literal_false"] == 0,
            }
            payload["gates"] = gates
            status_code = 0 if all(gates.values()) else 1
            print("off : " + json.dumps(off, ensure_ascii=False, sort_keys=True))
            print("on  : " + json.dumps(on, ensure_ascii=False, sort_keys=True))
            print("gates: " + json.dumps(gates, ensure_ascii=False, sort_keys=True))
            for row in on_rows:
                if not row["combined_hit"]:
                    print("  MISS %s %s" % (row["id"], row["query"]))
            for row in on_no_recall:
                if row["semantic_total"] > 0:
                    print("  FALSE SEMANTIC %s -> %s" % (row["query"], row["semantic_titles"]))

        elif args.mode == "scan":
            curve = []
            for value in [float(v) for v in args.threshold_values.split(",") if v.strip()]:
                rows = evaluate(service, corpus, ids, semantic=True, threshold=value)
                no_recall = evaluate_no_recall(service, corpus, semantic=True, threshold=value)
                summary = summarize(rows, no_recall)
                summary["threshold"] = value
                curve.append(summary)
                print(json.dumps(summary, ensure_ascii=False, sort_keys=True))
            payload["curve"] = curve

        else:  # probe
            no_recall = evaluate_no_recall(service, corpus, semantic=True, threshold=args.threshold)
            summary = summarize(evaluate(service, corpus, ids, semantic=True, threshold=args.threshold),
                                no_recall)
            payload.update({"probe": summary, "no_recall": no_recall})
            status_code = 0 if summary["no_recall_semantic_false"] == 0 else 1
            print(json.dumps(summary, ensure_ascii=False, sort_keys=True))
    finally:
        if args.keep_data:
            payload["data_dir"] = service.data_dir
            service.stop()
        else:
            service.cleanup()
        if args.out:
            with open(os.path.abspath(args.out), "w", encoding="utf-8") as handle:
                json.dump(payload, handle, ensure_ascii=False, indent=2)
    return status_code


if __name__ == "__main__":
    sys.exit(main())
