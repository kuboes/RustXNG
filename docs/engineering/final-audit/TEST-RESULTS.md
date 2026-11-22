# TEST-RESULTS

最終確認: 2026-11-16 21:33（Asia/Tokyo）

| コマンド | 結果 |
| --- | --- |
| `cargo fmt --all -- --check` | 成功（変更前は失敗） |
| `cargo clippy --locked --all-targets -- -D warnings` | 成功 |
| `cargo test --locked --all-targets` | 1178 passed / 4 ignored / 0 failed |
| `cargo check --locked --all-targets --no-default-features` | 成功 |
| `cargo build --release --locked --bin rustxng` | 成功（35,492,584 bytes） |
| `python3 -m unittest discover -s tools -p "test_*.py"` | 86 tests OK |
| `python3 tools/check_provider_policy.py` | 成功 |
| `python3 tools/provider_inventory.py --check` | 一致 |
| `cargo audit` | 0 vulnerabilities / 0 warnings（442 deps、advisory DB 2026-11-15） |
| `cargo deny --locked check` | advisories ok / bans ok / licenses ok / sources ok（`deny.toml`。CI の security ワークフローでも実行） |
| `npx markdownlint-cli2@0.24.0 "*.md" "docs/*.md" "tools/page-renderer/*.md"` | 38 files / 0 issues（初回は `docs/OBSERVABILITY.md` と `docs/README.md` の末尾改行で 2 件検出し修正） |
| `cargo build --locked --example benchmark` | 成功 |
| `python tools/smoke_agent_setup.py --binary target/debug/rustxng` | **3 ケース PASS**（default-port / cli-port / env-port）。`/readyz` と MCP stdio（initialize / tools/list / search）を含む。一度 29483 が他プロセスに保持されて default-port だけスキップしたが、解放後に 3/3 成功を確認 |

内訳（passed）: lib 1084、integration 37、extraction 2、main 6、rustxng-index 19、rustxng-wikidump 2、cli 4、integration 34、
mcp 12、media 1、network_coalescing 6、pdf 1、rendered_pages 1、examples 2。
ignored 4: mysql 1、rendered_pages 2、src/mcp/benchmark.rs 1（外部環境が必要）。

追加したテスト（本セッション）: エラー形式 2、pageno/クエリ長 2、autocomplete 1、research 期限・同時実行 2、
メディア limiter 1、readyz 1、MCP 記憶 1、Valkey clear 1、proxy scheme 1、メディア slot 2、
表抽出 4、表キャプション 1、ページクローム 2、数式の重複除去 3、脚注・定義リスト 2、CLI 4、readiness 判定 1、
Python: check_bench 4、OpenAPI 契約 5、ローカル索引評価 4、評価ハーネス 4（既存 1 + 追加 3）、監視ドキュメント 2、文書リンク 1、台帳失効 1。

コンテナイメージの検証（2026-11-17。この環境の docker は podman エミュレーションのため podman build を使用）:

| 確認 | 結果 |
| --- | --- |
| イメージビルド | 成功、localhost/rustxng:local 153 MB（rust:1.99.1-slim-bookworm でビルド、debian:bookworm-slim で実行） |
| 実行ユーザー | searxng（uid 10001、非 root） |
| /healthz | 200 |
| /readyz | 200、ready:true、engines:34、local_index:false |
| --check-config --profile clean（コンテナ内） | 終了 0、engines:0、ready:false、理由 2 件 |
| podman stop（SIGTERM） | 終了コード 0（graceful） |

Docker 無しでも同じ確認ができる（/tmp に「上書きだけ」の settings.toml を置き SEARXNG_SETTINGS_PATH を設定して release バイナリを起動）: /readyz 200 / engines:34 / local_index:false、SIGTERM で終了コード 0、ローカル索引は開かない。

実行していない検証: 実エンジン応答、実 Valkey、実 MySQL/MariaDB、実ブラウザー（Firefox レンダラー）、
実 LLM 課金、DNS rebinding の実接続再現、Windows/macOS 実機。
## ignored テスト 4 件の実行条件（2026-11-17 時点）

| テスト | 条件 | CI |
| --- | --- | --- |
| `tests/mysql.rs` の 1 件 | `--features db-engines` かつ `MYSQL_TEST_PORT` が指す MySQL/MariaDB が起動していること | `mysql-integration` ジョブ（MariaDB サービス）で `--ignored` 実行 |
| `tests/rendered_pages.rs` の 2 件 | Node.js と `tools/page-renderer`、Firefox が必要 | `javascript-pages` ジョブが `npx playwright install firefox` 後に `--include-ignored` で実行 |
| `src/mcp/benchmark.rs` の 1 件 | 保持済み eval 応答と `RUSTXNG_BUDGET_INPUT/OUTPUT` の指定が必要 | どの workflow でも実行しない（手動） |

ローカルで `--include-ignored` を実行した結果、rendered_pages の 2 件は Firefox 不在で失敗（条件どおり）。
Linux 以外の OS は CI の matrix に委譲し、この環境では検証していない（NOT_TESTED）。

## baseline との比較

| 項目 | f7639ce (v0.31.2) | 936b1e5（中間） | 2722523（最終候補） |
| --- | ---: | ---: | ---: |
| Rust テスト passed | 1177 | 1187 | 1180 |
| うち ignored | 4 | 4 | 4 |
| Python テスト | 85 | 86 | 86 |
| src 行数 | 86,859 | 83,202 | 83,202 + results/rank.rs 移動 |
| release バイナリ | 35,814,192 B | 35,813,744 B | 35.8 MB 前後 |
| 起動 → /healthz | 0.05–0.25 s | 0.186 s | 0.19–0.25 s |
| アイドル RSS | 23.8 MiB | 24.5 MiB | 24.5–24.7 MiB |

テスト件数は目的ではなく、機能削除（デッドコード）と統合で純増しない。
