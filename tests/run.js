#!/usr/bin/env node
"use strict"

const fs = require("fs")
const path = require("path")
const vm = require("vm")
const assert = require("assert")
const { spawnSync } = require("child_process")

const ROOT = path.resolve(__dirname, "..")
const JS = path.join(ROOT, "js")
const FIX = path.join(__dirname, "fixtures")

function loadEngine(file) {
  const src = fs
    .readFileSync(path.join(JS, file), "utf8")
    .replace(/^\.pragma library\s*\n/, "")
  const sandbox = {
    console,
    Date,
    Math,
    JSON,
    String,
    Number,
    Array,
    Object,
    parseInt,
    isNaN,
    exports: {},
    module: { exports: {} }
  }
  vm.createContext(sandbox)
  vm.runInContext(src, sandbox, { filename: file })
  const exported = {}
  for (const key of Object.keys(sandbox)) {
    if (["console", "Date", "Math", "JSON", "String", "Number", "Array", "Object", "parseInt", "isNaN", "exports", "module"].indexOf(key) >= 0)
      continue
    exported[key] = sandbox[key]
  }
  return exported
}

const Protocol = loadEngine("Protocol.js")
const Pause = loadEngine("Pause.js")
const Format = loadEngine("Format.js")
const Plan = loadEngine("Plan.js")
const Query = loadEngine("Query.js")
const Channel = loadEngine("Channel.js")

let passed = 0
let failed = 0

function test(name, fn) {
  try {
    fn()
    passed += 1
    process.stdout.write("ok  " + name + "\n")
  } catch (err) {
    failed += 1
    process.stderr.write("FAIL " + name + "\n" + (err && err.stack ? err.stack : err) + "\n")
  }
}

function fixture(name) {
  return fs.readFileSync(path.join(FIX, name), "utf8")
}

function jsonFix(name) {
  return JSON.parse(fixture(name))
}

test("protocol: parses frame-written and replies", () => {
  const ev = Protocol.parseLine('{"event":"frame-written","ts":1,"path":"/x.png","bytes":40}')
  assert.strictEqual(Protocol.isFrame(ev), true)
  const reply = Protocol.parseLine('{"event":"reply","id":2,"ok":true,"data":{"armed":true}}')
  assert.strictEqual(Protocol.isReply(reply), true)
  const stats = Protocol.emptyStats()
  Protocol.mergeStats(stats, reply.data)
  assert.strictEqual(stats.armed, true)
})

test("protocol: command assigns ids", () => {
  const a = JSON.parse(Protocol.command("arm", { byteCapGb: 2 }))
  const b = JSON.parse(Protocol.command("query", { q: "hello" }))
  assert.strictEqual(a.cmd, "arm")
  assert.strictEqual(b.cmd, "query")
  assert.ok(a.id !== b.id)
})

test("pause: disarmed is 0 fps", () => {
  assert.strictEqual(Pause.evaluate({ armed: false }), "disarmed")
})

test("pause: lock overlay portal exclusion idle", () => {
  assert.strictEqual(Pause.evaluate({ armed: true, locked: true, idleLimitMs: 120000 }), "locked")
  assert.strictEqual(Pause.evaluate({ armed: true, overlayOpen: true, idleLimitMs: 120000 }), "overlay")
  assert.strictEqual(Pause.evaluate({ armed: true, portalActive: true, idleLimitMs: 120000 }), "portal")
  assert.strictEqual(Pause.evaluate({ armed: true, excluded: "keepassxc", idleLimitMs: 120000 }), "excluded")
  assert.strictEqual(Pause.evaluate({ armed: true, idleMs: 180000, idleLimitMs: 120000 }), "idle")
  assert.strictEqual(Pause.evaluate({ armed: true, idleMs: 30000, idleLimitMs: 120000 }), null)
})

test("pause: excluded visible anywhere, not just focused", () => {
  const clients = jsonFix("clients-keepass-second-output.json")
  const hit = Pause.excludedVisible(clients, Pause.DEFAULT_EXCLUDES)
  assert.strictEqual(hit, "keepassxc")
  const clean = jsonFix("clients-normal.json")
  assert.strictEqual(Pause.excludedVisible(clean, Pause.DEFAULT_EXCLUDES), null)
})

test("pause: hidden keepass does not pause", () => {
  const clients = jsonFix("clients-keepass-hidden.json")
  assert.strictEqual(Pause.excludedVisible(clients, ["keepassxc"]), null)
})

test("pause: chrome incognito is labeled heuristic", () => {
  const clients = jsonFix("clients-incognito.json")
  const hit = Pause.privateBrowsing(clients)
  assert.ok(hit)
  assert.strictEqual(hit.heuristic, true)
  assert.ok(hit.marker.indexOf("Incognito") >= 0)
})

test("pause: firefox private browsing marker", () => {
  const clients = [{ class: "firefox", title: "Bank — (Private Browsing)", mapped: true, hidden: false }]
  const hit = Pause.privateBrowsing(clients)
  assert.ok(hit)
  assert.strictEqual(hit.heuristic, true)
})

test("format: 25-80KB planning band is days not fiction", () => {
  const band = Format.planningDays(2 * 1024 * 1024 * 1024)
  assert.ok(band.low > 2)
  assert.ok(band.high > band.low)
  assert.ok(band.high < 20)
  const label = Format.daysLabel(null, 2 * 1024 * 1024 * 1024)
  assert.ok(label.indexOf("25–80") >= 0 || label.indexOf("25-80") >= 0 || label.indexOf("days") >= 0)
})

test("format: measured days need a minute of data", () => {
  assert.strictEqual(Format.daysEstimate(100, 2e9, 1, 1000), null)
  const d = Format.daysEstimate(50 * 1024 * 1024, 2e9, 1, 1 + 86400000)
  assert.ok(d > 1)
})

test("format: human bytes", () => {
  assert.ok(Format.humanBytes(40 * 1024).indexOf("KB") >= 0)
  assert.ok(Format.humanBytes(2 * 1024 * 1024 * 1024).indexOf("GB") >= 0)
})

test("query: bbox scales to 0-1", () => {
  const b = Query.scaleBox({ x: 10, y: 20, w: 30, h: 10 }, { x: 100, y: 50 }, { w: 200, h: 100 }, { w: 100, h: 50 })
  assert.ok(b.x > 0 && b.x < 1)
  assert.ok(b.w > 0)
})

test("query: ROI-local tess box maps through crop origin onto stored frame", () => {
  const b = Query.scaleBox(
    { x: 0, y: 0, w: 48, h: 16 },
    { x: 400, y: 108 },
    { w: 1920, h: 1080 },
    { w: 1280, h: 720 },
    { w: 800, h: 600 }
  )
  assert.ok(Math.abs(b.x - 400 / 1920) < 0.001)
  assert.ok(Math.abs(b.y - 108 / 1080) < 0.001)
  assert.notStrictEqual(b.x, 0)
})

test("query: snippet around needle", () => {
  const s = Query.snippetAround("the commit hash abcdef123 lives here", "abcdef123")
  assert.ok(s.indexOf("abcdef123") >= 0)
})

test("query: fittedRect letterboxes PreserveAspectFit", () => {
  const r = Query.fittedRect(200, 100, 100, 100)
  assert.strictEqual(r.w, 100)
  assert.strictEqual(r.h, 100)
  assert.strictEqual(r.x, 50)
  assert.strictEqual(r.y, 0)
})

test("protocol: consent payload is one JSON object", () => {
  const line = Protocol.command("consent", { armNow: true, armOnLogin: false })
  const body = JSON.parse(line)
  assert.strictEqual(body.cmd, "consent")
  assert.strictEqual(body.armNow, true)
  assert.strictEqual(body.armOnLogin, false)
})

test("plan: browsers are unrecoverable tabs", () => {
  assert.strictEqual(Plan.isBrowser("firefox"), true)
  assert.strictEqual(Plan.isBrowser("kitty"), false)
  const label = Plan.stepLabel({ kind: "exec", class: "kitty", label: "Launch Kitty" })
  assert.strictEqual(label, "Launch Kitty")
})

test("plan: no client-side one-window fabrication (helper builds the command)", () => {
  // The launch command for a single-window reopen must be resolved by the
  // helper (rewindd plan::build_one), never fabricated in QML/JS from stored
  // window data that has no exec/cmd. Guard against the old bug regressing.
  assert.strictEqual(typeof Plan.oneWindowPlan, "undefined")
  const planSrc = fs.readFileSync(path.join(__dirname, "..", "js", "Plan.js"), "utf8")
  assert.ok(planSrc.indexOf("oneWindowPlan") < 0)
  assert.ok(planSrc.indexOf("win.exec || win.cmd") < 0)
})

test("query: gapReason maps lock vs gap spans", () => {
  const gaps = [{ from: 100, to: 200, reason: "lock" }, { from: 300, to: 400, reason: "gap" }]
  assert.strictEqual(Query.gapReason(150, gaps), "lock")
  assert.strictEqual(Query.gapReason(350, gaps), "gap")
  assert.strictEqual(Query.gapReason(50, gaps), "")
})

test("golden corpus: title+clipboard search without OCR", () => {
  const corpus = jsonFix("search-corpus.json")
  const q = "zebra-token"
  const hits = corpus.filter((row) => {
    const blob = (row.title + " " + row.app + " " + row.clip).toLowerCase()
    return blob.indexOf(q) >= 0
  })
  assert.ok(hits.length >= 1)
  const clipQ = corpus.filter((row) => String(row.clip).indexOf("git-hash-ff00aa") >= 0)
  assert.ok(clipQ.length >= 1)
})

test("manifest: id, kinds, keepLoaded, barWidget schema", () => {
  const man = JSON.parse(fs.readFileSync(path.join(ROOT, "manifest.json"), "utf8"))
  assert.strictEqual(man.schemaVersion, 1)
  assert.strictEqual(man.id, "io.github.chris.rewind")
  assert.ok(man.kinds.indexOf("service") >= 0)
  assert.ok(man.kinds.indexOf("overlay") >= 0)
  assert.ok(man.kinds.indexOf("bar-widget") >= 0)
  assert.strictEqual(man.keepLoaded, true)
  assert.strictEqual(man.entryPoints.service, "Service.qml")
  assert.strictEqual(man.barWidget.defaults.armOnLogin, false)
  assert.strictEqual(man.barWidget.defaults.byteCapGb, 2)
  assert.ok(man.id.indexOf("omarchy.") !== 0)
})

test("compat helper exists and is executable after chmod", () => {
  const p = path.join(ROOT, "compat", "rewindd.sh")
  assert.ok(fs.existsSync(p))
  fs.chmodSync(p, 0o755)
  const r = spawnSync(p, ["self-test"], { encoding: "utf8" })
  assert.strictEqual(r.status, 0, r.stderr)
  assert.ok(String(r.stdout).indexOf("self-test ok") >= 0)
})

test("Channel.parse reads snapshot JSON", () => {
  const u = Channel.parse('{"armed":true,"hits":[]}', {})
  assert.strictEqual(u.armed, true)
  assert.strictEqual(Channel.arrayOf(u.hits).length, 0)
})

test("live refresh overrides an armed snapshot and skips repeated consent", () => {
  const snap = { armed: true, consent: true, reason: "" }
  const live = { armed: false, consent: true, reason: "disarmed" }
  const ui = Channel.applyLiveUi({}, snap)
  assert.strictEqual(ui.armed, true)
  Channel.applyLiveUi(ui, live)
  assert.strictEqual(ui.armed, false)
  assert.strictEqual(ui.consent, true)
  assert.strictEqual(ui.firstRun, false)
  assert.strictEqual(Channel.overlayViewAfterRefresh(ui.consent, ""), "scrub")
  assert.strictEqual(Channel.overlayViewAfterRefresh(false, ""), "consent")
  assert.strictEqual(Channel.overlayViewAfterRefresh(true, "clips"), "clips")
})

test("overlay and bar widget do not call serviceFor", () => {
  const overlay = fs.readFileSync(path.join(ROOT, "Overlay.qml"), "utf8")
  const bar = fs.readFileSync(path.join(ROOT, "BarWidget.qml"), "utf8")
  assert.ok(overlay.indexOf("serviceFor") < 0)
  assert.ok(overlay.indexOf("firstPartyServiceFor") < 0)
  assert.ok(bar.indexOf("serviceFor") < 0)
  assert.ok(bar.indexOf("firstPartyServiceFor") < 0)
  assert.ok(overlay.indexOf("omarchy-shell") >= 0)
  assert.ok(bar.indexOf("omarchy-shell") >= 0)
})

test("linux helper workflow exists", () => {
  const yml = fs.readFileSync(path.join(ROOT, ".github/workflows/linux-helper.yml"), "utf8")
  assert.ok(yml.indexOf("ubuntu-latest") >= 0)
  assert.ok(yml.indexOf("--features wayland") >= 0)
  assert.ok(yml.indexOf("network-audit") >= 0)
  assert.ok(yml.indexOf("upload-artifact") >= 0)
})

test("network-audit fails when binary is missing", () => {
  const script = path.join(ROOT, "scripts", "network-audit.sh")
  fs.chmodSync(script, 0o755)
  const r = spawnSync(script, [], {
    encoding: "utf8",
    env: Object.assign({}, process.env, { PATH: process.env.PATH }),
    cwd: ROOT
  })
  if (fs.existsSync(path.join(ROOT, "bin", "rewindd"))) {
    return
  }
  assert.notStrictEqual(r.status, 0, r.stdout + r.stderr)
  assert.ok(String(r.stderr + r.stdout).indexOf("missing") >= 0)
})

test("IpcHandler exposes JSON-arg methods", () => {
  const src = fs.readFileSync(path.join(ROOT, "Service.qml"), "utf8")
  for (const name of ["consentNow", "copyClip", "executePlan", "wipe", "reopenPlan", "reopenWindow", "query", "refresh", "summon", "toggleArm"]) {
    assert.ok(src.indexOf("function " + name + "(arg: string)") >= 0
      || src.indexOf("function " + name + "(q: string)") >= 0, name)
  }
  // reopenWindow must forward a reopen-window command to the helper.
  assert.ok(src.indexOf('send("reopen-window"') >= 0)
})

test("overlay serializes IPC and opens via one refresh", () => {
  const src = fs.readFileSync(path.join(ROOT, "Overlay.qml"), "utf8")
  assert.ok(src.indexOf("function kickIpc()") >= 0)
  assert.ok(src.indexOf("ipcQueue") >= 0)
  assert.ok(src.indexOf('root.callSvc("refresh"') >= 0)
  assert.ok(src.indexOf("if (ipcProc.running)") >= 0)
})

test("overlay wipe has today, all, and range JSON", () => {
  const src = fs.readFileSync(path.join(ROOT, "Overlay.qml"), "utf8")
  assert.ok(src.indexOf('root.openWipe("today")') >= 0)
  assert.ok(src.indexOf('root.openWipe("all")') >= 0)
  assert.ok(src.indexOf('root.openWipe("range")') >= 0)
  assert.ok(src.indexOf('scope: "range"') >= 0)
  assert.ok(src.indexOf("wipeFromTs") >= 0)
  assert.ok(src.indexOf("wipeToTs") >= 0)
  assert.ok(src.indexOf("wipe today") < 0)
})

// Build a minimal rewind.db matching the Rust helper's schema, so the compat
// fallback tests exercise the REAL SQLite store (not a legacy JSONL index).
function seedDb(dir, sql) {
  fs.mkdirSync(path.join(dir, "frames"), { recursive: true })
  const schema = `
CREATE TABLE frames(ts INTEGER PRIMARY KEY,path TEXT,app TEXT,title TEXT,workspace TEXT,output TEXT,width INT,height INT,out_w INT,out_h INT,crop_x INT,crop_y INT,crop_w INT,crop_h INT,bytes INT,dhash INT,encoder TEXT,crop_path TEXT,crop_bytes INT DEFAULT 0);
CREATE TABLE clips(ts INTEGER PRIMARY KEY,mime TEXT,content TEXT,bytes INT);
CREATE TABLE layouts(ts INTEGER PRIMARY KEY,json TEXT);
CREATE TABLE ocr_boxes(ts INT,word TEXT,x REAL,y REAL,w REAL,h REAL);
CREATE TABLE events(ts INT,kind TEXT,reason TEXT);
CREATE VIRTUAL TABLE ocr USING fts5(ts UNINDEXED,text,app,title,clip);
`
  const r = spawnSync("sqlite3", [path.join(dir, "rewind.db"), schema + "\n" + (sql || "")], { encoding: "utf8" })
  if (r.status !== 0) throw new Error("sqlite3 seed failed: " + r.stderr)
}

test("compat query reads the real sqlite store", () => {
  if (spawnSync("sqlite3", ["--version"], { encoding: "utf8" }).status !== 0) return
  const tmp = fs.mkdtempSync(path.join(require("os").tmpdir(), "rewind-"))
  process.env.REWIND_DATA_DIR = tmp
  seedDb(tmp, `
INSERT INTO frames VALUES(1,'frames/1.webp','kitty','unique-xyz phrase','1','eDP-1',0,0,0,0,0,0,0,0,10,1,'cwebp',NULL,0);
INSERT INTO ocr VALUES(1,'unique-xyz phrase','kitty','unique-xyz phrase','');
`)
  const p = path.join(ROOT, "compat", "rewindd.sh")
  fs.chmodSync(p, 0o755)
  const r = spawnSync(p, ["query", "unique-xyz"], { encoding: "utf8", env: process.env })
  assert.strictEqual(r.status, 0, r.stderr + r.stdout)
  const body = JSON.parse(r.stdout)
  assert.ok(body.hits && body.hits.length >= 1, r.stdout)
})

test("compat daemon writes nothing while disarmed", () => {
  const os = require("os")
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "rewind-zero-"))
  const data = path.join(tmp, "data")
  const env = Object.assign({}, process.env, { REWIND_DATA_DIR: data })
  const p = path.join(ROOT, "compat", "rewindd.sh")
  fs.chmodSync(p, 0o755)
  const r = spawnSync(p, ["daemon"], {
    encoding: "utf8",
    env,
    input: '{"cmd":"hello","id":1}\n{"cmd":"stats","id":2}\n{"cmd":"timeline","id":3}\n{"cmd":"configure","id":4,"byteCapGb":2}\n{"cmd":"arm","id":5}\n{"cmd":"shutdown","id":6}\n'
  })
  assert.strictEqual(r.status, 0, r.stderr + r.stdout)
  assert.ok(r.stdout.indexOf('"event":"ready"') >= 0, r.stdout)
  const walked = []
  function walk(dir) {
    if (!fs.existsSync(dir))
      return
    for (const name of fs.readdirSync(dir)) {
      const pth = path.join(dir, name)
      if (fs.statSync(pth).isDirectory())
        walk(pth)
      else
        walked.push(path.relative(data, pth))
    }
  }
  walk(data)
  assert.deepStrictEqual(walked, [], "disarmed fallback wrote " + JSON.stringify(walked))
})

test("compat configure with existing consent is memory-only", () => {
  const os = require("os")
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "rewind-cfg-"))
  const env = Object.assign({}, process.env, { REWIND_DATA_DIR: tmp })
  const state = path.join(tmp, "state.json")
  const original = JSON.stringify({
    armed: false,
    consentAt: 1700000000000,
    armOnLogin: false,
    byteCap: 2147483648,
    cadenceMs: 3000,
    idlePauseSec: 120
  })
  fs.writeFileSync(state, original)
  const before = fs.readdirSync(tmp).sort()
  const p = path.join(ROOT, "compat", "rewindd.sh")
  fs.chmodSync(p, 0o755)
  const r = spawnSync(p, ["daemon"], {
    encoding: "utf8",
    env,
    input: '{"cmd":"configure","id":1,"byteCapGb":4,"cadenceMs":5000}\n{"cmd":"shutdown","id":2}\n'
  })
  assert.strictEqual(r.status, 0, r.stderr + r.stdout)
  assert.ok(r.stdout.indexOf('"event":"ready"') >= 0, r.stdout)
  assert.strictEqual(fs.readFileSync(state, "utf8"), original)
  assert.deepStrictEqual(fs.readdirSync(tmp).sort(), before)
})

test("service gates snapshots on consent and serves the overlay while open", () => {
  const src = fs.readFileSync(path.join(ROOT, "Service.qml"), "utf8")
  // Publishing the UI read-response channel requires consent (a fresh,
  // never-consented install writes nothing) and happens while recording OR
  // while the overlay is open — so the overlay is functional even though opening
  // it pauses capture (blocker r15/1).
  assert.ok(/publishUi:\s*root\.consent\s*&&\s*\(root\.armed\s*\|\|\s*root\.overlayOpen\)/.test(src))
  assert.ok(/publish\(\)\s*\{\s*\n\s*if \(!root\.publishUi\)/.test(src))
  // The read-response channel lives in the ephemeral tmpfs snapDir, not the
  // persistent data dir.
  assert.ok(/snapDir\s*\+\s*"\/timeline\.json"/.test(src))
  assert.ok(src.indexOf("data.wiped") >= 0)
  assert.ok(src.indexOf("root.refreshTimeline()") >= 0)
  assert.ok(src.indexOf("root.armed = true") < 0)
  assert.ok(src.indexOf("function summon(arg: string)") >= 0)
  assert.ok(src.indexOf("function toggleArm(arg: string)") >= 0)
  assert.ok(/helperReady = true/.test(src.split("if (ev.event === \"ready\")")[1] || ""))
  assert.ok(src.indexOf("root.helperReady = true") >= 0)
  const start = src.split("function startHelper")[1] || ""
  assert.ok(start.indexOf("root.helperReady = false") >= 0)
})

test("disarm never optimistically flips state; helper ack is authoritative", () => {
  const src = fs.readFileSync(path.join(ROOT, "Service.qml"), "utf8")
  const dis = (src.split("function disarm()")[1] || "").split("function toggleArm")[0]
  // No optimistic local mutation — the helper's state event flips armed.
  assert.ok(dis.indexOf("root.armed = false") < 0, "disarm must not optimistically set armed")
  assert.ok(dis.indexOf('send("disarm"') >= 0)
})

test("service builds the recorder on a fresh install when cargo is present", () => {
  const src = fs.readFileSync(path.join(ROOT, "Service.qml"), "utf8")
  assert.ok(src.indexOf("function buildHelper") >= 0)
  assert.ok(/command -v cargo/.test(src), "probe must detect cargo to build")
  assert.ok(src.indexOf("buildable") >= 0)
  assert.ok(src.indexOf("sh build.sh") >= 0)
  assert.ok(src.indexOf("root.triedBuild = true") >= 0, "must not loop on build")
})

test("bar press-and-hold opens clips only for a left-button hold", () => {
  const bar = fs.readFileSync(path.join(ROOT, "BarWidget.qml"), "utf8")
  const hold = (bar.split("onPressAndHold")[1] || "").split("}")[0] + (bar.split("onPressAndHold")[1] || "")
  assert.ok(/heldButton !== Qt\.LeftButton/.test(bar), "hold must be gated on left button")
})

test("compat fallback operates on the sqlite store, not a legacy jsonl index", () => {
  const sh = fs.readFileSync(path.join(ROOT, "compat", "rewindd.sh"), "utf8")
  assert.ok(sh.indexOf("rewind.db") >= 0, "fallback must target rewind.db")
  assert.ok(sh.indexOf("index.jsonl") < 0 && sh.indexOf("clips.jsonl") < 0, "no legacy JSONL")
  assert.ok(/DELETE FROM %s WHERE ts>=\? AND ts<=\?/.test(sh) && /"frames","clips","layouts","events","ocr_boxes"/.test(sh), "wipe must delete real sqlite rows")
  assert.ok(!/eval\s+"\$\(/.test(sh), "no eval of untrusted python/settings output")
})

test("bar polls live status and toggleArm always sends an arg", () => {
  const bar = fs.readFileSync(path.join(ROOT, "BarWidget.qml"), "utf8")
  assert.ok(bar.indexOf("function applyLive") >= 0)
  assert.ok(bar.indexOf("function pollStatus") >= 0)
  assert.ok(bar.indexOf("interval: 2500") >= 0)
  assert.ok(bar.indexOf("interval: 400") < 0)
  // toggleArm runs through a reply-capturing process (blocker r14/4: the dot
  // must reflect the helper's authoritative post-toggle reply, not a racing
  // poll) and still always sends the "{}" arg.
  assert.ok(bar.indexOf("id: toggleProc") >= 0)
  assert.ok(bar.indexOf('"toggleArm", "{}"') >= 0)
  const togIdx = bar.indexOf("function toggleArm")
  const togEnd = bar.indexOf("\n  function ", togIdx + 1)
  const togBody = bar.slice(togIdx, togEnd < 0 ? togIdx + 800 : togEnd)
  assert.ok(togBody.indexOf("toggleProc") >= 0)
  assert.ok(togBody.indexOf("pollStatus") < 0) // no racing poll after toggle
  const overlay = fs.readFileSync(path.join(ROOT, "Overlay.qml"), "utf8")
  assert.ok(overlay.indexOf("Channel.applyLiveUi") >= 0)
  assert.ok(overlay.indexOf("Channel.overlayViewAfterRefresh") >= 0)
  const readme = fs.readFileSync(path.join(ROOT, "README.md"), "utf8")
  assert.ok(readme.indexOf("toggleArm '{}'") >= 0)
  assert.ok(readme.indexOf("omarchy bar put") < 0)
  assert.ok(readme.indexOf("./scripts/rewind wipe") >= 0)
  assert.ok(readme.indexOf("omarchy plugin enable") >= 0)
  assert.ok(fs.existsSync(path.join(ROOT, "preview.png")))
})

test("rewind launcher forwards wipe range bounds to the sqlite store", () => {
  if (spawnSync("sqlite3", ["--version"], { encoding: "utf8" }).status !== 0) return
  const launcher = path.join(ROOT, "scripts", "rewind")
  assert.ok(fs.existsSync(launcher))
  fs.chmodSync(launcher, 0o755)
  const os = require("os")
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "rewind-range-"))
  const gone = path.join(tmp, "frames", "1.webp")
  const stay = path.join(tmp, "frames", "2.webp")
  seedDb(tmp, `
INSERT INTO frames VALUES(10,'frames/1.webp','a','a','1','o',0,0,0,0,0,0,0,0,10,1,'cwebp',NULL,0);
INSERT INTO frames VALUES(50,'frames/2.webp','b','b','1','o',0,0,0,0,0,0,0,0,10,2,'cwebp',NULL,0);
`)
  fs.writeFileSync(gone, "a")
  fs.writeFileSync(stay, "b")
  const env = Object.assign({}, process.env, { REWIND_DATA_DIR: tmp })
  const r = spawnSync(launcher, ["wipe", "range", "--from", "1", "--to", "20"], { encoding: "utf8", env })
  assert.strictEqual(r.status, 0, r.stderr + r.stdout)
  // Row 10 (in range) and its file are gone; row 50 (out of range) survives.
  const rows = spawnSync("sqlite3", [path.join(tmp, "rewind.db"), "SELECT ts FROM frames ORDER BY ts"], { encoding: "utf8" }).stdout.trim()
  assert.strictEqual(rows, "50", rows)
  assert.ok(!fs.existsSync(gone), "in-range frame file must be deleted")
  assert.ok(fs.existsSync(stay), "out-of-range frame file must survive")
})

test("ocr does not treat disarmed as idle and capture reports runtime backend", () => {
  const ocr = fs.readFileSync(path.join(ROOT, "src", "rewindd", "src", "ocr.rs"), "utf8")
  assert.ok(ocr.indexOf("if !shared.is_armed()") >= 0)
  const lib = fs.readFileSync(path.join(ROOT, "src", "rewindd", "src", "lib.rs"), "utf8")
  const idleFn = lib.split("fn is_idle")[1] || ""
  assert.ok(idleFn.indexOf("PauseReason::Idle") >= 0)
  assert.ok(idleFn.split("fn with_store")[0].indexOf("PauseReason::Disarmed") < 0)
  assert.ok(lib.indexOf("recording_now") >= 0)
  assert.ok(lib.indexOf("finalize_capture") >= 0)
  const cap = fs.readFileSync(path.join(ROOT, "src", "rewindd", "src", "capture.rs"), "utf8")
  assert.ok(cap.indexOf("active_backend") >= 0)
  assert.ok(cap.indexOf("wlr_retry_at") >= 0)
  assert.ok(cap.indexOf("grim_only = true") < 0)
})

test("overlay refreshes and clears after wipe; range uses archive bounds", () => {
  const src = fs.readFileSync(path.join(ROOT, "Overlay.qml"), "utf8")
  assert.ok(/function doWipe[\s\S]*callSvc\("refresh"/.test(src))
  assert.ok(src.indexOf("uiFirstTs") >= 0)
  assert.ok(src.indexOf("uiLastTs") >= 0)
  assert.ok(src.indexOf("full archive") >= 0)
  assert.ok(src.indexOf("function askOneWindow") >= 0)
  // askOneWindow must request the plan from the helper (async), not fabricate it.
  assert.ok(src.indexOf("Plan.oneWindowPlan") < 0)
  assert.ok(src.indexOf('callSvc("reopenWindow"') >= 0)
  assert.ok(src.indexOf("Measured on real UI") < 0)
  assert.ok(src.indexOf("Planning estimate") >= 0)
  assert.ok(src.indexOf("Query.gapReason") >= 0)
})

test("encoder fallback chain is cwebp then image-webp then smaller png", () => {
  const cargo = fs.readFileSync(path.join(ROOT, "src", "rewindd", "Cargo.toml"), "utf8")
  assert.ok(cargo.indexOf('"webp"') >= 0)
  const enc = fs.readFileSync(path.join(ROOT, "src", "rewindd", "src", "encode.rs"), "utf8")
  assert.ok(enc.indexOf("ImageWebp") >= 0)
  assert.ok(enc.indexOf("write_image_webp") >= 0)
  assert.ok(enc.indexOf("downscale_to(rgba, width, height, 720)") >= 0)
})

test("network-audit requires daemon completion, not strace || true", () => {
  const src = fs.readFileSync(path.join(ROOT, "scripts", "network-audit.sh"), "utf8")
  assert.ok(src.indexOf("strace") >= 0)
  assert.ok(!/strace[^\n]*\|\|\s*true/.test(src))
  assert.ok(src.indexOf('"event":"ready"') >= 0)
  assert.ok(src.indexOf('"bye":true') >= 0)
})

test("compat arm without consent is rejected and does not record consent", () => {
  const os = require("os")
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "rewind-norecord-"))
  const env = Object.assign({}, process.env, { REWIND_DATA_DIR: tmp })
  const p = path.join(ROOT, "compat", "rewindd.sh")
  fs.chmodSync(p, 0o755)
  const r = spawnSync(p, ["daemon"], {
    encoding: "utf8",
    env,
    input: '{"cmd":"arm","id":1}\n{"cmd":"shutdown","id":2}\n'
  })
  assert.strictEqual(r.status, 0, r.stderr + r.stdout)
  const framesDir = path.join(tmp, "frames")
  const files = fs.existsSync(framesDir) ? fs.readdirSync(framesDir) : []
  assert.strictEqual(files.length, 0)
  assert.ok(r.stdout.indexOf("consent required") >= 0, r.stdout)
  const statePath = path.join(tmp, "state.json")
  if (fs.existsSync(statePath)) {
    const st = JSON.parse(fs.readFileSync(statePath, "utf8"))
    assert.ok(!st.consentAt)
  }
})

test("compat consent persists armOnLogin and still does not record", () => {
  const os = require("os")
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "rewind-consent-"))
  const env = Object.assign({}, process.env, { REWIND_DATA_DIR: tmp })
  const p = path.join(ROOT, "compat", "rewindd.sh")
  fs.chmodSync(p, 0o755)
  const r = spawnSync(p, ["daemon"], {
    encoding: "utf8",
    env,
    input: '{"cmd":"consent","id":1,"armNow":true,"armOnLogin":true}\n{"cmd":"arm","id":2}\n{"cmd":"shutdown","id":3}\n'
  })
  assert.strictEqual(r.status, 0, r.stderr + r.stdout)
  const st = JSON.parse(fs.readFileSync(path.join(tmp, "state.json"), "utf8"))
  assert.ok(st.consentAt > 0)
  assert.strictEqual(st.armOnLogin, true)
  assert.ok(r.stdout.indexOf("does not record") >= 0, r.stdout)
  const files = fs.existsSync(path.join(tmp, "frames")) ? fs.readdirSync(path.join(tmp, "frames")) : []
  assert.strictEqual(files.length, 0)
})

test("compat wipe deletes real sqlite rows and files (no false success)", () => {
  if (spawnSync("sqlite3", ["--version"], { encoding: "utf8" }).status !== 0) return
  const os = require("os")
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "rewind-wipe-"))
  const gone = path.join(tmp, "frames", "1.webp")
  const stay = path.join(tmp, "frames", "2.webp")
  seedDb(tmp, `
INSERT INTO frames VALUES(1,'frames/1.webp','a','a','1','o',0,0,0,0,0,0,0,0,10,1,'cwebp',NULL,0);
INSERT INTO frames VALUES(2,'frames/2.webp','b','b','1','o',0,0,0,0,0,0,0,0,10,2,'cwebp',NULL,0);
INSERT INTO clips VALUES(1,'text/plain','old',3);
`)
  fs.writeFileSync(gone, "a")
  fs.writeFileSync(stay, "b")
  const env = Object.assign({}, process.env, { REWIND_DATA_DIR: tmp })
  const p = path.join(ROOT, "compat", "rewindd.sh")
  const r = spawnSync(p, ["wipe", "all"], { encoding: "utf8", env })
  assert.strictEqual(r.status, 0, r.stderr + r.stdout)
  assert.strictEqual(JSON.parse(r.stdout).wiped, 2, r.stdout)
  // The real store is actually empty — no false "wiped" while data survives.
  const frames = spawnSync("sqlite3", [path.join(tmp, "rewind.db"), "SELECT COUNT(*) FROM frames"], { encoding: "utf8" }).stdout.trim()
  const clips = spawnSync("sqlite3", [path.join(tmp, "rewind.db"), "SELECT COUNT(*) FROM clips"], { encoding: "utf8" }).stdout.trim()
  assert.strictEqual(frames, "0", "frames must be deleted from sqlite")
  assert.strictEqual(clips, "0", "clips must be deleted from sqlite")
  assert.ok(!fs.existsSync(gone) && !fs.existsSync(stay), "frame files must be unlinked")
})

const cargoAvailable = spawnSync("cargo", ["--version"], { encoding: "utf8" }).status === 0
const rust = cargoAvailable
  ? spawnSync("cargo", ["test", "--manifest-path", path.join(ROOT, "src/rewindd/Cargo.toml"), "--offline"], {
      encoding: "utf8"
    })
  : null
test("rust unit tests (offline if already built)", () => {
  if (!cargoAvailable) {
    // cargo is not installed on this host (e.g. the macOS authoring machine).
    // The Rust unit tests run in Linux CI; skip here rather than fail.
    console.log("  (skipped: cargo not available on this host — runs in Linux CI)")
    return
  }
  if (rust.status !== 0) {
    const again = spawnSync("cargo", ["test", "--manifest-path", path.join(ROOT, "src/rewindd/Cargo.toml")], {
      encoding: "utf8"
    })
    assert.strictEqual(again.status, 0, again.stderr + again.stdout)
  } else {
    assert.strictEqual(rust.status, 0, rust.stderr)
  }
})

process.stdout.write("\n" + passed + " passed, " + failed + " failed\n")
process.exit(failed ? 1 : 0)
