use crate::encode::which;
use image::RgbaImage;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct RawFrame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

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
    grim_only: bool,
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
            grim_only: false,
        }
    }

    pub fn using_grim(&self) -> bool {
        if self.grim_only {
            return true;
        }
        #[cfg(feature = "wayland")]
        {
            return self.wlr.is_none();
        }
        #[cfg(not(feature = "wayland"))]
        {
            true
        }
    }

    pub fn grab(&mut self, output: &str) -> Result<RawFrame, String> {
        #[cfg(feature = "wayland")]
        {
            if !self.grim_only {
                if self.wlr.is_none() {
                    match capture_wlr::Session::connect() {
                        Ok(sess) => self.wlr = Some(sess),
                        Err(err) => {
                            eprintln!(
                                "rewindd: wlr-screencopy connect failed ({err}); grim fallback"
                            );
                            self.grim_only = true;
                        }
                    }
                }
                if let Some(sess) = self.wlr.as_mut() {
                    match sess.grab(output) {
                        Ok(frame) => return Ok(frame),
                        Err(err) => {
                            eprintln!(
                                "rewindd: wlr-screencopy grab failed ({err}); grim this tick"
                            );
                            self.wlr = None;
                        }
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
    fn unchanged_cadence_backs_off_to_10s() {
        assert_eq!(next_cadence_ms(3000, true), 10_000);
        assert_eq!(next_cadence_ms(3000, false), 3000);
        assert_eq!(next_cadence_ms(5000, false), 5000);
    }
}
