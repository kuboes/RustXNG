# RISKS — 未解決と追跡項目（2026-11-16）

重大度: P0（即時）/ P1（次リリース前）/ P2（計画対応）/ P3（任意）。

| ID | 重大度 | 内容 | 根拠 | 次の一手 |
| --- | --- | --- | --- | --- |
| R1 | ~~P1~~ 解消 | `/research` の全体期限と同時実行制限（旧指摘） | 修正済み: `research.deadline`（既定120s、超過は 504）と `research.max_concurrency`（既定2、超過は 429）。src/web/handlers.rs, src/settings.rs, tests/integration.rs | limiter 既定 off のままでも増幅は有界 |
| R2 | ~~P2~~ 解消 | 画像/favicon の並列上限と limiter（旧指摘） | 修正済み: `Network.media_slots`（Semaphore=4、期限付き取得）と両ハンドラの limiter。src/network.rs, src/web/image_proxy.rs, src/web/favicons.rs, tests/integration.rs | 残: 実測の負荷試験は NOT_TESTED |
| R3 | P2 | エンジン側の pageno 演算は checked ではない | src/engines/*.rs | 入口上限 1000 を維持。`every_configured_engine_builds_requests_within_the_page_limit` が全エンジン × ページ 1/2/1000 を検証（99 リクエスト構築、open_meteo は設定不足で deferred、ページ固有の失敗 0）。max_page を上げる場合は checked 化が必要 |
| R4 | ~~P3~~ 解消 | `check_bench.py` の CI 未接続と CHANGELOG の乖離 | 修正済み: docs/BENCHMARK.md に現行 CI 範囲を明記し、tools/test_check_bench.py で下限・失敗件数の挙動を固定。CHANGELOG は履歴として変更しない | ライブ nightly の再開は外部判断 |
| R5 | P3 | 外部ネットワーク・実 DB・実ブラウザー依存の検証は CI の一部と ignored テストのみ（4 件） | tests/mysql.rs, tests/rendered_pages.rs, src/mcp/benchmark.rs | 定期実行の維持。結果を docs に残す |
| R6 | P3 | DNS rebinding / 実エンジン応答の再現はコード検証のみで実測していない | src/fetch/direct.rs, src/web/ssrf_tests.rs | ローカル DNS を使った再現手順を追加検討 |
| R7 | P3 | 検索品質（Hit@5 64–69）は旧構成（84）より低い。プロバイダー除外の帰結で、コードでは解決しない | docs/PERFORMANCE-2026-11-05.md, docs/ISSUE-RESOLUTION-2026-11-06.md | 許諾済み広域検索 API の確保、または索引拡充（外部判断） |
| R9 | ~~P1~~ 解消 | デッドコード（i18n / wikidata infobox）| 2026-11-17 に削除（コミット cc34686）。判断: 参照・include・build.rs・テンプレート・公開 API を確認し、外部利用は無し |
| R10 | ~~P2~~ 解消 | shutdown drain の期限 | `server.shutdown_grace`（既定 10 秒）を追加（コミット 71388da）、2 テストで検証 |
| R11 | ~~P2~~ 解消 | engine 例外の適用範囲 | 2026-11-17 修正（a89ded3）。有効 engine の base_url + search_url ホストのみ、proxy 時は cross-origin 拒否、別 engine ホストへのリダイレクト拒否。リダイレクト先/結果 URL/画像/MCP には非適用 |
| R12 | P2 | 多ソース融合評価の fixture が商用 API ダンプ（gitignore）依存で、クリーンクローンでは n=13 しか再現できない | `.gitignore` に対象 JSON、`bench/local-retrieval/fusion-lab/README.md` に注記 | URL+順位のみの派生 fixture を追跡下に作る（n≥100） |
| R14 | P2 | capability-aware ルーティングの効果量は仮定依存で、実エンジン A/B が無い | `routing_lab` の offcap 感度（MRR 優位 +0.206 → +0.047）、F5（deadline 指標が判別不能） | ライブ検索の許可・予算を得て A/B。それまで既定は不変（D23） |
| R13 | P3 | 既存の融合層は「単一プロバイダ単独」に有意に勝てない（ablation: position-only 0.768 < solo:exa 0.835、p=0.0015） | fusion-lab の実測（ローカル限定 fixture） | 主張を「融合層」と「後段の lexical/authority」に分けて記録 |
| R8 | P3 | プロバイダー条件の確認は「記録」であり法的適合の保証ではない | docs/PROVIDERS.md, docs/provider-policy.json | 利用者向けの注意書きを維持 |