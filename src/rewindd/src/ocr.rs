use crate::encode::which;
use crate::ipc::Event;
use crate::query::{scale_roi_to_frame, WordBox};
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
        if shared.is_stopping() {
            return;
        }
        thread::sleep(Duration::from_secs(4));
        if shared.is_stopping() {
            return;
        }
        let _ = process_pending(&shared);
    }
}

/// One OCR pass. No-ops while disarmed — pending crops stay untouched.
pub(crate) fn process_pending(shared: &DaemonState) -> i64 {
    if !shared.is_armed() {
        return 0;
    }
    if !shared.is_idle() {
        return 0;
    }
    if !tesseract_available() {
        return 0;
    }
    let gen = match crate::arm_ticket(shared) {
        Some(g) => g,
        None => return 0,
    };
    // Sample the privacy epoch with the ticket; a pause transition during the
    // blocking tesseract run advances it and blocks the OCR commit below.
    let epoch0 = shared.privacy_epoch();
    let pending = match shared.with_store(|s| s.pending_crops()) {
        Some(Ok(p)) => p,
        _ => return 0,
    };
    let queued = pending.len() as i64;
    let mut done = 0i64;
    for (ts, rel) in pending {
        if !crate::still_recording(shared, gen, epoch0) || !shared.is_idle() {
            break;
        }
        let path = shared.data_paths().root.join(&rel);
        if !path.exists() {
            continue;
        }
        match run_tesseract(&path) {
            Ok((text, raw_boxes)) => {
                // Re-check AFTER the blocking tesseract run, before committing:
                // `is_idle()` re-evaluates live compositor state, so if the user
                // resumed activity mid-OCR we stop; `still_recording` rejects a
                // disarm or any privacy-pause transition observed since entry.
                // (Idle is itself the expected OCR condition, so we must not
                // treat it as a pause here — only *loss* of idle stops us.)
                if !crate::still_recording(shared, gen, epoch0) || !shared.is_idle() {
                    break;
                }
                let meta = frame_geom(shared, ts);
                let boxes: Vec<WordBox> = raw_boxes
                    .into_iter()
                    .map(|b| {
                        if let Some(g) = &meta {
                            scale_roi_to_frame(
                                (b.x, b.y, b.w, b.h),
                                g.crop_x,
                                g.crop_y,
                                g.crop_w,
                                g.crop_h,
                                g.out_w,
                                g.out_h,
                                g.stored_w,
                                g.stored_h,
                                &b.word,
                            )
                        } else {
                            b
                        }
                    })
                    .collect();
                let (app, title, clip) = shared
                    .with_store(|store| {
                        let (app, title) = store_app_title(store, ts);
                        let clip = store.clip_at(ts).unwrap_or_default();
                        (app, title, clip)
                    })
                    .unwrap_or_default();
                // Fresh live pause check right before committing. `refresh_pause`
                // advances the epoch on any transition observed since entry (so
                // the epoch check rejects an OCR result whose crop spanned a
                // pause) AND lets us reject a HARD pause that is active now —
                // most importantly a Locked screen, which `is_idle()` treats as
                // idle but which must never receive a write. A committed OCR row
                // (and the crop deletion that follows) is only allowed when the
                // session is genuinely idle or fully recording.
                crate::refresh_pause(shared);
                if !shared.ocr_may_write() {
                    // The frame's crop stays on disk for a later pass; leave
                    // state consistent (no partial write, no deletion).
                    break;
                }
                let committed = crate::with_arm_read(shared, |arm| {
                    if !crate::write_allowed(shared, arm, gen)
                        || shared.privacy_epoch() != epoch0
                        || !shared.ocr_may_write()
                    {
                        return false;
                    }
                    shared
                        .with_store_mut(|store| {
                            store.commit_ocr_tx(ts, &text, &app, &title, &clip, &boxes, || {
                                crate::write_allowed(shared, arm, gen)
                                    && shared.privacy_epoch() == epoch0
                                    && shared.ocr_may_write()
                            })
                        })
                        .unwrap_or(Ok(false))
                        .unwrap_or(false)
                });
                // The crop belongs to an already-committed, authorized frame.
                // Once its text is durably committed the full-resolution crop is
                // pure surplus sensitive data, so delete it UNCONDITIONALLY —
                // even if a hard privacy pause (lock/overlay/…) engaged in the
                // meantime. Deleting your own already-authorized crop reduces
                // data on disk; it is privacy cleanup, never an observation
                // write (same principle as immediate orphan-temp cleanup).
                // `clear_crop` unlinks the file first and only nulls the row's
                // crop_path/crop_bytes once the file is actually gone, so an
                // interrupted unlink leaves the crop tracked for a later prune.
                if committed {
                    let _ = std::fs::remove_file(&path);
                    let _ = shared.with_store_mut(|s| s.clear_crop(ts));
                    done += 1;
                    emit(&Event::ocr_progress(done, queued));
                }
            }
            Err(_) => {}
        }
    }
    done
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

fn frame_geom(shared: &DaemonState, ts: i64) -> Option<crate::store::FrameGeom> {
    shared.with_store(|s| s.frame_geom(ts)).flatten()
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
