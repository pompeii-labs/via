#!/usr/bin/env sh
set -eu

APP_NAME="via"
INSTALL_DIR="${VIA_INSTALL_DIR:-$HOME/.via/bin}"
REPO="${VIA_REPO:-pompeii-labs/via}"
VERSION="${VIA_VERSION:-}"

uname_s="$(uname -s)"
uname_m="$(uname -m)"

case "$uname_s" in
  Darwin) os="apple-darwin" ;;
  Linux) os="unknown-linux-gnu" ;;
  *) echo "unsupported OS: $uname_s" >&2; exit 1 ;;
esac

case "$uname_m" in
  arm64|aarch64) arch="aarch64" ;;
  x86_64|amd64) arch="x86_64" ;;
  *) echo "unsupported architecture: $uname_m" >&2; exit 1 ;;
esac

target="$arch-$os"
mkdir -p "$INSTALL_DIR"

if [ -n "$VERSION" ]; then
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  archive="$APP_NAME-$VERSION-$target.tar.gz"
  url="https://github.com/$REPO/releases/download/v$VERSION/$archive"
  echo "downloading $url"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$tmp/$archive"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$url" -O "$tmp/$archive"
  else
    echo "install needs curl or wget to download releases" >&2
    exit 1
  fi
  tar -xzf "$tmp/$archive" -C "$tmp"
  install -m 755 "$tmp/$APP_NAME" "$INSTALL_DIR/$APP_NAME"
else
  if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo is required when VIA_VERSION is not set" >&2
    exit 1
  fi
  script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
  cargo build --release --manifest-path "$script_dir/Cargo.toml"
  install -m 755 "$script_dir/target/release/$APP_NAME" "$INSTALL_DIR/$APP_NAME"
fi

echo "installed $APP_NAME to $INSTALL_DIR/$APP_NAME"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "add $INSTALL_DIR to PATH to run $APP_NAME directly" ;;
esac
