# COMPATIBILITY — 既存利用者への影響

| 変更 | 影響 | 移行 |
| --- | --- | --- |
| 4xx の本文が要求フォーマットに一致 | JSON/CSV/RSS クライアントは本文形状が変わる（ステータスコードは不変） | 通常は改善。HTML 前提の自動化があれば要修正 |
| pageno > 1000 を 400 | 1000 ページ超の要求が拒否される | 実用上到達しない。必要なら `search.max_page` は 1000 未満で設定 |
| q > 2048 文字を 400 | 超長いクエリが拒否される | 分割して検索 |
| /autocompleter は既知名のみ | `backend=` に未知の名前を入れた場合、結果が空になる（以前は DuckDuckGo） | 既知の名前（duckduckgo / wikipedia / mwmbl）を使う |
| /research の期限・同時実行 | 120 秒超で 504、同時 3 件目以降は 429 | `research.deadline` / `research.max_concurrency` で調整 |
| proxies の未知キーは起動エラー | 設定ミスが起動時に分かる | キーは all / http / https のみ |
| Valkey キーに名前空間 | 旧キーは TTL まで残るが参照されない。キャッシュは再構築 | 操作不要 |
| `raw_content_limit` > 10 | 10 として扱う | 既定 5 のまま推奨 |

| `/readyz` を追加（上流にない） | 追加のみ。`/healthz` は不変 | 監視は `/healthz`（生存）と `/readyz`（構成）を分ける |
| 画像・favicon プロキシが limiter 対象 | 既定 limiter off のため既定挙動は不変 | 有効化時は 429 と `Retry-After` |
| 表の抽出が Markdown 風の行になる | `include_raw_content` の表部分が `\| a \| b \|` 形式に変わる | 列の境界が読めるようになる。旧形式前提の後処理があれば要修正 |
| `--check-config` を追加 | 追加のみ（起動しない） | 監視・CI で設定検証に使える |
| 数式が 1 回だけ出る（TeX annotation 優先） | 本文中の数式表記が `e^{i\pi}` などになる | 重複テキストが消える。旧表記前提の後処理があれば要修正 |
| 表キャプションが抽出結果に含まれる | 表の直前にキャプション行が増える | 情報の追加のみ |
| 句読点前の空白が消える | `include_raw_content` が `config.yaml:` のように出る | 読みやすさの改善。旧文字列前提の完全一致があれば要修正 |
| `nav` / `footer` / `hidden` / `aria-hidden` が本文から除かれる | ナビゲーション文言が `include_raw_content` に出なくなる | 情報の追加のみ（本文の欠落はテストで確認済み） |

SearXNG 互換の範囲: `/search` のパラメータ、JSON/CSV/RSS の成功時スキーマ、`/config`、`/stats` は不変。
エラー本文の形状のみ差分。
