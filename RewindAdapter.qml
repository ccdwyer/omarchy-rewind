import QtQuick
import Quickshell
import Quickshell.Hyprland
import qs.Commons

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

  // Ephemeral IPC snapshot channel: the service publishes overlay/bar read
  // responses (timeline, clips, moment, search hits, plan, stats) here, and the
  // overlay/bar read them. It lives in the tmpfs runtime dir (XDG_RUNTIME_DIR,
  // per-user 0700, cleared on logout), NOT the persistent data dir — these are
  // transient query responses derived from already-authorized data, never stored
  // observation. Serving them works while a privacy pause (e.g. the overlay's own
  // pause) is active, because reading already-recorded data is not a write of new
  // content. If no runtime dir exists it FAILS CLOSED (returns "" — no channel),
  // never falling back to a world-accessible /tmp or the persistent data dir.
  function snapDir() {
    try {
      var rt = Quickshell.env("XDG_RUNTIME_DIR")
      if (rt && rt.length)
        return rt + "/rewind"
    } catch (e) {}
    // No runtime dir: FAIL CLOSED. XDG_RUNTIME_DIR is a per-user 0700 tmpfs
    // (logind always sets it), so our `rewind` subdir there cannot be
    // pre-created or symlinked by another user. A `/tmp/rewind-$USER` fallback,
    // by contrast, has a world-writable parent an attacker could pre-create as
    // a symlink to siphon these sensitive snapshots — so we do NOT write the
    // read channel anywhere insecure. Returning "" disables the snapshot
    // channel (the overlay/bar simply show no cached data) rather than leak.
    return ""
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

  // `request` must be a Lua dispatcher (`hl.dsp.exec_cmd("…")`), not a classic
  // `exec cmd` string. Hyprland 0.55+ wraps the dispatch argument as
  // `hl.dispatch(<request>)`.
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
