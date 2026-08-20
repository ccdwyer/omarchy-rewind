import QtQuick
import Quickshell
import qs.Ui
import "js/Format.js" as Format
import "js/Pause.js" as Pause

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

  readonly property var rewindService: {
    if (bar && bar.shell && typeof bar.shell.serviceFor === "function") {
      var a = bar.shell.serviceFor(root.moduleName)
      if (a)
        return a
    }
    if (bar && bar.shell && typeof bar.shell.firstPartyServiceFor === "function") {
      var b = bar.shell.firstPartyServiceFor(root.moduleName)
      if (b)
        return b
    }
    return null
  }

  readonly property bool armed: rewindService ? rewindService.armed === true : false
  readonly property bool paused: rewindService ? rewindService.paused === true : false
  readonly property string pauseReason: rewindService ? String(rewindService.pauseReason || "") : "disarmed"
  readonly property int framesToday: rewindService ? Number(rewindService.framesToday || 0) : 0
  readonly property double bytesUsed: rewindService ? Number(rewindService.bytesUsed || 0) : 0
  readonly property bool opened: false

  function open() { root.summonOverlay("{}") }
  function close() {}
  function toggle() { root.toggleArm() }

  function pushSettings() {
    if (rewindService && typeof rewindService.applySettings === "function") {
      rewindService.applySettings({
        byteCapGb: root.byteCapGb,
        cadenceMs: root.cadenceMs,
        idlePauseSec: root.idlePauseSec,
        excludeApps: root.excludeApps,
        titlePausePatterns: root.titlePausePatterns,
        armOnLogin: root.armOnLogin
      })
    }
  }

  function toggleArm() {
    if (rewindService && typeof rewindService.toggleArm === "function") {
      rewindService.toggleArm()
      return
    }
    Quickshell.execDetached(["omarchy-shell", "shell", "call", root.moduleName, "toggleArm"])
  }

  function summonOverlay(payload) {
    if (rewindService && typeof rewindService.summonOverlay === "function") {
      rewindService.summonOverlay(payload || "{}")
      return
    }
    if (bar && bar.shell && typeof bar.shell.summon === "function") {
      bar.shell.summon(root.moduleName, payload || "{}")
      return
    }
    Quickshell.execDetached(["omarchy-shell", "shell", "summon", root.moduleName, payload || "{}"])
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

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
      onPressed: function(mouse) {
        if (mouse.button === Qt.LeftButton) {
          root.holdFired = false
          holdTimer.restart()
        } else if (mouse.button === Qt.RightButton) {
          root.summonOverlay("{}")
        }
      }
      onReleased: function(mouse) {
        if (mouse.button === Qt.LeftButton) {
          holdTimer.stop()
          if (!root.holdFired)
            root.toggleArm()
        }
      }
      onPressAndHold: {
        root.holdFired = true
        holdTimer.stop()
        root.summonOverlay("{\"view\":\"clips\"}")
      }
    }
  }

  Timer {
    id: holdTimer
    interval: 450
    repeat: false
    onTriggered: {
      root.holdFired = true
      root.summonOverlay("{\"view\":\"clips\"}")
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
