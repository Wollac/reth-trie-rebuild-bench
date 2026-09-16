#!/usr/bin/env bash
# Runs ON the box: toolchain, reth binary, the bench binary. Idempotent.
set -euo pipefail
RETH_VERSION="${RETH_VERSION:-v2.5.2}"

export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y -q build-essential clang libclang-dev pkg-config sysstat curl git

if ! command -v cargo >/dev/null; then
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
# shellcheck disable=SC1091
source "$HOME/.cargo/env"

if ! command -v reth >/dev/null || ! reth --version | grep -q "${RETH_VERSION#v}"; then
  curl -sSL "https://github.com/paradigmxyz/reth/releases/download/${RETH_VERSION}/reth-${RETH_VERSION}-x86_64-unknown-linux-gnu.tar.gz" \
    | tar -xz -C /usr/local/bin reth
fi
reth --version

cd "$HOME/reth-trie-rebuild-bench"
cargo build --release -p reth-trie-rebuild-bench --bin datadir_bench
echo "setup done"
