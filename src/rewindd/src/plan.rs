use crate::hypr::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Desktop {
    pub id: String,
    pub name: String,
    pub exec: String,
    pub wm_class: String,
}

pub fn application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }
    dirs.push(PathBuf::from("/usr/share/applications"));
    dirs.push(PathBuf::from("/usr/local/share/applications"));
    if let Ok(extra) = std::env::var("REWIND_APPLICATIONS") {
        for p in extra.split(':') {
            if !p.is_empty() {
                dirs.push(PathBuf::from(p));
            }
        }
    }
    dirs
}

pub fn desktop_map(dirs: Vec<PathBuf>) -> HashMap<String, Desktop> {
    let mut map = HashMap::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            if let Some(d) = parse_desktop(&path) {
                if !d.wm_class.is_empty() {
                    map.insert(d.wm_class.to_ascii_lowercase(), d.clone());
                }
                map.insert(d.id.to_ascii_lowercase(), d.clone());
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                map.entry(stem).or_insert(d);
            }
        }
    }
    map
}

pub fn parse_desktop(path: &Path) -> Option<Desktop> {
    let raw = std::fs::read_to_string(path).ok()?;
    let mut name = String::new();
    let mut exec = String::new();
    let mut wm_class = String::new();
    let mut in_desktop = false;
    for line in raw.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_desktop = line.eq_ignore_ascii_case("[desktop entry]");
            continue;
        }
        if !in_desktop || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "Name" if name.is_empty() => name = v.trim().to_string(),
                "Exec" if exec.is_empty() => exec = strip_exec(v.trim()),
                "StartupWMClass" => wm_class = v.trim().to_string(),
                _ => {}
            }
        }
    }
    if exec.is_empty() {
        return None;
    }
    let id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("app")
        .to_string();
    Some(Desktop {
        id,
        name,
        exec,
        wm_class,
    })
}

fn strip_exec(exec: &str) -> String {
    exec.split_whitespace()
        .filter(|t| !t.starts_with('%'))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn is_browser(class: &str) -> bool {
    let c = class.to_ascii_lowercase();
    ["firefox", "chrome", "chromium", "brave", "librewolf", "vivaldi"]
        .iter()
        .any(|b| c.contains(b))
}

pub fn build(stored: &[Client], live: &[Client], map: &HashMap<String, Desktop>) -> Value {
    let mut steps = Vec::new();
    let mut unrecoverable = Vec::new();
    for win in stored {
        if win.class.is_empty() {
            continue;
        }
        let live_hit = live.iter().find(|l| {
            l.class.eq_ignore_ascii_case(&win.class) && (l.title == win.title || l.address == win.address)
        });
        if let Some(cur) = live_hit {
            if cur.workspace != win.workspace {
                steps.push(json!({
                    "kind": "move",
                    "address": cur.address,
                    "class": win.class,
                    "title": win.title,
                    "workspace": win.workspace.parse::<i64>().unwrap_or(1),
                    "label": format!("Move {} to workspace {}", win.class, win.workspace)
                }));
            }
            if win.floating {
                steps.push(json!({
                    "kind": "geometry",
                    "address": cur.address,
                    "x": win.at.0,
                    "y": win.at.1,
                    "w": win.size.0,
                    "h": win.size.1,
                    "label": format!("Place {} at {},{}", win.class, win.at.0, win.at.1)
                }));
            }
        } else {
            let key = win.class.to_ascii_lowercase();
            if let Some(desk) = map.get(&key).cloned().or_else(|| {
                map.iter()
                    .find(|(k, _)| key.contains(k.as_str()) || k.contains(&key))
                    .map(|(_, d)| d.clone())
            }) {
                steps.push(json!({
                    "kind": "exec",
                    "class": win.class,
                    "title": win.title,
                    "cmd": desk.exec,
                    "desktop": desk.id,
                    "workspace": win.workspace.parse::<i64>().unwrap_or(1),
                    "label": format!("Launch {}", if desk.name.is_empty() { desk.id.clone() } else { desk.name.clone() })
                }));
            } else {
                unrecoverable.push(json!({
                    "class": win.class,
                    "title": win.title,
                    "reason": "no desktop file for this class"
                }));
            }
            if is_browser(&win.class) {
                unrecoverable.push(json!({
                    "class": win.class,
                    "title": win.title,
                    "reason": "tabs and unsaved page state cannot be restored"
                }));
            }
        }
        if win.title.to_ascii_lowercase().contains("untitled")
            || win.title.to_ascii_lowercase().contains("unsaved")
        {
            unrecoverable.push(json!({
                "class": win.class,
                "title": win.title,
                "reason": "unsaved document state is not in the snapshot"
            }));
        }
    }
    json!({
        "title": "Reopen & arrange",
        "honest": true,
        "steps": steps,
        "unrecoverable": unrecoverable,
        "note": "This launches missing apps and places windows. It is not session restore."
    })
}

pub fn self_test() -> Result<(), String> {
    let stored = vec![Client {
        class: "kitty".into(),
        title: "zsh".into(),
        workspace: "2".into(),
        at: (10, 10),
        size: (400, 300),
        floating: true,
        mapped: true,
        ..Client::default()
    }];
    let mut map = HashMap::new();
    map.insert(
        "kitty".into(),
        Desktop {
            id: "kitty".into(),
            name: "Kitty".into(),
            exec: "kitty".into(),
            wm_class: "kitty".into(),
        },
    );
    let plan = build(&stored, &[], &map);
    let steps = plan.get("steps").and_then(|s| s.as_array()).cloned().unwrap_or_default();
    if steps.is_empty() {
        return Err("plan produced no launch step".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn launches_missing_and_lists_browser_tabs() {
        let stored = vec![
            Client {
                class: "kitty".into(),
                workspace: "1".into(),
                mapped: true,
                ..Client::default()
            },
            Client {
                class: "firefox".into(),
                title: "Inbox".into(),
                workspace: "2".into(),
                mapped: true,
                ..Client::default()
            },
        ];
        let mut map = HashMap::new();
        map.insert(
            "kitty".into(),
            Desktop {
                id: "kitty".into(),
                name: "Kitty".into(),
                exec: "kitty".into(),
                wm_class: "kitty".into(),
            },
        );
        map.insert(
            "firefox".into(),
            Desktop {
                id: "firefox".into(),
                name: "Firefox".into(),
                exec: "firefox".into(),
                wm_class: "firefox".into(),
            },
        );
        let plan = build(&stored, &[], &map);
        let steps = plan["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        let unre = plan["unrecoverable"].as_array().unwrap();
        assert!(unre.iter().any(|u| u["reason"]
            .as_str()
            .unwrap()
            .contains("tabs")));
    }

    #[test]
    fn parse_desktop_strips_field_codes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Foo.desktop");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "[Desktop Entry]\nName=Foo\nExec=/usr/bin/foo %u --bar\nStartupWMClass=Foo\n"
        )
        .unwrap();
        let d = parse_desktop(&path).unwrap();
        assert_eq!(d.exec, "/usr/bin/foo --bar");
        assert_eq!(d.wm_class, "Foo");
    }
}
