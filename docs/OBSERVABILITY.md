# 監視とメトリクス

`GET /metrics` は Prometheus / OpenMetrics 形式で、プロセス内のカウンタとゲージを返します。
`general.enable_metrics` が false のときは 404 を返します（既定は true）。
このエンドポイントは外部プロバイダーへ問い合わせません。

## 主な系列

| 系列 | 型 | 意味 |
| --- | --- | --- |
| `searxng_search_requests_total` | counter | 処理した検索リクエスト数 |
| `searxng_search_results_total` | counter | クライアントへ返した結果数 |
| `searxng_uptime_seconds` | gauge | プロセス稼働秒数 |
| `searxng_search_duration_seconds_sum` / `_count` | counter | 検索の所要時間の合計と件数。平均は sum / count で求める（分位点は未提供） |
| `searxng_ready` | gauge | 検索元（エンジンまたはローカル索引）が構成されていれば 1。`/readyz` と同じ判定 |
| `searxng_engines_configured` | gauge | 構成から構築されたエンジン数 |
| `searxng_local_index` | gauge | 構成されたローカル索引がディスク上に存在すれば 1 |
| `searxng_cache_hit_total` / `searxng_cache_miss_total` | counter | 応答キャッシュの命中・外れ |
| `searxng_cache_error_total` | counter | キャッシュのデコード・通信エラー |
| `searxng_cache_entries` / `searxng_cache_bytes` | gauge | キャッシュの件数と保持バイト数（Valkey では 0） |
| `searxng_limiter_requests_total{verdict="allowed\|challenged\|blocked"}` | counter | レート制限の判定別件数 |
| `searxng_limiter_tracked` | gauge | レート制限が追跡しているクライアント数 |
| `searxng_http_responses_total{status="2xx\|3xx\|4xx\|5xx"}` | counter | 返した HTTP 応答のクラス別件数。エラー率のアラート向け |
| `searxng_engines_*{engine="..."}` | counter / gauge | エンジン別の送信数・結果数・エラー数・応答時間・信頼度・休止状態 |
| `searxng_engines_suspended_seconds{engine="..."}` | gauge | hard block による休止の残り秒数 |
| `searxng_engines_circuit_state{engine="...",state="closed\|half-open\|open"}` | gauge | 現在のサーキット状態だけが 1、それ以外は 0 |
| `searxng_engines_circuit_failure_ratio{engine="..."}` | gauge | 直近の窓（`search.circuit.window`）の失敗率（0〜1） |
| `searxng_engines_circuit_samples{engine="..."}` | gauge | 窓に入っている結果の数 |
| `searxng_engines_circuit_open_seconds{engine="..."}` | gauge | 開放が終わり、半開の試行が許されるまでの残り秒 |
| `searxng_engines_consecutive_failures{engine="..."}` | gauge | 最後の成功からの連続失敗数 |

## エンジンの健康状態

RustXNG はエンジンごとに 3 段階の防御を持ちます。`/stats`、`/metrics`、`/stats/errors`（JSON）は
同じ状態を別の形式で返します。

1. 休止（hard block）: 401/403/429/CAPTCHA などは `search.suspended_times` の秒数だけ即座に休止します。
   繰り返すと 1 時間 → 6 時間 → 24 時間 → 7 日まで延び、成功でリセットされます。
   `searxng_engines_suspended` と `searxng_engines_suspended_seconds` で見ます。
2. 空結果による休止: 結果を返さない状態が `search.empty_result_limit` 回続くと
   `search.empty_result_cooldown` 秒だけ休止します。`searxng_engines_resting` と
   `searxng_engines_resting_seconds` で見ます。
3. サーキットブレーカ: `search.circuit` の窓（既定 20 件、最小サンプル数 10）で失敗率がしきい値
   （既定 0.6）以上になると開きます。開いている間は検索から外れ、`open_seconds`（既定 30 秒）後に
   半開になり 1 回だけ試行します。成功すれば閉じ、失敗すれば開放時間を倍にして開き直します
   （上限 `max_open_seconds`、既定 600 秒）。

`/stats` の Engines 表には `Circuit`、`Failure rate`、`Samples`、`Open for` の列があり、エンジンごとの
状態を確認できます。`rustxng --check-config` は起動前のポリシーを `circuit` オブジェクトとして表示します。
起動直後はまだ検索していないため、状態はすべて `closed` です。

### 状態の意味

| 状態 | 意味 | 次の遷移 |
| --- | --- | --- |
| `closed` | 通常。検索に参加する | 失敗率がしきい値以上で `open` |
| `open` | 検索から外れている | `open_seconds` 経過で `half-open` |
| `half-open` | 試行を 1 回だけ許可 | 成功で `closed`、失敗で `open`（開放時間は倍） |

## 何を監視するか

| 監視対象 | 見る系列 | 異常の目安 | 主な原因 |
| --- | --- | --- | --- |
| 検索可能性 | `searxng_ready` | 5 分以上 0 | エンジンもローカル索引も無い。`--check-config` で確認 |
| サーキット開放 | `searxng_engines_circuit_state{state="open"}` | 10 分以上 1 が続く | 上流の障害・仕様変更・ネットワーク遮断 |
| 失敗率 | `searxng_engines_circuit_failure_ratio` | 0.6 以上が継続（`circuit_samples` が `minimum_samples` 以上） | 上流の不安定さ。窓が埋まるまでは判断しない |
| 長期休止 | `searxng_engines_suspended_seconds` | 3600 秒以上 | CAPTCHA / 429 / アクセス拒否。手動対応が必要なことが多い |
| 空結果の休止 | `searxng_engines_resting` | 1 が続く | クエリと無関係なエンジン、上流の空応答 |
| 連続失敗 | `searxng_engines_consecutive_failures` | 急増 | 一時的な上流障害。サーキットが開く前兆 |
| 自インスタンスの 5xx | `searxng_http_responses_total{status="5xx"}` | 5 分率で 5% 超 | 内部エラー。ログを確認 |
| 検索の遅さ | `searxng_search_duration_seconds_sum` / `_count` | 平均が `search.timeout_limit` に近い | 遅いエンジン、上流の遅延 |

数値はあくまで目安です。`search.circuit` の `window` や `open_seconds` を変えた場合は、しきい値も
合わせて調整してください。

## アラートの例

Prometheus の式はそのまま使えます。`for` は「その状態が続いたときだけ発火する」ための待ち時間です。

```promql
# 検索できるソースが 5 分間無い
searxng_ready == 0
# for: 5m

# いずれかのエンジンが 10 分間サーキット開放のまま
max by (engine) (searxng_engines_circuit_state{state="open"}) == 1
# for: 10m

# 窓が十分に埋まったうえで失敗率が高い（開放の直前〜開放中を検知する）
(searxng_engines_circuit_failure_ratio > 0.6)
  and on (engine) (searxng_engines_circuit_samples >= 10)
# for: 5m

# 1 時間を超える休止（自動復帰が遠い）
searxng_engines_suspended_seconds > 3600

# 自インスタンスの 5xx 率が 5% を超える
sum(rate(searxng_http_responses_total{status="5xx"}[5m]))
  / sum(rate(searxng_http_responses_total[5m])) > 0.05
# for: 10m

# 検索の平均所要時間が 5 秒を超える
sum(rate(searxng_search_duration_seconds_sum[5m]))
  / sum(rate(searxng_search_duration_seconds_count[5m])) > 5
```

`searxng_engines_circuit_failure_ratio` の比較は、`searxng_engines_circuit_samples` が
`search.circuit.minimum_samples` 以上になってから行います。窓が埋まる前の値は分母が小さく、
誤検知の原因になります。

## 復旧の手順

1. `/stats`（または `/stats/errors`）で `circuit`、`circuit_failure_ratio`、`circuit_samples`、
   `suspended_for` を確認します。`/metrics` の同じ系列と一致しているはずです。
2. 上流の状態を確認します。エンジンの URL が変わった、認証情報が失効した、ネットワークから
   遮断された、といった場合は設定を直します。
3. 設定を直しただけでは、既に開いているサーキットや休止はすぐには戻りません。開放時間
   （`open_seconds` から始まり、失敗のたびに倍）が過ぎるのを待つか、プロセスを再起動します。
   状態はメモリだけにあり、再起動でリセットされます。
4. 一時的な上流障害なら、待つだけで半開の試行が成功して `closed` に戻ります。
   `searxng_engines_circuit_state{state="half-open"}` が 1 になったあと、`closed` に戻るかを
   確認してください。
5. 恒久的に失敗するエンジンは、設定で無効化するか `[search.circuit]` のしきい値を見直します。
   しきい値を緩めると検索のたびに遅いエンジンを待つことになるため、まず上流の原因を直してください。

## 使い方の目安

- 生存確認は `/healthz`、構成の妥当性は `/readyz`（または `rustxng --check-config`）で行い、
  `searxng_ready` はアラート用の複製として使います。3 つは同じ判定を共有しています。
- `searxng_engines_error_count_total` の増加は上流の失敗、`searxng_engines_resting` は空応答が続いた休止を示します。
- `searxng_engines_circuit_state{state="open"}` はサーキットが開いて検索から外れている状態、
  `searxng_engines_circuit_failure_ratio` はその判断に使った直近の失敗率です。
- 検索の遅さは `searxng_search_duration_seconds_sum / _count` の平均で追います。分位点（p95 など）は提供しないため、必要なら `/stats` のタイミング表示や保存済み測定を使ってください。
- `searxng_http_responses_total{status="5xx"}` の増加は自インスタンスの障害、`4xx` の増加はクライアント側の誤りかレート制限です。応答自体は `/metrics` の描画後に数えるため、最初の取得では自分自身がまだ含まれません。
- `/healthz` も `/readyz` も上流の可用性は保証しません。実際の検索可否は JSON 検索で確認してください。

## 停止

- `SIGTERM`（コンテナや systemd の停止）と Ctrl-C（`SIGINT`）で安全に停止します。
  受け付けたリクエストは完了させてからプロセスを終了するため、`docker stop` や `systemctl stop` で
  強制終了（終了コード 143）になりません。
- `server.idle_timeout`（または `RUSTXNG_IDLE_TIMEOUT`）が 0 でなければ、無通信が続いた時点でも停止します。

## 制約

- メトリクスはプロセス単位です。複数インスタンスを並べる場合は集約側で合算してください。
- サーキットと休止の状態はプロセス内のメモリだけにあります。再起動するとカウンタ、休止、サーキットは
  すべて初期化され、状態は `closed` に戻ります。
- `searxng_engines_circuit_state` はエンジンごとに 3 系列（`closed` / `half-open` / `open`）を出します。
  系列数はエンジン数の 3 倍までで、エンジン名以外のラベルは増えません。
- `searxng_cache_bytes` はメモリキャッシュのみ意味を持ちます（Valkey バックエンドでは 0）。
- 系列名のうち `searxng_engines_*` は上流実装の名前を踏襲しています。
