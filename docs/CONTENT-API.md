# エージェント向け API: `/map`・`/extract`・`/mcp`

エージェントが「読む」だけでなく「探す」「構造化する」ための 3 つの面。
いずれも既存の検証済みページ取得を共有する: 非公開アドレスへの接続を拒否し、robots.txt を守り、
サイズ上限を適用する。limiter の対象でもある。

## `/map` — サイトの URL を列挙する

```bash
curl --fail --get 'http://127.0.0.1:29483/map' \
  --data-urlencode 'url=https://doc.rust-lang.org/book/' \
  --data 'limit=1000'
```

発見の順序:

1. `robots.txt` の `Sitemap:` 宣言（HTTP 取得。無くても失敗しない）
2. sitemap index の入れ子（上限 32 ファイル）
3. どれも URL を返さなかった場合だけ、シードページの `<a href>`

URL は正規化（フラグメント・追跡パラメータを除去）し、シードの origin に限定し、重複を除く。

```json
{
  "seed": "https://doc.rust-lang.org/book/",
  "origin": "https://doc.rust-lang.org",
  "used_sitemap": true,
  "truncated": false,
  "urls": [
    {"url": "https://doc.rust-lang.org/book/ch01-00-getting-started.html", "depth": 1, "source": "robots_sitemap"}
  ],
  "errors": []
}
```

`source` は `robots_sitemap` / `sitemap_index` / `seed_links`。
`errors` には読めなかった sitemap と短い理由が入る（1 件失敗しても残りは返す）。

## `/extract` — ページを構造化する

```bash
curl --fail --get 'http://127.0.0.1:29483/extract' \
  --data-urlencode 'url=https://example.com/product' \
  --data 'limit=50'
```

抽出元は JSON-LD → 定義リスト → 表の順に試し、**最初に得られた方式だけ**を返す。
同じデータを 4 通りで重複して返さないため。

```json
{
  "url": "https://example.com/product",
  "truncated": false,
  "records": [
    {
      "id": "#product",
      "source": "jsonld",
      "path": "script[0].@graph[0]",
      "fields": {"name": "Widget", "offers.price": "1200"}
    }
  ]
}
```

値はページにあるものだけで、推測はしない。各レコードは `source` と `path` を持ち、
どの要素から来たかを追える。`@graph` は 1 ノード 1 レコードに展開する。

## `/mcp` — HTTP で MCP を配信する

既定では無効。`settings.toml` で明示的に有効化する:

```toml
[mcp]
http = true
# リバースプロキシ配下なら公開ホスト名を列挙する（空なら loopback のみ）
allowed_hosts = ["search.example.org"]
```

有効にすると HTTP リスナーに `/mcp` が生え、stdio 版と同じツール（`search` / `read_page`）を
Streamable HTTP で提供する。

```bash
curl -s -X POST 'http://127.0.0.1:29483/mcp' \
  -H 'content-type: application/json' \
  -H 'accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"curl","version":"1"}}}'
```

### なぜ既定で閉じているか

`/mcp` は `read_page` と同じ取得能力をリモートのエージェントに渡す。公開インスタンスでは
意図的に開くものなので、既定は無効にしてある。`allowed_hosts` は Host ヘッダの検査に使い、
loopback 以外を許すときは明示的に列挙する（DNS リバインディング対策）。

## 制限

| 項目 | 値 |
| --- | --- |
| `/map` の URL 数 | 5,000 |
| `/map` が辿る sitemap | 32 |
| sitemap 1 件のサイズ | 8 MiB |
| `/map` が読むシードのリンク | 2,000 |
| `/extract` のレコード数 | 200 |
| フィールド値の長さ | 2,000 文字 |
| `/mcp` のリクエスト本文 | 4 MiB（transport 既定） |
