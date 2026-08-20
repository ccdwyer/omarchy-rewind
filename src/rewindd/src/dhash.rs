/// 64-bit perceptual hash: 8×8 average-hash mixed with mean luma so
/// solid dark and solid light do not collapse to the same value.
pub fn compute(rgba: &[u8], width: u32, height: u32) -> u64 {
    if width == 0 || height == 0 || rgba.len() < (width * height * 4) as usize {
        return 0;
    }
    let small = resize_gray(rgba, width, height, 8, 8);
    let sum: u32 = small.iter().map(|&p| p as u32).sum();
    let mean = sum / 64;
    let mut hash: u64 = 0;
    for (i, &px) in small.iter().enumerate() {
        if px as u32 > mean {
            hash |= 1 << i;
        }
    }
    (hash & 0x00FF_FFFF_FFFF_FFFF) | ((mean as u64) << 56)
}

pub fn distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

pub fn unchanged(a: u64, b: u64, threshold: u32) -> bool {
    a != 0 && b != 0 && distance(a, b) <= threshold
}

fn resize_gray(rgba: &[u8], width: u32, height: u32, tw: u32, th: u32) -> Vec<u8> {
    let mut out = vec![0u8; (tw * th) as usize];
    for y in 0..th {
        let sy = (y as f32 + 0.5) * height as f32 / th as f32;
        let iy = (sy as u32).min(height - 1);
        for x in 0..tw {
            let sx = (x as f32 + 0.5) * width as f32 / tw as f32;
            let ix = (sx as u32).min(width - 1);
            let i = ((iy * width + ix) * 4) as usize;
            let r = rgba[i] as u32;
            let g = rgba[i + 1] as u32;
            let b = rgba[i + 2] as u32;
            out[(y * tw + x) as usize] = ((r * 30 + g * 59 + b * 11) / 100) as u8;
        }
    }
    out
}

pub fn self_test() -> Result<(), String> {
    let mut dark = vec![0u8; 16 * 16 * 4];
    for px in dark.chunks_mut(4) {
        px[3] = 255;
    }
    let light = vec![255u8; 16 * 16 * 4];
    let a = compute(&dark, 16, 16);
    let b = compute(&light, 16, 16);
    if a == b {
        return Err("dhash collapsed light vs dark".into());
    }
    let c = compute(&dark, 16, 16);
    if a != c {
        return Err("dhash not deterministic".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            v.extend_from_slice(&[r, g, b, 255]);
        }
        v
    }

    fn gradient(w: u32, h: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let t = (x * 255 / (w - 1)) as u8;
                v.extend_from_slice(&[t, t, (255 - t).saturating_add(y as u8 / 4), 255]);
            }
        }
        v
    }

    #[test]
    fn identical_frames_match() {
        let a = gradient(64, 48);
        assert_eq!(compute(&a, 64, 48), compute(&a, 64, 48));
    }

    #[test]
    fn different_frames_diverge() {
        let a = gradient(64, 48);
        let b = solid(64, 48, 10, 10, 10);
        assert!(distance(compute(&a, 64, 48), compute(&b, 64, 48)) > 8);
    }

    #[test]
    fn near_duplicates_count_as_unchanged() {
        let a = gradient(32, 32);
        let mut b = a.clone();
        b[0] = b[0].saturating_add(3);
        assert!(unchanged(compute(&a, 32, 32), compute(&b, 32, 32), 4));
    }
}
