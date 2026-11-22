# SSRF マトリクス — 外部リクエスト経路の検証状況（2026-11-21）

対象: main `9a89415`。外部への HTTP / TCP 接続を行う経路を、実コードのデータフローで分類した。
外部ネットワークでの再現は行っていない（判定はコードと既存テストに基づく）。

## 結論

- 信頼できない入力から出る経路（検索結果の URL、利用者が指定した URL、取得先の robots.txt / sitemap /
  リダイレクト）は、すべて `src/fetch/direct.rs` の同じ 2 段で検証される。第 1 段 `is_fetchable` は URL の
  リテラル（スキームとホスト）を、第 2 段 `PublicResolver` は解決後のアドレスを見る。
- 検証を伴わずに接続しうるのは「事業者が設定した固定宛先」だけで、これは意図的な免除である。
  検索エンジンの `base_url`、外部ページバックエンド、調査用 LLM、埋め込み、更新の各エンドポイントが該当する。
- 監査時に 1 件の欠落を見つけ、同一の変更で塞いだ: `src/update.rs` のダウンローダーはリダイレクトを
  既定方針（最大 10 ホップ）で追い、飛び先を検査していなかった。現在は 1 ホップごとに `is_fetchable` を
  通し、上限も明示している（`redirect_policy`）。宛先が固定で取得物がチェックサム照合される点は変わらない。
  それ以外に、私的なリテラルアドレスへ到達できる経路は見つからなかった。

## 検証の部品

| 部品 | 場所 | 役割 |
| --- | --- | --- |
| `is_fetchable` | `src/fetch/direct.rs:135` | http(s) のみ。リテラルの IPv4 / IPv6 は `is_global` で判定し、`localhost` と `*.localhost` を拒否する。ホスト名はここでは通し、解決後に判定する |
| `is_global` | `src/fetch/direct.rs:84` | ループバック・私的・リンクローカル・CGNAT・文書用・マルチキャスト等を除外する。IPv4 射影・NAT64・6to4 の埋め込みアドレスも展開して判定する |
| `PublicResolver` | `src/fetch/direct.rs:173` | 解決は 1 回だけ。非グローバルを回答から除き、残りをそのまま接続先に渡す（再解決しないので DNS リバインディングが成立しない）。公開アドレスが 1 つも無ければ失敗する |
| ページ取得のリダイレクト方針 | `src/fetch/direct.rs:257` | 各ホップを `is_fetchable` で検査する。5 ホップまで |
| `engine_redirect_policy` / `redirect_allowed` | `src/network.rs:204` / `src/network.rs:138` | 同一オリジンは許可（正規化リダイレクト）。別オリジンは公開のみ。プロキシ経由では別オリジンをすべて拒否。別の設定済みエンジンは飛び先にできない |
| `AllowedResolver` / `validated_resolver` | `src/fetch/direct.rs:212` | 事業者が設定したエンジンのホスト名だけ、非グローバルの回答も許す。他の名前は `PublicResolver` が判定する |
| プロキシ無効化 | `src/fetch/direct.rs:266`, `src/fetch/direct.rs:743` | 検証付きのクライアントは `.no_proxy()`。明示プロキシ設定時はページ取得とメディア取得を fail-closed にする（プロキシが名前解決を代行すると検証が外れるため） |

## 経路

| # | 経路 | 入口 | 適用される検証 | 欠落 / 免除 |
| --- | --- | --- | --- | --- |
| 1 | 検索エンジン（`src/network.rs`） | `aggregator` のエンジン実行 → `Network::fetch`（`:475`）→ `send`（`:687`） | `validated_resolver`（設定済みエンジンのホストだけ免除）＋ `engine_redirect_policy`。per-engine プロキシのクライアントは別オリジンのリダイレクトを全拒否（`:426`） | 免除: 初期 URL は `is_fetchable` を通さない。`base_url` / `search_url` は事業者設定で、私設エンドポイントを許すため。信頼できない URL はこの入口に来ない |
| 2 | ページ読み取り（`src/fetch/direct.rs` の `PageReader`） | MCP `read_page`（`src/mcp/mod.rs:291`）、`/search?include_raw_content=1`、`/map`、`/extract`、`/research` → `read`（`:467`） | `is_fetchable`（`:489`）→ `PublicResolver` 付きクライアント → 各リダイレクトを `is_fetchable`。プロキシ設定時は fail-closed（`:482`）。本文バイト上限。MCP はそのセッションの検索が返した URL だけを許可する | なし |
| 3 | favicon（`src/web/favicons.rs`） | `GET /favicon_proxy`（`:91`） | オリジンだけを抽出（`:54`）→ HMAC 署名の検証（`:123`）→ `is_public_url`（`:126`）→ `Network::fetch_image` が再度 `is_fetchable`（`src/network.rs:522`）とメディア用クライアント（`PublicResolver`、リダイレクト検査）。プロキシ時は fail-closed | なし。パスとクエリは署名対象の外なので、そもそも取得しない |
| 4 | 画像プロキシ（`src/web/image_proxy.rs`） | `GET /image_proxy`（`:88`） | URL 全体の HMAC を定数時間で比較（`:114`）→ `is_public_url`（`:117`）→ `fetch_image` が再度 `is_fetchable` とメディア用クライアントを使う | なし。`Referer` をそのまま上流へ渡す（SSRF ではないが、プライバシー上の注意点） |
| 5 | ページレンダラーのリソース（`src/fetch/render.rs`） | レンダラーからの `{"type":"fetch"}` → `resource`（`:212`） | リソース URL を `is_fetchable`（`:220`）、GET / HEAD のみ。クライアントは `PageReader` と同一（`PublicResolver` ＋ リダイレクト検査）。リダイレクトされた応答は本文を捨てる（`:233`）。バイト上限 | なし（公開先へのリダイレクトは接続してから捨てる。私設へは到達しない） |
| 6 | クローラー（`src/bin/rustxng-index.rs`＋`src/crawl.rs`） | `rustxng-index` のシード・sitemap・robots.txt → `request`（`src/bin/rustxng-index.rs:665`） | `public_blocking_client` は `redirect(Policy::none())` ＋ `PublicResolver`（`src/fetch/direct.rs:303`）。リダイレクトは 1 ホップずつ `is_fetchable` ＋ 同一サイト ＋ スコープで検査する。sitemap の URL も使う前に同じ検査を通す（`sitemap_urls`、`src/bin/rustxng-index.rs:722`） | なし |
| 7 | 外部ページバックエンド（`PageSource` 実装、`src/fetch/`） | `[fetch]` で provider を選んだときの `PageReader` | 呼び出し前に `PageReader` が `is_fetchable`。エンドポイントは `api_client`（リダイレクトを追わない）。`is_supported_url`（`src/fetch/deepseek.rs:82`）が資格情報付き・長すぎる URL を拒否する | 免除: 実際の取得は事業者側で行われ、このプロセスの私設網には到達しない。事業者側の解決結果は制御できない。既定は無効でキーが必須 |
| 8 | 更新ダウンローダー（`src/update.rs`） | `rustxng --update` / `--check-update` | 宛先は固定の https 定数。`https_only(true)`（`:92`）、アセット URL は期待する接頭辞と完全一致（`:43`）、取得物は SHA-256 で照合する | 欠落: リダイレクトは既定方針（最大 10 ホップ）で追い、飛び先を検査しない。入力は利用者ではなく、宛先は固定。修正案は「残作業」 |

### 監査で追加確認した経路

| 経路 | 入口 | 検証 | 判定 |
| --- | --- | --- | --- |
| 構造化データ API（`src/official.rs:293`） | 検索後の公式サイト解決 | `Network::fetch`（経路 1 と同じ） | 検証付き。エンドポイントは定数 |
| 天気 answerer（`src/answerers/weather.rs:142`） | キーワード応答 | `Network::fetch` | 検証付き。エンドポイントは定数 |
| オートコンプリート（`src/autocomplete.rs:56`） | `/autocompleter` | `Network::fetch` | 検証付き。バックエンドは設定または定数 |
| 出口ノード確認プラグイン（`src/plugins/tor_check.rs:71`） | キーワード応答 | `Network::fetch` | 検証付き。エンドポイントは定数 |
| 調査用 LLM（`src/deep_research.rs:60`） | `/research` | 事業者設定の `base_url`。`.no_proxy()` 無し、リダイレクトは既定方針 | 免除（設定済みエンドポイント）。利用者入力は URL にならない |
| 埋め込み（`src/aggregator.rs:1424`） | 意味検索のランキング | 事業者設定の `embedding_url`。同上 | 免除（設定済みエンドポイント） |
| 文献データベースの補助 HTTP（`src/engines/pubmed.rs:86`） | オフラインエンジン | 独自クライアント。エンドポイントは定数、リダイレクトは既定方針 | 免除（定数）。公開ホストのみ |
| キー・バリューキャッシュ（`src/cache.rs:608`）／同エンジン（`src/engines/valkey_server.rs:71`） | キャッシュ読み書き・オフラインエンジン | 事業者設定の `host:port` へ生 TCP。HTTP ではない | 免除（事業者設定）。SSRF の判定対象外 |
| サイト地図（`src/extract.rs:85`） | `/map` | 取得先 robots.txt が名指しした sitemap も `PageReader` 経由（経路 2 と同じ検証） | 検証付き。オリジン外の sitemap も取得しうるが、公開ホストに限られる |

## 既存テストの被覆

| テスト | 場所 | 何を固定しているか |
| --- | --- | --- |
| `every_fetch_route_rejects_private_dns_before_connecting` | `src/web/ssrf_tests.rs:173` | 4 経路（image_proxy / favicon_proxy / raw content / research）で、私設へ解決する名前に対して接続そのものが起きないこと |
| `every_fetch_route_revalidates_literal_and_hostname_redirects` | `src/web/ssrf_tests.rs:189` | リテラルとホスト名のリダイレクトを各経路で再検証すること |
| `engine_redirects_may_not_leave_for_a_private_destination` ほか | `src/network.rs:2035` 付近 | エンジンのリダイレクト境界（別オリジン・プロキシ・別エンジン） |
| `literal_hosts_and_schemes_are_checked` | `src/fetch/direct.rs:853` | 10 進・8 進・16 進・末尾ドット等のリテラル表記が正規化されたうえで拒否されること |
| `configured_proxies_fail_closed` | `src/fetch/direct.rs:883` | プロキシ設定時にページ取得が失敗すること |

## 残作業

1. 環境プロキシ（`HTTPS_PROXY` 等）を使う設定では、プロキシ自体が飛び先を決める。更新ダウンローダーは
   現在も環境プロキシに従う。プロキシ運用では検査済みの 1 ホップ目しか保証できないため、`.no_proxy()` を
   強制するかは運用判断として残す。
2. 調査用 LLM と埋め込みのクライアントは、設定エンドポイントという信頼境界のままにする。リダイレクトを追う点は
   エンジンの `base_url` と同じ扱いであることを、設定のコメントに明記する。
3. 免除の範囲は「有効なエンジンのホストのみ」。無効化したエンジンのホストは免除されない
   （`configured_engine_hosts` は `enabled_engines` だけを見る）。この性質は維持する。

## パーサー側（fuzz）

リクエスト経路とは別に、信頼できないバイト列を解析する関数は [fuzz harness](../../fuzz/README.md) で継続的に確認する。
2026-11-21 に 1 件を検出し、同じ変更で修正した: RESP の応答が告知したバルク長を、読む前にそのまま確保していた
（`src/engines/valkey_server.rs` と `src/cache.rs`）。`$999999999999\r\n` の 1 行でプロセスが確保に失敗して
落ちる。現在は共有の `bounded_resp_length` が、解析できない値・負値・上限超過を読み込み前にエラーにする。
配列の入れ子にも上限（`MAX_REPLY_DEPTH`）を置いた。再現手順と、修正後も同じ入力で落ちないことの確認方法は
[fuzz/README](../../fuzz/README.md) にある。
