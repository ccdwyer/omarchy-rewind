.pragma library

function isBrowser(className) {
    var c = String(className || "").toLowerCase()
    var browsers = ["firefox", "chrome", "chromium", "brave", "librewolf", "vivaldi"]
    for (var i = 0; i < browsers.length; i++) {
        if (c.indexOf(browsers[i]) >= 0)
            return true
    }
    return false
}

function stepLabel(step) {
    if (!step)
        return ""
    if (step.label)
        return step.label
    if (step.kind === "exec")
        return "Launch " + (step.class || step.cmd || "app")
    if (step.kind === "move")
        return "Move " + (step.class || "window") + " to workspace " + step.workspace
    if (step.kind === "geometry")
        return "Place " + (step.class || "window")
    return step.kind || "step"
}

function unrecoverableLabel(item) {
    if (!item)
        return ""
    var who = item.title || item.class || "window"
    return who + " — " + (item.reason || "cannot restore")
}

function summarize(plan) {
    plan = plan || {}
    var steps = plan.steps || []
    var bad = plan.unrecoverable || []
    return {
        launch: countKind(steps, "exec"),
        move: countKind(steps, "move") + countKind(steps, "geometry"),
        blocked: bad.length,
        empty: steps.length === 0 && bad.length === 0
    }
}

function countKind(steps, kind) {
    var n = 0
    for (var i = 0; i < steps.length; i++) {
        if (steps[i] && steps[i].kind === kind)
            n += 1
    }
    return n
}
