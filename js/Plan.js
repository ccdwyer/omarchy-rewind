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

function oneWindowPlan(win) {
    win = win || {}
    var cls = win.class || win.app || ""
    var title = win.title || ""
    var ws = win.workspace || win.ws || "1"
    var addr = win.address || ""
    var x = 0
    var y = 0
    if (win.at && win.at.length >= 2) {
        x = win.at[0]
        y = win.at[1]
    } else {
        x = Number(win.x) || 0
        y = Number(win.y) || 0
    }
    if (isBrowser(cls)) {
        return {
            title: "Reopen " + (cls || "window"),
            honest: true,
            steps: [],
            unrecoverable: [{ class: cls, title: title, reason: "browser tabs cannot be restored" }],
            note: "One-window plan. Browser tabs and documents are unrecoverable. Confirm does nothing for this window."
        }
    }
    var steps = []
    steps.push({
        kind: "exec",
        class: cls,
        cmd: win.exec || win.cmd || "",
        workspace: ws,
        label: "Launch " + (cls || "app")
    })
    if (addr) {
        steps.push({
            kind: "move",
            class: cls,
            address: addr,
            workspace: ws,
            label: "Move to workspace " + ws
        })
        steps.push({
            kind: "geometry",
            class: cls,
            address: addr,
            x: x,
            y: y,
            label: "Place at " + x + "," + y
        })
    }
    return {
        title: "Reopen " + (cls || "window"),
        honest: true,
        steps: steps,
        unrecoverable: [],
        note: "Review this one-window plan. Confirm launches and places only this window."
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
