#!/bin/sh
set -eu

dst="/usr/local/bin"
maybe_sudo=""
if [ ! -w "$dst" ]; then
  if command -v sudo >/dev/null 2>&1; then
    maybe_sudo="sudo"
  fi
fi

echo "Stopping daemon service..."
if [ -x "$dst/zing" ]; then
  if [ "$(id -u)" -eq 0 ] && [ -n "${SUDO_USER:-}" ]; then
    uid="$(id -u "$SUDO_USER")"
    home="$(getent passwd "$SUDO_USER" | cut -d: -f6 || true)"
    sudo -u "$SUDO_USER" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
        "$dst/zing" daemon uninstall 2>/dev/null || true
  elif [ "$(id -u)" -ne 0 ]; then
    "$dst/zing" daemon uninstall 2>/dev/null || true
  fi
fi

echo "Removing binaries..."
$maybe_sudo rm -f "$dst/zing" "$dst/zing-daemon"

if command -v systemctl >/dev/null 2>&1; then
  if [ -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/zing-daemon.service" ]; then
    echo "Removing systemd user service..."
    systemctl --user disable --now zing-daemon 2>/dev/null || true
    rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/zing-daemon.service"
    systemctl --user daemon-reload 2>/dev/null || true
  fi
fi

echo "Removing config and schedule..."
rm -rf "${XDG_CONFIG_HOME:-$HOME/.config}/zing"

echo "Cleaning up socket and auth token..."
rm -f /tmp/zing.sock /tmp/zing.sock.auth

echo "zing uninstalled"
