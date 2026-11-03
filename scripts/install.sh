#!/usr/bin/env bash
# Install into ~/.local (or $PREFIX).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"

cargo build --release --manifest-path "$ROOT/Cargo.toml"
install -Dm755 "$ROOT/target/release/scrcpy-agent" "$PREFIX/bin/scrcpy-agent"
install -Dm644 "$ROOT/packaging/scrcpy-agent.desktop" \
    "$PREFIX/share/applications/scrcpy-agent.desktop"
if command -v convert >/dev/null 2>&1; then
    mkdir -p "$PREFIX/share/icons/hicolor/256x256/apps"
    convert -background none "$ROOT/packaging/icon.svg" -resize 256x256 \
        "$PREFIX/share/icons/hicolor/256x256/apps/scrcpy-agent.png"
fi
update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true

echo "installed: $PREFIX/bin/scrcpy-agent"
echo "register this MCP endpoint with your agent: http://127.0.0.1:18765/mcp"
