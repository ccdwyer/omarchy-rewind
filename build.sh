#!/bin/sh
# Build rewindd. The plugin QML degrades to compat/rewindd.sh when
# bin/rewindd is missing, so a failed build is not fatal at runtime.

set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
SRC="$ROOT/src/rewindd"
OUT="$ROOT/bin"

mkdir -p "$OUT"
chmod +x "$ROOT/compat/rewindd.sh" 2>/dev/null || true
chmod +x "$ROOT/scripts/"*.sh 2>/dev/null || true
chmod +x "$ROOT/scripts/rewind" 2>/dev/null || true

install_fallback() {
  # Keep the POSIX fallback ONLY at its compat path — do NOT write bin/rewindd.
  # The QML probe treats an executable bin/rewindd as the real Rust helper
  # (usingFallback=false); masquerading the shell fallback there would make the
  # UI wrongly claim recording works. With bin/rewindd absent, the probe finds
  # compat/rewindd.sh and reports "fallback" honestly (non-recording). The
  # `rewind` CLI (wipe/stats/query) is still provided from the fallback so those
  # commands work without the Rust binary.
  rm -f "$OUT/rewindd"
  cp "$ROOT/compat/rewindd.sh" "$OUT/rewind"
  chmod +x "$OUT/rewind"
  echo "build.sh: no Rust helper; recording unavailable. rewind CLI installed; daemon uses compat/rewindd.sh (fallback, non-recording)"
}

if ! command -v cargo >/dev/null 2>&1; then
  echo "build.sh: cargo not found; installing POSIX fallback" >&2
  install_fallback
  exit 0
fi

if cargo build --release --features wayland --manifest-path "$SRC/Cargo.toml"; then
  :
elif cargo build --release --manifest-path "$SRC/Cargo.toml"; then
  echo "build.sh: wayland feature unavailable; grim-only binary" >&2
else
  echo "build.sh: cargo build failed; installing POSIX fallback" >&2
  install_fallback
  exit 0
fi

BIN="$SRC/target/release/rewindd"
if [ ! -x "$BIN" ]; then
  echo "build.sh: release binary missing after cargo build" >&2
  install_fallback
  exit 1
fi
cp "$BIN" "$OUT/rewindd"
chmod +x "$OUT/rewindd"
cp "$OUT/rewindd" "$OUT/rewind"
echo "build.sh: wrote $OUT/rewindd"
