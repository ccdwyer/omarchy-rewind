use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Client {
    pub address: String,
    pub class: String,
    pub title: String,
    pub workspace: String,
    pub monitor: i64,
    pub at: (i64, i64),
    pub size: (i64, i64),
    pub floating: bool,
    pub mapped: bool,
    pub hidden: bool,
    pub pid: i64,
    pub fullscreen: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Monitor {
    pub name: String,
    pub width: i64,
    pub height: i64,
    pub x: i64,
    pub y: i64,
    pub focused: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActiveWindow {
    pub class: String,
    pub title: String,
    pub workspace: String,
    pub at: (i64, i64),
    pub size: (i64, i64),
    pub monitor: i64,
    pub address: String,
}

static LAST_CURSOR_KEY: AtomicU64 = AtomicU64::new(0);
static LAST_INPUT_MS: AtomicI64 = AtomicI64::new(0);

pub fn clients() -> Result<Vec<Client>, String> {
    parse_clients(&hyprctl(&["-j", "clients"])?)
}

pub fn parse_clients(raw: &str) -> Result<Vec<Client>, String> {
    let value: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let arr = value.as_array().cloned().unwrap_or_default();
    Ok(arr.iter().filter_map(parse_client).collect())
}

fn parse_client(v: &Value) -> Option<Client> {
    let class = v
        .get("class")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let workspace = v
        .get("workspace")
        .and_then(|w| {
            w.get("id")
                .and_then(|i| i.as_i64())
                .map(|i| i.to_string())
                .or_else(|| w.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
        })
        .unwrap_or_else(|| "1".into());
    let at = pair(v.get("at")).unwrap_or((0, 0));
    let size = pair(v.get("size")).unwrap_or((0, 0));
    Some(Client {
        address: v
            .get("address")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        class,
        title: v
            .get("title")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        workspace,
        monitor: v.get("monitor").and_then(|x| x.as_i64()).unwrap_or(0),
        at,
        size,
        floating: v.get("floating").and_then(|x| x.as_bool()).unwrap_or(false),
        mapped: v.get("mapped").and_then(|x| x.as_bool()).unwrap_or(true),
        hidden: v.get("hidden").and_then(|x| x.as_bool()).unwrap_or(false),
        pid: v.get("pid").and_then(|x| x.as_i64()).unwrap_or(0),
        fullscreen: fullscreen_flag(v.get("fullscreen")),
    })
}

fn fullscreen_flag(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        Some(Value::String(s)) => s != "0" && s != "false",
        _ => false,
    }
}

fn pair(v: Option<&Value>) -> Option<(i64, i64)> {
    let arr = v?.as_array()?;
    if arr.len() < 2 {
        return None;
    }
    Some((arr[0].as_i64().unwrap_or(0), arr[1].as_i64().unwrap_or(0)))
}

pub fn active_window() -> Result<ActiveWindow, String> {
    parse_active(&hyprctl(&["-j", "activewindow"])?)
}

pub fn parse_active(raw: &str) -> Result<ActiveWindow, String> {
    let v: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    if v.is_null() || v.get("class").is_none() {
        return Ok(ActiveWindow::default());
    }
    let c = parse_client(&v).unwrap_or_default();
    Ok(ActiveWindow {
        class: c.class,
        title: c.title,
        workspace: c.workspace,
        at: c.at,
        size: c.size,
        monitor: c.monitor,
        address: c.address,
    })
}

pub fn focused_monitor() -> Result<Monitor, String> {
    let mons = parse_monitors(&hyprctl(&["-j", "monitors"])?)?;
    Ok(mons
        .into_iter()
        .find(|m| m.focused)
        .unwrap_or_default())
}

pub fn parse_monitors(raw: &str) -> Result<Vec<Monitor>, String> {
    let value: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let arr = value.as_array().cloned().unwrap_or_default();
    Ok(arr
        .iter()
        .map(|v| Monitor {
            name: v
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            width: v.get("width").and_then(|x| x.as_i64()).unwrap_or(0),
            height: v.get("height").and_then(|x| x.as_i64()).unwrap_or(0),
            x: v.get("x").and_then(|x| x.as_i64()).unwrap_or(0),
            y: v.get("y").and_then(|x| x.as_i64()).unwrap_or(0),
            focused: v.get("focused").and_then(|x| x.as_bool()).unwrap_or(false),
        })
        .collect())
}

pub fn dispatch(request: &str) -> Result<(), String> {
    let status = Command::new("hyprctl")
        .args(["dispatch"])
        .arg(request)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("hyprctl dispatch failed: {request}"))
    }
}

pub fn locked() -> bool {
    if hyprlock_running() {
        return true;
    }
    if let Ok(raw) = hyprctl(&["-j", "devices"]) {
        let _ = raw;
    }
    session_locked()
}

fn hyprlock_running() -> bool {
    Command::new("pidof")
        .arg("hyprlock")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn session_locked() -> bool {
    let out = Command::new("loginctl")
        .args(["show-session", "self", "-p", "LockedHint", "--value"])
        .stdin(Stdio::null())
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim() == "yes",
        Err(_) => false,
    }
}

pub fn idle_ms() -> u64 {
    note_cursor_activity();
    if let Some(ms) = loginctl_idle_ms() {
        return ms;
    }
    let last = LAST_INPUT_MS.load(Ordering::SeqCst);
    if last <= 0 {
        return 0;
    }
    let now = now_ms();
    now.saturating_sub(last).max(0) as u64
}

fn loginctl_idle_ms() -> Option<u64> {
    let out = Command::new("loginctl")
        .args(["show-session", "self", "-p", "IdleSinceHint", "--value"])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if raw.is_empty() || raw == "0" {
        return Some(0);
    }
    let since: u64 = raw.parse().ok()?;
    // IdleSinceHint is CLOCK_REALTIME microseconds on systemd.
    let now_us = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_micros() as u64;
    if since == 0 || since > now_us {
        return Some(0);
    }
    Some((now_us - since) / 1000)
}

fn note_cursor_activity() {
    let out = Command::new("hyprctl")
        .args(["cursorpos"])
        .stdin(Stdio::null())
        .output();
    if let Ok(o) = out {
        let key = hash_bytes(&o.stdout);
        let prev = LAST_CURSOR_KEY.swap(key, Ordering::SeqCst);
        if prev != key {
            LAST_INPUT_MS.store(now_ms(), Ordering::SeqCst);
        }
    }
}

fn hash_bytes(b: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for x in b {
        h ^= *x as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn portal_screencast_active() -> bool {
    if let Ok(out) = Command::new("pw-dump")
        .stdin(Stdio::null())
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        if parse_portal_dump(&text) {
            return true;
        }
    }
    false
}

pub fn parse_portal_dump(dump: &str) -> bool {
    let lower = dump.to_ascii_lowercase();
    (lower.contains("xdg-desktop-portal") || lower.contains("gnome-shell"))
        && (lower.contains("screencast")
            || lower.contains("stream/input/video")
            || lower.contains("media.class\":\"video/source"))
}

fn hyprctl(args: &[&str]) -> Result<String, String> {
    let out = Command::new("hyprctl")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "hyprctl {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keepass_on_second_output() {
        let raw = r#"[{"address":"0x1","class":"kitty","title":"a","workspace":{"id":1},"at":[0,0],"size":[100,100],"mapped":true,"hidden":false,"monitor":0},{"address":"0x2","class":"keepassxc","title":"db","workspace":{"id":2},"at":[1920,0],"size":[400,400],"mapped":true,"hidden":false,"monitor":1}]"#;
        let clients = parse_clients(raw).unwrap();
        assert_eq!(clients.len(), 2);
        assert_eq!(clients[1].class, "keepassxc");
        assert_eq!(clients[1].monitor, 1);
        assert!(clients[1].mapped);
    }

    #[test]
    fn portal_dump_detects_screencast() {
        let dump = r#"[{"type":"PipeWire:Interface:Node","info":{"props":{"node.name":"xdg-desktop-portal-screencast","media.class":"Stream/Input/Video"}}}]"#;
        assert!(parse_portal_dump(dump));
        assert!(!parse_portal_dump(r#"[{"node.name":"firefox"}]"#));
    }
}
