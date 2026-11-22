# 自前インデックス

`rustxng-index` は、指定したサイトをクロールし、Tantivy の全文検索インデックスを作ります。
外部検索で見つからない文書を補う用途や、取得済みの文書だけを検索する用途に使えます。
検索できる範囲は収録したページに限られます。ページ数を増やしただけで検索精度が上がるわけではありません。

## 作成と更新

シードURLを1行ずつ書いた `seeds.txt` を用意します。

```text
https://doc.rust-lang.org/book/
https://docs.python.org/3/library/
```

```bash
rustxng-index crawl --seeds seeds.txt --out ./index-docs \
  --max-pages 5000 --max-depth 3 --max-per-host 1000 --workers 4 \
  --sitemaps --within-seed-path --contact ops@example.org

rustxng-index stats --index ./index-docs
rustxng-index search --index ./index-docs "python dict get" --limit 5
```

同じコマンドを再実行すると、今回取得したURLの文書を置き換えます。
同じURLを再登録しても文書数は増えません。再取得で `noindex`、404、410を確認した文書は削除します。
リダイレクトやcanonicalの変更時も、今回観測した要求URL・最終URLの古い記録を除きます。
過去の別canonical URLとの対応は保存していないため、既定では今回訪問しなかった文書を保持します。
同じサイトを継続的に巡回する方法は[定期クロール（巡回計画）](#定期クロール巡回計画)を参照してください。

`--skip-indexed` は、サイトマップに載っている登録済みURLを取得せず、続きを収集するための指定です。
そのURLの更新や削除も検出しなくなるため、既存文書を更新する実行では付けないでください。
サイトマップにないシードやリンク先は、次のページを発見するために取得します。

### 削除同期（`--prune-missing`）

今回の実行で一度も訪問しなかった文書を、範囲内に限って削除します。既定では無効です。

```bash
# 何が消えるかを先に確認する（索引は変更しない）
rustxng-index crawl --seeds seeds.txt --out ./index-docs --sitemaps \
  --prune-missing --dry-run

# 実際に削除する。削除したURLは1件ずつ表示する
rustxng-index crawl --seeds seeds.txt --out ./index-docs --sitemaps \
  --prune-missing
```

範囲は、シードのホスト（`www.` の有無は同一視）です。`--within-seed-path` を併用すると
シードのディレクトリ配下に限ります。`--scope URL` を1回以上指定した場合は、シードではなく
その接頭辞だけが対象になります。範囲外の文書には触れません。

「訪問しなかった」は「消えた」とは違います。次のいずれかに当てはまる実行では、削除を行わずに理由を表示します。

- `--max-pages` の上限で停止した、または `--max-per-host`・`--max-queue` の上限に達した
- `--skip-indexed` と併用した（取得しなかった登録済みURLが「未訪問」に見える）
- `--since` と併用した（新しかった文書は取得しないため、同じく「未訪問」に見える）

取得に失敗したURLも「訪問した」として扱います。通信の失敗を削除の理由にしないためです。

### 差分取得（`--since RFC3339`）

文書ごとの索引登録日時（`indexed_at`）を保存し、指定した時刻**より前**に登録された文書だけを取得し直します。
新しく登録済みの文書は取得せず、その文書のリンクも辿りません。`--sitemaps` が必要です。

```bash
rustxng-index crawl --seeds seeds.txt --out ./index-docs --sitemaps \
  --since 2026-11-01T00:00:00Z
```

`--skip-indexed` との違いは、判定が「登録済みか」ではなく「登録が新しいか」である点です。
索引が無い初回の実行では、`--since` を指定してもすべてのページを取得します。
差分取得だけでは削除を検出できないため、`--since` と `--prune-missing` は併用できません。

スキーマv3の既存インデックスはそのまま使えます。v2以前の索引は `indexed_at` を持たないため再作成が必要で、
その場合は「古いバージョンで作られた索引」として理由を表示して停止します。
読み取り専用で開く処理は、空のディレクトリや壊れた索引を自動修復・初期化しません。

### 定期クロール（巡回計画）

同じサイトを継続的に巡回するときは、サイトと間隔を書いた計画ファイルを使います。
計画は TOML で、`[[site]]` が 1 サイトです。

```toml
# crawl-plan.toml
[[site]]
name = "rust-book"                  # 省略すると最初のシードURL
seeds = ["https://doc.rust-lang.org/book/"]
interval = "24h"

[[site]]
seeds = ["https://docs.python.org/3/library/"]
interval = "7d"
sitemaps = true                     # 省略時はコマンドラインの値を継承
max_pages = 5000
within_seed_path = true
```

```bash
# 期限が来たサイトだけを巡回する（cron からはこれを呼ぶ）
rustxng-index crawl --plan crawl-plan.toml --out ./index-data --due \
  --contact ops@example.org

# 期限に関係なく全サイトを巡回する（手動の再取得）
rustxng-index crawl --plan crawl-plan.toml --out ./index-data

# 記録された状態を見る（巡回しない）
rustxng-index schedule --index ./index-data
```

- `interval` は `30s` / `15m` / `6h` / `7d` / `1w`、または秒数の整数です。0 はエラーです。
- `seed` と `seeds` のどちらかが必要です。1 つの `[[site]]` のシードは同じホストとポート（`www.` の有無は同一視）に揃えてください。別ホストは別の `[[site]]` にします。
- `name` は状態のキーです。省略すると正規化した最初のシード URL になります。シードの並べ替えや URL 変更で履歴が別サイトになるため、長く運用するときは明示してください。
- `sitemaps`、`max_pages`、`within_seed_path`、`scope` を書くと、そのサイトだけコマンドラインの値を上書きします。`scope` は `--prune-missing` を付けたときだけ使われます。
- 未知のキー、重複した `name`、HTTP(S) 以外のシードはエラーになります。書き間違いを黙って無視しません。

#### 何を外部に任せ、何をこのツールが保証するか

常駐モード（`--watch`）は用意していません。既存の運用は cron / systemd timer などのバッチで、
常駐プロセスを足すと同じ索引の書き込みを 2 つのスケジューラが奪い合います。役割は次のように分かれます。

- 外部（cron など）: コマンドを起動する時刻。たとえば 15 分ごとに `--due` 付きで呼びます。
- このツール: 起動しても期限が来ていないサイトには触れないこと、次回時刻・前回巡回時刻・最終成功時刻・連続失敗回数を索引ディレクトリに保存すること、期限が来たサイトを 1 つずつ巡回すること。

同じ索引に対してクロールを同時に実行することはできません。2 つ目の実行は索引の書き込みロックを
取得できず、どのサイトの状態も更新せずに終了コード 1 で終わります（サイトの失敗としては数えません）。
cron の起動間隔は、計画のいちばん短い `interval` より短くしないでください。
`--due` を付けない実行は期限を無視して全サイトを巡回するので、定期実行には使いません。

#### 状態ファイル

索引ディレクトリの `crawl-schedule.json` にサイトごとの状態を保存します。
書き込みは一時ファイルに書いてから置き換えるため、途中で落ちても壊れた JSON は残りません。
壊れたファイルや未知の `version` を見つけたときは、黙って初期化せずエラーで停止します。

| 項目 | 意味 |
| --- | --- |
| `interval_seconds` | 計画ファイルの巡回間隔（秒） |
| `last_attempt` | 最後に巡回を開始した時刻（Unix 秒） |
| `last_success` | 最後に巡回が成功した時刻（Unix 秒） |
| `consecutive_failures` | 連続失敗回数 |
| `next_due` | 次回巡回時刻（Unix 秒） |

`interval` を書き換えると、次回時刻は最後に成功した巡回（まだ無ければ最後の巡回開始）を基準に計算し直されます。
計画から外したサイトの記録は残ります。同じ `name` で戻せば、その記録から再開します。

#### 失敗したとき

そのサイトの巡回がエラーで終わった場合、または 1 ページも索引に書けず取得失敗・バックオフだけが
記録された場合を「サイトの失敗」とします。個々のページの 404・noindex・robots.txt による除外は
失敗に数えません。

失敗すると次回時刻を間隔の 1 倍、2 倍、4 倍…と後ろへずらし、7 日で頭打ちにします
（間隔が 7 日より長い場合は間隔が下限）。成功すると連続失敗回数は 0 に戻り、次回は
「成功時刻 + 間隔」になります。失敗したサイトがあっても残りのサイトは巡回し、終了コードは 1 になります。
状態ファイルはサイトごとに保存するので、途中で停止しても完了済みサイトの結果は残ります。

#### 差分取得・削除同期との関係

計画からの巡回も、`--seeds` で 1 サイトだけを指定した実行と同じ規則です。

- `--since RFC3339` を付けると、全サイトで同じ時刻を基準に、索引登録が古い文書だけを取得し直します。`--sitemaps` が必要という条件も同じで、計画の全サイトで sitemaps が有効（コマンドラインか `sitemaps = true`）である必要があります。
- `--prune-missing` を付けると、そのサイトのシード範囲だけを対象に、今回訪問しなかった文書を削除します。`--max-pages` などで止まったサイトは削除しません。サイトごとの `scope` を書いた場合はその接頭辞が対象です。
- `--since` と `--prune-missing` は併用できません。`--skip-indexed` と `--prune-missing` も併用できません。
- `--max-pages`、`--max-per-host`、`--max-queue`、`--delay-ms`、`--workers`、`--contact` は全サイトに適用します（`max_pages` だけは計画ファイルでサイトごとに上書きできます）。

`--since` を使う定期実行では、取得しなかった新しめの文書は「未訪問」に見えるため削除同期は行えません。
削除も定期化したい場合は、`--prune-missing` を付けて期限ごとに全ページを取得する実行にしてください。

## 検索サーバーから使う

StandardとCleanの組み込み設定は `index-docs` と `index-wikipedia` を参照します。
索引は配布バイナリに含まれず、そのディレクトリがなければ利用されません。
別の場所を使う場合は、設定ファイルで指定します。

```toml
[search]
local_index_path = "index-docs"
wikipedia_index_path = "index-wikipedia"
local_index_merge = true
```

`local_index_merge = true` は、検索元を指定していない一般検索に索引の結果を混ぜます。
`false` にしても、索引を明示指定した検索は利用できます。
Wikipediaの索引は、別の [`rustxng-wikidump`](WIKIPEDIA-INDEX.md) コマンドで作成します。

### 意味スコアの併用（任意）

`search.embedding_url` を設定すると、一般検索で外部エンジンと併用したときに限り、
索引の各ヒットへクエリとのコサイン類似度を付け、BM25の順位に `search.embedding_weight` の重みで混ぜます。
語が一致しない言い換えのページを上位へ動かすための任意機能です。

```toml
[search]
embedding_url = "http://127.0.0.1:8000"   # 空ならBM25のみ
embedding_model = "BAAI/bge-small-en-v1.5"
embedding_weight = 2.5
embedding_topk = 25
```

- `embedding_url` が空（既定）のとき、埋め込みは要求しません。順位・スコアは従来と同じです。
- 索引だけを指定した検索（`engines=local_index`、`!local_index`）は、従来どおり埋め込みを使いません。
  索引の持ち主が順位を決め、1ソースの結果順を保つためです。
- 応答が不正な場合は警告を記録し、BM25の順位のまま返します。検索自体は失敗しません。
- 併用時の索引スコアはBM25ではなく、類似度を混ぜた値です。単独検索のスコアとは比較できません。
- 外部エンジンとまとめたときの数値スコアにも類似度が反映されます。索引のヒットは、その並び順を
  変えない範囲で、類似度に応じて最大1.5倍の重みで統合スコアに加点されます（`embedding_weight`
  とは別の固定上限）。類似度が無い場合の順位とスコアは従来と同じです。

```bash
rustxng --profile clean --config local-settings.toml

# 自前クロールの索引だけを検索する
curl --fail --get 'http://127.0.0.1:29483/search' \
  --data-urlencode 'q=std::sync::Arc' \
  --data 'format=json' --data 'engines=local_index' --data 'max_results=10'

# 対象ドメインの中から上位の文書を探す
curl --fail --get 'http://127.0.0.1:29483/search' \
  --data-urlencode 'q=async runtime' \
  --data 'format=json' --data 'engines=local_index' \
  --data 'include_domains=doc.rust-lang.org'
```

`engines=local_index,wikipedia_index` で両方を選べます。クエリ内の `!local_index`、`!wikipedia_index` も使えます。
明示した索引がどれも利用できない場合はエラーになり、外部検索へ切り替わりません。
通常の検索元を指定した場合も、指定外のローカル索引や公式サイト検索を追加しません。
`include_raw_content` など、検索後に本文を取得する機能は別途ネットワークを使います。

ローカル索引は1ページ20件で、`pageno` により次のページへ進めます。
`max_results` は返す件数の上限で、ページの区切りは変えません。
取得可能な検索範囲は先頭10,000件までです。範囲超過や索引検索の失敗は応答に記録します。
クエリは16,384バイト・128内容語まで、ドメイン条件は許可と除外を合わせて64件までです。

## 一致と順位

タイトル・本文・URLの語をBM25で検索し、タイトル一致を3倍、URL一致を1.5倍にします。
英語などの語は、短いクエリでは全語、3語以上では内容語の約3/5の一致を必要とします。
タイトル・本文にクエリ全体がフレーズとして現れる場合は加点します。
`std::sync::Arc` のような句読点を含む語も、索引内部のフィールド指定として解釈せず検索します。

日本語・中国語は文字バイグラムを使います。同じ文字列の一部分だけが一致する文書で結果を埋めないよう、
語ごとの一致数を確認し、英語と日本語が混在するクエリにも同じ内容語の条件を適用します。
Wikipedia索引は記事の冒頭のみを持つため、従来どおり内容語のいずれかが一致すれば候補になります。

ドメインの許可・除外は、索引から上位件数を取り出す前に適用します。
`example.org` はそのホストとサブドメインに一致し、`example.org.evil.test` には一致しません。
結果のスニペットは保存した冒頭部分です。質問に合う本文抜粋は、検索後のMCP `read_page(query=...)` で取得できます。

## クロールの制約

| 項目 | 動作 |
| --- | --- |
| 接続先 | 公開HTTP(S)のみ。DNS応答を検査し、検査したアドレスへ接続する。非公開IPを含む応答は拒否 |
| プロキシ | 接続先の検証を保証できないため、プロキシ設定がある環境ではクロールを開始しない |
| リダイレクト | 最大5回。同じサイト名（wwwの有無を許容）に限定。明示ポートの変更とHTTPS→HTTPを拒否し、各移動先でrobotsと範囲を再確認 |
| robots.txt | オリジンごとに取得。通信・読み取り失敗、上限超過、429、5xxでは取得を見送る |
| 取得間隔 | `--delay-ms` は既定1,000ms。ページ・robots・サイトマップで同じホストの送信間隔を共有 |
| ページの指示 | `noindex`、`nofollow`、`X-Robots-Tag`、リンクの `nofollow`・`ugc`・`sponsored` を考慮 |
| サイトマップ | XMLとして解析し、名前空間・文字参照を処理。訪問済みURL、深さ、文書数を制限 |
| 件数 | `--max-pages` は既定500。`--max-per-host` は既定2,000 |
| 削除同期 | `--prune-missing` は既定OFF。上限で止まった実行、`--skip-indexed`・`--since` との併用では削除しない |
| 差分取得 | `--since` は文書の `indexed_at` を比較。`--sitemaps` が必要 |
| 巡回計画 | `--plan` でサイトと間隔を宣言。`--due` は期限到来分だけを巡回し、状態を `crawl-schedule.json` に保存 |
| 連続失敗 | 失敗ごとに次回を間隔の2倍ずつ後ろへ（上限7日）。成功で連続失敗回数と次回時刻を戻す |
| サイトマップ数 | `--max-sitemaps` は既定64文書。シードのオリジンごとの上限。各文書のリダイレクトは別途最大5回。0で探索しない |
| 待機キュー | `--max-queue` は既定10,000。満杯のときに追加できなかったURL数を終了時に表示 |
| 応答サイズ | ページ2MiB、robots 512KiB、サイトマップ10MiBまで。展開後の本文に適用 |
| 終了 | ページ上限や書き込み失敗でワーカーを停止し、稼働中のワーカーが終了してから戻る |

`--within-seed-path` は、シードのディレクトリ配下に取得先を制限します。
たとえば `https://docs.python.org/3/` なら `/3/` の下を対象にします。
このパス制限はページに適用し、robots.txtやサイトマップ自体には適用しません。
canonicalは同一オリジンのURLのみ採用し、フラグメントと既知の追跡パラメータを除きます。
PDF・画像・スクリプトなどは索引クローラーの対象外です。HTML本文はJavaScriptを実行せず抽出します。
停止時はホスト間隔とバックオフの待機を解除します。送信済みのHTTP要求は完了または20秒のタイムアウトを待ちます。

## 評価（オフライン・ネットワーク不要）

自前インデックスの検索品質は、外部プロバイダーへ問い合わせずに測れます。

```bash
cargo run --release --locked --example local_quality -- index-docs bench/quality-questions-100.json > /tmp/local.json
python3 tools/eval_local_index.py /tmp/local.json --json /tmp/local-metrics.json
# 変更前後の比較
python3 tools/eval_local_index.py /tmp/local-after.json --baseline /tmp/local-before.json
```

`tools/eval_local_index.py` は Hit@1 / Hit@5 / Hit@10、MRR@10、nDCG@10（二値関連度）、
p50 / p95 を出します。gold URL の照合は [reference_urls.py](../tools/reference_urls.py) の
パス境界ルールを使うため、ディレクトリ参照と `.html` の別ページを混同しません。
`--min-hit5` を付けると下限を割ったときに終了コード 1 を返し、CI のゲートにできます。

この数値は**索引に含まれる範囲**の上限に縛られます。Wikipedia・論文・公式サイトを gold に持つ質問は、
文書索引だけでは到達できないため、ヒット率の低さをそのままランキングの欠陥と解釈しないでください。

## 過去の評価

今回のローカル検索・集約処理の変更前後比較は [評価手順と生データ](../bench/local-retrieval/README.md) を参照してください。
開発用の人工データによる回帰評価で、Web検索全体やExaとの品質比較ではありません。

[2026-11-04の構成比較](COMPARISON-2026-11-04.md)は、文書サイトから作った41,144ページの索引とWikipedia索引を使った記録です。
古い小規模索引の測定は [`bench/eval-100-localmerge.json`](../bench/eval-100-localmerge.json)、
127,200ページの測定は [`bench/eval-100-127k.json`](../bench/eval-100-127k.json) に残しています。
これらは異なる収録範囲・プロバイダー設定での過去の観測で、現在の変更による改善量やExaとの同条件比較を示すものではありません。
