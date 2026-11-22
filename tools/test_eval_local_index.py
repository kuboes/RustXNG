from __future__ import annotations

import json
import math
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from eval_local_index import metrics

ROOT = Path(__file__).resolve().parent.parent


def row(rank: int | None, millis: float = 1.0) -> dict:
    urls = [f"https://example.com/other{i}" for i in range(1, 10)]
    gold = ["https://example.com/answer"]
    if rank is not None:
        urls.insert(rank - 1, "https://example.com/answer")
    return {"id": f"r{rank}", "query": "q", "gold": gold, "urls": urls, "millis": millis}


class EvalLocalIndexTests(unittest.TestCase):
    def test_ranks_map_to_hit_at_k_and_mrr(self) -> None:
        result = metrics([row(1), row(3), row(7), row(None)])
        self.assertEqual(result["rows"], 4)
        self.assertEqual(result["hit@1"], 1)
        self.assertEqual(result["hit@5"], 2)
        self.assertEqual(result["hit@10"], 3)
        self.assertAlmostEqual(result["mrr@10"], round((1 + 1 / 3 + 1 / 7) / 4, 4))
        expected = (1 + 1 / math.log2(4) + 1 / math.log2(8)) / 4
        self.assertAlmostEqual(result["ndcg@10"], round(expected, 4))

    def test_directory_references_accept_pages_below_them(self) -> None:
        result = metrics([{
            "id": "dir",
            "query": "q",
            "gold": ["https://docs.example/guide"],
            "urls": ["https://docs.example/guide/install"],
            "millis": 2.0,
        }])
        self.assertEqual(result["hit@1"], 1)

    def test_latency_percentiles_and_empty_input(self) -> None:
        result = metrics([row(1, millis=float(value)) for value in range(1, 101)])
        self.assertEqual(result["p50_ms"], 51.0)
        self.assertGreaterEqual(result["p95_ms"], 95.0)
        self.assertEqual(metrics([])["rows"], 0)
        self.assertEqual(metrics([])["hit@5"], 0)

    def test_cli_reports_and_can_gate_on_hit5(self) -> None:
        handle = tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8")
        json.dump({"rows": [row(1), row(None)]}, handle)
        handle.close()
        self.addCleanup(Path(handle.name).unlink)
        ok = subprocess.run(
            [sys.executable, str(ROOT / "tools" / "eval_local_index.py"), handle.name, "--min-hit5", "1"],
            capture_output=True,
            text=True,
        )
        self.assertEqual(ok.returncode, 0, ok.stderr)
        failed = subprocess.run(
            [sys.executable, str(ROOT / "tools" / "eval_local_index.py"), handle.name, "--min-hit5", "5"],
            capture_output=True,
            text=True,
        )
        self.assertEqual(failed.returncode, 1)
        self.assertIn("below the floor", failed.stderr)


if __name__ == "__main__":
    unittest.main()