#!/usr/bin/env python3
"""Score a `local_quality` export: Hit@K, MRR and nDCG without any network."""
from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

from reference_urls import first_hit_rank



def metrics(rows: list[dict]) -> dict:
    hits = {1: 0, 5: 0, 10: 0}
    reciprocal = 0.0
    ndcg = 0.0
    times: list[float] = []
    for row in rows:
        urls = row.get("urls") or []
        gold = row.get("gold") or []
        times.append(float(row.get("millis", 0.0)))
        rank = first_hit_rank(urls, gold)
        if rank is None:
            continue
        for k in hits:
            if rank <= k:
                hits[k] += 1
        reciprocal += 1.0 / rank
        ndcg += 1.0 / math.log2(rank + 1)  # one relevant document, gain 1
    total = len(rows)
    times.sort()

    def percentile(p: float) -> float:
        if not times:
            return 0.0
        return times[min(len(times) - 1, int(round(p / 100 * (len(times) - 1))))]

    return {
        "rows": total,
        "hit@1": hits[1],
        "hit@5": hits[5],
        "hit@10": hits[10],
        "mrr@10": round(reciprocal / total, 4) if total else 0.0,
        "ndcg@10": round(ndcg / total, 4) if total else 0.0,
        "p50_ms": round(percentile(50), 2),
        "p95_ms": round(percentile(95), 2),
    }


def load_rows(path: Path) -> list[dict]:
    data = json.loads(path.read_text(encoding="utf-8"))
    rows = data.get("rows")
    if not isinstance(rows, list):
        raise SystemExit(f"{path}: expected a rows array from examples/local_quality")
    return rows


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path", help="local_quality JSON output")
    parser.add_argument("--baseline", help="earlier output to compare against")
    parser.add_argument("--min-hit5", type=int, default=None, help="fail when Hit@5 is below this")
    parser.add_argument("--json", help="write the metrics here")
    args = parser.parse_args()

    current = metrics(load_rows(Path(args.path)))
    print(json.dumps(current, ensure_ascii=False))
    if args.json:
        Path(args.json).write_text(json.dumps(current, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    if args.baseline:
        before = metrics(load_rows(Path(args.baseline)))
        delta = {key: round(current[key] - before[key], 4) for key in ("hit@1", "hit@5", "hit@10", "mrr@10", "ndcg@10")}
        print(json.dumps({"baseline": before, "delta": delta}, ensure_ascii=False))
    if args.min_hit5 is not None and current["hit@5"] < args.min_hit5:
        print(f"::error::Hit@5 {current['hit@5']} is below the floor {args.min_hit5}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())