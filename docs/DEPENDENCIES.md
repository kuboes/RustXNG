# 依存関係の方針

RustXNG の依存は `Cargo.lock` で固定し、`deny.toml` でライセンス・禁止事項・取得元を検査します。
CI の security ワークフローが次の 2 つを実行します。

| 検査 | ツール | 対象 |
| --- | --- | --- |
| 既知の脆弱性 | `cargo audit` 0.22.2 | RustSec advisory DB（`tools/check_audit.py` が基準線と照合） |
| ライセンス・禁止事項・取得元 | `cargo deny` 0.20.2 | `deny.toml` |

## `deny.toml` の内容

- **licenses**: 許可するのは MIT / Apache-2.0（LLVM exception 含む）/ BSD-2・3 / ISC / Zlib / Unicode-3.0 /
  MPL-2.0 / CC0-1.0 / 0BSD / CDLA-Permissive-2.0 / AGPL-3.0-or-later（本プロジェクト自身）。
  MPL-2.0 は scraper 経由の 4 クレート（NOTICE に記載）、CDLA-Permissive-2.0 は webpki-roots（TLS ルート証明書）。
- **bans**: ワイルドカード指定（`*`）は拒否。同一クレートの複数バージョンは警告にとどめる（エコシステム側の都合で頻発するため）。
- **sources**: crates.io 以外のレジストリと git 依存は拒否。
- **advisories**: `cargo audit` と二重になるが、`deny.toml` 側でも既定のまま有効にしてある。

## 実行

```bash
cargo deny --locked check        # ライセンス・bans・sources・advisories
cargo audit                      # RustSec のみ
```

## 例外の追加

新しいライセンスや取得元が必要になった場合は、`deny.toml` を変更する同じコミットで、
この文書に「どのクレートが、なぜ必要か」を追記します。理由を書けない依存は追加しません。

## 注意

- `cargo deny` は**ポリシーの検査**であり、法的適合や脆弱性が無いことの保証ではありません。
- advisory DB と crate のメタデータは取得時点の内容です。CI は毎回最新の DB を使います。
