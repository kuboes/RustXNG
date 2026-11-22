# CURRENT_STATE — RustXNG

基準日: 2026-11-16 / 基準コミット: f7639ce（v0.31.2、main, origin/main と一致）

## 何であるか

SearXNG を Rust で書き直した self-hosted メタ検索エンジン。外部プロバイダーの結果を統合し、
JSON API と MCP で AI エージェントへ本文を渡せる。web-scale のインデックスは持たず、
選択したサイトのローカル索引（Tantivy）と Wikipedia ダンプ索引を別途取り込む。

想定プロダクトビジョン: Open Search Infrastructure for AI Agents（人間向け検索と
SearXNG 互換性は維持）。

## 構成（実測）

| 項目 | 値 | 根拠 |
| --- | --- | --- |
| パッケージ | rustxng 0.31.2, edition 2024, MSRV 1.89 | Cargo.toml |
| バイナリ | rustxng / rustxng-index / rustxng-wikidump | Cargo.toml, src/bin |
| エンジン | src/engines/*.rs 272 ファイル | ls src/engines |
| 設定のエンジン数 | 273（有効 34 / 無効 239） | settings.toml, provider_inventory.py --check |
| プロファイル | standard（既定）/ clean / full | settings.toml 系 |
| テスト関数 | 1141（tests 51 + src 1088 + examples 2） | grep 計測 |
| Python テスト | 66 | python -m unittest discover -s tools |
| CI | ci / security / docker / release / nightly-bench | .github/workflows |

## 検索プロファイル

- standard: 文書化された API / RSS / JSON の有効 34 エントリのみ。HTML 検索結果は取得しない。
- clean: `use_default_settings = false`。外部検索なし。ローカル索引のみ。
- full: HTML プロバイダーと課金 API（Exa）を含む明示 opt-in。起動時に利用条件の注意を表示。

プロバイダーの条件は `docs/PROVIDERS.md`、機械可読な台帳は `docs/provider-policy.json` と
`docs/provider-review-pending.json`。台帳の検証は `tools/check_provider_policy.py`（CI 実行）。

## 既知の制約

- 外部ネットワーク・実 DB・実ブラウザーに依存する検証は CI の一部ジョブと ignored テストに限定。
- 検索品質は 2026-11-05 / 11-08 の 100 問で Hit@5 64–69。旧 API 併用構成より低い（意図的な
  プロバイダー除外の結果）。数値は履歴であり現行版の再測定ではない。
- ライブ検索の品質は上流の状態に依存する。オフライン索引の評価とは条件が違う。

## 参照

- 監査（2026-11-02）: docs/AUDIT-2026-11-02.md
- 性能（2026-11-05）: docs/PERFORMANCE-2026-11-05.md
- 課題解決と測定: docs/ISSUE-RESOLUTION-2026-11-05.md, -11-06.md
- 互換性: docs/COMPATIBILITY.md、提供元条件: docs/PROVIDERS.md
- 本ディレクトリ: ROADMAP.md, AUDIT.md, PERFORMANCE.md, DECISIONS.md, RISKS.md, EXECUTION_STATE.md
