#!/usr/bin/env bash
set -euo pipefail

targets=(
  "aarch64-apple-darwin"
  "x86_64-apple-darwin"
  "x86_64-unknown-linux-gnu"
  "aarch64-unknown-linux-gnu"
)

for target in "${targets[@]}"; do
  echo "==> building $target"
  rustup target add "$target" >/dev/null
  cargo build --release --target "$target"
done
