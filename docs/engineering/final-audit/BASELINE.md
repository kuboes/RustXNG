# BASELINE — 変更前の状態

| 項目 | 値 |
| --- | --- |
| Commit | f7639ce（v0.31.2） |
| 作業ツリー | クリーン（開始時 `git status` 空） |
| CI（main, run 43703503946） | `ci` 失敗。失敗ステップは 3 OS 共通で `Format` |
| 他のワークフロー | security-audit 成功、nightly-offline 成功 |
| `cargo fmt --all -- --check` | 失敗（16 箇所 / src/cache.rs, src/network.rs, src/results.rs） |
| `cargo clippy --locked --all-targets -- -D warnings` | 成功 |
| `cargo test --locked --all-targets` | 1137 passed / 4 ignored / 0 failed |
| Python | 65 tests OK、check_provider_policy 通過、provider_inventory 一致 |
| `cargo audit --json` | vulnerabilities 0 / warnings 0（advisory DB 2026-11-15） |
| release バイナリ | 35,456,640 bytes（2026-11-15 ビルド） |
| 起動→/healthz | 未測定（本セッションで初回測定） |
