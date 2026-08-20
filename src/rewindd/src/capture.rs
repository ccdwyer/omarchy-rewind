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
        }
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
    let dest = tmp_path();
    let mut cmd = Command::new("grim");
    cmd.arg("-t").arg("png");
    if !output.is_empty() {
        cmd.arg("-o").arg(output);
    }
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

fn tmp_path() -> PathBuf {
    let mut p = std::env::temp_dir().join("rewind-capture");
    let _ = std::fs::create_dir_all(&p);
    p.push(format!("{}.png", std::process::id()));
    p
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
}
