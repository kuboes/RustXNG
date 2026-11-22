# ROADMAP — RustXNG maturation

基準日: 2026-11-16 / 基準コミット: f7639ce (v0.31.2)

既存文書を再利用し、重複を作らない。日付つきの監査・測定は `docs/AUDIT-*.md`、
`docs/PERFORMANCE-*.md`、`docs/ISSUE-RESOLUTION-*.md`、`bench/results/` を正とする。
このディレクトリは「現在の計画と状態」だけを持つ。

## Phase 1 — Stabilization

目的: 既存機能を信頼できる状態にする。新機能は追加しない。

- [x] main が赤い原因の特定と解消（CI `Format` = 3 OS 失敗、commit f7639ce の run 43703503946）
- [x] 監査項目 A01–A18 の現行コードでの再判定（`AUDIT.md`）
- [x] 未解消だった A12（Valkey キャッシュ名前空間と clear）の修正
- [x] JSON/CSV/RSS クライアントが HTML エラーを受け取る問題の修正
- [x] pageno の絶対上限、クエリ長上限の追加（オーバーフロー防止）
- [x] MCP の URL 記憶（セッション境界・上限）の回帰テスト追加
- [ ] 実エンジンを用いた回帰（外部ネットワーク依存のため NOT_TESTED）

## Phase 2 — Security, Compliance and Trust

- [x] A04（/autocompleter）: レート制限、既知バックエンドのみ、全体 5s 期限
- [x] A13（proxy）: scheme 別 Proxy、未知キーは起動時エラー
- [x] A03 の一部: `raw_content_limit` を 1..=10 にクランプ
- [x] プロバイダー台帳チェックの clean プロファイル検証モデルを実行時構成に一致させる
- [x] review_date の失効（365 日）検出を追加
- [x] A03 残り: 画像/favicon の並列上限（Semaphore=4）と limiter 適用
- [x] /research の全体期限（`research.deadline`）と同時実行制限（`research.max_concurrency`）
- [ ] DNS rebinding の実接続再現（コード検証のみ、実測は NOT_TESTED）

## Phase 3 — Architecture and Maintainability

- [ ] 責務分離の見直し（監査で死コード 2 件・循環 3 組・重複 3 件を特定。`results/rank.rs` 分離のみ利益大と判断、実装は次段）
- [x] 依存削減の計測: slim 構成（`--no-default-features`）は 26,961,784 bytes で既定より 24.0% 小さい（PERFORMANCE.md）
- [x] API 契約: エラー形式をコードと一致させ、`docs/openapi.yaml` と契約ドリフトテストを追加
- [x] エンジン契約: 全エンジン × ページ 1/2/1000 のリクエスト構築をテスト（panic・ページ固有の失敗を検出）
- [x] リクエストボディ上限を 64 KiB に明示（413、回帰テスト）
- [x] 検索レイテンシの平均（`searxng_search_duration_seconds_sum/_count`）
- [x] 評価ハーネスの失敗検出（`--restart-binary` の起動失敗を黙って無視しない）

## Phase 4 — Independent Search Technology

- [ ] 適応的ルーティング、rank fusion の変更（測定基盤は既存。変更は未実施）
- [x] 評価手順の再現性確認（bench/results と tools/eval_quality.py の条件を確認）
- [x] ローカル索引のオフライン評価ツール（`tools/eval_local_index.py`、Hit@1/5/10・MRR・nDCG・レイテンシ、`--baseline`/`--min-hit5`）
- [x] ランキング定数の A/B（TITLE_BOOST 3.0 vs 4.0）: 効果なしとして不採用、計測手順を文書化
- [x] 本文抽出の回帰コーパス（5 fixture + manifest、`cargo test --test extraction`）

## Phase 5 — Ecosystem, Adoption and OSS Maturity

- [x] ドキュメント間の不一致（README ja/ko の MCP 記述差）修正
- [x] OpenAPI 仕様（`docs/openapi.yaml`）と、コード上限値との不一致を検出するテスト
- [x] `/readyz` による readiness（生存確認と検索可能性の分離）
- [ ] 軽量クライアント SDK の必要性判断（OpenAPI + 既存 HTTP クライアントで足りるか）
- [x] 独立監査用の資料一式（`final-audit/`）
- [x] `/readyz`（検索可能性の readiness。プロセス生存と区別する）
- [x] `/metrics` の拡張（readiness・キャッシュエラー・レート制限）と [OBSERVABILITY.md](../OBSERVABILITY.md)
- [x] ブランド資産（ロゴ・wordmark・favicon）の同梱と README/HTML への反映
- [x] 設定検証: cache / research / base_url の誤設定を起動前に拒否（`Settings::validate`）
- [x] オフライン smoke に `/readyz` と MCP stdio（initialize / tools/list / search）を追加
- [x] `SIGTERM` での graceful shutdown（コンテナ / systemd の停止で処理中リクエストを完了）
- [x] HTTP 応答のステータスクラス別メトリクス（`searxng_http_responses_total`）
- [x] `tools/check_bench.py` の回帰テストと CI 範囲の明文化（R4）
- [x] コンテナイメージの実ビルドと実行検証（非 root、/readyz、--check-config、SIGTERM で終了 0）
- [x] Phase 3.3 の判断: URL 検証は接続時検査を維持（型は導入しない、D19）
- [x] 監視ドキュメントとメトリクス名のドリフト検査
- [x] CI のオフライン手順をローカルで再現（markdownlint・example ビルド・provider inventory・smoke）
- [x] 依存ポリシー（`deny.toml`: licenses / bans / sources / advisories）を CI に組み込み
- [x] 抽出器拒否（413・クエリ不正）も `format=json` では JSON で返す
- [x] 起動前の警告診断（`Settings::warnings` → `--check-config` と起動ログ）
- [x] 文書内の相対リンク検査（`tools/test_doc_links.py`）とリリース計画の更新
- [x] 独立監査用の候補スナップショット（`final-audit/CANDIDATE-STATE.md`）
- [x] エンジン健康状態の可視化（`/stats`・`/metrics`・`--check-config` のサーキット表示と [OBSERVABILITY.md](../OBSERVABILITY.md) の監視手順）
- [ ] 外部クライアント実機確認（環境制約により NOT_TESTED）
