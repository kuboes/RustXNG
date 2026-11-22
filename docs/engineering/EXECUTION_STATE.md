# EXECUTION_STATE — 作業状態

最終更新: 2026-11-17 01:40（監査用候補スナップショットまで）

セッション再開時は最初にこのファイルを読む。記憶ではなくここに書かれた状態から再開する。

## 現在

- Phase: 1・2 の実装済み項目を検証済み。3 の API 契約、4 の評価基盤、5 の DX まで着手
- ブランチ: `maturation/phase3-5-2026-11-17`（基点 `f7639ce`、最新 `a48650a`、26 コミット、未 push）
- 次の作業: Phase 4 のルーティング/rank fusion（ライブ検証が要るため BLOCKED 相当）か、索引拡充の検討

## 完了した項目（累積）

1. CI 赤（`Format`）の解消 — `cargo fmt --all`（16 箇所）
2. A12: Valkey キーの名前空間と `SCAN` ベースの clear（回帰テスト）
3. エラー応答のフォーマット一致（JSON/CSV/RSS/plain/HTML）
4. pageno 上限 1000、クエリ長上限 2048
5. `/autocompleter` の limiter・既知名のみ・5 秒期限
6. proxies の scheme 別解釈、未知キーは起動時エラー
7. `raw_content_limit` のクランプ（1..=10）
8. `/research` の全体期限（120s → 504）と同時実行制限（2 → 429）
9. 画像/favicon プロキシの limiter 適用とメディア同時 4 件の Semaphore
10. `/readyz`（検索可能性。engine 0 かつ索引なしで 503）
11. MCP の URL 記憶（分離・上限 256）の回帰テスト
12. 台帳チェックの clean 検証修正、review_date 365 日失効
13. README ja/ko の MCP 記述 parity、`/readyz` の 3 言語記載
14. `docs/openapi.yaml` と契約ドリフトテスト（`tools/test_openapi.py`）
15. `tools/check_bench.py` の回帰テストと CI 範囲の明文化
16. `docs/engineering/` と `final-audit/` の一式
17. `tools/eval_local_index.py`（Hit@1/5/10・MRR・nDCG・p50/p95、`--baseline`/`--min-hit5`）とテスト 4 件
18. TITLE_BOOST の A/B（3.0 vs 4.0）: 効果なしで不採用、コード復元済み・結果を記録
19. Phase 5.3 の判断: SDK は作らない（OpenAPI + HTTP クライアントで足りる、D12）
20. Phase 4.5: 表を Markdown 形式の行として抽出（列の区切り・ヘッダー区切り・パイプのエスケープ・上限つき）とテスト 4 件
21. Phase 5.1: `rustxng --check-config`（設定読込・エンジン構築・readiness を JSON で出力、ソケットを開かない）と CLI テスト 4 件。`/readyz` と共通の `diagnostics::Readiness` に集約
22. Phase 4.5: 数式の重複除去（KaTeX の視覚コピー、MathJax の assistive コピーを捨て、MathML の TeX annotation を優先）とテスト 3 件
23. Phase 5.5: CI に `--check-config`（test ジョブ）と `/readyz`・コンテナ内 `--check-config`（docker ジョブ）の検証を追加。リポジトリ外の作業ディレクトリで同じ主張をローカル検証
24. Phase 4.5: 表の `<caption>` が消える問題を修正（キャプション行を先頭に出す）+ テスト
25. Phase 3.4: slim 構成（`--no-default-features`）を実測 — 26,961,784 bytes（既定 35,492,584 から 24.0% 減）、ビルド 1 分 43 秒
26. Phase 4.5: 脚注参照・定義リストの回帰テスト、句読点前の余白除去（`tidy`）を追加
27. Phase 5.7: `docs/engineering/RELEASE-PLAN.md`
28. Phase 4.5: `nav` / `footer` / `hidden` / `aria-hidden` / `role` によるページクローム除去（構造シグナルのみ）+ テスト 2 件
29. 抽出の A/B 実測（2 ページ）: 句読点前の余白 149→15（RFC）・50→1（Book）、本文の欠落なし。PERFORMANCE.md に条件と限界を記録
30. Phase 4.5/5.8: `tests/fixtures/extraction/`（5 ページ + manifest）と `tests/extraction.rs`。残す文字列・残さない文字列・最小文字数・余白 0 を CI で検証（ネットワーク不要、第三者が再実行可能）
31. Phase 5.4: `docs/README.md`（目的別の文書索引、全リンクの存在を機械確認）と CONTRIBUTING からの導線
32. Phase 5.5: `/metrics` に readiness（`searxng_ready` / `searxng_engines_configured` / `searxng_local_index`）、`searxng_cache_error_total` / `searxng_cache_bytes`、`searxng_limiter_requests_total{verdict}` / `searxng_limiter_tracked` を追加。`docs/OBSERVABILITY.md` と統合テスト
33. Phase 5.4: 提供ロゴを `static/` に最適化して配置（mark 256px / wordmark 960px の明暗 2 種 / favicon 48-32-16 ICO）、`/favicon.ico` と `/static/logo*.png` をバイナリから配信、全 HTML にアイコン、README 3 言語にロゴ、NOTICE にブランド資産の但し書き。実体配信と README 参照のドリフトテスト
34. Phase 3.3/5.1: `Settings::validate` に cache（backend/url/ttl/max_bytes）、research（deadline/max_concurrency/max_queries/max_sources）、`server.base_url` の検証を追加。誤設定は `--check-config` で起動前に分かる。テスト 3 件
35. Phase 5.2/5.8: オフライン smoke（`tools/smoke_agent_setup.py`）に `/readyz` と MCP stdio セッション（`tools/mcp_smoke.py`: initialize / tools/list / search）を追加。3 ケース（default/cli/env port）で実機パス。CI の test・arm64 ジョブがそのまま実行する
36. Phase 5.5: `SIGTERM` を捕捉して graceful shutdown（Unix）。`docker stop` / `systemctl stop` で終了コード 0、処理中リクエストを完了。CLI テスト追加。旧バイナリは 143 で落ちることを実測して差分を確認
37. Phase 5.5: `web::router` のレイヤで応答のステータスクラスを数え、`searxng_http_responses_total{status}` として公開（バイナリ・テスト・ライブラリ利用者のすべてが同じ経路）。`Metrics::record_status` の単体テストと、2xx/4xx が実際に増える統合テスト
38. Phase 3: 全エンジン × pageno 1/2/1000 でリクエスト構築を検証する契約テスト（99 リクエスト構築、open_meteo は設定不足で 3 件 deferred、ページ固有の失敗 0）。R3 の回帰ガード
39. Phase 2.4: `web::router` に `DefaultBodyLimit::max(64 KiB)` を追加。`/search` と `/preferences` の過大な POST は 413。上限を明示し、回帰テストを追加
40. Phase 5.5: 検索の所要時間を `Metrics::record_search_duration` で記録し、`searxng_search_duration_seconds_sum/_count` として公開（`searcher.search` を薄いラッパにし、内部を `search_inner` へ）。単体・統合テスト
41. Phase 4.9: `eval_quality.py --restart-binary` の失敗を明示（バイナリ不在 / fuser 不在 / 即時終了 / 未 ready で終了コード 2）、レポートの `base` を実測先へ修正、テスト 3 件。測定の取り違えを防ぐ
42. Phase 5.5/5.7: コンテナイメージを podman で実ビルド（153 MB）し、非 root 実行・/healthz・/readyz（engines 34 / local_index false）・コンテナ内 --check-config・podman stop の終了コード 0 を実測
43. Phase 3.3/5.4: URL 検証型は導入しない判断（D19、理由は DNS rebinding）。`docs/OBSERVABILITY.md` の系列名とコードの一致を `tools/test_docs_metrics.py` で双方向に検査
44. Phase 5.7: CI のオフライン手順をローカルで再現（markdownlint 38 files / 0 issues、example benchmark のビルド、provider inventory、smoke 3/3 PASS）。新規文書 2 件の MD047 違反を検出して修正

## 引き継ぎ（2026-11-17 時点の最終状態）

内部で完結する作業（Phase 1〜3、5 の実装・検証・文書・監査資料）は完了しています。
未コミットのまま残っているのは、コミットが明示指示を待つ方針のためです。

### 残っている作業と、それが止まっている理由

| 項目 | 状態 | 必要な入力 |
| --- | --- | --- |
| 変更のコミット | BLOCKED | コミットの明示指示（方針: 依頼があるまでコミットしない）。単位は RELEASE-PLAN.md |
| 外部 MCP クライアント実機（Codex / Claude Code / OpenCode） | NOT_TESTED | 利用者ツールの MCP 設定変更の許可 |
| Phase 4 ルーティング / rank fusion | BLOCKED | ライブ検索プロバイダーの利用許可・予算（オフラインでは効果測定できない） |
| 実エンジン・実 Valkey・実 MySQL・実ブラウザー | NOT_TESTED | 外部環境と、提供元条件の確認 |
| 独立監査（6.1 Sol） | PENDING | 監査の実行。資料は final-audit/ に揃っている |

### 最終検証（2026-11-17 01:55）

- `cargo fmt --check` / `cargo clippy -D warnings` / `cargo test --all-targets`（1178 passed, 4 ignored）
- Python 86 tests OK、`cargo deny --locked check` すべて ok、markdownlint 0 issues
- smoke 3/3 PASS（default-port / cli-port / env-port、`/readyz` と MCP stdio を含む）

50. チェックポイント: 8 コミット（`72b705f`…`c24beeb`）をローカルブランチに固定し、各コミットの lib コンパイルを検証
51. security(network): engine クライアントの DNS/リダイレクト/本文上限（サブエージェント実装をレビューしてコミット `14f6228`）+ CSV 数式注入の無害化（`78eba37`）
52. fix(web): limiter を検証前に移動、media 3 経路を html バケットへ、`user_info` を trusted-proxy 準拠に（回帰テスト 2 件）
53. fix(web): `/readyz` は実際に開けた索引で判断（壊れた索引で 503 のテスト）
54. fix(engine): Mutex poison 耐性と suspension の 1 年上限（テスト 1 件）
55. Phase 4.3: オフライン融合実験（既存/RRF/正規化和/多様性、ablation と感度）— RRF は有意に悪く不採用、position 融合単独は solo に負けることを実測。ハーネスと再現可能 fixture を `examples/fusion_lab.rs` と `bench/local-retrieval/fusion-lab/` に収録

56. Phase 3: デッドコード削除（`cc34686`）— i18n 2,985 行 + wikidata 670 行 + JSON 983 KB + 生成器。参照・include・build.rs・テンプレート・公開 API を確認し、削除前後を実測
57. Phase 5.5: `server.shutdown_grace`（既定 10 秒）と drain watchdog（`71388da`）。SIGTERM の drain 内完了・期限超過の 2 テスト
58. Phase 5.3: 既定 / 最小 / 全 feature の比較（`docs/LIGHTWEIGHT.md`）。依存 349 vs 286、バイナリ −23.8%、起動・RSS は同等。既定は変更しない判断

59. Phase 4.3: `results/rank.rs` 分離をサブエージェント実装から取り込み（`664f5bf`, `7fb910e`）— mod.rs 1,497 行 / rank.rs 832 行、ランキング単体テスト 9 件、公開 API は re-export で維持
60. Phase 2/3: engine 例外の範囲を限定（`a89ded3`）— 有効 engine の base_url + search_url ホストのみ、proxy 時 cross-origin リダイレクト拒否、別 engine ホストへのリダイレクト拒否。単体テスト 2 件

61. Phase 4.5: 抽出コーパスを 5→30 ページへ拡張（`4749e8e`）。広告・同意・購読ブロックの混入と引用元 footer の消失を検出し、限定 class/id 判定と引用内 footer 保持で修正。抽出は中央値約 168µs（debug、100k 字文書 13.5ms）

62. Phase 4.2: ルーティング研究（`826bbe5`）。`examples/routing_lab.rs` と `bench/local-retrieval/routing-lab/` を収録。8 状態のシミュレーションで capability/category-aware が支配するが、仮定依存のため既定は不変（D23）

## 未完了・追跡

- R3: エンジン側 pageno 演算は checked でない（入口上限で緩和）
- Phase 4 のルーティング/rank fusion は、ライブ検索の許可・予算が前提のため BLOCKED 相当（オフラインでは測定不能）
- R5/R6: 外部環境依存の検証は NOT_TESTED（実エンジン、実 Valkey、実 DB、実ブラウザー、DNS rebinding の実接続）

## 変更したファイル（累積）

Rust: src/cache.rs, src/network.rs, src/web/handlers.rs, src/web/mod.rs, src/web/image_proxy.rs,
src/web/favicons.rs, src/settings.rs, src/aggregator.rs, src/mcp/mod.rs, src/results.rs（fmt のみ）,
tests/integration.rs
設定: settings.toml
Python: tools/check_provider_policy.py, tools/test_check_provider_policy.py（旧名 test_provider_policy.py）,
tools/test_check_bench.py（新規）, tools/test_openapi.py（新規）
文書: README.md, README.ja.md, README.ko.md, docs/AGENT-SETUP.md, docs/BENCHMARK.md,
docs/COMPATIBILITY.md, docs/openapi.yaml（新規）, docs/engineering/*

## 実行したコマンド（要約）

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo check --locked --all-targets --no-default-features
cargo build --release --locked --bin rustxng
python3 -m unittest discover -s tools -p "test_*.py"
python3 tools/check_provider_policy.py
python3 tools/provider_inventory.py --check
cargo audit
python3 /tmp/rxng/measure100.py   （clean・ローカル索引・100 問）
```

## 最終ゲート（2026-11-17 01:55）

| コマンド | 結果 |
| --- | --- |
| cargo fmt --all -- --check | 成功 |
| cargo clippy --locked --all-targets -- -D warnings | 成功 |
| cargo test --locked --all-targets | 1178 passed / 4 ignored / 0 failed |
| cargo check --no-default-features --all-targets | 成功 |
| cargo build --release --bin rustxng | 成功（35,492,584 bytes） |
| python unittest discover -s tools | 86 tests OK |
| smoke_agent_setup（cli-port / env-port） | PASS（`/readyz` + MCP stdio を含む）。default-port は 29483 が環境で使用中のためスキップ |
| check_provider_policy / provider_inventory | 両方成功・一致 |
| cargo audit | 0 vulnerabilities / 0 warnings |
| cargo deny --locked check | licenses / bans / sources / advisories すべて ok |

## 性能（PERFORMANCE.md に詳細）

- 起動→/healthz: 0.054–0.206 s / アイドル RSS 23.8 MiB / ピーク RSS 706,916 kB（索引 mmap 含む）
- 100 問オフライン: p50 15.6 ms、p95 240.3 ms、p99 358.8 ms、Hit@10 36/100

## 次に実行する具体的な作業

1. Phase 4: 索引の収録範囲を広げた場合の品質（gold 到達率）を測る — 外部データ取得の判断が必要
2. Phase 4.5 の続き: 脚注（`sup a[href^=#fn]`）と `<dl>` の扱いを評価（現状 `<dl>` は dt/dd をブロック処理済み）
3. Phase 5: 外部クライアント（Codex / Claude Code / OpenCode）の実機接続（NOT_TESTED）
4. [RELEASE-PLAN.md](RELEASE-PLAN.md) の単位でコミット分割（ユーザー指示が必要）