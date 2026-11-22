# 軽量ビルドと既定 feature

`db-engines` は postgres / mysql / sqlite / mongodb を検索元として使うための feature で、
既定で有効です。ここでは 3 構成を実測し、既定を変えるべきかを判断します。

## 実測（2026-11-17、Linux x86_64、Rust 1.100.0）

| 指標 | 既定（db-engines） | 最小（`--no-default-features`） | 全 feature |
| --- | ---: | ---: | ---: |
| 依存クレート（normal, ユニーク） | 349 | 286 | 349（既定と同じ） |
| release バイナリ | 35,813,744 B | 27,300,256 B（−23.8%） | 35,813,744 B |
| release ビルド（依存キャッシュあり） | 1 分 52 秒 | 1 分 36 秒 | 1 分 47 秒 |
| 起動 → /healthz（standard, 34 engines） | 0.186 s | 0.208 s | 同左 |
| アイドル RSS | 24.5 MiB | 23.5 MiB | 同左 |
| ピーク RSS（1 検索） | 82,596 kB | 80,296 kB | 同左 |

最小構成で減るのは主に SQLite（同梱 C ライブラリ）、MongoDB、MySQL、PostgreSQL の
クレート群です。起動時間とメモリの差は測定誤差の範囲で、**実行性能の差は確認できません**。

## 判断

**既定は変更しません。**

- 既定を最小にすると、DB を検索元にしている利用者が設定ごと機能を失う（移行コストが大きい）
- バイナリは 8.5 MB 小さいが、起動・メモリ・検索性能に差は無い
- 最小構成は CI の `no-default-features` ジョブで継続的にビルド検証している

## 使い分け

- 既定: DB を検索元にする可能性がある一般的な配備
- 最小: DB を使わない配備（コンテナ、小さい VPS、配布サイズを抑えたい場合）
  `cargo build --release --no-default-features` または `cargo install --no-default-features`
- 全 feature: 現在は既定と同じ（feature は `db-engines` のみ）

## インストール要件

- 既定・最小とも Rust 1.89 以上と C ツールチェーン（`rusqlite` の bundled SQLite 用）
- 外部 DB サーバーは、DB エンジンを実際に使う場合だけ必要。クライアントライブラリは純 Rust
- 最小構成では MySQL/PostgreSQL/MongoDB のクレート自体が入らない

## 互換性への影響

既定 feature を変えると、`[engines]` に DB エンジンを持つ既存設定が起動時に「未知のエンジン」になります。
そのため既定は変えず、最小構成は明示的な opt-in とします。
