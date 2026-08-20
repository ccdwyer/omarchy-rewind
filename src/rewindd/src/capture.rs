use crate::encode::which;
use image::RgbaImage;
use std::path::PathBuf;
use std::process::{Command, Stdio};
#[cfg(feature = "wayland")]
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct RawFrame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Preferred backend from compile-time/env probes. Prefer `CaptureSession::active_backend`
/// for the backend actually in use after grim fallback.
pub fn backend_name() -> &'static str {
    #[cfg(feature = "wayland")]
    {
        if std::env::var("WAYLAND_DISPLAY").is_ok() {
            return "wlr-screencopy";
        }
    }
    if which("grim") {
        "grim"
    } else {
        "missing"
    }
}

/// Cadence for the next tick. Unchanged dHash backs off to 10s as specified.
pub fn next_cadence_ms(base_ms: u64, last_unchanged: bool) -> u64 {
    if last_unchanged {
        10_000
    } else {
        base_ms.clamp(250, 60_000)
    }
}

/// Persistent capture source. Holds the wlr-screencopy session across ticks.
pub struct CaptureSession {
    #[cfg(feature = "wayland")]
    wlr: Option<capture_wlr::Session>,
    #[cfg(feature = "wayland")]
    wlr_retry_at: Instant,
    /// Frame counter for the env-gated synthetic test backend only.
    test_seq: u32,
    /// Test-only hook fired at the start of every `grab()`, letting a test
    /// interleave an arm-state change *during* the blocking grab window to
    /// exercise the disarm/re-arm race. Never compiled into release builds.
    #[cfg(test)]
    grab_hook: Option<Box<dyn FnMut() + Send>>,
}

impl Default for CaptureSession {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureSession {
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "wayland")]
            wlr: None,
            #[cfg(feature = "wayland")]
            wlr_retry_at: Instant::now(),
            test_seq: 0,
            #[cfg(test)]
            grab_hook: None,
        }
    }

    /// Install a hook invoked at the start of every `grab()`. Test-only.
    #[cfg(test)]
    pub fn set_grab_hook(&mut self, hook: Box<dyn FnMut() + Send>) {
        self.grab_hook = Some(hook);
    }

    pub fn using_grim(&self) -> bool {
        #[cfg(feature = "wayland")]
        {
            return self.wlr.is_none();
        }
        #[cfg(not(feature = "wayland"))]
        {
            true
        }
    }

    /// Backend that served (or will serve) the last/next grab, not the env probe.
    pub fn active_backend(&self) -> &'static str {
        #[cfg(feature = "wayland")]
        {
            if self.wlr.is_some() {
                return "wlr-screencopy";
            }
        }
        if which("grim") {
            "grim"
        } else {
            "missing"
        }
    }

    #[cfg(feature = "wayland")]
    fn bump_wlr_retry(&mut self) {
        self.wlr_retry_at = Instant::now() + Duration::from_secs(15);
    }

    pub fn grab(&mut self, output: &str) -> Result<RawFrame, String> {
        // Test-only: fire the interleave hook at the start of the grab window so
        // a test can disarm/re-arm *during* the grab and prove the ticket taken
        // before grab (not after) is what gates the commit.
        #[cfg(test)]
        if let Some(hook) = self.grab_hook.as_mut() {
            hook();
        }
        // Deterministic synthetic backend for the no-network audit / CI, where
        // no real compositor is present. Produces a fixed off-white frame with
        // a moving pixel so dedup does not skip it. Never used unless the env
        // var is explicitly set (tests and scripts/network-audit.sh only).
        if std::env::var("REWIND_TEST_CAPTURE").is_ok() {
            let (w, h) = (64u32, 64u32);
            let mut rgba = vec![0xF0u8; (w * h * 4) as usize];
            // Move one pixel each grab so successive frames differ and dedup
            // does not skip them.
            let idx = ((self.test_seq as usize) % (w * h) as usize) * 4;
            self.test_seq = self.test_seq.wrapping_add(1);
            rgba[idx] = 0x10;
            rgba[idx + 1] = 0x20;
            rgba[idx + 2] = 0x30;
            let _ = output;
            return Ok(RawFrame {
                rgba,
                width: w,
                height: h,
            });
        }
        // Never hand an empty output name to a real backend: grim would omit
        // `-o` and grab every display, and the wlr backend would fall back to
        // the first output. Refuse rather than capture the wrong monitor.
        if output.is_empty() {
            return Err("refusing capture: no focused output resolved".into());
        }
        #[cfg(feature = "wayland")]
        {
            if self.wlr.is_none() && Instant::now() >= self.wlr_retry_at {
                match capture_wlr::Session::connect() {
                    Ok(sess) => self.wlr = Some(sess),
                    Err(err) => {
                        eprintln!(
                            "rewindd: wlr-screencopy connect failed ({err}); grim fallback, will retry"
                        );
                        self.bump_wlr_retry();
                    }
                }
            }
            if let Some(sess) = self.wlr.as_mut() {
                match sess.grab(output) {
                    Ok(frame) => return Ok(frame),
                    Err(err) => {
                        eprintln!(
                            "rewindd: wlr-screencopy grab failed ({err}); grim this tick, will retry"
                        );
                        self.wlr = None;
                        self.bump_wlr_retry();
                    }
                }
            }
        }
        grab_grim(output)
    }
}

pub fn grab_focused(output: &str) -> Result<RawFrame, String> {
    CaptureSession::new().grab(output)
}

fn grab_grim(output: &str) -> Result<RawFrame, String> {
    if !which("grim") {
        return Err("neither wlr-screencopy nor grim is available".into());
    }
    // An empty output would make grim capture every display. Callers must pass
    // an exact focused output; refuse otherwise (defense in depth behind the
    // guard in `grab`).
    if output.is_empty() {
        return Err("refusing grim capture: empty output name".into());
    }
    let dest = secure_stage_path()?;
    let mut cmd = Command::new("grim");
    cmd.arg("-t").arg("png");
    cmd.arg("-o").arg(output);
    cmd.arg(&dest);
    let status = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        let _ = std::fs::remove_file(&dest);
        return Err("grim failed".into());
    }
    let frame = load_png(&dest)?;
    let _ = std::fs::remove_file(&dest);
    Ok(frame)
}

/// A fresh, securely-created full-resolution staging path for a grim capture.
/// Lives in a validated private per-UID runtime dir (0700, not a symlink) with
/// an unpredictable filename, created O_EXCL so nothing pre-existing can be
/// followed or clobbered. grim then overwrites this exact file we own.
fn secure_stage_path() -> Result<PathBuf, String> {
    let dir = crate::perms::secure_runtime_dir("stage").map_err(|e| e.to_string())?;
    for _ in 0..8 {
        let candidate = dir.join(crate::perms::random_name("png"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true) // O_EXCL: fail if the path already exists
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("could not create a unique secure staging file".into())
}

pub fn load_png(path: &PathBuf) -> Result<RawFrame, String> {
    let img = image::open(path).map_err(|e| e.to_string())?.to_rgba8();
    Ok(from_rgba(img))
}

pub fn from_rgba(img: RgbaImage) -> RawFrame {
    let width = img.width();
    let height = img.height();
    RawFrame {
        rgba: img.into_raw(),
        width,
        height,
    }
}

#[cfg(feature = "wayland")]
#[path = "capture_wlr.rs"]
mod capture_wlr;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_is_named() {
        let name = backend_name();
        assert!(name == "grim" || name == "missing" || name == "wlr-screencopy");
    }

    #[test]
    fn active_backend_reports_session_not_probe() {
        let session = CaptureSession::new();
        let name = session.active_backend();
        assert!(name == "grim" || name == "missing" || name == "wlr-screencopy");
        if session.using_grim() {
            assert_ne!(name, "wlr-screencopy");
        }
    }

    #[test]
    fn unchanged_cadence_backs_off_to_10s() {
        assert_eq!(next_cadence_ms(3000, true), 10_000);
        assert_eq!(next_cadence_ms(3000, false), 3000);
        assert_eq!(next_cadence_ms(5000, false), 5000);
    }

    #[test]
    fn grab_empty_output_captures_nothing() {
        // An unresolved focused output must never reach a real backend: grim
        // would grab every display and wlr would fall back to the first output.
        // `grab("")` must fail closed rather than capture the wrong monitor.
        // Serialize on the shared env lock and clear the synthetic-capture env
        // var so a parallel test cannot flip `grab` onto the test backend.
        let _env = crate::TEST_CAPTURE_ENV_LOCK.lock().unwrap();
        std::env::remove_var("REWIND_TEST_CAPTURE");
        let mut session = CaptureSession::new();
        assert!(
            session.grab("").is_err(),
            "empty output name must not produce a capture"
        );
    }

    #[test]
    fn grab_grim_empty_output_errors() {
        assert!(
            grab_grim("").is_err(),
            "grim must refuse an empty output (would capture all displays)"
        );
    }
}
