# DECISIONS — 2026-11-16

ADR 形式。各決定に「理由」「代替案」「影響」を付ける。既存の設計判断は
`docs/PROVIDERS.md` とコードのコメントを正とし、ここには今回の判断だけを残す。

## D1: エラー応答を要求フォーマットに合わせる

- 決定: `/search` の 4xx は要求フォーマットで返す（json → `{"error": …}`、csv/rss → text/plain、html → テンプレート）。
- 理由: `format=json` のクライアントが HTML を受け取ると原因を読めず、監視・自動化が壊れる。
- 代替案: すべて text/plain に統一（既存 HTML 利用者を壊す）、JSON のみ特別扱い（CSV/RSS が残る）。
- 影響: 4xx の本文形式が変わる。ステータスコードは不変。`ErrorTemplate` は HTML 経路だけが使う。

## D2: pageno は入口で 1000 に制限する

- 決定: `search.max_page = 0`（無制限）でも入口で 1000 を上限にする。
- 理由: 各エンジンが pageno を offset に掛けるため、u32 全域の値でオーバーフローする（旧 A16）。
- 代替案: 55 エンジンすべてを checked arithmetic に書き換える（変更範囲が広く、抜けが残る）。
- 影響: 1000 ページ超の要求は 400。実用上の結果は 1000 ページより手前で尽きる。

## D3: クエリ長は 2048 文字まで

- 決定: `q` が 2048 文字を超えたら 400。
- 理由: 全エンジンの URL 構築とクエリ解析に流れる入力で、上限がない。
- 影響: 通常の検索語では到達しない。長文貼り付けは明示的に拒否される。

## D4: Valkey のキーに名前空間を付け、clear は SCAN で消す

- 決定: すべてのキーを `rustxng:cache:` 配下に置き、`KEYS` ではなく `SCAN`（COUNT 256）で消す。
- 理由: 旧実装は `KEYS searxng:*` で、実際のキー（SHA-256 hex）と一致せず何も消えなかった。
  `KEYS` は大規模 DB をサーバー側でブロックする。
- 代替案: 無名キーのまま `SCAN MATCH *`（同居アプリのキーまで消す）。
- 影響: 旧バージョンが書いたキーは TTL まで残るが、新実装からは参照されない。ロールバックしても
  旧キーが読めなくなるだけで、キャッシュは再構築される。

## D5: autocomplete は既知バックエンドのみ、1 リクエスト 5 秒

- 決定: `backend=` の各要素を `BACKENDS`（duckduckgo / wikipedia / mwmbl）に限定し、
  重複を除き、ハンドラ全体を 5 秒で打ち切る。limiter を適用する。
- 理由: 未知名が DuckDuckGo に流れ、N 個の候補で外部呼出が N 倍化していた。無認証で反復可能。
- 影響: 未知名を指定した場合の挙動が「DuckDuckGo の結果」から「無視」に変わる。

## D6: proxies はキーの scheme を尊重する

- 決定: `all` / `http` / `https` をそれぞれ `Proxy::all/http/https` に割り当て、他のキーは起動時エラー。
- 理由: 旧実装は全キーを `Proxy::all` で登録し、BTreeMap 昇順の先頭が全通信を奪っていた。
- 影響: `proxies` に未知キーを持つ既存設定は起動しなくなる（意図した fail-closed）。

## D7: raw_content の同時取得は 10 件まで

- 決定: `search.raw_content_limit` を 1..=10 にクランプ。
- 理由: 取得した本文を同時にメモリへ保持する。既定 5 は維持。
- 影響: 10 を超える設定は 10 として扱われる。

## D22: readiness は「開けた索引」で判断する

- 決定: `Readiness::evaluate` は `Searcher` が実際に開いた索引の有無を受け取る（`None` のときだけパス存在で代替）。
- 理由: `meta.json` の存在だけでは壊れた索引を ready と誤判定する（実測）。
- 影響: `--check-config` と `/readyz` は同じ Searcher から判定するため一致したまま。

## D21: media プロキシは html バケットで数える

- 決定: `/image_proxy`・`/favicon_proxy`・`/autocompleter` は limiter の html バケットを使う（`/research` は JSON のまま）。
- 理由: 上流は `format != html` のときだけ API_WINDOW を加算する。ページ表示の favicon で API 枠を食うのは上流不一致。
- 影響: API 予算が検索 API 専用になる。burst/long は全リクエスト共通のまま。

## D21: engine 例外は「有効な engine の base_url と search_url のホスト」に限る

- 決定: DNS 検証の例外に含めるのは、有効な engine の `base_url` と `search_url` の**ホスト名のみ**。
  - リダイレクト先・検索結果 URL・画像 URL・MCP read_page には適用しない。
  - proxy 使用時は cross-origin リダイレクトを一切追わない（proxy が名前解決するためローカルで検証できない）。
  - 別の設定済み engine ホストへの cross-origin リダイレクトは拒否する（目的地として信頼しても、経由地にはしない）。
- 理由: 自己ホスト型 engine を search_url だけで設定する運用があり、無条件の除外は危険。境界を最小化した。
- 実測: engine-ssrf サブエージェントがローカル 3 サーバーと自前 proxy で接続先を確認（private ヒット 0、proxy 経由の nip.io は修正前 1）。

## D20: engine クライアントも宛先検証する（設定ホストだけ例外）

- 決定: engine 用 client に検証リゾルバとリダイレクト検証を入れ、本文を 8 MiB で打ち切る。例外は `base_url` のホストのみ。
- 理由: engine 応答も信頼できない。operator 設定のホストは信頼するが、リダイレクト先には適用しない。
- 残課題: `search_url` のみを持つ自己ホスト engine は例外にならない（RISKS R11）。
## D19: URL 検証は型ではなく接続時の検査を維持する

- 決定: `ValidatedUrl` のような newtype を導入しない。
- 理由: 非信頼 URL の安全性は「どの IP に接続したか」で決まり、DNS は検証と接続の間で変わり得る。
  型に「検証済み」を持たせても、接続時に再検証を省けば DNS rebinding で破られる。現状は
  `PublicResolver`（全解決 IP の分類）と各リダイレクトの再検証で担保している。
- 代替案: 検証済みアドレスを型で運ぶ（`ResolvedAddress`）。接続層が既に SocketAddr を固定しており、
  追加の型は利益が小さい。
- 影響: 呼び出し側は URL 文字列を渡す。安全性は `fetch::is_fetchable` と接続層のテスト（`ssrf_tests`）が守る。

## D23: ルーティングの既定は変更しない（能力適合は実 A/B 待ち）

- 決定: シミュレーションで優位な capability-aware / category-aware を既定にしない。staged の既定（tier 8, target 10）も据え置く。
- 理由: 効果量が「クエリと engine の能力適合」という仮定に依存し（offcap 0.35→0.70 で MRR 優位 +0.206→+0.047）、実エンジンでの A/B が無い。
- 代替案: tier 3 は Hit@10 −17.8pt で不採用。reliability-aware は 1–2% 失敗率では no-op。
- 次段: `src/query.rs` の種別判定 → `select_engines` の拡張という低コスト案から、許可・予算のある実 A/B で検証する。

## D22: 抽出コーパスを 30 ページへ拡張し、限定した class/id 判定を認める

- 決定: D18 を一部改訂。`class`/`id` を使うのは広告・同意・購読の**先頭セグメント一致**（ad, ads, adslot,
  advert, advertisement, sponsor, sponsored, cookie, consent, gdpr, newsletter, subscribe）に限る。
  `related`/`social`/`advice` は対象外のまま。引用・figure 内の `<footer>` は帰属として残す。
- 根拠: 30 ページのコーパスで 2 件の欠落を検出（広告・同意ブロックの混入、引用元の消失）。修正後は
  ads-cookie-banner が 588→435 字（−26%）、quotes-footnotes が 581→615 字（帰属行が復活）で、
  他の 28 ページと既存 24 テストは不変。
- 代替案: 構造シグナルのみ（広告 div は除去できない）、全 class 語のブラックリスト（本文を誤って落とす）。

## D18: ページクロームの除去は構造シグナルだけに限る

- 決定: `nav` / `footer` / `[hidden]` / `[aria-hidden=true]` / `role=navigation|banner|contentinfo|search` を
  本文抽出から外す。クラス名（`related`、`social`、`sidebar` など）では判定しない。
- 理由: クラス名は本文中にも現れる（"social" を扱う解説記事など）。誤って本文を落とす危険の方が、
  取りこぼす危険より大きい。
- 実測: RFC 9309 と Rust Book 4.1 では、どちらも本文コンテナが選ばれており削減効果は 0（防御としてのみ機能）。
- 代替案: readability 系のスコアリング（リンク密度・テキスト密度）。コーパス評価が前提のため今回は導入しない。

## D17: 句読点の前の余分な空白を落とす

- 決定: 行を組み立てた後に、` .` ` ,` ` :` ` ;` ` !` ` ?` ` )` ` ]` ` %` の前の空白を取り除く（`tidy`）。
- 理由: インライン要素を連結するたびに空白を入れるため、`config.yaml :` や `claim [1] .` のように
  句読点の前に空白が残っていた。読みにくく、フレーズ一致の検索でも不利になる。
- 代替案: 空白を一切入れない（`tmux를` のような単語連結が起きる）。
- 影響: 本文抽出の表記が変わる（句読点前の空白のみ）。意味は変えない。

## D16: 数式は 1 回だけ、TeX annotation を優先して出す

- 決定: `katex-html` / `MJX-TEX` / `mjx-assistive*` のサブツリーを捨て、`<math>` は `annotation` の TeX を優先、
  無ければ MathML のテキストを使う。
- 理由: 同じ式が視覚用・読み上げ用・MathML 用に 2 回以上現れ、本文が「a + b a+b」のような重複になっていた。
- 影響: 数式の表記が TeX になる（`e^{i\\pi}`）。プレーンテキストを期待する後処理には差分。

## D15: `--check-config` は `/readyz` と同じ判定を共有する

- 決定: readiness の判定を `src/diagnostics.rs` の `Readiness::evaluate` に一本化し、`/readyz` と CLI の両方が使う。
- 理由: 同じ問い（「この構成は検索できるか」）に 2 か所で答えると、片方だけ更新されて食い違う。
- 影響: 判定条件を変えるときは 1 か所の変更で済む。CLI はソケットを開かず、外部へも問い合わせない。

## D14: 表は Markdown 風の行として抽出する

- 決定: `content.rs` で `<table>` を `| a | b |` 形式の行にし、先頭行が全て `th` なら区切り行を挟む。
  セル内の `|` はエスケープし、行 200・列 32・セル 160 文字で打ち切る。
- 理由: 従来はセルが空白で連結され、3 列以上の表が「a 1 extra」のような1行になり、列の境界情報が失われていた。
- 代替案: セルごとに改行（行の対応が壊れる）、HTML のまま返す（本文抽出の契約外）。
- 影響: 本文抽出結果の表部分が変わる（より構造的）。入れ子の表は外側のセル内テキストとして 1 回だけ出る。

## D13: TITLE_BOOST の変更を採用しない（測定で効果なし）

- 決定: `TITLE_BOOST` を 3.0 → 4.0 にする実験を行い、採用しなかった。
- 根拠: index-docs（41,144 文書）と 100 問で Hit@1 19→19、Hit@5 24→24、Hit@10 25→24、
  MRR@10 0.2136→0.2125、nDCG@10 0.2226→0.2195。改善は確認できず、僅かに悪化した。
- 影響: コード変更なし（実験後に復元）。評価は `tools/eval_local_index.py` で再現できる。

## D12: SDK は提供しない（OpenAPI と既存 HTTP クライアントで足りる）

- 決定: 専用 SDK を作らず、`docs/openapi.yaml` と利用例で代替する。
- 理由: 公開 API は `/search`、`/autocompleter`、`/research` 程度で、HTTP クライアント 1 本で扱える。
  SDK はバージョン追随・言語ごとの保守という恒久的コストを生む（Engineering Principles の
  「必要性を説明できない依存・抽象化を導入しない」に該当）。
- 代替案: 生成 SDK（OpenAPI Generator）。生成物の品質と保守負担の実測が無いため保留。
- 影響: 利用者は curl / fetch / requests で直接呼ぶ。仕様のドリフトは `tools/test_openapi.py` が検出する。

## D11: OpenAPI 仕様を SSoT ではなく「ドリフト検査の対象」として置く

- 決定: `docs/openapi.yaml` に JSON API を記述し、`tools/test_openapi.py` が (a) パラメータ名が `SearchParams` に実在するか
  (b) pageno/クエリ長/research 上限がコード定数と一致するか (c) 記載パスがルーターに実在するかを検証する。
- 理由: 散文だけの契約は実装から静かにずれる。仕様を「正」と宣言するとコードが従わされるので、両者の一致を強制する。
- 影響: 上限値やパラメータ名を変えるときは仕様とテストも同時に更新する必要がある（意図した摩擦）。

## D9: メディア取得は同時 4 件までに固定する

- 決定: `Network.media_slots`（Semaphore=4）で `fetch_image` を制限し、取得は要求期限で打ち切る。
- 理由: 画像・favicon は 1 件最大 8MiB をメモリに保持する。並列数が呼び出し側の自由だと、同じ署名付き URL の
  反復でメモリと上流負荷を増幅できる。
- 代替案: 設定項目にする（値を説明できる運用根拠がまだ無い）。
- 影響: 同時 5 件目以降はスロット待ち。期限を超えたら `Timeout` として失敗する。

## D10: `/readyz` を追加し、生存と検索可能性を分ける

- 決定: `/readyz` は `engines` 数とローカル索引の有無を JSON で返し、どちらも無ければ 503。
- 理由: 外部プロバイダーへ問い合わせずに「この構成は検索できない」を運用側が検知できる必要がある。
  `/healthz` はプロセスの生存だけを示す。
- 代替案: `/healthz` に状態を含める（SearXNG 互換の単純な 200 を壊す）。
- 影響: 監視は `/healthz`（生存）と `/readyz`（構成）を分けて使う。どちらも上流の可用性は保証しない。

## D8: 台帳チェックに 365 日の失効を入れる

- 決定: review_date が 365 日を超えたら check_provider_policy.py が失敗する。
- 理由: 条件の記録が古いまま「確認済み」として扱われるのを防ぐ。
- 影響: 期限切れ時は CI が赤くなる。失効は自動無効化ではなく再確認の要求。
