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
OUT="$TMP/daemon.out"
ERR="$TMP/daemon.err"
set +e
# REWIND_TEST_CAPTURE forces a deterministic synthetic capture backend and
# treats the session as active, so an armed daemon on this headless runner
# actually writes frames — otherwise the "no-network during capture" proof
# could pass without any capture having happened.
{
  echo '{"cmd":"consent","id":1,"armNow":true}'
  echo '{"cmd":"arm","id":2}'
  sleep 6
  echo '{"cmd":"stats","id":3}'
  echo '{"cmd":"disarm","id":4}'
  echo '{"cmd":"shutdown","id":5}'
} | REWIND_TEST_CAPTURE=1 strace -e trace=network -o "$LOG" -f "$BIN" daemon >"$OUT" 2>"$ERR"
status=$?
set -e

if [ "$status" -ne 0 ]; then
  echo "FAIL: rewindd/strace exited $status before completing the capture cycle" >&2
  cat "$OUT" "$ERR" "$LOG" 2>/dev/null || true
  exit 1
fi
if ! grep -q '"event":"ready"' "$OUT"; then
  echo "FAIL: daemon never emitted ready; capture cycle did not start" >&2
  cat "$OUT" "$ERR" >&2
  exit 1
fi
if ! grep -q '"bye":true' "$OUT"; then
  echo "FAIL: daemon did not complete shutdown; capture cycle did not finish" >&2
  cat "$OUT" "$ERR" >&2
  exit 1
fi
if ! grep -q '"event":"reply"' "$OUT"; then
  echo "FAIL: daemon produced no command replies; capture cycle did not run" >&2
  cat "$OUT" "$ERR" >&2
  exit 1
fi
# The proof is only meaningful if a frame was actually captured and written.
# Require either a frame-written event or a positive frame count in stats.
frames_written=$(grep -c '"event":"frame-written"' "$OUT" 2>/dev/null || echo 0)
frames_stat=$(grep -oE '"frames":[0-9]+' "$OUT" | grep -oE '[0-9]+' | sort -rn | head -1)
frames_stat=${frames_stat:-0}
if [ "$frames_written" -eq 0 ] && [ "$frames_stat" -eq 0 ]; then
  echo "FAIL: no frame was captured during the audit; the no-network proof is vacuous" >&2
  cat "$OUT" "$ERR" >&2
  exit 1
fi
echo "captured frames: events=$frames_written stat=$frames_stat"

if grep -E 'connect\(|sendto\(|recvfrom\(' "$LOG" | grep -v 'UNIX' | grep -v 'AF_UNIX' >/dev/null 2>&1; then
  echo "FAIL: unexpected network syscall"
  cat "$LOG"
  exit 1
fi
echo "ok: no non-UNIX connect/send/recv during a capture cycle"
echo "strace log: no inet sockets"
