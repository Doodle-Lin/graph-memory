#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P5-A / task-27：P5 release 与 P4-D 的字面行为逐条核对。

调用方提供两份 `run_eval.py` 的 `report.json`，把所有**随机 UUID / 时间戳**归一化后
逐条比较。用途：证明"新增语义通道后，字面检索逐位不变"。

用法：
  python -X utf8 tools/p5_compare_literal.py \
      --p5 artifacts/p5-semantic/eval-p5-literal/<label>/report.json \
      --p4d artifacts/eval/p4d-1/report.json
"""
import argparse
import json
import sys

# 归一化掉每条运行都不同的字段：UUID 与播种时间。
VOLATILE = ("project_id", "id", "created_at", "updated_at", "generated_at", "data_dir")


def normalize(value):
    if isinstance(value, dict):
        return {k: normalize(v) for k, v in value.items() if k not in VOLATILE}
    if isinstance(value, list):
        return [normalize(v) for v in value]
    return value


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--p5", required=True)
    parser.add_argument("--p4d", required=True)
    args = parser.parse_args()

    p5 = json.load(open(args.p5, encoding="utf-8"))
    p4d = json.load(open(args.p4d, encoding="utf-8"))

    checks = []
    checks.append(("metrics", normalize(p5["metrics"]) == normalize(p4d["metrics"])))
    checks.append(("per_category", normalize(p5["per_category"]) == normalize(p4d["per_category"])))
    checks.append(("queries", normalize(p5["queries"]) == normalize(p4d["queries"])))
    checks.append(("worst", normalize(p5["worst"]) == normalize(p4d["worst"])))

    print("p5  exe_sha256=%s" % p5["meta"].get("exe_sha256"))
    print("p4d exe_sha256=%s" % p4d["meta"].get("exe_sha256"))
    for name, ok in checks:
        print("  %-14s identical=%s" % (name, ok))
    print("metrics: %s" % json.dumps(p5["metrics"], ensure_ascii=False))
    return 0 if all(ok for _, ok in checks) else 1


if __name__ == "__main__":
    sys.exit(main())
