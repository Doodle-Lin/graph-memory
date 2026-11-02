"""Offline, dependency-free contract tests for product recovery evaluation.

This does not pretend to test the Rust retrieval engine. It checks fixture
integrity and evaluator behavior, and can run in CI without model downloads.
"""
import json
import unittest
from pathlib import Path

FIXTURE = Path(__file__).parent / "fixtures" / "product_recovery_v1.json"


def evaluate(case, returned_ids, answer):
    ids = set(returned_ids)
    expected = set(case["expected_ids"])
    forbidden = set(case.get("forbidden_ids", []))
    missing = sorted(expected - ids)
    forbidden_hits = sorted(forbidden & ids)
    normalized_answer = answer.casefold()
    missing_terms = [
        term for term in case.get("required_terms", [])
        if term.casefold() not in normalized_answer
    ]
    abstained = not answer.strip() or any(
        phrase in normalized_answer
        for phrase in ("insufficient evidence", "not available", "无法确认", "没有足够证据")
    )
    abstention_failed = bool(case.get("must_abstain") and not abstained)
    return {
        "passed": not (missing or forbidden_hits or missing_terms or abstention_failed),
        "missing_ids": missing,
        "forbidden_ids": forbidden_hits,
        "missing_terms": missing_terms,
        "abstention_failed": abstention_failed,
    }


class ProductRecoveryContractTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cases = json.loads(FIXTURE.read_text(encoding="utf-8"))["cases"]

    def test_unique_ids_and_well_formed_cases(self):
        self.assertEqual(len(self.cases), len({case["id"] for case in self.cases}))
        for case in self.cases:
            self.assertTrue(case["query"])
            self.assertFalse(set(case["expected_ids"]) & set(case["forbidden_ids"]))

    def test_positive_case(self):
        case = self.cases[0]
        result = evaluate(case, ["board-zx-8397"], "Use ZX-8397 board.")
        self.assertTrue(result["passed"])

    def test_wrong_identifier_is_rejected(self):
        case = self.cases[0]
        result = evaluate(case, ["board-zx-8797"], "Use ZX-8797 board.")
        self.assertFalse(result["passed"])
        self.assertEqual(result["missing_ids"], ["board-zx-8397"])

    def test_project_recovery_requires_all_context(self):
        case = self.cases[1]
        result = evaluate(case, ["alpha-decision", "alpha-next"], "benchmark retry")
        self.assertFalse(result["passed"])
        self.assertIn("alpha-blocker", result["missing_ids"])

    def test_no_evidence_requires_abstention(self):
        case = self.cases[3]
        self.assertFalse(evaluate(case, [], "The token is abc")["passed"])
        self.assertTrue(evaluate(case, [], "无法确认，没有足够证据")["passed"])


if __name__ == "__main__":
    unittest.main()
