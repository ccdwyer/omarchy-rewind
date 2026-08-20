.pragma library

var KB_LOW = 25
var KB_HIGH = 80
var DEFAULT_CAP = 2147483648

function humanBytes(n) {
    var v = Number(n) || 0
    if (v < 1024)
        return Math.round(v) + " B"
    if (v < 1024 * 1024)
        return (v / 1024).toFixed(v < 10 * 1024 ? 1 : 0) + " KB"
    if (v < 1024 * 1024 * 1024)
        return (v / (1024 * 1024)).toFixed(v < 10 * 1024 * 1024 ? 1 : 0) + " MB"
    return (v / (1024 * 1024 * 1024)).toFixed(2) + " GB"
}

function daysEstimate(bytes, cap, firstTs, now) {
    bytes = Number(bytes) || 0
    cap = Number(cap) || DEFAULT_CAP
    firstTs = Number(firstTs) || 0
    now = Number(now) || Date.now()
    if (bytes < 8192 || cap <= 0 || firstTs <= 0)
        return null
    var elapsed = now - firstTs
    if (elapsed < 60000)
        return null
    var perDay = bytes / (elapsed / 86400000)
    if (perDay <= 0)
        return null
    return cap / perDay
}

function planningDays(cap) {
    cap = Number(cap) || DEFAULT_CAP
    var low = KB_LOW * 1024
    var high = KB_HIGH * 1024
    var framesPerDay = 86400 / 10
    return {
        low: cap / (high * framesPerDay),
        high: cap / (low * framesPerDay)
    }
}

function daysLabel(days, cap) {
    if (days !== null && days !== undefined && !isNaN(Number(days))) {
        var d = Number(days)
        if (d < 1)
            return "≈" + Math.max(1, Math.round(d * 24)) + " h at your usage"
        if (d < 10)
            return "≈" + d.toFixed(1) + " days at your usage"
        return "≈" + Math.round(d) + " days at your usage"
    }
    var band = planningDays(cap)
    return "planning estimate ≈" + Math.round(band.low) + "–" + Math.round(band.high) + " days at ~25–80 KB/frame"
}

function clockLabel(ts) {
    var d = new Date(Number(ts) || 0)
    if (isNaN(d.getTime()) || !ts)
        return ""
    var hh = d.getHours()
    var mm = d.getMinutes()
    var ss = d.getSeconds()
    function pad(n) { return n < 10 ? "0" + n : String(n) }
    return pad(hh) + ":" + pad(mm) + ":" + pad(ss)
}

function dayLabel(ts) {
    var d = new Date(Number(ts) || 0)
    if (isNaN(d.getTime()) || !ts)
        return ""
    return d.toDateString()
}

function barText(stats) {
    var _ = stats
    return "󰑓"
}

function fileUrl(path) {
    if (!path)
        return ""
    var p = String(path)
    if (p.indexOf("file:") === 0)
        return p
    if (p.charAt(0) !== "/")
        return "file://" + p
    return "file://" + p
}
