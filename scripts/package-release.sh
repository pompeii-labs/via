#!/usr/bin/env bash
set -euo pipefail

version="${VIA_VERSION:-$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)}"
dist="dist"
mkdir -p "$dist"

targets=(
  "aarch64-apple-darwin"
  "x86_64-apple-darwin"
  "x86_64-unknown-linux-gnu"
  "aarch64-unknown-linux-gnu"
)

for target in "${targets[@]}"; do
  bin="target/$target/release/via"
  if [ ! -x "$bin" ]; then
    echo "missing $bin; run scripts/build-release.sh first" >&2
    exit 1
  fi
  work="$(mktemp -d)"
  cp "$bin" "$work/via"
  tar -C "$work" -czf "$dist/via-$version-$target.tar.gz" via
  rm -rf "$work"
done

echo "wrote release archives to $dist/"
