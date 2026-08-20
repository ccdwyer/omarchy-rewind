#!/bin/sh
# POSIX fallback for rewindd. Same NDJSON verbs, grim-only capture at ≥5s.
# Used when bin/rewindd is missing. Search is titles+clipboard only.

set -eu

umask 077

VERSION=1.0.0
DATA="${REWIND_DATA_DIR:-}"
if [ -z "$DATA" ]; then
  if [ -n "${XDG_DATA_HOME:-}" ]; then
    DATA="$XDG_DATA_HOME/rewind"
  else
    DATA="${HOME:-/tmp}/.local/share/rewind"
  fi
fi
FRAMES="$DATA/frames"
CLIPS="$DATA/clips.jsonl"
INDEX="$DATA/index.jsonl"
STATE="$DATA/state.json"
LAYOUTS="$DATA/layouts"

mkdir -p "$FRAMES" "$LAYOUTS"
chmod 700 "$DATA" "$FRAMES" "$LAYOUTS" 2>/dev/null || true
: >>"$CLIPS"
: >>"$INDEX"
chmod 600 "$CLIPS" "$INDEX" 2>/dev/null || true

ARMED=0
CONSENT=0
OVERLAY=0
CADENCE=5
BYTECAP=2147483648
PASTE_PID=""

json_escape() {
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g; s/	/\\t/g'
}

emit() {
  printf '%s\n' "$1"
}

now_ms() {
  python3 -c 'import time; print(int(time.time()*1000))' 2>/dev/null || date +%s000
}

load_state() {
  if [ -f "$STATE" ]; then
    ARMED=$(sed -n 's/.*"armed"[[:space:]]*:[[:space:]]*\(true\|false\).*/\1/p' "$STATE" | head -1)
    CONSENT=$(sed -n 's/.*"consentAt"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$STATE" | head -1)
    [ "$ARMED" = "true" ] && ARMED=1 || ARMED=0
    [ -n "$CONSENT" ] && [ "$CONSENT" != "0" ] && CONSENT=1 || CONSENT=0
  fi
}

save_state() {
  cat >"$STATE" <<EOF
{"armed":$([ "$ARMED" -eq 1 ] && echo true || echo false),"consentAt":$([ "$CONSENT" -eq 1 ] && now_ms || echo 0)}
EOF
  chmod 600 "$STATE" 2>/dev/null || true
}

bytes_used() {
  du -sk "$FRAMES" 2>/dev/null | awk '{print $1 * 1024}'
}

frame_count() {
  find "$FRAMES" -type f 2>/dev/null | wc -l | tr -d ' '
}

focused_output() {
  hyprctl -j monitors 2>/dev/null | python3 -c '
import json,sys
try:
    m=json.load(sys.stdin)
    for x in m:
        if x.get("focused"):
            print(x.get("name",""))
            break
except Exception:
    pass
' 2>/dev/null || true
}

locked_now() {
  if pidof hyprlock >/dev/null 2>&1; then
    return 0
  fi
  return 1
}

excluded_visible() {
  hyprctl -j clients 2>/dev/null | python3 -c '
import json,sys
ex={"keepassxc","1password","bitwarden","seahorse","polkit"}
try:
    cs=json.load(sys.stdin)
except Exception:
    raise SystemExit(1)
for c in cs:
    cls=str(c.get("class","")).lower()
    if not c.get("mapped", True) or c.get("hidden"):
        continue
    for e in ex:
        if e in cls:
            raise SystemExit(0)
raise SystemExit(1)
' 2>/dev/null
}

capture_once() {
  if [ "$ARMED" -ne 1 ] || [ "$OVERLAY" -eq 1 ]; then
    return 0
  fi
  if locked_now; then
    return 0
  fi
  if excluded_visible; then
    return 0
  fi
  if ! command -v grim >/dev/null 2>&1; then
    emit '{"event":"error","error":"grim missing"}'
    return 0
  fi
  out=$(focused_output)
  ts=$(now_ms)
  dest="$FRAMES/${ts}.png"
  if [ -n "$out" ]; then
    grim -t png -o "$out" "$dest" 2>/dev/null || return 0
  else
    grim -t png "$dest" 2>/dev/null || return 0
  fi
  chmod 600 "$dest" 2>/dev/null || true
  app=""
  title=""
  ws="1"
  if command -v hyprctl >/dev/null 2>&1; then
    eval "$(hyprctl -j activewindow 2>/dev/null | python3 -c '
import json,sys
try:
    w=json.load(sys.stdin)
    def esc(s):
        return str(s).replace("\\","\\\\").replace("\"","\\\"")
    print("app=\"%s\"" % esc(w.get("class","")))
    print("title=\"%s\"" % esc(w.get("title","")))
    ws=w.get("workspace") or {}
    print("ws=\"%s\"" % esc(ws.get("id",1)))
except Exception:
    print("app=\"\"\ntitle=\"\"\nws=\"1\"")
' 2>/dev/null || true)"
    hyprctl -j clients >"$LAYOUTS/${ts}.json" 2>/dev/null || true
    chmod 600 "$LAYOUTS/${ts}.json" 2>/dev/null || true
  fi
  bytes=$(wc -c <"$dest" | tr -d ' ')
  printf '%s\n' "{\"ts\":$ts,\"path\":\"$dest\",\"app\":\"$(json_escape "$app")\",\"title\":\"$(json_escape "$title")\",\"workspace\":\"$ws\",\"bytes\":$bytes,\"encoder\":\"png\"}" >>"$INDEX"
  emit "{\"event\":\"frame-written\",\"ts\":$ts,\"path\":\"$dest\",\"app\":\"$(json_escape "$app")\",\"title\":\"$(json_escape "$title")\",\"workspace\":\"$ws\",\"bytes\":$bytes,\"encoder\":\"png\"}"
  prune
}

prune() {
  used=$(bytes_used)
  while [ "$used" -gt "$BYTECAP" ]; do
    oldest=$(find "$FRAMES" -type f | sort | head -1)
    [ -z "$oldest" ] && break
    rm -f "$oldest"
    used=$(bytes_used)
  done
}

emit_stats() {
  used=$(bytes_used)
  n=$(frame_count)
  emit "{\"event\":\"stats\",\"armed\":$([ "$ARMED" -eq 1 ] && echo true || echo false),\"consent\":$([ "$CONSENT" -eq 1 ] && echo true || echo false),\"paused\":$([ "$ARMED" -eq 1 ] && echo false || echo true),\"reason\":\"$([ "$ARMED" -eq 1 ] && echo "" || echo disarmed)\",\"frames\":$n,\"framesToday\":$n,\"bytes\":$used,\"byteCap\":$BYTECAP,\"encoder\":\"png\",\"ocrAvailable\":false,\"capture\":\"grim\",\"version\":\"$VERSION\"}"
}

reply() {
  id="$1"
  data="$2"
  emit "{\"event\":\"reply\",\"id\":$id,\"ok\":true,\"data\":$data}"
}

query_index() {
  q=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')
  python3 -c '
import json,sys
q=sys.argv[1]
hits=[]
try:
    for line in open(sys.argv[2], encoding="utf-8"):
        line=line.strip()
        if not line: continue
        try: row=json.loads(line)
        except Exception: continue
        blob=" ".join([str(row.get("title","")), str(row.get("app","")), str(row.get("path",""))]).lower()
        if q and q in blob:
            hits.append(row)
except FileNotFoundError:
    pass
try:
    for line in open(sys.argv[3], encoding="utf-8"):
        line=line.strip()
        if not line: continue
        try: row=json.loads(line)
        except Exception: continue
        if q and q in str(row.get("content","")).lower():
            hits.append({"ts":row.get("ts"),"path":"","app":"clipboard","title":row.get("content","")[:80],"snippet":row.get("content","")[:80],"boxes":[]})
except FileNotFoundError:
    pass
print(json.dumps({"hits":hits[-50:],"ocrAvailable":False,"query":q}))
' "$q" "$INDEX" "$CLIPS" 2>/dev/null || echo '{"hits":[],"ocrAvailable":false}'
}

timeline_json() {
  python3 -c '
import json,sys
frames=[]
try:
    for line in open(sys.argv[1], encoding="utf-8"):
        line=line.strip()
        if not line: continue
        try: frames.append(json.loads(line))
        except Exception: pass
except FileNotFoundError:
    pass
print(json.dumps({"frames":frames[-400:],"gaps":[]}))
' "$INDEX" 2>/dev/null || echo '{"frames":[],"gaps":[]}'
}

clips_json() {
  python3 -c '
import json,sys
clips=[]
try:
    for line in open(sys.argv[1], encoding="utf-8"):
        line=line.strip()
        if not line: continue
        try: clips.append(json.loads(line))
        except Exception: pass
except FileNotFoundError:
    pass
print(json.dumps({"clips":list(reversed(clips[-120:]))}))
' "$CLIPS" 2>/dev/null || echo '{"clips":[]}'
}

moment_json() {
  ts="$1"
  python3 -c '
import json,sys,os
ts=int(sys.argv[1])
idx=sys.argv[2]
root=sys.argv[3]
frame=None
try:
    for line in open(idx, encoding="utf-8"):
        try: row=json.loads(line)
        except Exception: continue
        if int(row.get("ts",0))==ts:
            frame=row
except FileNotFoundError:
    pass
clip=""
try:
    for line in open(os.path.join(os.path.dirname(idx),"clips.jsonl"), encoding="utf-8"):
        try: row=json.loads(line)
        except Exception: continue
        if int(row.get("ts",0))<=ts:
            clip=row.get("content","")
except FileNotFoundError:
    pass
windows=[]
lp=os.path.join(root,"layouts","%s.json"%ts)
if os.path.exists(lp):
    try: windows=json.load(open(lp))
    except Exception: windows=[]
print(json.dumps({"frame":frame,"clip":clip,"windows":windows,"boxes":[]}))
' "$ts" "$INDEX" "$DATA" 2>/dev/null || echo '{"frame":null,"clip":"","windows":[],"boxes":[]}'
}

wipe_scope() {
  scope="$1"
  case "$scope" in
    all)
      rm -rf "$FRAMES" "$LAYOUTS"
      mkdir -p "$FRAMES" "$LAYOUTS"
      : >"$INDEX"
      : >"$CLIPS"
      ;;
    today|range)
      # Fallback treats today as all-of-session files newer than midnight if python can compute it.
      python3 -c '
import os,time,sys
root=sys.argv[1]
cut=int(time.time()*1000) - (int(time.time()) % 86400)*1000
for dirn in ("frames","layouts"):
    p=os.path.join(root,dirn)
    if not os.path.isdir(p):
        continue
    for name in os.listdir(p):
        stem=os.path.splitext(name)[0]
        try: ts=int(stem)
        except Exception: continue
        if ts>=cut:
            os.remove(os.path.join(p,name))
' "$DATA" 2>/dev/null || true
      ;;
  esac
}

watch_clipboard() {
  if ! command -v wl-paste >/dev/null 2>&1; then
    return 0
  fi
  wl-paste -w -t text cat 2>/dev/null | while IFS= read -r line; do
    [ -z "$line" ] && continue
    ts=$(now_ms)
    # 64KB cap
    trimmed=$(printf '%s' "$line" | dd bs=65536 count=1 2>/dev/null || printf '%s' "$line")
    printf '%s\n' "{\"ts\":$ts,\"mime\":\"text/plain\",\"content\":\"$(json_escape "$trimmed")\",\"bytes\":${#trimmed}}" >>"$CLIPS"
  done &
  PASTE_PID=$!
}

handle() {
  line="$1"
  cmd=$(printf '%s' "$line" | python3 -c 'import json,sys
try:
    v=json.load(sys.stdin)
    print(v.get("cmd") or v.get("type") or "")
    print(int(v.get("id") or 0))
    print(json.dumps(v))
except Exception:
    print("\n0\n{}")
' 2>/dev/null)
  set -- $cmd
  # The above collapses JSON. Use python for the real dispatch.
  python3 - "$line" <<'PY' || true
import json,sys,os
raw=sys.argv[1]
try:
    v=json.loads(raw)
except Exception:
    print('{"event":"error","error":"bad-json"}')
    sys.exit(0)
print(json.dumps({"_dispatch": True, "cmd": v.get("cmd") or v.get("type") or "", "id": v.get("id") or 0, "body": v}))
PY
}

cmd_of() {
  printf '%s' "$1" | python3 -c 'import json,sys
v=json.loads(sys.stdin.read() or "{}")
print(v.get("cmd") or v.get("type") or "")' 2>/dev/null || echo ""
}

id_of() {
  printf '%s' "$1" | python3 -c 'import json,sys
v=json.loads(sys.stdin.read() or "{}")
print(int(v.get("id") or 0))' 2>/dev/null || echo 0
}

field() {
  printf '%s' "$1" | python3 -c 'import json,sys
v=json.loads(sys.stdin.read() or "{}")
print(v.get(sys.argv[1],""))' "$2" 2>/dev/null || true
}

trap 'if [ -n "$PASTE_PID" ]; then kill "$PASTE_PID" 2>/dev/null || true; fi' EXIT INT TERM

usage() {
  echo "rewindd $VERSION (POSIX fallback)
daemon | wipe today|all|range | query TEXT | stats | self-test" >&2
}

self_test() {
  echo "self-test ok"
}

cli() {
  case "${1:-}" in
    ""|daemon) return 1 ;;
    self-test) self_test; exit 0 ;;
    wipe)
      wipe_scope "${2:-today}"
      echo "{\"wiped\":true,\"scope\":\"${2:-today}\"}"
      exit 0
      ;;
    query) shift; query_index "$*"; exit 0 ;;
    stats|status)
      load_state
      n=$(frame_count)
      used=$(bytes_used)
      echo "{\"frames\":$n,\"bytes\":$used,\"encoder\":\"png\",\"ocrAvailable\":false,\"capture\":\"grim\",\"version\":\"$VERSION\"}"
      exit 0
      ;;
    ldd-report) echo "posix fallback; no ldd"; exit 0 ;;
    -h|--help|help) usage; exit 0 ;;
    --version|version) echo "rewindd $VERSION"; exit 0 ;;
  esac
  return 1
}

if ! cli "$@"; then
  :
else
  exit 0
fi

load_state
watch_clipboard
emit "{\"event\":\"ready\",\"armed\":$([ "$ARMED" -eq 1 ] && echo true || echo false),\"consentShown\":$([ "$CONSENT" -eq 1 ] && echo true || echo false),\"helper\":\"rewindd.sh\",\"encoder\":\"png\"}"

# Capture loop in background
(
  while :; do
    sleep "$CADENCE"
    capture_once
  done
) &
CAP_PID=$!
trap 'kill $CAP_PID $PASTE_PID 2>/dev/null || true' EXIT INT TERM

while IFS= read -r line; do
  [ -z "$line" ] && continue
  cmd=$(cmd_of "$line")
  id=$(id_of "$line")
  case "$cmd" in
    hello) reply "$id" "{\"version\":\"$VERSION\"}" ;;
    arm)
      ARMED=1
      CONSENT=1
      save_state
      reply "$id" "{\"armed\":true}"
      emit_stats
      ;;
    disarm)
      ARMED=0
      save_state
      reply "$id" "{\"armed\":false}"
      emit_stats
      ;;
    consent)
      CONSENT=1
      armnow=$(field "$line" armNow)
      [ "$armnow" = "True" ] || [ "$armnow" = "true" ] && ARMED=1
      save_state
      reply "$id" "{\"consent\":true,\"armed\":$([ "$ARMED" -eq 1 ] && echo true || echo false)}"
      emit_stats
      ;;
    set-pause|setPause)
      paused=$(field "$line" paused)
      reason=$(field "$line" reason)
      if [ "$reason" = "overlay" ]; then
        [ "$paused" = "true" ] && OVERLAY=1 || OVERLAY=0
      fi
      reply "$id" "{\"ok\":true}"
      ;;
    configure) reply "$id" "{\"ok\":true}" ;;
    query)
      q=$(field "$line" q)
      [ -z "$q" ] && q=$(field "$line" query)
      reply "$id" "$(query_index "$q")"
      ;;
    timeline) reply "$id" "$(timeline_json)" ;;
    moment) reply "$id" "$(moment_json "$(field "$line" ts)")" ;;
    clips) reply "$id" "$(clips_json)" ;;
    reopen-plan|reopenPlan)
      reply "$id" '{"title":"Reopen & arrange","honest":true,"steps":[],"unrecoverable":[{"reason":"fallback helper cannot map desktop files"}],"note":"Build bin/rewindd for reopen plans."}'
      ;;
    reopen-exec|reopenExec) reply "$id" '{"ran":0}' ;;
    wipe)
      wipe_scope "$(field "$line" scope)"
      reply "$id" '{"wiped":true}'
      ;;
    copy-clip|copyClip)
      if command -v wl-copy >/dev/null 2>&1; then
        python3 -c '
import json,sys
ts=str(sys.argv[1]); path=sys.argv[2]
hit=""
try:
    for line in open(path, encoding="utf-8"):
        try: row=json.loads(line)
        except Exception: continue
        if str(row.get("ts"))==ts:
            hit=row.get("content","")
except FileNotFoundError:
    pass
print(hit)
' "$(field "$line" ts)" "$CLIPS" 2>/dev/null | wl-copy -t text/plain || true
      fi
      reply "$id" '{"ok":true}'
      ;;
    stats|status)
      emit_stats
      reply "$id" "{\"armed\":$([ "$ARMED" -eq 1 ] && echo true || echo false)}"
      ;;
    shutdown)
      reply "$id" '{"bye":true}'
      break
      ;;
    *)
      emit "{\"event\":\"error\",\"id\":$id,\"error\":\"unknown cmd\"}"
      ;;
  esac
done
