use crate::encode::which;
use crate::ipc::Event;
use crate::query::{scale_box, WordBox};
use crate::{emit, DaemonState};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub fn tesseract_available() -> bool {
    which("tesseract")
}

pub(crate) fn idle_loop(shared: Arc<DaemonState>) {
    if !tesseract_available() {
        return;
    }
    loop {
        thread::sleep(Duration::from_secs(4));
        if !shared.is_idle() {
            continue;
        }
        let pending = match shared.store().pending_crops() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let queued = pending.len() as i64;
        let mut done = 0i64;
        for (ts, rel) in pending {
            if !shared.is_idle() {
                break;
            }
            let path = shared.data_paths().root.join(&rel);
            if !path.exists() {
                let _ = shared.store().clear_crop(ts);
                continue;
            }
            match run_tesseract(&path) {
                Ok((text, raw_boxes)) => {
                    let meta = frame_geom(&shared, ts);
                    let boxes: Vec<WordBox> = raw_boxes
                        .into_iter()
                        .map(|mut b| {
                            if let Some(g) = &meta {
                                let scaled = scale_box(
                                    (b.x, b.y, b.w, b.h),
                                    (g.0, g.1),
                                    (g.2, g.3),
                                    (g.4, g.5),
                                );
                                b.x = scaled.x;
                                b.y = scaled.y;
                                b.w = scaled.w;
                                b.h = scaled.h;
                            }
                            b
                        })
                        .collect();
                    {
                        let store = shared.store();
                        let (app, title) = store_app_title(&store, ts);
                        let clip = store.clip_at(ts).unwrap_or_default();
                        let _ = store.index_search_row(ts, &text, &app, &title, &clip);
                        let _ = store.insert_ocr_boxes(ts, &boxes);
                        let _ = store.clear_crop(ts);
                    }
                    done += 1;
                    emit(&Event::ocr_progress(done, queued));
                }
                Err(_) => {
                    let _ = shared.store().clear_crop(ts);
                }
            }
        }
    }
}

fn store_app_title(store: &crate::store::Store, ts: i64) -> (String, String) {
    match store.moment(ts) {
        Ok(v) => {
            let frame = v.get("frame").cloned().unwrap_or(serde_json::Value::Null);
            (
                frame
                    .get("app")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                frame
                    .get("title")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            )
        }
        Err(_) => (String::new(), String::new()),
    }
}

fn frame_geom(shared: &DaemonState, ts: i64) -> Option<(f64, f64, f64, f64, f64, f64)> {
    let v = shared.store().moment(ts).ok()?;
    let f = v.get("frame")?;
    // crop origin is not in the moment JSON; skip scale if missing.
    let sw = f.get("width").and_then(|x| x.as_i64()).unwrap_or(0) as f64;
    let sh = f.get("height").and_then(|x| x.as_i64()).unwrap_or(0) as f64;
    if sw <= 0.0 || sh <= 0.0 {
        return None;
    }
    Some((0.0, 0.0, sw, sh, sw, sh))
}

pub fn run_tesseract(path: &Path) -> Result<(String, Vec<WordBox>), String> {
    let out = Command::new("nice")
        .args([
            "-n",
            "19",
            "tesseract",
            path.to_str().unwrap_or(""),
            "stdout",
            "tsv",
        ])
        .stdin(Stdio::null())
        .output()
        .or_else(|_| {
            Command::new("tesseract")
                .args([path.to_str().unwrap_or(""), "stdout", "tsv"])
                .stdin(Stdio::null())
                .output()
        })
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("tesseract failed".into());
    }
    Ok(parse_tsv(&String::from_utf8_lossy(&out.stdout)))
}

pub fn parse_tsv(tsv: &str) -> (String, Vec<WordBox>) {
    let mut words = Vec::new();
    let mut boxes = Vec::new();
    for (i, line) in tsv.lines().enumerate() {
        if i == 0 {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 12 {
            continue;
        }
        let level: i32 = cols[0].parse().unwrap_or(0);
        if level != 5 {
            continue;
        }
        let word = cols[11].trim();
        if word.is_empty() {
            continue;
        }
        let left: f64 = cols[6].parse().unwrap_or(0.0);
        let top: f64 = cols[7].parse().unwrap_or(0.0);
        let width: f64 = cols[8].parse().unwrap_or(0.0);
        let height: f64 = cols[9].parse().unwrap_or(0.0);
        words.push(word.to_string());
        boxes.push(WordBox {
            word: word.to_string(),
            x: left,
            y: top,
            w: width,
            h: height,
        });
    }
    (words.join(" "), boxes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tesseract_tsv_level5() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
5\t1\t1\t1\t1\t1\t10\t20\t30\t12\t96\thello\n\
5\t1\t1\t1\t1\t2\t44\t20\t40\t12\t95\tworld\n";
        let (text, boxes) = parse_tsv(tsv);
        assert_eq!(text, "hello world");
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[0].word, "hello");
        assert_eq!(boxes[0].x, 10.0);
    }
}
