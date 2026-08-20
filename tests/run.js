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

test("IpcHandler exposes JSON-arg methods", () => {
  const src = fs.readFileSync(path.join(ROOT, "Service.qml"), "utf8")
  for (const name of ["consentNow", "copyClip", "executePlan", "wipe", "reopenPlan", "query"]) {
    assert.ok(src.indexOf("function " + name + "(arg: string)") >= 0
      || src.indexOf("function " + name + "(q: string)") >= 0, name)
  }
})

test("compat query over index jsonl", () => {
  const tmp = fs.mkdtempSync(path.join(require("os").tmpdir(), "rewind-"))
  process.env.REWIND_DATA_DIR = tmp
  fs.mkdirSync(path.join(tmp, "frames"), { recursive: true })
  fs.writeFileSync(
    path.join(tmp, "index.jsonl"),
    JSON.stringify({ ts: 1, path: "/x.png", app: "kitty", title: "unique-xyz phrase", workspace: "1", bytes: 10 }) + "\n"
  )
  fs.writeFileSync(path.join(tmp, "clips.jsonl"), "")
  const p = path.join(ROOT, "compat", "rewindd.sh")
  fs.chmodSync(p, 0o755)
  const r = spawnSync(p, ["query", "unique-xyz"], { encoding: "utf8", env: process.env })
  assert.strictEqual(r.status, 0, r.stderr + r.stdout)
  const body = JSON.parse(r.stdout)
  assert.ok(body.hits && body.hits.length >= 1)
})

test("compat does not record on arm", () => {
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
  assert.ok(r.stdout.indexOf("does not record") >= 0 || r.stdout.indexOf("compat-norecord") >= 0)
})

test("compat wipe rewrites index instead of leaving stale rows", () => {
  const os = require("os")
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "rewind-wipe-"))
  fs.mkdirSync(path.join(tmp, "frames"), { recursive: true })
  const gone = path.join(tmp, "frames", "1.png")
  const stay = path.join(tmp, "frames", "2.png")
  fs.writeFileSync(gone, "a")
  fs.writeFileSync(stay, "b")
  fs.writeFileSync(
    path.join(tmp, "index.jsonl"),
    JSON.stringify({ ts: 1, path: gone }) + "\n" + JSON.stringify({ ts: 2, path: stay }) + "\n"
  )
  fs.writeFileSync(path.join(tmp, "clips.jsonl"), JSON.stringify({ ts: 1, content: "old" }) + "\n")
  const env = Object.assign({}, process.env, { REWIND_DATA_DIR: tmp })
  const p = path.join(ROOT, "compat", "rewindd.sh")
  const r = spawnSync(p, ["wipe", "all"], { encoding: "utf8", env })
  assert.strictEqual(r.status, 0, r.stderr + r.stdout)
  const idx = fs.readFileSync(path.join(tmp, "index.jsonl"), "utf8").trim()
  assert.strictEqual(idx, "")
  const clips = fs.readFileSync(path.join(tmp, "clips.jsonl"), "utf8").trim()
  assert.strictEqual(clips, "")
  assert.ok(!fs.existsSync(gone))
  assert.ok(!fs.existsSync(stay))
})

const rust = spawnSync("cargo", ["test", "--manifest-path", path.join(ROOT, "src/rewindd/Cargo.toml"), "--offline"], {
  encoding: "utf8"
})
test("rust unit tests (offline if already built)", () => {
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
