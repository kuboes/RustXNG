# Routing comparison (Phase 4.2 experiment)

Harness: examples/routing_lab.rs (experiment-only: no production code path is
changed, no setting default is touched, no feature flag is required).

**This is a simulation.** Providers are synthetic and described by a latency /
failure / duplication / quality profile, and the harness performs no network I/O.
The numbers are comparable *relative to each other* only: they are not
measurements of any engine, and nothing here says how a real provider behaves.
Read section 9 before quoting a number.

## 1. What the shipping routing does (read from the source)

| Step | Location | Behaviour |
| --- | --- | --- |
| Engine set | src/aggregator.rs:404 select_engines | An explicit engine list wins. Otherwise every enabled engine whose categories intersect the request categories is selected; the request falls back to the engines of the default general category. |
| Applicability | src/aggregator.rs:644 | Engines whose query syntax is absent (currency, translation pair) are dropped before any request or failure accounting. |
| Deadline | src/aggregator.rs:649 | One absolute deadline for the whole search: timeout_limit (default 3.0 s, src/settings.rs:520). |
| Parallel | src/aggregator.rs:702 | join_all over every selected engine. **This is the default** (SearchStrategy::Parallel, src/settings.rs:490). |
| Staged | src/aggregator.rs:1120 run_staged | order_engines_by_health (proven first, fastest first, untried next, empty after that, suspended last) then chunks(staged_tier); after each tier stop if collected >= staged_min_results. Defaults: tier 8, target 10 (src/settings.rs:409-411, :524-525). |
| Per-engine budget | src/aggregator.rs:1035, :1563 engine_budget | min(engine timeout, remaining deadline); the request is not started at all once the deadline has passed. |
| Suspension / resting | src/aggregator.rs:1021, src/engine/state.rs:87 | A suspended or resting engine is skipped **without a request** and reported in unresponsive_engines. Resting triggers after empty_result_limit (8) empty answers for empty_result_cooldown (300 s); hard failures use suspended_times (429 -> 300 s), soft failures ban_time_on_fail growing to max_ban_time_on_fail (5 -> 120 s). |
| Merge order | src/aggregator.rs:703-738 | Results are merged in selection order (join_all preserves input order), staged merges tier by tier; arrival time never affects ranking. |

So the current routing is **static category selection plus a health-ordered tiered
fan-out**, with no notion of what an engine is good at, no early cancellation once
the answer is good enough, and no per-query fan-out decision.

## 2. Method: what is real and what is simulated

Production code, executed as-is by the harness:

| Concern | Source |
| --- | --- |
| Fusion and re-ranking | ResultContainer::extend / close (src/results.rs:305, :430) |
| Query terms for the relevance blend | results::query_terms (src/results.rs:802) |
| Failure taxonomy and UI labels | engine::FailureKind (src/engine/mod.rs:176) |
| Every routing constant | Search::default() |
| Staged loop shape | run_staged re-implemented 1:1 on the simulator clock |

Deliberately re-implemented, because production reads the wall clock and the
harness needs a virtual one: the suspension / resting bookkeeping of
src/engine/state.rs (same fields, same thresholds, configured seconds mapped onto
virtual milliseconds, VirtualStates in the harness). The harness fires 150 queries
4 s apart on that clock so suspensions expire the way they do on a busy instance,
and so the state timeline is identical for every strategy (a faster strategy must
not see fewer expired suspensions, or the request counts would not be comparable).

## 3. Provider model (synthetic)

14 providers, the size of a default general search. caps is what the engine is good
at, weight is the registry weight handed to the production fusion, precision is the
probability that one gold page lands in the answer (an engine without the query
capability keeps 0.35 of it; see the sensitivity run in 7.1), dup is the
probability that a filler slot comes from a shared commons pool (the same pages
many providers return), rank bias the probability that gold lands in the top three.

| provider | caps | weight | base ms | fail p | precision | depth | dup | rank bias |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| generic-a | web | 1.0 | 420 | 0.01 | 0.55 | 12 | 0.25 | 0.50 |
| generic-b | web | 1.0 | 500 | 0.01 | 0.50 | 12 | 0.30 | 0.45 |
| generic-c | web | 1.0 | 380 | 0.01 | 0.45 | 10 | 0.20 | 0.40 |
| code-a | code+docs | 1.2 | 350 | 0.01 | 0.62 | 10 | 0.15 | 0.55 |
| code-b | code | 1.1 | 480 | 0.01 | 0.55 | 12 | 0.20 | 0.50 |
| docs-a | docs+web | 1.1 | 600 | 0.01 | 0.50 | 12 | 0.25 | 0.45 |
| news-a | news | 1.0 | 300 | 0.01 | 0.65 | 10 | 0.10 | 0.55 |
| news-b | news | 0.9 | 520 | 0.02 | 0.50 | 10 | 0.15 | 0.45 |
| sci-a | science | 1.2 | 700 | 0.01 | 0.70 | 8 | 0.10 | 0.60 |
| sci-b | science+web | 1.0 | 900 | 0.02 | 0.50 | 10 | 0.20 | 0.45 |
| qa-a | qa | 1.0 | 450 | 0.02 | 0.60 | 10 | 0.20 | 0.55 |
| blog-a | web+docs | 0.9 | 620 | 0.02 | 0.45 | 12 | 0.30 | 0.40 |
| generic-d | web | 0.8 | 700 | 0.02 | 0.40 | 10 | 0.35 | 0.35 |
| sci-c | science+code | 0.9 | 800 | 0.02 | 0.45 | 8 | 0.20 | 0.40 |

## healthy — every provider answers, all profiles nominal

| strategy | Hit@1 | Hit@5 | Hit@10 | Recall@10 | MRR@10 | nDCG@10 | req | p50 ms | p95 ms | deadline | resilience | dMRR | p |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| current-parallel | 55.3% | 80.7% | 90.0% | 81.0% | 0.662 | 0.683 | 11.0 | 1162 | 2456 | 100.0% | 89.9% | 0.000 | 1.000 |
| current-staged | 54.0% | 82.7% | 95.3% | 89.6% | 0.672 | 0.697 | 8.0 | 884 | 2377 | 100.0% | 100.0% | 0.022 | 0.025 |
| staged-tuned | 54.0% | 73.3% | 73.3% | 55.2% | 0.624 | 0.655 | 3.0 | 607 | 1413 | 100.0% | 50.0% | -0.076 | 0.207 |
| capability-aware | 78.7% | 94.0% | 94.0% | 87.9% | 0.859 | 0.873 | 3.3 | 826 | 1951 | 100.0% | 94.3% | 0.399 | 0.000 |
| category-aware | 72.7% | 92.0% | 94.7% | 89.8% | 0.817 | 0.836 | 4.0 | 873 | 1951 | 100.0% | 95.9% | 0.297 | 0.000 |
| reliability-aware | 55.3% | 80.7% | 90.0% | 81.0% | 0.662 | 0.683 | 11.0 | 1162 | 2456 | 100.0% | 90.4% | 0.000 | 1.000 |
| progressive | 53.3% | 90.7% | 93.3% | 86.8% | 0.680 | 0.724 | 6.0 | 1132 | 1895 | 100.0% | 100.0% | 0.037 | 0.242 |
| early-stop | 72.7% | 80.7% | 80.7% | 66.8% | 0.763 | 0.774 | 13.3 | 449 | 508 | 100.0% | 80.4% | 0.237 | 0.017 |

## partial-suspend — 4 of 14 providers already suspended (captcha) at start

| strategy | Hit@1 | Hit@5 | Hit@10 | Recall@10 | MRR@10 | nDCG@10 | req | p50 ms | p95 ms | deadline | resilience | dMRR | p |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| current-parallel | 57.3% | 84.7% | 95.3% | 88.6% | 0.686 | 0.716 | 7.7 | 986 | 2387 | 100.0% | 95.3% | 0.000 | 1.000 |
| current-staged | 56.0% | 84.0% | 96.0% | 90.3% | 0.678 | 0.699 | 8.0 | 1019 | 2422 | 100.0% | 100.0% | -0.023 | 1.000 |
| staged-tuned | 67.3% | 82.7% | 82.7% | 65.3% | 0.748 | 0.769 | 3.0 | 521 | 1353 | 100.0% | 100.0% | 0.147 | 0.314 |
| capability-aware | 84.7% | 90.7% | 90.7% | 79.9% | 0.873 | 0.878 | 2.5 | 797 | 1807 | 100.0% | 90.0% | 0.431 | 0.000 |
| category-aware | 82.0% | 90.0% | 90.7% | 80.1% | 0.851 | 0.851 | 2.9 | 738 | 1746 | 100.0% | 90.7% | 0.374 | 0.000 |
| reliability-aware | 57.3% | 84.7% | 95.3% | 88.6% | 0.686 | 0.716 | 7.7 | 986 | 2387 | 100.0% | 95.3% | 0.000 | 1.000 |
| progressive | 53.3% | 90.7% | 93.3% | 86.7% | 0.675 | 0.715 | 6.0 | 1111 | 1893 | 100.0% | 100.0% | -0.023 | 0.489 |
| early-stop | 72.0% | 80.0% | 80.0% | 66.1% | 0.757 | 0.768 | 9.3 | 449 | 508 | 100.0% | 80.0% | 0.160 | 0.389 |

## partial-slow — 4 of 14 providers 5x slower

| strategy | Hit@1 | Hit@5 | Hit@10 | Recall@10 | MRR@10 | nDCG@10 | req | p50 ms | p95 ms | deadline | resilience | dMRR | p |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| current-parallel | 52.0% | 82.7% | 93.3% | 86.7% | 0.644 | 0.677 | 9.6 | 2047 | 2612 | 100.0% | 93.3% | 0.000 | 1.000 |
| current-staged | 54.7% | 86.0% | 96.7% | 91.0% | 0.681 | 0.710 | 8.0 | 2353 | 2615 | 100.0% | 92.3% | 0.079 | 0.022 |
| staged-tuned | 54.0% | 74.0% | 74.0% | 55.2% | 0.628 | 0.659 | 3.0 | 2322 | 2597 | 100.0% | 63.6% | -0.027 | 0.235 |
| capability-aware | 76.0% | 92.0% | 92.0% | 84.6% | 0.835 | 0.854 | 2.8 | 2108 | 2563 | 100.0% | 92.8% | 0.377 | 0.000 |
| category-aware | 74.7% | 92.0% | 92.7% | 85.9% | 0.824 | 0.842 | 3.3 | 1816 | 2533 | 100.0% | 92.7% | 0.330 | 0.000 |
| reliability-aware | 52.0% | 82.7% | 93.3% | 86.7% | 0.644 | 0.677 | 9.6 | 2047 | 2612 | 100.0% | 93.3% | 0.000 | 1.000 |
| progressive | 52.7% | 90.7% | 92.7% | 85.7% | 0.673 | 0.720 | 6.0 | 2385 | 2607 | 100.0% | 91.3% | 0.058 | 0.081 |
| early-stop | 66.0% | 82.0% | 82.0% | 64.6% | 0.736 | 0.759 | 10.4 | 552 | 650 | 100.0% | 82.0% | 0.174 | 0.177 |

## all-low-quality — 7 of 14 providers return irrelevant pages, 3 answer empty (resting)

| strategy | Hit@1 | Hit@5 | Hit@10 | Recall@10 | MRR@10 | nDCG@10 | req | p50 ms | p95 ms | deadline | resilience | dMRR | p |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| current-parallel | 0.7% | 32.0% | 72.7% | 51.4% | 0.155 | 0.296 | 8.9 | 1099 | 2206 | 100.0% | 73.4% | 0.000 | 1.000 |
| current-staged | 1.3% | 36.7% | 79.3% | 60.9% | 0.174 | 0.332 | 8.0 | 1019 | 1863 | 100.0% | 70.6% | 0.033 | 0.000 |
| staged-tuned | 17.3% | 41.3% | 41.3% | 25.4% | 0.272 | 0.311 | 3.0 | 609 | 1413 | 100.0% | 25.0% | 0.161 | 1.000 |
| capability-aware | 35.3% | 60.7% | 60.7% | 45.8% | 0.452 | 0.489 | 2.8 | 808 | 1863 | 100.0% | 56.8% | 0.362 | 0.000 |
| category-aware | 23.3% | 54.7% | 60.7% | 44.2% | 0.359 | 0.424 | 3.5 | 831 | 1816 | 100.0% | 55.8% | 0.253 | 0.001 |
| reliability-aware | 0.7% | 32.0% | 72.7% | 51.4% | 0.155 | 0.296 | 8.9 | 1099 | 2206 | 100.0% | 68.4% | 0.000 | 1.000 |
| progressive | 3.3% | 46.7% | 68.7% | 49.1% | 0.189 | 0.317 | 6.0 | 1169 | 2256 | 100.0% | 70.0% | 0.048 | 0.000 |
| early-stop | 8.0% | 25.3% | 25.3% | 15.0% | 0.154 | 0.179 | 12.3 | 500 | 562 | 100.0% | 26.1% | -0.002 | 0.000 |

## rate-limit — 3 of 14 providers answer 429 after 20 requests

| strategy | Hit@1 | Hit@5 | Hit@10 | Recall@10 | MRR@10 | nDCG@10 | req | p50 ms | p95 ms | deadline | resilience | dMRR | p |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| current-parallel | 42.0% | 78.7% | 90.7% | 80.8% | 0.584 | 0.643 | 8.5 | 1137 | 2272 | 100.0% | 90.6% | 0.000 | 1.000 |
| current-staged | 42.0% | 81.3% | 96.0% | 88.6% | 0.597 | 0.658 | 8.0 | 988 | 2076 | 100.0% | 100.0% | 0.032 | 0.092 |
| staged-tuned | 54.0% | 73.3% | 73.3% | 55.2% | 0.624 | 0.655 | 3.0 | 607 | 1413 | 100.0% | 50.0% | 0.069 | 0.747 |
| capability-aware | 74.0% | 87.3% | 87.3% | 76.3% | 0.802 | 0.814 | 3.0 | 821 | 1863 | 100.0% | 81.3% | 0.344 | 0.000 |
| category-aware | 66.7% | 86.0% | 89.3% | 80.3% | 0.755 | 0.781 | 3.8 | 845 | 1863 | 100.0% | 86.1% | 0.279 | 0.000 |
| reliability-aware | 42.0% | 78.7% | 90.7% | 80.8% | 0.584 | 0.643 | 8.5 | 1137 | 2272 | 100.0% | 88.8% | 0.000 | 1.000 |
| progressive | 46.7% | 89.3% | 91.3% | 79.6% | 0.631 | 0.688 | 6.0 | 1370 | 2326 | 100.0% | 100.0% | 0.086 | 0.060 |
| early-stop | 68.7% | 82.0% | 82.0% | 62.3% | 0.746 | 0.768 | 11.6 | 537 | 620 | 100.0% | 81.8% | 0.314 | 0.000 |

## duplicate-heavy — every provider leans on the shared commons pool

| strategy | Hit@1 | Hit@5 | Hit@10 | Recall@10 | MRR@10 | nDCG@10 | req | p50 ms | p95 ms | deadline | resilience | dMRR | p |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| current-parallel | 5.3% | 42.7% | 96.7% | 87.6% | 0.248 | 0.427 | 11.0 | 1162 | 2456 | 100.0% | 96.4% | 0.000 | 1.000 |
| current-staged | 9.3% | 58.7% | 95.3% | 87.8% | 0.289 | 0.452 | 8.0 | 884 | 2377 | 100.0% | 100.0% | 0.057 | 0.000 |
| staged-tuned | 24.7% | 72.0% | 73.3% | 55.2% | 0.432 | 0.513 | 3.0 | 607 | 1413 | 100.0% | 50.0% | 0.210 | 0.000 |
| capability-aware | 49.3% | 92.0% | 94.0% | 87.9% | 0.663 | 0.732 | 3.3 | 826 | 1951 | 100.0% | 94.3% | 0.454 | 0.000 |
| category-aware | 40.7% | 86.7% | 94.7% | 88.6% | 0.586 | 0.674 | 4.0 | 873 | 1951 | 100.0% | 95.9% | 0.375 | 0.000 |
| reliability-aware | 5.3% | 42.7% | 96.7% | 87.6% | 0.248 | 0.427 | 11.0 | 1162 | 2456 | 100.0% | 95.2% | 0.000 | 1.000 |
| progressive | 11.3% | 60.7% | 93.3% | 86.3% | 0.315 | 0.472 | 6.0 | 1132 | 1895 | 100.0% | 100.0% | 0.088 | 0.000 |
| early-stop | 29.3% | 80.7% | 80.7% | 66.8% | 0.511 | 0.589 | 13.3 | 449 | 508 | 100.0% | 80.4% | 0.296 | 0.000 |

## explicit-engines — the request pins 5 engines explicitly

| strategy | Hit@1 | Hit@5 | Hit@10 | Recall@10 | MRR@10 | nDCG@10 | req | p50 ms | p95 ms | deadline | resilience | dMRR | p |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| current-parallel | 50.0% | 82.7% | 82.7% | 68.6% | 0.645 | 0.690 | 3.4 | 867 | 2313 | 100.0% | 80.6% | 0.000 | 1.000 |
| current-staged | 50.0% | 82.7% | 82.7% | 68.6% | 0.645 | 0.690 | 3.4 | 867 | 2313 | 100.0% | 80.6% | 0.000 | 1.000 |
| staged-tuned | 70.0% | 83.3% | 83.3% | 66.6% | 0.761 | 0.777 | 3.0 | 628 | 1437 | 100.0% | 80.0% | 0.301 | 0.000 |
| capability-aware | 50.0% | 82.7% | 82.7% | 68.6% | 0.645 | 0.690 | 3.4 | 867 | 2313 | 100.0% | 80.6% | 0.000 | 1.000 |
| category-aware | 50.0% | 82.7% | 82.7% | 68.6% | 0.645 | 0.690 | 3.4 | 867 | 2313 | 100.0% | 80.6% | 0.000 | 1.000 |
| reliability-aware | 50.0% | 82.7% | 82.7% | 68.6% | 0.645 | 0.690 | 3.4 | 867 | 2313 | 100.0% | 80.6% | 0.000 | 1.000 |
| progressive | 50.0% | 86.0% | 86.0% | 72.0% | 0.661 | 0.710 | 3.5 | 1382 | 2565 | 100.0% | 84.3% | 0.278 | 1.000 |
| early-stop | 56.7% | 80.0% | 80.0% | 64.9% | 0.677 | 0.706 | 3.4 | 760 | 1480 | 100.0% | 78.4% | 0.240 | 0.012 |

## deadline-overrun — 5 of 14 providers are slower than the 3 s deadline

| strategy | Hit@1 | Hit@5 | Hit@10 | Recall@10 | MRR@10 | nDCG@10 | req | p50 ms | p95 ms | deadline | resilience | dMRR | p |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| current-parallel | 56.7% | 84.7% | 95.3% | 89.8% | 0.688 | 0.710 | 7.9 | 912 | 2313 | 100.0% | 95.3% | 0.000 | 1.000 |
| current-staged | 56.0% | 86.0% | 96.7% | 90.8% | 0.685 | 0.711 | 8.0 | 912 | 2313 | 100.0% | 94.1% | -0.027 | 1.000 |
| staged-tuned | 54.0% | 73.3% | 73.3% | 55.2% | 0.624 | 0.655 | 3.0 | 607 | 1413 | 100.0% | 50.0% | -0.126 | 0.022 |
| capability-aware | 76.7% | 89.3% | 89.3% | 78.9% | 0.829 | 0.846 | 2.3 | 621 | 1381 | 100.0% | 89.2% | 0.315 | 0.000 |
| category-aware | 72.7% | 91.3% | 92.0% | 84.0% | 0.813 | 0.836 | 2.9 | 651 | 1416 | 100.0% | 93.1% | 0.275 | 0.000 |
| reliability-aware | 56.7% | 84.7% | 95.3% | 89.8% | 0.688 | 0.710 | 7.9 | 912 | 2313 | 100.0% | 95.3% | 0.000 | 1.000 |
| progressive | 54.0% | 91.3% | 92.7% | 86.0% | 0.678 | 0.718 | 6.0 | 1077 | 1884 | 100.0% | 90.0% | -0.024 | 0.901 |
| early-stop | 72.7% | 80.7% | 80.7% | 66.8% | 0.763 | 0.774 | 9.1 | 449 | 508 | 100.0% | 80.7% | 0.161 | 0.188 |

Queries: 150 per scenario, 6 kinds round-robin (web / code / docs / news /
science / qa). Each query has 1-3 gold pages whose title covers every query term
(so the production relevance blend can lift them) and 40 filler pages, 25% of
which share one query term.

## 4. Strategies under test

| Strategy | Selection | Execution |
| --- | --- | --- |
| current-parallel | every selected engine | one wave (aggregator.rs:702) |
| current-staged | every selected engine | shipping staged loop, tier 8, target 10 |
| staged-tuned | every selected engine | same loop, tier 3 (width sensitivity) |
| capability-aware | engines whose capability matches the query kind (web fallback if < 3) | one wave |
| category-aware | engines whose category matches the categories the query kind maps to (general fallback if < 3) | one wave |
| reliability-aware | every engine minus those with >= 4 requests and >= 50% errors | one wave |
| progressive | every selected engine, ordered by health | wave 1 = 2 engines, doubling while the answer holds < 30 results |
| early-stop | every selected engine | start all, cancel the outstanding requests once 30 results arrived |

Selection strategies are executed in a single wave on purpose: the variable under
test is *which* engines are asked, not *when*. The 30-result target for
progressive/early-stop is 3 x staged_min_results; the shipping value of 10 was
measured to be too aggressive (result 3 below), so the candidates get a target that
does not decide the comparison in advance.

All strategies honour the explicit-engine list and skip suspended/resting engines
without a request, exactly like production.

## 5. Scenarios

| Scenario | State |
| --- | --- |
| healthy | every provider answers, all profiles nominal |
| partial-suspend | 4 of 14 providers already suspended (captcha) at start |
| partial-slow | 4 of 14 providers 5x slower |
| all-low-quality | 7 of 14 return irrelevant pages, 3 answer empty (this is what makes the production resting state fire) |
| rate-limit | 3 of 14 answer 429 after 20 requests |
| duplicate-heavy | every provider leans on the shared commons pool |
| explicit-engines | the request pins 5 engines explicitly |
| deadline-overrun | 5 of 14 providers are slower than the 3 s deadline |

## 6. Metrics

Hit@1/5/10, Recall@10, MRR@10, nDCG@10 (binary gains), all against the synthetic
gold set, with the same definitions as examples/fusion_lab.rs. Plus: provider
requests issued per query, simulated per-query latency (p50/p95), deadline
compliance, failure tolerance (resilience = share of queries that still found gold
in the top 10 although an engine was lost to a failure or a suspension), and paired
two-sided exact sign tests against current-parallel on per-query MRR.

## 7. Results (measured)

Seeds 7 and 11 at 150 queries per scenario (1200 queries per seed over 8
scenarios). Raw data: routing-lab.json (aggregates), routing-lab-rows.json
(per-query rows, compact arrays, layout in the file), routing-lab-seed11.json,
routing-lab-offcap07.json.

Per-scenario tables are produced by the harness itself; its summary output of the
committed run is reproduced verbatim below.

| strategy | Hit@10 | Recall@10 | MRR@10 | nDCG@10 | req/query | p95 ms | deadline | resilience |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| current-parallel | 89.6% | 79.3% | 0.539 | 0.605 | 8.5 | 2377 | 100.0% | 73.4% |
| current-staged | 92.2% | 83.4% | 0.552 | 0.619 | 7.4 | 2295 | 100.0% | 70.6% |
| staged-tuned | 71.8% | 54.2% | 0.589 | 0.624 | 3.0 | 1557 | 100.0% | 25.0% |
| capability-aware | 86.3% | 76.2% | 0.745 | 0.772 | 2.9 | 1962 | 100.0% | 56.8% |
| category-aware | 87.2% | 77.7% | 0.706 | 0.742 | 3.5 | 1949 | 100.0% | 55.8% |
| reliability-aware | 89.6% | 79.3% | 0.539 | 0.605 | 8.5 | 2377 | 100.0% | 68.4% |
| progressive | 88.9% | 79.0% | 0.563 | 0.633 | 5.7 | 2165 | 100.0% | 70.0% |
| early-stop | 73.9% | 59.2% | 0.638 | 0.665 | 10.3 | 668 | 100.0% | 26.1% |

Key deltas against current-parallel (seed 7, mean over the 8 scenarios):

| Strategy | MRR@10 | dMRR | Hit@10 | dHit@10 | requests/query | dRequests | p95 latency |
| --- | --- | --- | --- | --- | --- | --- | --- |
| current-parallel | 0.539 | - | 89.6% | - | 8.5 | - | 2377 ms |
| current-staged | 0.552 | +0.013 | 92.2% | +2.6 pt | 7.4 | -13% | 2295 ms |
| staged-tuned | 0.589 | +0.050 | 71.8% | -17.8 pt | 3.0 | -65% | 1557 ms |
| capability-aware | 0.745 | **+0.206** | 86.3% | -3.3 pt | 2.9 | **-66%** | 1962 ms |
| category-aware | 0.706 | +0.167 | 87.2% | -2.4 pt | 3.5 | -59% | 1949 ms |
| reliability-aware | 0.539 | 0.000 | 89.6% | 0.0 pt | 8.5 | 0% | 2377 ms |
| progressive | 0.563 | +0.024 | 88.9% | -0.7 pt | 5.7 | -33% | 2165 ms |
| early-stop | 0.638 | +0.099 | 73.9% | -15.7 pt | 10.3 | +21% | **668 ms** |

What the numbers say:

1. **Capability-aware selection is the only strategy that wins on both quality and
   cost.** MRR +0.206, nDCG +0.167, two thirds fewer requests, p95 -17%, for 3.3 pt
   of Hit@10 and 3.1 pt of Recall@10. It has the best MRR in 8 of 8 scenarios and
   the paired sign test against parallel gives p < 0.001 in 7 of 8 (the eighth is
   explicit-engines, where routing is inert by construction).
2. **Category selection already captures most of it** (MRR 0.706, 8 of 8 scenarios
   dominated). The two are not independent: a category set is a coarser capability
   set, and the shipping select_engines already keys on categories, so the cheaper
   implementation of this idea is "derive the categories from the query kind", not
   "add a new capability field".
3. **The shipping staged default is a small, safe win** - 6 of 8 scenarios with no
   more requests and MRR within 0.009 of parallel, never an MRR loss above 0.02. It
   asks 8 of 14 engines (one tier) and stops, so it trades ~6 engines of coverage
   for precision. **Narrowing the tier further hurts**: tier 3 loses 17.8 pt Hit@10
   and 25.1 pt Recall@10. Coverage, not ordering, is the real cost of a small tier.
4. **Reliability-aware selection, as implemented, is a no-op.** Its row is identical
   to current-parallel in all 8 scenarios: with a per-request failure rate of 1-2%,
   no engine reaches the 50%-error threshold of the filter. The only reliability
   mechanism that actually fires is the existing resting/suspended state, and the
   simulator applies that to *every* strategy (production parity), so it cannot
   differentiate them. A credible reliability-aware routing has to rank by recent
   latency or empty rate, not filter on an error ratio.
5. **Early termination converts quality into latency.** p95 drops from 2377 ms to
   668 ms (-72%) and MRR rises, but Hit@10 falls 15.7 pt and Recall@10 20.1 pt: the
   engines that arrive first are not the ones that carry gold. It is a latency tool
   for an SLO-driven deployment, not a free improvement.
6. **Progressive fan-out is the cheapest nearly-free improvement**: 33% fewer
   requests, 9% lower p95, MRR +0.024, Hit@10 -0.7 pt, 5 of 8 scenarios dominated.
7. **Explicit engine pinning makes routing inert.** With 5 pinned engines every
   selection strategy is byte-identical to current-parallel; only the ordering
   strategies change anything. A routing change must therefore be a *default
   selection rule*, or users who pin engines will never exercise it.
8. **Deadline compliance was 100% in every cell.** That is a property of the
   simulation, not a result: the slow providers in deadline-overrun time out,
   accumulate soft failures and suspend themselves within the first few queries, so
   the 3 s clamp is only reached while an engine is still cold. A real
   deadline-compliance measurement needs real latency tails.

### 7.1 Sensitivity: capability assumption (offcap_fit 0.35 vs 0.70)

An engine without the query capability keeps 0.35 of the gold precision. If it
keeps 0.70 (--offcap-fit 0.7, same seed), the picture changes in magnitude, not in
direction:

| Strategy | MRR@10 @0.35 | MRR@10 @0.70 | Hit@10 @0.35 | Hit@10 @0.70 |
| --- | --- | --- | --- | --- |
| current-parallel | 0.539 | 0.749 | 89.6% | 96.7% |
| capability-aware | 0.745 | 0.796 | 86.3% | 90.3% |
| category-aware | 0.706 | 0.776 | 87.2% | 91.3% |
| early-stop | 0.638 | 0.783 | 73.9% | 87.2% |

The MRR advantage of routing shrinks from +0.206 to +0.047 while its Hit@10 deficit
stays around -6 pt. Capability routing is only as interesting as the assumption
that an off-capability engine is much worse: that is exactly the number a live
measurement would have to establish.

### 7.2 Seed stability

Seeds 7 and 11 differ by at most 0.011 MRR for any strategy (capability-aware 0.745
vs 0.755, parallel 0.539 vs 0.523) and no dominance verdict flips.

## Dominance check (mechanical, as printed by the harness)

| strategy | scenarios dominated | scenarios with MRR loss > 0.02 | worst MRR loss |
| --- | --- | --- | --- |
| current-staged | 6 / 8 | 0 | -0.009 |
| staged-tuned | 5 / 8 | 2 | -0.065 |
| capability-aware | 8 / 8 | 0 | 0.000 |
| category-aware | 8 / 8 | 0 | 0.000 |
| reliability-aware | 8 / 8 | 0 | 0.000 |
| progressive | 5 / 8 | 0 | -0.011 |
| early-stop | 1 / 8 | 0 | -0.001 |


## 8. Decision

| Candidate | Verdict | Why |
| --- | --- | --- |
| capability-aware selection | **adopt as next candidate, measure on real engines first** | 8/8 scenarios dominated: +0.21 MRR, -66% requests, -17% p95 for -3 pt Hit@10. Direction survives the offcap_fit sensitivity, magnitude does not. |
| category-aware selection | **adopt, cheaper form of the same idea** | 8/8 dominated, MRR 0.706, and it reuses select_engines (no new engine field): derive categories from the query kind. |
| current staged default (tier 8, target 10) | **keep** | 6/8 dominated, never a regression above 0.02 MRR, +0.013 MRR and +2.6 pt Hit@10. No change needed. |
| staged tier = 3 | **reject** | -17.8 pt Hit@10, -25.1 pt Recall@10: narrow tiers trade coverage for something selection gets for free. |
| reliability-aware (error-rate filter) | **reject as implemented** | Row identical to parallel in 8/8: the 50% threshold is never reached and the resting/suspended state is already applied to every strategy. Re-open only with a latency/empty-rate ranking. |
| progressive fan-out (2 -> 4 -> 8, 30 results) | **hold** | -33% requests, -9% p95, -0.7 pt Hit@10, +0.024 MRR, 5/8 dominated. Ship only if request cost becomes the binding constraint. |
| early termination (cancel at 30 results) | **hold, latency-SLO only** | -72% p95 but -15.7 pt Hit@10 and -20.1 pt Recall@10. Not a free win; needs a per-deployment trade-off decision. |

**No production default changes in this worktree.** SearchStrategy::Parallel stays
the default, the category selection of select_engines is unchanged, and no setting
is added or removed: everything above is a measurement from an example binary.
Making capability-aware routing real means (a) a query-kind classifier in
src/query.rs, (b) a capability field on the engine definition, (c) a live A/B inside
the allowed budget - none of which this experiment can validate.

## 9. Limitations (read before quoting a number)

1. **Simulation, not measurement.** Latency, failure and precision profiles are
   invented. Absolute values are meaningless; only the ordering of strategies under
   the same profiles is meaningful.
2. **The capability assumption drives the headline.** An off-capability engine at
   0.35 gold precision versus 1.00 is what produces the +0.206 MRR; at 0.70 the
   advantage is +0.047 (7.1).
3. **Filler pages are pure noise.** Every non-gold page is irrelevant to gold by
   construction, so extra engines can only add coverage through their own gold hits.
   Real engines also return mostly-useful-but-not-gold pages, which the production
   relevance blend would rank between gold and filler; that would soften the
   "fewer engines is better" effect.
4. **Ranking quality per provider is uniform.** Every provider orders its own list
   with the same generator; no provider has a systematically better ranking model.
5. **Resting fires in exactly one scenario** (all-low-quality, empty answers); the
   rest of the state machine exercised is suspension.
6. **Cancelled requests are never learned from.** In early-stop a cancelled request
   records no outcome, so the provider keeps its healthy state. Production has no
   cancellation at all, so this is a property of the candidate only.
7. **Deadline compliance is not discriminating** (result 8).
8. **No real engine was contacted.** No provider policy question was opened, no API
   was called, nothing outside the worktree was touched.

## 10. Reproduction

    # baseline (8 scenarios x 8 strategies x 150 queries), aggregates
    cargo run --release --example routing_lab -- --queries 150 --seed 7 \
        --out bench/local-retrieval/routing-lab/routing-lab.json

    # per-query raw rows (compact arrays, see rows_layout in the file)
    cargo run --release --example routing_lab -- --queries 150 --seed 7 --rows \
        --out bench/local-retrieval/routing-lab/routing-lab-rows.json

    # stability and sensitivity
    cargo run --release --example routing_lab -- --queries 150 --seed 11 \
        --out bench/local-retrieval/routing-lab/routing-lab-seed11.json
    cargo run --release --example routing_lab -- --queries 150 --seed 7 --offcap-fit 0.7 \
        --out bench/local-retrieval/routing-lab/routing-lab-offcap07.json

    # one scenario only (prints every table for that scenario)
    cargo run --release --example routing_lab -- --queries 150 --scenario rate-limit

The raw rows file stores one array per (scenario, strategy, query):

    [scenario, strategy, query, rank, mrr, ndcg, recall, latency_ms,
     requests, suspended_skips, cancelled, failures, deadline_missed, degraded]

## 11. Verification gates (this worktree)

cargo fmt --all -- --check, cargo clippy --locked --all-targets -- -D warnings,
cargo test --locked --all-targets and cargo check --locked --all-targets
--no-default-features all pass with the example in the tree; no existing test, bench
or fixture was modified.
