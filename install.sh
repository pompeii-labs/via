#!/usr/bin/env sh
set -eu

APP_NAME="via"
INSTALL_DIR="${VIA_INSTALL_DIR:-$HOME/.via/bin}"
REPO="${VIA_REPO:-pompeii-labs/via}"
VERSION="${VIA_VERSION:-${1:-latest}}"

uname_s="$(uname -s)"
uname_m="$(uname -m)"

case "$uname_s" in
  Darwin) os="macos" ;;
  Linux) os="linux" ;;
  *) echo "unsupported OS: $uname_s" >&2; exit 1 ;;
esac

case "$uname_m" in
  arm64|aarch64) arch="arm64" ;;
  x86_64|amd64) arch="x86_64" ;;
  *) echo "unsupported architecture: $uname_m" >&2; exit 1 ;;
esac

artifact="$APP_NAME-$os-$arch"

need_downloader() {
  if command -v curl >/dev/null 2>&1; then
    return 0
  fi
  if command -v wget >/dev/null 2>&1; then
    return 0
  fi
  echo "install needs curl or wget" >&2
  exit 1
}

download_file() {
  url="$1"
  output="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$output"
  else
    wget -q "$url" -O "$output"
  fi
}

download_stdout() {
  url="$1"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url"
  else
    wget -q "$url" -O -
  fi
}

resolve_version() {
  if [ "$VERSION" = "latest" ]; then
    tag="$(download_stdout "https://api.github.com/repos/$REPO/releases/latest" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)"
    if [ -z "$tag" ]; then
      echo "could not resolve latest Via release for $REPO" >&2
      exit 1
    fi
    printf '%s\n' "${tag#v}"
  else
    printf '%s\n' "${VERSION#v}"
  fi
}

verify_checksum() {
  check_archive="$1"
  checksum="$2"
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$(dirname "$check_archive")" && sha256sum -c "$(basename "$checksum")")
  elif command -v shasum >/dev/null 2>&1; then
    (cd "$(dirname "$check_archive")" && shasum -a 256 -c "$(basename "$checksum")")
  else
    echo "warning: sha256 checker not found; skipping checksum verification" >&2
  fi
}

update_path() {
  case ":$PATH:" in
    *":$INSTALL_DIR:"*) return 0 ;;
  esac

  if [ "${VIA_NO_PATH_UPDATE:-0}" = "1" ]; then
    echo "add $INSTALL_DIR to PATH to run $APP_NAME directly"
    return 0
  fi

  profile="${VIA_PROFILE:-}"
  if [ -z "$profile" ]; then
    shell_name="$(basename "${SHELL:-sh}")"
    case "$shell_name" in
      zsh) profile="$HOME/.zshrc" ;;
      bash) profile="$HOME/.bashrc" ;;
      *) profile="$HOME/.profile" ;;
    esac
  fi

  if [ "$INSTALL_DIR" = "$HOME/.via/bin" ]; then
    path_line='export PATH="$HOME/.via/bin:$PATH"'
  else
    path_line="export PATH=\"$INSTALL_DIR:\$PATH\""
  fi

  touch "$profile"
  if ! grep -F "$path_line" "$profile" >/dev/null 2>&1; then
    {
      printf '\n# Via\n'
      printf '%s\n' "$path_line"
    } >> "$profile"
    echo "added $INSTALL_DIR to PATH in $profile"
    echo "restart your shell or run: . $profile"
  fi
}

need_downloader
version="$(resolve_version)"
archive="$artifact.tar.gz"
base_url="${VIA_RELEASE_BASE_URL:-https://github.com/$REPO/releases/download/v$version}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$INSTALL_DIR"

echo "downloading $base_url/$archive"
download_file "$base_url/$archive" "$tmp/$archive"

if download_file "$base_url/$archive.sha256" "$tmp/$archive.sha256"; then
  verify_checksum "$tmp/$archive" "$tmp/$archive.sha256"
else
  echo "warning: checksum not found for $archive" >&2
fi

tar -C "$tmp" -x -z -f "$tmp/$archive"
if [ ! -x "$tmp/$APP_NAME" ]; then
  echo "release archive did not contain executable $APP_NAME" >&2
  exit 1
fi

install -m 755 "$tmp/$APP_NAME" "$INSTALL_DIR/$APP_NAME"
update_path

echo "installed $APP_NAME $version to $INSTALL_DIR/$APP_NAME"
