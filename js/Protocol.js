.pragma library

var nextId = 1

function allocId() {
    nextId += 1
    return nextId
}

function command(cmd, extra) {
    var body = extra && typeof extra === "object" ? extra : {}
    body.cmd = cmd
    if (body.id === undefined)
        body.id = allocId()
    return JSON.stringify(body)
}

function parseLine(line) {
    if (!line)
        return null
    var raw = String(line).trim()
    if (!raw)
        return null
    try {
        return JSON.parse(raw)
    } catch (e) {
        return { event: "error", error: "bad-json", raw: raw }
    }
}

function isReply(ev) {
    return ev && ev.event === "reply"
}

function isError(ev) {
    return ev && ev.event === "error"
}

function isFrame(ev) {
    return ev && ev.event === "frame-written"
}

function isStats(ev) {
    return ev && (ev.event === "stats" || ev.event === "state")
}

function mergeStats(target, ev) {
    if (!ev || !target)
        return target
    var src = ev.data && typeof ev.data === "object" ? ev.data : ev
    var keys = ["armed", "paused", "reason", "frames", "framesToday", "bytes",
                "byteCap", "daysEstimate", "encoder", "ocrAvailable", "capture",
                "consent", "version", "firstTs", "lastTs", "status", "fallback", "helper"]
    for (var i = 0; i < keys.length; i++) {
        var k = keys[i]
        if (src[k] !== undefined)
            target[k] = src[k]
    }
    return target
}

function emptyStats() {
    return {
        armed: false,
        paused: false,
        reason: "disarmed",
        frames: 0,
        framesToday: 0,
        bytes: 0,
        byteCap: 2147483648,
        daysEstimate: null,
        encoder: "png",
        ocrAvailable: false,
        capture: "grim",
        consent: false,
        version: "1.0.0",
        firstTs: 0,
        lastTs: 0
    }
}
