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

pub fn grab_focused(output: &str) -> Result<RawFrame, String> {
    #[cfg(feature = "wayland")]
    {
        match capture_wlr::grab(output) {
            Ok(frame) => return Ok(frame),
            Err(err) => {
                eprintln!("rewindd: wlr-screencopy failed ({err}); falling back to grim");
            }
        }
    }
    grab_grim(output)
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
}
