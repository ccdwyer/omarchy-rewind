#!/bin/sh
# 3-minute seed for the 60-second Rewind demo. Run after the plugin is
# armed. Opens two terminals, types a distinctive phrase, copies a hash.

set -eu

PHRASE="${REWIND_PHRASE:-zebra-token-rewind-demo}"
HASH="${REWIND_HASH:-$(git rev-parse HEAD 2>/dev/null || echo git-hash-ff00aa)}"

# These values are interpolated into nested `sh -c` commands below, so restrict
# them to a safe charset to prevent shell injection from a hostile env var.
# Strip anything outside [A-Za-z0-9._-]; fall back to the defaults if emptied.
sanitize() { printf '%s' "$1" | tr -cd 'A-Za-z0-9._-'; }
PHRASE="$(sanitize "$PHRASE")"
HASH="$(sanitize "$HASH")"
[ -n "$PHRASE" ] || PHRASE="zebra-token-rewind-demo"
[ -n "$HASH" ] || HASH="git-hash-ff00aa"

say() { printf '%s\n' "$*"; }

say "Rewind demo seed. Distinctive phrase: $PHRASE"
say "Clipboard hash: $HASH"

if command -v hyprctl >/dev/null 2>&1; then
  # Sanitize the cwd before interpolating it into the nested single-quoted
  # sh -c payload — an unsanitized path containing a quote or shell metachar
  # would break the quoting (or inject). Keep only safe path characters.
  SAFE_PWD=$(pwd | tr -cd 'A-Za-z0-9/._ -')
  hyprctl dispatch exec "kitty --hold sh -c 'echo $PHRASE; echo typed in $SAFE_PWD; exec zsh'" || true
  sleep 1
  hyprctl dispatch exec "kitty --hold sh -c 'echo second window; exec zsh'" || true
else
  say "hyprctl missing — type $PHRASE yourself and copy $HASH"
fi

if command -v wl-copy >/dev/null 2>&1; then
  printf '%s' "$HASH" | wl-copy -t text/plain
  say "Copied hash to clipboard."
fi

say "Leave Rewind armed for ~20s, then:"
say "  omarchy-shell shell summon io.github.chris.rewind '{}'"
say "Search for: $PHRASE"
say "Long-press the bar chip for clipboard history, Enter to re-copy $HASH"
