#!/usr/bin/env python3
"""Measure search and per-engine latency against a running rustxng instance.

Every request asks /search?format=json&full=1 and appends a unique suffix to
the query. The response cache is keyed by the request, so the suffix forces a
cold cache and the numbers describe real engine work instead of a cache hit.
Queries are taken from bench/queries.txt in a cycle. The JSON "timings" array
gives the per-engine seconds; the client-measured wall clock gives the overall
request latency.

Usage:
    python3 tools/bench_latency.py --base-url http://127.0.0.1:29483
    python3 tools/bench_latency.py --base-url ... --requests 40 --limit 5
    python3 tools/bench_latency.py --base-url ... --requests 40 --json
"""

from __future__ import annotations

import argparse
import json
import secrets
import statistics
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

DEFAULT_BASE_URL = "http://127.0.0.1:29483"
DEFAULT_QUERIES = Path(__file__).resolve().parent.parent / "bench" / "queries.txt"
USER_AGENT = "rustxng-latency-bench/1.0"
FORMAT = "json"


def percentile(values: list[float], fraction: float) -> float:
    """Nearest-rank percentile, the convention the other bench tools use."""
    if not values:
        return 0.0
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, int((len(ordered) - 1) * fraction + 0.5)))
    return ordered[index]


def stats(values: list[float]) -> dict:
    if not values:
        return {"n": 0, "p50_ms": 0.0, "p95_ms": 0.0, "mean_ms": 0.0, "max_ms": 0.0}
    return {
        "n": len(values),
        "p50_ms": round(percentile(values, 0.50), 1),
        "p95_ms": round(percentile(values, 0.95), 1),
        "mean_ms": round(statistics.fmean(values), 1),
        "max_ms": round(max(values), 1),
    }


def load_queries(path: str, limit: int) -> list[str]:
    try:
        lines = Path(path).read_text(encoding="utf-8").splitlines()
    except OSError as error:
        raise SystemExit(f"cannot read {path}: {error}") from error
    queries = [line.strip() for line in lines if line.strip() and not line.strip().startswith("#")]
    if not queries:
        raise SystemExit(f"no queries in {path}")
    return queries[:limit] if limit else queries


def search(base_url: str, query: str, timeout: float) -> dict:
    """Run one JSON search, returning the wall clock and the engine timings."""
    params = {"q": query, "format": FORMAT, "full": "1"}
    url = base_url.rstrip("/") + "/search?" + urllib.parse.urlencode(params)
    request = urllib.request.Request(
        url, headers={"User-Agent": USER_AGENT, "Accept": "application/json"}
    )
    started = time.perf_counter()
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            body = json.load(response)
    except urllib.error.HTTPError as error:
        detail = error.read()[:200].decode("utf-8", "replace")
        return {
            "millis": (time.perf_counter() - started) * 1000,
            "timings": [],
            "error": f"HTTP {error.code}: {detail}",
        }
    except Exception as error:  # noqa: BLE001 - report whatever happened
        return {
            "millis": (time.perf_counter() - started) * 1000,
            "timings": [],
            "error": f"{type(error).__name__}: {error}",
        }

    millis = (time.perf_counter() - started) * 1000
    timings = []
    for item in body.get("timings") or []:
        try:
            timings.append((str(item.get("engine", "?")), float(item.get("total", 0.0)) * 1000.0))
        except (TypeError, ValueError):
            continue
    return {"millis": millis, "timings": timings, "error": None}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--base-url", default=DEFAULT_BASE_URL, help="running rustxng base URL")
    parser.add_argument("--queries", default=str(DEFAULT_QUERIES), help="query list, one per line")
    parser.add_argument(
        "--requests", type=int, default=5, help="total number of requests (default: 5)"
    )
    parser.add_argument("--limit", type=int, default=0, help="use at most N queries (0 = all)")
    parser.add_argument("--timeout", type=float, default=60.0, help="HTTP timeout in seconds")
    parser.add_argument("--json", action="store_true", help="print machine readable JSON")
    args = parser.parse_args(argv)
    if args.requests < 1:
        parser.error("--requests must be at least 1")
    if args.limit < 0:
        parser.error("--limit cannot be negative")

    queries = load_queries(args.queries, args.limit)
    nonce = secrets.token_hex(4)
    overall: list[float] = []
    per_engine: dict[str, list[float]] = {}
    errors: list[dict] = []

    for counter in range(1, args.requests + 1):
        query = queries[(counter - 1) % len(queries)]
        # The cache key is the request URL: a fresh suffix per request is what
        # makes every sample a cold-cache sample.
        unique = f"{query} z{nonce}{counter:03d}"
        result = search(args.base_url, unique, args.timeout)
        overall.append(result["millis"])
        if result["error"]:
            errors.append({"query": query, "error": result["error"]})
            continue
        for engine, millis in result["timings"]:
            per_engine.setdefault(engine, []).append(millis)

    engines = [{"engine": name, **stats(values)} for name, values in per_engine.items()]
    # Slowest tail first: that is what the latency budget is about.
    engines.sort(key=lambda row: (-row["p95_ms"], row["engine"].lower()))
    report = {
        "base_url": args.base_url,
        "queries": len(queries),
        "requests": len(overall),
        "errors": len(errors),
        "error_samples": errors[:5],
        "overall": stats(overall),
        "engines": engines,
    }

    if args.json:
        json.dump(report, sys.stdout, ensure_ascii=False, indent=2)
        sys.stdout.write("\n")
        return 0

    overall_stats = report["overall"]
    print(f"base URL: {report['base_url']}")
    print(
        f"cold cache: {report['requests']} requests over {report['queries']} queries"
    )
    print(
        f"overall    n={overall_stats['n']}  p50={overall_stats['p50_ms']:.1f} ms  "
        f"p95={overall_stats['p95_ms']:.1f} ms  mean={overall_stats['mean_ms']:.1f} ms"
    )
    print()
    print(f"{'engine':<32} {'n':>5} {'p50 ms':>10} {'p95 ms':>10} {'mean ms':>10}")
    print("-" * 71)
    for row in engines:
        print(
            f"{row['engine']:<32} {row['n']:>5} {row['p50_ms']:>10.1f} "
            f"{row['p95_ms']:>10.1f} {row['mean_ms']:>10.1f}"
        )
    print()
    if errors:
        print(f"request errors: {len(errors)}")
        for error in errors[:5]:
            print(f"  {error['query']}: {error['error']}")
    else:
        print("request errors: 0")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
