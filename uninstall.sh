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
# Kill tray if running
pkill -x zing-tray 2>/dev/null || true
# Kill GUI if running
pkill -x zing-gui 2>/dev/null || true
# Uninstall daemon service
zing_bin=""
if [ -x "$dst/zing" ]; then
  zing_bin="$dst/zing"
elif [ -x "$HOME/.local/bin/zing" ]; then
  zing_bin="$HOME/.local/bin/zing"
fi
if [ -n "$zing_bin" ]; then
  if [ "$(id -u)" -eq 0 ] && [ -n "${SUDO_USER:-}" ]; then
    uid="$(id -u "$SUDO_USER")"
    home="$(getent passwd "$SUDO_USER" | cut -d: -f6 || true)"
    sudo -u "$SUDO_USER" env HOME="$home" XDG_RUNTIME_DIR="/run/user/$uid" \
        "$zing_bin" daemon uninstall 2>/dev/null || true
  elif [ "$(id -u)" -ne 0 ]; then
    "$zing_bin" daemon uninstall 2>/dev/null || true
  fi
fi

echo "Removing binaries..."
$maybe_sudo rm -f "$dst/zing" "$dst/zing-daemon" "$dst/zing-gui" "$dst/zing-tray"
# Also check ~/.local/bin (some install paths end up here)
rm -f "$HOME/.local/bin/zing" "$HOME/.local/bin/zing-daemon" "$HOME/.local/bin/zing-gui" "$HOME/.local/bin/zing-tray"

if command -v systemctl >/dev/null 2>&1; then
  if [ -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/zing-daemon.service" ]; then
    echo "Removing systemd user service..."
    systemctl --user disable --now zing-daemon 2>/dev/null || true
    rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/zing-daemon.service"
    systemctl --user daemon-reload 2>/dev/null || true
  fi
fi

echo "Removing desktop entries..."
rm -f "${HOME}/.local/share/applications/zing-gui.desktop"
rm -f "${HOME}/.config/autostart/zing-gui.desktop"

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "${HOME}/.local/share/applications" 2>/dev/null || true
fi

echo "Removing config and schedule..."
rm -rf "${XDG_CONFIG_HOME:-$HOME/.config}/zing"

echo "Cleaning up socket and auth token..."
rm -f /tmp/zing.sock /tmp/zing.sock.auth
uid="$(id -u)"
rm -f "/run/user/$uid/zing.sock" "/run/user/$uid/zing.sock.auth"

echo "zing uninstalled"
