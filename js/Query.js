.pragma library

function scaleBox(cropBox, cropOrigin, outSize, storedSize) {
    var x = cropBox.x || 0
    var y = cropBox.y || 0
    var w = cropBox.w || 0
    var h = cropBox.h || 0
    var ox = cropOrigin.x || 0
    var oy = cropOrigin.y || 0
    var outW = outSize.w || 1
    var outH = outSize.h || 1
    var sw = storedSize.w || 1
    var sh = storedSize.h || 1
    var sx = sw / outW
    var sy = sh / outH
    return {
        word: cropBox.word || "",
        x: clamp01(((ox + x) * sx) / sw),
        y: clamp01(((oy + y) * sy) / sh),
        w: clamp01((w * sx) / sw),
        h: clamp01((h * sy) / sh)
    }
}

function clamp01(n) {
    if (n < 0)
        return 0
    if (n > 1)
        return 1
    return n
}

function snippetAround(hay, needle) {
    hay = String(hay || "")
    needle = String(needle || "")
    var lower = hay.toLowerCase()
    var n = needle.toLowerCase()
    var idx = lower.indexOf(n)
    if (idx < 0)
        return hay.substring(0, 64)
    var start = Math.max(0, idx - 24)
    var end = Math.min(hay.length, idx + n.length + 24)
    var s = hay.substring(start, end)
    if (start > 0)
        s = "…" + s
    if (end < hay.length)
        s = s + "…"
    return s
}

function hitMarkers(hits) {
    var out = []
    if (!hits)
        return out
    for (var i = 0; i < hits.length; i++) {
        if (hits[i] && hits[i].ts)
            out.push(Number(hits[i].ts))
    }
    return out
}

function fittedRect(containerW, containerH, sourceW, sourceH) {
    containerW = Number(containerW) || 0
    containerH = Number(containerH) || 0
    sourceW = Number(sourceW) || 0
    sourceH = Number(sourceH) || 0
    if (!sourceW || !sourceH || !containerW || !containerH)
        return { x: 0, y: 0, w: containerW, h: containerH }
    var scale = Math.min(containerW / sourceW, containerH / sourceH)
    var w = sourceW * scale
    var h = sourceH * scale
    return {
        x: (containerW - w) / 2,
        y: (containerH - h) / 2,
        w: w,
        h: h
    }
}

function boxesForQuery(boxes, needle) {
    var n = String(needle || "").toLowerCase()
    var out = []
    if (!boxes)
        return out
    for (var i = 0; i < boxes.length; i++) {
        var b = boxes[i]
        var w = String(b.word || "").toLowerCase()
        if (!n || w.indexOf(n) >= 0 || n.indexOf(w) >= 0)
            out.push(b)
    }
    return out
}
