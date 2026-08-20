import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import qs.Commons
import qs.Ui
import "js/Format.js" as Format
import "js/Pause.js" as Pause
import "js/Plan.js" as Plan
import "js/Query.js" as Query
import "js/Channel.js" as Channel

Item {
  id: root

  property var shell: null
  property var manifest: null
  property var pluginRegistry: null
  property string omarchyPath: Quickshell.env("OMARCHY_PATH") || ""
  property string pluginId: "io.github.chris.rewind"
  property bool opened: false
  property string view: "scrub"
  property string searchText: ""
  property bool searchOpen: false
  property int selectedIndex: 0
  // When a fresh overlay open loads the timeline, jump to the newest frame
  // (the latest moment) rather than sitting on the oldest loaded one.
  property bool selectLatestOnLoad: false
  property var frames: []
  property var gaps: []
  property var hits: []
  property var clips: []
  property var moment: ({})
  // When a search hit is OLDER than the loaded (newest-N) timeline window, the
  // timeline strip has no frame for it. In that case the loaded `moment` (which
  // the helper returns for ANY ts, with an absolute path) becomes the
  // authoritative displayed frame, so search jumps to the real matched
  // screenshot instead of stranding on a recent one. Cleared the moment the
  // user scrubs the strip again.
  property bool momentAuthoritative: false
  property var plan: ({})
  property bool showPlan: false
  property bool showWipe: false
  property string wipeScope: "today"
  property double wipeFromTs: 0
  property double wipeToTs: 0
  property var ipcQueue: []
  property bool ipcBusy: false
  property var highlightBoxes: []
  property bool firstRun: false
  property int seenHitsRevision: -1
  property bool awaitingSearch: false
  property int planRequestTs: 0
  property bool planReady: false
  property bool uiConsent: false
  property bool uiArmed: false
  property string uiPauseReason: "disarmed"
  property bool uiOcr: false
  property string openView: ""
  property double uiByteCap: 2147483648
  property var uiDaysEstimate: null
  property double uiFirstTs: 0
  property double uiLastTs: 0

  RewindAdapter { id: adapter }
  readonly property string dataDir: adapter.dataDir()
  readonly property string snapDir: adapter.snapDir()

  property color background: Color.menu.background
  property color foreground: Color.menu.text
  property color border: Color.menu.border
  property color scrim: Color.menu.scrim
  property color accent: Color.accent
  property var borderSpec: Border.surfaceSpec("menu", "border", border, Math.max(1, Style.space(2)))
  readonly property int cornerRadius: Style.cornerRadius
  property string fontFamily: Style.font.menuFamily
  readonly property bool reduceMotion: {
    try { if (Style && Style.reduceMotion) return true } catch (e) {}
    try { if (Quickshell.env("OMARCHY_REDUCED_MOTION") === "1") return true } catch (e2) {}
    return false
  }
  readonly property int motionMs: reduceMotion ? 0 : 140

  function callSvc(name, arg) {
    var payload = arg
    if (arg !== undefined && arg !== null && typeof arg === "object")
      payload = JSON.stringify(arg)
    var cmd = ["omarchy-shell", root.pluginId, name]
    if (payload !== undefined && payload !== null && String(payload).length)
      cmd.push(String(payload))
    var q = []
    for (var i = 0; i < root.ipcQueue.length; i++)
      q.push(root.ipcQueue[i])
    q.push(cmd)
    root.ipcQueue = q
    root.kickIpc()
    return "queued"
  }

  function kickIpc() {
    if (root.ipcBusy)
      return
    if (!root.ipcQueue.length)
      return
    if (ipcProc.running)
      return
    var cmd = root.ipcQueue[0]
    var rest = []
    for (var i = 1; i < root.ipcQueue.length; i++)
      rest.push(root.ipcQueue[i])
    root.ipcQueue = rest
    root.ipcBusy = true
    ipcProc.command = cmd
    ipcProc.running = true
  }

  function onCallReply(text) {
    var raw = String(text || "").trim()
    if (raw && raw.charAt(0) === "{") {
      try {
        var ev = JSON.parse(raw)
        if (ev.hits !== undefined) {
          root.hits = ev.hits
          root.applySearchHits()
        }
        if (ev.steps || ev.unrecoverable) {
          if (!root.planRequestTs || Number(ev.ts || root.planRequestTs) === Number(root.planRequestTs)) {
            root.plan = ev.plan || ev
            root.planReady = true
          }
        }
        if (ev.frame || ev.windows)
          root.moment = ev.moment || ev
        if (ev.wiped !== undefined) {
          root.highlightBoxes = []
          root.hits = []
          root.moment = {}
          root.frames = []
          root.clips = []
          root.gaps = []
          root.selectedIndex = 0
          root.plan = {}
          root.planReady = false
        }
        if (ev.armed !== undefined || ev.consent !== undefined) {
          root.applyUi(ev)
          if (root.opened) {
            if (root.openView === "clips")
              root.view = "clips"
            else
              root.view = Channel.overlayViewAfterRefresh(root.uiConsent, root.openView)
          }
        }
      } catch (e) {}
    }
    uiView.reload()
    timelineView.reload()
    clipsView.reload()
    momentView.reload()
    hitsView.reload()
    planView.reload()
  }

  function focusForView() {
    if (root.view === "clips")
      clipList.forceActiveFocus()
    else
      keyCatcher.forceActiveFocus()
  }

  function open(payloadJson) {
    root.opened = true
    root.showPlan = false
    root.showWipe = false
    root.searchText = ""
    root.searchOpen = false
    root.view = "scrub"
    var payload = {}
    try {
      payload = payloadJson && String(payloadJson).length ? JSON.parse(payloadJson) : {}
    } catch (e) { payload = {} }
    root.openView = payload.view || ""
    root.selectLatestOnLoad = true
    root.firstRun = !root.uiConsent
    if (payload.view === "clips")
      root.view = "clips"
    else if (payload.view === "consent" || root.firstRun)
      root.view = "consent"
    root.callSvc("refresh", { overlay: true })
    root.pull()
    Qt.callLater(root.focusForView)
  }

  function close() {
    root.callSvc("setOverlayOpen", "false")
    root.opened = false
    root.showPlan = false
  }

  function toggle() {
    if (root.opened)
      root.close()
    else
      root.open("{}")
  }

  // `shell call <id> toggleArm` lands on this overlay. Forward to the service
  // IpcHandler so leftover binds still arm/disarm instead of returning unknown.
  function toggleArm(arg) {
    var payload = arg === undefined || arg === null || String(arg).length === 0 ? "{}" : arg
    return root.callSvc("toggleArm", payload)
  }

  function installBinds(arg) {
    var payload = arg === undefined || arg === null ? "" : String(arg)
    return root.callSvc("installBinds", payload)
  }

  function pull() {
    root.applyUi(uiView.text ? uiView.text() : "")
    root.applyTimeline(timelineView.text ? timelineView.text() : "")
    root.applyClips(clipsView.text ? clipsView.text() : "")
    root.applyMoment(momentView.text ? momentView.text() : "")
    root.applyHits(hitsView.text ? hitsView.text() : "")
    root.applyPlanFile(planView.text ? planView.text() : "")
    if (root.frames.length) {
      if (root.selectedIndex >= root.frames.length)
        root.selectedIndex = root.frames.length - 1
    }
  }

  function applyUi(raw) {
    var u = raw && typeof raw === "object" ? raw : Channel.parse(raw, {})
    var live = Channel.applyLiveUi({
      armed: root.uiArmed,
      consent: root.uiConsent,
      reason: root.uiPauseReason,
      ocrAvailable: root.uiOcr,
      byteCap: root.uiByteCap,
      daysEstimate: root.uiDaysEstimate,
      firstTs: root.uiFirstTs,
      lastTs: root.uiLastTs
    }, u)
    root.uiArmed = live.armed === true
    root.uiConsent = live.consent === true
    root.uiPauseReason = live.reason || (root.uiArmed ? "" : "disarmed")
    root.uiOcr = live.ocrAvailable === true
    if (live.byteCap !== undefined)
      root.uiByteCap = Number(live.byteCap) || root.uiByteCap
    root.uiDaysEstimate = live.daysEstimate
    if (live.firstTs !== undefined)
      root.uiFirstTs = Number(live.firstTs) || 0
    if (live.lastTs !== undefined)
      root.uiLastTs = Number(live.lastTs) || 0
    root.firstRun = !root.uiConsent
  }

  function applyTimeline(raw) {
    var t = Channel.parse(raw, {})
    root.frames = Channel.arrayOf(t.frames)
    root.gaps = Channel.arrayOf(t.gaps)
    if (!root.frames.length) {
      root.moment = {}
      root.highlightBoxes = []
    } else if (root.selectLatestOnLoad) {
      root.selectedIndex = root.frames.length - 1
      root.selectLatestOnLoad = false
      root.momentAuthoritative = false
    } else if (root.selectedIndex >= root.frames.length) {
      root.selectedIndex = root.frames.length - 1
    }
  }

  function applyClips(raw) {
    var c = Channel.parse(raw, {})
    root.clips = Channel.arrayOf(c.clips)
  }

  function applyMoment(raw) {
    var m = Channel.parse(raw, {})
    root.moment = m.moment || m
  }

  function applyHits(raw) {
    var h = Channel.parse(raw, {})
    var rev = h.revision !== undefined ? Number(h.revision) : -1
    root.hits = Channel.arrayOf(h.hits)
    if (root.awaitingSearch && rev !== root.seenHitsRevision && rev >= 0) {
      root.seenHitsRevision = rev
      root.awaitingSearch = false
      root.applySearchHits()
    }
  }

  function applyPlanFile(raw) {
    var p = Channel.parse(raw, {})
    if (!root.showPlan)
      return
    if (p.ts !== undefined && Number(p.ts) !== Number(root.planRequestTs))
      return
    if (p.ready === true) {
      root.plan = p.plan || {}
      root.planReady = true
    }
  }

  function currentFrame() {
    if (!root.frames.length)
      return null
    return root.frames[Math.max(0, Math.min(root.selectedIndex, root.frames.length - 1))]
  }

  // The frame actually shown/labeled. When a search hit lies outside the loaded
  // timeline window, the authoritative moment's own frame is displayed so scrub
  // jumps to the matched screenshot; otherwise the selected timeline frame.
  function displayFrame() {
    if (root.momentAuthoritative && root.moment && root.moment.frame && root.moment.frame.path)
      return root.moment.frame
    return root.currentFrame()
  }

  function ensureMoment() {
    var f = root.currentFrame()
    if (!f)
      return
    root.callSvc("moment", String(f.ts))
  }

  function stepFrame(delta) {
    if (!root.frames.length)
      return
    // Scrubbing the strip returns authority to the timeline frame.
    root.momentAuthoritative = false
    var next = root.selectedIndex + delta
    if (next < 0) next = 0
    if (next > root.frames.length - 1) next = root.frames.length - 1
    root.selectedIndex = next
    root.ensureMoment()
  }

  function stepMinute(dir) {
    root.momentAuthoritative = false
    var f = root.currentFrame()
    if (!f)
      return
    var target = Number(f.ts) + dir * 60000
    var best = root.selectedIndex
    var bestDist = 1e15
    for (var i = 0; i < root.frames.length; i++) {
      var d = Math.abs(Number(root.frames[i].ts) - target)
      if (d < bestDist) {
        bestDist = d
        best = i
      }
    }
    root.selectedIndex = best
    root.ensureMoment()
  }

  function runSearch() {
    var cur = Channel.parse(hitsView.text ? hitsView.text() : "", {})
    root.seenHitsRevision = cur.revision !== undefined ? Number(cur.revision) : -1
    root.awaitingSearch = true
    root.callSvc("query", root.searchText)
  }

  function applySearchHits() {
    var hits = root.hits || []
    if (!hits.length) {
      root.highlightBoxes = []
      return
    }
    var ts = Number(hits[0].ts)
    var found = -1
    for (var i = 0; i < root.frames.length; i++) {
      if (Number(root.frames[i].ts) === ts) {
        found = i
        break
      }
    }
    if (found >= 0) {
      // Hit is within the loaded timeline window: select its strip frame.
      root.selectedIndex = found
      root.momentAuthoritative = false
    } else {
      // Hit is older than the newest-N window and has no strip frame. Display
      // the loaded moment's own frame (moment() returns any ts with an absolute
      // path) so the search jumps to the actual matched screenshot rather than
      // stranding on a recent frame while showing old metadata/highlights.
      root.momentAuthoritative = true
    }
    root.highlightBoxes = (hits[0].boxes || []).slice()
    root.callSvc("moment", String(ts))
  }

  function copyCurrentClip() {
    var f = root.currentFrame()
    var ts = f ? f.ts : 0
    var clip = root.moment && root.moment.clip
    if (!ts && root.clips.length)
      ts = root.clips[0].ts
    root.callSvc("copyClip", String(ts))
  }

  function copySelectedClip() {
    if (!root.clips.length)
      return
    var idx = clipList.currentIndex
    if (idx < 0 || idx >= root.clips.length)
      idx = 0
    root.callSvc("copyClip", String(root.clips[idx].ts))
  }

  function askPlan() {
    var f = root.currentFrame()
    if (!f)
      return
    root.plan = {}
    root.planReady = false
    root.planRequestTs = Number(f.ts)
    root.showPlan = true
    root.callSvc("reopenPlan", String(f.ts))
  }

  function askOneWindow(win) {
    if (!win)
      return
    // The reopen plan (including the launch command) is built by the helper
    // from the desktop-file map + live clients — the UI must not fabricate a
    // command from stored window data that has no exec/cmd. Request it async
    // and show it for confirmation once it arrives.
    var ts = Number(win.ts || (root.currentFrame() ? root.currentFrame().ts : 0))
    var target = {
      "address": String(win.address || ""),
      "class": String(win.class || win.app || ""),
      "title": String(win.title || "")
    }
    root.plan = {}
    root.planReady = false
    root.planRequestTs = ts
    root.showPlan = true
    root.callSvc("reopenWindow", { "ts": ts, "target": target })
  }

  function execPlan() {
    root.callSvc("executePlan", root.plan || {})
    root.showPlan = false
  }

  function openWipe(scope) {
    root.wipeScope = scope
    if (scope === "range") {
      var lo = Number(root.uiFirstTs) || 0
      var hi = Number(root.uiLastTs) || 0
      if (root.frames.length) {
        var a = Number(root.frames[0].ts)
        var b = Number(root.frames[root.frames.length - 1].ts)
        if (a > b) {
          var swap = a
          a = b
          b = swap
        }
        if (!lo || a < lo)
          lo = a
        if (!hi || b > hi)
          hi = b
      }
      var cur = root.currentFrame()
      root.wipeFromTs = lo
      root.wipeToTs = cur ? Number(cur.ts) : hi
      if (root.wipeToTs < root.wipeFromTs) {
        var tmp = root.wipeFromTs
        root.wipeFromTs = root.wipeToTs
        root.wipeToTs = tmp
      }
    }
    root.showWipe = true
  }

  function setWipeBound(which) {
    var f = root.currentFrame()
    if (!f)
      return
    if (which === "from")
      root.wipeFromTs = Number(f.ts)
    else
      root.wipeToTs = Number(f.ts)
  }

  function doWipe() {
    root.highlightBoxes = []
    root.hits = []
    root.moment = {}
    root.momentAuthoritative = false
    root.plan = {}
    root.planReady = false
    root.frames = []
    root.clips = []
    root.gaps = []
    root.selectedIndex = 0
    if (root.wipeScope === "range")
      root.callSvc("wipe", { scope: "range", from: root.wipeFromTs, to: root.wipeToTs })
    else
      root.callSvc("wipe", { scope: root.wipeScope })
    root.callSvc("refresh", { overlay: true })
    root.showWipe = false
  }

  function isHit(ts) {
    for (var i = 0; i < root.hits.length; i++) {
      if (Number(root.hits[i].ts) === Number(ts))
        return true
    }
    return false
  }

  Process {
    id: ipcProc
    running: false
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: root.onCallReply(text)
    }
    onExited: {
      root.ipcBusy = false
      root.kickIpc()
    }
  }

  FileView {
    id: uiView
    path: root.snapDir.length ? (root.snapDir + "/ui.json") : ""
    watchChanges: true
    printErrors: false
    onLoaded: root.applyUi(text())
    onFileChanged: reload()
  }
  FileView {
    id: timelineView
    path: root.snapDir.length ? (root.snapDir + "/timeline.json") : ""
    watchChanges: true
    printErrors: false
    onLoaded: root.applyTimeline(text())
    onFileChanged: reload()
  }
  FileView {
    id: clipsView
    path: root.snapDir.length ? (root.snapDir + "/clips.json") : ""
    watchChanges: true
    printErrors: false
    onLoaded: root.applyClips(text())
    onFileChanged: reload()
  }
  FileView {
    id: momentView
    path: root.snapDir.length ? (root.snapDir + "/moment.json") : ""
    watchChanges: true
    printErrors: false
    onLoaded: root.applyMoment(text())
    onFileChanged: reload()
  }
  FileView {
    id: hitsView
    path: root.snapDir.length ? (root.snapDir + "/hits.json") : ""
    watchChanges: true
    printErrors: false
    onLoaded: root.applyHits(text())
    onFileChanged: reload()
  }
  FileView {
    id: planView
    path: root.snapDir.length ? (root.snapDir + "/plan.json") : ""
    watchChanges: true
    printErrors: false
    onLoaded: root.applyPlanFile(text())
    onFileChanged: reload()
  }

  Timer {
    interval: root.opened ? 220 : 800
    running: root.opened
    repeat: true
    onTriggered: root.pull()
  }

  PanelWindow {
    id: panel
    visible: root.opened
    anchors { top: true; bottom: true; left: true; right: true }
    color: "transparent"
    WlrLayershell.namespace: "rewind"
    WlrLayershell.layer: WlrLayer.Overlay
    WlrLayershell.keyboardFocus: WlrKeyboardFocus.Exclusive
    exclusionMode: ExclusionMode.Ignore

    Rectangle {
      anchors.fill: parent
      color: root.scrim
      opacity: root.opened ? 1 : 0
      Behavior on opacity { NumberAnimation { duration: root.motionMs } }
    }

    Item {
      id: keyCatcher
      anchors.fill: parent
      focus: true
      Keys.priority: Keys.BeforeItem
      Keys.onPressed: function(event) {
        if (!root.opened) return
        if (event.key === Qt.Key_Escape) {
          if (root.showPlan) root.showPlan = false
          else if (root.showWipe) root.showWipe = false
          else if (root.searchOpen) { root.searchOpen = false; searchField.focus = false }
          else root.close()
          event.accepted = true
        } else if (event.key === Qt.Key_Slash && !root.searchOpen && root.view !== "consent") {
          root.searchOpen = true
          searchField.forceActiveFocus()
          event.accepted = true
        } else if (event.key === Qt.Key_Left && root.view === "scrub") {
          root.stepFrame(event.modifiers & Qt.ShiftModifier ? 0 : -1)
          if (event.modifiers & Qt.ShiftModifier) root.stepMinute(-1)
          event.accepted = true
        } else if (event.key === Qt.Key_Right && root.view === "scrub") {
          if (event.modifiers & Qt.ShiftModifier) root.stepMinute(1)
          else root.stepFrame(1)
          event.accepted = true
        } else if (event.key === Qt.Key_Home) {
          root.selectedIndex = 0
          root.ensureMoment()
          event.accepted = true
        } else if (event.key === Qt.Key_End) {
          root.selectedIndex = Math.max(0, root.frames.length - 1)
          root.ensureMoment()
          event.accepted = true
        } else if (event.key === Qt.Key_R && root.view === "scrub") {
          root.askPlan()
          event.accepted = true
        } else if (event.key === Qt.Key_Y) {
          root.copyCurrentClip()
          event.accepted = true
        } else if ((event.key === Qt.Key_Return || event.key === Qt.Key_Enter) && root.view === "clips") {
          root.copySelectedClip()
          event.accepted = true
        } else if (event.key === Qt.Key_Down && root.view === "clips") {
          clipList.currentIndex = Math.min(clipList.count - 1, clipList.currentIndex + 1)
          event.accepted = true
        } else if (event.key === Qt.Key_Up && root.view === "clips") {
          clipList.currentIndex = Math.max(0, clipList.currentIndex - 1)
          event.accepted = true
        } else if (event.key === Qt.Key_C && (event.modifiers & Qt.ControlModifier) === 0 && root.view === "scrub") {
          root.view = "clips"
          Qt.callLater(root.focusForView)
          event.accepted = true
        }
      }
    }

    // ---- consent --------------------------------------------------------
    BorderSurface {
      visible: root.view === "consent"
      width: Math.min(Style.space(640), panel.width - Style.gapsOut * 2)
      height: Math.min(consentCol.implicitHeight + Style.space(48), panel.height - Style.gapsOut * 2)
      radius: root.cornerRadius
      anchors.centerIn: parent
      color: root.background
      borderSpec: root.borderSpec
      opacity: visible ? 1 : 0

      Column {
        id: consentCol
        anchors.fill: parent
        anchors.margins: Style.spacing.panelPadding
        spacing: Style.space(14)

        Text {
          text: "Your day stays on this disk"
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.heading
          font.bold: true
          wrapMode: Text.WordWrap
          width: parent.width
        }
        Text {
          width: parent.width
          wrapMode: Text.WordWrap
          color: root.foreground
          opacity: 0.82
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
          text: "Rewind records the focused screen, clipboard text, and window layout only after you arm it. Nothing is uploaded. There is no account. The bar chip is the truth — if it says disarmed, nothing is written."
        }
        Text {
          width: parent.width
          wrapMode: Text.WordWrap
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
          text: {
            var cap = root.uiByteCap || 2147483648
            return "Planning estimate: ~25–80 KB per 720p frame (not measured on this host). Default cap "
                   + Format.humanBytes(cap) + " · " + Format.daysLabel(root.uiDaysEstimate, cap)
                   + ". Oldest frames go first. Search works on titles and clipboard even if tesseract is not installed."
          }
        }
        Text {
          width: parent.width
          wrapMode: Text.WordWrap
          color: root.foreground
          opacity: 0.75
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          text: "Capture pauses while an excluded app is visible on any output, while a screencast portal is active, while this overlay is open, while the session is locked, and after two minutes idle. Private-window titles (Firefox / Chrome / Brave) are a labeled heuristic."
        }
        Text {
          width: parent.width
          wrapMode: Text.WordWrap
          color: root.foreground
          opacity: 0.7
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          text: "Default exclusions: KeePassXC, 1Password, Bitwarden, Seahorse, polkit agents."
        }
        Row {
          spacing: Style.space(10)
          Rectangle {
            width: 18; height: 18; radius: 3
            color: loginCheck.checked ? root.accent : "transparent"
            border.color: root.foreground
            border.width: 1
            MouseArea {
              id: loginCheck
              anchors.fill: parent
              property bool checked: false
              onClicked: checked = !checked
            }
          }
          Text {
            text: "Arm on login (off until you check it)"
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            anchors.verticalCenter: parent.verticalCenter
          }
        }
        Row {
          spacing: Style.space(12)
          Rectangle {
            width: stayBtn.implicitWidth + Style.space(24)
            height: Style.space(36)
            radius: Style.spacing.labelGap
            color: Style.selectedFillFor(root.foreground, root.accent)
            Text {
              id: stayBtn
              anchors.centerIn: parent
              text: "Keep disarmed"
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
              font.bold: true
            }
            MouseArea {
              anchors.fill: parent
              onClicked: {
                root.callSvc("consentNow", { armNow: false, armOnLogin: loginCheck.checked })
                root.view = "scrub"
                root.firstRun = false
              }
            }
          }
          Rectangle {
            width: armBtn.implicitWidth + Style.space(24)
            height: Style.space(36)
            radius: Style.spacing.labelGap
            color: "transparent"
            border.color: root.accent
            border.width: 1
            Text {
              id: armBtn
              anchors.centerIn: parent
              text: "Arm now"
              color: root.accent
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
              font.bold: true
            }
            MouseArea {
              anchors.fill: parent
              onClicked: {
                root.callSvc("consentNow", { armNow: true, armOnLogin: loginCheck.checked })
                root.view = "scrub"
                root.firstRun = false
              }
            }
          }
        }
      }
    }

    // ---- main scrubber --------------------------------------------------
    Column {
      visible: root.view === "scrub"
      anchors.fill: parent
      anchors.margins: Style.gapsOut
      spacing: Style.space(10)

      Row {
        width: parent.width
        spacing: Style.space(12)
        Text {
          text: "Rewind"
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.heading
          font.bold: true
          anchors.verticalCenter: parent.verticalCenter
        }
        Text {
          text: root.uiArmed ? Pause.reasonLabel(root.uiPauseReason) : "disarmed"
          color: root.foreground
          opacity: 0.7
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
          anchors.verticalCenter: parent.verticalCenter
        }
        Item { width: Style.space(16); height: 1 }
        Rectangle {
          id: searchBox
          width: Math.min(Style.space(420), parent.width * 0.4)
          height: Style.space(32)
          radius: Style.spacing.labelGap
          color: Style.normalFillFor(root.foreground, root.accent)
          border.color: root.searchOpen ? root.accent : root.border
          border.width: 1
          TextInput {
            id: searchField
            anchors.fill: parent
            anchors.margins: Style.space(8)
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            clip: true
            text: root.searchText
            onTextChanged: root.searchText = text
            onAccepted: root.runSearch()
            Keys.onPressed: function(event) {
              if (event.key === Qt.Key_Escape) {
                root.searchOpen = false
                keyCatcher.forceActiveFocus()
                event.accepted = true
              }
            }
          }
          Text {
            visible: searchField.text.length === 0
            anchors.verticalCenter: parent.verticalCenter
            anchors.left: parent.left
            anchors.leftMargin: Style.space(8)
            text: "/ search titles, clipboard, OCR"
            color: root.foreground
            opacity: 0.45
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
          }
        }
        Text {
          text: root.uiOcr ? "" : "OCR off — titles & clipboard still search"
          color: root.foreground
          opacity: 0.55
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          anchors.verticalCenter: parent.verticalCenter
        }
      }

      Row {
        width: parent.width
        height: parent.height - Style.space(168)
        spacing: Style.space(12)

        Item {
          width: parent.width - Style.space(320)
          height: parent.height
          BorderSurface {
            anchors.fill: parent
            radius: root.cornerRadius
            color: root.background
            borderSpec: root.borderSpec
            Image {
              id: frameImage
              anchors.fill: parent
              anchors.margins: Style.space(8)
              fillMode: Image.PreserveAspectFit
              asynchronous: true
              cache: true
              source: {
                var f = root.displayFrame()
                if (f && f.path)
                  return Format.fileUrl(f.path)
                if (root.moment && root.moment.frame && root.moment.frame.path)
                  return Format.fileUrl(root.moment.frame.path)
                return ""
              }
            }
            Repeater {
              model: {
                var boxes = root.highlightBoxes
                if ((!boxes || !boxes.length) && root.moment && root.moment.boxes)
                  boxes = Query.boxesForQuery(root.moment.boxes, root.searchText)
                return boxes || []
              }
              delegate: Rectangle {
                required property var modelData
                color: Qt.rgba(0.2, 0.7, 1, 0.28)
                border.color: root.accent
                border.width: 1
                property var fit: Query.fittedRect(
                  frameImage.width, frameImage.height,
                  frameImage.sourceSize.width, frameImage.sourceSize.height)
                x: frameImage.x + fit.x + (modelData.x || 0) * fit.w
                y: frameImage.y + fit.y + (modelData.y || 0) * fit.h
                width: Math.max(4, (modelData.w || 0) * fit.w)
                height: Math.max(4, (modelData.h || 0) * fit.h)
              }
            }
            Column {
              visible: !root.frames.length
              anchors.centerIn: parent
              width: parent.width * 0.7
              spacing: Style.space(10)
              Text {
                width: parent.width
                text: "No frames yet. Arm Rewind from the bar, work for a few seconds, then come back."
                color: root.foreground
                opacity: 0.7
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
                wrapMode: Text.WordWrap
                horizontalAlignment: Text.AlignHCenter
              }
            }
          }
        }

        BorderSurface {
          width: Style.space(308)
          height: parent.height
          radius: root.cornerRadius
          color: root.background
          borderSpec: root.borderSpec
          Column {
            anchors.fill: parent
            anchors.margins: Style.space(12)
            spacing: Style.space(10)
            Text {
              text: root.displayFrame() ? Format.clockLabel(root.displayFrame().ts) : ""
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.heading
              font.bold: true
            }
            Text {
              width: parent.width
              text: root.displayFrame() ? ((root.displayFrame().app || "") + " — " + (root.displayFrame().title || "")) : ""
              color: root.foreground
              opacity: 0.75
              wrapMode: Text.WordWrap
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
            }
            Text {
              text: "Clipboard"
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
              font.bold: true
            }
            Text {
              width: parent.width
              text: (root.moment && root.moment.clip) ? String(root.moment.clip) : "No clip at this moment"
              color: root.foreground
              opacity: 0.8
              wrapMode: Text.Wrap
              maximumLineCount: 6
              elide: Text.ElideRight
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
            }
            Row {
              spacing: Style.space(8)
              Rectangle {
                width: copyLbl.implicitWidth + Style.space(16)
                height: Style.space(28)
                radius: 4
                color: Style.normalFillFor(root.foreground, root.accent)
                Text { id: copyLbl; anchors.centerIn: parent; text: "y  copy clip"; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
                MouseArea { anchors.fill: parent; onClicked: root.copyCurrentClip() }
              }
              Rectangle {
                width: planLbl.implicitWidth + Style.space(16)
                height: Style.space(28)
                radius: 4
                color: Style.normalFillFor(root.foreground, root.accent)
                Text { id: planLbl; anchors.centerIn: parent; text: "r  reopen & arrange"; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
                MouseArea { anchors.fill: parent; onClicked: root.askPlan() }
              }
            }
            Text {
              text: "Windows"
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
              font.bold: true
            }
            ListView {
              width: parent.width
              height: parent.height - Style.space(220)
              clip: true
              model: (root.moment && root.moment.windows) ? root.moment.windows : []
              delegate: Column {
                required property var modelData
                width: parent.width
                spacing: Style.space(4)
                Text {
                  width: parent.width
                  text: (modelData.class || "") + " · ws " + (modelData.workspace || "") + "\n" + (modelData.title || "")
                  color: root.foreground
                  opacity: 0.8
                  wrapMode: Text.WordWrap
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.bodySmall
                }
                Rectangle {
                  width: reopenOneLab.implicitWidth + Style.space(12)
                  height: Style.space(22)
                  radius: 4
                  color: Style.normalFillFor(root.foreground, root.accent)
                  Text {
                    id: reopenOneLab
                    anchors.centerIn: parent
                    text: "Reopen"
                    color: root.foreground
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.bodySmall
                  }
                  MouseArea {
                    anchors.fill: parent
                    onClicked: root.askOneWindow(modelData)
                  }
                }
              }
            }
            Text {
              text: "Wipe"
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
              font.bold: true
            }
            Row {
              spacing: Style.space(6)
              Rectangle {
                width: wipeTodayLab.implicitWidth + Style.space(12)
                height: Style.space(24)
                radius: 4
                color: Style.normalFillFor(root.foreground, root.accent)
                Text { id: wipeTodayLab; anchors.centerIn: parent; text: "today"; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
                MouseArea { anchors.fill: parent; onClicked: root.openWipe("today") }
              }
              Rectangle {
                width: wipeAllLab.implicitWidth + Style.space(12)
                height: Style.space(24)
                radius: 4
                color: Style.normalFillFor(root.foreground, root.accent)
                Text { id: wipeAllLab; anchors.centerIn: parent; text: "all"; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
                MouseArea { anchors.fill: parent; onClicked: root.openWipe("all") }
              }
              Rectangle {
                width: wipeRangeLab.implicitWidth + Style.space(12)
                height: Style.space(24)
                radius: 4
                color: Style.normalFillFor(root.foreground, root.accent)
                Text { id: wipeRangeLab; anchors.centerIn: parent; text: "range"; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
                MouseArea { anchors.fill: parent; onClicked: root.openWipe("range") }
              }
            }
          }
        }
      }

      BorderSurface {
        width: parent.width
        height: Style.space(132)
        radius: root.cornerRadius
        color: root.background
        borderSpec: root.borderSpec
        Column {
          anchors.fill: parent
          anchors.margins: Style.space(8)
          spacing: Style.space(6)
          Item {
            id: density
            width: parent.width
            height: Style.space(10)
            Row {
              anchors.fill: parent
              spacing: 0
              Repeater {
                model: Math.max(1, Math.min(root.frames.length, 240))
                delegate: Rectangle {
                  required property int index
                  width: density.width / Math.max(1, Math.min(root.frames.length, 240))
                  height: parent.height
                  color: {
                    if (!root.frames.length)
                      return "transparent"
                    var step = Math.max(1, Math.floor(root.frames.length / 240))
                    var f = root.frames[Math.min(index * step, root.frames.length - 1)]
                    var gap = Query.gapReason(f.ts, root.gaps)
                    if (gap === "lock")
                      return Qt.rgba(0.75, 0.2, 0.2, 0.7)
                    if (gap)
                      return Qt.rgba(0, 0, 0, 0.45)
                    if (root.isHit(f.ts))
                      return root.accent
                    return Qt.rgba(root.accent.r, root.accent.g, root.accent.b, 0.45)
                  }
                }
              }
            }
            Repeater {
              model: root.gaps
              delegate: Rectangle {
                required property var modelData
                height: parent.height
                visible: root.frames.length > 1
                x: {
                  var a = Number(root.frames[0].ts)
                  var b = Number(root.frames[root.frames.length - 1].ts)
                  var span = Math.max(1, b - a)
                  return density.width * ((Number(modelData.from) - a) / span)
                }
                width: {
                  var a = Number(root.frames[0].ts)
                  var b = Number(root.frames[root.frames.length - 1].ts)
                  var span = Math.max(1, b - a)
                  return Math.max(2, density.width * ((Number(modelData.to) - Number(modelData.from)) / span))
                }
                color: (modelData.reason === "lock")
                       ? Qt.rgba(0.8, 0.15, 0.15, 0.35)
                       : Qt.rgba(0.05, 0.05, 0.05, 0.4)
              }
            }
          }
          ListView {
            id: strip
            width: parent.width
            height: parent.height - Style.space(18)
            orientation: ListView.Horizontal
            spacing: Style.space(6)
            clip: true
            model: root.frames
            currentIndex: root.selectedIndex
            highlightMoveDuration: root.motionMs
            delegate: Item {
              required property int index
              required property var modelData
              width: Style.space(92)
              height: strip.height
              BorderSurface {
                anchors.fill: parent
                radius: 4
                color: index === root.selectedIndex
                       ? Style.selectedFillFor(root.foreground, root.accent)
                       : Style.normalFillFor(root.foreground, root.accent)
                borderSpec: index === root.selectedIndex
                            ? Border.controlSpec("focus", root.foreground, root.accent)
                            : Border.controlSpec("normal", root.foreground, root.accent)
                Image {
                  anchors.fill: parent
                  anchors.margins: 2
                  source: Format.fileUrl(modelData.path)
                  fillMode: Image.PreserveAspectCrop
                  asynchronous: true
                  cache: true
                  sourceSize.width: Style.space(92)
                  sourceSize.height: strip.height
                }
                Rectangle {
                  visible: root.isHit(modelData.ts)
                  width: 6; height: 6; radius: 3
                  color: root.accent
                  anchors.top: parent.top
                  anchors.horizontalCenter: parent.horizontalCenter
                  anchors.topMargin: 3
                }
                Text {
                  anchors.bottom: parent.bottom
                  anchors.horizontalCenter: parent.horizontalCenter
                  text: Format.clockLabel(modelData.ts)
                  color: root.foreground
                  font.pixelSize: 9
                  font.family: root.fontFamily
                }
              }
              MouseArea {
                anchors.fill: parent
                onClicked: {
                  root.selectedIndex = index
                  root.ensureMoment()
                }
              }
            }
          }
        }
      }
    }

    // ---- clipboard history ----------------------------------------------
    BorderSurface {
      visible: root.view === "clips"
      width: Math.min(Style.space(640), panel.width - Style.gapsOut * 2)
      height: Math.min(Style.space(520), panel.height - Style.gapsOut * 2)
      radius: root.cornerRadius
      anchors.centerIn: parent
      color: root.background
      borderSpec: root.borderSpec
      Column {
        anchors.fill: parent
        anchors.margins: Style.spacing.panelPadding
        spacing: Style.space(10)
        Row {
          width: parent.width
          spacing: Style.space(12)
          Text {
            text: "Clipboard history"
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.heading
            font.bold: true
          }
          Text {
            text: "Enter copies · Esc closes"
            color: root.foreground
            opacity: 0.6
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
            anchors.verticalCenter: parent.verticalCenter
          }
        }
        ListView {
          id: clipList
          width: parent.width
          height: parent.height - Style.space(48)
          clip: true
          model: root.clips
          focus: root.view === "clips"
          Keys.onReturnPressed: root.copySelectedClip()
          Keys.onEnterPressed: root.copySelectedClip()
          delegate: Rectangle {
            required property int index
            required property var modelData
            width: clipList.width
            height: Style.space(56)
            color: index === clipList.currentIndex
                   ? Style.selectedFillFor(root.foreground, root.accent)
                   : "transparent"
            Column {
              anchors.fill: parent
              anchors.margins: Style.space(8)
              Text {
                text: Format.clockLabel(modelData.ts)
                color: root.foreground
                opacity: 0.6
                font.family: root.fontFamily
                font.pixelSize: Style.font.bodySmall
              }
              Text {
                width: parent.width
                text: String(modelData.content || "")
                color: root.foreground
                elide: Text.ElideRight
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
              }
            }
            MouseArea {
              anchors.fill: parent
              onClicked: clipList.currentIndex = index
              onDoubleClicked: root.callSvc("copyClip", String(modelData.ts))
            }
          }
        }
      }
    }

    // ---- reopen plan ----------------------------------------------------
    Rectangle {
      visible: root.showPlan
      anchors.fill: parent
      color: Qt.rgba(0, 0, 0, 0.45)
      MouseArea { anchors.fill: parent; onClicked: root.showPlan = false }
      BorderSurface {
        width: Math.min(Style.space(560), parent.width - Style.gapsOut * 2)
        height: Math.min(Style.space(420), parent.height - Style.gapsOut * 2)
        anchors.centerIn: parent
        radius: root.cornerRadius
        color: root.background
        borderSpec: root.borderSpec
        MouseArea { anchors.fill: parent; onClicked: {} }
        Column {
          anchors.fill: parent
          anchors.margins: Style.spacing.panelPadding
          spacing: Style.space(10)
          Text {
            text: "Reopen & arrange"
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.heading
            font.bold: true
          }
          Text {
            width: parent.width
            wrapMode: Text.WordWrap
            text: !root.planReady
                  ? "Loading plan for this moment…"
                  : ((root.plan && root.plan.note) ? root.plan.note : "Review the plan. This launches missing apps and places windows. It is not session restore.")
            color: root.foreground
            opacity: 0.75
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
          }
          ListView {
            width: parent.width
            height: Style.space(160)
            clip: true
            model: (root.plan && root.plan.steps) ? root.plan.steps : []
            delegate: Text {
              required property var modelData
              text: "· " + Plan.stepLabel(modelData)
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
            }
          }
          Text {
            text: "Cannot recover"
            visible: root.plan && root.plan.unrecoverable && root.plan.unrecoverable.length
            color: root.foreground
            font.bold: true
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
          }
          ListView {
            width: parent.width
            height: Style.space(80)
            clip: true
            visible: root.plan && root.plan.unrecoverable && root.plan.unrecoverable.length
            model: (root.plan && root.plan.unrecoverable) ? root.plan.unrecoverable : []
            delegate: Text {
              required property var modelData
              width: parent.width
              wrapMode: Text.WordWrap
              text: Plan.unrecoverableLabel(modelData)
              color: root.foreground
              opacity: 0.7
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
            }
          }
          Row {
            spacing: Style.space(12)
            Rectangle {
              width: goLbl.implicitWidth + Style.space(20)
              height: Style.space(32)
              radius: 4
              opacity: root.planReady ? 1 : 0.38
              color: Style.selectedFillFor(root.foreground, root.accent)
              Text { id: goLbl; anchors.centerIn: parent; text: root.planReady ? "Confirm" : "Loading plan…"; color: root.foreground; font.bold: true; font.family: root.fontFamily }
              MouseArea { anchors.fill: parent; enabled: root.planReady; onClicked: root.execPlan() }
            }
            Rectangle {
              width: noLbl.implicitWidth + Style.space(20)
              height: Style.space(32)
              radius: 4
              color: "transparent"
              border.color: root.border
              border.width: 1
              Text { id: noLbl; anchors.centerIn: parent; text: "Cancel"; color: root.foreground; font.family: root.fontFamily }
              MouseArea { anchors.fill: parent; onClicked: root.showPlan = false }
            }
          }
        }
      }
    }

    // ---- wipe confirm ---------------------------------------------------
    Rectangle {
      visible: root.showWipe
      anchors.fill: parent
      color: Qt.rgba(0, 0, 0, 0.45)
      MouseArea { anchors.fill: parent; onClicked: root.showWipe = false }
      BorderSurface {
        width: Style.space(420)
        height: root.wipeScope === "range" ? Style.space(260) : Style.space(160)
        anchors.centerIn: parent
        radius: root.cornerRadius
        color: root.background
        borderSpec: root.borderSpec
        MouseArea { anchors.fill: parent; onClicked: {} }
        Column {
          anchors.centerIn: parent
          width: parent.width - Style.space(32)
          spacing: Style.space(12)
          Text {
            text: root.wipeScope === "range" ? "Wipe a time range" : ("Wipe " + root.wipeScope + "?")
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.heading
          }
          Column {
            visible: root.wipeScope === "range"
            width: parent.width
            spacing: Style.space(8)
            Text {
              width: parent.width
              wrapMode: Text.WordWrap
              text: "From " + Format.clockLabel(root.wipeFromTs) + "  →  " + Format.clockLabel(root.wipeToTs) + ". Bounds are the full archive (first–last recorded), not only the loaded strip. Scrub to a frame, then set start or end."
              color: root.foreground
              opacity: 0.75
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
            }
            Row {
              spacing: Style.space(8)
              Rectangle {
                width: fromLab.implicitWidth + Style.space(12)
                height: Style.space(24)
                radius: 4
                color: Style.normalFillFor(root.foreground, root.accent)
                Text { id: fromLab; anchors.centerIn: parent; text: "current as start"; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
                MouseArea { anchors.fill: parent; onClicked: root.setWipeBound("from") }
              }
              Rectangle {
                width: toLab.implicitWidth + Style.space(12)
                height: Style.space(24)
                radius: 4
                color: Style.normalFillFor(root.foreground, root.accent)
                Text { id: toLab; anchors.centerIn: parent; text: "current as end"; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
                MouseArea { anchors.fill: parent; onClicked: root.setWipeBound("to") }
              }
            }
          }
          Row {
            spacing: Style.space(12)
            Rectangle {
              width: 88; height: 32; radius: 4
              color: Style.selectedFillFor(root.foreground, root.accent)
              Text { anchors.centerIn: parent; text: "Wipe"; color: root.foreground; font.family: root.fontFamily }
              MouseArea { anchors.fill: parent; onClicked: root.doWipe() }
            }
            Rectangle {
              width: 88; height: 32; radius: 4
              border.color: root.border; border.width: 1; color: "transparent"
              Text { anchors.centerIn: parent; text: "Cancel"; color: root.foreground; font.family: root.fontFamily }
              MouseArea { anchors.fill: parent; onClicked: root.showWipe = false }
            }
          }
        }
      }
    }
  }
}
