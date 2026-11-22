# CHANGES — 変更一覧

## Rust

| ファイル | 変更 | 理由 |
| --- | --- | --- |
| src/cache.rs | Valkey キーの `rustxng:cache:` 名前空間、`SCAN`+`DEL` による clear、RESP 応答を生で扱う `raw_command` | 旧 clear は実際のキーと一致せず何も消えなかった。`KEYS` は大規模 DB をブロックする |
| src/network.rs | `proxies` を scheme 別（all/http/https）に登録、未知キーは Build エラー。メディア取得に Semaphore=4 と期限付きスロット取得 | 全キーを `Proxy::all` で登録していたため BTreeMap 先頭が全通信を奪っていた。メディアは 1 件 8MiB を保持する |
| src/aggregator.rs | `raw_content_limit` を 1..=10 にクランプ | 本文 4MiB × N を同時保持できた |
| src/settings.rs | `research.deadline`（120）、`research.max_concurrency`（2） | /research の全体期限と同時実行上限 |
| src/web/mod.rs | `AppState.research_slots`（Semaphore） | 同上 |
| src/web/handlers.rs | `format_error` の導入、4xx を要求フォーマットで返す、pageno ≤ 1000、q ≤ 2048、/research の期限と 429、/autocompleter の limiter・ホワイトリスト・5s 期限、`/readyz` | 監査 N2/N3/N11/N13/A04/A16 |
| src/web/image_proxy.rs, src/web/favicons.rs | limiter 適用（`plain` 形式の拒否） | 監査 N12 |
| src/mcp/mod.rs | URL 記憶の分離・上限の回帰テスト | 挙動変更なし |
| src/content.rs | 表を Markdown 風の行として抽出（列区切り・ヘッダー区切り・パイプのエスケープ・行/列/セルの上限） | 旧実装はセルを空白で連結し列の境界を失っていた |
| src/results.rs | rustfmt のみ | CI 緑化 |

## 設定・ツール・文書

- settings.toml: `[research]` に `deadline` と `max_concurrency` を追加（コメント付き）
- tools/check_provider_policy.py: clean プロファイルの検証モデルを実行時構成に一致、review_date 365 日失効
- tools/test_provider_policy.py: 失効の回帰テスト
- tools/eval_local_index.py（新規）: ローカル索引の Hit@1/5/10・MRR・nDCG・p50/p95、`--baseline` 差分、`--min-hit5` ゲート
- tools/test_eval_local_index.py（新規）: 順位→指標の換算、パス境界照合、CLI ゲートのテスト 4 件
- tools/test_check_bench.py（新規）: 下限・失敗件数のゲート 4 件
- tools/test_openapi.py（新規）: OpenAPI とコードのドリフト検査 5 件
- docs/LOCAL-INDEX.md: オフライン評価の手順と限界。docs/BENCHMARK.md: 現行 CI 範囲
- src/web/handlers.rs: `/metrics` に readiness・キャッシュエラー・レート制限の系列を追加
- docs/OBSERVABILITY.md（新規）: 公開系列と監視の使い分け
- static/logo.png, static/logo-wordmark.png, static/logo-wordmark-light.png, static/favicon.ico: ブランド資産（最適化済み）
- src/web/handlers.rs, src/web/mod.rs, templates/*.html, README*.md, NOTICE: ロゴ配信・favicon リンク・出所の明記
- src/settings.rs: cache / research / server.base_url の検証を追加（誤設定は起動時と `--check-config` でエラー）
- tools/smoke_agent_setup.py: `/readyz` と MCP stdio セッションの確認を追加
- tools/mcp_smoke.py（新規）: stdlib のみで initialize / tools/list / search を検証
- src/main.rs: Unix で SIGTERM を捕捉し graceful shutdown（終了コード 0）
- tests/cli.rs: SIGTERM で停止することを検証
- tests/integration.rs: 全エンジンのページ範囲（1/2/1000）でのリクエスト構築を検証
- src/web/mod.rs: リクエストボディ上限 64 KiB（`DefaultBodyLimit`）、統合テスト付き
- src/metrics.rs, src/aggregator.rs, src/web/handlers.rs: 検索所要時間の合計・件数メトリクス
- tools/eval_quality.py: `--restart-binary` の起動失敗を明示的なエラーにし、レポートの base を実測先に修正。tools/test_eval_quality.py（新規）
- tools/test_docs_metrics.py（新規）: `docs/OBSERVABILITY.md` と出力メトリクス名の双方向ドリフト検査
- deny.toml（新規）+ .github/workflows/security.yml: `cargo deny` によるライセンス・bans・sources・advisories 検査
- docs/DEPENDENCIES.md（新規）: 依存ポリシーと例外手順
- src/web/mod.rs: `format=json` の抽出器拒否を JSON に整形（413・クエリ不正）、統合テスト
- src/settings.rs, src/main.rs: `Settings::warnings` と `--check-config` の `warnings`、`MAX_PAGENO` を settings へ移動
- tools/test_doc_links.py（新規）: README と docs/** の相対リンク検査
- docs/engineering/RELEASE-PLAN.md: 14 コミット単位・検証コマンド・未検証項目に更新
- docs/engineering/final-audit/CANDIDATE-STATE.md（新規）: 監査対象のスナップショット
- src/metrics.rs, src/web/mod.rs: 応答のステータスクラス別カウンタと `searxng_http_responses_total{status}`
- src/diagnostics.rs（新規）: readiness 判定を `/readyz` と CLI で共有
- src/main.rs: `--check-config`（設定読込・エンジン構築・readiness を JSON 出力、ソケットを開かない）
- tests/cli.rs（新規）: `--check-config` の 4 テスト
- tests/extraction.rs と tests/fixtures/extraction/（新規）: 本文抽出の回帰コーパス（5 ページ + manifest、ネットワーク不要）
- src/content.rs: 数式の重複除去、表キャプションの保持、句読点前の余白除去、構造シグナルによるページクローム除去
- .github/workflows/ci.yml: test ジョブに `--check-config`、docker ジョブに `/readyz` とコンテナ内 `--check-config` の検証
- tests/integration.rs: 6 テスト追加（format 別エラー、pageno 上限、クエリ長、autocomplete、research 同時実行、research 期限）
- README.ja.md / README.ko.md: MCP の安全記述・accept_language・本文上限の parity
- docs/COMPATIBILITY.md: research の新設定を追記
- docs/engineering/*: 本資料一式

## コミット

未作成（方針: 明示指示があるまでコミットしない）。
