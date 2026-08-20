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
