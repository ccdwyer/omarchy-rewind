import QtQuick
import Quickshell
import Quickshell.Io
import qs.Ui
import "js/Format.js" as Format
import "js/Pause.js" as Pause
import "js/Channel.js" as Channel

BarWidget {
  id: root
  moduleName: "io.github.chris.rewind"

  property double byteCapGb: 2
  property int cadenceMs: 3000
  property int idlePauseSec: 120
  property string excludeApps: "keepassxc,1Password,1password,Bitwarden,bitwarden,seahorse,polkit-gnome-authentication-agent-1,polkit-kde-authentication-agent-1,lxqt-policykit-agent"
  property string titlePausePatterns: ""
  property bool armOnLogin: false

  property bool holdFired: false
  property bool armed: false
  property bool paused: false
  property string pauseReason: "disarmed"
  property int framesToday: 0
  property double bytesUsed: 0
  readonly property bool opened: false

  RewindAdapter { id: adapter }

  function open() { root.summonOverlay("{}") }
  function close() {}
  function toggle() { root.toggleArm() }

  function callShell(method, arg) {
    var payload = arg === undefined || arg === null || String(arg).length === 0 ? "{}" : String(arg)
    Quickshell.execDetached(["omarchy-shell", "shell", "call", root.moduleName, method, payload])
  }

  function pushSettings() {
    root.callShell("configure", JSON.stringify({
      byteCapGb: root.byteCapGb,
      cadenceMs: root.cadenceMs,
      idlePauseSec: root.idlePauseSec,
      excludeApps: root.excludeApps,
      titlePausePatterns: root.titlePausePatterns,
      armOnLogin: root.armOnLogin
    }))
  }

  function toggleArm() {
    root.callShell("toggleArm", "{}")
  }

  function applyLive(raw) {
    var u = Channel.parse(raw, {})
    if (u.armed !== undefined)
      root.armed = u.armed === true
    if (u.paused !== undefined)
      root.paused = u.paused === true
    if (u.reason !== undefined)
      root.pauseReason = u.reason || (root.armed ? "" : "disarmed")
    if (u.framesToday !== undefined)
      root.framesToday = Number(u.framesToday) || 0
    if (u.bytes !== undefined)
      root.bytesUsed = Number(u.bytes) || 0
  }

  function summonOverlay(payload) {
    Quickshell.execDetached(["omarchy-shell", "shell", "summon", root.moduleName, payload || "{}"])
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  FileView {
    id: uiView
    path: adapter.dataDir() + "/ui.json"
    watchChanges: true
    printErrors: false
    onLoaded: root.applyLive(text())
    onFileChanged: reload()
  }

  Process {
    id: statusProc
    running: false
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: root.applyLive(text)
    }
  }

  Timer {
    interval: 400
    running: true
    repeat: true
    onTriggered: {
      if (statusProc.running)
        return
      statusProc.command = ["omarchy-shell", "shell", "call", root.moduleName, "status", "{}"]
      statusProc.running = true
    }
  }

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: Format.barText({
      armed: root.armed,
      paused: root.paused,
      framesToday: root.framesToday,
      bytes: root.bytesUsed
    })
    tooltipText: root.armed
                 ? (Pause.reasonLabel(root.pauseReason) + " · " + Format.humanBytes(root.bytesUsed) + " · hold for clipboard")
                 : "Rewind disarmed · click to arm · hold for clipboard history"

    Rectangle {
      width: 7
      height: 7
      radius: 4
      anchors.left: parent.left
      anchors.verticalCenter: parent.verticalCenter
      anchors.leftMargin: 6
      color: !root.armed ? "#6b6b6b" : (root.paused ? "#c9a227" : "#3dcc7a")
    }

    MouseArea {
      anchors.fill: parent
      acceptedButtons: Qt.LeftButton | Qt.RightButton
      hoverEnabled: true
      pressAndHoldInterval: 450
      onPressed: function(mouse) {
        if (mouse.button === Qt.LeftButton)
          root.holdFired = false
        else if (mouse.button === Qt.RightButton)
          root.summonOverlay("{}")
      }
      onReleased: function(mouse) {
        if (mouse.button === Qt.LeftButton && !root.holdFired)
          root.toggleArm()
      }
      onPressAndHold: {
        root.holdFired = true
        root.summonOverlay("{\"view\":\"clips\"}")
      }
    }
  }

  Component.onCompleted: root.pushSettings()
  onByteCapGbChanged: root.pushSettings()
  onCadenceMsChanged: root.pushSettings()
  onIdlePauseSecChanged: root.pushSettings()
  onExcludeAppsChanged: root.pushSettings()
  onTitlePausePatternsChanged: root.pushSettings()
  onArmOnLoginChanged: root.pushSettings()
}
