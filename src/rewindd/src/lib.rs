//! rewindd — capture, index, and recover local desktop history.
//!
//! Heavy work lives here so the Omarchy shell process never encodes or
//! OCRs on the QML thread. The binary speaks NDJSON on stdio.

pub mod capture;
pub mod clipboard;
pub mod config;
pub mod dhash;
pub mod encode;
pub mod exclude;
pub mod hypr;
pub mod ipc;
pub mod ocr;
pub mod pause;
pub mod perms;
pub mod pixel;
pub mod plan;
pub mod query;
pub mod store;

use crate::config::Settings;
use crate::ipc::{Command, Event};
use crate::pause::{PauseInput, PauseReason};
use crate::perms::DataPaths;
use crate::store::{FrameInsert, Store};
use serde_json::json;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as Proc, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const CLIP_CAP: usize = 64 * 1024;

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn self_test() -> Result<(), String> {
    dhash::self_test()?;
    exclude::self_test()?;
    pause::self_test()?;
    plan::self_test()?;
    query::self_test()?;
    encode::self_test()?;
    if crate::pixel::startup_recording(0, true, true) {
        return Err("startup armed without consent".into());
    }
    let dir = tempfile_dir()?;
    let mut store = Store::open(&dir.join("rewind.db")).map_err(|e| e.to_string())?;
    store.self_test().map_err(|e| e.to_string())?;
    perms::assert_private_tree(&dir).map_err(|e| e.to_string())?;
    Ok(())
}

fn tempfile_dir() -> Result<PathBuf, String> {
    let base = std::env::temp_dir().join(format!("rewindd-selftest-{}", std::process::id()));
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    perms::secure_dir(&base).map_err(|e| e.to_string())?;
    Ok(base)
}

pub(crate) struct ArmState {
    pub armed: bool,
    /// Monotonic arm generation. Bumped under the exclusive gate lock on every
    /// arm/disarm transition. A write is authorized only if the capturing job's
    /// ticket generation still matches, so a job that began before a
    /// disarm/re-arm cannot commit stale material across the disarmed boundary.
    pub gen: u64,
}

pub(crate) struct Shared {
    gate: RwLock<ArmState>,
    settings: Mutex<Settings>,
    store: Mutex<Option<Store>>,
    paths: DataPaths,
    armed: AtomicBool,
    overlay_open: AtomicBool,
    last_dhash: AtomicU64,
    last_write_ms: AtomicU64,
    encoder: Mutex<String>,
    capture_backend: Mutex<String>,
    last_pause: Mutex<Option<PauseReason>>,
    arm_gen: AtomicU64,
    /// Monotonic privacy epoch. Bumped under `last_pause`'s lock on EVERY
    /// pause-state transition (lock, idle, overlay, portal, exclusion,
    /// private-browsing, title-pattern, disarm — on and off). A capture/OCR/clip
    /// job records the epoch at entry, before any blocking work, and every
    /// commit requires the epoch to be unchanged AND the session to be currently
    /// not paused. This closes the window where a privacy pause *begins during*
    /// the blocking grab/read/tesseract and the frame/clip would otherwise still
    /// commit (the arm generation alone does not move on a non-disarm pause).
    privacy_epoch: AtomicU64,
    /// Pause/resume transitions held in memory. Entering (or leaving) a pause
    /// must not itself write to the database — that would be a write while
    /// paused. Transitions are buffered here with their original timestamp and
    /// flushed only inside an authorized, currently-unpaused transaction (see
    /// `flush_pending_gaps`). Bounded so a long paused period cannot grow it
    /// without limit.
    pending_gaps: Mutex<Vec<(i64, &'static str, String)>>,
    tripwire: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

fn boot_shared(data_dir: &Path) -> Result<Arc<Shared>, String> {
    perms::install_umask();
    let paths = DataPaths::bare(data_dir);
    let settings = Settings::load(&paths.state).unwrap_or_default();
    let armed = crate::pixel::startup_recording(
        settings.consent_at,
        settings.arm_on_login,
        settings.armed,
    );
    let store = if armed {
        DataPaths::prepare(data_dir)?;
        Some(Store::open(&paths.db)?)
    } else if paths.db.exists() {
        Store::open_read_only(&paths.db).ok()
    } else {
        None
    };
    Ok(Arc::new(Shared {
        gate: RwLock::new(ArmState {
            armed,
            gen: if armed { 1 } else { 0 },
        }),
        settings: Mutex::new(settings),
        store: Mutex::new(store),
        paths,
        armed: AtomicBool::new(armed),
        overlay_open: AtomicBool::new(false),
        last_dhash: AtomicU64::new(0),
        last_write_ms: AtomicU64::new(0),
        encoder: Mutex::new(encode::preferred_name().to_string()),
        capture_backend: Mutex::new(capture::backend_name().to_string()),
        last_pause: Mutex::new(None),
        arm_gen: AtomicU64::new(if armed { 1 } else { 0 }),
        privacy_epoch: AtomicU64::new(0),
        pending_gaps: Mutex::new(Vec::new()),
        tripwire: Mutex::new(None),
    }))
}

pub fn run_daemon(data_dir: PathBuf) -> io::Result<()> {
    let shared = boot_shared(&data_dir).map_err(io::Error::other)?;

    emit(&Event::ready(
        shared.armed.load(Ordering::SeqCst),
        shared.settings.lock().unwrap().consent_at > 0,
        encode::preferred_name(),
    ));

    let cap = Arc::clone(&shared);
    thread::Builder::new()
        .name("rewind-capture".into())
        .spawn(move || capture_loop(cap))
        .ok();

    let clip = Arc::clone(&shared);
    thread::Builder::new()
        .name("rewind-clipboard".into())
        .spawn(move || clipboard::watch(clip))
        .ok();

    let ocr = Arc::clone(&shared);
    thread::Builder::new()
        .name("rewind-ocr".into())
        .spawn(move || ocr::idle_loop(ocr))
        .ok();

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match Command::parse(line) {
            Ok(cmd) => {
                if matches!(cmd, Command::Shutdown { .. }) {
                    handle_command(&shared, cmd);
                    break;
                }
                handle_command(&shared, cmd);
            }
            Err(err) => emit(&Event::error(None, err)),
        }
    }
    Ok(())
}

fn handle_command(shared: &Arc<Shared>, cmd: Command) {
    match cmd {
        Command::Hello { id } => emit(&Event::reply(id, json!({"version": VERSION}))),
        Command::Arm { id, settings } => {
            apply_settings(shared, settings);
            let consent = shared.settings.lock().unwrap().consent_at;
            if consent == 0 {
                emit(&Event::error(
                    Some(id),
                    "consent required: arm rejected until the consent screen is recorded".into(),
                ));
                emit_state(shared, id);
                return;
            }
            if let Err(e) = arm_now(shared) {
                emit(&Event::error(Some(id), e));
                emit_state(shared, id);
                return;
            }
            emit_state(shared, id);
        }
        Command::Disarm { id } => {
            disarm_now(shared);
            emit_state(shared, id);
        }
        Command::Consent {
            id,
            arm_now: do_arm,
            arm_on_login,
        } => {
            let prev = shared.settings.lock().unwrap().clone();
            {
                let mut st = shared.settings.lock().unwrap();
                st.consent_at = now_ms();
                st.arm_on_login = arm_on_login;
                st.armed = false;
            }
            if let Err(e) = persist_consent(shared) {
                *shared.settings.lock().unwrap() = prev;
                emit(&Event::error(Some(id), e));
                emit_state(shared, id);
                return;
            }
            if do_arm {
                if let Err(e) = arm_now(shared) {
                    emit(&Event::error(Some(id), e));
                    emit_state(shared, id);
                    return;
                }
            }
            emit_state(shared, id);
        }
        Command::SetPause {
            id,
            reason,
            paused,
        } => {
            if reason == "overlay" {
                shared.overlay_open.store(paused, Ordering::SeqCst);
            }
            emit_state(shared, id);
        }
        Command::Configure { id, settings } => {
            apply_settings(shared, settings);
            emit_state(shared, id);
        }
        Command::Query { id, q, limit, from, to } => match query_locked(shared, &q, limit, from, to)
        {
            Ok(data) => emit(&Event::reply(id, data)),
            Err(e) => emit(&Event::error(Some(id), e)),
        },
        Command::Timeline { id, from, to, limit } => {
            let data = shared
                .with_store(|s| s.timeline(from, to, limit))
                .unwrap_or_else(|| Ok(json!({"frames": [], "gaps": []})));
            match data {
                Ok(data) => emit(&Event::reply(id, data)),
                Err(e) => emit(&Event::error(Some(id), e.to_string())),
            }
        }
        Command::Moment { id, ts } => {
            let data = shared
                .with_store(|s| s.moment(ts))
                .unwrap_or_else(|| Ok(json!({"frame": null, "clip": "", "windows": [], "boxes": []})));
            match data {
                Ok(data) => emit(&Event::reply(id, data)),
                Err(e) => emit(&Event::error(Some(id), e.to_string())),
            }
        }
        Command::Clips { id, limit } => {
            let data = shared
                .with_store(|s| s.clips(limit))
                .unwrap_or_else(|| Ok(json!({"clips": []})));
            match data {
                Ok(data) => emit(&Event::reply(id, data)),
                Err(e) => emit(&Event::error(Some(id), e.to_string())),
            }
        }
        Command::ReopenPlan { id, ts } => match build_plan(shared, ts) {
            Ok(data) => emit(&Event::reply(id, data)),
            Err(e) => emit(&Event::error(Some(id), e.to_string())),
        },
        Command::ReopenWindow { id, ts, target } => match build_one_plan(shared, ts, &target) {
            Ok(plan) => emit(&Event::reply(id, plan)),
            Err(e) => emit(&Event::error(Some(id), e)),
        },
        Command::ReopenExec { id, plan } => match execute_plan(plan) {
            Ok(data) => emit(&Event::reply(id, data)),
            Err(e) => emit(&Event::error(Some(id), e.to_string())),
        },
        Command::Wipe {
            id,
            scope,
            from,
            to,
        } => match authorized_wipe(shared, &scope, from, to) {
            Ok(data) => emit(&Event::reply(id, data)),
            Err(e) => emit(&Event::error(Some(id), e)),
        },
        Command::CopyClip { id, ts } => match copy_clip(shared, ts) {
            Ok(data) => emit(&Event::reply(id, data)),
            Err(e) => emit(&Event::error(Some(id), e.to_string())),
        },
        Command::Stats { id } => emit(&Event::reply(id, stats_json(shared))),
        Command::Shutdown { id } => {
            emit(&Event::reply(id, json!({"bye": true})));
        }
    }
}

fn apply_settings(shared: &Shared, incoming: serde_json::Value) {
    let mut st = shared.settings.lock().unwrap();
    st.merge_json(&incoming);
    drop(st);
    persist_settings(shared);
}

fn persist_settings(shared: &Shared) {
    if !shared.armed.load(Ordering::SeqCst) {
        return;
    }
    let st = shared.settings.lock().unwrap();
    let _ = st.save(&shared.paths.state);
}

fn persist_consent(shared: &Shared) -> Result<(), String> {
    let st = shared.settings.lock().unwrap();
    if st.consent_at == 0 {
        return Ok(());
    }
    st.save(&shared.paths.state)
}

fn ensure_store(shared: &Shared) -> Result<(), String> {
    let mut g = shared.store.lock().unwrap();
    if g.as_ref().map(|s| s.is_writable()).unwrap_or(false) {
        return Ok(());
    }
    DataPaths::prepare(&shared.paths.root)?;
    *g = Some(Store::open(&shared.paths.db)?);
    Ok(())
}

fn authorized_wipe(
    shared: &Shared,
    scope: &str,
    from: i64,
    to: i64,
) -> Result<serde_json::Value, String> {
    let mut arm = shared.gate.write().unwrap();
    let was_armed = arm.armed;
    if !was_armed && !shared.paths.db.exists() {
        return Ok(json!({"wiped": 0, "scope": scope, "from": from, "to": to}));
    }
    ensure_store(shared)?;
    let data = shared
        .with_store_mut(|s| s.wipe(scope, from, to))
        .ok_or_else(|| "wipe: store unavailable".to_string())??;
    if !was_armed {
        drop_writable_store(shared);
        arm.armed = false;
        shared.armed.store(false, Ordering::SeqCst);
        shared.settings.lock().unwrap().armed = false;
    }
    Ok(data)
}

fn drop_writable_store(shared: &Shared) {
    let mut g = shared.store.lock().unwrap();
    let Some(s) = g.take() else {
        return;
    };
    if s.is_writable() {
        let _ = s.checkpoint();
        drop(s);
        *g = if shared.paths.db.exists() {
            Store::open_read_only(&shared.paths.db).ok()
        } else {
            None
        };
    } else {
        *g = Some(s);
    }
}

fn arm_now(shared: &Shared) -> Result<(), String> {
    let mut arm = shared.gate.write().unwrap();
    {
        let mut st = shared.settings.lock().unwrap();
        st.armed = true;
    }
    if let Err(e) = ensure_store(shared) {
        shared.settings.lock().unwrap().armed = false;
        arm.armed = false;
        shared.armed.store(false, Ordering::SeqCst);
        return Err(e);
    }
    // Bump the generation under the exclusive gate lock so it stays consistent
    // with `armed`; mirror to the lock-free atomic for cheap reads.
    let next = shared.arm_gen.fetch_add(1, Ordering::SeqCst) + 1;
    arm.armed = true;
    arm.gen = next;
    shared.armed.store(true, Ordering::SeqCst);
    drop(arm);
    persist_settings(shared);
    Ok(())
}

fn disarm_now(shared: &Shared) {
    // Take the exclusive gate first so no in-flight capture can observe a
    // half-updated state; bump the generation and flip armed together.
    let mut arm = shared.gate.write().unwrap();
    let next = shared.arm_gen.fetch_add(1, Ordering::SeqCst) + 1;
    arm.armed = false;
    arm.gen = next;
    shared.armed.store(false, Ordering::SeqCst);
    {
        let mut st = shared.settings.lock().unwrap();
        st.armed = false;
    }
    // A disarm is also a privacy transition; advance the epoch and drop any
    // cached clip so a re-arm cannot resurrect a clip copied around the disarm.
    shared.privacy_epoch.fetch_add(1, Ordering::SeqCst);
    clipboard::clear_cached();
    drop_writable_store(shared);
}

pub(crate) fn with_arm_read<R>(shared: &Shared, f: impl FnOnce(&ArmState) -> R) -> R {
    let g = shared.gate.read().unwrap();
    f(&g)
}

fn query_locked(
    shared: &Shared,
    q: &str,
    limit: usize,
    from: i64,
    to: i64,
) -> Result<serde_json::Value, String> {
    match shared.with_store(|s| s.search(q, limit, from, to)) {
        Some(r) => r.map_err(|e| e.to_string()),
        None => Ok(json!({"hits": [], "ocrAvailable": false, "query": q})),
    }
}

fn build_plan(shared: &Shared, ts: i64) -> Result<serde_json::Value, String> {
    let stored = match shared.with_store(|s| s.layout_at(ts)) {
        Some(Ok(v)) => v,
        Some(Err(e)) => return Err(e.to_string()),
        None => Vec::new(),
    };
    let live = hypr::clients().unwrap_or_default();
    let map = plan::desktop_map(plan::application_dirs());
    Ok(plan::build(&stored, &live, &map))
}

fn build_one_plan(
    shared: &Shared,
    ts: i64,
    target: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let stored = match shared.with_store(|s| s.layout_at(ts)) {
        Some(Ok(v)) => v,
        Some(Err(e)) => return Err(e.to_string()),
        None => Vec::new(),
    };
    let live = hypr::clients().unwrap_or_default();
    let map = plan::desktop_map(plan::application_dirs());
    Ok(plan::build_one(&stored, &live, &map, target))
}

fn execute_plan(plan: serde_json::Value) -> Result<serde_json::Value, String> {
    let steps = plan
        .get("steps")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();
    let mut ran = Vec::new();
    for step in steps {
        let kind = step.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        match kind {
            "exec" => {
                if let Some(cmd) = step.get("cmd").and_then(|c| c.as_str()) {
                    let ws = step
                        .get("workspace")
                        .and_then(|w| w.as_i64())
                        .unwrap_or(1);
                    let req = format!("[workspace {ws} silent] {cmd}");
                    let _ = hypr::dispatch(&format!("exec {req}"));
                    ran.push(step);
                }
            }
            "move" => {
                if let (Some(addr), Some(ws)) = (
                    step.get("address").and_then(|a| a.as_str()),
                    step.get("workspace").and_then(|w| w.as_i64()),
                ) {
                    let _ = hypr::dispatch(&format!("movetoworkspacesilent {ws},address:{addr}"));
                    ran.push(step);
                }
            }
            "geometry" => {
                if let (Some(addr), Some(x), Some(y)) = (
                    step.get("address").and_then(|a| a.as_str()),
                    step.get("x").and_then(|v| v.as_i64()),
                    step.get("y").and_then(|v| v.as_i64()),
                ) {
                    let _ = hypr::dispatch(&format!(
                        "movewindowpixel exact {x} {y},address:{addr}"
                    ));
                    ran.push(step);
                }
            }
            _ => {}
        }
    }
    Ok(json!({"ran": ran.len(), "steps": ran}))
}

fn copy_clip(shared: &Shared, ts: i64) -> Result<serde_json::Value, String> {
    let content = match shared.with_store(|s| s.clip_at(ts)) {
        Some(Ok(c)) => c,
        Some(Err(e)) => return Err(e.to_string()),
        None => String::new(),
    };
    if content.is_empty() {
        return Err("no clip at that moment".into());
    }
    clipboard::copy_text(&content)?;
    Ok(json!({"ok": true, "bytes": content.len()}))
}

fn stats_json(shared: &Shared) -> serde_json::Value {
    let snap = shared
        .with_store(|s| s.stats().unwrap_or_default())
        .unwrap_or_default();
    let settings = shared.settings.lock().unwrap();
    json!({
        "armed": shared.armed.load(Ordering::SeqCst),
        "consent": settings.consent_at > 0,
        "paused": current_pause(shared).is_some(),
        "reason": current_pause(shared).map(|r| r.as_str()).unwrap_or(""),
        "frames": snap.frames,
        "framesToday": snap.frames_today,
        "bytes": snap.bytes,
        "byteCap": settings.byte_cap,
        "daysEstimate": query::days_estimate(snap.bytes, settings.byte_cap, snap.first_ts, now_ms()),
        "firstTs": snap.first_ts,
        "lastTs": snap.last_ts,
        "encoder": shared.encoder.lock().unwrap().clone(),
        "ocrAvailable": ocr::tesseract_available(),
        "capture": shared.capture_backend.lock().unwrap().clone(),
        "version": VERSION
    })
}

fn emit_state(shared: &Shared, id: u64) {
    let data = stats_json(shared);
    emit(&Event::reply(id, data.clone()));
    emit(&Event::from_stats(data));
}

pub fn emit(event: &Event) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{}", event.to_json());
    let _ = out.flush();
}

fn current_pause(shared: &Shared) -> Option<PauseReason> {
    evaluate_pause(shared)
}

/// Whether OCR may commit given a pause reason. OCR runs opportunistically
/// during genuine idle, so `None` (fully recording) and `Some(Idle)` permit a
/// commit; every hard pause (Locked, Overlay, Portal, Excluded,
/// PrivateBrowsing, TitlePattern, Disarmed) forbids it. Pure so the "locked
/// screen blocks OCR writes" rule is testable without a compositor.
fn ocr_write_ok(reason: &Option<PauseReason>) -> bool {
    matches!(reason, None | Some(PauseReason::Idle))
}

fn evaluate_pause(shared: &Shared) -> Option<PauseReason> {
    if !shared.armed.load(Ordering::SeqCst) {
        return Some(PauseReason::Disarmed);
    }
    if shared.overlay_open.load(Ordering::SeqCst) {
        return Some(PauseReason::Overlay);
    }
    let settings = shared.settings.lock().unwrap().clone();
    // With the synthetic test-capture backend (CI / no-network audit) there is
    // no compositor to report idle/lock/portal, so treat the session as active
    // to prove that an armed daemon actually writes frames. Never set outside
    // tests/audit.
    let test_capture = std::env::var("REWIND_TEST_CAPTURE").is_ok();
    let clients = if test_capture {
        Vec::new()
    } else {
        hypr::clients().unwrap_or_default()
    };
    let locked = if test_capture { false } else { hypr::locked() };
    let idle_ms = if test_capture { 0 } else { hypr::idle_ms() };
    let portal = if test_capture {
        false
    } else {
        hypr::portal_screencast_active()
    };
    let input = PauseInput {
        armed: true,
        locked,
        idle_ms,
        idle_limit_ms: settings.idle_pause_sec.saturating_mul(1000),
        overlay_open: false,
        excluded: exclude::excluded_visible(&clients, &settings.exclude_apps),
        portal_active: portal,
        private_browsing: exclude::private_browsing(&clients),
        title_pause: exclude::title_pause(&clients, &settings.title_pause_patterns),
    };
    pause::evaluate(&input)
}

#[cfg(test)]
fn recording_now(shared: &Shared) -> bool {
    evaluate_pause(shared).is_none()
}

/// Capture a ticket at the start of a capture/OCR job: the arm generation read
/// under the gate's read lock. `None` if disarmed. The returned generation is
/// what every later `still_armed`/`write_allowed` check must match, so a
/// disarm (which bumps the generation) invalidates the job even if a
/// subsequent re-arm sets `armed` back to true.
pub(crate) fn arm_ticket(shared: &Shared) -> Option<u64> {
    let g = shared.gate.read().unwrap();
    if g.armed {
        Some(g.gen)
    } else {
        None
    }
}

/// Authorize a write against a held gate read-guard and the job's ticket.
/// Both the armed flag and the generation are read from the same gate-protected
/// snapshot, so this is race-free with respect to arm/disarm (which hold the
/// write lock). A generation mismatch means a disarm happened since the ticket
/// was issued — reject even if currently re-armed.
pub(crate) fn write_allowed(shared: &Shared, arm: &ArmState, ticket: u64) -> bool {
    shared.fire_tripwire();
    // Gate-protected armed flag and generation are the authority (race-free vs
    // arm/disarm, which hold the write lock); the atomic mirror is an extra
    // belt-and-suspenders read that also lets an interrupted disarm be observed
    // immediately. A generation mismatch means a disarm happened since the
    // ticket was issued, so re-arming does not resurrect a stale job.
    arm.armed && arm.gen == ticket && shared.armed.load(Ordering::SeqCst)
}

/// Convenience check used at each step of a capture: acquires the gate read
/// lock and verifies the job's ticket is still the live generation.
pub(crate) fn still_armed(shared: &Shared, gen: u64) -> bool {
    with_arm_read(shared, |arm| write_allowed(shared, arm, gen))
}

fn discard_capture_files(files: &[PathBuf]) {
    for f in files {
        let parent = f.parent().map(|p| p.to_path_buf());
        let _ = std::fs::remove_file(f);
        if let Some(p) = parent {
            let _ = std::fs::remove_dir(p);
        }
    }
}

/// Observe the live pause state and, if it transitioned since the last
/// observation, bump the privacy epoch and record the pause/resume event.
/// Returns the current pause reason (None = recording). This is the single
/// point where the epoch advances, so a capture/OCR/clip job that samples the
/// epoch at entry and re-checks it (or calls this) before commit cannot write
/// material captured across a privacy pause that began mid-work.
fn refresh_pause(shared: &Shared) -> Option<PauseReason> {
    let reason = evaluate_pause(shared);
    note_pause_change(shared, reason.clone());
    reason
}

/// Live recording check for a running job: arm generation still matches the
/// job's ticket AND no privacy transition has occurred since the job's epoch.
pub(crate) fn still_recording(shared: &Shared, gen: u64, epoch0: u64) -> bool {
    still_armed(shared, gen) && shared.privacy_epoch.load(Ordering::SeqCst) == epoch0
}

fn note_pause_change(shared: &Shared, reason: Option<PauseReason>) {
    let mut prev = shared.last_pause.lock().unwrap();
    if prev.as_ref() == reason.as_ref() {
        return;
    }
    // Every transition (pause on, pause off, pause-reason change) advances the
    // privacy epoch under this same lock, and invalidates any cached clip: a
    // clip copied in one recording window must never attach to a frame from a
    // different one.
    shared.privacy_epoch.fetch_add(1, Ordering::SeqCst);
    clipboard::clear_cached();
    // Do NOT write the pause/resume event here: entering a pause must not itself
    // produce a database write. Buffer the transition (with its real timestamp)
    // in memory; it is persisted later, only inside an authorized and currently
    // unpaused transaction (`flush_pending_gaps`). The buffer is bounded so a
    // long paused stretch cannot grow it without limit.
    {
        let mut gaps = shared.pending_gaps.lock().unwrap();
        let kind = if reason.is_some() { "pause" } else { "resume" };
        gaps.push((
            now_ms(),
            kind,
            reason.as_ref().map(|r| r.as_str()).unwrap_or("").to_string(),
        ));
        const MAX_PENDING_GAPS: usize = 512;
        let len = gaps.len();
        if len > MAX_PENDING_GAPS {
            gaps.drain(0..len - MAX_PENDING_GAPS);
        }
    }
    *prev = reason;
    emit(&Event::from_stats(stats_json(shared)));
}

/// Persist buffered pause/resume transitions, but only when the session is
/// currently recording (armed and not paused). This is the single place a gap
/// event reaches the database, so no write is ever a side effect of pausing.
/// Called from the capture loop while recording; a no-op when paused/disarmed
/// or when nothing is buffered.
fn flush_pending_gaps(shared: &Shared) {
    // Nothing to do unless there is buffered gap metadata AND we are recording
    // right now. `refresh_pause` observes the live state (and advances the epoch
    // on any freshly-seen transition), so a pause that engaged since the caller
    // last checked is caught here and blocks the flush.
    if shared.pending_gaps.lock().unwrap().is_empty() {
        return;
    }
    if refresh_pause(shared).is_some() {
        return;
    }
    with_arm_read(shared, |arm| {
        if !write_allowed(shared, arm, arm.gen) {
            return;
        }
        let pending: Vec<(i64, &'static str, String)> = {
            let mut g = shared.pending_gaps.lock().unwrap();
            std::mem::take(&mut *g)
        };
        if pending.is_empty() {
            return;
        }
        let wrote = shared.with_store_mut(|store| {
            if !store.is_writable() {
                return false;
            }
            for (ts, kind, reason) in &pending {
                let _ = store.record_event(*ts, *kind, reason.as_str());
            }
            true
        });
        // If the store was unavailable, put the transitions back so a later
        // authorized flush can persist them rather than dropping the gaps.
        if wrote != Some(true) {
            let mut g = shared.pending_gaps.lock().unwrap();
            let mut restored = pending;
            restored.append(&mut *g);
            *g = restored;
        }
    });
}

fn finalize_capture(
    shared: &Shared,
    settings: &Settings,
    insert: &FrameInsert,
    files: &[PathBuf],
    clients: &[hypr::Client],
    ticket: u64,
    epoch0: u64,
) -> Result<bool, String> {
    // Re-observe the live pause state right before committing (outside the gate,
    // since it does compositor IO): a pause that engaged during the preceding
    // encode/crop work bumps the epoch here, so the epoch check below rejects
    // the frame. `live.is_some()` also rejects a pause that is active *now*.
    let live = refresh_pause(shared);
    // Hold the gate read lock across the whole commit so a concurrent disarm
    // (which needs the write lock) cannot interleave between the authorization
    // check and the transaction commit.
    with_arm_read(shared, |arm| {
        let epoch_ok = shared.privacy_epoch.load(Ordering::SeqCst) == epoch0;
        if !write_allowed(shared, arm, ticket) || !epoch_ok || live.is_some() {
            discard_capture_files(files);
            return Ok(false);
        }
        // Attach only a clip committed under this frame's exact (generation,
        // epoch); `None` for anything stale or from a different recording
        // window, so a rejected/paused clip can never leak into search data.
        let clip = clipboard::latest_cached(ticket, epoch0).unwrap_or_default();
        // Re-checked at each step INSIDE the transaction: still armed under the
        // original ticket AND no privacy transition since the job's epoch.
        match shared.with_store_mut(|store| {
            store.commit_capture_tx(insert, clients, &clip, settings.byte_cap, || {
                write_allowed(shared, arm, ticket)
                    && shared.privacy_epoch.load(Ordering::SeqCst) == epoch0
            })
        }) {
            Some(Ok(true)) => {
                if write_allowed(shared, arm, ticket)
                    && shared.privacy_epoch.load(Ordering::SeqCst) == epoch0
                {
                    Ok(true)
                } else {
                    discard_capture_files(files);
                    Ok(false)
                }
            }
            Some(Ok(false)) => {
                discard_capture_files(files);
                Ok(false)
            }
            Some(Err(e)) => {
                discard_capture_files(files);
                Err(e)
            }
            None => {
                discard_capture_files(files);
                Err("store not initialized".into())
            }
        }
    })
}

fn capture_loop(shared: Arc<Shared>) {
    let mut session = capture::CaptureSession::new();
    let mut last_unchanged = false;
    loop {
        let settings = shared.settings.lock().unwrap().clone();
        let grim = session.using_grim();
        let base_ms = if grim {
            settings.cadence_ms.max(5000)
        } else {
            settings.cadence_ms.max(1000)
        };
        let wait = capture::next_cadence_ms(base_ms, last_unchanged);
        thread::sleep(Duration::from_millis(wait));

        *shared.capture_backend.lock().unwrap() = session.active_backend().to_string();

        let reason = evaluate_pause(&shared);
        note_pause_change(&shared, reason.clone());
        if reason.is_some() {
            last_unchanged = false;
            continue;
        }

        // Recording is live right now: persist any pause/resume transitions that
        // were buffered in memory while paused. This is the authorized, unpaused
        // window their gap metadata is allowed to reach the database.
        flush_pending_gaps(&shared);

        match capture_once(&shared, &settings, &mut session) {
            Ok(true) => last_unchanged = true,
            Ok(false) => last_unchanged = false,
            Err(err) => {
                last_unchanged = false;
                emit(&Event::error(None, err));
            }
        }
    }
}

fn capture_once(
    shared: &Shared,
    settings: &Settings,
    session: &mut capture::CaptureSession,
) -> Result<bool, String> {
    // Acquire the generation ticket at function entry, BEFORE any metadata
    // collection or the blocking screen grab. If disarmed now, capture nothing.
    // Reading the ticket late (after grab) would let a disarm→re-arm that
    // happens *during* the grab hand this pre-disarm frame a fresh valid
    // generation, so it would commit after a disarm — defeating the zero-write
    // contract. The original ticket flows through every later check and commit.
    let Some(gen) = arm_ticket(shared) else {
        return Ok(false);
    };
    // Sample the privacy epoch alongside the arm ticket, before any blocking
    // work. A lock/idle/overlay/portal/exclusion pause that *begins during* the
    // grab bumps the epoch (via `refresh_pause`), so `still_recording` and the
    // commit gate reject this frame even though the arm generation is unchanged.
    let epoch0 = shared.privacy_epoch.load(Ordering::SeqCst);
    let monitor = hypr::focused_monitor().unwrap_or_default();
    // Fail closed on an unresolved focused output. An empty output name would
    // make grim omit `-o` (capturing EVERY display, including a sensitive
    // secondary monitor) and the wlr backend fall back to the first output.
    // Capture nothing this tick rather than record the wrong display. The
    // synthetic test-capture backend ignores the output name, so it is exempt.
    if monitor.name.is_empty() && std::env::var("REWIND_TEST_CAPTURE").is_err() {
        return Ok(false);
    }
    let active = hypr::active_window().unwrap_or_default();
    let clients = hypr::clients().unwrap_or_default();
    let raw = session.grab(&monitor.name)?;
    // Re-evaluate pause immediately after the blocking grab: if a pause became
    // active while we were blocked, this bumps the epoch and returns Some, so we
    // drop the frame now rather than committing content from a paused moment.
    if refresh_pause(shared).is_some() || !still_recording(shared, gen, epoch0) {
        return Ok(false);
    }
    let hash = dhash::compute(&raw.rgba, raw.width, raw.height);
    let last = shared.last_dhash.load(Ordering::SeqCst);
    if last != 0 && last == hash && settings.skip_unchanged {
        shared.last_dhash.store(hash, Ordering::SeqCst);
        return Ok(true);
    }

    let scaled = encode::downscale_720p(&raw.rgba, raw.width, raw.height);
    if !still_recording(shared, gen, epoch0) {
        return Ok(false);
    }

    if !still_recording(shared, gen, epoch0) {
        return Ok(false);
    }
    ensure_store(shared)?;
    if !still_recording(shared, gen, epoch0) {
        return Ok(false);
    }

    let ts = now_ms();
    let stem = format!(
        "frames/{}/{}{}",
        day_stamp(ts),
        ts,
        if hash == last { "-d" } else { "" }
    );
    let dest = shared.paths.root.join(&stem);
    if let Some(parent) = dest.parent() {
        perms::secure_dir(parent).map_err(|e| e.to_string())?;
    }
    if !still_recording(shared, gen, epoch0) {
        return Ok(false);
    }
    let (enc, written) = encode::encode_frame(&scaled.bytes, scaled.width, scaled.height, &dest)?;
    let mut files = vec![written.clone()];
    if !still_recording(shared, gen, epoch0) {
        discard_capture_files(&files);
        return Ok(false);
    }
    perms::secure_file(&written).map_err(|e| e.to_string())?;
    let rel = written
        .strip_prefix(&shared.paths.root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| written.display().to_string());

    let mut crop_rel = None;
    let mut crop_x = 0i64;
    let mut crop_y = 0i64;
    let mut crop_w = raw.width as i64;
    let mut crop_h = raw.height as i64;
    if let Some(roi) = encode::crop_roi(&raw.rgba, raw.width, raw.height, &active, &monitor) {
        if !still_recording(shared, gen, epoch0) {
            discard_capture_files(&files);
            return Ok(false);
        }
        crop_x = roi.x as i64;
        crop_y = roi.y as i64;
        crop_w = roi.frame.width as i64;
        crop_h = roi.frame.height as i64;
        let crop_name = format!("crops/{ts}.png");
        let crop_path = shared.paths.root.join(&crop_name);
        if let Some(parent) = crop_path.parent() {
            perms::secure_dir(parent).map_err(|e| e.to_string())?;
        }
        encode::write_png(
            &roi.frame.bytes,
            roi.frame.width,
            roi.frame.height,
            &crop_path,
        )?;
        files.push(crop_path);
        crop_rel = Some(crop_name);
    }

    if !still_recording(shared, gen, epoch0) {
        discard_capture_files(&files);
        return Ok(false);
    }

    let bytes = std::fs::metadata(&written)
        .map(|m| m.len() as i64)
        .unwrap_or(0);
    let insert = FrameInsert {
        ts,
        path: rel,
        app: active.class.clone(),
        title: active.title.clone(),
        workspace: active.workspace.clone(),
        output: monitor.name.clone(),
        width: scaled.width as i64,
        height: scaled.height as i64,
        out_w: raw.width as i64,
        out_h: raw.height as i64,
        crop_x,
        crop_y,
        crop_w,
        crop_h,
        bytes,
        dhash: hash as i64,
        encoder: enc.as_str().to_string(),
        crop_path: crop_rel,
    };

    if !finalize_capture(shared, settings, &insert, &files, &clients, gen, epoch0)? {
        return Ok(false);
    }

    *shared.encoder.lock().unwrap() = enc.as_str().to_string();
    shared.last_dhash.store(hash, Ordering::SeqCst);
    shared.last_write_ms.store(ts as u64, Ordering::SeqCst);
    emit(&Event::frame_written(&insert));
    Ok(false)
}

fn day_stamp(ts: i64) -> String {
    let secs = ts / 1000;
    let days = secs / 86400;
    // Calendar day is only used for directory sharding; queries use raw ms.
    format!("{days}")
}

pub fn run_cli(args: &[String], data_dir: PathBuf) -> i32 {
    perms::install_umask();
    if args.is_empty() {
        if let Err(e) = run_daemon(data_dir) {
            eprintln!("rewindd: {e}");
            return 1;
        }
        return 0;
    }
    match args[0].as_str() {
        "daemon" => {
            if let Err(e) = run_daemon(data_dir) {
                eprintln!("rewindd: {e}");
                return 1;
            }
            0
        }
        "self-test" => match self_test() {
            Ok(()) => {
                println!("self-test ok");
                0
            }
            Err(e) => {
                eprintln!("self-test failed: {e}");
                1
            }
        },
        "wipe" => cli_wipe(&args[1..], &data_dir),
        "query" => cli_query(&args[1..], &data_dir),
        "stats" | "status" => cli_stats(&data_dir),
        "ldd-report" => cli_ldd(),
        "-h" | "--help" | "help" => {
            print_help();
            0
        }
        "--version" | "version" => {
            println!("rewindd {VERSION}");
            0
        }
        other => {
            eprintln!("rewindd: unknown command {other}");
            print_help();
            2
        }
    }
}

fn cli_wipe(args: &[String], data_dir: &Path) -> i32 {
    let scope = args.first().map(|s| s.as_str()).unwrap_or("today");
    let mut from = 0;
    let mut to = 0;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--from" if i + 1 < args.len() => {
                from = args[i + 1].parse().unwrap_or(0);
                i += 2;
            }
            "--to" if i + 1 < args.len() => {
                to = args[i + 1].parse().unwrap_or(0);
                i += 2;
            }
            _ => i += 1,
        }
    }
    match DataPaths::prepare(data_dir)
        .and_then(|p| Store::open(&p.db))
        .and_then(|mut s| s.wipe(scope, from, to))
    {
        Ok(v) => {
            println!("{v}");
            0
        }
        Err(e) => {
            eprintln!("rewindd wipe: {e}");
            1
        }
    }
}

fn cli_query(args: &[String], data_dir: &Path) -> i32 {
    let q = args.join(" ");
    let paths = DataPaths::bare(data_dir);
    if !paths.db.exists() {
        println!("{}", json!({"hits": [], "ocrAvailable": false, "query": q}));
        return 0;
    }
    match Store::open_read_only(&paths.db).and_then(|s| s.search(&q, 50, 0, 0)) {
        Ok(v) => {
            println!("{v}");
            0
        }
        Err(e) => {
            eprintln!("rewindd query: {e}");
            1
        }
    }
}

fn cli_stats(data_dir: &Path) -> i32 {
    let paths = DataPaths::bare(data_dir);
    if !paths.db.exists() {
        println!(
            "{}",
            json!({
                "frames": 0,
                "framesToday": 0,
                "bytes": 0,
                "encoder": encode::preferred_name(),
                "ocrAvailable": ocr::tesseract_available(),
                "capture": capture::backend_name(),
                "version": VERSION
            })
        );
        return 0;
    }
    match Store::open_read_only(&paths.db) {
        Ok(s) => {
            let snap = s.stats().unwrap_or_default();
            println!(
                "{}",
                json!({
                    "frames": snap.frames,
                    "framesToday": snap.frames_today,
                    "bytes": snap.bytes,
                    "encoder": encode::preferred_name(),
                    "ocrAvailable": ocr::tesseract_available(),
                    "capture": capture::backend_name(),
                    "version": VERSION
                })
            );
            0
        }
        Err(e) => {
            eprintln!("rewindd stats: {e}");
            1
        }
    }
}

fn cli_ldd() -> i32 {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("rewindd"));
    if cfg!(target_os = "linux") {
        let out = Proc::new("ldd")
            .arg(&exe)
            .stdout(Stdio::piped())
            .output();
        match out {
            Ok(o) => {
                print!("{}", String::from_utf8_lossy(&o.stdout));
                0
            }
            Err(e) => {
                eprintln!("ldd: {e}");
                1
            }
        }
    } else {
        println!("ldd unavailable on this OS; binary={}", exe.display());
        0
    }
}

fn print_help() {
    eprintln!(
        "rewindd {VERSION}\n\
         Local desktop recorder for Rewind. No network.\n\n\
         (no args) | daemon     NDJSON stdio daemon\n\
         wipe today|all|range [--from MS --to MS]\n\
         query TEXT\n\
         stats\n\
         ldd-report\n\
         self-test"
    );
}

// Shared is used by sibling modules through Arc.
impl Shared {
    pub fn data_paths(&self) -> &DataPaths {
        &self.paths
    }

    #[allow(dead_code)]
    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::SeqCst)
    }

    pub fn is_recording(&self) -> bool {
        evaluate_pause(self).is_none()
    }

    /// Current privacy epoch. A job samples this at entry and requires it to be
    /// unchanged at commit; any pause transition in between advances it.
    pub(crate) fn privacy_epoch(&self) -> u64 {
        self.privacy_epoch.load(Ordering::SeqCst)
    }

    pub fn is_idle(&self) -> bool {
        matches!(
            evaluate_pause(self),
            Some(PauseReason::Idle) | Some(PauseReason::Locked)
        )
    }

    /// Whether OCR may commit its extracted text right now. OCR's operating
    /// window is genuine idle (no user activity), so `Idle` — and fully live
    /// recording (`None`) — are permitted. Every OTHER pause reason (Locked,
    /// Overlay, Portal, Excluded, PrivateBrowsing, TitlePattern, Disarmed) is a
    /// hard pause during which no write may happen, so OCR must defer. This is
    /// distinct from `is_idle()`, which deliberately treats Locked as idle for
    /// the purpose of *running* tesseract cheaply — but a locked screen must not
    /// receive database writes.
    pub(crate) fn ocr_may_write(&self) -> bool {
        ocr_write_ok(&evaluate_pause(self))
    }

    pub fn with_store<R>(&self, f: impl FnOnce(&Store) -> R) -> Option<R> {
        let g = self.store.lock().unwrap();
        g.as_ref().map(f)
    }

    pub fn with_store_mut<R>(&self, f: impl FnOnce(&mut Store) -> R) -> Option<R> {
        let mut g = self.store.lock().unwrap();
        match g.as_mut() {
            Some(s) if s.is_writable() => Some(f(s)),
            _ => None,
        }
    }

    fn fire_tripwire(&self) {
        let hook = self.tripwire.lock().unwrap().clone();
        if let Some(h) = hook {
            h();
        }
    }

    #[cfg(test)]
    fn set_tripwire(&self, f: impl Fn() + Send + Sync + 'static) {
        *self.tripwire.lock().unwrap() = Some(Arc::new(f));
    }

    #[cfg(test)]
    fn clear_tripwire(&self) {
        *self.tripwire.lock().unwrap() = None;
    }
}

// clipboard/ocr modules need Shared
pub(crate) use Shared as DaemonState;

/// Serializes tests that toggle the process-global `REWIND_TEST_CAPTURE` env
/// var (capture/clipboard race tests), so they can't observe each other's
/// setting under the default parallel test runner.
#[cfg(test)]
pub(crate) static TEST_CAPTURE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn list_files(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        if !root.exists() {
            return out;
        }
        fn walk(dir: &Path, acc: &mut Vec<String>, root: &Path) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, acc, root);
                } else if let Ok(rel) = path.strip_prefix(root) {
                    acc.push(rel.display().to_string());
                }
            }
        }
        walk(root, &mut out, root);
        out.sort();
        out
    }

    #[test]
    fn disarmed_boot_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        assert!(!data.exists() || list_files(&data).is_empty());
        assert!(shared.store.lock().unwrap().is_none());

        handle_command(&shared, Command::Hello { id: 1 });
        handle_command(&shared, Command::Stats { id: 2 });
        handle_command(
            &shared,
            Command::Timeline {
                id: 3,
                from: 0,
                to: 0,
                limit: 400,
            },
        );
        handle_command(
            &shared,
            Command::Clips {
                id: 4,
                limit: 50,
            },
        );
        handle_command(
            &shared,
            Command::Query {
                id: 5,
                q: "hello".into(),
                limit: 10,
                from: 0,
                to: 0,
            },
        );
        handle_command(
            &shared,
            Command::Configure {
                id: 6,
                settings: json!({"byteCapGb": 2, "cadenceMs": 3000}),
            },
        );
        handle_command(
            &shared,
            Command::Arm {
                id: 7,
                settings: json!({"cmd": "arm"}),
            },
        );
        handle_command(&shared, Command::Disarm { id: 8 });
        handle_command(
            &shared,
            Command::Wipe {
                id: 9,
                scope: "today".into(),
                from: 0,
                to: 0,
            },
        );
        handle_command(&shared, Command::Shutdown { id: 10 });

        assert!(
            !data.exists() || list_files(&data).is_empty(),
            "disarmed launch wrote: {:?}",
            list_files(&data)
        );
        assert!(!shared.paths.db.exists());
        assert!(!shared.paths.state.exists());
        assert!(shared.store.lock().unwrap().is_none());
    }

    #[test]
    fn consent_then_arm_creates_store() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        {
            let mut st = shared.settings.lock().unwrap();
            st.consent_at = now_ms();
            st.armed = true;
        }
        persist_consent(&shared).unwrap();
        arm_now(&shared).unwrap();
        assert!(shared.paths.state.exists());
        assert!(shared.paths.db.exists());
        assert!(shared.store.lock().unwrap().is_some());
        assert!(list_files(&data).iter().any(|p| p.ends_with("state.json")));
    }

    fn content_fingerprint(root: &Path) -> Vec<(String, u64, u64)> {
        let mut out = Vec::new();
        if !root.exists() {
            return out;
        }
        fn walk(dir: &Path, acc: &mut Vec<(String, u64, u64)>, root: &Path) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, acc, root);
                    continue;
                }
                let Ok(rel) = path.strip_prefix(root) else {
                    continue;
                };
                let bytes = fs::read(&path).unwrap_or_default();
                let mut h = 0xcbf29ce484222325u64;
                for b in &bytes {
                    h ^= *b as u64;
                    h = h.wrapping_mul(0x100000001b3);
                }
                acc.push((rel.display().to_string(), bytes.len() as u64, h));
            }
        }
        walk(root, &mut out, root);
        out.sort();
        out
    }

    fn seed_archive(data: &Path) -> FrameInsert {
        DataPaths::prepare(data).unwrap();
        let mut store = Store::open(&data.join("rewind.db")).unwrap();
        fs::create_dir_all(data.join("crops")).unwrap();
        fs::create_dir_all(data.join("frames")).unwrap();
        let crop_rel = "crops/pending.png";
        fs::write(data.join(crop_rel), b"fake-crop-png").unwrap();
        fs::write(data.join("frames/old.png"), b"fake-frame").unwrap();
        let f = FrameInsert {
            ts: 1_700_000_000_000,
            path: "frames/old.png".into(),
            app: "kitty".into(),
            title: "seed".into(),
            workspace: "1".into(),
            output: "eDP-1".into(),
            width: 1280,
            height: 720,
            out_w: 1920,
            out_h: 1080,
            crop_x: 10,
            crop_y: 20,
            crop_w: 100,
            crop_h: 80,
            bytes: 10,
            dhash: 1,
            encoder: "png".into(),
            crop_path: Some(crop_rel.into()),
        };
        store.insert_frame(&f).unwrap();
        store.record_event(f.ts, "pause", "overlay").unwrap();
        store.checkpoint().unwrap();
        drop(store);
        let mut settings = Settings::default();
        settings.consent_at = 1;
        settings.armed = false;
        settings.arm_on_login = false;
        settings.save(&data.join("state.json")).unwrap();
        fs::write(data.join("ui.json"), "{\"armed\":false}\n").unwrap();
        f
    }

    #[test]
    fn existing_archive_stays_frozen_while_disarmed() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let seeded = seed_archive(&data);
        let before_files = content_fingerprint(&data);
        let before_names = list_files(&data);

        let shared = boot_shared(&data).unwrap();
        assert!(!shared.armed.load(Ordering::SeqCst));
        let after_boot = content_fingerprint(&data);
        assert_eq!(
            before_files, after_boot,
            "boot_shared mutated a disarmed archive:\nbefore={before_files:?}\nafter={after_boot:?}"
        );
        assert_eq!(before_names, list_files(&data));

        let before_counts = shared
            .with_store(|s| s.mutation_counters().unwrap())
            .unwrap();
        let before_pending = shared
            .with_store(|s| s.pending_crop_count().unwrap())
            .unwrap();
        assert!(before_pending >= 1);

        handle_command(&shared, Command::Disarm { id: 1 });
        handle_command(
            &shared,
            Command::Configure {
                id: 2,
                settings: json!({"byteCapGb": 4, "cadenceMs": 5000}),
            },
        );
        persist_settings(&shared);
        note_pause_change(&shared, evaluate_pause(&shared));
        assert_eq!(ocr::process_pending(&shared), 0);
        assert!(!shared.is_idle());
        clipboard::ingest(&shared, "should-not-store");

        let after_files = content_fingerprint(&data);
        let after_counts = shared
            .with_store(|s| s.mutation_counters().unwrap())
            .unwrap();
        let after_pending = shared
            .with_store(|s| s.pending_crop_count().unwrap())
            .unwrap();
        assert_eq!(
            before_files, after_files,
            "disarmed session mutated files:\nbefore={before_files:?}\nafter={after_files:?}"
        );
        assert_eq!(before_names, list_files(&data));
        assert_eq!(before_counts, after_counts);
        assert_eq!(before_pending, after_pending);
        assert!(data.join("crops/pending.png").exists());
        assert_eq!(seeded.path, "frames/old.png");
        assert!(shared
            .with_store(|s| !s.is_writable())
            .unwrap_or(true));
    }

    #[test]
    fn disarm_during_capture_discards_files_and_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        {
            let mut st = shared.settings.lock().unwrap();
            st.consent_at = now_ms();
        }
        persist_consent(&shared).unwrap();
        arm_now(&shared).unwrap();
        let ticket = arm_ticket(&shared).expect("armed");

        let frame_rel = "frames/1/in-flight.webp";
        let crop_rel = "crops/in-flight.png";
        let written = data.join(frame_rel);
        let crop = data.join(crop_rel);
        fs::create_dir_all(written.parent().unwrap()).unwrap();
        fs::create_dir_all(crop.parent().unwrap()).unwrap();
        fs::write(&written, b"in-flight-frame").unwrap();
        fs::write(&crop, b"in-flight-crop").unwrap();

        let insert = FrameInsert {
            ts: 99,
            path: frame_rel.into(),
            app: "kitty".into(),
            title: "race".into(),
            workspace: "1".into(),
            output: "eDP-1".into(),
            width: 32,
            height: 24,
            out_w: 32,
            out_h: 24,
            crop_x: 0,
            crop_y: 0,
            crop_w: 32,
            crop_h: 24,
            bytes: 15,
            dhash: 9,
            encoder: "png".into(),
            crop_path: Some(crop_rel.into()),
        };
        let settings = shared.settings.lock().unwrap().clone();

        // Disarm through the real path (drops the writable store, bumps gen).
        disarm_now(&shared);

        let committed = finalize_capture(
            &shared,
            &settings,
            &insert,
            &[written.clone(), crop.clone()],
            &[],
            ticket,
        )
        .unwrap();
        assert!(!committed);
        assert!(!written.exists(), "disarm must discard the in-flight frame");
        assert!(!crop.exists(), "disarm must discard the in-flight crop");
        let counts = shared
            .with_store(|s| s.mutation_counters().unwrap())
            .unwrap();
        assert_eq!(counts.0, 0, "no frame rows");
        assert_eq!(counts.1, 0, "no event rows");
    }

    #[test]
    fn capture_grab_spanning_disarm_rearm_does_not_commit() {
        // The ticket is taken at capture_once entry, BEFORE the blocking grab.
        // If a disarm→re-arm happens *during* grab (fresh generation, armed
        // again by the time the post-grab checks run), the pre-disarm frame
        // must still be rejected. With the old ordering (ticket read after
        // grab) this frame would commit under the re-armed generation — the
        // exact race this guards against.
        let _env = TEST_CAPTURE_ENV_LOCK.lock().unwrap();
        std::env::set_var("REWIND_TEST_CAPTURE", "1");
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        {
            let mut st = shared.settings.lock().unwrap();
            st.consent_at = now_ms();
        }
        persist_consent(&shared).unwrap();
        arm_now(&shared).unwrap();
        let settings = shared.settings.lock().unwrap().clone();

        let mut session = capture::CaptureSession::new();
        // Fired at the start of grab: disarm then re-arm, so the generation
        // advances twice and `armed` is true again when grab returns.
        let hook_shared = shared.clone();
        session.set_grab_hook(Box::new(move || {
            disarm_now(&hook_shared);
            arm_now(&hook_shared).unwrap();
        }));

        let kept = capture_once(&shared, &settings, &mut session).unwrap();
        assert!(
            !kept,
            "a capture whose grab spanned disarm/re-arm must not report a kept frame"
        );

        let counts = shared
            .with_store(|s| s.mutation_counters().unwrap())
            .unwrap();
        assert_eq!(
            counts.0, 0,
            "no frame rows for a capture whose grab spanned disarm/re-arm"
        );
        assert_eq!(counts.1, 0, "no event rows either");
        std::env::remove_var("REWIND_TEST_CAPTURE");
    }

    #[test]
    fn armed_unpaused_capture_writes_a_frame() {
        // Guard against the privacy-epoch/pause rechecks over-rejecting: a
        // normal armed capture with no pause must still commit exactly one
        // frame.
        let _env = TEST_CAPTURE_ENV_LOCK.lock().unwrap();
        std::env::set_var("REWIND_TEST_CAPTURE", "1");
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        {
            let mut st = shared.settings.lock().unwrap();
            st.consent_at = now_ms();
        }
        persist_consent(&shared).unwrap();
        arm_now(&shared).unwrap();
        let settings = shared.settings.lock().unwrap().clone();

        let mut session = capture::CaptureSession::new();
        let _ = capture_once(&shared, &settings, &mut session).unwrap();

        let counts = shared
            .with_store(|s| s.mutation_counters().unwrap())
            .unwrap();
        assert_eq!(counts.0, 1, "an armed, unpaused capture must write one frame");
        std::env::remove_var("REWIND_TEST_CAPTURE");
    }

    #[test]
    fn capture_privacy_pause_during_grab_does_not_commit() {
        // A NON-disarm privacy pause (here: the overlay opening) that begins
        // *during* the blocking grab must drop the frame. The arm generation is
        // unchanged across an overlay pause, so this is caught only by the
        // privacy-epoch/live-pause recheck, not by the arm-generation guard.
        let _env = TEST_CAPTURE_ENV_LOCK.lock().unwrap();
        std::env::set_var("REWIND_TEST_CAPTURE", "1");
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        {
            let mut st = shared.settings.lock().unwrap();
            st.consent_at = now_ms();
        }
        persist_consent(&shared).unwrap();
        arm_now(&shared).unwrap();
        let settings = shared.settings.lock().unwrap().clone();

        let mut session = capture::CaptureSession::new();
        // Overlay opens mid-grab — a privacy pause with the SAME arm generation.
        let hook_shared = shared.clone();
        session.set_grab_hook(Box::new(move || {
            hook_shared.overlay_open.store(true, Ordering::SeqCst);
        }));

        let kept = capture_once(&shared, &settings, &mut session).unwrap();
        assert!(!kept, "a frame grabbed as the overlay opened must be dropped");

        let counts = shared
            .with_store(|s| s.mutation_counters().unwrap())
            .unwrap();
        assert_eq!(
            counts.0, 0,
            "no frame rows for a capture whose grab spanned an overlay pause"
        );
        std::env::remove_var("REWIND_TEST_CAPTURE");
    }

    #[test]
    fn clip_cache_cleared_and_not_attached_across_privacy_pause() {
        // A clip committed in one recording window must not attach to a frame in
        // another: any privacy transition advances the epoch and clears the
        // cache, so `latest_cached` for the old (gen, epoch) yields nothing.
        let _env = TEST_CAPTURE_ENV_LOCK.lock().unwrap();
        std::env::set_var("REWIND_TEST_CAPTURE", "1");
        clipboard::clear_cached();
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        {
            let mut st = shared.settings.lock().unwrap();
            st.consent_at = now_ms();
        }
        persist_consent(&shared).unwrap();
        arm_now(&shared).unwrap();

        let gen = arm_ticket(&shared).unwrap();
        let epoch = shared.privacy_epoch();
        clipboard::ingest(&shared, "AKIA-DISTINCTIVE-WINDOW-CLIP");
        assert_eq!(
            clipboard::latest_cached(gen, epoch).as_deref(),
            Some("AKIA-DISTINCTIVE-WINDOW-CLIP"),
            "clip should be cached under its own (gen, epoch) right after commit"
        );

        // A privacy transition (overlay on, then off) advances the epoch and
        // clears the cache.
        shared.overlay_open.store(true, Ordering::SeqCst);
        note_pause_change(&shared, evaluate_pause(&shared));
        shared.overlay_open.store(false, Ordering::SeqCst);
        note_pause_change(&shared, evaluate_pause(&shared));

        assert!(
            shared.privacy_epoch() != epoch,
            "epoch must advance across a pause transition"
        );
        assert!(
            clipboard::latest_cached(gen, epoch).is_none(),
            "stale clip must not be retrievable for the pre-transition window"
        );
        clipboard::clear_cached();
        std::env::remove_var("REWIND_TEST_CAPTURE");
    }

    #[test]
    fn disarmed_is_not_ocr_idle() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        assert!(!shared.is_armed());
        assert!(!shared.is_idle());
        assert!(!recording_now(&shared));
    }

    #[test]
    fn arm_then_disarm_drops_writable_store_and_freezes_fs() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        {
            let mut st = shared.settings.lock().unwrap();
            st.consent_at = now_ms();
        }
        persist_consent(&shared).unwrap();
        arm_now(&shared).unwrap();
        assert!(shared
            .with_store(|s| s.is_writable())
            .unwrap_or(false));
        shared
            .with_store_mut(|s| {
                s.insert_frame(&FrameInsert {
                    ts: 11,
                    path: "frames/a.png".into(),
                    app: "kitty".into(),
                    title: "t".into(),
                    workspace: "1".into(),
                    output: "eDP-1".into(),
                    width: 8,
                    height: 8,
                    out_w: 8,
                    out_h: 8,
                    crop_x: 0,
                    crop_y: 0,
                    crop_w: 8,
                    crop_h: 8,
                    bytes: 4,
                    dhash: 1,
                    encoder: "png".into(),
                    crop_path: None,
                })
            })
            .unwrap()
            .unwrap();
        shared.with_store(|s| s.checkpoint().unwrap());
        let before = content_fingerprint(&data);
        disarm_now(&shared);
        assert!(!shared.armed.load(Ordering::SeqCst));
        assert!(shared
            .with_store(|s| !s.is_writable())
            .unwrap_or(true));
        let after_disarm = content_fingerprint(&data);
        handle_command(
            &shared,
            Command::Configure {
                id: 1,
                settings: json!({"cadenceMs": 4000}),
            },
        );
        assert_eq!(ocr::process_pending(&shared), 0);
        clipboard::ingest(&shared, "nope");
        assert_eq!(
            after_disarm,
            content_fingerprint(&data),
            "disarmed lifecycle mutated files after exclusive drop"
        );
        let _ = before;
    }

    #[test]
    fn persist_consent_failure_rolls_back() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir(data.join("state.json")).unwrap();
        let shared = boot_shared(&data).unwrap();
        handle_command(
            &shared,
            Command::Consent {
                id: 1,
                arm_now: false,
                arm_on_login: false,
            },
        );
        assert_eq!(shared.settings.lock().unwrap().consent_at, 0);
        assert!(!shared.armed.load(Ordering::SeqCst));
    }

    #[test]
    fn ensure_store_failure_restores_disarmed_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        {
            let mut st = shared.settings.lock().unwrap();
            st.consent_at = now_ms();
        }
        persist_consent(&shared).unwrap();
        fs::create_dir_all(shared.paths.db.clone()).unwrap();
        let err = arm_now(&shared);
        assert!(err.is_err(), "{err:?}");
        assert!(!shared.armed.load(Ordering::SeqCst));
        assert!(!shared.settings.lock().unwrap().armed);
        assert!(!shared.gate.read().unwrap().armed);
    }

    #[test]
    fn cli_query_and_stats_do_not_create_storage() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("missing");
        assert_eq!(cli_query(&["hello".into()], &data), 0);
        assert_eq!(cli_stats(&data), 0);
        assert!(!data.exists());
    }

    #[test]
    fn keep_disarmed_consent_persists_across_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        handle_command(
            &shared,
            Command::Consent {
                id: 1,
                arm_now: false,
                arm_on_login: false,
            },
        );
        assert!(!shared.armed.load(Ordering::SeqCst));
        assert!(shared.paths.state.exists());
        let at = shared.settings.lock().unwrap().consent_at;
        assert!(at > 0);
        drop(shared);

        let again = boot_shared(&data).unwrap();
        let st = again.settings.lock().unwrap().clone();
        assert!(st.consent_at > 0);
        assert!(!st.arm_on_login);
        assert!(!st.armed);
        assert!(!again.armed.load(Ordering::SeqCst));
        assert!(again
            .with_store(|s| s.is_writable())
            .unwrap_or(false)
            == false);
    }

    #[test]
    fn concurrent_disarm_during_gated_commit_writes_nothing() {
        use std::sync::Barrier;
        use std::thread;

        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        {
            let mut st = shared.settings.lock().unwrap();
            st.consent_at = now_ms();
        }
        persist_consent(&shared).unwrap();
        arm_now(&shared).unwrap();

        let written = data.join("frames/1/in-flight.webp");
        fs::create_dir_all(written.parent().unwrap()).unwrap();
        fs::write(&written, b"in-flight").unwrap();
        let insert = FrameInsert {
            ts: 99,
            path: "frames/1/in-flight.webp".into(),
            app: "kitty".into(),
            title: "race".into(),
            workspace: "1".into(),
            output: "eDP-1".into(),
            width: 8,
            height: 8,
            out_w: 8,
            out_h: 8,
            crop_x: 0,
            crop_y: 0,
            crop_w: 8,
            crop_h: 8,
            bytes: 9,
            dhash: 1,
            encoder: "png".into(),
            crop_path: None,
        };
        let parked = Arc::new(Barrier::new(2));
        let writer = {
            let shared = Arc::clone(&shared);
            let parked = Arc::clone(&parked);
            let written = written.clone();
            let ticket = arm_ticket(&shared).expect("armed at start");
            thread::spawn(move || {
                let committed = with_arm_read(&shared, |arm| {
                    if !write_allowed(&shared, arm, ticket) {
                        discard_capture_files(&[written.clone()]);
                        return false;
                    }
                    let mut n = 0;
                    shared
                        .with_store_mut(|store| {
                            store.commit_capture_tx(&insert, &[], "", 1_000_000, || {
                                n += 1;
                                if n >= 2 {
                                    parked.wait();
                                    false
                                } else {
                                    true
                                }
                            })
                        })
                        .unwrap_or(Ok(false))
                        .unwrap_or(false)
                });
                if !committed {
                    discard_capture_files(&[written]);
                }
                committed
            })
        };
        let disarmer = {
            let shared = Arc::clone(&shared);
            let parked = Arc::clone(&parked);
            thread::spawn(move || {
                parked.wait();
                disarm_now(&shared);
            })
        };
        let committed = writer.join().expect("writer");
        disarmer.join().expect("disarmer");
        assert!(!committed);
        assert!(!written.exists());
        assert!(!shared.armed.load(Ordering::SeqCst));
        assert!(shared
            .with_store(|s| !s.is_writable())
            .unwrap_or(true));
        let counts = shared
            .with_store(|s| s.mutation_counters().unwrap())
            .unwrap();
        assert_eq!(counts.0, 0);
    }

    #[test]
    fn stale_ticket_after_disarm_rearm_cannot_commit() {
        // A capture job takes its ticket while armed, then the user disarms and
        // re-arms (a fresh generation). The stale job must NOT commit, even
        // though the daemon is armed again by the time it reaches finalize.
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        {
            let mut st = shared.settings.lock().unwrap();
            st.consent_at = now_ms();
        }
        persist_consent(&shared).unwrap();
        arm_now(&shared).unwrap();

        // Ticket issued under the first armed generation.
        let ticket = arm_ticket(&shared).expect("armed");

        // Disarm then re-arm: generation advances twice; armed is true again.
        disarm_now(&shared);
        arm_now(&shared).unwrap();
        assert!(shared.armed.load(Ordering::SeqCst));
        assert_ne!(
            arm_ticket(&shared).expect("re-armed"),
            ticket,
            "generation must advance across disarm/re-arm"
        );

        // The stale job's authorization must be denied despite being armed.
        assert!(
            !still_armed(&shared, ticket),
            "stale ticket must not authorize a write after disarm/re-arm"
        );

        let written = data.join("frames/1/stale.webp");
        fs::create_dir_all(written.parent().unwrap()).unwrap();
        fs::write(&written, b"stale").unwrap();
        let insert = FrameInsert {
            ts: 123,
            path: "frames/1/stale.webp".into(),
            app: "kitty".into(),
            title: "stale".into(),
            workspace: "1".into(),
            output: "eDP-1".into(),
            width: 8,
            height: 8,
            out_w: 8,
            out_h: 8,
            crop_x: 0,
            crop_y: 0,
            crop_w: 8,
            crop_h: 8,
            bytes: 5,
            dhash: 1,
            encoder: "png".into(),
            crop_path: None,
        };
        let settings = shared.settings.lock().unwrap().clone();
        let committed =
            finalize_capture(&shared, &settings, &insert, &[written.clone()], &[], ticket).unwrap();
        assert!(!committed, "stale-ticket finalize must not commit");
        assert!(!written.exists(), "stale capture file must be discarded");
        let counts = shared
            .with_store(|s| s.mutation_counters().unwrap())
            .unwrap();
        assert_eq!(counts.0, 0, "no rows written for a stale-ticket job");
    }

    #[test]
    fn disarmed_wipe_is_user_authorized_and_does_not_rearm() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        {
            let mut st = shared.settings.lock().unwrap();
            st.consent_at = now_ms();
        }
        persist_consent(&shared).unwrap();
        arm_now(&shared).unwrap();
        shared
            .with_store_mut(|s| {
                s.insert_frame(&FrameInsert {
                    ts: now_ms(),
                    path: "frames/x.png".into(),
                    app: "kitty".into(),
                    title: "t".into(),
                    workspace: "1".into(),
                    output: "eDP-1".into(),
                    width: 8,
                    height: 8,
                    out_w: 8,
                    out_h: 8,
                    crop_x: 0,
                    crop_y: 0,
                    crop_w: 8,
                    crop_h: 8,
                    bytes: 4,
                    dhash: 1,
                    encoder: "png".into(),
                    crop_path: None,
                })
            })
            .unwrap()
            .unwrap();
        disarm_now(&shared);
        assert!(!shared.armed.load(Ordering::SeqCst));
        let reply = authorized_wipe(&shared, "all", 0, 0).unwrap();
        assert!(reply.get("wiped").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);
        assert!(!shared.armed.load(Ordering::SeqCst));
        assert!(!shared.settings.lock().unwrap().armed);
        assert!(shared
            .with_store(|s| !s.is_writable())
            .unwrap_or(true));
        let counts = shared
            .with_store(|s| s.mutation_counters().unwrap())
            .unwrap();
        assert_eq!(counts.0, 0);
    }

    #[test]
    fn ocr_and_clipboard_roll_back_when_disarmed_mid_transaction() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        {
            let mut st = shared.settings.lock().unwrap();
            st.consent_at = now_ms();
        }
        persist_consent(&shared).unwrap();
        arm_now(&shared).unwrap();
        // Gate is now armed at generation 1; the tripwire below simulates a
        // mid-transaction disarm by flipping the atomic mirror, which
        // write_allowed observes and rejects.
        let frame = FrameInsert {
            ts: 50,
            path: "frames/old.png".into(),
            app: "kitty".into(),
            title: "seed".into(),
            workspace: "1".into(),
            output: "eDP-1".into(),
            width: 8,
            height: 8,
            out_w: 8,
            out_h: 8,
            crop_x: 0,
            crop_y: 0,
            crop_w: 8,
            crop_h: 8,
            bytes: 4,
            dhash: 1,
            encoder: "png".into(),
            crop_path: Some("crops/p.png".into()),
        };
        shared
            .with_store_mut(|s| s.insert_frame(&frame))
            .unwrap()
            .unwrap();

        let hits = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let hits_ocr = Arc::clone(&hits);
        let s = Arc::clone(&shared);
        shared.set_tripwire(move || {
            if hits_ocr.fetch_add(1, Ordering::SeqCst) >= 1 {
                s.arm_gen.fetch_add(1, Ordering::SeqCst);
                s.armed.store(false, Ordering::SeqCst);
            }
        });
        let boxes = [crate::query::WordBox {
            word: "x".into(),
            x: 0.0,
            y: 0.0,
            w: 0.1,
            h: 0.1,
        }];
        let ocr_ok = shared
            .with_store_mut(|store| {
                store.commit_ocr_tx(50, "x", "kitty", "seed", "", &boxes, || {
                    still_armed(&shared, 1)
                })
            })
            .unwrap()
            .unwrap();
        assert!(!ocr_ok);

        shared.armed.store(true, Ordering::SeqCst);
        shared.arm_gen.store(1, Ordering::SeqCst);
        hits.store(0, Ordering::SeqCst);
        clipboard::ingest(&shared, "clipboard-secret");
        shared.clear_tripwire();

        let counts = shared
            .with_store(|s| s.mutation_counters().unwrap())
            .unwrap();
        assert_eq!(counts.2, 0, "no ocr boxes");
        assert_eq!(counts.3, 0, "no clips");
    }

    #[test]
    fn ocr_write_ok_blocks_locked_and_hard_pauses() {
        // OCR may write only while genuinely recording or idle; every hard pause
        // — above all a locked screen — must block the OCR commit.
        assert!(ocr_write_ok(&None));
        assert!(ocr_write_ok(&Some(PauseReason::Idle)));
        for hard in [
            PauseReason::Locked,
            PauseReason::Overlay,
            PauseReason::Portal,
            PauseReason::Excluded,
            PauseReason::PrivateBrowsing,
            PauseReason::TitlePattern,
            PauseReason::Disarmed,
        ] {
            assert!(
                !ocr_write_ok(&Some(hard.clone())),
                "{hard:?} must block an OCR write"
            );
        }
    }

    #[test]
    fn ocr_may_write_false_while_hard_paused() {
        // A live overlay pause is compositor-independent (checked before the
        // test-capture shortcut), so it deterministically exercises the live
        // OCR gate: OCR must not be permitted to write while paused.
        let _env = TEST_CAPTURE_ENV_LOCK.lock().unwrap();
        std::env::set_var("REWIND_TEST_CAPTURE", "1");
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        {
            let mut st = shared.settings.lock().unwrap();
            st.consent_at = now_ms();
        }
        persist_consent(&shared).unwrap();
        arm_now(&shared).unwrap();
        assert!(shared.ocr_may_write(), "recording session may OCR-write");
        shared.overlay_open.store(true, Ordering::SeqCst);
        assert!(
            !shared.ocr_may_write(),
            "a hard pause (overlay) must block OCR writes"
        );
        shared.overlay_open.store(false, Ordering::SeqCst);
        std::env::remove_var("REWIND_TEST_CAPTURE");
    }

    #[test]
    fn pause_transition_does_not_write_to_store() {
        // Entering (or leaving) a pause must not itself write to the database.
        // The transition is buffered in memory; the events table is untouched
        // until an authorized, unpaused flush.
        let _env = TEST_CAPTURE_ENV_LOCK.lock().unwrap();
        std::env::set_var("REWIND_TEST_CAPTURE", "1");
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        {
            let mut st = shared.settings.lock().unwrap();
            st.consent_at = now_ms();
        }
        persist_consent(&shared).unwrap();
        arm_now(&shared).unwrap();

        let before = shared
            .with_store(|s| s.mutation_counters().unwrap())
            .unwrap();
        // Simulate a pause transition (recording -> paused).
        note_pause_change(&shared, Some(PauseReason::Overlay));
        let after = shared
            .with_store(|s| s.mutation_counters().unwrap())
            .unwrap();
        assert_eq!(
            before.1, after.1,
            "a pause transition must not write an events row"
        );
        assert!(
            !shared.pending_gaps.lock().unwrap().is_empty(),
            "the transition must be buffered in memory"
        );
        std::env::remove_var("REWIND_TEST_CAPTURE");
    }

    #[test]
    fn flush_pending_gaps_persists_only_when_recording() {
        // Buffered gaps reach the database only through an authorized, unpaused
        // flush — never as a side effect of pausing.
        let _env = TEST_CAPTURE_ENV_LOCK.lock().unwrap();
        std::env::set_var("REWIND_TEST_CAPTURE", "1");
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        {
            let mut st = shared.settings.lock().unwrap();
            st.consent_at = now_ms();
        }
        persist_consent(&shared).unwrap();
        arm_now(&shared).unwrap();

        // Buffer a transition; the events table stays empty.
        note_pause_change(&shared, Some(PauseReason::Overlay));
        let mid = shared
            .with_store(|s| s.mutation_counters().unwrap())
            .unwrap();
        assert_eq!(mid.1, 0, "no events written on pause");
        assert!(!shared.pending_gaps.lock().unwrap().is_empty());

        // Recording is live (overlay_open never set, test-capture active), so a
        // flush persists the buffered gap(s) and drains the buffer.
        flush_pending_gaps(&shared);
        let after = shared
            .with_store(|s| s.mutation_counters().unwrap())
            .unwrap();
        assert!(after.1 >= 1, "flush must persist buffered gap events");
        assert!(
            shared.pending_gaps.lock().unwrap().is_empty(),
            "flush must drain the buffer"
        );
    }

    #[test]
    fn clipboard_commit_blocked_by_pause_engaged_after_ticket() {
        // A pause that engages after the clipboard ticket is taken but before the
        // commit must block the write: no clip row, nothing cached.
        let _env = TEST_CAPTURE_ENV_LOCK.lock().unwrap();
        std::env::set_var("REWIND_TEST_CAPTURE", "1");
        clipboard::clear_cached();
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("rewind");
        let shared = boot_shared(&data).unwrap();
        {
            let mut st = shared.settings.lock().unwrap();
            st.consent_at = now_ms();
        }
        persist_consent(&shared).unwrap();
        arm_now(&shared).unwrap();

        // Engage a hard pause (overlay) that ingest's pre-commit refresh_pause
        // will observe, then ingest a clip.
        shared.overlay_open.store(true, Ordering::SeqCst);
        clipboard::ingest(&shared, "AKIA-SHOULD-NOT-STORE");

        let counts = shared
            .with_store(|s| s.mutation_counters().unwrap())
            .unwrap();
        assert_eq!(counts.3, 0, "no clip row may be written while paused");
        shared.overlay_open.store(false, Ordering::SeqCst);
        clipboard::clear_cached();
        std::env::remove_var("REWIND_TEST_CAPTURE");
    }
}
