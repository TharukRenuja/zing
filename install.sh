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
    gui="zing-gui"
    tray="zing-tray"
    ;;
  darwin)
    case "$arch" in
      x86_64) echo "Intel Mac builds are not available"; exit 1 ;;
    esac
    suffix="${arch}-mac"
    ext="dmg"
    bin="zing"
    daemon="zing-daemon"
    gui="zing-gui"
    tray="zing-tray"
    ;;
  mingw*|msys*|cygwin*)
    suffix="${arch}-windows"
    ext="exe"
    bin="zing.exe"
    daemon=""
    gui=""
    ;;
  *) echo "unsupported os: $os"; exit 1 ;;
esac

if [ "$VERSION" = "latest" ]; then
  VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep -o '"tag_name": "[^"]*"' | head -1 | sed 's/"tag_name": "//;s/"$//')
fi

if [ -z "$VERSION" ]; then
  echo "error: failed to determine latest version"
  exit 1
fi

case "$VERSION" in
  v*) ;;
  *) echo "error: invalid version format: $VERSION"; exit 1 ;;
esac

download_url="https://github.com/${REPO}/releases/download/${VERSION}/zing-${VERSION}-${suffix}.${ext}"

tmp=$(mktemp -d)
archive="${tmp}/zing.${ext}"

echo "Downloading zing ${VERSION} for ${suffix}..."
if ! curl -fsSL "$download_url" -o "$archive"; then
  rm -rf "$tmp"
  echo "error: failed to download from $download_url"
  exit 1
fi

case "$ext" in
  tar.gz)
    tar xzf "$archive" -C "$tmp"
    for f in "$tmp"/zing-*; do
      b=$(basename "$f")
      case "$b" in
        zing-daemon-*) cp "$f" "${tmp}/zing-daemon" ;;
        zing-gui-*)    cp "$f" "${tmp}/zing-gui" ;;
        zing-tray-*)   cp "$f" "${tmp}/zing-tray" ;;
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
    if [ -f "$mnt/zing-gui" ]; then
      cp "$mnt/zing-gui" "${tmp}/"
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

if [ -n "$gui" ] && [ -f "${tmp}/zing-gui" ]; then
  $maybe_sudo cp "${tmp}/zing-gui" "$dst/$gui"
  $maybe_sudo chmod +x "$dst/$gui"
  echo "  $dst/$gui"
fi

if [ -n "$tray" ] && [ -f "${tmp}/zing-tray" ]; then
  $maybe_sudo cp "${tmp}/zing-tray" "$dst/$tray"
  $maybe_sudo chmod +x "$dst/$tray"
  echo "  $dst/$tray"
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

# Write the browser native-host manifests so the extension works out of the
# box. This must run as the invoking user (not root): the manifests live in the
# user's browser config/home dirs. Best-effort: warn, don't abort.
if [ -x "$dst/zing" ]; then
  run_user_commands() {
    if [ "$(id -u)" -eq 0 ] && [ -n "${SUDO_USER:-}" ]; then
      uid="$(id -u "$SUDO_USER")"
      home="$(getent passwd "$SUDO_USER" | cut -d: -f6 || true)"
      sudo -u "$SUDO_USER" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" "$@"
    else
      "$@"
    fi
  }
  if [ "$(id -u)" -eq 0 ] && [ -n "${SUDO_USER:-}" ]; then
    echo "Installing browser native host manifests for user $SUDO_USER..."
    if ! run_user_commands "$dst/zing" extension install; then
      echo "warning: could not write browser native host manifests"
      echo "         run manually as your user: zing extension install"
    fi
  elif [ "$(id -u)" -ne 0 ]; then
    echo "Installing browser native host manifests..."
    if ! run_user_commands "$dst/zing" extension install; then
      echo "warning: could not write browser native host manifests"
      echo "         run manually: zing extension install"
    fi
  fi
fi

# Create a desktop entry so the GUI shows up in the start menu / launcher.
# The entry is user-local and owned by the invoking user. Autostart is enabled
# so the tray icon is present after login (like a real download manager).
if [ -x "$dst/$gui" ]; then
  # Fetch the app icon from the repo and install it to the pixmaps dir.
  if command -v curl >/dev/null 2>&1; then
    if [ -d /usr/share/pixmaps ] && [ -w /usr/share/pixmaps ]; then
      curl -fsSL "https://raw.githubusercontent.com/${REPO}/main/packaging/icons/zing.png" -o /usr/share/pixmaps/zing.png 2>/dev/null || true
    elif [ -d /usr/local/share/pixmaps ] && [ -w /usr/local/share/pixmaps ]; then
      curl -fsSL "https://raw.githubusercontent.com/${REPO}/main/packaging/icons/zing.png" -o /usr/local/share/pixmaps/zing.png 2>/dev/null || true
    fi
  fi
  run_user_commands "$dst/$gui" --install-desktop-entry --autostart > /dev/null 2>&1 || \
    echo "  warning: could not create the desktop entry; run: $gui --install-desktop-entry"
fi

rm -rf "$tmp"

# ── Restart running services after install/update ────────────────
if [ "$os" = "linux" ]; then
  # Restart the daemon via systemd if the service is registered
  restart_daemon() {
    if [ "$(id -u)" -eq 0 ] && [ -n "${SUDO_USER:-}" ]; then
      uid="$(id -u "$SUDO_USER")"
      home="$(getent passwd "$SUDO_USER" | cut -d: -f6 || true)"
      sudo -u "$SUDO_USER" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
        systemctl --user restart zing-daemon 2>/dev/null || true
    elif [ "$(id -u)" -ne 0 ]; then
      systemctl --user restart zing-daemon 2>/dev/null || true
    fi
  }

  # Restart the tray if it's running (kill old, relaunch)
  restart_tray() {
    local tray_pid=""
    if [ "$(id -u)" -eq 0 ] && [ -n "${SUDO_USER:-}" ]; then
      uid="$(id -u "$SUDO_USER")"
      tray_pid=$(sudo -u "$SUDO_USER" pgrep -x zing-tray 2>/dev/null || true)
    elif [ "$(id -u)" -ne 0 ]; then
      tray_pid=$(pgrep -x zing-tray 2>/dev/null || true)
    fi
    if [ -n "$tray_pid" ]; then
      echo "Restarting zing-tray..."
      if [ "$(id -u)" -eq 0 ] && [ -n "${SUDO_USER:-}" ]; then
        uid="$(id -u "$SUDO_USER")"
        home="$(getent passwd "$SUDO_USER" | cut -d: -f6 || true)"
        sudo -u "$SUDO_USER" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
          kill "$tray_pid" 2>/dev/null || true
        sleep 1
        sudo -u "$SUDO_USER" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
          G_MESSAGES_DEBUG="" "$dst/$tray" </dev/null >/dev/null 2>&1 &
      elif [ "$(id -u)" -ne 0 ]; then
        kill "$tray_pid" 2>/dev/null || true
        sleep 1
        G_MESSAGES_DEBUG="" "$dst/$tray" </dev/null >/dev/null 2>&1 &
      fi
    fi
  }

  echo "Restarting services..."
  restart_daemon
  restart_tray
fi

echo "zing ${VERSION} installed to $dst"
echo "Restart your terminal or run: hash -r"
echo "The daemon service is ready: use 'zing daemon status' to check it."
echo "The GUI is available: run 'zing-gui' (look for 'zing GUI' in your launcher)."