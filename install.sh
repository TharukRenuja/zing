#!/bin/sh
set -eu

REPO="${REPO:-TharukRenuja/rxdl}"
VERSION="${VERSION:-latest}"

arch=$(uname -m)
os=$(uname -s | tr '[:upper:]' '[:lower:]')

case "$arch" in
  x86_64|amd64) arch="x86_64" ;;
  aarch64|arm64) arch="aarch64" ;;
  *) echo "unsupported arch: $arch"; exit 1 ;;
esac

case "$os" in
  linux) target="${arch}-unknown-linux-gnu" ;;
  darwin) target="${arch}-apple-darwin" ;;
  mingw*|msys*|cygwin*) target="${arch}-pc-windows-msvc"; ext=".exe" ;;
  *) echo "unsupported os: $os"; exit 1 ;;
esac

if [ "$VERSION" = "latest" ]; then
  url="https://github.com/${REPO}/releases/latest/download/rxdl-${target}.tar.gz"
else
  url="https://github.com/${REPO}/releases/download/v${VERSION}/rxdl-${target}.tar.gz"
fi

tmp=$(mktemp -d)
archive="${tmp}/rxdl.tar.gz"
curl -fsSL "$url" -o "$archive"
tar xzf "$archive" -C "$tmp"
cp "${tmp}/rxdl-${target}/rxdl${ext:-}" "$HOME/.local/bin/rxdl"
chmod +x "$HOME/.local/bin/rxdl"
rm -rf "$tmp"

echo "Installed to $HOME/.local/bin/rxdl"
echo "Make sure $HOME/.local/bin is in PATH"
