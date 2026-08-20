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
use std::sync::{Arc, Mutex};
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

pub(crate) struct Shared {
    settings: Mutex<Settings>,
    store: Mutex<Store>,
    paths: DataPaths,
    armed: AtomicBool,
    overlay_open: AtomicBool,
    last_dhash: AtomicU64,
    last_write_ms: AtomicU64,
    encoder: Mutex<String>,
    last_pause: Mutex<Option<PauseReason>>,
}

pub fn run_daemon(data_dir: PathBuf) -> io::Result<()> {
    perms::install_umask();
    let paths = DataPaths::prepare(&data_dir).map_err(io::Error::other)?;
    let store = Store::open(&paths.db).map_err(io::Error::other)?;
    let settings = Settings::load(&paths.state).unwrap_or_default();
    let armed = settings.armed && settings.consent_at > 0;
    let shared = Arc::new(Shared {
        settings: Mutex::new(settings),
        store: Mutex::new(store),
        paths,
        armed: AtomicBool::new(armed),
        overlay_open: AtomicBool::new(false),
        last_dhash: AtomicU64::new(0),
        last_write_ms: AtomicU64::new(0),
        encoder: Mutex::new(encode::preferred_name().to_string()),
        last_pause: Mutex::new(None),
    });

    emit(&Event::ready(
        armed,
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
            let mut st = shared.settings.lock().unwrap();
            if st.consent_at == 0 {
                st.consent_at = now_ms();
            }
            st.armed = true;
            let _ = st.save(&shared.paths.state);
            drop(st);
            shared.armed.store(true, Ordering::SeqCst);
            emit_state(shared, id);
        }
        Command::Disarm { id } => {
            let mut st = shared.settings.lock().unwrap();
            st.armed = false;
            let _ = st.save(&shared.paths.state);
            drop(st);
            shared.armed.store(false, Ordering::SeqCst);
            emit_state(shared, id);
        }
        Command::Consent {
            id,
            arm_now,
            arm_on_login,
        } => {
            let mut st = shared.settings.lock().unwrap();
            st.consent_at = now_ms();
            st.arm_on_login = arm_on_login;
            st.armed = arm_now;
            let _ = st.save(&shared.paths.state);
            drop(st);
            shared.armed.store(arm_now, Ordering::SeqCst);
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
            match shared.store.lock().unwrap().timeline(from, to, limit) {
                Ok(data) => emit(&Event::reply(id, data)),
                Err(e) => emit(&Event::error(Some(id), e.to_string())),
            }
        }
        Command::Moment { id, ts } => match shared.store.lock().unwrap().moment(ts) {
            Ok(data) => emit(&Event::reply(id, data)),
            Err(e) => emit(&Event::error(Some(id), e.to_string())),
        },
        Command::Clips { id, limit } => match shared.store.lock().unwrap().clips(limit) {
            Ok(data) => emit(&Event::reply(id, data)),
            Err(e) => emit(&Event::error(Some(id), e.to_string())),
        },
        Command::ReopenPlan { id, ts } => match build_plan(shared, ts) {
            Ok(data) => emit(&Event::reply(id, data)),
            Err(e) => emit(&Event::error(Some(id), e.to_string())),
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
        } => match shared.store.lock().unwrap().wipe(&scope, from, to) {
            Ok(data) => emit(&Event::reply(id, data)),
            Err(e) => emit(&Event::error(Some(id), e.to_string())),
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
    let _ = st.save(&shared.paths.state);
}

fn query_locked(
    shared: &Shared,
    q: &str,
    limit: usize,
    from: i64,
    to: i64,
) -> Result<serde_json::Value, String> {
    let store = shared.store.lock().unwrap();
    store
        .search(q, limit, from, to)
        .map_err(|e| e.to_string())
}

fn build_plan(shared: &Shared, ts: i64) -> Result<serde_json::Value, String> {
    let stored = shared
        .store
        .lock()
        .unwrap()
        .layout_at(ts)
        .map_err(|e| e.to_string())?;
    let live = hypr::clients().unwrap_or_default();
    let map = plan::desktop_map(plan::application_dirs());
    Ok(plan::build(&stored, &live, &map))
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
    let content = shared
        .store
        .lock()
        .unwrap()
        .clip_at(ts)
        .map_err(|e| e.to_string())?;
    if content.is_empty() {
        return Err("no clip at that moment".into());
    }
    clipboard::copy_text(&content)?;
    Ok(json!({"ok": true, "bytes": content.len()}))
}

fn stats_json(shared: &Shared) -> serde_json::Value {
    let store = shared.store.lock().unwrap();
    let snap = store.stats().unwrap_or_default();
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
        "encoder": shared.encoder.lock().unwrap().clone(),
        "ocrAvailable": ocr::tesseract_available(),
        "capture": capture::backend_name(),
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

fn evaluate_pause(shared: &Shared) -> Option<PauseReason> {
    if !shared.armed.load(Ordering::SeqCst) {
        return Some(PauseReason::Disarmed);
    }
    if shared.overlay_open.load(Ordering::SeqCst) {
        return Some(PauseReason::Overlay);
    }
    let settings = shared.settings.lock().unwrap().clone();
    let clients = hypr::clients().unwrap_or_default();
    let locked = hypr::locked();
    let idle_ms = hypr::idle_ms();
    let portal = hypr::portal_screencast_active();
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

        let reason = evaluate_pause(&shared);
        {
            let mut prev = shared.last_pause.lock().unwrap();
            if prev.as_ref() != reason.as_ref() {
                if let Ok(store) = shared.store.lock() {
                    let _ = store.record_event(
                        now_ms(),
                        if reason.is_some() { "pause" } else { "resume" },
                        reason
                            .as_ref()
                            .map(|r| r.as_str())
                            .unwrap_or(""),
                    );
                }
                *prev = reason.clone();
                emit(&Event::from_stats(stats_json(&shared)));
            }
        }
        if reason.is_some() {
            last_unchanged = false;
            continue;
        }

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
    let monitor = hypr::focused_monitor().unwrap_or_default();
    let active = hypr::active_window().unwrap_or_default();
    let clients = hypr::clients().unwrap_or_default();
    let raw = session.grab(&monitor.name)?;
    let hash = dhash::compute(&raw.rgba, raw.width, raw.height);
    let last = shared.last_dhash.load(Ordering::SeqCst);
    if last != 0 && last == hash && settings.skip_unchanged {
        shared.last_dhash.store(hash, Ordering::SeqCst);
        return Ok(true);
    }

    let scaled = encode::downscale_720p(&raw.rgba, raw.width, raw.height);
    let ts = now_ms();
    let ext = encode::preferred_ext();
    let rel = format!(
        "frames/{}/{}{}.{}",
        day_stamp(ts),
        ts,
        if hash == last { "-d" } else { "" },
        ext
    );
    let dest = shared.paths.root.join(&rel);
    if let Some(parent) = dest.parent() {
        perms::secure_dir(parent).map_err(|e| e.to_string())?;
    }
    let used = encode::encode_frame(&scaled.bytes, scaled.width, scaled.height, &dest)?;
    perms::secure_file(&dest).map_err(|e| e.to_string())?;
    *shared.encoder.lock().unwrap() = used.as_str().to_string();

    let mut crop_rel = None;
    if let Some(roi) = encode::crop_roi(
        &raw.rgba,
        raw.width,
        raw.height,
        &active,
        &monitor,
    ) {
        let crop_name = format!("crops/{ts}.png");
        let crop_path = shared.paths.root.join(&crop_name);
        if let Some(parent) = crop_path.parent() {
            perms::secure_dir(parent).map_err(|e| e.to_string())?;
        }
        encode::write_png(&roi.bytes, roi.width, roi.height, &crop_path)?;
        perms::secure_file(&crop_path).map_err(|e| e.to_string())?;
        crop_rel = Some(crop_name);
    }

    let bytes = std::fs::metadata(&dest).map(|m| m.len() as i64).unwrap_or(0);
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
        crop_x: active.at.0,
        crop_y: active.at.1,
        crop_w: active.size.0,
        crop_h: active.size.1,
        bytes,
        dhash: hash as i64,
        encoder: used.as_str().to_string(),
        crop_path: crop_rel,
    };

    {
        let mut store = shared.store.lock().unwrap();
        store.insert_frame(&insert).map_err(|e| e.to_string())?;
        store
            .insert_layout(ts, &clients)
            .map_err(|e| e.to_string())?;
        let clip = clipboard::latest_cached().unwrap_or_default();
        store
            .index_search_row(ts, "", &insert.app, &insert.title, &clip)
            .map_err(|e| e.to_string())?;
        store
            .prune_to(settings.byte_cap)
            .map_err(|e| e.to_string())?;
    }

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
    match DataPaths::prepare(data_dir)
        .and_then(|p| Store::open(&p.db))
        .and_then(|s| s.search(&q, 50, 0, 0))
    {
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
    match DataPaths::prepare(data_dir).and_then(|p| Store::open(&p.db)) {
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

    pub fn is_idle(&self) -> bool {
        matches!(
            evaluate_pause(self),
            Some(PauseReason::Idle) | Some(PauseReason::Locked) | Some(PauseReason::Disarmed)
        )
    }

    pub fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap()
    }
}

// clipboard/ocr modules need Shared
pub(crate) use Shared as DaemonState;
