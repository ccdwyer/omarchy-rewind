use crate::encode::which;
use crate::{now_ms, DaemonState, CLIP_CAP};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};
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

fn clear_cached() {
    if let Ok(mut g) = cache().lock() {
        g.clear();
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
        // Each clipboard change is written in full, then a NUL, so multiline
        // text is one event instead of one ingest per line.
        let child = Command::new("wl-paste")
            .args(["-w", "-t", "text", "sh", "-c", "cat; printf '\\0'"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            thread::sleep(Duration::from_secs(5));
            continue;
        };
        if let Some(stdout) = child.stdout.take() {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut buf = Vec::new();
                match reader.read_until(0, &mut buf) {
                    Ok(0) => break,
                    Ok(_) => {
                        if buf.last() == Some(&0) {
                            buf.pop();
                        }
                        if buf.is_empty() {
                            continue;
                        }
                        let text = String::from_utf8_lossy(&buf);
                        ingest(&shared, &text);
                    }
                    Err(_) => break,
                }
            }
        }
        let _ = child.wait();
        thread::sleep(Duration::from_secs(1));
    }
}

pub(crate) fn ingest(shared: &DaemonState, raw: &str) {
    if raw.is_empty() {
        return;
    }
    let text = cap_text(raw);
    if !shared.is_recording() {
        // Drop secrets copied while paused (lock, overlay, excluded app, …)
        // so the next frame cannot attach them.
        clear_cached();
        return;
    }
    {
        let mut g = cache().lock().unwrap();
        if *g == text {
            return;
        }
        *g = text.clone();
    }
    crate::with_arm_read(shared, |arm| {
        if !crate::write_allowed(shared, arm, arm.gen) {
            return;
        }
        let ts = now_ms();
        let _ = shared.with_store_mut(|store| {
            let _ = store.commit_clip_tx(ts, "text/plain", &text, || {
                crate::write_allowed(shared, arm, arm.gen)
            });
        });
    });
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

    #[test]
    fn cap_keeps_multiline() {
        let s = "line1\nline2\nline3";
        assert_eq!(cap_text(s), s);
    }
}
