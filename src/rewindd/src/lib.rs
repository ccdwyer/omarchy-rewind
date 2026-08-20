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

pub(crate) struct Shared {
    settings: Mutex<Settings>,
    store: Mutex<Option<Store>>,
    paths: DataPaths,
    armed: AtomicBool,
    overlay_open: AtomicBool,
    last_dhash: AtomicU64,
    last_write_ms: AtomicU64,
    encoder: Mutex<String>,
    last_pause: Mutex<Option<PauseReason>>,
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
    let store = if paths.db.exists() {
        DataPaths::prepare(data_dir)?;
        Some(Store::open(&paths.db)?)
    } else {
        None
    };
    Ok(Arc::new(Shared {
        settings: Mutex::new(settings),
        store: Mutex::new(store),
        paths,
        armed: AtomicBool::new(armed),
        overlay_open: AtomicBool::new(false),
        last_dhash: AtomicU64::new(0),
        last_write_ms: AtomicU64::new(0),
        encoder: Mutex::new(encode::preferred_name().to_string()),
        last_pause: Mutex::new(None),
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
            let mut st = shared.settings.lock().unwrap();
            if st.consent_at == 0 {
                drop(st);
                emit(&Event::error(
                    Some(id),
                    "consent required: arm rejected until the consent screen is recorded".into(),
                ));
                emit_state(shared, id);
                return;
            }
            st.armed = true;
            drop(st);
            if let Err(e) = ensure_store(shared) {
                emit(&Event::error(Some(id), e));
                emit_state(shared, id);
                return;
            }
            persist_settings(shared);
            shared.armed.store(true, Ordering::SeqCst);
            emit_state(shared, id);
        }
        Command::Disarm { id } => {
            let mut st = shared.settings.lock().unwrap();
            st.armed = false;
            drop(st);
            persist_settings(shared);
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
            drop(st);
            persist_settings(shared);
            if arm_now {
                if let Err(e) = ensure_store(shared) {
                    emit(&Event::error(Some(id), e));
                    emit_state(shared, id);
                    return;
                }
            }
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
        Command::ReopenExec { id, plan } => match execute_plan(plan) {
            Ok(data) => emit(&Event::reply(id, data)),
            Err(e) => emit(&Event::error(Some(id), e.to_string())),
        },
        Command::Wipe {
            id,
            scope,
            from,
            to,
        } => {
            let data = shared.with_store_mut(|s| s.wipe(&scope, from, to));
            match data {
                Some(Ok(data)) => emit(&Event::reply(id, data)),
                Some(Err(e)) => emit(&Event::error(Some(id), e.to_string())),
                None => emit(&Event::reply(id, json!({"wiped": 0, "scope": scope}))),
            }
        }
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
    let st = shared.settings.lock().unwrap();
    if st.consent_at == 0 && !st.armed {
        return;
    }
    let _ = st.save(&shared.paths.state);
}

fn ensure_store(shared: &Shared) -> Result<(), String> {
    let mut g = shared.store.lock().unwrap();
    if g.is_some() {
        return Ok(());
    }
    DataPaths::prepare(&shared.paths.root)?;
    *g = Some(Store::open(&shared.paths.db)?);
    Ok(())
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
                let _ = shared.with_store(|store| {
                    store.record_event(
                        now_ms(),
                        if reason.is_some() { "pause" } else { "resume" },
                        reason.as_ref().map(|r| r.as_str()).unwrap_or(""),
                    )
                });
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

    ensure_store(shared)?;

    let scaled = encode::downscale_720p(&raw.rgba, raw.width, raw.height);
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
    let (enc, written) = encode::encode_frame(&scaled.bytes, scaled.width, scaled.height, &dest)?;
    perms::secure_file(&written).map_err(|e| e.to_string())?;
    *shared.encoder.lock().unwrap() = enc.as_str().to_string();
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
        crop_rel = Some(crop_name);
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

    match shared.with_store_mut(|store| -> Result<(), String> {
        store.insert_frame(&insert)?;
        store.insert_layout(ts, &clients)?;
        let clip = clipboard::latest_cached().unwrap_or_default();
        store.index_search_row(ts, "", &insert.app, &insert.title, &clip)?;
        store.prune_to(settings.byte_cap)?;
        Ok(())
    }) {
        Some(Ok(())) => {}
        Some(Err(e)) => return Err(e),
        None => return Err("store not initialized".into()),
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

    pub fn with_store<R>(&self, f: impl FnOnce(&Store) -> R) -> Option<R> {
        let g = self.store.lock().unwrap();
        g.as_ref().map(f)
    }

    pub fn with_store_mut<R>(&self, f: impl FnOnce(&mut Store) -> R) -> Option<R> {
        let mut g = self.store.lock().unwrap();
        g.as_mut().map(f)
    }
}

// clipboard/ocr modules need Shared
pub(crate) use Shared as DaemonState;

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
        persist_settings(&shared);
        ensure_store(&shared).unwrap();
        assert!(shared.paths.state.exists());
        assert!(shared.paths.db.exists());
        assert!(shared.store.lock().unwrap().is_some());
        assert!(list_files(&data).iter().any(|p| p.ends_with("state.json")));
    }
}
