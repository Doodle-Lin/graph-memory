"""Live Rust desktop API retrieval acceptance runner.

Requires the Tauri HTTP service on localhost:9121 and an isolated seeded test DB.
Unlike the offline contract tests, this calls the real /api/retrieve endpoint.
No assertions about actual agent tool use are made here.
"""
import argparse
import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path

FIXTURE = Path(__file__).parent / "fixtures" / "product_recovery_v1.json"


def retrieve(base_url, query, top_k=5):
    body = json.dumps({"query": query, "top_k": top_k, "spread": True}).encode("utf-8")
    req = urllib.request.Request(
        base_url.rstrip("/") + "/api/retrieve",
        data=body, headers={"Content-Type": "application/json"}, method="POST"
    )
    with urllib.request.urlopen(req, timeout=30) as response:
        payload = json.load(response)
    if not isinstance(payload, list):
        raise ValueError("Expected /api/retrieve to return a list")
    return payload


def evaluate_retrieval(case, results):
    ids = [r.get("id") for r in results]
    expected = set(case["expected_ids"])
    forbidden = set(case.get("forbidden_ids", []))
    return {
        "id": case["id"],
        "expected_found": sorted(expected.intersection(ids)),
        "missing": sorted(expected.difference(ids)),
        "forbidden_found": sorted(forbidden.intersection(ids)),
        "returned_count": len(ids),
        "pass": expected.issubset(ids) and not forbidden.intersection(ids),
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--api", default=os.getenv("GM_API_URL", "http://127.0.0.1:9121"))
    parser.add_argument("--fixture", type=Path, default=FIXTURE)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--allow-unseeded", action="store_true",
                        help="Diagnostic only: print results without enforcing expected IDs")
    args = parser.parse_args()
    cases = json.loads(args.fixture.read_text(encoding="utf-8"))["cases"]
    report = []
    for case in cases:
        try:
            results = retrieve(args.api, case["query"])
            outcome = evaluate_retrieval(case, results)
            outcome["top_titles"] = [r.get("title", "")[:80] for r in results]
            report.append(outcome)
        except (urllib.error.URLError, ValueError, TimeoutError) as exc:
            print(f"ERROR {case['id']}: {exc}", file=sys.stderr)
            return 2
    output = {"endpoint": args.api, "cases": report,
              "warning": "Fixture expected_ids require matching seeded database; this script does not seed it."}
    rendered = json.dumps(output, ensure_ascii=False, indent=2)
    print(rendered)
    if args.output:
        args.output.write_text(rendered + "\n", encoding="utf-8")
    if args.allow_unseeded:
        return 0
    if any(not x["pass"] for x in report):
        print("FAIL: missing/forbidden retrieval IDs. Verify fixture and seeded DB.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
