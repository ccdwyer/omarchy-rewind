import QtQuick
import Quickshell
import Quickshell.Hyprland

Item {
  id: adapter

  readonly property string pluginId: "io.github.chris.rewind"

  function pluginDirFrom(url) {
    var u = String(url || Qt.resolvedUrl("."))
    if (u.indexOf("file://") === 0)
      u = u.slice(7)
    if (u.length > 1 && u.charAt(u.length - 1) === "/")
      u = u.slice(0, u.length - 1)
    return u
  }

  function homeDir() {
    try {
      return Quickshell.env("HOME") || "/tmp"
    } catch (e) {
      return "/tmp"
    }
  }

  function dataDir() {
    try {
      var override = Quickshell.env("REWIND_DATA_DIR")
      if (override && override.length)
        return override
      var xdg = Quickshell.env("XDG_DATA_HOME")
      if (xdg && xdg.length)
        return xdg + "/rewind"
    } catch (e) {}
    return homeDir() + "/.local/share/rewind"
  }

  function resolveHelper(dir) {
    return {
      binary: dir + "/bin/rewindd",
      fallback: dir + "/compat/rewindd.sh"
    }
  }

  function writeLine(proc, line) {
    if (!proc || !line)
      return false
    try {
      if (typeof proc.write === "function") {
        proc.write(String(line) + "\n")
        return true
      }
    } catch (e) {}
    return false
  }

  function summon(shell, payload) {
    var body = payload || "{}"
    if (shell && typeof shell.summon === "function") {
      shell.summon(pluginId, body)
      return "ok"
    }
    try {
      Quickshell.execDetached(["omarchy-shell", "shell", "summon", pluginId, body])
      return "ok"
    } catch (e) {
      return "failed"
    }
  }

  function hide(shell) {
    if (shell && typeof shell.hide === "function") {
      shell.hide(pluginId)
      return "ok"
    }
    try {
      Quickshell.execDetached(["omarchy-shell", "shell", "hide", pluginId])
      return "ok"
    } catch (e) {
      return "failed"
    }
  }

  function dispatchHypr(request) {
    if (!request)
      return false
    try {
      Hyprland.dispatch(request)
      return true
    } catch (e) {
      try {
        Quickshell.execDetached(["hyprctl", "dispatch", request])
        return true
      } catch (e2) {
        return false
      }
    }
  }

  function reduceMotion() {
    try {
      if (Style && Style.reduceMotion)
        return true
    } catch (e) {}
    try {
      if (Quickshell.env("OMARCHY_REDUCED_MOTION") === "1")
        return true
    } catch (e2) {}
    return false
  }
}
