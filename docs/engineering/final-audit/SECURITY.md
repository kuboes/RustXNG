# SECURITY — 今回の変更と残存リスク

## 修正した境界

| 項目 | 変更 | 検証 |
| --- | --- | --- |
| エラー応答の情報 | json/csv/rss で HTML を返さない（原因文字列は従来どおり） | tests/integration.rs の新規テスト |
| 入力上限 | pageno ≤ 1000、q ≤ 2048 文字 | 同上 |
| 外部呼出の増幅 | /autocompleter は既知バックエンドのみ・5 秒期限・limiter 対象 | 同上 |
| 資源消費 | /research 全体期限 120 秒、同時 2 件（超過は 429/504） | 新規テスト 2 件 |
| メモリ | raw_content の同時取得を 10 件まで | コード（クランプ）+ 既存の本文上限テスト |
| 経路の保証 | proxies の scheme 別解釈、未知キーは起動時エラー | src/network.rs の新規単体テスト |
| キャッシュ | Valkey キーの名前空間分離、clear は SCAN | src/cache.rs の新規テスト（ページ送り含む） |

## 既存の防御（今回触っていないが確認した）

- 非信頼 URL の接続時 IP 検証と各リダイレクト再検証（src/fetch/direct.rs）
- 結果境界での http/https 以外の URL 破棄（src/results.rs）
- 圧縮展開後のストリーム上限（本文・画像・favicon）
- MCP の read_page は同一セッションで search が返した URL のみ

## 残存リスク

- 画像/favicon プロキシに並列上限と limiter がない（RISKS R2）
- エンジン側 pageno 演算は checked ではない（入口上限で実害は抑制）
- 実接続での SSRF/DNS rebinding 再現は未実施（コード検証のみ）
- プロバイダー条件の記録は法的適合の保証ではない
