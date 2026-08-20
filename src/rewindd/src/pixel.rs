/// wl_shm fourcc-style formats we convert into RGBA8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShmFormat {
    Argb8888,
    Xrgb8888,
    Abgr8888,
    Xbgr8888,
    Unknown(u32),
}

impl ShmFormat {
    pub fn from_wayland_raw(raw: u32) -> Self {
        // little-endian DRM fourcc
        match raw {
            0x3432_5241 => ShmFormat::Argb8888, // AR24
            0x3432_5258 => ShmFormat::Xrgb8888, // XR24
            0x3432_4241 => ShmFormat::Abgr8888, // AB24
            0x3432_4258 => ShmFormat::Xbgr8888, // XB24
            0 => ShmFormat::Argb8888,
            other => ShmFormat::Unknown(other),
        }
    }
}

/// Convert one 4-byte pixel from the advertised shm layout into RGBA.
pub fn decode_pixel(fmt: ShmFormat, raw: [u8; 4]) -> [u8; 4] {
    match fmt {
        ShmFormat::Argb8888 => [raw[2], raw[1], raw[0], raw[3]],
        ShmFormat::Xrgb8888 => [raw[2], raw[1], raw[0], 255],
        ShmFormat::Abgr8888 => [raw[0], raw[1], raw[2], raw[3]],
        ShmFormat::Xbgr8888 => [raw[0], raw[1], raw[2], 255],
        ShmFormat::Unknown(_) => [raw[2], raw[1], raw[0], 255],
    }
}

pub fn decode_buffer(fmt: ShmFormat, src: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        let row = (y * stride) as usize;
        for x in 0..width {
            let i = row + (x as usize) * 4;
            if i + 3 >= src.len() {
                continue;
            }
            let px = decode_pixel(fmt, [src[i], src[i + 1], src[i + 2], src[i + 3]]);
            let o = ((y * width + x) * 4) as usize;
            rgba[o..o + 4].copy_from_slice(&px);
        }
    }
    rgba
}

/// Local calendar-day start (ms since epoch) for wipe/stats "today".
pub fn local_day_start_ms(now_ms: i64) -> i64 {
    if now_ms <= 0 {
        return 0;
    }
    let secs = now_ms / 1000;
    #[cfg(unix)]
    {
        let t = secs as libc::time_t;
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&t, &mut tm).is_null() {
                return now_ms - (now_ms % 86_400_000);
            }
            tm.tm_hour = 0;
            tm.tm_min = 0;
            tm.tm_sec = 0;
            let start = libc::mktime(&mut tm);
            if start < 0 {
                return now_ms - (now_ms % 86_400_000);
            }
            (start as i64) * 1000
        }
    }
    #[cfg(not(unix))]
    {
        now_ms - (now_ms % 86_400_000)
    }
}

pub fn local_day_bounds(now_ms: i64) -> (i64, i64) {
    let start = local_day_start_ms(now_ms);
    (start, now_ms.max(start))
}

/// Startup recording is consent + armOnLogin only. Persisted `armed` is ignored.
pub fn startup_recording(consent_at: i64, arm_on_login: bool, _persisted_armed: bool) -> bool {
    consent_at > 0 && arm_on_login
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xrgb_forces_opaque() {
        let px = decode_pixel(ShmFormat::Xrgb8888, [0x11, 0x22, 0x33, 0x00]);
        assert_eq!(px, [0x33, 0x22, 0x11, 255]);
    }

    #[test]
    fn argb_keeps_alpha() {
        let px = decode_pixel(ShmFormat::Argb8888, [0x11, 0x22, 0x33, 0x80]);
        assert_eq!(px, [0x33, 0x22, 0x11, 0x80]);
    }

    #[test]
    fn startup_ignores_persisted_armed() {
        assert!(!startup_recording(1, false, true));
        assert!(!startup_recording(0, true, true));
        assert!(startup_recording(1, true, false));
    }

    #[test]
    fn local_day_is_not_future() {
        let now = 1_700_000_000_000i64;
        let (lo, hi) = local_day_bounds(now);
        assert!(lo <= now);
        assert_eq!(hi, now);
        assert!(now - lo < 86_400_000);
    }
}
