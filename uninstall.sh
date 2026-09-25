#!/bin/sh
set -eu

dst="/usr/local/bin"
maybe_sudo=""
if [ ! -w "$dst" ]; then
  if command -v sudo >/dev/null 2>&1; then
    maybe_sudo="sudo"
  fi
fi

echo "Stopping services..."

# Stop the systemd service first so it doesn't auto-restart on kill.
if command -v systemctl >/dev/null 2>&1; then
  systemctl --user stop zing-daemon.service 2>/dev/null || true
  systemctl --user disable zing-daemon.service 2>/dev/null || true
fi

# Force kill any remaining processes (may not respond to SIGTERM).
pkill -9 -x zing-daemon 2>/dev/null || true

# Remove systemd service file.
if command -v systemctl >/dev/null 2>&1; then
  if [ -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/zing-daemon.service" ]; then
    rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/zing-daemon.service"
    systemctl --user daemon-reload 2>/dev/null || true
  fi
fi

echo "Removing binaries..."
$maybe_sudo rm -f "$dst/zing" "$dst/zing-daemon"
rm -f "$HOME/.local/bin/zing" "$HOME/.local/bin/zing-daemon"

echo "Removing config and schedule..."
rm -rf "${XDG_CONFIG_HOME:-$HOME/.config}/zing"

echo "Cleaning up socket and auth token..."
rm -f /tmp/zing.sock /tmp/zing.sock.auth
uid="$(id -u)"
rm -f "/run/user/$uid/zing.sock" "/run/user/$uid/zing.sock.auth"

echo "zing uninstalled"
