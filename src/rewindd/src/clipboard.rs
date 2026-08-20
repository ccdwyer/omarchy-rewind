use crate::encode::which;
use crate::{now_ms, DaemonState, CLIP_CAP};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

static LAST: OnceLock<Mutex<String>> = OnceLock::new();

fn cache() -> &'static Mutex<String> {
    LAST.get_or_init(|| Mutex::new(String::new()))
}

pub fn latest_cached() -> Option<String> {
    let g = cache().lock().ok()?;
    if g.is_empty() {
        None
    } else {
        Some(g.clone())
    }
}

pub fn cap_text(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if bytes.len() <= CLIP_CAP {
        return raw.to_string();
    }
    let mut end = CLIP_CAP;
    while end > 0 && !raw.is_char_boundary(end) {
        end -= 1;
    }
    String::from_utf8_lossy(&bytes[..end]).to_string()
}

pub fn copy_text(text: &str) -> Result<(), String> {
    if !which("wl-copy") {
        return Err("wl-copy missing".into());
    }
    let mut child = Command::new("wl-copy")
        .arg("-t")
        .arg("text/plain")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let _ = child.wait();
    Ok(())
}

pub(crate) fn watch(shared: Arc<DaemonState>) {
    if !which("wl-paste") {
        return;
    }
    loop {
        let child = Command::new("wl-paste")
            .args(["-w", "-t", "text", "cat"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            thread::sleep(Duration::from_secs(5));
            continue;
        };
        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                ingest(&shared, &line);
            }
        }
        let _ = child.wait();
        thread::sleep(Duration::from_secs(1));
    }
}

fn ingest(shared: &DaemonState, raw: &str) {
    if raw.is_empty() {
        return;
    }
    let text = cap_text(raw);
    {
        let mut g = cache().lock().unwrap();
        if *g == text {
            return;
        }
        *g = text.clone();
    }
    if !shared.is_armed() {
        return;
    }
    let ts = now_ms();
    let store = shared.store();
    let _ = store.insert_clip(ts, "text/plain", &text);
    let _ = store.index_search_row(ts, "", "", "", &text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_truncates_at_64k() {
        let s = "a".repeat(CLIP_CAP + 50);
        let out = cap_text(&s);
        assert_eq!(out.len(), CLIP_CAP);
    }
}
