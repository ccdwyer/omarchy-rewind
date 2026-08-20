.pragma library

var DEFAULT_EXCLUDES = [
    "keepassxc",
    "1Password",
    "1password",
    "Bitwarden",
    "bitwarden",
    "seahorse",
    "polkit-gnome-authentication-agent-1",
    "polkit-kde-authentication-agent-1",
    "lxqt-policykit-agent",
    "mate-polkit",
    "xfce-polkit"
]

var PRIVATE_MARKERS = [
    { browser: "firefox", marker: "(Private Browsing)" },
    { browser: "firefox", marker: "Private Browsing" },
    { browser: "librewolf", marker: "Private Browsing" },
    { browser: "google-chrome", marker: "(Incognito)" },
    { browser: "google-chrome", marker: "Incognito" },
    { browser: "chromium", marker: "(Incognito)" },
    { browser: "chromium", marker: "Incognito" },
    { browser: "chrome", marker: "Incognito" },
    { browser: "brave-browser", marker: "Private" },
    { browser: "brave", marker: "Private Window" },
    { browser: "brave", marker: "Private" }
]

function splitCsv(s) {
    if (!s)
        return []
    if (Array.isArray(s))
        return s.slice()
    var parts = String(s).split(",")
    var out = []
    for (var i = 0; i < parts.length; i++) {
        var t = parts[i].trim()
        if (t)
            out.push(t)
    }
    return out
}

function matchesClass(className, exclude) {
    var c = String(className || "").toLowerCase()
    for (var i = 0; i < exclude.length; i++) {
        var e = String(exclude[i] || "").toLowerCase()
        if (e && (c === e || c.indexOf(e) >= 0 || e.indexOf(c) >= 0))
            return true
    }
    return false
}

function clientVisible(c) {
    if (!c)
        return false
    if (c.mapped === false)
        return false
    if (c.hidden === true)
        return false
    return true
}

function excludedVisible(clients, exclude) {
    var list = exclude && exclude.length ? exclude : DEFAULT_EXCLUDES
    for (var i = 0; i < clients.length; i++) {
        var c = clients[i]
        if (clientVisible(c) && matchesClass(c.class, list))
            return c.class
    }
    return null
}

function privateBrowsing(clients) {
    for (var i = 0; i < clients.length; i++) {
        var c = clients[i]
        if (!clientVisible(c))
            continue
        var cls = String(c.class || "").toLowerCase()
        var title = String(c.title || "")
        for (var j = 0; j < PRIVATE_MARKERS.length; j++) {
            var m = PRIVATE_MARKERS[j]
            if (cls.indexOf(m.browser) >= 0 && title.indexOf(m.marker) >= 0) {
                return {
                    class: c.class,
                    title: c.title,
                    marker: m.marker,
                    heuristic: true
                }
            }
        }
    }
    return null
}

function titlePause(clients, patterns) {
    var list = splitCsv(patterns)
    if (!list.length)
        return null
    for (var i = 0; i < clients.length; i++) {
        var c = clients[i]
        if (!clientVisible(c))
            continue
        var hay = (String(c.class || "") + " " + String(c.title || "")).toLowerCase()
        for (var j = 0; j < list.length; j++) {
            var p = String(list[j] || "").toLowerCase()
            if (p && hay.indexOf(p) >= 0)
                return list[j]
        }
    }
    return null
}

function evaluate(input) {
    if (!input || !input.armed)
        return "disarmed"
    if (input.locked)
        return "locked"
    if (input.overlayOpen)
        return "overlay"
    if (input.portalActive)
        return "portal"
    if (input.excluded)
        return "excluded"
    if (input.privateBrowsing)
        return "private-browsing"
    if (input.titlePause)
        return "title-pattern"
    var idleLimit = input.idleLimitMs || 120000
    if (idleLimit > 0 && (input.idleMs || 0) >= idleLimit)
        return "idle"
    return null
}

function reasonLabel(reason) {
    if (reason === "disarmed")
        return "disarmed"
    if (reason === "locked")
        return "paused · lock"
    if (reason === "idle")
        return "paused · idle"
    if (reason === "overlay")
        return "paused · overlay"
    if (reason === "portal")
        return "paused · screencast"
    if (reason === "excluded")
        return "paused · excluded app"
    if (reason === "private-browsing")
        return "paused · private window (heuristic)"
    if (reason === "title-pattern")
        return "paused · title pattern"
    if (reason)
        return "paused · " + reason
    return "recording"
}
