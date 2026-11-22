# REPRODUCE — 検証手順

```bash
git clone https://github.com/RustXNG/RustXNG.git
cd RustXNG
git checkout f7639ce
# 本セッションの変更を適用した作業ツリー（未コミット）を使う

cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo check --locked --all-targets --no-default-features

cargo test --locked --test extraction   # 本文抽出の回帰コーパス（ネットワーク不要）
python3 -m unittest discover -s tools -p "test_*.py"
python3 tools/smoke_agent_setup.py --binary target/debug/rustxng   # HTTP + /readyz + MCP stdio（オフライン）

# コンテナ相当（上書きだけの settings.toml を置き、索引の無い構成で起動）
#   printf '# overrides only\n' > /tmp/settings.toml
#   SEARXNG_SETTINGS_PATH=/tmp/settings.toml target/release/rustxng --bind 127.0.0.1 --port 29577
#   curl -s http://127.0.0.1:29577/readyz
#   SEARXNG_SETTINGS_PATH=/tmp/settings.toml target/release/rustxng --check-config

# コンテナ（この環境では docker が podman エミュレーション）
podman build -t rustxng:local .
podman run --detach --name rustxng-verify --publish 127.0.0.1:29578:29483 rustxng:local
curl -s http://127.0.0.1:29578/readyz
podman exec rustxng-verify rustxng --check-config --profile clean
podman stop -t 15 rustxng-verify
python3 tools/check_provider_policy.py
python3 tools/provider_inventory.py --check
cargo audit --json

# 性能（clean プロファイル、ローカル索引が必要）
cargo build --release --locked --bin rustxng
target/release/rustxng --profile clean --bind 127.0.0.1 --port 29558 &
python3 tools/eval_quality.py --base http://127.0.0.1:29558 \
  --questions bench/quality-questions-100.json --delay 0 --json /tmp/clean100.json
```

個別の確認:

```bash
# エラー形式
curl -s -D- 'http://127.0.0.1:29558/search?format=json' | head -5
# pageno 上限
curl -s 'http://127.0.0.1:29558/search?q=rust&format=json&pageno=4294967295'
# JSON エラー形式（400）
curl -s 'http://127.0.0.1:29558/search?format=csv'
```

Valkey の clear はローカルの Valkey か、src/cache.rs のモックサーバーテスト
（`cargo test --locked --lib cache::valkey_tests`）で再現できる。