#!/bin/bash
# Install caly as a per-user systemd service.
# Usage: bash packaging/install-systemd.sh [--prefix /usr/local]
set -eu

PREFIX="${1:-/usr/local}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="$PREFIX/bin"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"

echo "==> Building (release)"
(cd "$ROOT" && cargo build --release --bin caly)

echo "==> Installing binary to $BIN_DIR"
install -Dm755 "$ROOT/target/release/caly" "$BIN_DIR/caly"

echo "==> Installing unit to $UNIT_DIR"
install -Dm644 "$ROOT/packaging/systemd/caly.service" "$UNIT_DIR/caly.service"

echo "==> Reloading user units"
systemctl --user daemon-reload

echo "==> Enabling and starting"
systemctl --user enable --now caly.service

echo "==> Verify: systemctl --user status caly ; caly status"
echo "==> For TUN, run once: caly doctor --fix   (grants CAP_NET_ADMIN to the core binaries)"
