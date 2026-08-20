#!/bin/sh
# fetch-rewindd.sh — download a prebuilt Linux `rewindd` recorder for this
# machine's architecture from the plugin's GitHub Releases and install it to
# bin/rewindd, verifying its SHA-256 first.
#
# This is tier 3 of the recorder bootstrap (Service.qml): used on a clean judge
# machine that has neither a bundled bin/rewindd nor `cargo` to build one. The
# release binaries + checksums are produced by .github/workflows/release.yml.
#
# Trust model: the binary is verified against `checksums/rewindd-<arch>.sha256`
# committed IN THIS CLONE when present (authenticity — a tampered release cannot
# match a checksum pinned at the ref you installed). If no committed checksum is
# present, it verifies against the checksum published alongside the binary in
# the same release (integrity — protects against a corrupted download). The
# committed-checksum path is preferred and documented in README.
#
# Exit codes: 0 installed; 2 no network tool; 3 download failed; 4 checksum
# mismatch; 5 unsupported arch.  Never installs an unverified binary.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
OUT="$ROOT/bin"
mkdir -p "$OUT"

# --- architecture ---------------------------------------------------------
case "$(uname -m)" in
  x86_64|amd64)        ARCH=x86_64 ;;
  aarch64|arm64)       ARCH=aarch64 ;;
  *) echo "fetch-rewindd: unsupported arch $(uname -m)" >&2; exit 5 ;;
esac
ASSET="rewindd-$ARCH"

TMP=$(mktemp -d "${TMPDIR:-/tmp}/rewindd-fetch.XXXXXX")
trap 'rm -rf "$TMP"' EXIT
BINTMP="$TMP/$ASSET"

# --- obtain the binary ----------------------------------------------------
# Test hook: REWIND_FETCH_LOCAL points at a local file to use as the
# "downloaded" binary, so the verify+install path is exercisable offline.
if [ -n "${REWIND_FETCH_LOCAL:-}" ]; then
  cp "$REWIND_FETCH_LOCAL" "$BINTMP"
  PUBLISHED_SHA="${REWIND_FETCH_LOCAL_SHA:-}"
else
  # Derive the GitHub repo from this clone's origin remote.
  REMOTE=$(git -C "$ROOT" remote get-url origin 2>/dev/null || true)
  [ -n "$REMOTE" ] || { echo "fetch-rewindd: no git origin to locate releases" >&2; exit 3; }
  # git@github.com:owner/repo(.git) | https://github.com/owner/repo(.git) -> https://github.com/owner/repo
  REPO=$(printf '%s' "$REMOTE" \
    | sed -e 's#^git@github.com:#https://github.com/#' \
          -e 's#\.git$##')
  BASE="$REPO/releases/latest/download"

  fetch() { # url dest
    if command -v curl >/dev/null 2>&1; then
      curl -fsSL --retry 2 -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
      wget -q -O "$2" "$1"
    else
      echo "fetch-rewindd: neither curl nor wget available" >&2; exit 2
    fi
  }
  fetch "$BASE/$ASSET" "$BINTMP" || { echo "fetch-rewindd: download failed" >&2; exit 3; }
  # Published checksum from the same release (integrity fallback).
  if fetch "$BASE/$ASSET.sha256" "$TMP/$ASSET.sha256" 2>/dev/null; then
    PUBLISHED_SHA=$(awk '{print $1; exit}' "$TMP/$ASSET.sha256")
  else
    PUBLISHED_SHA=""
  fi
fi

# --- checksum -------------------------------------------------------------
sha_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'
  else echo ""; fi
}
ACTUAL=$(sha_of "$BINTMP")
[ -n "$ACTUAL" ] || { echo "fetch-rewindd: no sha256 tool to verify" >&2; exit 4; }

# Prefer a checksum committed in this clone (authenticity); else the published
# one (integrity). Refuse to install if we have no expected checksum at all.
COMMITTED="$ROOT/checksums/$ASSET.sha256"
if [ -f "$COMMITTED" ]; then
  EXPECT=$(awk '{print $1; exit}' "$COMMITTED")
  SRC="committed checksum"
elif [ -n "$PUBLISHED_SHA" ]; then
  EXPECT="$PUBLISHED_SHA"
  SRC="release-published checksum"
else
  echo "fetch-rewindd: no expected checksum (committed or published) — refusing" >&2
  exit 4
fi
if [ "$ACTUAL" != "$EXPECT" ]; then
  echo "fetch-rewindd: checksum mismatch vs $SRC (got $ACTUAL, want $EXPECT)" >&2
  exit 4
fi

# --- install (atomic) -----------------------------------------------------
chmod +x "$BINTMP"
mv -f "$BINTMP" "$OUT/rewindd"
cp -f "$OUT/rewindd" "$OUT/rewind" 2>/dev/null || true
chmod +x "$OUT/rewind" 2>/dev/null || true
echo "fetch-rewindd: installed $OUT/rewindd ($ASSET, verified against $SRC)"
