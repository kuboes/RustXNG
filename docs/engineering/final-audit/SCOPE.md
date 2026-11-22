# SCOPE — 独立監査の対象

- リポジトリ: https://github.com/RustXNG/RustXNG
- Baseline commit: `f7639ce`（v0.31.2、main）
- Candidate: `f7639ce` 上の未コミット作業ツリー（コミットは作成していない。方針上、明示指示があるまでコミットしない）
- 監査対象: 2026-11-16 のセッションで加えた変更（src 8 ファイル、tests、settings.toml、tools 2 ファイル、README 2 件、docs）
- 対象外: 外部ネットワーク・実 DB・実ブラウザー・実 LLM を用いる検証。実エンジンの応答一致。
- 前提: 本資料は実装者自身の記録であり、独立監査の代替ではない。

候補の正確な状態（変更ファイル数・検証結果・未検証項目）は [CANDIDATE-STATE.md](CANDIDATE-STATE.md) を参照。

## 判定してほしい点

1. エラー応答のフォーマット変更が既存クライアントを壊さないか
2. pageno 1000 / クエリ 2048 文字の上限が妥当か（境界の根拠）
3. Valkey のキー名前空間変更と SCAN 実装の正しさ（カーソル、TTL、並行性）
4. `/research` の期限と同時実行制限が新しい詰まりやリークを生まないか
5. proxies の scheme 別解釈が reqwest の挙動と一致するか
6. autocomplete の既知名限定が SearXNG 互換性を損なわないか
7. 台帳チェックの検証モデル変更（clean）と 365 日失効の妥当性
