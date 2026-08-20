.pragma library

function parse(raw, fallback) {
    if (!raw)
        return fallback || {}
    try {
        var v = JSON.parse(String(raw))
        if (v && typeof v === "object")
            return v
    } catch (e) {}
    return fallback || {}
}

function arrayOf(v) {
    if (!v)
        return []
    if (v.length !== undefined && typeof v !== "string")
        return v
    return []
}

function applyLiveUi(target, src) {
    if (!target || !src)
        return target
    if (src.armed !== undefined)
        target.armed = src.armed === true
    if (src.consent !== undefined)
        target.consent = src.consent === true
    if (src.reason !== undefined)
        target.reason = src.reason || (target.armed ? "" : "disarmed")
    if (src.ocrAvailable !== undefined)
        target.ocrAvailable = src.ocrAvailable === true
    if (src.byteCap !== undefined)
        target.byteCap = Number(src.byteCap) || target.byteCap
    if (src.daysEstimate !== undefined)
        target.daysEstimate = src.daysEstimate
    if (src.firstTs !== undefined)
        target.firstTs = Number(src.firstTs) || 0
    if (src.lastTs !== undefined)
        target.lastTs = Number(src.lastTs) || 0
    if (src.framesToday !== undefined)
        target.framesToday = Number(src.framesToday) || 0
    if (src.bytes !== undefined)
        target.bytes = Number(src.bytes) || 0
    if (src.status !== undefined)
        target.status = String(src.status || "")
    if (src.fallback !== undefined)
        target.fallback = src.fallback === true
    target.firstRun = !target.consent
    return target
}

function overlayViewAfterRefresh(consent, requestedView) {
    if (!consent)
        return "consent"
    if (requestedView === "clips")
        return "clips"
    if (requestedView === "consent")
        return "consent"
    return "scrub"
}
