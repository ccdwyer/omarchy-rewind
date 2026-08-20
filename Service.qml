import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Hyprland
import "js/Protocol.js" as Protocol
import "js/Pause.js" as Pause
import "js/Format.js" as Format

Item {
  id: root

  property var shell: null
  property var manifest: null
  property var pluginRegistry: null
  property string omarchyPath: Quickshell.env("OMARCHY_PATH") || ""

  readonly property string pluginId: "io.github.chris.rewind"
  readonly property string pluginDir: adapter.pluginDirFrom(Qt.resolvedUrl("."))
  readonly property string dataDir: adapter.dataDir()

  property string helperPath: ""
  property bool helperIsBinary: false
  property bool helperReady: false
  property bool usingFallback: false
  property string helperStatus: "starting"
  property int restarts: 0
  property int nextCmdId: 1
  property var pending: ({})

  property bool armed: false
  property bool paused: false
  property string pauseReason: "disarmed"
  property bool consent: false
  property bool overlayOpen: false
  property int frames: 0
  property int framesToday: 0
  property double bytesUsed: 0
  property double byteCap: 2147483648
  property var daysEstimate: null
  property string encoder: "png"
  property bool ocrAvailable: false
  property string captureBackend: "grim"
  property string lastStatus: "starting"
  property int statsRevision: 0

  property var timeline: []
  property var gaps: []
  property var clips: []
  property var hits: []
  property var currentMoment: ({})
  property var currentPlan: ({})
  property int timelineRevision: 0
  property int clipsRevision: 0
  property int momentRevision: 0
  property int hitsRevision: 0
  property int lastPlanTs: 0
  property bool planReady: false
  property string lastQuery: ""

  property double byteCapGb: 2
  property int cadenceMs: 3000
  property int idlePauseSec: 120
  property string excludeApps: "keepassxc,1Password,1password,Bitwarden,bitwarden,seahorse,polkit-gnome-authentication-agent-1,polkit-kde-authentication-agent-1,lxqt-policykit-agent"
  property string titlePausePatterns: ""
  property bool armOnLogin: false

  RewindAdapter { id: adapter }

  function settingsPayload() {
    return {
      byteCapGb: root.byteCapGb,
      cadenceMs: root.cadenceMs,
      idlePauseSec: root.idlePauseSec,
      excludeApps: root.excludeApps,
      titlePausePatterns: root.titlePausePatterns,
      armOnLogin: root.armOnLogin
    }
  }

  function applySettings(obj) {
    if (!obj)
      return
    if (obj.byteCapGb !== undefined)
      root.byteCapGb = Number(obj.byteCapGb) || root.byteCapGb
    if (obj.cadenceMs !== undefined)
      root.cadenceMs = Number(obj.cadenceMs) || root.cadenceMs
    if (obj.idlePauseSec !== undefined)
      root.idlePauseSec = Number(obj.idlePauseSec) || root.idlePauseSec
    if (obj.excludeApps !== undefined)
      root.excludeApps = String(obj.excludeApps)
    if (obj.titlePausePatterns !== undefined)
      root.titlePausePatterns = String(obj.titlePausePatterns)
    if (obj.armOnLogin !== undefined)
      root.armOnLogin = !!obj.armOnLogin
    send("configure", root.settingsPayload())
    return "ok"
  }

  function send(cmd, extra) {
    var body = extra && typeof extra === "object" ? extra : {}
    root.nextCmdId += 1
    body.cmd = cmd
    body.id = root.nextCmdId
    var line = JSON.stringify(body)
    var ok = adapter.writeLine(daemonProc, line)
    if (!ok) {
      fallbackWrite(line)
    }
    return body.id
  }

  function fallbackWrite(line) {
    cmdFile.setText(line + "\n")
  }

  function onDaemonLine(line) {
    var ev = Protocol.parseLine(line)
    if (!ev)
      return
    if (ev.event === "ready") {
      root.helperStatus = "ready"
      root.consent = ev.consentShown === true
      root.armed = ev.armed === true
      root.encoder = ev.encoder || root.encoder
      root.lastStatus = "ready"
      send("configure", root.settingsPayload())
      send("stats", {})
      root.publish()
      return
    }
    if (ev.event === "reply") {
      Protocol.mergeStats(root, ev.data || {})
      root.ingestReply(ev)
      root.bumpStats()
      return
    }
    if (ev.event === "stats" || ev.event === "state") {
      Protocol.mergeStats(root, ev)
      root.paused = ev.paused === true || (ev.reason && ev.reason !== "" && ev.reason !== "disarmed" && !ev.armed)
      if (ev.reason !== undefined)
        root.pauseReason = ev.reason || (root.armed ? "" : "disarmed")
      if (ev.armed !== undefined)
        root.armed = ev.armed === true
      if (ev.consent !== undefined)
        root.consent = ev.consent === true
      root.bumpStats()
      return
    }
    if (ev.event === "frame-written") {
      root.frames += 1
      root.framesToday += 1
      if (ev.bytes)
        root.bytesUsed += Number(ev.bytes) || 0
      root.bumpStats()
      if (root.overlayOpen)
        root.refreshTimeline()
      return
    }
    if (ev.event === "ocr-progress") {
      root.lastStatus = "ocr " + (ev.done || 0) + "/" + (ev.queued || 0)
      return
    }
    if (ev.event === "error")
      root.lastStatus = ev.error || "error"
  }

  function ingestReply(ev) {
    var data = ev.data || {}
    if (data.framesToday !== undefined)
      root.framesToday = Number(data.framesToday) || 0
    if (data.bytes !== undefined)
      root.bytesUsed = Number(data.bytes) || 0
    if (data.byteCap !== undefined)
      root.byteCap = Number(data.byteCap) || root.byteCap
    if (data.daysEstimate !== undefined)
      root.daysEstimate = data.daysEstimate
    if (data.encoder)
      root.encoder = data.encoder
    if (data.ocrAvailable !== undefined)
      root.ocrAvailable = data.ocrAvailable === true
    if (data.capture)
      root.captureBackend = data.capture
    if (data.armed !== undefined)
      root.armed = data.armed === true
    if (data.consent !== undefined)
      root.consent = data.consent === true
    if (data.paused !== undefined)
      root.paused = data.paused === true
    if (data.reason !== undefined)
      root.pauseReason = data.reason || ""
    if (data.hits !== undefined) {
      root.hits = data.hits
      root.hitsRevision += 1
    }
    if (data.frames && data.frames.length !== undefined && typeof data.frames !== "number" && data.frames[0] !== undefined && data.frames[0].ts !== undefined) {
      root.timeline = data.frames
      root.gaps = data.gaps || []
      root.timelineRevision += 1
    } else if (typeof data.frames === "number") {
      root.frames = data.frames
    }
    if (data.clips && data.clips.length !== undefined && typeof data.clips !== "string") {
      root.clips = data.clips
      root.clipsRevision += 1
    }
    if (data.frame || data.windows) {
      root.currentMoment = data
      root.momentRevision += 1
    }
    if (data.steps || data.unrecoverable) {
      root.currentPlan = data
      root.planReady = true
    }
    root.publish()
  }

  function bumpStats() {
    root.statsRevision += 1
    root.publish()
  }

  function publish() {
    uiSnap.setText(JSON.stringify(root.statsObject()) + "\n")
    timelineSnap.setText(JSON.stringify({
      frames: root.timeline,
      gaps: root.gaps,
      revision: root.timelineRevision
    }) + "\n")
    clipsSnap.setText(JSON.stringify({
      clips: root.clips,
      revision: root.clipsRevision
    }) + "\n")
    momentSnap.setText(JSON.stringify({
      moment: root.currentMoment,
      revision: root.momentRevision
    }) + "\n")
    hitsSnap.setText(JSON.stringify({
      hits: root.hits,
      revision: root.hitsRevision,
      query: root.lastQuery
    }) + "\n")
    planSnap.setText(JSON.stringify({
      ts: root.lastPlanTs,
      ready: root.planReady,
      plan: root.currentPlan
    }) + "\n")
  }

  function statsObject() {
    return {
      armed: root.armed,
      paused: root.paused,
      reason: root.pauseReason,
      frames: root.frames,
      framesToday: root.framesToday,
      bytes: root.bytesUsed,
      byteCap: root.byteCap,
      daysEstimate: root.daysEstimate,
      encoder: root.encoder,
      ocrAvailable: root.ocrAvailable,
      capture: root.captureBackend,
      consent: root.consent,
      helper: root.helperPath,
      fallback: root.usingFallback,
      status: root.lastStatus
    }
  }

  function arm() {
    if (!root.consent) {
      root.openConsent()
      return "consent"
    }
    send("arm", root.settingsPayload())
    root.armed = true
    root.pauseReason = ""
    root.bumpStats()
    return "ok"
  }

  function disarm() {
    send("disarm", {})
    root.armed = false
    root.pauseReason = "disarmed"
    root.bumpStats()
    return "ok"
  }

  function toggleArm() {
    if (root.armed)
      return root.disarm()
    return root.arm()
  }

  function parseJsonArg(arg, fallback) {
    if (arg === undefined || arg === null || arg === "")
      return fallback
    if (typeof arg === "object")
      return arg
    var s = String(arg)
    try {
      return JSON.parse(s)
    } catch (e) {
      return s
    }
  }

  function consentNow(arg) {
    var armNow = false
    var onLogin = false
    var parsed = root.parseJsonArg(arg, {})
    if (typeof parsed === "boolean")
      armNow = parsed
    else if (typeof parsed === "number")
      armNow = parsed !== 0
    else if (typeof parsed === "string")
      armNow = parsed === "true" || parsed === "1"
    else if (parsed && typeof parsed === "object") {
      armNow = !!(parsed.armNow || parsed.arm_now)
      onLogin = !!(parsed.armOnLogin || parsed.arm_on_login)
    }
    send("consent", { armNow: armNow, armOnLogin: onLogin })
    root.consent = true
    root.armOnLogin = onLogin
    if (armNow) {
      root.armed = true
      root.pauseReason = ""
    }
    root.bumpStats()
    return "ok"
  }

  function setOverlayOpen(open) {
    root.overlayOpen = open === true
    send("set-pause", { reason: "overlay", paused: root.overlayOpen })
    return "ok"
  }

  function refreshTimeline() {
    send("timeline", { limit: 400 })
  }

  function refreshClips() {
    send("clips", { limit: 120 })
  }

  function requestMoment(ts) {
    send("moment", { ts: Number(ts) || 0 })
  }

  function requestQuery(q) {
    root.lastQuery = String(q || "")
    send("query", { q: root.lastQuery, limit: 60 })
  }

  function requestPlan(ts) {
    root.lastPlanTs = Number(ts) || 0
    root.planReady = false
    root.currentPlan = {}
    root.publish()
    send("reopen-plan", { ts: root.lastPlanTs })
  }

  function executePlan(arg) {
    var plan = root.currentPlan || {}
    if (arg !== undefined && arg !== null && arg !== "") {
      var parsed = root.parseJsonArg(arg, null)
      if (parsed && typeof parsed === "object")
        plan = parsed.plan || parsed
    }
    send("reopen-exec", { plan: plan })
    return "ok"
  }

  function copyClip(arg) {
    var ts = 0
    var parsed = root.parseJsonArg(arg, arg)
    if (typeof parsed === "number")
      ts = parsed
    else if (typeof parsed === "string")
      ts = Number(parsed) || 0
    else if (parsed && typeof parsed === "object" && parsed.ts !== undefined)
      ts = Number(parsed.ts) || 0
    send("copy-clip", { ts: ts })
    return "ok"
  }

  function wipe(arg, from, to) {
    var scope = "today"
    var lo = Number(from) || 0
    var hi = Number(to) || 0
    var parsed = root.parseJsonArg(arg, arg)
    if (parsed && typeof parsed === "object" && from === undefined) {
      scope = parsed.scope || "today"
      lo = Number(parsed.from) || 0
      hi = Number(parsed.to) || 0
    } else if (typeof parsed === "string" || typeof arg === "string") {
      scope = String(parsed || arg || "today")
    }
    send("wipe", { scope: scope, from: lo, to: hi })
    return "ok"
  }

  function summonOverlay(payload) {
    return adapter.summon(root.shell, payload || "{}")
  }

  function openConsent() {
    return root.summonOverlay("{\"view\":\"consent\"}")
  }

  function openClips() {
    return root.summonOverlay("{\"view\":\"clips\"}")
  }

  function openTimeline() {
    return root.summonOverlay("{}")
  }

  function ping() { return "ok" }
  function status() { return JSON.stringify(root.statsObject()) }

  function findHelper() {
    var paths = adapter.resolveHelper(root.pluginDir)
    probeProc.command = [
      "sh", "-c",
      "if [ -x \"$1\" ]; then echo binary; elif [ -x \"$2\" ]; then echo fallback; else echo missing; fi",
      "sh", paths.binary, paths.fallback
    ]
    probeProc.running = true
  }

  function startHelper(kind) {
    var paths = adapter.resolveHelper(root.pluginDir)
    if (kind === "binary") {
      root.helperPath = paths.binary
      root.helperIsBinary = true
      root.usingFallback = false
    } else {
      root.helperPath = paths.fallback
      root.helperIsBinary = false
      root.usingFallback = true
    }
    if (!root.helperPath) {
      root.helperStatus = "missing"
      return
    }
    daemonProc.command = [root.helperPath, "daemon"]
    daemonProc.running = true
    root.helperStatus = "starting"
    root.helperReady = true
  }

  Process {
    id: probeProc
    running: false
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var kind = String(text || "").trim()
        if (kind === "binary" || kind === "fallback")
          root.startHelper(kind)
        else {
          root.helperStatus = "missing"
          root.lastStatus = "helper missing"
        }
      }
    }
  }

  Process {
    id: daemonProc
    running: false
    stdinEnabled: true
    stdout: SplitParser {
      splitMarker: "\n"
      onRead: function(data) { root.onDaemonLine(data) }
    }
    stderr: StdioCollector { }
    onStarted: {
      root.helperStatus = "started"
      root.lastStatus = root.usingFallback ? "fallback" : "daemon"
    }
    onExited: function() {
      root.helperStatus = "exited"
      root.restarts += 1
      restartTimer.interval = Math.min(8000, 400 * Math.pow(2, Math.min(root.restarts, 5)))
      restartTimer.restart()
    }
  }

  Timer {
    id: restartTimer
    interval: 800
    repeat: false
    onTriggered: {
      if (root.helperPath)
        daemonProc.running = true
    }
  }

  FileView {
    id: cmdFile
    path: root.dataDir + "/cmd.ndjson"
    atomicWrites: true
    printErrors: false
    watchChanges: false
  }

  FileView { id: uiSnap; path: root.dataDir + "/ui.json"; atomicWrites: true; printErrors: false }
  FileView { id: timelineSnap; path: root.dataDir + "/timeline.json"; atomicWrites: true; printErrors: false }
  FileView { id: clipsSnap; path: root.dataDir + "/clips.json"; atomicWrites: true; printErrors: false }
  FileView { id: momentSnap; path: root.dataDir + "/moment.json"; atomicWrites: true; printErrors: false }
  FileView { id: hitsSnap; path: root.dataDir + "/hits.json"; atomicWrites: true; printErrors: false }
  FileView { id: planSnap; path: root.dataDir + "/plan.json"; atomicWrites: true; printErrors: false }

  Connections {
    target: Hyprland
    function onRawEvent(event) {
      if (!event)
        return
      var name = String(event.name || "")
      if (name === "lock" || name === "sessionlock")
        root.lastStatus = "lock-event"
      if (name === "unlock" || name === "sessionunlock")
        root.lastStatus = "unlock-event"
    }
  }

  IpcHandler {
    target: "io.github.chris.rewind"

    function ping(): string { return "ok" }
    function status(): string { return root.status() }
    function arm(): string { return root.arm() }
    function disarm(): string { return root.disarm() }
    function toggleArm(): string { return root.toggleArm() }
    function summon(): string { return root.openTimeline() }
    function openTimeline(): string { return root.openTimeline() }
    function openClips(): string { return root.openClips() }
    function openConsent(): string { return root.openConsent() }
    function query(q: string): string { root.requestQuery(q); return "ok" }
    function consentNow(arg: string): string { return root.consentNow(arg) }
    function copyClip(arg: string): string { return root.copyClip(arg) }
    function executePlan(arg: string): string { return root.executePlan(arg) }
    function wipe(arg: string): string { return root.wipe(arg) }
    function reopenPlan(arg: string): string { root.requestPlan(Number(arg) || 0); return "ok" }
    function moment(arg: string): string { root.requestMoment(Number(arg) || 0); return "ok" }
    function timeline(arg: string): string { root.refreshTimeline(); return "ok" }
    function clips(arg: string): string { root.refreshClips(); return "ok" }
    function configure(arg: string): string { return root.applySettings(root.parseJsonArg(arg, {})) }
    function setOverlayOpen(arg: string): string {
      var v = String(arg || "")
      return root.setOverlayOpen(v === "true" || v === "1")
    }
  }

  Component.onCompleted: {
    root.findHelper()
  }
}
