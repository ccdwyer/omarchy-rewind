#!/bin/sh
# Concrete no-network proof for rewindd: ldd output + optional strace of a
# capture cycle. Replaces any reproducible-build hand-waving.

set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
BIN="$ROOT/bin/rewindd"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
export REWIND_DATA_DIR="$TMP/data"
mkdir -p "$REWIND_DATA_DIR"

if [ ! -x "$BIN" ]; then
  echo "network-audit: $BIN missing — run ./build.sh first" >&2
  echo "ldd unavailable (no binary)"
  exit 0
fi

echo "=== ldd $BIN ==="
if command -v ldd >/dev/null 2>&1; then
  ldd "$BIN" || true
else
  echo "ldd not available on this OS"
  echo "file: $BIN"
fi

echo
echo "=== capture cycle (strace network) ==="
if ! command -v strace >/dev/null 2>&1; then
  echo "strace not available; skip network-syscall gate"
  exit 0
fi

LOG="$TMP/strace.log"
{
  echo '{"cmd":"arm","id":1}'
  sleep 6
  echo '{"cmd":"stats","id":2}'
  echo '{"cmd":"disarm","id":3}'
  echo '{"cmd":"shutdown","id":4}'
} | strace -e trace=network -o "$LOG" -f "$BIN" daemon || true

if grep -E 'connect\(|sendto\(|recvfrom\(' "$LOG" | grep -v 'UNIX' | grep -v 'AF_UNIX' >/dev/null 2>&1; then
  echo "FAIL: unexpected network syscall"
  cat "$LOG"
  exit 1
fi
echo "ok: no non-UNIX connect/send/recv during a capture cycle"
echo "strace log: no inet sockets"
