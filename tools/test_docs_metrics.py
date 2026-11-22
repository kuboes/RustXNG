from __future__ import annotations

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
NAME = re.compile(r"searxng_[a-z0-9_]+")


def emitted_names() -> set[str]:
    code = (ROOT / "src" / "web" / "handlers.rs").read_text(encoding="utf-8")
    names = set(NAME.findall(code))
    # A format string can leave a bare prefix behind; it is not a series.
    return {name for name in names if not name.endswith("_")}


class ObservabilityDocsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.docs = (ROOT / "docs" / "OBSERVABILITY.md").read_text(encoding="utf-8")
        self.emitted = emitted_names()

    def test_every_emitted_series_is_documented(self) -> None:
        documented = set(NAME.findall(self.docs))
        covered = set(documented)
        # The per-engine family is documented as a wildcard row.
        if "searxng_engines_*" in self.docs:
            covered |= {name for name in self.emitted if name.startswith("searxng_engines_")}
        # A sum/count pair is documented as "searxng_..._sum / _count".
        covered |= {name.replace("_sum", "_count") for name in documented}
        missing = sorted(self.emitted - covered)
        self.assertEqual(missing, [], f"series emitted but not documented: {missing}")

    def test_every_documented_series_exists_in_the_code(self) -> None:
        documented = {name for name in NAME.findall(self.docs) if not name.endswith("_")}
        missing = sorted(name for name in documented if name not in self.emitted)
        self.assertEqual(missing, [], f"documented but never emitted: {missing}")


if __name__ == "__main__":
    unittest.main()