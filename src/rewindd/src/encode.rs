use crate::hypr::{ActiveWindow, Monitor};
use crate::perms;
use image::{ImageBuffer, Rgba, RgbaImage};
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoder {
    Cwebp,
    ImageWebp,
    Png,
}

impl Encoder {
    pub fn as_str(self) -> &'static str {
        match self {
            Encoder::Cwebp => "cwebp",
            Encoder::ImageWebp => "image-webp",
            Encoder::Png => "png",
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Encoder::Cwebp | Encoder::ImageWebp => "webp",
            Encoder::Png => "png",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RgbaFrame {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub fn preferred_name() -> &'static str {
    if which("cwebp") {
        "cwebp"
    } else {
        "png"
    }
}

pub fn preferred_ext() -> &'static str {
    if which("cwebp") {
        "webp"
    } else {
        "png"
    }
}

pub fn which(bin: &str) -> bool {
    Command::new("sh")
        .args(["-c", "command -v \"$1\" >/dev/null", "sh", bin])
        .stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn downscale_720p(rgba: &[u8], width: u32, height: u32) -> RgbaFrame {
    if width == 0 || height == 0 {
        return RgbaFrame {
            bytes: rgba.to_vec(),
            width,
            height,
        };
    }
    let long = width.max(height);
    if long <= 1280 {
        return RgbaFrame {
            bytes: rgba.to_vec(),
            width,
            height,
        };
    }
    let scale = 1280.0 / long as f32;
    let nw = ((width as f32 * scale).round() as u32).max(1);
    let nh = ((height as f32 * scale).round() as u32).max(1);
    let img = buffer(rgba, width, height);
    let resized = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle);
    RgbaFrame {
        bytes: resized.into_raw(),
        width: nw,
        height: nh,
    }
}

pub fn crop_roi(
    rgba: &[u8],
    width: u32,
    height: u32,
    active: &ActiveWindow,
    monitor: &Monitor,
) -> Option<RgbaFrame> {
    if active.size.0 <= 0 || active.size.1 <= 0 {
        return None;
    }
    let x = (active.at.0 - monitor.x).max(0) as u32;
    let y = (active.at.1 - monitor.y).max(0) as u32;
    if x >= width || y >= height {
        return None;
    }
    let w = (active.size.0 as u32).min(width - x).max(1);
    let h = (active.size.1 as u32).min(height - y).max(1);
    let img = buffer(rgba, width, height);
    let cropped = image::imageops::crop_imm(&img, x, y, w, h).to_image();
    Some(RgbaFrame {
        width: cropped.width(),
        height: cropped.height(),
        bytes: cropped.into_raw(),
    })
}

pub fn encode_frame(
    rgba: &[u8],
    width: u32,
    height: u32,
    dest: &Path,
) -> Result<Encoder, String> {
    if which("cwebp") {
        if write_cwebp(rgba, width, height, dest).is_ok() {
            return Ok(Encoder::Cwebp);
        }
    }
    write_png(rgba, width, height, dest)?;
    Ok(Encoder::Png)
}

pub fn write_png(rgba: &[u8], width: u32, height: u32, dest: &Path) -> Result<(), String> {
    let img = buffer(rgba, width, height);
    img.save(dest).map_err(|e| e.to_string())?;
    perms::secure_file(dest).map_err(|e| e.to_string())?;
    Ok(())
}

fn write_cwebp(rgba: &[u8], width: u32, height: u32, dest: &Path) -> Result<(), String> {
    let tmp = dest.with_extension("src.png");
    write_png(rgba, width, height, &tmp)?;
    let status = Command::new("cwebp")
        .args([
            "-quiet",
            "-q",
            "75",
            "-m",
            "4",
            "-o",
            dest.to_str().unwrap_or("/dev/null"),
            tmp.to_str().unwrap_or(""),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&tmp);
    if status.success() {
        Ok(())
    } else {
        Err("cwebp failed".into())
    }
}

fn buffer(rgba: &[u8], width: u32, height: u32) -> RgbaImage {
    let expected = (width * height * 4) as usize;
    let mut data = rgba.to_vec();
    if data.len() < expected {
        data.resize(expected, 0);
    } else if data.len() > expected {
        data.truncate(expected);
    }
    ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(width, height, data).unwrap_or_else(|| {
        ImageBuffer::from_pixel(width.max(1), height.max(1), Rgba([0, 0, 0, 255]))
    })
}

pub fn self_test() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("rewind-encode-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut px = vec![0u8; 32 * 24 * 4];
    for (i, chunk) in px.chunks_mut(4).enumerate() {
        chunk[0] = (i % 255) as u8;
        chunk[1] = 40;
        chunk[2] = 80;
        chunk[3] = 255;
    }
    let scaled = downscale_720p(&px, 32, 24);
    let dest = dir.join("t.png");
    write_png(&scaled.bytes, scaled.width, scaled.height, &dest)?;
    if !dest.exists() {
        return Err("png missing".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_caps_long_edge() {
        let px = vec![20u8; 1920 * 1080 * 4];
        let s = downscale_720p(&px, 1920, 1080);
        assert!(s.width <= 1280);
        assert!(s.height <= 1280);
        assert_eq!(s.width, 1280);
        assert_eq!(s.height, 720);
    }

    #[test]
    fn small_frames_stay() {
        let px = vec![20u8; 800 * 600 * 4];
        let s = downscale_720p(&px, 800, 600);
        assert_eq!((s.width, s.height), (800, 600));
    }

    #[test]
    fn roi_crop_clamps() {
        let px = vec![9u8; 100 * 80 * 4];
        let active = ActiveWindow {
            at: (90, 70),
            size: (40, 40),
            ..ActiveWindow::default()
        };
        let mon = Monitor {
            x: 0,
            y: 0,
            ..Monitor::default()
        };
        let crop = crop_roi(&px, 100, 80, &active, &mon).unwrap();
        assert_eq!(crop.width, 10);
        assert_eq!(crop.height, 10);
    }
}
