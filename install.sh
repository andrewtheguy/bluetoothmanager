#!/bin/sh
# Install the latest bluetoothmanager release binary from GitHub.
#
#   curl -fsSL https://raw.githubusercontent.com/andrewtheguy/bluetoothmanager/main/install.sh | sh
#
# Environment:
#   VERSION      release tag to install, e.g. v0.0.1 (default: latest)
#   INSTALL_DIR  where to put the binary (default: /usr/local/bin if writable
#                or running as root, otherwise ~/.local/bin)
set -eu

REPO="andrewtheguy/bluetoothmanager"
BIN="bluetoothmanager"

err() { printf 'install: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = "Linux" ] || err "only Linux is supported (BlueZ over D-Bus); got $(uname -s)"

case "$(uname -m)" in
  x86_64|amd64)  arch=linux-amd64 ;;
  aarch64|arm64) arch=linux-arm64 ;;
  *) err "no prebuilt binary for $(uname -m); build from source with: cargo install --git https://github.com/$REPO" ;;
esac

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL --retry 3 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -qO "$2" "$1"; }
else
  err "need curl or wget"
fi

if [ -n "${INSTALL_DIR:-}" ]; then
  dir=$INSTALL_DIR
elif [ "$(id -u)" = 0 ] || [ -w /usr/local/bin ]; then
  dir=/usr/local/bin
else
  dir=$HOME/.local/bin
fi

asset="$BIN-$arch"
if [ -n "${VERSION:-}" ]; then
  url="https://github.com/$REPO/releases/download/$VERSION/$asset"
else
  url="https://github.com/$REPO/releases/latest/download/$asset"
fi

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT INT TERM

printf 'downloading %s\n' "$url"
fetch "$url" "$tmp" || err "download failed: $url"
chmod 755 "$tmp"

mkdir -p "$dir"
mv "$tmp" "$dir/$BIN"
trap - EXIT
printf 'installed %s\n' "$("$dir/$BIN" --version)"
printf '  -> %s\n' "$dir/$BIN"

case ":$PATH:" in
  *":$dir:"*) ;;
  *) printf '\nnote: %s is not on your PATH. Add it with:\n  export PATH="%s:$PATH"\n' "$dir" "$dir" ;;
esac
