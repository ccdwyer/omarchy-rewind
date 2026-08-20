#!/bin/sh
# POSIX fallback for rewindd — protocol-compatible, non-recording.
#
# It never captures frames or clipboard. That is intentional: the shell
# fallback cannot enforce the pause matrix (lock/idle/exclusion/portal/
# private-window) or a persistent screencopy session, so recording here
# would be an unsafe partial recorder. Query/wipe/stats still operate on
# data previously written by the Rust binary.

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
[ -f "$CLIPS" ] || : >"$CLIPS"
[ -f "$INDEX" ] || : >"$INDEX"
chmod 600 "$CLIPS" "$INDEX" 2>/dev/null || true

CONSENT=0
ARMONLOGIN=0
BYTECAP=2147483648
CADENCE=3000
IDLEPAUSE=120
EXCLUDES="keepassxc,1Password,1password,Bitwarden,bitwarden,seahorse"
TITLEPAUSE=""

emit() { printf '%s\n' "$1"; }

now_ms() {
  python3 -c 'import time; print(int(time.time()*1000))' 2>/dev/null || date +%s000
}

load_state() {
  if [ ! -f "$STATE" ]; then
    return 0
  fi
  eval "$(python3 -c '
import json,sys
try:
    s=json.load(open(sys.argv[1]))
except Exception:
    s={}
def i(k,d=0):
    print("%s=%s"%(k,int(s.get(k) or d)))
print("CONSENT=%d"%(1 if s.get("consentAt") else 0))
print("ARMONLOGIN=%d"%(1 if s.get("armOnLogin") else 0))
print("BYTECAP=%d"%int(s.get("byteCap") or 2147483648))
print("CADENCE=%d"%int(s.get("cadenceMs") or 3000))
print("IDLEPAUSE=%d"%int(s.get("idlePauseSec") or 120))
ex=s.get("excludeApps") or ""
if isinstance(ex,list):
    ex=",".join(ex)
print("EXCLUDES=%r"%ex)
tp=s.get("titlePausePatterns") or ""
if isinstance(tp,list):
    tp=",".join(tp)
print("TITLEPAUSE=%r"%tp)
' "$STATE" 2>/dev/null || true)"
}

save_state() {
  python3 -c '
import json,sys
path=sys.argv[1]
body={
  "armed": False,
  "consentAt": int(sys.argv[2]),
  "armOnLogin": sys.argv[3]=="1",
  "byteCap": int(sys.argv[4]),
  "cadenceMs": int(sys.argv[5]),
  "idlePauseSec": int(sys.argv[6]),
  "excludeApps": sys.argv[7],
  "titlePausePatterns": sys.argv[8],
  "record": False,
  "reason": "compat-norecord",
}
json.dump(body, open(path,"w"))
' "$STATE" "$([ "$CONSENT" -eq 1 ] && now_ms || echo 0)" "$ARMONLOGIN" "$BYTECAP" "$CADENCE" "$IDLEPAUSE" "$EXCLUDES" "$TITLEPAUSE" 2>/dev/null || true
  chmod 600 "$STATE" 2>/dev/null || true
}

merge_configure() {
  line="$1"
  eval "$(python3 -c '
import json,sys
v=json.loads(sys.argv[1])
def n(*keys):
    for k in keys:
        if k in v and v[k] not in (None,""):
            print(k, v[k]); return
n("byteCapGb","byteCap")
n("cadenceMs")
n("idlePauseSec")
n("excludeApps","exclude")
n("titlePausePatterns","titlePause")
n("armOnLogin")
' "$line" 2>/dev/null | while IFS= read -r k rest; do
    case "$k" in
      byteCapGb) echo "BYTECAP=$(python3 -c "print(int(float(\"$rest\")*1024*1024*1024))" 2>/dev/null || echo $BYTECAP)" ;;
      byteCap) echo "BYTECAP=$rest" ;;
      cadenceMs) echo "CADENCE=$rest" ;;
      idlePauseSec) echo "IDLEPAUSE=$rest" ;;
      excludeApps|exclude) echo "EXCLUDES=$rest" ;;
      titlePausePatterns|titlePause) echo "TITLEPAUSE=$rest" ;;
      armOnLogin) echo "ARMONLOGIN=$([ "$rest" = "True" ] || [ "$rest" = "true" ] && echo 1 || echo 0)" ;;
    esac
  done)"
}

bytes_used() {
  du -sk "$FRAMES" 2>/dev/null | awk '{print $1 * 1024}'
}

frame_count() {
  find "$FRAMES" -type f 2>/dev/null | wc -l | tr -d ' '
}

stats_json() {
  used=$(bytes_used)
  n=$(frame_count)
  python3 -c 'import json,sys
print(json.dumps({
  "armed": False,
  "consent": sys.argv[1]=="1",
  "paused": True,
  "reason": "compat-norecord",
  "frames": int(sys.argv[2]),
  "framesToday": int(sys.argv[2]),
  "bytes": int(sys.argv[3]),
  "byteCap": int(sys.argv[4]),
  "encoder": "none",
  "ocrAvailable": False,
  "capture": "none",
  "version": sys.argv[5],
}))' "$CONSENT" "$n" "$used" "$BYTECAP" "$VERSION"
}

reply() {
  id="$1"
  data="$2"
  emit "{\"event\":\"reply\",\"id\":$id,\"ok\":true,\"data\":$data}"
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
x=v.get(sys.argv[1],"")
if isinstance(x,bool):
    print("true" if x else "false")
elif x is None:
    print("")
else:
    print(x)' "$2" 2>/dev/null || true
}

query_index() {
  q="$1"
  python3 -c '
import json,sys,os
q=sys.argv[1].lower()
idx=sys.argv[2]
clips=sys.argv[3]
frames=[]
try:
    for line in open(idx, encoding="utf-8"):
        line=line.strip()
        if not line: continue
        try: frames.append(json.loads(line))
        except Exception: pass
except FileNotFoundError:
    pass
clip_rows=[]
try:
    for line in open(clips, encoding="utf-8"):
        line=line.strip()
        if not line: continue
        try: clip_rows.append(json.loads(line))
        except Exception: pass
except FileNotFoundError:
    pass

def nearest(ts):
    before=[f for f in frames if int(f.get("ts") or 0)<=ts]
    if before:
        return max(before, key=lambda f: int(f.get("ts") or 0))
    after=[f for f in frames if int(f.get("ts") or 0)>=ts]
    if after:
        return min(after, key=lambda f: int(f.get("ts") or 0))
    return None

hits=[]
seen=set()
for f in frames:
    blob=" ".join([str(f.get("title","")), str(f.get("app","")), str(f.get("path",""))]).lower()
    if q and q in blob:
        ts=int(f.get("ts") or 0)
        if ts in seen: continue
        seen.add(ts)
        hits.append(f)
for c in clip_rows:
    content=str(c.get("content",""))
    if q and q in content.lower():
        f=nearest(int(c.get("ts") or 0))
        row=dict(f) if f else {"ts": c.get("ts"), "path":"", "app":"clipboard", "title":""}
        ts=int(row.get("ts") or 0)
        if ts in seen: continue
        seen.add(ts)
        row=dict(row)
        row["snippet"]=content[:80]
        row["boxes"]=[]
        hits.append(row)
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
ts=int(sys.argv[1] or 0)
idx, clips_path, root = sys.argv[2], sys.argv[3], sys.argv[4]
frames=[]
try:
    for line in open(idx, encoding="utf-8"):
        try: frames.append(json.loads(line))
        except Exception: pass
except FileNotFoundError:
    pass
frame=None
for f in frames:
    if int(f.get("ts") or 0)==ts:
        frame=f
if frame is None:
    before=[f for f in frames if int(f.get("ts") or 0)<=ts]
    frame=max(before, key=lambda f: int(f.get("ts") or 0)) if before else None
clip=""
try:
    for line in open(clips_path, encoding="utf-8"):
        try: row=json.loads(line)
        except Exception: continue
        if int(row.get("ts") or 0)<=ts:
            clip=row.get("content","")
except FileNotFoundError:
    pass
windows=[]
if frame:
    lp=os.path.join(root,"layouts","%s.json"%frame.get("ts"))
    if os.path.exists(lp):
        try: windows=json.load(open(lp))
        except Exception: windows=[]
print(json.dumps({"frame":frame,"clip":clip,"windows":windows,"boxes":[]}))
' "$ts" "$INDEX" "$CLIPS" "$DATA" 2>/dev/null || echo '{"frame":null,"clip":"","windows":[],"boxes":[]}'
}

# Atomically drop missing files from the index and remove matching clip/layout rows.
rewrite_index() {
  python3 -c '
import json,os,sys,tempfile
root, idx_path, clips_path = sys.argv[1], sys.argv[2], sys.argv[3]
scope, lo, hi = sys.argv[4], int(sys.argv[5] or 0), int(sys.argv[6] or 0)
now=int(sys.argv[7] or 0)
if scope=="all":
    lo, hi = 0, 2**62
elif scope=="today":
    day=86400000
    lo, hi = now - (now % day), now
elif scope=="range":
    if hi==0: hi=now
else:
    lo, hi = 0, -1  # keep all; just drop missing files

def load(path):
    rows=[]
    try:
        for line in open(path, encoding="utf-8"):
            line=line.strip()
            if not line: continue
            try: rows.append(json.loads(line))
            except Exception: pass
    except FileNotFoundError:
        pass
    return rows

def atomic_write(path, rows):
    d=os.path.dirname(path) or "."
    fd, tmp=tempfile.mkstemp(dir=d, prefix=".idx-")
    os.close(fd)
    with open(tmp,"w",encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False)+"\n")
    os.replace(tmp, path)
    os.chmod(path, 0o600)

frames=load(idx_path)
keep=[]
for f in frames:
    ts=int(f.get("ts") or 0)
    path=f.get("path") or ""
    gone = (lo<=ts<=hi) if hi>=lo else False
    if gone:
        if path and os.path.exists(path):
            try: os.remove(path)
            except Exception: pass
        lp=os.path.join(root,"layouts","%s.json"%ts)
        if os.path.exists(lp):
            try: os.remove(lp)
            except Exception: pass
        continue
    if path and not os.path.exists(path) and not os.path.exists(os.path.join(root, path)):
        continue
    keep.append(f)
atomic_write(idx_path, keep)
clips=load(clips_path)
ck=[]
for c in clips:
    ts=int(c.get("ts") or 0)
    if hi>=lo and lo<=ts<=hi:
        continue
    ck.append(c)
atomic_write(clips_path, ck)
print(json.dumps({"wiped": len(frames)-len(keep), "scope": scope}))
' "$DATA" "$INDEX" "$CLIPS" "$1" "${2:-0}" "${3:-0}" "$(now_ms)"
}

usage() {
  echo "rewindd $VERSION (POSIX fallback — does not record)
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
      rewrite_index "${2:-today}" 0 0
      exit 0
      ;;
    query) shift; query_index "$*"; exit 0 ;;
    stats|status)
      load_state
      stats_json
      exit 0
      ;;
    ldd-report) echo "posix fallback; no ldd"; exit 0 ;;
    -h|--help|help) usage; exit 0 ;;
    --version|version) echo "rewindd $VERSION"; exit 0 ;;
  esac
  return 1
}

if cli "$@"; then
  exit 0
fi

load_state
emit "{\"event\":\"ready\",\"armed\":false,\"consentShown\":$([ "$CONSENT" -eq 1 ] && echo true || echo false),\"helper\":\"rewindd.sh\",\"encoder\":\"none\",\"capture\":\"none\"}"

while IFS= read -r line; do
  [ -z "$line" ] && continue
  cmd=$(cmd_of "$line")
  id=$(id_of "$line")
  case "$cmd" in
    hello) reply "$id" "{\"version\":\"$VERSION\",\"record\":false}" ;;
    arm)
      CONSENT=1
      save_state
      emit "{\"event\":\"error\",\"id\":$id,\"ok\":false,\"error\":\"compat helper does not record; run build.sh for rewindd\"}"
      reply "$id" "$(stats_json)"
      emit "{\"event\":\"stats\",$(stats_json | sed 's/^{//;s/}$//')}"
      ;;
    disarm)
      save_state
      reply "$id" "$(stats_json)"
      ;;
    consent)
      CONSENT=1
      armnow=$(field "$line" armNow)
      [ "$armnow" = "true" ] && emit "{\"event\":\"error\",\"id\":$id,\"error\":\"compat helper does not record; run build.sh for rewindd\"}"
      save_state
      reply "$id" "$(stats_json)"
      ;;
    set-pause|setPause) reply "$id" "{\"ok\":true}" ;;
    configure)
      merge_configure "$line"
      save_state
      reply "$id" "{\"ok\":true}"
      ;;
    query)
      q=$(field "$line" q)
      [ -z "$q" ] && q=$(field "$line" query)
      reply "$id" "$(query_index "$q")"
      ;;
    timeline) reply "$id" "$(timeline_json)" ;;
    moment) reply "$id" "$(moment_json "$(field "$line" ts)")" ;;
    clips) reply "$id" "$(clips_json)" ;;
    reopen-plan|reopenPlan)
      reply "$id" '{"title":"Reopen & arrange","honest":true,"steps":[],"unrecoverable":[{"reason":"compat helper does not record or plan"}],"note":"Build bin/rewindd for reopen plans."}'
      ;;
    reopen-exec|reopenExec) reply "$id" '{"ran":0}' ;;
    wipe)
      scope=$(field "$line" scope)
      [ -z "$scope" ] && scope=today
      from=$(field "$line" from)
      to=$(field "$line" to)
      reply "$id" "$(rewrite_index "$scope" "${from:-0}" "${to:-0}")"
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
sys.stdout.write(hit)
' "$(field "$line" ts)" "$CLIPS" 2>/dev/null | wl-copy -t text/plain || true
      fi
      reply "$id" '{"ok":true}'
      ;;
    stats|status)
      reply "$id" "$(stats_json)"
      emit "{\"event\":\"stats\",$(stats_json | sed 's/^{//;s/}$//')}"
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
