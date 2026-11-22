# KNOWN-ISSUES

| ID | 重大度 | 内容 | 状態 |
| --- | --- | --- | --- |
| R2 | ~~P2~~ 解消 | /image_proxy と /favicon_proxy の limiter・並列上限 | Semaphore=4 と limiter を追加済み |
| R3 | P2 | エンジン側 pageno 演算は checked でない | 入口上限で緩和 |
| R4 | ~~P3~~ 解消 | tools/check_bench.py が CI から呼ばれない | docs/BENCHMARK.md に現行範囲を明記、テスト追加 |
| R5 | P3 | 外部環境依存の検証は ignored 4 件と CI の一部のみ | 定期実行に依存 |
| R6 | P3 | DNS rebinding / 実エンジン応答の再現は未実施 | NOT_TESTED |
| R7 | P3 | ライブ検索の品質は旧構成より低い（Hit@5 64–69） | 外部判断待ち |
| R8 | P3 | プロバイダー条件の記録は法的適合の保証ではない | 文書で明示 |
| N6 | P3 | MCP の予算で pop された URL が記憶されないことのテストは未追加 | 一部のみ |
