# RELEASE-PLAN — 未コミット変更のリリース準備

基準: 2026-11-17 / 基点コミット `f7639ce`（v0.31.2）。変更は**未コミットの作業ツリー**にあり、
この文書はマージ後のリリース単位を提案する（タグ作成・公開は行わない）。

## 推奨バージョン

**0.32.0（minor）**。新設定（`research.deadline` / `max_concurrency`）、新 CLI フラグ（`--check-config`）、
新エンドポイント（`/readyz`）、新メトリクス、`/favicon.ico` の実体化を含む。破壊的変更は無いが patch では表せない。

## コミット単位（Conventional Commits）

| # | 主な対象 | メッセージ案 |
| --- | --- | --- |
| 1 | src/cache.rs | `fix(cache): namespace Valkey keys and clear them with SCAN` |
| 2 | src/network.rs, src/aggregator.rs | `security(network): bound media fetches, honour proxy schemes and clamp raw content` |
| 3 | src/settings.rs, src/main.rs | `feat(cli): add --check-config with readiness and warnings` |
| 4 | src/web/handlers.rs, src/web/mod.rs, src/web/image_proxy.rs, src/web/favicons.rs | `fix(web): answer errors in the requested format and bound every input` |
| 5 | src/web/handlers.rs, src/diagnostics.rs, src/web/mod.rs | `feat(web): add /readyz and share the readiness check` |
| 6 | src/settings.rs, src/web/mod.rs, settings.toml | `feat(research): add a request deadline and a concurrency limit` |
| 7 | src/main.rs | `fix(server): stop gracefully on SIGTERM` |
| 8 | src/content.rs | `feat(extract): keep tables, captions and single-copy formulas` |
| 9 | src/metrics.rs, src/web/mod.rs, src/web/handlers.rs | `feat(observability): export readiness, cache, limiter, status and latency metrics` |
| 10 | static/*, src/web/handlers.rs, templates/*, README*, NOTICE | `feat(brand): ship logo, wordmark and favicon` |
| 11 | tests/*, src/mcp/mod.rs | `test: cover limits, isolation, CLI, extraction and pages` |
| 12 | tools/*.py | `test(tools): gate benchmarks, OpenAPI, metrics docs and links` |
| 13 | deny.toml, .github/workflows/* | `ci: check dependency policy, readiness and smoke paths` |
| 14 | docs/*, README* | `docs: record readiness, evaluation, policy and engineering state` |
| 15 | src/fetch/*, src/network.rs | `security(network): validate engine clients, redirects and body size` |
| 16 | src/web/feeds.rs | `fix(web): stop CSV cells from starting a spreadsheet formula` |
| 17 | src/web/handlers.rs, src/web/image_proxy.rs, src/web/favicons.rs | `fix(web): charge media to the page window and check the limiter first` |
| 18 | src/diagnostics.rs, src/web/handlers.rs, src/main.rs | `fix(web): readiness follows the index that actually opened` |
| 19 | src/engine/state.rs, src/settings.rs | `fix(engine): survive a poisoned state lock and bound suspension windows` |
| 20 | examples/fusion_lab.rs, bench/local-retrieval/fusion-lab/ | `feat(eval): offline rank-fusion lab` |
| 21 | src/i18n.rs, src/wikidata_infobox.rs, tools/import_translations.py, docs | `refactor: remove unused i18n and wikidata infobox modules` |
| 22 | src/settings.rs, src/main.rs, tests/cli.rs | `feat(server): bound the shutdown drain with shutdown_grace` |
| 23 | src/network.rs | `security(network): distrust redirects through proxies and to other engines` |
| 24 | tests/fixtures/extraction/, src/content.rs | `test(extraction): grow the offline corpus to 30 pages` |
| 25 | examples/routing_lab.rs, bench/local-retrieval/routing-lab/ | `test(routing): compare engine-routing strategies offline` |
| 26 | src/results/mod.rs, src/results/rank.rs | `refactor(results): move ranking and policy into results/rank.rs` |

整形のみの差分（src/results.rs など）は対応するコミットに混ぜず、`style:` として分けるか
該当コミットの本文に rustfmt と明記する。

## CHANGELOG

`release-please` が conventional commit から生成するため `[Unreleased]` を手で足さない。
マージ後の draft で確認する項目:

- `feat` が Features、`fix` が Bug Fixes、`security` が本文で言及されること
- 破壊的変更が無いこと（`!` を付けない）
- `tools/release_tools.py notes --tag <新タグ>` が空でないこと

## マージ前に実行する検証

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo check --locked --all-targets --no-default-features
python3 -m unittest discover -s tools -p "test_*.py"
cargo deny --locked check
cargo audit
python3 tools/smoke_agent_setup.py --binary target/debug/rustxng
npx --yes markdownlint-cli2@0.24.0 "*.md" "docs/*.md" "tools/page-renderer/*.md"
```

## 未検証（リリース判断の前に第三者が確認する項目）

- 外部 MCP クライアント（Codex / Claude Code / OpenCode）の実機接続
- 実エンジン応答・実 Valkey・実 MySQL・実ブラウザー（Firefox レンダラー）・実 LLM 課金
- Windows / macOS 実機（CI の matrix に委譲）
- 独立監査（6.1 Sol）。状態は READY_FOR_INDEPENDENT_AUDIT のまま

コンテナイメージは podman で実ビルド・実行検証済み（2026-11-17、153 MB、非 root、`/readyz`、SIGTERM で 0）。