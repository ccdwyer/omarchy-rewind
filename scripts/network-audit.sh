#!/bin/sh
# Concrete no-network proof for rewindd: ldd + strace of a capture cycle.
# Fails when the helper binary is missing (including this macOS authoring host).

set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
BIN="$ROOT/bin/rewindd"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
export REWIND_DATA_DIR="$TMP/data"
mkdir -p "$REWIND_DATA_DIR"

if [ ! -x "$BIN" ]; then
  echo "FAIL: $BIN missing — no prebuilt Linux helper on this machine. Run ./build.sh on Linux or use the linux-helper GitHub Actions artifact." >&2
  exit 1
fi

if head -1 "$BIN" 2>/dev/null | grep -q '^#!'; then
  echo "FAIL: $BIN is the POSIX fallback script, not a compiled helper. Network audit requires bin/rewindd from ./build.sh (Linux CI)." >&2
  exit 1
fi

echo "=== ldd $BIN ==="
if command -v ldd >/dev/null 2>&1; then
  ldd "$BIN"
else
  if [ "$(uname -s)" = "Linux" ]; then
    echo "FAIL: ldd missing on Linux" >&2
    exit 1
  fi
  echo "ldd not available on this OS; binary=$(file "$BIN" 2>/dev/null || echo "$BIN")"
fi

echo
echo "=== capture cycle (strace network) ==="
if ! command -v strace >/dev/null 2>&1; then
  if [ "$(uname -s)" = "Linux" ]; then
    echo "FAIL: strace missing on Linux" >&2
    exit 1
  fi
  echo "FAIL: strace not available; cannot prove no-network on this OS" >&2
  exit 1
fi

LOG="$TMP/strace.log"
{
  echo '{"cmd":"consent","id":1,"armNow":true}'
  echo '{"cmd":"arm","id":2}'
  sleep 6
  echo '{"cmd":"stats","id":3}'
  echo '{"cmd":"disarm","id":4}'
  echo '{"cmd":"shutdown","id":5}'
} | strace -e trace=network -o "$LOG" -f "$BIN" daemon || true

if grep -E 'connect\(|sendto\(|recvfrom\(' "$LOG" | grep -v 'UNIX' | grep -v 'AF_UNIX' >/dev/null 2>&1; then
  echo "FAIL: unexpected network syscall"
  cat "$LOG"
  exit 1
fi
echo "ok: no non-UNIX connect/send/recv during a capture cycle"
echo "strace log: no inet sockets"
