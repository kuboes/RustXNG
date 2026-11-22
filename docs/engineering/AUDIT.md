# AUDIT — 旧指摘の現行判定と新規指摘（2026-11-16）

対象: main `f7639ce`（v0.31.2）。判定は実コードのデータフローと既存テストに基づく。
外部ネットワーク・実 DB・実ブラウザーでの再現は行っていない（NOT_TESTED）。

## 旧監査（docs/AUDIT-2026-11-02.md, 対象 fef998b）の現状

| ID | 旧指摘 | 現状 | 根拠 |
| --- | --- | --- | --- |
| A01 | SSRF 判定が接続先を保証しない | 解消 | src/fetch/direct.rs:163-206（全解決 IP の分類、採用アドレスで接続、各リダイレクト再検証）、src/web/ssrf_tests.rs:173-213 |
| A02 | javascript: がリンクに残る | 解消 | src/results.rs:363-377, 547-554（境界で http/https 以外を破棄）、tests/integration.rs の HTML テスト |
| A03 | 応答サイズ上限より前に全本文読み込み | 部分解消 | 本文/画像/favicon は上限付きストリーム読取（src/network.rs:588-607, src/fetch/direct.rs:598-614）。残: 画像・favicon の並列上限なし、raw_content の fan-out 未クランプ → 今回クランプ追加 |
| A04 | /research がレート制限を迂回 | 解消（/research）/ 未解消（/autocompleter）→ 今回修正 | handlers.rs の limiter 呼出、今回 autocompleter に limiter・whitelist・5s 期限 |
| A05 | Cookie/ヘッダーが違う検索でキャッシュ共有 | 解消 | src/network.rs request_cache_key（cookie/header/form/json/proxy を含む SHA-256） |
| A06 | キャッシュ待ちが検索期限の外 | 解消 | src/network.rs:374-396（timeout_at で lock/read/write）、src/cache.rs の期限テスト |
| A07 | timeout_limit=inf で panic | 解消 | f318d72、crate::aggregator::is_valid_timeout_limit による入口検証 |
| A08 | 宣言した MSRV でビルド不可 | 解消 | rust-version 1.89、ci.yml の msrv ジョブ、依存 MSRV 最大 1.89（rusqlite） |
| A09 | 依存監査が失敗し CI に無い | 解消 | .github/workflows/security.yml + tools/check_audit.py。ローカル cargo audit: 0 vuln / 0 warn |
| A10 | サイト指定再検索で Query 設定を失う | 解消 | aggregator の Query 維持（PR #12 由来の修正） |
| A11 | クロール完了判定がリンク追加より先 | 解消 | src/bin/rustxng-index.rs の in_flight 順序 |
| A12 | Valkey の clear が実キーを削除しない | 解消（今回） | キーに `rustxng:cache:` を付与し、`SCAN` + ページ単位 `DEL` に変更。src/cache.rs、回帰テスト追加 |
| A13 | 不正な個別プロキシで直接接続へ戻る | 部分解消 → 今回補完 | 不正値は Build エラー。今回 scheme 別 Proxy（all/http/https）と未知キーの起動時エラーを追加 |
| A14 | LLM の JSON 配列をカンマ分割 | 解消 | fee1a93 |
| A15 | ベンチの startswith 比較が別ページを数える | 解消 | tools/reference_urls.py first_hit_rank とテスト |
| A16 | 大きいページ番号で整数 overflow | 部分解消 → 今回補完 | ハンドラで pageno ≤ 1000 に制限（format 付き 400）。エンジン側 checked 化は未実施 |
| A17 | 部分設定ファイルが既定値まで上書き | 解消 | settings.rs merge_values（値ツリーで deep_merge 後に deserialize） |
| A18 | リリースノート抽出が空 | 解消 | tools/release_tools.py notes（欠落・空で失敗） |

## 今回の新規指摘

| ID | 重大度 | 内容 | 状態 |
| --- | --- | --- | --- |
| N1 | P1 | main の CI が `Format` で赤（3 OS）。rustfmt 未適用の差分が 16 箇所 | 修正済み（cargo fmt --all） |
| N2 | P1 | `format=json/csv/rss` のエラーが HTML テンプレートで返り、クライアントが原因を読めない | 修正済み（format_error）+ 回帰テスト2件 |
| N3 | P1 | `/autocompleter` が limiter 非適用で、`backend=` の任意リストが外部呼出を N 倍化（未知名は DuckDuckGo） | 修正済み（limiter + 既知名のみ + 5s 期限）+ 回帰テスト |
| N4 | P2 | `proxies = {http=…, https=…}` が scheme を無視して全通信を BTreeMap 先頭に送る | 修正済み（scheme 別 Proxy、未知キーは起動時エラー）+ 回帰テスト |
| N5 | P2 | `raw_content_limit` が無制限に設定でき、本文 4MiB × N を同時保持 | 修正済み（1..=10 にクランプ） |
| N6 | P2 | MCP の URL 記憶がインスタンス単位・上限 256 であることがテストで固定されていない | 修正済み（回帰テスト追加） |
| N7 | P3 | `tools/check_provider_policy.py` が clean プロファイルを「standard の engines を継承」として検証しており、実行時構成（engine 0）と不一致 | 修正済み |
| N8 | P3 | 台帳の review_date に失効ルールがなく、規約改定を検知できない | 修正済み（365 日で再確認を要求） |
| N9 | P3 | README ja/ko が MCP の安全記述・accept_language・本文上限で英語版と不一致 | 修正済み |
| N10 | P3 | `tools/check_bench.py` がどのワークフローからも呼ばれず、CHANGELOG の記述と乖離 | 未対応（RISKS R4） |
| N11 | P1 | `/research` に全体期限・同時実行制限がなく、1 リクエストで逐次検索と本文取得を増幅できる | 修正済み（`research.deadline` 既定120s → 504、`research.max_concurrency` 既定2 → 429）+ 回帰テスト2件 |
| N12 | P2 | 画像/favicon プロキシに limiter も並列上限もない（1 件 8MiB を無制限並列で保持） | 修正済み（Semaphore=4、期限付き取得、両ハンドラで limiter）+ 回帰テスト3件 |
| N13 | P3 | プロセス生存確認（`/healthz`）と「検索できるか」を区別する手段がない | 修正済み（`/readyz` を追加。engine 0 かつ索引なしで 503）+ 回帰テスト |
| N14 | P3 | 機械可読な HTTP API 仕様が無く、契約がドキュメント散文のみ | 修正済み（`docs/openapi.yaml` + `tools/test_openapi.py` による上限・パラメータ・ルートのドリフト検査） |
| N15 | P3 | メディア取得の同時数が呼び出し側の自由で、8MiB を並列に保持できた | 修正済み（Semaphore=4、期限付き取得、テスト 2 件） |
| N16 | P3 | 表の抽出がセルを空白で連結し、列の境界が失われていた | 修正済み（Markdown 風の行、ヘッダー区切り、エスケープ、上限）+ テスト 4 件 |
| N17 | P3 | 設定の妥当性を起動前に検証する手段がない（起動して初めて分かる） | 修正済み（`--check-config`、`/readyz` と共通の判定）+ CLI テスト 4 件 |
| N18 | P3 | CI が `/healthz` の 200 しか見ておらず、readiness と設定検証が回帰しても検出できなかった | 修正済み（test ジョブに `--check-config`、docker ジョブに `/readyz` とコンテナ内 `--check-config`）+ リポジトリ外でのローカル検証 |
| N19 | P3 | 数式が二重に出る（KaTeX の視覚コピー、MathML の markup と TeX annotation） | 修正済み（重複マークアップを捨て、annotation を優先）+ テスト 3 件 |
| N20 | P3 | 表の `<caption>` が抽出結果から消える（行だけを走査していた） | 修正済み（キャプションを先頭行に出す）+ テスト 1 件 |
| N21 | P3 | ページクローム（`nav` / `footer` / `hidden` / `aria-hidden` / `role=navigation`）が本文に混ざる | 修正済み（構造シグナルのみで除去）+ テスト 2 件。実ページ 2 件では本文コンテナが選ばれるため削減量は 0（NOT_MEASURED） |
| N37 | P2 | `/autocompleter`・画像/ファビコンの limiter が JSON API のバケットを消費し、ページ表示で自分が 429 になっていた（上流は非 HTML のみ加算） | 修正済み（media 3 経路を html バケットへ）。回帰テスト 2 件（順序とバケット分離） |
| N38 | P2 | `/readyz` が `meta.json` の存在だけで判断し、壊れた索引でも ready=200 を返していた | 修正済み（`Searcher` が実際に開けた索引を渡す）。壊れた索引で 503 のテスト |
| N39 | P3 | `EngineStates` の `expect` が Mutex poison で全検索を panic させ、suspension 秒に上限が無かった | 修正済み（`unwrap_or_else(into_inner)`、`validate_suspension` で 1 年上限）+ テスト |
| N40 | P2 | CSV のセルが `=+-@` で始まると表計算ソフトが数式として実行し得た（CWE-1236） | 修正済み（先頭アポストロフィで無害化）+ テスト（コミット済み） |
| N41 | P2 | engine クライアントが DNS/リダイレクト/本文サイズを検証していなかった | 修正済み（`validated_resolver`、リダイレクト各段検証、8 MiB 上限）。設定済み engine ホストのみ例外 |
| N43 | P1 | `src/i18n.rs`（2,985 行）と `src/wikidata_infobox.rs`（670 行）が未参照のデッドコードで、翻訳表と JSON 983 KB を同梱していた | 削除済み（参照・include・build.rs・テンプレート・公開 API を全確認）。src LOC 86,859→83,202、バイナリは −448 B（リンカが既に未使用 section を落としていた）、起動と RSS は不変 |
| N47 | P1（研究） | ルーティングがクエリ種別・能力・遅延履歴を見ず、実行戦略の比較も無かった | シミュレーション研究を実施（`826bbe5`）。capability-aware / category-aware が全 8 状態で現行 parallel を支配（MRR 0.539→0.745、要求 −66%）が、能力仮定に効果量が依存（offcap 0.35→0.70 で +0.206→+0.047）。既定は変更せず、実 A/B を前提条件として記録 |
| N48 | P2 | ルーティング比較の再現基盤が無かった | `examples/routing_lab.rs` + `bench/local-retrieval/routing-lab/`（README・集計・per-query）を収録。再現コマンドは README |
| N46 | P2 | 本文抽出のコーパスが 5 ページのみで、広告・同意ブロックの混入と引用元の消失を検出できなかった | 30 ページへ拡張（すべて合成 HTML）。欠落 2 件を修正（限定 class/id 判定、引用 footer の保持）+ 回帰テスト 2 件 |
| N45 | P2 | engine 例外が base_url のみで search_url 設定の自己ホスト engine が動かず、proxy 使用時は resolver 検証が効かず `nip.io` 経由の private 到達が成立、別 engine へのリダイレクトも未制限だった | 修正済み（例外は有効 engine の base_url + search_url ホストのみ、proxy 時 cross-origin 拒否、別 engine ホストへのリダイレクト拒否）+ 単体テスト 2 件 |
| N44 | P2 | graceful shutdown の drain に期限が無く、`/research` 中は systemd/docker の猶予で強制終了され得た | 修正済み（`server.shutdown_grace` 既定 10 秒、期限後はランタイム停止で子プロセスも kill_on_drop）。SIGTERM の drain 内完了と期限超過の 2 テスト |
| N42 | P3 | NOTICE に CDLA-Permissive-2.0（webpki-roots）が無く、`/about` に AGPL §13 の source リンクが無かった | 修正済み |
| N36 | P3 | 文書内の相対リンクが実在するかを検査しておらず、移動・改名で静かに壊れ得た | 修正済み（`tools/test_doc_links.py`: README と docs/** の相対リンクを解決。初回実行で破損 0 件） |
| N35 | P3 | 有効なまま意図と異なる設定（`max_page = 0`、非ループバックで limiter 無効、metrics 無効、research 有効で鍵なし）を診断する手段がなかった | 修正済み（`Settings::warnings`。`--check-config` の `warnings` と起動時ログに出す）+ 単体・CLI テスト |
| N34 | P2 | axum がハンドラ前に返す拒否（ボディ超過・クエリ不正）は text/plain で、`format=json` のクライアントだけが JSON を読めなかった | 修正済み（`web::router` のミドルウェアで `format=json` の 400/413/415 を JSON に整形）+ 回帰テスト（413 と `pageno=abc`） |
| N33 | P2 | ライセンス・禁止事項・取得元の依存検査が CI に無く、`cargo audit`（脆弱性のみ）だけだった | 修正済み（`deny.toml` を追加し `cargo deny --locked check` が licenses / bans / sources / advisories すべて ok。security ワークフローに 0.20.2 固定で追加）+ docs/DEPENDENCIES.md |
| N32 | P3 | 新規文書 2 件が MD047（末尾改行）違反で、CI の markdownlint を落とす状態だった | 修正済み（末尾を単一改行に）。CI と同じ glob で markdownlint をローカル実行し 38 files / 0 issues を確認 |
| N31 | P3 | `docs/OBSERVABILITY.md` の系列名がコードとずれても検出できなかった | 修正済み（`tools/test_docs_metrics.py`: 出力される全系列が文書化され、文書の系列が実在することを検査。ワイルドカードと sum/count の対は許容） |
| N30 | P2 | `eval_quality.py --restart-binary` が起動失敗を黙って見逃し、別プロセスや停止中のポートを測り続ける可能性があった | 修正済み（バイナリ不在は ValueError、`fuser` 不在・即時終了・未 ready は RuntimeError で終了コード 2。レポートの `base` も実測先に修正）+ テスト 3 件 |
| N29 | P3 | 検索レイテンシがメトリクスに出ておらず、SLO の平均値を追えなかった | 修正済み（`searcher.search` で所要時間を記録し `searxng_search_duration_seconds_sum/_count` を公開）+ 単体・統合テスト |
| N28 | P2 | リクエストボディの上限が axum 既定の 2 MiB のままで、アプリ側で説明・制御できていなかった | 修正済み（`web::router` に `DefaultBodyLimit::max(64 KiB)`。超過は 413）+ 回帰テスト |
| N27 | P3 | HTTP 応答のステータスクラスが観測できず、エラー率のアラートを作れなかった | 修正済み（`web::router` のレイヤで `record_status`、`searxng_http_responses_total{status}` を公開）+ 単体・統合テスト |
| N26 | P2 | `SIGTERM` を処理しておらず、`docker stop` / `systemctl stop` で強制終了（143）していた。処理中リクエストの完了を待たない | 修正済み（Unix で SIGTERM を select し graceful shutdown。CLI テストで終了コード 0 を検証。旧バイナリが 143 になることも実測） |
| N25 | P3 | オフライン smoke が `/healthz` と HTTP 検索しか見ておらず、`/readyz` と MCP stdio の退行を検出できなかった | 修正済み（smoke に `/readyz` と `tools/mcp_smoke.py` による initialize / tools/list / search を追加。CI の test と arm64 ジョブで実行される）+ 3 ケースの実機パス |
| N24 | P3 | `cache.backend` / `cache.url` / `research.deadline` / `server.base_url` の誤設定が起動時に検出されず、実行時に黙って無効化・クランプされていた | 修正済み（`Settings::validate` に 3 系統の検証を追加、`--check-config` で事前に検出）+ テスト 3 件 |
| N23 | P3 | `/favicon.ico` が 204 を返し、ロゴ資産もドキュメントから参照されていなかった | 修正済み（`static/` に mark / wordmark / favicon を配置してバイナリへ埋め込み、全 HTML に `rel="icon"`、README 3 言語にロゴ、NOTICE に出所を明記）+ テスト |
| N22 | P3 | `/metrics` に readiness・キャッシュエラー・レート制限の状態が出ておらず、アラート設計ができない | 修正済み（`searxng_ready` / `searxng_engines_configured` / `searxng_local_index` / `searxng_cache_error_total` / `searxng_cache_bytes` / `searxng_limiter_*`）+ テスト 1 件 |
