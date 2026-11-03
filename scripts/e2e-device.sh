#!/usr/bin/env bash
# End-to-end check against a real device (USB, authorized).
# Requires: adb, curl, python3. Starts a headless agent on an unused port.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${PORT:-18777}"
BIN="$ROOT/target/release/scrcpy-agent"
OUT="$ROOT/dist/e2e"
mkdir -p "$OUT"

if [ ! -x "$BIN" ]; then
    echo "build first: cargo build --release" >&2
    exit 1
fi

SERIAL="${SERIAL:-$(adb devices | awk 'NR>1 && $2=="device" {print $1; exit}')}"
if [ -z "${SERIAL:-}" ]; then
    echo "no authorized device found (adb devices)" >&2
    exit 1
fi
echo "device: $SERIAL"

cleanup() {
    if [ -n "${AGENT_PID:-}" ] && kill -0 "$AGENT_PID" 2>/dev/null; then
        kill "$AGENT_PID" 2>/dev/null || true
        wait "$AGENT_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

"$BIN" --headless --serial "$SERIAL" --mcp-port "$PORT" --log-level info &
AGENT_PID=$!

for _ in $(seq 1 50); do
    if curl -fsS "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1; then break; fi
    sleep 0.2
done

call() {
    local name="$1" args="${2:-{}}"
    curl -fsS -X POST "http://127.0.0.1:$PORT/mcp" \
        -H 'Content-Type: application/json' -H 'Accept: application/json' \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$name\",\"arguments\":$args}}"
}

echo "==> session status"
call session_status '{}' | python3 -c 'import sys,json; print(json.load(sys.stdin)["result"]["content"][0]["text"])'

echo "==> screenshot"
call screenshot '{}' > "$OUT/screenshot.json"
python3 - "$OUT/screenshot.json" "$OUT/screen1.png" <<'PY'
import base64, json, sys, struct
payload = json.load(open(sys.argv[1]))["result"]
image = next(c for c in payload["content"] if c["type"] == "image")
data = base64.b64decode(image["data"])
open(sys.argv[2], "wb").write(data)
w, h = struct.unpack(">II", data[16:24])
print(f"screenshot {w}x{h} ({len(data)} bytes)")
PY

WM_SIZE="$(adb -s "$SERIAL" shell wm size | tr -d '\r')"
echo "wm size: $WM_SIZE"

echo "==> launch Settings, tap search, type text"
call launch_app '{"package":"com.android.settings"}' >/dev/null
sleep 1.5
call screenshot '{}' > "$OUT/before.json"
python3 - "$OUT/before.json" <<'PY' > "$OUT/coords.txt"
import json, sys, struct, base64
payload = json.load(open(sys.argv[1]))["result"]
image = next(c for c in payload["content"] if c["type"] == "image")
data = base64.b64decode(image["data"])
w, h = struct.unpack(">II", data[16:24])
meta = json.loads(next(c for c in payload["content"] if c["type"] == "text")["text"])
print(meta.get("width", w), meta.get("height", h))
PY
read -r W H < "$OUT/coords.txt"
X=$((W / 2))
Y=$((H / 12))
call tap "{\"x\":$X,\"y\":$Y}" >/dev/null
sleep 0.6
call type_text '{"text":"bluetooth"}' >/dev/null
sleep 0.8
call screenshot '{}' > "$OUT/after.json"
python3 - "$OUT/before.json" "$OUT/after.json" <<'PY'
import base64, json, sys
def image(path):
    payload = json.load(open(path))["result"]
    return base64.b64decode(next(c for c in payload["content"] if c["type"] == "image")["data"])
before, after = image(sys.argv[1]), image(sys.argv[2])
print("screen changed after typing:", before != after)
PY

echo "==> back, notifications, rotate"
call back '{}' >/dev/null
call notifications '{"open":true}' >/dev/null
sleep 0.4
call notifications '{"open":false}' >/dev/null
call rotate '{}' >/dev/null || true
call session_status '{}' | python3 -c 'import sys,json; print(json.load(sys.stdin)["result"]["content"][0]["text"])'

echo "==> stop session"
call stop_session '{}' >/dev/null
echo "E2E finished; artifacts in $OUT"
