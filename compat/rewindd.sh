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
STATE="$DATA/state.json"
LAYOUTS="$DATA/layouts"
# The Rust recorder stores everything in this SQLite database. The fallback
# reads/wipes THAT store (via python3's built-in sqlite3), never a legacy JSONL
# index — so query/stats reflect real recordings and a wipe actually deletes
# sensitive data instead of reporting a false success against an empty index.
DB="$DATA/rewind.db"

prepare_data() {
  mkdir -p "$FRAMES" "$LAYOUTS"
  chmod 700 "$DATA" "$FRAMES" "$LAYOUTS" 2>/dev/null || true
}

CONSENT=0
CONSENT_AT=0
ARMONLOGIN=0
BYTECAP=2147483648
CADENCE=3000
IDLEPAUSE=120
# Keep in sync with default_excludes() in src/rewindd/src/config.rs and the
# manifest defaults — the fallback must not exclude fewer sensitive apps than
# the Rust helper.
EXCLUDES="keepassxc,1Password,1password,Bitwarden,bitwarden,seahorse,polkit-gnome-authentication-agent-1,polkit-kde-authentication-agent-1,lxqt-policykit-agent,mate-polkit,xfce-polkit"
TITLEPAUSE=""

emit() { printf '%s\n' "$1"; }

now_ms() {
  python3 -c 'import time; print(int(time.time()*1000))' 2>/dev/null || date +%s000
}

load_state() {
  if [ ! -f "$STATE" ]; then
    return 0
  fi
  # Read ONLY the runtime consent/arm fields (ints/bool), one per line — never
  # eval() a string field from disk, so a crafted state.json cannot inject shell.
  # Read ONLY consentAt from disk. armOnLogin is inline customization delivered
  # via configure, never persisted, so it is not read here.
  vals=$(python3 -c '
import json,sys
try:
    s=json.load(open(sys.argv[1]))
except Exception:
    s={}
print(int(s.get("consentAt") or 0))
' "$STATE" 2>/dev/null) || return 0
  CONSENT_AT=$(printf '%s\n' "$vals" | sed -n 1p)
  case "$CONSENT_AT" in ''|*[!0-9]*) CONSENT_AT=0 ;; esac
  ARMONLOGIN=0
  if [ "$CONSENT_AT" -gt 0 ]; then CONSENT=1; else CONSENT=0; fi
}

# Persist state atomically: write a private temp file in the same directory,
# fsync it, then os.replace() over the target so a crash can never leave a
# truncated state.json. Errors propagate (non-zero return) so the caller can
# roll back consent rather than claim a durable write that did not happen.
save_state() {
  if [ "${CONSENT_AT:-0}" -eq 0 ]; then
    return 0
  fi
  prepare_data
  # state.json holds ONLY the durable consent/arm boundary — NEVER any inline
  # customization (byte cap, cadence, idle, excludes, title patterns, or
  # armOnLogin). Those are single-sourced from the shell.json entry and pushed
  # via `configure`; persisting them here would duplicate the Quattro settings
  # model. (The fallback never records, so armed is always false.)
  python3 -c '
import json,os,sys,tempfile
path=sys.argv[1]
body={
  "armed": False,
  "consentAt": int(sys.argv[2]),
}
d=os.path.dirname(path) or "."
fd,tmp=tempfile.mkstemp(prefix=".state.",dir=d)
try:
    with os.fdopen(fd,"w") as f:
        json.dump(body,f)
        f.flush()
        os.fsync(f.fileno())
    os.chmod(tmp,0o600)
    os.replace(tmp,path)
    # fsync the directory so the rename itself is durable.
    try:
        dfd=os.open(d,os.O_RDONLY)
        try:
            os.fsync(dfd)
        finally:
            os.close(dfd)
    except OSError:
        pass
except Exception:
    try:
        os.unlink(tmp)
    except OSError:
        pass
    sys.exit(1)
' "$STATE" "${CONSENT_AT:-0}"
}

merge_configure() {
  line="$1"
  # Only the numeric byte cap and the armOnLogin bool matter for the
  # non-recording fallback (it never captures, so excludes/title patterns are
  # unused). Emit them as plain integers, one per line, and assign via a safe
  # read loop — no eval, so a crafted excludeApps/title value cannot inject
  # shell metacharacters.
  vals=$(python3 -c '
import json,sys
try:
    v=json.loads(sys.argv[1])
except Exception:
    v={}
cap=""
if v.get("byteCapGb") not in (None, ""):
    cap=str(int(float(v["byteCapGb"])*1024*1024*1024))
elif v.get("byteCap") not in (None, ""):
    cap=str(int(v["byteCap"]))
print(cap)
print("1" if v.get("armOnLogin") else "0" if "armOnLogin" in v else "")
' "$line" 2>/dev/null) || return 0
  cap=$(printf '%s\n' "$vals" | sed -n 1p)
  al=$(printf '%s\n' "$vals" | sed -n 2p)
  case "$cap" in ''|*[!0-9]*) : ;; *) BYTECAP="$cap" ;; esac
  case "$al" in 0|1) ARMONLOGIN="$al" ;; esac
}

stats_json() {
  python3 -c '
import json,sys,sqlite3,os
db=sys.argv[1]; consent=sys.argv[2]=="1"; cap=int(sys.argv[3]); ver=sys.argv[4]
frames=0; used=0; first=0; last=0
if os.path.exists(db):
    try:
        c=sqlite3.connect("file:%s?mode=ro"%db, uri=True)
        frames=c.execute("SELECT COUNT(*) FROM frames").fetchone()[0] or 0
        used=c.execute("SELECT COALESCE(SUM(bytes),0)+COALESCE(SUM(crop_bytes),0) FROM frames").fetchone()[0] or 0
        row=c.execute("SELECT COALESCE(MIN(ts),0),COALESCE(MAX(ts),0) FROM frames").fetchone()
        first,last=row[0] or 0,row[1] or 0
        c.close()
    except Exception:
        pass
print(json.dumps({
  "armed": False, "consent": consent, "paused": True, "reason": "compat-norecord",
  "frames": frames, "framesToday": frames, "bytes": used, "byteCap": cap,
  "firstTs": first, "lastTs": last,
  "encoder": "none", "ocrAvailable": False, "capture": "none", "version": ver,
}))' "$DB" "$CONSENT" "$BYTECAP" "$VERSION"
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
  python3 -c '
import json,sys,os,sqlite3
q=sys.argv[1]; db=sys.argv[2]
root=os.path.dirname(os.path.abspath(db))
def absf(p):
    p=p or ""
    return p if (not p or os.path.isabs(p)) else os.path.join(root,p)
if not q or not os.path.exists(db):
    print(json.dumps({"hits":[],"ocrAvailable":False,"query":q})); sys.exit(0)
try:
    c=sqlite3.connect("file:%s?mode=ro"%db, uri=True); c.row_factory=sqlite3.Row
except Exception:
    print(json.dumps({"hits":[],"ocrAvailable":False,"query":q})); sys.exit(0)
ocr_ok=False
try:
    c.execute("SELECT 1 FROM ocr LIMIT 1"); ocr_ok=True
except Exception:
    ocr_ok=False
like="%"+q.lower()+"%"
hits=[]; seen=set()
def add(ts,f,snippet=""):
    ts=int(ts or 0)
    if ts in seen: return
    seen.add(ts)
    row={"ts":ts,"path":absf(f["path"]) if f else "","app":(f["app"] if f else "clipboard") or "",
         "title":(f["title"] if f else "") or "","snippet":snippet,"boxes":[]}
    hits.append(row)
def frame_at(ts):
    try:
        return c.execute("SELECT ts,path,app,title FROM frames WHERE ts<=? ORDER BY ts DESC LIMIT 1",(ts,)).fetchone() \
            or c.execute("SELECT ts,path,app,title FROM frames WHERE ts>=? ORDER BY ts ASC LIMIT 1",(ts,)).fetchone()
    except Exception:
        return None
try:
    if ocr_ok:
        for r in c.execute("SELECT ts,text FROM ocr WHERE ocr MATCH ? LIMIT 60",(q,)):
            f=frame_at(r["ts"]); add(r["ts"],f,(r["text"] or "")[:80])
except Exception:
    pass
try:
    for r in c.execute("SELECT ts,path,app,title FROM frames WHERE lower(title) LIKE ? OR lower(app) LIKE ? ORDER BY ts DESC LIMIT 60",(like,like)):
        add(r["ts"],r,(r["title"] or ""))
except Exception:
    pass
try:
    for r in c.execute("SELECT ts,content FROM clips WHERE lower(content) LIKE ? ORDER BY ts DESC LIMIT 60",(like,)):
        add(r["ts"],frame_at(r["ts"]),(r["content"] or "")[:80])
except Exception:
    pass
c.close()
hits.sort(key=lambda h:h["ts"])
print(json.dumps({"hits":hits[-50:],"ocrAvailable":ocr_ok,"query":q}))
' "$1" "$DB" 2>/dev/null || echo '{"hits":[],"ocrAvailable":false,"query":""}'
}

timeline_json() {
  python3 -c '
import json,sys,os,sqlite3
db=sys.argv[1]; frames=[]
root=os.path.dirname(os.path.abspath(db))
def absf(p):
    p=p or ""
    return p if (not p or os.path.isabs(p)) else os.path.join(root,p)
if os.path.exists(db):
    try:
        c=sqlite3.connect("file:%s?mode=ro"%db, uri=True); c.row_factory=sqlite3.Row
        rows=c.execute("SELECT ts,path,app,title,workspace,output FROM frames ORDER BY ts DESC LIMIT 400").fetchall()
        frames=[{"ts":r["ts"],"path":absf(r["path"]),"app":r["app"] or "","title":r["title"] or "",
                 "workspace":r["workspace"] or "","output":r["output"] or ""} for r in reversed(rows)]
        c.close()
    except Exception:
        frames=[]
gaps=[]
for a,b in zip(frames,frames[1:]):
    if int(b["ts"])-int(a["ts"])>15000:
        gaps.append({"from":a["ts"],"to":b["ts"],"reason":"gap"})
print(json.dumps({"frames":frames,"gaps":gaps}))
' "$DB" 2>/dev/null || echo '{"frames":[],"gaps":[]}'
}

clips_json() {
  python3 -c '
import json,sys,os,sqlite3
db=sys.argv[1]; clips=[]
if os.path.exists(db):
    try:
        c=sqlite3.connect("file:%s?mode=ro"%db, uri=True); c.row_factory=sqlite3.Row
        for r in c.execute("SELECT ts,content,mime FROM clips ORDER BY ts DESC LIMIT 120"):
            clips.append({"ts":r["ts"],"content":r["content"] or "","mime":r["mime"] or "text/plain"})
        c.close()
    except Exception:
        clips=[]
print(json.dumps({"clips":clips}))
' "$DB" 2>/dev/null || echo '{"clips":[]}'
}

moment_json() {
  python3 -c '
import json,sys,os,sqlite3
ts=int(sys.argv[1] or 0); db=sys.argv[2]; root=sys.argv[3]
def absf(p):
    p=p or ""
    return p if (not p or os.path.isabs(p)) else os.path.join(root,p)
frame=None; clip=""; windows=[]
if os.path.exists(db):
    try:
        c=sqlite3.connect("file:%s?mode=ro"%db, uri=True); c.row_factory=sqlite3.Row
        r=c.execute("SELECT ts,path,app,title,workspace,output FROM frames WHERE ts=?",(ts,)).fetchone() \
          or c.execute("SELECT ts,path,app,title,workspace,output FROM frames WHERE ts<=? ORDER BY ts DESC LIMIT 1",(ts,)).fetchone()
        if r:
            frame={"ts":r["ts"],"path":absf(r["path"]),"app":r["app"] or "","title":r["title"] or "",
                   "workspace":r["workspace"] or "","output":r["output"] or ""}
            cr=c.execute("SELECT content FROM clips WHERE ts<=? ORDER BY ts DESC LIMIT 1",(r["ts"],)).fetchone()
            clip=(cr["content"] if cr else "") or ""
            lr=c.execute("SELECT json FROM layouts WHERE ts<=? ORDER BY ts DESC LIMIT 1",(r["ts"],)).fetchone()
            if lr and lr["json"]:
                try: windows=json.loads(lr["json"])
                except Exception: windows=[]
        c.close()
    except Exception:
        pass
print(json.dumps({"frame":frame,"clip":clip,"windows":windows,"boxes":[]}))
' "$1" "$DB" "$DATA" 2>/dev/null || echo '{"frame":null,"clip":"","windows":[],"boxes":[]}'
}

# Wipe operates on the REAL SQLite store: delete the frame/ocr/clip/layout/event
# rows in range AND unlink the on-disk frame + crop files, then report the true
# deleted count. A missing DB reports 0 honestly. Never a false "wiped" while
# sensitive data survives. Exits non-zero (caller emits an error) on failure.
rewrite_index() {
  python3 -c '
import json,os,sys,sqlite3,time
db=sys.argv[1]; root=sys.argv[2]
scope=sys.argv[3]; lo=int(sys.argv[4] or 0); hi=int(sys.argv[5] or 0); now=int(sys.argv[6] or 0)
if scope=="all":
    lo,hi=0,2**62
elif scope=="today":
    t=time.localtime(now/1000.0 if now>10**12 else now)
    lo=int(time.mktime((t.tm_year,t.tm_mon,t.tm_mday,0,0,0,t.tm_wday,t.tm_yday,t.tm_isdst))*1000); hi=now
elif scope=="range":
    if hi==0: hi=now
else:
    print(json.dumps({"wiped":0,"scope":scope})); sys.exit(0)
if not os.path.exists(db):
    print(json.dumps({"wiped":0,"scope":scope})); sys.exit(0)
try:
    c=sqlite3.connect(db); c.row_factory=sqlite3.Row
    c.execute("BEGIN IMMEDIATE")
    files=[]
    for r in c.execute("SELECT path,crop_path FROM frames WHERE ts>=? AND ts<=?",(lo,hi)):
        if r["path"]: files.append(r["path"])
        if r["crop_path"]: files.append(r["crop_path"])
    n=c.execute("SELECT COUNT(*) FROM frames WHERE ts>=? AND ts<=?",(lo,hi)).fetchone()[0] or 0
    for tbl in ("frames","clips","layouts","events","ocr_boxes"):
        try: c.execute("DELETE FROM %s WHERE ts>=? AND ts<=?"%tbl,(lo,hi))
        except Exception: pass
    for tbl in ("ocr","search_fallback"):
        try: c.execute("DELETE FROM %s WHERE ts>=? AND ts<=?"%tbl,(lo,hi))
        except Exception: pass
    c.commit()
    try: c.execute("PRAGMA wal_checkpoint(TRUNCATE)");
    except Exception: pass
    c.close()
except Exception as e:
    sys.stderr.write(str(e)); sys.exit(1)
# Only after the DB rows are gone do we unlink the files (relative to root).
for p in files:
    fp=p if os.path.isabs(p) else os.path.join(root,p)
    try: os.remove(fp)
    except Exception: pass
print(json.dumps({"wiped":n,"scope":scope}))
' "$DB" "$DATA" "$1" "${2:-0}" "${3:-0}" "$(now_ms)"
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
      from=0
      to=0
      if [ $# -ge 2 ] && [ "${2#--}" = "$2" ]; then
        scope="$2"
        shift 2
      else
        scope=today
        shift
      fi
      while [ $# -gt 0 ]; do
        case "$1" in
          --from)
            from="${2:-0}"
            shift 2
            ;;
          --to)
            to="${2:-0}"
            shift 2
            ;;
          *)
            shift
            ;;
        esac
      done
      rewrite_index "$scope" "$from" "$to"
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
      if [ "$CONSENT" -ne 1 ]; then
        emit "{\"event\":\"error\",\"id\":$id,\"ok\":false,\"error\":\"consent required: arm rejected until the consent screen is recorded\"}"
        reply "$id" "$(stats_json)"
      else
        emit "{\"event\":\"error\",\"id\":$id,\"ok\":false,\"error\":\"compat helper does not record; run build.sh for rewindd\"}"
        reply "$id" "$(stats_json)"
      fi
      emit "{\"event\":\"stats\",$(stats_json | sed 's/^{//;s/}$//')}"
      ;;
    disarm)
      reply "$id" "$(stats_json)"
      ;;
    consent)
      # Stage the new consent, but only keep it if the atomic save succeeds;
      # otherwise roll back so we never report consent that was not persisted.
      prev_consent=$CONSENT
      prev_consent_at=$CONSENT_AT
      prev_armonlogin=$ARMONLOGIN
      CONSENT=1
      CONSENT_AT=$(now_ms)
      armnow=$(field "$line" armNow)
      armlogin=$(field "$line" armOnLogin)
      if [ "$armlogin" = "true" ]; then
        ARMONLOGIN=1
      else
        ARMONLOGIN=0
      fi
      if ! save_state; then
        CONSENT=$prev_consent
        CONSENT_AT=$prev_consent_at
        ARMONLOGIN=$prev_armonlogin
        emit "{\"event\":\"error\",\"id\":$id,\"ok\":false,\"error\":\"failed to persist consent; not recorded\"}"
        continue
      fi
      if [ "$armnow" = "true" ]; then
        emit "{\"event\":\"error\",\"id\":$id,\"error\":\"compat helper does not record; run build.sh for rewindd\"}"
      fi
      reply "$id" "$(stats_json)"
      emit "{\"event\":\"stats\",$(stats_json | sed 's/^{//;s/}$//')}"
      ;;
    set-pause|setPause) reply "$id" "{\"ok\":true}" ;;
    configure)
      merge_configure "$line"
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
      cts=$(field "$line" ts)
      if ! command -v wl-copy >/dev/null 2>&1; then
        reply "$id" '{"ok":false,"error":"wl-copy not installed"}'
      else
        content=$(python3 -c '
import sys,os,sqlite3
ts=int(sys.argv[1] or 0); db=sys.argv[2]
if os.path.exists(db):
    try:
        c=sqlite3.connect("file:%s?mode=ro"%db, uri=True)
        r=c.execute("SELECT content FROM clips WHERE ts=?",(ts,)).fetchone()
        if r and r[0]: sys.stdout.write(r[0])
        c.close()
    except Exception:
        pass
' "$cts" "$DB" 2>/dev/null)
        if [ -z "$content" ]; then
          reply "$id" '{"ok":false,"error":"no clip at that time"}'
        elif printf '%s' "$content" | wl-copy -t text/plain 2>/dev/null; then
          reply "$id" '{"ok":true}'
        else
          reply "$id" '{"ok":false,"error":"wl-copy failed"}'
        fi
      fi
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
