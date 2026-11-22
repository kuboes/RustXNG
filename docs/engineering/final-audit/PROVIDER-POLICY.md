# PROVIDER-POLICY

## 変更

1. `tools/check_provider_policy.py` の clean プロファイル検証を実行時構成に一致させた。
   `settings.clean.toml` は `use_default_settings = false` で Standard の engines を継承しないため、
   旧検証は「存在しない構成」を検査していた。現在は overlay の engines（空）を検証する。
2. `review_date` が 365 日を超えた場合にエラーにする（`REVIEW_MAX_AGE_DAYS`）。失効は自動無効化ではなく
   再確認の要求。
3. 回帰テストを 1 件追加（400 日前の review_date で「older than」が出る）。

## 現状の台帳（測定点）

- settings.toml: engines 273（有効 34 / 無効 239 / inactive 0）、`engine_allowlist` 空
- 有効 34 件すべて `standard_allowed = true`、`status = reviewed_allowed_for_scope`、review_date 2026-11-05
- `provider_inventory.py --check` 一致、`check_provider_policy.py` 通過
- 未審査 148 件はすべて無効

## 未確認

- 各プロバイダーの一次資料の再確認は本セッションでは行っていない（review_date は 2026-11-05 の記録のまま）
- 商用利用・保存・再配布の可否は提供元ごとに異なり、台帳の記録以上の保証はしない
