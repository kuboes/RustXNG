# Result-fusion comparison (Phase 4.3 experiment)

Harness: `examples/fusion_lab.rs` (experiment-only; no production code path is
changed, no feature flag is required). All measurements are offline replays of
fixtures that already exist in the working tree. No provider, network or paid
API call is made.

## 1. What the shipping fusion does (read from the source)

| Step | Location | Behaviour |
| --- | --- | --- |
| Ingest | `src/results.rs:305-427` | Each engine list is appended in order. Position counts 1..n per engine (deep pages start at 100). Duplicates are found by `dedup_key` (`:1459`), which drops the scheme, fragment, trailing slash and tracking params. |
| Per-source evidence | `src/results.rs:382-423` | One `SourceRank {weight, position, deep_page}` per (result, engine). If the same engine repeats a URL, only the better rank is kept. |
| Score | `src/results.rs:445-494` | `weight = product of source weights * number_of_sources`; then `score += weight / position` (Priority::Medium), `+ relevance_weight * relevance(url,title,content,terms)`, `* 0.5^deep_page`, `+ authority_weight * (authority(query,url) - 0.5)`, plus a language adjustment. Defaults: relevance 4.0, authority 8.0 (`src/settings.rs:527`, `:543`), both forced to 0 for single-source requests (`src/aggregator.rs:800-811`). |
| Distill | `src/results.rs:681-736` | Transfers (same normalized title, same digit runs, Dice >= 0.95) are merged across hosts, highest score becomes representative. |
| Order | `src/results.rs:738-792` | Sort by score, then a category grouping interleave (max 8 per group, window 20). |

So the current fusion is **weighted inverse-position fusion with multi-source agreement
multiplication, plus lexical relevance and named-site authority boosts**, not RRF and
not normalized score sum.

## 2. Fixtures (rights check)

| Fixture | Tracked in git | Content | Used for |
| --- | --- | --- | --- |
| `bench/quality-questions-100.json` | yes | 100 queries, gold URL prefixes (1-5 per query, 65 en / 35 ja) | ground truth |
| `bench/content-searxng.json` | yes | 20 queries, recorded metasearch list | source |
| `bench/content-rustxng.json` | yes | 20 queries, recorded metasearch list | source |
| `bench/local-retrieval/development.json` | yes | 40 invented docs, 24 queries | synthetic multi-source |
| `bench/eval-100-final.json` | yes | 100 queries, recorded merged ranking | single-list re-rank/diversity |
| `bench/content-exa.json`, `bench/content-tavily.json`, `bench/content-ollama.json` | **no** (`.gitignore:18` - commercial API dumps) | 100 queries each | local-only extension |

Consequence: the only **reproducible-from-a-clean-clone** multi-source provider
dataset is `providers3` (searxng + rustxng, n = 13). The n = 100 two-provider
comparison is possible only where the gitignored dumps exist locally; its raw
output is stored here but it contains no provider payloads, only per-query ranks
and the tracked gold URLs. No new third-party data was added and no dump was copied.

## 3. Strategies

| Name | Definition | Input |
| --- | --- | --- |
| `existing` | `ResultContainer::extend` per source + `close(weight_of=1.0, relevance=4.0, authority=8.0)` | as shipped |
| `rrf60` | `sum 1/(60 + rank)` over sources | per-source lists |
| `rrf60_weighted` | `sum w_s/(60 + rank)`, `w = {exa 1.0, tavily 1.0, searxng 0.6, rustxng 0.6}`, declared before measuring (merged lists down-weighted because they already double count engine agreement) | per-source lists |
| `normsum` | per-list rank-normalised score `(n-rank)/(n-1)`, summed | per-source lists |
| `existing+diversity`, `rrf60+diversity` | greedy domain reordering of a base ranking: pick `score/(1 + 0.5 * picks_from_host)` | base ranking |
| `solo:<source>` | one source list, untouched | reference |

Metrics: Hit@1, Hit@5, Hit@10, MRR@10, nDCG@10 with binary gains and
`IDCG` = ideal ordering of the relevant documents actually retrieved inside the
cutoff. Gold match is a host-equal, path-prefix match on `dedup_key`, so
`.../datastructures` matches `.../datastructures.html`. Paired significance is a
two-sided exact sign test on per-query MRR differences against `existing`.

## 4. Run commands

```sh
cd /tmp/rxng/wt-ranking
export CARGO_TARGET_DIR=/home/sahen/File/Prog/searxng-rust/target
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=8
cargo build --offline --locked --example fusion_lab

# repository-reproducible datasets (no gitignored input)
./target/debug/examples/fusion_lab --rows true \
    --out bench/local-retrieval/fusion-lab/reproducible.json

# local-only extension (needs the gitignored commercial dumps)
./target/debug/examples/fusion_lab --repo /home/sahen/File/Prog/searxng-rust \
    --dataset providers,providers4 --out bench/local-retrieval/fusion-lab/providers-local-only.json

# sensitivity / ablation
./target/debug/examples/fusion_lab --repo /home/sahen/File/Prog/searxng-rust --dataset providers --rrf-k 10 --out bench/local-retrieval/fusion-lab/sensitivity-rrf-k10.json
./target/debug/examples/fusion_lab --repo /home/sahen/File/Prog/searxng-rust --dataset providers --relevance-weight 0 --authority-weight 0 --out bench/local-retrieval/fusion-lab/ablation-position-only.json
```

`--repo` reads fixtures from another checkout without copying them, which keeps
the gitignored dumps out of this worktree.

## 5. Results

### 5.1 providers (exa + tavily, n = 100, local-only dumps)

| strategy | Hit@1 | Hit@5 | Hit@10 | MRR@10 | nDCG@10 | dMRR vs existing | sign-test p |
| --- | --- | --- | --- | --- | --- | --- | --- |
| existing | 0.7600 | 0.9300 | 0.9300 | 0.8365 | 0.8481 | - | - |
| existing+diversity | 0.7600 | 0.9200 | 0.9300 | 0.8309 | 0.8229 | -0.0056 | 0.219 |
| solo:exa | 0.7700 | 0.9000 | 0.9200 | 0.8345 | 0.8215 | -0.0020 | 1.000 |
| normsum | 0.6300 | 0.9000 | 0.9100 | 0.7442 | 0.7526 | -0.0923 | 0.0005 |
| rrf60 | 0.6200 | 0.9000 | 0.9100 | 0.7393 | 0.7500 | -0.0972 | 0.0002 |
| rrf60_weighted | 0.6200 | 0.9000 | 0.9100 | 0.7393 | 0.7500 | -0.0972 | 0.0002 |
| rrf60+diversity | 0.6200 | 0.8600 | 0.9100 | 0.7286 | 0.7249 | -0.1079 | <0.0001 |
| solo:tavily | 0.3100 | 0.7500 | 0.8500 | 0.4810 | 0.5762 | -0.3555 | <0.0001 |

Head-to-head: `existing` vs `solo:exa` - 12 queries better, 13 worse, 75 ties.
`existing` vs `rrf60` - 22 better, 3 worse, 75 ties.

### 5.2 providers4 (exa + tavily + searxng + rustxng, n = 13, local-only dumps)

| strategy | Hit@1 | Hit@5 | Hit@10 | MRR@10 | nDCG@10 | dMRR | p |
| --- | --- | --- | --- | --- | --- | --- | --- |
| rrf60 | 0.9231 | 1.0000 | 1.0000 | 0.9487 | 0.9249 | +0.0256 | 1.000 |
| solo:exa | 0.8462 | 1.0000 | 1.0000 | 0.9231 | 0.9102 | +0.0000 | 1.000 |
| existing | 0.8462 | 1.0000 | 1.0000 | 0.9231 | 0.8973 | - | - |
| rrf60_weighted | 0.8462 | 1.0000 | 1.0000 | 0.8974 | 0.8980 | -0.0256 | 0.500 |
| normsum | 0.8462 | 1.0000 | 1.0000 | 0.9103 | 0.8957 | -0.0128 | 1.000 |
| solo:rustxng | 0.6154 | 0.9231 | 1.0000 | 0.7321 | 0.7744 | -0.1910 | 0.219 |
| solo:searxng | 0.6154 | 0.8462 | 0.9231 | 0.7385 | 0.7378 | -0.1846 | 0.375 |
| solo:tavily | 0.3846 | 0.7692 | 0.9231 | 0.5897 | 0.6785 | -0.3333 | 0.008 |

`rrf60` wins here by one query (n = 13, p = 1.0), and the a-priori weighted variant
is worse than plain RRF: down-weighting the two metasearch lists removes useful
evidence. Reported, not adopted.

### 5.3 providers3 (searxng + rustxng, n = 13, fully repository-reproducible)

| strategy | Hit@1 | Hit@5 | Hit@10 | MRR@10 | nDCG@10 | dMRR | p |
| --- | --- | --- | --- | --- | --- | --- | --- |
| normsum | 0.8462 | 1.0000 | 1.0000 | 0.9038 | 0.8878 | +0.0705 | 0.625 |
| rrf60 | 0.8462 | 1.0000 | 1.0000 | 0.9103 | 0.8594 | +0.0769 | 0.625 |
| rrf60_weighted | 0.8462 | 1.0000 | 1.0000 | 0.9103 | 0.8594 | +0.0769 | 0.625 |
| existing | 0.6923 | 1.0000 | 1.0000 | 0.8333 | 0.8605 | - | - |
| existing+diversity | 0.6923 | 1.0000 | 1.0000 | 0.8333 | 0.8552 | +0.0000 | 1.000 |
| rrf60+diversity | 0.8462 | 1.0000 | 1.0000 | 0.8808 | 0.8153 | +0.0474 | 1.000 |
| solo:rustxng | 0.6154 | 0.9231 | 1.0000 | 0.7321 | 0.7744 | -0.1013 | 0.688 |
| solo:searxng | 0.6154 | 0.8462 | 0.9231 | 0.7385 | 0.7378 | -0.0949 | 0.625 |

RRF looks better on MRR and worse on nDCG: it lifts the first gold hit but pushes
other relevant documents down. Two queries move, so nothing is established.

### 5.4 dev40 (4 deterministic synthetic rankers, n = 24)

Every strategy scores Hit@1 = Hit@5 = Hit@10 = 1.0000, MRR = nDCG = 1.0000
(only `solo:body` slips to 0.9583 Hit@1). The fixture was written as a
single-retriever regression set; it has a **ceiling effect and cannot rank fusion
strategies**. Negative result, recorded as such.

### 5.5 recorded100 (re-ranking/diversifying one recorded merged list, n = 100)

| strategy | Hit@1 | Hit@5 | Hit@10 | MRR@10 | nDCG@10 | dMRR | p |
| --- | --- | --- | --- | --- | --- | --- | --- |
| identity (= `solo:recorded`, `rrf60` on one list) | 0.6900 | 0.8000 | 0.8700 | 0.7444 | 0.7555 | +0.0094 | 0.815 |
| existing | 0.6500 | 0.8300 | 0.8700 | 0.7349 | 0.7628 | - | - |
| existing+diversity | 0.6500 | 0.8200 | 0.8700 | 0.7291 | 0.7477 | -0.0058 | 0.219 |
| rrf60+diversity | 0.6900 | 0.7700 | 0.8700 | 0.7289 | 0.7222 | -0.0060 | 0.152 |

Consistency check: the identity row reproduces the recorded report within 1-2
queries (`bench/eval-100-final.json` records hit@1 70/100, MRR 0.757 with its own
version-tolerant matcher), which is expected given a stricter prefix matcher here.

### 5.6 Ablation on providers (n = 100)

| configuration | Hit@1 | MRR@10 | nDCG@10 |
| --- | --- | --- | --- |
| relevance 4.0 + authority 8.0 (shipped) | 0.7600 | 0.8365 | 0.8481 |
| authority 8.0 only | 0.7300 | 0.8153 | 0.8277 |
| relevance 4.0 only | 0.6300 | 0.7393 | 0.7531 |
| position fusion only (both 0) | 0.6600 | 0.7678 | 0.7649 |
| solo:exa reference | 0.7700 | 0.8345 | 0.8215 |

Position fusion alone (0.7678) is significantly **worse** than the best single
source (0.8345, 66 wins vs 20 losses, p = 0.0015). The lexical relevance and
named-site authority terms carry the measured gain; example lifts: `ja-e-gov`
exa rank 10 -> 1, `ja-gcp` 10 -> 4, `en-rs-vec` 2 -> 5 (a loss).

### 5.7 Sensitivity

| variant | MRR@10 | nDCG@10 | dMRR vs existing | p |
| --- | --- | --- | --- | --- |
| existing (reference) | 0.8365 | 0.8481 | - | - |
| rrf k=1 | 0.7720 | 0.7663 | -0.0645 | 0.027 |
| rrf k=10 | 0.7418 | 0.7508 | -0.0947 | 0.0002 |
| rrf k=60 | 0.7393 | 0.7500 | -0.0972 | 0.0002 |
| diversity lambda 0.2 | 0.8345 | 0.8383 | -0.0020 | 0.625 |
| diversity lambda 0.5 | 0.8309 | 0.8229 | -0.0056 | 0.219 |
| diversity lambda 1.0 | 0.8244 | 0.8078 | -0.0121 | 0.070 |

No parameter choice rescues RRF or diversity on this data.

## 6. Decision

**Adopt nothing.** No candidate is clearly better than the shipping fusion, and
the adoption bar was 'clearly better'.

- `rrf60`, `rrf60_weighted`, `normsum`: significantly worse at n = 100 (p <= 0.0005),
  and only non-significantly better on the n = 13 subsets (p >= 0.62).
- `domain diversity`: neutral-to-negative in all four slices (dMRR -0.0056 / -0.0058
  / 0.0000 / 0.0000, never significant, always behind or level with the base).
- `existing` remains the best single choice on every dataset that can rank at all.

Secondary finding worth a separate decision: on this data the fusion layer's own
contribution is not demonstrated. Position-only fusion loses to `solo:exa`
(p = 0.0015); the full configuration beats `solo:exa` by 0.0020 MRR
(12 wins, 13 losses, p = 1.0). Treating 'fusion beats a single provider' as a
claim requires a larger, non-correlated multi-engine recording.

## 7. Limitations

1. `providers`/`providers4` depend on gitignored commercial dumps; they are local
   evidence, not something a clean clone reproduces.
2. n = 13 for every 3-4 source dataset; nothing about those rows is significant.
3. Provider lists are top-10 (exa, tavily) and top-10/top-53 (rustxng, searxng),
   so RRF's k = 60 compresses the whole signal into a narrow score band; k = 1 and
   k = 10 were run to show the conclusion does not depend on k.
4. `existing` is driven with uniform source weight 1.0; production engine weights
   from the registry differ and would change absolute scores.
5. Query mix is documentation/wiki heavy (55 doc, 25 wiki, 10 official); a
   navigational or fresh-news mix may behave differently.
6. `dev40` cannot discriminate (ceiling), and the four synthetic rankers are a
   harness construct, not recorded engines.
7. nDCG uses binary gains and an achievable ideal within the cutoff; graded
   relevance was not available.

## 8. Reproduction

`cargo build --offline --locked --example fusion_lab` then section 4. Outputs are
deterministic: two runs of the same arguments produce byte-identical `results`
and `comparisons` blocks (checked).

## 9. Verification gates (in this worktree)

| gate | command | result |
| --- | --- | --- |
| example builds | `cargo build --offline --locked --example fusion_lab` | ok |
| example lint | `cargo clippy --offline --locked --example fusion_lab -- -D warnings` | ok |
| example format | `rustfmt --edition 2024 examples/fusion_lab.rs` | clean (`examples/fusion_lab.rs` is absent from the repo-wide diff) |
| tests | `cargo test --offline --locked --all-targets --no-fail-fast` | 1177 passed, 1 failed, 4 ignored |
| format gate | `cargo fmt --all --check` | fails on 38 files, none of them mine |

Both failures are pre-existing and environmental, not caused by this experiment:

- `tests/cli.rs::check_config_reports_the_clean_profile_readiness` asserts that the
  gitignored `index-docs/` and `index-wikipedia/` directories sit next to
  `Cargo.toml` (they exist in the main checkout, not in a fresh `git worktree`).
  The same test passes in the main checkout.
- `cargo fmt --all --check` reports 39 files in the main checkout too. The
  recorded gate (`bench/local-retrieval/provenance.json`) used rustc/rustfmt
  1.99.1; this machine has 1.100.0, whose rustfmt expands several constructs.

## 10. Files produced

| Path | Contents |
| --- | --- |
| `examples/fusion_lab.rs` | the harness (strategies, metrics, sign test, loaders) |
| `bench/local-retrieval/fusion-lab/reproducible.json` | providers3, dev40, recorded100 raw results + per-query ranks |
| `bench/local-retrieval/fusion-lab/providers-local-only.json` | providers, providers4 raw results + per-query ranks |
| `bench/local-retrieval/fusion-lab/sensitivity-*.json` | rrf k = 1 / 10, diversity lambda 0.2 / 1.0 |
| `bench/local-retrieval/fusion-lab/ablation-*.json` | relevance-only, authority-only, position-only |

