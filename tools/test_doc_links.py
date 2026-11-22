"""Every relative link in the shipped Markdown must point at a real path."""
from __future__ import annotations

import re
import unittest
from pathlib import Path
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parent.parent
LINK = re.compile(r"\]\(([^)\s]+)(?:\s+[^)]*)?\)")
SKIP_PREFIX = ("http://", "https://", "mailto:", "#")


def markdown_files() -> list[Path]:
    files = list(ROOT.glob("*.md")) + list((ROOT / "docs").rglob("*.md"))
    return sorted(set(files))


def broken_links(path: Path) -> list[str]:
    text = path.read_text(encoding="utf-8")
    broken = []
    for target in LINK.findall(text):
        if target.startswith(SKIP_PREFIX):
            continue
        clean = unquote(target.split("#", 1)[0])
        if not clean:
            continue
        resolved = (path.parent / clean).resolve()
        if not resolved.exists():
            broken.append(target)
    return broken


class DocumentLinkTests(unittest.TestCase):
    def test_relative_links_resolve(self) -> None:
        offenders = {}
        for path in markdown_files():
            missing = broken_links(path)
            if missing:
                offenders[str(path.relative_to(ROOT))] = missing
        self.assertEqual(offenders, {}, f"broken relative links: {offenders}")


if __name__ == "__main__":
    unittest.main()