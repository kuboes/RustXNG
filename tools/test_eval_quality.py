import contextlib
import io
import json
import os
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import eval_quality


class FailureLatencyTest(unittest.TestCase):
    def test_timeouts_and_instant_failures_remain_in_latency_and_quality(self):
        with tempfile.TemporaryDirectory() as directory:
            questions = Path(directory) / "questions.json"
            report_path = Path(directory) / "report.json"
            questions.write_text(json.dumps({"queries": [
                {"id": str(i), "query": "test", "gold": ["https://example.com/answer"]}
                for i in range(3)
            ]}), encoding="utf-8")
            response = io.BytesIO(json.dumps({"results": [
                {"url": "https://example.com/answer"}
            ]}).encode())
            argv = [
                "eval_quality.py", "--questions", str(questions),
                "--json", str(report_path), "--delay", "0",
            ]
            with (
                patch("sys.argv", argv),
                patch("eval_quality.urllib.request.urlopen", side_effect=[
                    TimeoutError("request timed out"), response, OSError("connection refused"),
                ]),
                patch("eval_quality.time.perf_counter", side_effect=[
                    100.0, 102.0, 200.0, 200.1, 300.0, 300.0,
                ]),
                contextlib.redirect_stdout(io.StringIO()),
            ):
                self.assertEqual(eval_quality.main(), 1)
            report = json.loads(report_path.read_text(encoding="utf-8"))

        self.assertEqual(report["questions"], 3)
        self.assertEqual(report["errors"], 2)
        self.assertEqual(report["hit@1"], 1)
        self.assertEqual(report["hit@5"], 1)
        self.assertEqual(report["mrr@10"], 0.333)
        self.assertEqual(report["median_ms"], 100.0)
        self.assertEqual([row["rank"] for row in report["rows"]], [None, 1, None])
        self.assertEqual(report["rows"][0]["millis"], 2000.0)
        self.assertEqual(report["rows"][2]["millis"], 0.0)
        self.assertIn("TimeoutError", report["rows"][0]["error"])
        self.assertIn("OSError", report["rows"][2]["error"])


# /bin/true exists on Linux but not on macOS or Windows; resolve it once.
TRUE_BINARY = shutil.which("true")
FALSE_BINARY = shutil.which("false")


class RestartInstanceTests(unittest.TestCase):
    def test_a_missing_binary_is_rejected_before_anything_starts(self):
        with self.assertRaises(ValueError) as caught:
            eval_quality.restart_instance(
                "/nonexistent/rustxng", 8905, "http://127.0.0.1:8905"
            )
        self.assertIn("not found", str(caught.exception))

    @unittest.skipUnless(TRUE_BINARY, "no true binary on this platform")
    def test_a_missing_fuser_is_reported_not_ignored(self):
        with patch("eval_quality.shutil.which", return_value=None):
            with self.assertRaises(RuntimeError) as caught:
                eval_quality.restart_instance(
                    TRUE_BINARY, 8905, "http://127.0.0.1:8905"
                )
        self.assertIn("fuser", str(caught.exception))

    @unittest.skipUnless(FALSE_BINARY, "no false binary on this platform")
    def test_a_binary_that_exits_is_reported(self):
        # /bin/false exists and exits immediately; fuser is stubbed out so the
        # test never kills a real process.
        with (
            patch("eval_quality.shutil.which", return_value="/usr/bin/fuser"),
            patch("eval_quality.subprocess.run"),
        ):
            with self.assertRaises(RuntimeError) as caught:
                eval_quality.restart_instance(
                    FALSE_BINARY, 8905, "http://127.0.0.1:8905", attempts=2
                )
        self.assertIn("exited", str(caught.exception))


if __name__ == "__main__":
    unittest.main()
