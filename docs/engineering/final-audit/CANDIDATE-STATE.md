# CANDIDATE-STATE — 独立監査に渡す候補の状態

記録日: 2026-11-17。この文書は監査対象を一意に指すためのスナップショットである。

## チェックポイント（2026-11-17 23:30 更新）

作業ツリーはローカルブランチ `maturation/phase3-5-2026-11-17` に 8 コミットで固定した（**未 push**）。
各コミットは単独で `cargo check --locked --lib` が通ることを確認済み。

| # | SHA | 内容 |
| --- | --- | --- |
| baseline | `f7639ce` | v0.31.2、main |
| 1 | `72b705f` | style(results): rustfmt |
| 2 | `3cd66cb` | fix(cache): Valkey 名前空間と SCAN |
| 3 | `651b758` | feat(observability): ステータスクラスと検索所要時間 |
| 4 | `9fb4fbd` | security(network): メディア上限・proxy scheme・raw_content clamp |
| 5 | `57c00bc` | feat(cli): --check-config / warnings / SIGTERM |
| 6 | `91a8f67` | feat(web): エラー形式・/readyz・各種上限・ブランド |
| 7 | `b5dc2a5` | test: CLI・抽出コーパス・統合テスト |
| 8 | `c24beeb` | docs(tools): OpenAPI・評価ツール・依存ポリシー・記録 |
| 9 | `14f6228` | security(network): engine クライアントの検証・リダイレクト・本文上限 |
| 10 | `78eba37` | fix(web): CSV の数式注入を無害化 |
| 11 | `a69f961` | fix(web): limiter 順序・media バケット・real_ip・readyz 索引 |
| 12 | `e6181cb` | fix(engine): poison 耐性と suspension 上限 |
| 13 | `d7428e1` | docs(legal): CDLA と source リンク |
| 14 | `4e0cd68` | feat(eval): オフライン融合実験ラボ |
| 15 | `d6e6313` | docs(engineering): 監査・決定・実測 |
| 16 | `8ca24f5` | docs(security) |
| 17 | `d5ffe50` | style: rustfmt |
| 18 | `cc34686` | refactor: i18n / wikidata infobox の削除（3,657 行 + JSON 983 KB） |
| 19 | `71388da` | feat(server): `shutdown_grace`（既定 10 秒）と drain watchdog |
| 20 | `842a82a` | test(cli): gitignore された索引に依存しない check-config |
| 21 | `664f5bf` | refactor(results): ランキングを `results/rank.rs` へ分離 |
| 22 | `7fb910e` | test(results): rank.rs のグループ順序テスト |
| 23 | `3431806` | style(cli): rustfmt |
| 24 | `a89ded3` | security(network): proxy 経由・別 engine へのリダイレクト拒否、search_url 例外 |
| 25 | `4749e8e` | test(extraction): コーパス 5→30 ページ、欠落 2 件の修正 |
| 26 | `826bbe5` | test(routing): ルーティング戦略のオフライン比較 |
| candidate | `826bbe5` | 監査対象（28 コミット、未 push） |

## 識別

| 項目 | 値 |
| --- | --- |
| リポジトリ | https://github.com/RustXNG/RustXNG |
| 基点コミット（baseline） | `f7639cede00634a48db3261709bc359752f109eb`（v0.31.2、main） |
| 候補（candidate） | 基点コミット上の**未コミット作業ツリー**（candidate のコミット SHA は存在しない） |
| ブランチ | `main`（`origin/main` と一致） |
| 追跡ファイルの変更 | 41 files changed, 2153 insertions(+), 263 deletions(-) |
| 未追跡の追加 | 21 パス（下記） |

## 追加した未追跡パス

```text
deny.toml
docs/DEPENDENCIES.md
docs/OBSERVABILITY.md
docs/README.md
docs/engineering/
docs/openapi.yaml
src/diagnostics.rs
static/favicon.ico
static/logo-wordmark-light.png
static/logo-wordmark.png
static/logo.png
tests/cli.rs
tests/extraction.rs
tests/fixtures/extraction/
tools/eval_local_index.py
tools/mcp_smoke.py
tools/test_check_bench.py
tools/test_doc_links.py
tools/test_docs_metrics.py
tools/test_eval_local_index.py
tools/test_openapi.py
```

## 検証の要約（2026-11-17 時点）

| 検証 | 結果 |
| --- | --- |
| `cargo fmt --all -- --check` | 成功 |
| `cargo clippy --locked --all-targets -- -D warnings` | 成功 |
| `cargo test --locked --all-targets` | 1178 passed / 4 ignored / 0 failed |
| `cargo check --locked --all-targets --no-default-features` | 成功 |
| `python3 -m unittest discover -s tools` | 86 tests OK |
| `cargo deny --locked check` | licenses / bans / sources / advisories すべて ok |
| `cargo audit` | 0 vulnerabilities / 0 warnings |
| markdownlint（CI と同じ glob） | 0 issues |
| smoke（`tools/smoke_agent_setup.py`） | 3/3 PASS（`/readyz` と MCP stdio を含む） |
| コンテナ（podman） | ビルド 153 MB、非 root、`/readyz`、`--check-config`、SIGTERM で 0 |

詳細と再現手順: [TEST-RESULTS.md](TEST-RESULTS.md)、[REPRODUCE.md](REPRODUCE.md)、
[../PERFORMANCE.md](../PERFORMANCE.md)。

## 未検証（意図的に委譲）

- 外部 MCP クライアント（Codex / Claude Code / OpenCode）の実機接続
- 実エンジン応答・実 Valkey・実 MySQL/MariaDB・実ブラウザー（Firefox レンダラー）・実 LLM 課金
- Windows / macOS 実機
- `rust:1.99.1` から MSRV 1.89 でのビルド（CI の msrv ジョブに委譲）

## 監査担当への注意

- 変更は**未コミット**である。監査は作業ツリーを対象にするか、[RELEASE-PLAN.md](../RELEASE-PLAN.md) の
  単位でコミットしたうえで SHA を記録する。
- 本資料は実装者自身の記録であり、独立監査の代替ではない。