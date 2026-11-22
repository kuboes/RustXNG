from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def run_gate(path: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(ROOT / "tools" / "check_bench.py"), path],
        capture_output=True,
        text=True,
    )


class CheckBenchTests(unittest.TestCase):
    def write(self, data: dict) -> str:
        handle = tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8")
        json.dump(data, handle)
        handle.close()
        self.addCleanup(Path(handle.name).unlink)
        return handle.name

    def test_a_healthy_run_passes(self) -> None:
        result = run_gate(self.write({"hit@1": 70, "hit@5": 84, "mrr@10": 0.757, "errors": 0}))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("hit@1=70", result.stdout)

    def test_a_run_below_the_floor_fails(self) -> None:
        result = run_gate(self.write({"hit@1": 49, "errors": 0}))
        self.assertEqual(result.returncode, 1)
        self.assertIn("below the floor", result.stdout)

    def test_failed_requests_fail_the_gate(self) -> None:
        result = run_gate(self.write({"hit@1": 70, "errors": 2}))
        self.assertEqual(result.returncode, 1)
        self.assertIn("2 requests failed", result.stdout)

    def test_a_result_without_hit1_is_treated_as_broken(self) -> None:
        result = run_gate(self.write({"errors": 0}))
        self.assertEqual(result.returncode, 1)
        self.assertIn("below the floor", result.stdout)


if __name__ == "__main__":
    unittest.main()