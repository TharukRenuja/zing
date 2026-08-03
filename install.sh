#!/bin/sh
set -eu

REPO="${REPO:-TharukRenuja/zing}"
VERSION="${VERSION:-latest}"

arch=$(uname -m)
os=$(uname -s | tr '[:upper:]' '[:lower:]')

case "$arch" in
  x86_64|amd64) arch="x86_64" ;;
  aarch64|arm64) arch="aarch64" ;;
  *) echo "unsupported arch: $arch"; exit 1 ;;
esac

case "$os" in
  linux)
    suffix="${arch}-linux"
    ext="tar.gz"
    bin="zing"
    daemon="zing-daemon"
    ;;
  darwin)
    case "$arch" in
      x86_64) echo "Intel Mac builds are not available"; exit 1 ;;
    esac
    suffix="${arch}-mac"
    ext="dmg"
    bin="zing"
    daemon="zing-daemon"
    ;;
  mingw*|msys*|cygwin*)
    suffix="${arch}-windows"
    ext="exe"
    bin="zing.exe"
    daemon=""
    ;;
  *) echo "unsupported os: $os"; exit 1 ;;
esac

if [ "$VERSION" = "latest" ]; then
  VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed 's/.*: "//;s/".*//')
fi

download_url="https://github.com/${REPO}/releases/download/${VERSION}/zing-${VERSION}-${suffix}.${ext}"

tmp=$(mktemp -d)
archive="${tmp}/zing.${ext}"

echo "Downloading zing ${VERSION} for ${suffix}..."
curl -fsSL "$download_url" -o "$archive"

case "$ext" in
  tar.gz)
    tar xzf "$archive" -C "$tmp"
    for f in "$tmp"/zing-*; do
      b=$(basename "$f")
      case "$b" in
        zing-daemon-*) cp "$f" "${tmp}/zing-daemon" ;;
        zing-*)        cp "$f" "${tmp}/zing" ;;
      esac
    done
    if [ ! -f "${tmp}/zing" ]; then
      echo "error: zing binary not found in archive"
      exit 1
    fi
    ;;
  dmg)
    mnt="${tmp}/mnt"
    hdiutil attach -quiet -nobrowse -mountpoint "$mnt" "$archive"
    cp "$mnt/zing" "${tmp}/"
    if [ -f "$mnt/zing-daemon" ]; then
      cp "$mnt/zing-daemon" "${tmp}/"
    fi
    hdiutil detach -quiet "$mnt"
    ;;
  exe)
    mv "$archive" "${tmp}/zing.exe"
    ;;
esac

dst="/usr/local/bin"
maybe_sudo=""
if [ ! -w "$dst" ]; then
  if command -v sudo >/dev/null 2>&1; then
    maybe_sudo="sudo"
  else
    echo "error: $dst is not writable and sudo is not available"
    exit 1
  fi
fi

echo "Installing to $dst..."
$maybe_sudo cp "${tmp}/zing" "$dst/$bin"
$maybe_sudo chmod +x "$dst/$bin"
echo "  $dst/$bin"

if [ -n "$daemon" ] && [ -f "${tmp}/zing-daemon" ]; then
  $maybe_sudo cp "${tmp}/zing-daemon" "$dst/$daemon"
  $maybe_sudo chmod +x "$dst/$daemon"
  echo "  $dst/$daemon"
fi

echo "Installing shell completions..."
compsh="$dst/zing"
if [ -d /usr/share/bash-completion/completions ]; then
  $compsh completions bash | $maybe_sudo tee /usr/share/bash-completion/completions/zing > /dev/null 2>&1 || true
  echo "  bash completions"
fi
if [ -d /usr/share/zsh/site-functions ]; then
  $compsh completions zsh | $maybe_sudo tee /usr/share/zsh/site-functions/_zing > /dev/null 2>&1 || true
  echo "  zsh completions"
fi
if [ -d /usr/share/fish/vendor_completions.d ]; then
  $compsh completions fish | $maybe_sudo tee /usr/share/fish/vendor_completions.d/zing.fish > /dev/null 2>&1 || true
  echo "  fish completions"
fi

# Register the daemon as a systemd *user* service so downloads can run in the
# background. This must run as the invoking user (not root): systemctl --user
# needs that user's session bus, which means the command must not go through
# the same sudo used for the /usr/local/bin copies above.
if [ "$os" = "linux" ] && [ -x "$dst/zing" ]; then
  if [ "$(id -u)" -eq 0 ] && [ -n "${SUDO_USER:-}" ]; then
    uid="$(id -u "$SUDO_USER")"
    home="$(getent passwd "$SUDO_USER" | cut -d: -f6 || true)"
    echo "Installing daemon service for user $SUDO_USER..."
    if ! sudo -u "$SUDO_USER" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
        "$dst/zing" daemon install; then
      echo "warning: could not register the daemon service"
      echo "         run manually as your user: zing daemon install"
    fi
  elif [ "$(id -u)" -ne 0 ]; then
    echo "Installing daemon service..."
    if ! "$dst/zing" daemon install; then
      echo "warning: could not register the daemon service"
      echo "         run manually: zing daemon install"
    fi
  else
    echo "warning: running as root, daemon service not registered"
    echo "         after install, run as your user: zing daemon install"
  fi
fi

rm -rf "$tmp"

echo "zing ${VERSION} installed to $dst"
echo "Restart your terminal or run: hash -r"
echo "The daemon service is ready: use 'zing daemon status' to check it."
