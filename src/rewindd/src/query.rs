use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct WordBox {
    pub word: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub ts: i64,
    pub path: String,
    pub app: String,
    pub title: String,
    pub snippet: String,
    pub boxes: Vec<WordBox>,
}

pub fn to_fts(q: &str) -> String {
    let mut parts = Vec::new();
    for token in q.split_whitespace() {
        let cleaned: String = token
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if cleaned.is_empty() {
            continue;
        }
        parts.push(format!("\"{cleaned}\""));
    }
    if parts.is_empty() {
        "\"\"".into()
    } else {
        parts.join(" AND ")
    }
}

pub fn snippet_around(hay: &str, needle: &str) -> String {
    let lower = hay.to_ascii_lowercase();
    let n = needle.to_ascii_lowercase();
    if let Some(idx) = lower.find(&n) {
        let start = idx.saturating_sub(24);
        let end = (idx + n.len() + 24).min(hay.len());
        let mut s = String::new();
        if start > 0 {
            s.push('…');
        }
        s.push_str(hay.get(start..end).unwrap_or(needle));
        if end < hay.len() {
            s.push('…');
        }
        return s;
    }
    hay.chars().take(64).collect()
}

/// Map a Tesseract box from crop-pixel space into stored-frame fractions 0..1.
///
/// Tesseract runs on the full-res focused-window ROI. The stored screenshot is
/// a uniformly scaled copy of the whole output, so boxes must be offset by the
/// crop origin and divided by the *output* size (not the ROI size).
pub fn scale_box(
    crop_box: (f64, f64, f64, f64),
    crop_origin: (f64, f64),
    out: (f64, f64),
    stored: (f64, f64),
) -> WordBox {
    scale_roi_to_frame(
        crop_box,
        crop_origin.0,
        crop_origin.1,
        0.0,
        0.0,
        out.0,
        out.1,
        stored.0,
        stored.1,
        "",
    )
}

pub fn scale_roi_to_frame(
    crop_box: (f64, f64, f64, f64),
    crop_x: f64,
    crop_y: f64,
    crop_w: f64,
    crop_h: f64,
    out_w: f64,
    out_h: f64,
    stored_w: f64,
    stored_h: f64,
    word: &str,
) -> WordBox {
    let (x, y, w, h) = crop_box;
    let ow = if out_w > 0.0 { out_w } else { stored_w.max(1.0) };
    let oh = if out_h > 0.0 { out_h } else { stored_h.max(1.0) };
    let sw = stored_w.max(1.0);
    let sh = stored_h.max(1.0);
    // Tesseract pixels are ROI-local in the crop_w×crop_h focused-window image.
    // Stored frame is a uniform scale of the full output (out_w×out_h).
    let _roi = (crop_w, crop_h);
    let x_out = crop_x + x;
    let y_out = crop_y + y;
    let sx = sw / ow;
    let sy = sh / oh;
    WordBox {
        word: word.to_string(),
        x: ((x_out * sx) / sw).clamp(0.0, 1.0),
        y: ((y_out * sy) / sh).clamp(0.0, 1.0),
        w: ((w * sx) / sw).clamp(0.0, 1.0),
        h: ((h * sy) / sh).clamp(0.0, 1.0),
    }
}

pub fn days_estimate(bytes: i64, cap: i64, first_ts: i64, now: i64) -> Option<f64> {
    if bytes < 8_192 || cap <= 0 || first_ts <= 0 {
        return None;
    }
    let elapsed = now - first_ts;
    if elapsed < 60_000 {
        return None;
    }
    let per_ms = bytes as f64 / elapsed as f64;
    if per_ms <= 0.0 {
        return None;
    }
    let per_day = per_ms * 86_400_000.0;
    Some(cap as f64 / per_day)
}

pub fn planning_days(cap: i64) -> (f64, f64) {
    // Planning band for the consent screen before any frames exist.
    // Uses the 10s dHash-skip cadence, not the 3s floor — most desktop
    // minutes are unchanged, and promising 3s×80KB would understate retention.
    let low = 25.0 * 1024.0;
    let high = 80.0 * 1024.0;
    let cadence = 10.0;
    let frames_per_day = 86_400.0 / cadence;
    let days_high = cap as f64 / (low * frames_per_day);
    let days_low = cap as f64 / (high * frames_per_day);
    (days_low, days_high)
}

pub fn self_test() -> Result<(), String> {
    let fts = to_fts("hello world!");
    if !fts.contains("hello") || !fts.contains("AND") {
        return Err(format!("bad fts: {fts}"));
    }
    let days = days_estimate(50_000, 2_000_000_000, 1, 1 + 86_400_000);
    if days.is_none() {
        return Err("days estimate none".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_quotes_tokens() {
        assert_eq!(to_fts("foo bar"), "\"foo\" AND \"bar\"");
    }

    #[test]
    fn bbox_scales_to_unit_square() {
        let b = scale_box((10.0, 20.0, 30.0, 10.0), (100.0, 50.0), (200.0, 100.0), (100.0, 50.0));
        assert!((b.x - 0.55).abs() < 0.02);
        assert!(b.w > 0.0 && b.w < 1.0);
    }

    #[test]
    fn roi_box_is_not_normalized_as_full_frame() {
        // Crop at (400, 108) on a 1920×1080 output, stored 1280×720.
        // Tess box at top-left of the ROI must land at 400/1920, not 0.
        let b = scale_roi_to_frame(
            (0.0, 0.0, 48.0, 16.0),
            400.0,
            108.0,
            800.0,
            600.0,
            1920.0,
            1080.0,
            1280.0,
            720.0,
            "hi",
        );
        assert!((b.x - 400.0 / 1920.0).abs() < 0.001);
        assert!((b.y - 108.0 / 1080.0).abs() < 0.001);
        assert!((b.w - 48.0 / 1920.0).abs() < 0.001);
        assert_ne!(b.x, 0.0);
        // Treating the ROI as the full frame would put this box at 0.
        assert!((b.x - 0.0).abs() > 0.1);
    }

    #[test]
    fn planning_range_is_honest() {
        let (lo, hi) = planning_days(2 * 1024 * 1024 * 1024);
        assert!(lo > 0.0 && hi > lo);
        // 25–80 KB at a 10s write average is a handful of days, not weeks.
        assert!(hi < 20.0);
        assert!(lo > 2.0);
    }
}
