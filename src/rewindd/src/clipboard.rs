use crate::encode::which;
use crate::{now_ms, DaemonState, CLIP_CAP};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

/// A cached clip is tagged with the (arm generation, privacy epoch) it was
/// committed under. `latest_cached` only returns it for a frame carrying the
/// exact same tag, so a clip can only ever attach to a frame from the same
/// uninterrupted recording window — never resurrected across a disarm or a
/// privacy pause.
struct Cached {
    text: String,
    gen: u64,
    epoch: u64,
}

static LAST: OnceLock<Mutex<Option<Cached>>> = OnceLock::new();

fn cache() -> &'static Mutex<Option<Cached>> {
    LAST.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
thread_local! {
    /// Test-only seam fired inside `ingest` right after the generation ticket
    /// is captured, letting a test interleave a disarm→re-arm between ticket
    /// acquisition and the commit. Never present in release builds.
    static AFTER_TICKET_HOOK: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
fn set_after_ticket_hook(hook: Box<dyn FnMut()>) {
    AFTER_TICKET_HOOK.with(|h| *h.borrow_mut() = Some(hook));
}

#[cfg(test)]
fn fire_after_ticket_hook() {
    AFTER_TICKET_HOOK.with(|h| {
        if let Some(hook) = h.borrow_mut().as_mut() {
            hook();
        }
    });
}

/// Return the cached clip only if it was committed under the same arm
/// generation AND privacy epoch as the frame being finalized. Any mismatch
/// (stale generation, a pause transition since, or nothing cached) yields None.
pub(crate) fn latest_cached(gen: u64, epoch: u64) -> Option<String> {
    let g = cache().lock().ok()?;
    let c = g.as_ref()?;
    if c.gen == gen && c.epoch == epoch && !c.text.is_empty() {
        Some(c.text.clone())
    } else {
        None
    }
}

pub(crate) fn clear_cached() {
    if let Ok(mut g) = cache().lock() {
        *g = None;
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
    // Capture the generation ticket at the very start, before caching or
    // committing this clipboard event. If disarmed, drop it (and clear the
    // cache) so a subsequent re-arm cannot resurrect a clip that was copied
    // before/around the disarm. Using the current `arm.gen` at commit time
    // instead would let a disarm→re-arm during processing authorize a stale
    // clip under the new generation, defeating the zero-write contract.
    let Some(ticket) = crate::arm_ticket(shared) else {
        clear_cached();
        return;
    };
    // Sample the privacy epoch alongside the ticket, before any processing. Both
    // must still hold at commit, so a disarm OR any privacy pause that begins
    // between here and the commit invalidates this clip.
    let epoch0 = shared.privacy_epoch();
    // Test-only: let a test disarm→re-arm here, between ticket capture and the
    // commit below, to prove the retained ticket (not the live `arm.gen`) gates
    // the write.
    #[cfg(test)]
    fire_after_ticket_hook();
    let text = cap_text(raw);
    if !shared.is_recording() {
        // Drop secrets copied while paused (lock, overlay, excluded app, …)
        // so the next frame cannot attach them.
        clear_cached();
        return;
    }
    // Dedup: this exact clip is already cached for this recording window.
    if let Ok(g) = cache().lock() {
        if let Some(c) = g.as_ref() {
            if c.gen == ticket && c.epoch == epoch0 && c.text == text {
                return;
            }
        }
    }
    // Commit FIRST, under the gate and the original (ticket, epoch); publish to
    // the cache only on success. Caching before validation would let a rejected
    // clip attach to the next valid frame (leaking into search data), so the
    // cache is only ever populated with a clip that actually committed.
    let committed = crate::with_arm_read(shared, |arm| {
        if !crate::write_allowed(shared, arm, ticket) || shared.privacy_epoch() != epoch0 {
            return false;
        }
        let ts = now_ms();
        shared
            .with_store_mut(|store| {
                store.commit_clip_tx(ts, "text/plain", &text, || {
                    crate::write_allowed(shared, arm, ticket) && shared.privacy_epoch() == epoch0
                })
            })
            .unwrap_or(Ok(false))
            .unwrap_or(false)
    });
    if committed {
        if let Ok(mut g) = cache().lock() {
            *g = Some(Cached {
                text,
                gen: ticket,
                epoch: epoch0,
            });
        }
    } else {
        clear_cached();
    }
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

    #[test]
    fn clip_ingest_spanning_disarm_rearm_does_not_commit() {
        // The ticket is captured at ingest entry. If a disarm→re-arm happens
        // between that capture and the commit (fired via the after-ticket
        // hook), the retained ticket — not the live `arm.gen` — must gate the
        // write, so the clip is NOT committed under the new generation.
        let _env = crate::TEST_CAPTURE_ENV_LOCK.lock().unwrap();
        std::env::set_var("REWIND_TEST_CAPTURE", "1");
        clear_cached();
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = crate::boot_shared(&data).unwrap();
        {
            let mut st = shared.settings.lock().unwrap();
            st.consent_at = now_ms();
        }
        crate::persist_consent(&shared).unwrap();
        crate::arm_now(&shared).unwrap();

        let hook_shared = shared.clone();
        set_after_ticket_hook(Box::new(move || {
            crate::disarm_now(&hook_shared);
            crate::arm_now(&hook_shared).unwrap();
        }));

        ingest(&shared, "AKIA-DISTINCTIVE-RACE-CLIP");

        let counts = shared
            .with_store(|s| s.mutation_counters().unwrap())
            .unwrap();
        assert_eq!(
            counts.3, 0,
            "no clip row for an ingest whose processing spanned disarm/re-arm"
        );

        AFTER_TICKET_HOOK.with(|h| *h.borrow_mut() = None);
        clear_cached();
        std::env::remove_var("REWIND_TEST_CAPTURE");
    }
}
