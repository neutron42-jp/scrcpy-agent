#!/usr/bin/env bash
# Build a single-file AppImage in dist/.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="$ROOT/dist"
APPDIR="$DIST/AppDir"
RUNTIME="$DIST/runtime-x86_64"

echo "==> cargo build --release"
cargo build --release --manifest-path "$ROOT/Cargo.toml"

rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/lib" \
         "$APPDIR/usr/share/applications" \
         "$APPDIR/usr/share/icons/hicolor/256x256/apps" \
         "$APPDIR/usr/share/glib-2.0/schemas"

install -Dm755 "$ROOT/target/release/scrcpy-agent" "$APPDIR/usr/bin/scrcpy-agent"
install -Dm755 "$ROOT/packaging/AppRun" "$APPDIR/AppRun"
install -Dm644 "$ROOT/packaging/scrcpy-agent.desktop" "$APPDIR/scrcpy-agent.desktop"
install -Dm644 "$ROOT/packaging/scrcpy-agent.desktop" \
    "$APPDIR/usr/share/applications/scrcpy-agent.desktop"

if command -v convert >/dev/null 2>&1; then
    convert -background none "$ROOT/packaging/icon.svg" -resize 256x256 "$APPDIR/scrcpy-agent.png"
    install -Dm644 "$APPDIR/scrcpy-agent.png" \
        "$APPDIR/usr/share/icons/hicolor/256x256/apps/scrcpy-agent.png"
else
    echo "warning: ImageMagick not found; using the SVG icon only" >&2
    cp "$ROOT/packaging/icon.svg" "$APPDIR/scrcpy-agent.svg"
fi

echo "==> GLib schemas"
mkdir -p "$APPDIR/usr/share/glib-2.0/schemas"
cp -r /usr/share/glib-2.0/schemas/. "$APPDIR/usr/share/glib-2.0/schemas/"
glib-compile-schemas "$APPDIR/usr/share/glib-2.0/schemas"

echo "==> shared-library closure"
python3 "$ROOT/scripts/bundle_libs.py" "$APPDIR/usr/bin/scrcpy-agent" "$APPDIR/usr/lib"

echo "==> AppImage runtime"
if [ ! -x "$RUNTIME" ]; then
    curl -sSL -o "$RUNTIME" \
        https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-x86_64
    chmod +x "$RUNTIME"
fi

echo "==> squashfs + AppImage"
rm -f "$DIST/scrcpy-agent.squashfs"
mksquashfs "$APPDIR" "$DIST/scrcpy-agent.squashfs" -root-owned -noappend -comp zstd -quiet
cat "$RUNTIME" "$DIST/scrcpy-agent.squashfs" > "$DIST/scrcpy-agent-x86_64.AppImage"
chmod +x "$DIST/scrcpy-agent-x86_64.AppImage"
rm -f "$DIST/scrcpy-agent.squashfs"

echo
echo "built: $DIST/scrcpy-agent-x86_64.AppImage"
echo "run:   $DIST/scrcpy-agent-x86_64.AppImage"
