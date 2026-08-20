use crate::hypr::Client;
use crate::now_ms;
use crate::perms;
use crate::query::{self, Hit, WordBox};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct FrameInsert {
    pub ts: i64,
    pub path: String,
    pub app: String,
    pub title: String,
    pub workspace: String,
    pub output: String,
    pub width: i64,
    pub height: i64,
    pub out_w: i64,
    pub out_h: i64,
    pub crop_x: i64,
    pub crop_y: i64,
    pub crop_w: i64,
    pub crop_h: i64,
    pub bytes: i64,
    pub dhash: i64,
    pub encoder: String,
    pub crop_path: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct StatsSnap {
    pub frames: i64,
    pub frames_today: i64,
    pub bytes: i64,
    pub first_ts: i64,
}

pub struct Store {
    conn: Connection,
    root: PathBuf,
}

impl Store {
    pub fn open(db: &Path) -> Result<Self, String> {
        if let Some(parent) = db.parent() {
            perms::secure_dir(parent).map_err(|e| e.to_string())?;
        }
        let conn = Connection::open(db).map_err(|e| e.to_string())?;
        perms::secure_file(db).map_err(|e| e.to_string())?;
        conn.busy_timeout(std::time::Duration::from_millis(2500))
            .map_err(|e| e.to_string())?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| e.to_string())?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| e.to_string())?;
        let store = Self {
            conn,
            root: db.parent().unwrap_or(Path::new(".")).to_path_buf(),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), String> {
        self.conn
            .execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS frames (
                    ts INTEGER PRIMARY KEY,
                    path TEXT NOT NULL,
                    app TEXT,
                    title TEXT,
                    workspace TEXT,
                    output TEXT,
                    width INTEGER,
                    height INTEGER,
                    out_w INTEGER,
                    out_h INTEGER,
                    crop_x INTEGER,
                    crop_y INTEGER,
                    crop_w INTEGER,
                    crop_h INTEGER,
                    bytes INTEGER,
                    dhash INTEGER,
                    encoder TEXT,
                    crop_path TEXT
                );
                CREATE TABLE IF NOT EXISTS clips (
                    ts INTEGER PRIMARY KEY,
                    mime TEXT,
                    content TEXT,
                    bytes INTEGER
                );
                CREATE TABLE IF NOT EXISTS layouts (
                    ts INTEGER PRIMARY KEY,
                    json TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS ocr_boxes (
                    ts INTEGER,
                    word TEXT,
                    x REAL,
                    y REAL,
                    w REAL,
                    h REAL
                );
                CREATE TABLE IF NOT EXISTS events (
                    ts INTEGER,
                    kind TEXT,
                    reason TEXT
                );
                CREATE TABLE IF NOT EXISTS meta (
                    key TEXT PRIMARY KEY,
                    value TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_frames_app ON frames(app);
                CREATE INDEX IF NOT EXISTS idx_ocr_boxes_ts ON ocr_boxes(ts);
                "#,
            )
            .map_err(|e| e.to_string())?;
        let _ = self.conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS ocr USING fts5(
                ts UNINDEXED, text, app, title, clip, tokenize='unicode61'
            );",
        );
        Ok(())
    }

    pub fn insert_frame(&mut self, f: &FrameInsert) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT OR REPLACE INTO frames
             (ts,path,app,title,workspace,output,width,height,out_w,out_h,
              crop_x,crop_y,crop_w,crop_h,bytes,dhash,encoder,crop_path)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            params![
                f.ts,
                f.path,
                f.app,
                f.title,
                f.workspace,
                f.output,
                f.width,
                f.height,
                f.out_w,
                f.out_h,
                f.crop_x,
                f.crop_y,
                f.crop_w,
                f.crop_h,
                f.bytes,
                f.dhash,
                f.encoder,
                f.crop_path
            ],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn prune_to(&mut self, cap: i64) -> Result<i64, String> {
        if cap <= 0 {
            return Ok(0);
        }
        let mut removed = 0i64;
        loop {
            let total: i64 = self
                .conn
                .query_row("SELECT COALESCE(SUM(bytes),0) FROM frames", [], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            if total <= cap {
                break;
            }
            let row: Option<(i64, String, Option<String>)> = self
                .conn
                .query_row(
                    "SELECT ts, path, crop_path FROM frames ORDER BY ts ASC LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            let Some((ts, path, crop)) = row else {
                break;
            };
            self.delete_ts(ts, &path, crop.as_deref())?;
            removed += 1;
        }
        Ok(removed)
    }

    fn delete_ts(&self, ts: i64, path: &str, crop: Option<&str>) -> Result<(), String> {
        let _ = std::fs::remove_file(self.root.join(path));
        if let Some(c) = crop {
            let _ = std::fs::remove_file(self.root.join(c));
        }
        self.conn
            .execute("DELETE FROM frames WHERE ts=?1", params![ts])
            .map_err(|e| e.to_string())?;
        self.conn
            .execute("DELETE FROM clips WHERE ts=?1", params![ts])
            .ok();
        self.conn
            .execute("DELETE FROM layouts WHERE ts=?1", params![ts])
            .ok();
        self.conn
            .execute("DELETE FROM ocr_boxes WHERE ts=?1", params![ts])
            .ok();
        let _ = self
            .conn
            .execute("DELETE FROM ocr WHERE ts=?1", params![ts]);
        Ok(())
    }

    pub fn insert_clip(&self, ts: i64, mime: &str, content: &str) -> Result<(), String> {
        let bytes = content.len() as i64;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO clips (ts,mime,content,bytes) VALUES (?1,?2,?3,?4)",
                params![ts, mime, content, bytes],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn insert_layout(&self, ts: i64, clients: &[Client]) -> Result<(), String> {
        let body = serde_json::to_string(clients).map_err(|e| e.to_string())?;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO layouts (ts,json) VALUES (?1,?2)",
                params![ts, body],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn record_event(&self, ts: i64, kind: &str, reason: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO events (ts,kind,reason) VALUES (?1,?2,?3)",
                params![ts, kind, reason],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn index_search_row(
        &self,
        ts: i64,
        text: &str,
        app: &str,
        title: &str,
        clip: &str,
    ) -> Result<(), String> {
        let _ = self
            .conn
            .execute("DELETE FROM ocr WHERE ts=?1", params![ts]);
        match self.conn.execute(
            "INSERT INTO ocr (ts,text,app,title,clip) VALUES (?1,?2,?3,?4,?5)",
            params![ts, text, app, title, clip],
        ) {
            Ok(_) => Ok(()),
            Err(_) => {
                self.ensure_search_fallback()?;
                self.conn
                    .execute(
                        "INSERT OR REPLACE INTO search_fallback (ts,text,app,title,clip)
                         VALUES (?1,?2,?3,?4,?5)",
                        params![ts, text, app, title, clip],
                    )
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
        }
    }

    fn ensure_search_fallback(&self) -> Result<(), String> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS search_fallback (
                    ts INTEGER PRIMARY KEY,
                    text TEXT, app TEXT, title TEXT, clip TEXT
                );",
            )
            .map_err(|e| e.to_string())
    }

    pub fn insert_ocr_boxes(&self, ts: i64, boxes: &[WordBox]) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM ocr_boxes WHERE ts=?1", params![ts])
            .map_err(|e| e.to_string())?;
        for b in boxes {
            self.conn
                .execute(
                    "INSERT INTO ocr_boxes (ts,word,x,y,w,h) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![ts, b.word, b.x, b.y, b.w, b.h],
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn pending_crops(&self) -> Result<Vec<(i64, String)>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT ts, crop_path FROM frames
                 WHERE crop_path IS NOT NULL AND crop_path != ''
                 ORDER BY ts ASC LIMIT 32",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn clear_crop(&self, ts: i64) -> Result<(), String> {
        if let Ok(path) = self.conn.query_row(
            "SELECT crop_path FROM frames WHERE ts=?1",
            params![ts],
            |r| r.get::<_, Option<String>>(0),
        ) {
            if let Some(p) = path {
                let _ = std::fs::remove_file(self.root.join(p));
            }
        }
        self.conn
            .execute(
                "UPDATE frames SET crop_path=NULL WHERE ts=?1",
                params![ts],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn search(&self, q: &str, limit: usize, from: i64, to: i64) -> Result<Value, String> {
        let limit = limit.clamp(1, 200);
        let q = q.trim();
        if q.is_empty() {
            return Ok(json!({"hits": [], "ocrAvailable": false, "query": ""}));
        }
        let mut hits = match self.search_fts(q, limit, from, to) {
            Ok(h) => h,
            Err(_) => self.search_like(q, limit, from, to)?,
        };
        for hit in &mut hits {
            hit.boxes = self.boxes_for(hit.ts, q)?;
        }
        Ok(json!({
            "hits": hits,
            "ocrAvailable": crate::ocr::tesseract_available(),
            "query": q
        }))
    }

    fn search_fts(&self, q: &str, limit: usize, from: i64, to: i64) -> Result<Vec<Hit>, String> {
        let fts = query::to_fts(q);
        let root = self.root.clone();
        let mut stmt = self
            .conn
            .prepare(
                "SELECT o.ts, f.path, f.app, f.title, snippet(ocr, 1, '[', ']', '…', 12)
                 FROM ocr o
                 JOIN frames f ON f.ts = o.ts
                 WHERE ocr MATCH ?1
                   AND (?2 = 0 OR o.ts >= ?2)
                   AND (?3 = 0 OR o.ts <= ?3)
                 ORDER BY o.ts DESC
                 LIMIT ?4",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![fts, from, to, limit as i64], |r| {
                Ok(Hit {
                    ts: r.get(0)?,
                    path: root.join(r.get::<_, String>(1)?).display().to_string(),
                    app: r.get(2)?,
                    title: r.get(3)?,
                    snippet: r.get(4)?,
                    boxes: vec![],
                })
            })
            .map_err(|e| e.to_string())?;
        let mut hits = Vec::new();
        for row in rows {
            hits.push(row.map_err(|e| e.to_string())?);
        }
        Ok(hits)
    }

    fn search_like(&self, q: &str, limit: usize, from: i64, to: i64) -> Result<Vec<Hit>, String> {
        let like = format!("%{}%", q.to_ascii_lowercase());
        let root = self.root.clone();
        let sql = "SELECT f.ts, f.path, f.app, f.title,
                          COALESCE(c.content,'')
                   FROM frames f
                   LEFT JOIN clips c ON c.ts = f.ts
                   WHERE (?2 = 0 OR f.ts >= ?2)
                     AND (?3 = 0 OR f.ts <= ?3)
                     AND (lower(f.title) LIKE ?1 OR lower(f.app) LIKE ?1
                          OR lower(COALESCE(c.content,'')) LIKE ?1)
                   ORDER BY f.ts DESC LIMIT ?4";
        let mut stmt = self.conn.prepare(sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![like, from, to, limit as i64], |r| {
                let title: String = r.get(3)?;
                let clip: String = r.get(4)?;
                Ok(Hit {
                    ts: r.get(0)?,
                    path: root.join(r.get::<_, String>(1)?).display().to_string(),
                    app: r.get(2)?,
                    title: title.clone(),
                    snippet: query::snippet_around(&format!("{title} {clip}"), q),
                    boxes: vec![],
                })
            })
            .map_err(|e| e.to_string())?;
        let mut hits = Vec::new();
        for row in rows {
            hits.push(row.map_err(|e| e.to_string())?);
        }
        Ok(hits)
    }

    fn boxes_for(&self, ts: i64, q: &str) -> Result<Vec<WordBox>, String> {
        let needle = q.to_ascii_lowercase();
        let mut stmt = self
            .conn
            .prepare("SELECT word,x,y,w,h FROM ocr_boxes WHERE ts=?1")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![ts], |r| {
                Ok(WordBox {
                    word: r.get(0)?,
                    x: r.get(1)?,
                    y: r.get(2)?,
                    w: r.get(3)?,
                    h: r.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            let b = row.map_err(|e| e.to_string())?;
            if b.word.to_ascii_lowercase().contains(&needle)
                || needle.contains(&b.word.to_ascii_lowercase())
            {
                out.push(b);
            }
        }
        Ok(out)
    }

    pub fn timeline(&self, from: i64, to: i64, limit: usize) -> Result<Value, String> {
        let limit = limit.clamp(1, 2000);
        let root = self.root.clone();
        let mut stmt = self
            .conn
            .prepare(
                "SELECT ts,path,app,title,workspace,bytes,encoder
                 FROM frames
                 WHERE (?1 = 0 OR ts >= ?1) AND (?2 = 0 OR ts <= ?2)
                 ORDER BY ts ASC
                 LIMIT ?3",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![from, to, limit as i64], |r| {
                Ok(json!({
                    "ts": r.get::<_, i64>(0)?,
                    "path": root.join(r.get::<_, String>(1)?).display().to_string(),
                    "app": r.get::<_, String>(2)?,
                    "title": r.get::<_, String>(3)?,
                    "workspace": r.get::<_, String>(4)?,
                    "bytes": r.get::<_, i64>(5)?,
                    "encoder": r.get::<_, String>(6)?
                }))
            })
            .map_err(|e| e.to_string())?;
        let mut frames = Vec::new();
        for row in rows {
            frames.push(row.map_err(|e| e.to_string())?);
        }
        let gaps = infer_gaps(&frames);
        Ok(json!({"frames": frames, "gaps": gaps}))
    }

    pub fn moment(&self, ts: i64) -> Result<Value, String> {
        let root = self.root.clone();
        let frame: Option<Value> = self
            .conn
            .query_row(
                "SELECT ts,path,app,title,workspace,bytes,encoder,width,height
                 FROM frames WHERE ts=?1",
                params![ts],
                |r| {
                    Ok(json!({
                        "ts": r.get::<_, i64>(0)?,
                        "path": root.join(r.get::<_, String>(1)?).display().to_string(),
                        "app": r.get::<_, String>(2)?,
                        "title": r.get::<_, String>(3)?,
                        "workspace": r.get::<_, String>(4)?,
                        "bytes": r.get::<_, i64>(5)?,
                        "encoder": r.get::<_, String>(6)?,
                        "width": r.get::<_, i64>(7)?,
                        "height": r.get::<_, i64>(8)?
                    }))
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let clip = self
            .clip_at(ts)
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_default();
        let layout = self.layout_at(ts).unwrap_or_default();
        let boxes: Vec<WordBox> = {
            let mut stmt = self
                .conn
                .prepare("SELECT word,x,y,w,h FROM ocr_boxes WHERE ts=?1")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![ts], |r| {
                    Ok(WordBox {
                        word: r.get(0)?,
                        x: r.get(1)?,
                        y: r.get(2)?,
                        w: r.get(3)?,
                        h: r.get(4)?,
                    })
                })
                .map_err(|e| e.to_string())?;
            rows.filter_map(|r| r.ok()).collect()
        };
        Ok(json!({
            "frame": frame,
            "clip": clip,
            "windows": layout,
            "boxes": boxes
        }))
    }

    pub fn clips(&self, limit: usize) -> Result<Value, String> {
        let limit = limit.clamp(1, 500);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT ts,mime,content,bytes FROM clips ORDER BY ts DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![limit as i64], |r| {
                Ok(json!({
                    "ts": r.get::<_, i64>(0)?,
                    "mime": r.get::<_, String>(1)?,
                    "content": r.get::<_, String>(2)?,
                    "bytes": r.get::<_, i64>(3)?
                }))
            })
            .map_err(|e| e.to_string())?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row.map_err(|e| e.to_string())?);
        }
        Ok(json!({"clips": items}))
    }

    pub fn clip_at(&self, ts: i64) -> Result<String, String> {
        let exact: Option<String> = self
            .conn
            .query_row(
                "SELECT content FROM clips WHERE ts=?1",
                params![ts],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(c) = exact {
            return Ok(c);
        }
        self.conn
            .query_row(
                "SELECT content FROM clips WHERE ts<=?1 ORDER BY ts DESC LIMIT 1",
                params![ts],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())
            .map(|o| o.unwrap_or_default())
    }

    pub fn layout_at(&self, ts: i64) -> Result<Vec<Client>, String> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT json FROM layouts WHERE ts<=?1 ORDER BY ts DESC LIMIT 1",
                params![ts],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        match raw {
            Some(s) => serde_json::from_str(&s).map_err(|e| e.to_string()),
            None => Ok(Vec::new()),
        }
    }

    pub fn wipe(&mut self, scope: &str, from: i64, to: i64) -> Result<Value, String> {
        let now = now_ms();
        let (lo, hi) = match scope {
            "all" => (0, i64::MAX),
            "today" => {
                let day = 86_400_000;
                let start = now - (now % day);
                (start, now)
            }
            "range" => (from, if to == 0 { now } else { to }),
            other => return Err(format!("unknown wipe scope: {other}")),
        };
        let mut stmt = self
            .conn
            .prepare("SELECT ts,path,crop_path FROM frames WHERE ts>=?1 AND ts<=?2")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![lo, hi], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let collected: Vec<_> = rows.filter_map(|r| r.ok()).collect();
        let n = collected.len();
        for (ts, path, crop) in collected {
            let _ = self.delete_ts(ts, &path, crop.as_deref());
        }
        if scope == "all" {
            self.conn
                .execute_batch("DELETE FROM clips; DELETE FROM layouts; DELETE FROM events; DELETE FROM ocr_boxes;")
                .map_err(|e| e.to_string())?;
            let _ = self.conn.execute_batch("DELETE FROM ocr;");
        }
        Ok(json!({"wiped": n, "scope": scope}))
    }

    pub fn stats(&self) -> Result<StatsSnap, String> {
        let frames: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM frames", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let bytes: i64 = self
            .conn
            .query_row("SELECT COALESCE(SUM(bytes),0) FROM frames", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let first_ts: i64 = self
            .conn
            .query_row("SELECT COALESCE(MIN(ts),0) FROM frames", [], |r| r.get(0))
            .unwrap_or(0);
        let now = now_ms();
        let day = 86_400_000;
        let start = now - (now % day);
        let frames_today: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM frames WHERE ts>=?1",
                params![start],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(StatsSnap {
            frames,
            frames_today,
            bytes,
            first_ts,
        })
    }

    pub fn self_test(&mut self) -> Result<(), String> {
        let f = FrameInsert {
            ts: 1_700_000_000_000,
            path: "frames/t.png".into(),
            app: "kitty".into(),
            title: "demo phrase unique-xyz".into(),
            workspace: "1".into(),
            output: "eDP-1".into(),
            width: 1280,
            height: 720,
            out_w: 1920,
            out_h: 1080,
            crop_x: 0,
            crop_y: 0,
            crop_w: 800,
            crop_h: 600,
            bytes: 40_000,
            dhash: 1,
            encoder: "png".into(),
            crop_path: None,
        };
        self.insert_frame(&f)?;
        self.insert_clip(f.ts, "text/plain", "abc123deadbeef")?;
        self.index_search_row(f.ts, "", &f.app, &f.title, "abc123deadbeef")?;
        let found = self.search("unique-xyz", 10, 0, 0)?;
        let hits = found.get("hits").and_then(|h| h.as_array()).cloned();
        if hits.map(|h| h.is_empty()).unwrap_or(true) {
            return Err("title search missed".into());
        }
        let found2 = self.search("abc123deadbeef", 10, 0, 0)?;
        if found2
            .get("hits")
            .and_then(|h| h.as_array())
            .map(|h| h.is_empty())
            .unwrap_or(true)
        {
            return Err("clipboard search missed".into());
        }
        Ok(())
    }
}

fn infer_gaps(frames: &[Value]) -> Vec<Value> {
    let mut gaps = Vec::new();
    for pair in frames.windows(2) {
        let a = pair[0].get("ts").and_then(|t| t.as_i64()).unwrap_or(0);
        let b = pair[1].get("ts").and_then(|t| t.as_i64()).unwrap_or(0);
        if b - a > 15_000 {
            gaps.push(json!({"from": a, "to": b, "reason": "gap"}));
        }
    }
    gaps
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample(ts: i64, bytes: i64, title: &str) -> FrameInsert {
        FrameInsert {
            ts,
            path: format!("frames/{ts}.png"),
            app: "kitty".into(),
            title: title.into(),
            workspace: "1".into(),
            output: "eDP-1".into(),
            width: 100,
            height: 80,
            out_w: 100,
            out_h: 80,
            crop_x: 0,
            crop_y: 0,
            crop_w: 100,
            crop_h: 80,
            bytes,
            dhash: ts,
            encoder: "png".into(),
            crop_path: None,
        }
    }

    #[test]
    fn prune_oldest_first_to_byte_cap() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("rewind.db")).unwrap();
        for i in 0..5 {
            let f = sample(1_000 + i, 100, "t");
            let p = dir.path().join(&f.path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, vec![0u8; 100]).unwrap();
            store.insert_frame(&f).unwrap();
        }
        let removed = store.prune_to(250).unwrap();
        assert!(removed >= 2);
        let snap = store.stats().unwrap();
        assert!(snap.bytes <= 250);
        assert!(snap.frames <= 3);
    }

    #[test]
    fn titles_and_clipboard_search_without_ocr() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("rewind.db")).unwrap();
        store.insert_frame(&sample(50, 10, "commit message zebra-token")).unwrap();
        store.insert_clip(50, "text/plain", "git-hash-ff00aa").unwrap();
        store
            .index_search_row(50, "", "kitty", "commit message zebra-token", "git-hash-ff00aa")
            .unwrap();
        let a = store.search("zebra-token", 10, 0, 0).unwrap();
        assert_eq!(a["hits"].as_array().unwrap().len(), 1);
        let b = store.search("git-hash-ff00aa", 10, 0, 0).unwrap();
        assert_eq!(b["hits"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn wipe_today_keeps_older() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("rewind.db")).unwrap();
        store.insert_frame(&sample(10, 10, "old")).unwrap();
        store.insert_frame(&sample(now_ms(), 10, "new")).unwrap();
        store.wipe("today", 0, 0).unwrap();
        let snap = store.stats().unwrap();
        assert_eq!(snap.frames, 1);
    }
}
