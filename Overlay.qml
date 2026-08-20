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
  property var frames: []
  property var gaps: []
  property var hits: []
  property var clips: []
  property var moment: ({})
  property var plan: ({})
  property bool showPlan: false
  property bool showWipe: false
  property string wipeScope: "today"
  property var highlightBoxes: []
  property bool firstRun: false

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

  function svc() {
    if (pluginRegistry && typeof pluginRegistry.serviceFor === "function") {
      var a = pluginRegistry.serviceFor(root.pluginId)
      if (a) return a
    }
    if (shell && typeof shell.serviceFor === "function") {
      var b = shell.serviceFor(root.pluginId)
      if (b) return b
    }
    if (shell && typeof shell.firstPartyServiceFor === "function") {
      var c = shell.firstPartyServiceFor(root.pluginId)
      if (c) return c
    }
    return null
  }

  function callSvc(name, arg) {
    var s = root.svc()
    if (s && typeof s[name] === "function") {
      if (arg === undefined)
        return s[name]()
      return s[name](arg)
    }
    var cmd = ["omarchy-shell", "shell", "call", root.pluginId, name]
    if (arg !== undefined && arg !== null && String(arg).length)
      cmd.push(String(arg))
    ipcProc.command = cmd
    ipcProc.running = true
    return "queued"
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
    var s = root.svc()
    root.firstRun = !(s && s.consent)
    if (payload.view === "consent" || root.firstRun)
      root.view = "consent"
    else if (payload.view === "clips")
      root.view = "clips"
    if (s && typeof s.setOverlayOpen === "function")
      s.setOverlayOpen(true)
    root.pull()
    Qt.callLater(function() { keyCatcher.forceActiveFocus() })
  }

  function close() {
    var s = root.svc()
    if (s && typeof s.setOverlayOpen === "function")
      s.setOverlayOpen(false)
    root.opened = false
    root.showPlan = false
  }

  function toggle() {
    if (root.opened)
      root.close()
    else
      root.open("{}")
  }

  function pull() {
    var s = root.svc()
    if (!s)
      return
    if (typeof s.refreshTimeline === "function")
      s.refreshTimeline()
    if (typeof s.refreshClips === "function")
      s.refreshClips()
    root.frames = s.timeline || []
    root.gaps = s.gaps || []
    root.clips = s.clips || []
    root.hits = s.hits || []
    root.moment = s.currentMoment || {}
    root.plan = s.currentPlan || {}
    if (root.frames.length) {
      if (root.selectedIndex >= root.frames.length)
        root.selectedIndex = root.frames.length - 1
      root.ensureMoment()
    }
  }

  function currentFrame() {
    if (!root.frames.length)
      return null
    return root.frames[Math.max(0, Math.min(root.selectedIndex, root.frames.length - 1))]
  }

  function ensureMoment() {
    var f = root.currentFrame()
    if (!f)
      return
    var s = root.svc()
    if (s && typeof s.requestMoment === "function")
      s.requestMoment(f.ts)
  }

  function stepFrame(delta) {
    if (!root.frames.length)
      return
    var next = root.selectedIndex + delta
    if (next < 0) next = 0
    if (next > root.frames.length - 1) next = root.frames.length - 1
    root.selectedIndex = next
    root.ensureMoment()
  }

  function stepMinute(dir) {
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
    var s = root.svc()
    if (s && typeof s.requestQuery === "function")
      s.requestQuery(root.searchText)
    if (s)
      root.hits = s.hits || []
    if (root.hits.length) {
      var ts = Number(root.hits[0].ts)
      for (var i = 0; i < root.frames.length; i++) {
        if (Number(root.frames[i].ts) === ts) {
          root.selectedIndex = i
          break
        }
      }
      root.highlightBoxes = (root.hits[0].boxes || []).slice()
      root.ensureMoment()
    } else {
      root.highlightBoxes = []
    }
  }

  function copyCurrentClip() {
    var f = root.currentFrame()
    var ts = f ? f.ts : 0
    var clip = root.moment && root.moment.clip
    if (!ts && root.clips.length)
      ts = root.clips[0].ts
    root.callSvc("copyClip", ts)
  }

  function askPlan() {
    var f = root.currentFrame()
    if (!f)
      return
    var s = root.svc()
    if (s && typeof s.requestPlan === "function")
      s.requestPlan(f.ts)
    root.plan = s && s.currentPlan ? s.currentPlan : root.plan
    root.showPlan = true
  }

  function execPlan() {
    var s = root.svc()
    if (s && typeof s.executePlan === "function")
      s.executePlan(root.plan)
    root.showPlan = false
  }

  function doWipe() {
    root.callSvc("wipe", root.wipeScope)
    root.showWipe = false
    Qt.callLater(root.pull)
  }

  function isHit(ts) {
    for (var i = 0; i < root.hits.length; i++) {
      if (Number(root.hits[i].ts) === Number(ts))
        return true
    }
    return false
  }

  Process { id: ipcProc; running: false }

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
        } else if (event.key === Qt.Key_C && (event.modifiers & Qt.ControlModifier) === 0 && root.view === "scrub") {
          root.view = "clips"
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
            var s = root.svc()
            var cap = s && s.byteCap ? s.byteCap : 2147483648
            return "Measured on real UI: 25–80 KB per 720p frame. Default cap "
                   + Format.humanBytes(cap) + " · " + Format.daysLabel(s ? s.daysEstimate : null, cap)
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
                root.callSvc("consentNow", false)
                var s = root.svc()
                if (s && typeof s.consentNow === "function")
                  s.consentNow(false, loginCheck.checked)
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
                var s = root.svc()
                if (s && typeof s.consentNow === "function")
                  s.consentNow(true, loginCheck.checked)
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
          text: {
            var s = root.svc()
            var armed = s ? s.armed : false
            var reason = s ? s.pauseReason : "disarmed"
            return armed ? Pause.reasonLabel(reason) : "disarmed"
          }
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
          text: root.svc() && root.svc().ocrAvailable ? "" : "OCR off — titles & clipboard still search"
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
                var f = root.currentFrame()
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
                x: frameImage.x + (modelData.x || 0) * frameImage.width
                y: frameImage.y + (modelData.y || 0) * frameImage.height
                width: Math.max(4, (modelData.w || 0) * frameImage.width)
                height: Math.max(4, (modelData.h || 0) * frameImage.height)
              }
            }
            Text {
              visible: !root.frames.length
              anchors.centerIn: parent
              text: "No frames yet. Arm Rewind from the bar, work for a few seconds, then come back."
              color: root.foreground
              opacity: 0.7
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
              wrapMode: Text.WordWrap
              width: parent.width * 0.6
              horizontalAlignment: Text.AlignHCenter
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
              text: root.currentFrame() ? Format.clockLabel(root.currentFrame().ts) : ""
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.heading
              font.bold: true
            }
            Text {
              width: parent.width
              text: root.currentFrame() ? ((root.currentFrame().app || "") + " — " + (root.currentFrame().title || "")) : ""
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
              delegate: Text {
                required property var modelData
                width: parent.width
                text: (modelData.class || "") + " · ws " + (modelData.workspace || "") + "\n" + (modelData.title || "")
                color: root.foreground
                opacity: 0.8
                wrapMode: Text.WordWrap
                font.family: root.fontFamily
                font.pixelSize: Style.font.bodySmall
              }
            }
            Text {
              text: "wipe today  ·  wipe all"
              color: root.foreground
              opacity: 0.55
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
              MouseArea {
                anchors.fill: parent
                onClicked: { root.wipeScope = "today"; root.showWipe = true }
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
          Row {
            id: density
            width: parent.width
            height: Style.space(10)
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
                  if (root.isHit(f.ts))
                    return root.accent
                  return Qt.rgba(root.accent.r, root.accent.g, root.accent.b, 0.45)
                }
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
          Keys.onReturnPressed: {
            if (root.clips.length)
              root.callSvc("copyClip", root.clips[clipList.currentIndex].ts)
          }
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
              onDoubleClicked: root.callSvc("copyClip", modelData.ts)
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
            text: (root.plan && root.plan.note) ? root.plan.note : "Review the plan. This launches missing apps and places windows. It is not session restore."
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
              color: Style.selectedFillFor(root.foreground, root.accent)
              Text { id: goLbl; anchors.centerIn: parent; text: "Confirm"; color: root.foreground; font.bold: true; font.family: root.fontFamily }
              MouseArea { anchors.fill: parent; onClicked: root.execPlan() }
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
      BorderSurface {
        width: Style.space(360)
        height: Style.space(160)
        anchors.centerIn: parent
        radius: root.cornerRadius
        color: root.background
        borderSpec: root.borderSpec
        Column {
          anchors.centerIn: parent
          spacing: Style.space(12)
          Text {
            text: "Wipe " + root.wipeScope + "?"
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.heading
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
