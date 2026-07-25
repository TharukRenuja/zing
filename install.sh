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

rm -rf "$tmp"

echo "zing ${VERSION} installed to $dst"

case "$os" in
  linux)
    if command -v systemctl >/dev/null 2>&1; then
      echo
      printf "Set up systemd user service for zing-daemon? [Y/n] "
      read -r ans
      case "$ans" in
        n*|N*) ;;
        *)
          svc="zing-daemon.service"
          svc_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
          svc_url="https://raw.githubusercontent.com/${REPO}/main/daemon/${svc}"
          mkdir -p "$svc_dir"
          if curl -fsSL "$svc_url" -o "$svc_dir/$svc"; then
            systemctl --user daemon-reload
            printf "Enable and start zing-daemon now? [Y/n] "
            read -r ans2
            case "$ans2" in
              n*|N*) echo "  Run later: systemctl --user enable --now zing-daemon" ;;
              *)
                systemctl --user enable --now zing-daemon
                echo "  zing-daemon enabled and started"
                ;;
            esac
          else
            echo "  warning: failed to download service file from $svc_url"
          fi
          ;;
      esac
    fi
    ;;
esac
