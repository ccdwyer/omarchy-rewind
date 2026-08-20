use crate::hypr::Client;
use crate::now_ms;
use crate::perms;
use crate::query::{self, Hit, WordBox};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
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
    /// On-disk size of the full-resolution OCR crop file (crop_path), counted
    /// in the managed byte budget alongside the thumbnail `bytes`. Zeroed when
    /// OCR finishes and deletes the crop.
    pub crop_bytes: i64,
}

#[derive(Debug, Clone, Default)]
pub struct StatsSnap {
    pub frames: i64,
    pub frames_today: i64,
    pub bytes: i64,
    pub first_ts: i64,
    pub last_ts: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct FrameGeom {
    pub crop_x: f64,
    pub crop_y: f64,
    pub crop_w: f64,
    pub crop_h: f64,
    pub out_w: f64,
    pub out_h: f64,
    pub stored_w: f64,
    pub stored_h: f64,
}

pub struct Store {
    conn: Connection,
    root: PathBuf,
    writable: bool,
}

impl Store {
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Read-only open: no WAL, no migrate, no chmod, no path creation.
    pub fn open_read_only(db: &Path) -> Result<Self, String> {
        if !db.exists() {
            return Err("no database".into());
        }
        let uri = format!(
            "file:{}?mode=ro&immutable=1",
            db.display().to_string().replace('\\', "/")
        );
        let conn = Connection::open_with_flags(
            &uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .or_else(|_| {
            Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        })
        .map_err(|e| e.to_string())?;
        let _ = conn.busy_timeout(std::time::Duration::from_millis(2500));
        let _ = conn.pragma_update(None, "query_only", true);
        Ok(Self {
            conn,
            root: db.parent().unwrap_or(Path::new(".")).to_path_buf(),
            writable: false,
        })
    }

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
            writable: true,
        };
        store.migrate()?;
        // Crash recovery: retry any deletion tombstones and orphan crops left
        // by a prior interrupted prune/wipe, so sensitive files can't survive a
        // crash-between-row-and-file across restarts. A writable open only
        // happens in an armed (authorized) context.
        let _ = store.flush_unlinks();
        let _ = store.sweep_orphan_crops();
        let _ = store.sweep_orphan_frames();
        // Checkpoint so the schema (especially `clips`) lives in the main
        // file, not only WAL. Read-only opens use immutable=1 and cannot see
        // WAL; without this, a helper restart while disarmed queries a main
        // file that still lacks tables and surfaces `no such table: clips`.
        let _ = store.checkpoint();
        Ok(store)
    }

    pub fn checkpoint(&self) -> Result<(), String> {
        if !self.writable {
            return Ok(());
        }
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(|e| e.to_string())
    }

    fn missing_table(err: &str) -> bool {
        err.to_ascii_lowercase().contains("no such table")
    }

    fn migrate(&self) -> Result<(), String> {
        // One transaction so a crash cannot leave `frames` without `clips`.
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| e.to_string())?;
        tx.execute_batch(
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
                -- Durable deletion tombstones. A file path is recorded here
                -- inside the SAME transaction that removes its row, BEFORE we
                -- attempt to unlink it. If the process crashes or the unlink
                -- fails, the tombstone survives so a startup/authorized sweep
                -- can still delete the orphaned screenshot/crop. A wipe that
                -- leaves residual tombstones reports failure rather than
                -- claiming success.
                CREATE TABLE IF NOT EXISTS pending_unlink (
                    path TEXT PRIMARY KEY
                );
                CREATE INDEX IF NOT EXISTS idx_frames_app ON frames(app);
                CREATE INDEX IF NOT EXISTS idx_ocr_boxes_ts ON ocr_boxes(ts);
                "#,
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        // Account full-resolution OCR crops in the managed byte budget. The crop
        // is a separate on-disk file (crop_path) whose size is NOT the frame
        // thumbnail's `bytes`; without this, crops could grow unbounded while
        // reported usage (SUM(bytes)) stayed under the cap. Added via ALTER so
        // existing databases migrate; the error when the column already exists is
        // intentionally ignored.
        let _ = self
            .conn
            .execute_batch("ALTER TABLE frames ADD COLUMN crop_bytes INTEGER DEFAULT 0;");
        let _ = self.conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS ocr USING fts5(
                ts UNINDEXED, text, app, title, clip, tokenize='unicode61'
            );",
        );
        Ok(())
    }

    pub fn insert_frame(&mut self, f: &FrameInsert) -> Result<(), String> {
        if !self.writable {
            return Err("read-only store".into());
        }
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT OR REPLACE INTO frames
             (ts,path,app,title,workspace,output,width,height,out_w,out_h,
              crop_x,crop_y,crop_w,crop_h,bytes,dhash,encoder,crop_path,crop_bytes)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
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
                f.crop_path,
                f.crop_bytes
            ],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Oldest-first byte-cap prune executed against an already-open
    /// transaction, so pruning commits atomically with the insert that pushed
    /// storage over the cap. Returns the on-disk frame/crop paths to unlink
    /// after the transaction commits (file removal cannot be transactional).
    /// Any SQL error propagates and rolls back the whole capture.
    /// Total bytes of ALL retained observation data counted against the cap:
    /// frame thumbnails + full-res OCR crops + clipboard content + window
    /// layouts + OCR/search text. Each sub-total is tolerant of a missing table
    /// (unwrap_or(0)) so it works before optional tables/FTS exist. Clipboard-,
    /// layout- and OCR-only growth are therefore bounded, not just frames.
    /// (WAL/page overhead is bounded separately by the truncating checkpoint.)
    fn managed_total(conn: &rusqlite::Connection) -> i64 {
        let q = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0) };
        q("SELECT COALESCE(SUM(bytes),0)+COALESCE(SUM(crop_bytes),0) FROM frames")
            + q("SELECT COALESCE(SUM(LENGTH(content)),0) FROM clips")
            + q("SELECT COALESCE(SUM(LENGTH(json)),0) FROM layouts")
            + q("SELECT COALESCE(SUM(LENGTH(text)),0) FROM ocr")
            // search_fallback / OCR text index: count all searchable fields, not
            // just `text` (app, title and captured clip text are retained too).
            + q("SELECT COALESCE(SUM(LENGTH(text)+LENGTH(COALESCE(app,''))+LENGTH(COALESCE(title,''))+LENGTH(COALESCE(clip,''))),0) FROM search_fallback")
            // Per-word OCR bounding boxes are retained observation data too.
            + q("SELECT COALESCE(SUM(LENGTH(word)),0) FROM ocr_boxes")
    }

    /// Oldest timestamp across every observation table (frames, clips, layouts,
    /// events), so pruning advances through clipboard-/layout-only periods that
    /// carry no frame — not just frame windows.
    fn oldest_ts(conn: &rusqlite::Connection, after: Option<i64>) -> Option<i64> {
        let union = "SELECT ts FROM frames{w}
             UNION ALL SELECT ts FROM clips{w}
             UNION ALL SELECT ts FROM layouts{w}
             UNION ALL SELECT ts FROM events{w}";
        match after {
            Some(a) => {
                let sql = format!("SELECT MIN(ts) FROM ({})", union.replace("{w}", " WHERE ts>?1"));
                conn.query_row(&sql, params![a], |r| r.get::<_, Option<i64>>(0))
                    .ok()
                    .flatten()
            }
            None => {
                let sql = format!("SELECT MIN(ts) FROM ({})", union.replace("{w}", ""));
                conn.query_row(&sql, [], |r| r.get::<_, Option<i64>>(0))
                    .ok()
                    .flatten()
            }
        }
    }

    fn prune_within_tx(
        tx: &rusqlite::Transaction<'_>,
        cap: i64,
    ) -> Result<Vec<(String, Option<String>)>, String> {
        let mut unlink: Vec<(String, Option<String>)> = Vec::new();
        if cap <= 0 {
            return Ok(unlink);
        }
        loop {
            let total = Self::managed_total(tx);
            if total <= cap {
                break;
            }
            let Some(ts) = Self::oldest_ts(tx, None) else { break };
            let next = Self::oldest_ts(tx, Some(ts));
            let hi = next.map(|n| n.saturating_sub(1)).unwrap_or(i64::MAX);
            // Collect the frame + crop files in this window before deleting rows.
            {
                let mut stmt = tx
                    .prepare("SELECT path, crop_path FROM frames WHERE ts>=?1 AND ts<=?2")
                    .map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map(params![ts, hi], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
                    })
                    .map_err(|e| e.to_string())?;
                for r in rows {
                    unlink.push(r.map_err(|e| e.to_string())?);
                }
            }
            // Delete every observation row in the oldest window across all
            // tables (clipboard-only periods included), counting total rows
            // removed to guarantee forward progress.
            let mut removed_here = 0usize;
            for (table, required) in [
                ("frames", true),
                ("clips", true),
                ("layouts", true),
                ("events", true),
                ("ocr_boxes", true),
                ("ocr", false),
                ("search_fallback", false),
            ] {
                let sql = format!("DELETE FROM {table} WHERE ts>=?1 AND ts<=?2");
                let res = tx.execute(&sql, params![ts, hi]);
                match res {
                    Ok(n) => removed_here += n,
                    Err(e) if Self::missing_table(&e.to_string()) => {
                        // Partial/legacy schema: skip this table rather than
                        // aborting the capture that just created a frame.
                    }
                    Err(e) if required => return Err(e.to_string()),
                    Err(_) => {}
                }
            }
            // Guard against a pathological no-progress loop.
            if removed_here == 0 {
                break;
            }
        }
        // Record a durable tombstone for every file to unlink, inside this same
        // transaction, so the intent-to-delete survives a crash between commit
        // and the actual unlink.
        Self::tombstone_files(tx, &unlink)?;
        Ok(unlink)
    }

    /// Record file paths to be unlinked into `pending_unlink` within `tx`.
    fn tombstone_files(
        tx: &rusqlite::Transaction<'_>,
        files: &[(String, Option<String>)],
    ) -> Result<(), String> {
        for (path, crop) in files {
            tx.execute(
                "INSERT OR IGNORE INTO pending_unlink (path) VALUES (?1)",
                params![path],
            )
            .map_err(|e| e.to_string())?;
            if let Some(c) = crop {
                tx.execute(
                    "INSERT OR IGNORE INTO pending_unlink (path) VALUES (?1)",
                    params![c],
                )
                .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    /// Attempt to unlink every tombstoned file; drop the tombstone only once the
    /// file is actually gone (a NotFound is treated as gone). Returns the number
    /// of tombstones that could NOT be cleared (residual files still on disk).
    pub fn flush_unlinks(&self) -> Result<usize, String> {
        let mut paths: Vec<String> = Vec::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT path FROM pending_unlink")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            for r in rows {
                paths.push(r.map_err(|e| e.to_string())?);
            }
        }
        let mut remaining = 0usize;
        for p in paths {
            let full = self.root.join(&p);
            match std::fs::remove_file(&full) {
                Ok(()) => {
                    let _ = self
                        .conn
                        .execute("DELETE FROM pending_unlink WHERE path=?1", params![p]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Already gone — clear the tombstone.
                    let _ = self
                        .conn
                        .execute("DELETE FROM pending_unlink WHERE path=?1", params![p]);
                }
                Err(_) => {
                    remaining += 1;
                }
            }
        }
        Ok(remaining)
    }

    /// Count of outstanding deletion tombstones (residual sensitive files).
    pub fn pending_unlink_count(&self) -> i64 {
        self.conn
            .query_row("SELECT COUNT(*) FROM pending_unlink", [], |r| r.get(0))
            .unwrap_or(0)
    }

    pub fn prune_to(&mut self, cap: i64) -> Result<i64, String> {
        if cap <= 0 {
            return Ok(0);
        }
        let mut removed = 0i64;
        loop {
            let total = Self::managed_total(&self.conn);
            if total <= cap {
                break;
            }
            let Some(ts) = Self::oldest_ts(&self.conn, None) else {
                break;
            };
            let next = Self::oldest_ts(&self.conn, Some(ts));
            let hi = next.map(|n| n.saturating_sub(1)).unwrap_or(i64::MAX);
            self.delete_range(0, hi)?;
            removed += 1;
        }
        Ok(removed)
    }

    fn delete_range(&mut self, lo: i64, hi: i64) -> Result<i64, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT ts, path, crop_path FROM frames WHERE ts>=?1 AND ts<=?2")
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
        let frames: Vec<_> = rows.filter_map(|r| r.ok()).collect();
        drop(stmt);
        let n = frames.len() as i64;
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM frames WHERE ts>=?1 AND ts<=?2", params![lo, hi])
            .map_err(|e| e.to_string())?;
        for sql in [
            "DELETE FROM clips WHERE ts>=?1 AND ts<=?2",
            "DELETE FROM layouts WHERE ts>=?1 AND ts<=?2",
            "DELETE FROM events WHERE ts>=?1 AND ts<=?2",
            "DELETE FROM ocr_boxes WHERE ts>=?1 AND ts<=?2",
        ] {
            if let Err(e) = tx.execute(sql, params![lo, hi]) {
                if !Self::missing_table(&e.to_string()) {
                    return Err(e.to_string());
                }
            }
        }
        let _ = tx.execute("DELETE FROM ocr WHERE ts>=?1 AND ts<=?2", params![lo, hi]);
        let _ = tx.execute(
            "DELETE FROM search_fallback WHERE ts>=?1 AND ts<=?2",
            params![lo, hi],
        );
        // Tombstone the files inside the same transaction so an interrupted
        // unlink cannot leave an untracked, wipe-surviving screenshot.
        let files: Vec<(String, Option<String>)> = frames
            .into_iter()
            .map(|(_ts, path, crop)| (path, crop))
            .collect();
        Self::tombstone_files(&tx, &files)?;
        tx.commit().map_err(|e| e.to_string())?;
        let _ = self.flush_unlinks();
        Ok(n)
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

    pub fn nearest_frame_ts(&self, ts: i64) -> Option<i64> {
        let before: Option<i64> = self
            .conn
            .query_row(
                "SELECT ts FROM frames WHERE ts <= ?1 ORDER BY ts DESC LIMIT 1",
                params![ts],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        if before.is_some() {
            return before;
        }
        self.conn
            .query_row(
                "SELECT ts FROM frames WHERE ts >= ?1 ORDER BY ts ASC LIMIT 1",
                params![ts],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    fn frame_meta(&self, ts: i64) -> Option<(String, String, String)> {
        self.conn
            .query_row(
                "SELECT path, app, title FROM frames WHERE ts=?1",
                params![ts],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .ok()
            .flatten()
    }

    fn read_search_row(&self, ts: i64) -> (String, String, String) {
        let from_fts: Option<(String, String, String)> = self
            .conn
            .query_row(
                "SELECT text, app, title FROM ocr WHERE ts=?1",
                params![ts],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .ok()
            .flatten();
        if let Some(row) = from_fts {
            return row;
        }
        let from_fb: Option<(String, String, String)> = self
            .conn
            .query_row(
                "SELECT text, app, title FROM search_fallback WHERE ts=?1",
                params![ts],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .ok()
            .flatten();
        if let Some(row) = from_fb {
            return row;
        }
        if let Some((_, app, title)) = self.frame_meta(ts) {
            return (String::new(), app, title);
        }
        (String::new(), String::new(), String::new())
    }

    /// Index clipboard text onto the nearest frame so OCR-free search hits a screenshot.
    pub fn record_clip_search(&self, clip_ts: i64, content: &str) -> Result<(), String> {
        let frame_ts = self.nearest_frame_ts(clip_ts).unwrap_or(clip_ts);
        let (text, app, title) = self.read_search_row(frame_ts);
        self.index_search_row(frame_ts, &text, &app, &title, content)
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

    /// One transaction for frame + layout + search. `still` is polled before each write
    /// and immediately before commit; false rolls back.
    pub fn commit_capture_tx<F>(
        &mut self,
        insert: &FrameInsert,
        clients: &[Client],
        clip: &str,
        byte_cap: i64,
        mut still: F,
    ) -> Result<bool, String>
    where
        F: FnMut() -> bool,
    {
        if !self.writable {
            return Err("read-only store".into());
        }
        if !still() {
            return Ok(false);
        }
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT OR REPLACE INTO frames
             (ts,path,app,title,workspace,output,width,height,out_w,out_h,
              crop_x,crop_y,crop_w,crop_h,bytes,dhash,encoder,crop_path,crop_bytes)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
            params![
                insert.ts,
                insert.path,
                insert.app,
                insert.title,
                insert.workspace,
                insert.output,
                insert.width,
                insert.height,
                insert.out_w,
                insert.out_h,
                insert.crop_x,
                insert.crop_y,
                insert.crop_w,
                insert.crop_h,
                insert.bytes,
                insert.dhash,
                insert.encoder,
                insert.crop_path,
                insert.crop_bytes
            ],
        )
        .map_err(|e| e.to_string())?;
        if !still() {
            return Ok(false);
        }
        let body = serde_json::to_string(clients).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT OR REPLACE INTO layouts (ts,json) VALUES (?1,?2)",
            params![insert.ts, body],
        )
        .map_err(|e| e.to_string())?;
        if !still() {
            return Ok(false);
        }
        let _ = tx.execute("DELETE FROM ocr WHERE ts=?1", params![insert.ts]);
        if tx
            .execute(
                "INSERT INTO ocr (ts,text,app,title,clip) VALUES (?1,?2,?3,?4,?5)",
                params![insert.ts, "", insert.app, insert.title, clip],
            )
            .is_err()
        {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS search_fallback (
                    ts INTEGER PRIMARY KEY, text TEXT, app TEXT, title TEXT, clip TEXT
                );",
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO search_fallback (ts,text,app,title,clip)
                 VALUES (?1,?2,?3,?4,?5)",
                params![insert.ts, "", insert.app, insert.title, clip],
            )
            .map_err(|e| e.to_string())?;
        }
        if !still() {
            return Ok(false);
        }
        // Enforce the byte cap oldest-first INSIDE this transaction, so a
        // capture never reports success while storage is over the limit. A
        // prune failure propagates and rolls back the whole insert.
        let to_unlink = Self::prune_within_tx(&tx, byte_cap)?;
        if !still() {
            return Ok(false);
        }
        tx.commit().map_err(|e| e.to_string())?;
        // Files are removed only after the DB rows are durably gone.
        // Rows are durably gone and their files tombstoned inside the tx;
        // unlink now, leaving any failures tombstoned for a later sweep.
        let _ = to_unlink;
        let _ = self.flush_unlinks();
        Ok(true)
    }

    pub fn commit_ocr_tx<F>(
        &mut self,
        ts: i64,
        text: &str,
        app: &str,
        title: &str,
        clip: &str,
        boxes: &[WordBox],
        mut still: F,
    ) -> Result<bool, String>
    where
        F: FnMut() -> bool,
    {
        if !self.writable {
            return Err("read-only store".into());
        }
        if !still() {
            return Ok(false);
        }
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        // INVARIANT: OCR only ever annotates a frame that was already captured
        // and committed while armed-and-unpaused. `pending_crops()` sources ts
        // values exclusively from committed `frames` rows, so OCR never touches
        // unauthorized/new content — this check makes that explicit and also
        // skips a frame the byte cap pruned between queueing and this commit.
        // A missing frame row means there is nothing authorized to annotate:
        // write nothing (the tx rolls back on drop).
        let frame_exists: i64 = tx
            .query_row("SELECT COUNT(1) FROM frames WHERE ts=?1", params![ts], |r| {
                r.get(0)
            })
            .unwrap_or(0);
        if frame_exists == 0 {
            return Ok(false);
        }
        let _ = tx.execute("DELETE FROM ocr WHERE ts=?1", params![ts]);
        if tx
            .execute(
                "INSERT INTO ocr (ts,text,app,title,clip) VALUES (?1,?2,?3,?4,?5)",
                params![ts, text, app, title, clip],
            )
            .is_err()
        {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS search_fallback (
                    ts INTEGER PRIMARY KEY, text TEXT, app TEXT, title TEXT, clip TEXT
                );",
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO search_fallback (ts,text,app,title,clip)
                 VALUES (?1,?2,?3,?4,?5)",
                params![ts, text, app, title, clip],
            )
            .map_err(|e| e.to_string())?;
        }
        if !still() {
            return Ok(false);
        }
        tx.execute("DELETE FROM ocr_boxes WHERE ts=?1", params![ts])
            .map_err(|e| e.to_string())?;
        for b in boxes {
            if !still() {
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO ocr_boxes (ts,word,x,y,w,h) VALUES (?1,?2,?3,?4,?5,?6)",
                params![ts, b.word, b.x, b.y, b.w, b.h],
            )
            .map_err(|e| e.to_string())?;
        }
        if !still() {
            return Ok(false);
        }
        // Do NOT null crop_path here. The crop file is still on disk at this
        // point; the caller deletes the file first and only then clears the
        // metadata (`clear_crop`). If we nulled the path inside this commit and
        // a privacy pause began before the file deletion ran, the row would no
        // longer reference the file — leaving an untracked full-resolution crop
        // that survives both pruning and wipe. Keeping crop_path set until the
        // file is actually gone means an interrupted deletion still leaves the
        // crop tracked, so a later prune/wipe reclaims it.
        tx.commit().map_err(|e| e.to_string())?;
        Ok(true)
    }

    pub fn commit_clip_tx<F>(
        &mut self,
        ts: i64,
        mime: &str,
        content: &str,
        byte_cap: i64,
        mut still: F,
    ) -> Result<bool, String>
    where
        F: FnMut() -> bool,
    {
        if !self.writable {
            return Err("read-only store".into());
        }
        if !still() {
            return Ok(false);
        }
        let bytes = content.len() as i64;
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT OR REPLACE INTO clips (ts,mime,content,bytes) VALUES (?1,?2,?3,?4)",
            params![ts, mime, content, bytes],
        )
        .map_err(|e| e.to_string())?;
        if !still() {
            return Ok(false);
        }
        let frame_ts = {
            let before: Option<i64> = tx
                .query_row(
                    "SELECT ts FROM frames WHERE ts <= ?1 ORDER BY ts DESC LIMIT 1",
                    params![ts],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            before.or_else(|| {
                tx.query_row(
                    "SELECT ts FROM frames WHERE ts >= ?1 ORDER BY ts ASC LIMIT 1",
                    params![ts],
                    |r| r.get(0),
                )
                .optional()
                .ok()
                .flatten()
            })
            .unwrap_or(ts)
        };
        if !still() {
            return Ok(false);
        }
        let (text, app, title) = {
            let from_fts: Option<(String, String, String)> = tx
                .query_row(
                    "SELECT text, app, title FROM ocr WHERE ts=?1",
                    params![frame_ts],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .ok()
                .flatten();
            from_fts.unwrap_or_else(|| (String::new(), String::new(), String::new()))
        };
        let _ = tx.execute("DELETE FROM ocr WHERE ts=?1", params![frame_ts]);
        if tx
            .execute(
                "INSERT INTO ocr (ts,text,app,title,clip) VALUES (?1,?2,?3,?4,?5)",
                params![frame_ts, text, app, title, content],
            )
            .is_err()
        {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS search_fallback (
                    ts INTEGER PRIMARY KEY, text TEXT, app TEXT, title TEXT, clip TEXT
                );",
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO search_fallback (ts,text,app,title,clip)
                 VALUES (?1,?2,?3,?4,?5)",
                params![frame_ts, text, app, title, content],
            )
            .map_err(|e| e.to_string())?;
        }
        if !still() {
            return Ok(false);
        }
        // Clipboard content counts against the cap too, so prune oldest-first
        // inside this same transaction — clipboard-heavy periods (even with no
        // new frames) can no longer grow storage without bound.
        let to_unlink = Self::prune_within_tx(&tx, byte_cap)?;
        if !still() {
            return Ok(false);
        }
        tx.commit().map_err(|e| e.to_string())?;
        // Rows are durably gone and their files tombstoned inside the tx;
        // unlink now, leaving any failures tombstoned for a later sweep.
        let _ = to_unlink;
        let _ = self.flush_unlinks();
        Ok(true)
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

    /// Delete the full-resolution OCR crop file for `ts`, THEN clear its
    /// metadata — but only null crop_path/crop_bytes once the file is actually
    /// gone. If the unlink fails, the row keeps referencing the file so a later
    /// prune/wipe still reclaims it (no untracked crop can survive).
    pub fn clear_crop(&self, ts: i64) -> Result<(), String> {
        let path: Option<String> = self
            .conn
            .query_row(
                "SELECT crop_path FROM frames WHERE ts=?1",
                params![ts],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .flatten();
        let gone = match path {
            Some(p) => {
                let full = self.root.join(&p);
                match std::fs::remove_file(&full) {
                    Ok(()) => true,
                    // Already absent counts as gone; any other error keeps the
                    // path referenced so the file stays tracked for reclamation.
                    Err(e) => e.kind() == std::io::ErrorKind::NotFound,
                }
            }
            None => true,
        };
        if gone {
            self.conn
                .execute(
                    "UPDATE frames SET crop_path=NULL, crop_bytes=0 WHERE ts=?1",
                    params![ts],
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Reclaim any full-resolution crop file on disk that no frames row still
    /// references (e.g. a crop orphaned by an earlier interrupted deletion, or
    /// left by a crash between the OCR commit and the file unlink). Run only in
    /// an authorized, unpaused window. Returns the number of files removed.
    pub fn sweep_orphan_crops(&self) -> Result<usize, String> {
        let crops_dir = self.root.join("crops");
        let entries = match std::fs::read_dir(&crops_dir) {
            Ok(e) => e,
            Err(_) => return Ok(0), // no crops dir yet: nothing to sweep
        };
        let mut removed = 0usize;
        for entry in entries.flatten() {
            let full = entry.path();
            if !full.is_file() {
                continue;
            }
            // Relative path as stored in crop_path (root-relative "crops/<name>").
            let rel = match full.strip_prefix(&self.root) {
                Ok(r) => r.to_string_lossy().to_string(),
                Err(_) => continue,
            };
            let referenced: i64 = self
                .conn
                .query_row(
                    "SELECT COUNT(1) FROM frames WHERE crop_path=?1",
                    params![rel],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if referenced == 0 {
                if std::fs::remove_file(&full).is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    /// Remove screenshot files under `frames/` with no referencing `frames` row
    /// — e.g. a frame written to its final path by an interrupted capture that
    /// crashed (power loss / SIGKILL) BEFORE its DB transaction committed. Such
    /// a file is untracked sensitive screen content that would otherwise survive
    /// pruning and `wipe all`. Frames live under `frames/<day>/<file>`, so we
    /// recurse one level. Returns (removed_ok, residual) where residual counts
    /// unreferenced files that could NOT be unlinked. Run only in an authorized,
    /// unpaused window (startup and every authorized wipe).
    pub fn sweep_orphan_frames(&self) -> Result<(usize, usize), String> {
        let frames_dir = self.root.join("frames");
        let day_dirs = match std::fs::read_dir(&frames_dir) {
            Ok(e) => e,
            Err(_) => return Ok((0, 0)), // no frames dir yet: nothing to sweep
        };
        let mut removed = 0usize;
        let mut residual = 0usize;
        let check = |full: &Path, removed: &mut usize, residual: &mut usize| {
            // Relative path as stored in frames.path ("frames/<day>/<file>" in
            // production, or "frames/<file>" in tests).
            let rel = match full.strip_prefix(&self.root) {
                Ok(r) => r.to_string_lossy().to_string(),
                Err(_) => return,
            };
            let referenced: i64 = self
                .conn
                .query_row(
                    "SELECT COUNT(1) FROM frames WHERE path=?1",
                    params![rel],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if referenced == 0 {
                if std::fs::remove_file(full).is_ok() {
                    *removed += 1;
                } else {
                    *residual += 1;
                }
            }
        };
        for entry in day_dirs.flatten() {
            let path = entry.path();
            if path.is_file() {
                // A frame stored directly under frames/ (test layout).
                check(&path, &mut removed, &mut residual);
            } else if path.is_dir() {
                // A day directory: frames/<day>/<file> (production layout).
                if let Ok(files) = std::fs::read_dir(&path) {
                    for f in files.flatten() {
                        let full = f.path();
                        if full.is_file() {
                            check(&full, &mut removed, &mut residual);
                        }
                    }
                }
            }
        }
        Ok((removed, residual))
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
        let clip_hits = self.search_clips(q, limit, from, to).unwrap_or_default();
        merge_hits(&mut hits, clip_hits, limit);
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
                "SELECT COALESCE(f.ts, nf.ts, o.ts),
                        COALESCE(f.path, nf.path, ''),
                        COALESCE(f.app, nf.app, o.app),
                        COALESCE(f.title, nf.title, o.title),
                        snippet(ocr, 1, '[', ']', '…', 12)
                 FROM ocr o
                 LEFT JOIN frames f ON f.ts = o.ts
                 LEFT JOIN frames nf ON nf.ts = (
                     SELECT ts FROM frames WHERE ts <= o.ts ORDER BY ts DESC LIMIT 1
                 )
                 WHERE ocr MATCH ?1
                   AND (?2 = 0 OR COALESCE(f.ts, nf.ts, o.ts) >= ?2)
                   AND (?3 = 0 OR COALESCE(f.ts, nf.ts, o.ts) <= ?3)
                 ORDER BY COALESCE(f.ts, nf.ts, o.ts) DESC
                 LIMIT ?4",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![fts, from, to, limit as i64], |r| {
                let rel: String = r.get(1)?;
                Ok(Hit {
                    ts: r.get(0)?,
                    path: if rel.is_empty() {
                        String::new()
                    } else {
                        root.join(rel).display().to_string()
                    },
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
        let sql_with_clips = "SELECT f.ts, f.path, f.app, f.title,
                          COALESCE((
                              SELECT c.content FROM clips c
                              WHERE c.ts <= f.ts
                              ORDER BY c.ts DESC LIMIT 1
                          ), '')
                   FROM frames f
                   WHERE (?2 = 0 OR f.ts >= ?2)
                     AND (?3 = 0 OR f.ts <= ?3)
                     AND (lower(f.title) LIKE ?1 OR lower(f.app) LIKE ?1
                          OR lower(COALESCE((
                              SELECT c.content FROM clips c
                              WHERE c.ts <= f.ts
                              ORDER BY c.ts DESC LIMIT 1
                          ), '')) LIKE ?1)
                   ORDER BY f.ts DESC LIMIT ?4";
        let sql_frames = "SELECT f.ts, f.path, f.app, f.title, ''
                   FROM frames f
                   WHERE (?2 = 0 OR f.ts >= ?2)
                     AND (?3 = 0 OR f.ts <= ?3)
                     AND (lower(f.title) LIKE ?1 OR lower(f.app) LIKE ?1)
                   ORDER BY f.ts DESC LIMIT ?4";
        let mut stmt = match self.conn.prepare(sql_with_clips) {
            Ok(s) => s,
            Err(e) if Self::missing_table(&e.to_string()) => self
                .conn
                .prepare(sql_frames)
                .map_err(|e| e.to_string())?,
            Err(e) => return Err(e.to_string()),
        };
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
        let mut stmt = match self
            .conn
            .prepare("SELECT word,x,y,w,h FROM ocr_boxes WHERE ts=?1")
        {
            Ok(s) => s,
            Err(e) if Self::missing_table(&e.to_string()) => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
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
                // Select the NEWEST `limit` frames in the window (DESC + LIMIT),
                // then return them oldest-first for chronological display. Using
                // ASC + LIMIT would drop recent captures once an archive exceeds
                // `limit` frames, hiding the most relevant (and demo) history.
                "SELECT ts,path,app,title,workspace,bytes,encoder FROM (
                     SELECT ts,path,app,title,workspace,bytes,encoder
                     FROM frames
                     WHERE (?1 = 0 OR ts >= ?1) AND (?2 = 0 OR ts <= ?2)
                     ORDER BY ts DESC
                     LIMIT ?3
                 ) ORDER BY ts ASC",
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
                "SELECT ts,path,app,title,workspace,bytes,encoder,width,height,
                        out_w,out_h,crop_x,crop_y,crop_w,crop_h
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
                        "height": r.get::<_, i64>(8)?,
                        "out_w": r.get::<_, i64>(9)?,
                        "out_h": r.get::<_, i64>(10)?,
                        "crop_x": r.get::<_, i64>(11)?,
                        "crop_y": r.get::<_, i64>(12)?,
                        "crop_w": r.get::<_, i64>(13)?,
                        "crop_h": r.get::<_, i64>(14)?
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
            match self.conn.prepare("SELECT word,x,y,w,h FROM ocr_boxes WHERE ts=?1") {
                Ok(mut stmt) => {
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
                }
                Err(e) if Self::missing_table(&e.to_string()) => Vec::new(),
                Err(e) => return Err(e.to_string()),
            }
        };
        Ok(json!({
            "frame": frame,
            "clip": clip,
            "windows": layout,
            "boxes": boxes
        }))
    }

    pub fn clips(&self, limit: usize) -> Result<Value, String> {
        match self.clips_inner(limit) {
            Ok(v) => Ok(v),
            Err(e) if Self::missing_table(&e) => {
                if self.writable {
                    self.migrate()?;
                    self.clips_inner(limit)
                } else {
                    Ok(json!({"clips": []}))
                }
            }
            Err(e) => Err(e),
        }
    }

    fn clips_inner(&self, limit: usize) -> Result<Value, String> {
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
        match self.clip_at_inner(ts) {
            Ok(s) => Ok(s),
            Err(e) if Self::missing_table(&e) => {
                if self.writable {
                    self.migrate()?;
                    self.clip_at_inner(ts)
                } else {
                    Ok(String::new())
                }
            }
            Err(e) => Err(e),
        }
    }

    fn clip_at_inner(&self, ts: i64) -> Result<String, String> {
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
        let raw: Option<String> = match self
            .conn
            .query_row(
                "SELECT json FROM layouts WHERE ts<=?1 ORDER BY ts DESC LIMIT 1",
                params![ts],
                |r| r.get(0),
            )
            .optional()
        {
            Ok(v) => v,
            Err(e) if Self::missing_table(&e.to_string()) => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        match raw {
            Some(s) => serde_json::from_str(&s).map_err(|e| e.to_string()),
            None => Ok(Vec::new()),
        }
    }

    pub fn wipe(&mut self, scope: &str, from: i64, to: i64) -> Result<Value, String> {
        let now = now_ms();
        let (lo, hi) = match scope {
            "all" => (0, i64::MAX),
            "today" => crate::pixel::local_day_bounds(now),
            "range" => (from, if to == 0 { now } else { to }),
            other => return Err(format!("unknown wipe scope: {other}")),
        };
        let n = self.delete_range(lo, hi)?;
        // Sweep any orphaned crops AND frames (files with no referencing row —
        // e.g. a screenshot written by a capture that crashed before its DB
        // commit) and retry outstanding tombstones. A wipe that leaves residual
        // sensitive files reports ok:false with the residual count rather than
        // lying that everything is gone.
        let _ = self.sweep_orphan_crops();
        let frame_residual = self.sweep_orphan_frames().map(|(_, r)| r).unwrap_or(0);
        let residual = self
            .flush_unlinks()
            .unwrap_or_else(|_| self.pending_unlink_count().max(0) as usize)
            + frame_residual;
        let ok = residual == 0;
        Ok(json!({
            "wiped": n,
            "scope": scope,
            "from": lo,
            "to": hi,
            "ok": ok,
            "residual": residual as i64
        }))
    }

    pub fn stats(&self) -> Result<StatsSnap, String> {
        let frames: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM frames", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        // Report the FULL managed total (frames+crops+clips+layouts+OCR text),
        // so the "usage vs cap" the UI shows reflects everything that counts
        // against the cap — not just frame bytes.
        let bytes: i64 = Self::managed_total(&self.conn);
        let first_ts: i64 = self
            .conn
            .query_row("SELECT COALESCE(MIN(ts),0) FROM frames", [], |r| r.get(0))
            .unwrap_or(0);
        let last_ts: i64 = self
            .conn
            .query_row("SELECT COALESCE(MAX(ts),0) FROM frames", [], |r| r.get(0))
            .unwrap_or(0);
        let now = now_ms();
        let start = crate::pixel::local_day_start_ms(now);
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
            last_ts,
        })
    }

    /// Crop origin/size in capture pixels plus output and stored-frame size.
    pub fn mutation_counters(&self) -> Result<(i64, i64, i64, i64), String> {
        let frames: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM frames", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let events: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let boxes: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM ocr_boxes", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let clips: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
            .unwrap_or(0);
        Ok((frames, events, boxes, clips))
    }

    pub fn pending_crop_count(&self) -> Result<i64, String> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM frames WHERE crop_path IS NOT NULL AND crop_path != ''",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())
    }

    pub fn frame_geom(&self, ts: i64) -> Option<FrameGeom> {
        self.conn
            .query_row(
                "SELECT crop_x, crop_y, crop_w, crop_h, out_w, out_h, width, height
                 FROM frames WHERE ts=?1",
                params![ts],
                |r| {
                    Ok(FrameGeom {
                        crop_x: r.get::<_, i64>(0)? as f64,
                        crop_y: r.get::<_, i64>(1)? as f64,
                        crop_w: r.get::<_, i64>(2)? as f64,
                        crop_h: r.get::<_, i64>(3)? as f64,
                        out_w: r.get::<_, i64>(4)? as f64,
                        out_h: r.get::<_, i64>(5)? as f64,
                        stored_w: r.get::<_, i64>(6)? as f64,
                        stored_h: r.get::<_, i64>(7)? as f64,
                    })
                },
            )
            .optional()
            .ok()
            .flatten()
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
            crop_bytes: 0,
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

    fn search_clips(
        &self,
        q: &str,
        limit: usize,
        from: i64,
        to: i64,
    ) -> Result<Vec<Hit>, String> {
        let like = format!("%{}%", q.to_ascii_lowercase());
        let mut stmt = self
            .conn
            .prepare(
                "SELECT ts, content FROM clips
                 WHERE lower(content) LIKE ?1
                   AND (?2 = 0 OR ts >= ?2)
                   AND (?3 = 0 OR ts <= ?3)
                 ORDER BY ts DESC LIMIT ?4",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![like, from, to, limit as i64], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?;
        let mut hits = Vec::new();
        for row in rows {
            let (clip_ts, content) = row.map_err(|e| e.to_string())?;
            let frame_ts = self.nearest_frame_ts(clip_ts).unwrap_or(clip_ts);
            let (path, app, title) = match self.frame_meta(frame_ts) {
                Some((p, a, t)) => (self.root.join(p).display().to_string(), a, t),
                None => (String::new(), String::new(), String::new()),
            };
            hits.push(Hit {
                ts: frame_ts,
                path,
                app,
                title,
                snippet: query::snippet_around(&content, q),
                boxes: vec![],
            });
        }
        Ok(hits)
    }
}

fn merge_hits(into: &mut Vec<Hit>, extra: Vec<Hit>, limit: usize) {
    for hit in extra {
        if into.iter().any(|h| h.ts == hit.ts && h.snippet == hit.snippet) {
            continue;
        }
        if into.iter().any(|h| h.ts == hit.ts) {
            continue;
        }
        into.push(hit);
    }
    into.sort_by(|a, b| b.ts.cmp(&a.ts));
    into.truncate(limit);
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
            crop_bytes: 0,
        }
    }

    #[test]
    fn timeline_returns_newest_frames_not_oldest() {
        // Blocker 1: once an archive exceeds the timeline limit, the strip must
        // show the NEWEST frames (so recent/seeded demo history is visible),
        // returned oldest-first for chronological scrubbing.
        let dir = tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("rewind.db")).unwrap();
        for ts in 1..=10 {
            store.insert_frame(&sample(ts, 10, "t")).unwrap();
        }
        let v = store.timeline(0, 0, 3).unwrap();
        let frames = v["frames"].as_array().unwrap();
        let got: Vec<i64> = frames.iter().map(|f| f["ts"].as_i64().unwrap()).collect();
        assert_eq!(got, vec![8, 9, 10], "newest 3, ascending");
    }

    #[test]
    fn search_hit_outside_timeline_window_still_resolves_a_frame() {
        // Blocker 2 (r18) — backend guarantee behind the overlay fix: a search
        // hit can be OLDER than the newest-N timeline window, so the strip has
        // no frame for it. moment(ts) must still return that exact frame (with
        // an absolute path) so the overlay can display the matched screenshot.
        let dir = tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("rewind.db")).unwrap();
        // An early frame + an early clip with a unique token, then many newer
        // frames that push the early one out of a small timeline window.
        store.insert_frame(&sample(1, 10, "t")).unwrap();
        store.insert_clip(1, "text/plain", "zzUNIQUEmarkerzz").unwrap();
        store.record_clip_search(1, "zzUNIQUEmarkerzz").unwrap();
        for ts in 2..=50 {
            store.insert_frame(&sample(ts, 10, "t")).unwrap();
        }
        // Small window: the early frame (ts=1) is NOT in the newest-5 strip.
        let tl = store.timeline(0, 0, 5).unwrap();
        let in_window: Vec<i64> = tl["frames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["ts"].as_i64().unwrap())
            .collect();
        assert!(!in_window.contains(&1), "early frame is out of the window");
        // Search finds the early hit...
        let hits = store.search("zzUNIQUEmarkerzz", 10, 0, 0).unwrap();
        let ts0 = hits["hits"][0]["ts"].as_i64().unwrap();
        assert_eq!(ts0, 1);
        // ...and moment(ts) resolves that exact out-of-window frame with a path.
        let m = store.moment(ts0).unwrap();
        assert_eq!(m["frame"]["ts"].as_i64().unwrap(), 1);
        assert!(m["frame"]["path"].as_str().unwrap().contains("frames/"));
    }

    #[test]
    fn delete_range_tombstones_then_unlinks_files() {
        // Blocker 2: deleting rows tombstones the files inside the tx, then
        // unlinks them; a clean run leaves no tombstone and no file.
        let dir = tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("rewind.db")).unwrap();
        std::fs::create_dir_all(dir.path().join("frames")).unwrap();
        std::fs::create_dir_all(dir.path().join("crops")).unwrap();
        let mut f = sample(5, 10, "t");
        f.crop_path = Some("crops/5.png".into());
        f.crop_bytes = 3;
        std::fs::write(dir.path().join("frames/5.png"), b"SCREEN").unwrap();
        std::fs::write(dir.path().join("crops/5.png"), b"CROP").unwrap();
        store.insert_frame(&f).unwrap();
        let n = store.wipe("all", 0, 0).unwrap();
        assert_eq!(n["wiped"].as_i64().unwrap(), 1);
        assert_eq!(n["ok"].as_bool().unwrap(), true);
        assert_eq!(n["residual"].as_i64().unwrap(), 0);
        assert!(!dir.path().join("frames/5.png").exists());
        assert!(!dir.path().join("crops/5.png").exists());
        assert_eq!(store.pending_unlink_count(), 0);
    }

    #[test]
    fn orphan_frame_from_crash_before_commit_is_swept() {
        // Blocker 3 (r18): a capture that wrote its screenshot to frames/ but
        // crashed BEFORE the DB commit leaves an untracked file with no row.
        // The sweep (run at startup and every authorized wipe) must remove it so
        // it cannot survive `wipe all` or evade the byte cap.
        let dir = tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("rewind.db")).unwrap();
        std::fs::create_dir_all(dir.path().join("frames/20260820")).unwrap();
        // One committed frame (referenced) + one orphan (no row), both layouts.
        let mut f = sample(100, 10, "kept");
        f.path = "frames/20260820/100.webp".into();
        std::fs::write(dir.path().join("frames/20260820/100.webp"), b"KEPT").unwrap();
        store.insert_frame(&f).unwrap();
        std::fs::write(dir.path().join("frames/20260820/999.webp"), b"ORPHAN").unwrap();
        // Also a flat-layout orphan to cover the test/legacy path.
        std::fs::write(dir.path().join("frames/flat-orphan.webp"), b"ORPHAN2").unwrap();

        let (removed, residual) = store.sweep_orphan_frames().unwrap();
        assert_eq!(removed, 2, "both orphan frame files removed");
        assert_eq!(residual, 0);
        assert!(dir.path().join("frames/20260820/100.webp").exists(), "referenced frame kept");
        assert!(!dir.path().join("frames/20260820/999.webp").exists());
        assert!(!dir.path().join("frames/flat-orphan.webp").exists());

        // And `wipe all` reports ok:true with zero residual (no orphan survives).
        std::fs::write(dir.path().join("frames/20260820/888.webp"), b"LATE-ORPHAN").unwrap();
        let w = store.wipe("all", 0, 0).unwrap();
        assert_eq!(w["ok"].as_bool().unwrap(), true);
        assert_eq!(w["residual"].as_i64().unwrap(), 0);
        assert!(!dir.path().join("frames/20260820/888.webp").exists());
    }

    #[test]
    fn wipe_reports_residual_when_file_cannot_be_unlinked() {
        // Blocker 2: a tombstone whose file cannot be removed (here it is a
        // non-empty directory) keeps the tombstone and makes flush_unlinks
        // report residual > 0 — a wipe must not falsely claim success while
        // sensitive files remain.
        let dir = tempdir().unwrap();
        let store = Store::open(&dir.path().join("rewind.db")).unwrap();
        // A directory cannot be removed by remove_file → unlink fails, tombstone
        // stays. Stand-in for an un-unlinkable residual file.
        std::fs::create_dir_all(dir.path().join("stuck")).unwrap();
        std::fs::write(dir.path().join("stuck/inner"), b"x").unwrap();
        store
            .conn
            .execute(
                "INSERT INTO pending_unlink (path) VALUES ('stuck')",
                [],
            )
            .unwrap();
        let remaining = store.flush_unlinks().unwrap();
        assert_eq!(remaining, 1, "un-unlinkable file remains tombstoned");
        assert_eq!(store.pending_unlink_count(), 1);
    }

    #[test]
    fn crop_bytes_count_in_managed_budget_and_prune() {
        let dir = tempdir().unwrap();
        let mut s = Store::open(&dir.path().join("db.sqlite")).unwrap();
        // One frame: tiny thumbnail, a large OCR crop that hasn't been consumed.
        let mut f = sample(1000, 10, "a");
        f.crop_path = Some("crops/1000.png".into());
        f.crop_bytes = 5_000;
        s.insert_frame(&f).unwrap();
        // The managed budget counts the crop, not just the thumbnail.
        assert_eq!(s.stats().unwrap().bytes, 5_010);
        // A cap between thumbnail-only and thumbnail+crop must trigger pruning,
        // which it would not if crops were excluded from accounting.
        assert!(s.prune_to(100).unwrap() >= 1, "crop bytes must drive pruning");
        assert_eq!(s.stats().unwrap().bytes, 0);
        // After OCR consumes+deletes a crop, its bytes leave the budget.
        let mut g = sample(2000, 20, "b");
        g.crop_path = Some("crops/2000.png".into());
        g.crop_bytes = 9_000;
        s.insert_frame(&g).unwrap();
        assert_eq!(s.stats().unwrap().bytes, 9_020);
        s.clear_crop(2000).unwrap();
        assert_eq!(
            s.stats().unwrap().bytes,
            20,
            "clearing a consumed crop drops its bytes from the budget"
        );
    }

    #[test]
    fn frame_geom_loads_crop_and_output() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("rewind.db")).unwrap();
        let mut f = sample(42, 10, "geom");
        f.crop_x = 400;
        f.crop_y = 108;
        f.crop_w = 800;
        f.crop_h = 600;
        f.out_w = 1920;
        f.out_h = 1080;
        f.width = 1280;
        f.height = 720;
        store.insert_frame(&f).unwrap();
        let g = store.frame_geom(42).unwrap();
        assert_eq!(g.crop_x, 400.0);
        assert_eq!(g.crop_w, 800.0);
        assert_eq!(g.out_w, 1920.0);
        assert_eq!(g.stored_w, 1280.0);
    }

    #[test]
    fn capture_tx_rolls_back_when_still_fails_mid_commit() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("rewind.db")).unwrap();
        let f = sample(7, 10, "race");
        let mut n = 0;
        let ok = store
            .commit_capture_tx(&f, &[], "", 1_000_000, || {
                n += 1;
                n < 2
            })
            .unwrap();
        assert!(!ok);
        assert_eq!(store.mutation_counters().unwrap().0, 0);
    }

    #[test]
    fn ocr_tx_rolls_back_when_still_fails_mid_commit() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("rewind.db")).unwrap();
        store.insert_frame(&sample(7, 10, "ocr")).unwrap();
        let boxes = [crate::query::WordBox {
            word: "hi".into(),
            x: 0.1,
            y: 0.1,
            w: 0.2,
            h: 0.1,
        }];
        let mut n = 0;
        let ok = store
            .commit_ocr_tx(7, "hi", "kitty", "t", "", &boxes, || {
                n += 1;
                n < 2
            })
            .unwrap();
        assert!(!ok);
        assert_eq!(store.mutation_counters().unwrap().2, 0);
        assert_eq!(store.pending_crop_count().unwrap(), 0);
    }

    #[test]
    fn clip_tx_rolls_back_when_still_fails_mid_commit() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("rewind.db")).unwrap();
        store.insert_frame(&sample(7, 10, "clip")).unwrap();
        let mut n = 0;
        let ok = store
            .commit_clip_tx(7, "text/plain", "secret", 0, || {
                n += 1;
                n < 2
            })
            .unwrap();
        assert!(!ok);
        assert_eq!(store.mutation_counters().unwrap().3, 0);
    }

    #[test]
    fn read_only_open_does_not_create_sidecar() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("rewind.db");
        {
            let mut store = Store::open(&db).unwrap();
            store.insert_frame(&sample(1, 10, "a")).unwrap();
            store.checkpoint().unwrap();
        }
        let before: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        let ro = Store::open_read_only(&db).unwrap();
        assert!(!ro.is_writable());
        assert_eq!(ro.stats().unwrap().frames, 1);
        drop(ro);
        let after: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(before.len(), after.len());
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
    fn clipboard_event_search_joins_nearest_frame_without_ocr() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("rewind.db")).unwrap();
        store.insert_frame(&sample(100, 10, "editor")).unwrap();
        store
            .index_search_row(100, "", "kitty", "editor", "")
            .unwrap();
        store
            .insert_clip(150, "text/plain", "orphan-clip-token-zz")
            .unwrap();
        store
            .record_clip_search(150, "orphan-clip-token-zz")
            .unwrap();
        let hits = store.search("orphan-clip-token-zz", 10, 0, 0).unwrap();
        let arr = hits["hits"].as_array().expect("hits array");
        assert_eq!(arr.len(), 1, "{hits}");
        assert_eq!(arr[0]["ts"], 100);
        assert!(
            arr[0]["path"]
                .as_str()
                .unwrap_or("")
                .contains("100"),
            "path should be the nearest frame: {}",
            arr[0]["path"]
        );
    }

    #[test]
    fn wipe_range_deletes_clips_layouts_by_range() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("rewind.db")).unwrap();
        store.insert_frame(&sample(100, 10, "a")).unwrap();
        store.insert_clip(150, "text/plain", "secret-clip").unwrap();
        store
            .insert_layout(150, &[])
            .unwrap();
        store.wipe("range", 100, 200).unwrap();
        let clips = store.clips(10).unwrap();
        assert_eq!(clips["clips"].as_array().unwrap().len(), 0);
        assert_eq!(store.stats().unwrap().frames, 0);
    }

    #[test]
    fn prune_removes_clips_in_oldest_window() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("rewind.db")).unwrap();
        for ts in [100, 200, 300, 400, 500] {
            let f = sample(ts, 100, "t");
            let p = dir.path().join(&f.path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, vec![0u8; 100]).unwrap();
            store.insert_frame(&f).unwrap();
        }
        store.insert_clip(150, "text/plain", "old-secret").unwrap();
        store.prune_to(250).unwrap();
        let clips = store.clips(10).unwrap();
        let arr = clips["clips"].as_array().unwrap();
        assert!(
            arr.iter().all(|c| c["content"] != "old-secret"),
            "{clips}"
        );
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

    #[test]
    fn ocr_commit_keeps_crop_tracked_until_file_deleted() {
        // Blocker 1: commit_ocr_tx must NOT null crop_path. If a pause lands
        // between the OCR commit and the crop-file deletion, the crop must stay
        // referenced so a later prune/wipe still reclaims it — never an
        // untracked sensitive file surviving on disk.
        let dir = tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("rewind.db")).unwrap();
        std::fs::create_dir_all(dir.path().join("crops")).unwrap();
        let crop_rel = "crops/9.png";
        std::fs::write(dir.path().join(crop_rel), b"SENSITIVE").unwrap();
        let mut f = sample(9, 10, "x");
        f.crop_path = Some(crop_rel.into());
        f.crop_bytes = 9;
        store.insert_frame(&f).unwrap();
        let ok = store
            .commit_ocr_tx(9, "hello", "kitty", "x", "", &[], || true)
            .unwrap();
        assert!(ok);
        let still: Option<String> = store
            .conn
            .query_row("SELECT crop_path FROM frames WHERE ts=9", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            still.as_deref(),
            Some(crop_rel),
            "OCR commit leaves the crop tracked (deleted separately)"
        );
        assert!(dir.path().join(crop_rel).exists());
        // The authorized cleanup deletes the file, THEN clears metadata.
        store.clear_crop(9).unwrap();
        assert!(!dir.path().join(crop_rel).exists(), "crop file deleted");
        let after: Option<String> = store
            .conn
            .query_row("SELECT crop_path FROM frames WHERE ts=9", [], |r| r.get(0))
            .unwrap();
        assert!(after.is_none(), "crop_path cleared only after the file is gone");
    }

    #[test]
    fn sweep_orphan_crops_removes_unreferenced_files() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("rewind.db")).unwrap();
        std::fs::create_dir_all(dir.path().join("crops")).unwrap();
        std::fs::write(dir.path().join("crops/ref.png"), b"r").unwrap();
        std::fs::write(dir.path().join("crops/orphan.png"), b"o").unwrap();
        let mut f = sample(1, 1, "ref");
        f.crop_path = Some("crops/ref.png".into());
        f.crop_bytes = 1;
        store.insert_frame(&f).unwrap();
        let removed = store.sweep_orphan_crops().unwrap();
        assert_eq!(removed, 1, "only the unreferenced crop is swept");
        assert!(dir.path().join("crops/ref.png").exists());
        assert!(!dir.path().join("crops/orphan.png").exists());
    }

    #[test]
    fn clipboard_growth_prunes_under_cap() {
        // Blocker 5: clipboard-only growth (no frames) must trigger pruning and
        // stay under the cap — clip content is counted in the managed total.
        let dir = tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("rewind.db")).unwrap();
        let cap = 200i64;
        for ts in 1..=50 {
            store
                .commit_clip_tx(ts, "text/plain", &"x".repeat(20), cap, || true)
                .unwrap();
        }
        let total = Store::managed_total(&store.conn);
        assert!(
            total <= cap,
            "clipboard-only growth must prune under cap (got {total})"
        );
        let n: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
            .unwrap();
        assert!(n > 0 && n < 50, "oldest clips pruned, newest kept (n={n})");
    }

    #[test]
    fn frames_only_db_clips_query_is_empty_not_error() {
        // A main file that has frames but not clips (WAL not checkpointed,
        // crash mid-migrate, or a legacy db) must not fail the overlay's
        // clips/moment refresh with `no such table: clips`.
        let dir = tempdir().unwrap();
        let db = dir.path().join("rewind.db");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE frames (
                    ts INTEGER PRIMARY KEY, path TEXT NOT NULL, app TEXT, title TEXT,
                    workspace TEXT, output TEXT, width INTEGER, height INTEGER,
                    out_w INTEGER, out_h INTEGER, crop_x INTEGER, crop_y INTEGER,
                    crop_w INTEGER, crop_h INTEGER, bytes INTEGER, dhash INTEGER,
                    encoder TEXT, crop_path TEXT
                );
                INSERT INTO frames (ts, path, app, title, workspace, output,
                    width, height, out_w, out_h, crop_x, crop_y, crop_w, crop_h,
                    bytes, dhash, encoder)
                VALUES (1, 'frames/1.webp', 'kitty', 't', '1', 'eDP-1',
                    100, 80, 100, 80, 0, 0, 100, 80, 10, 1, 'png');",
            )
            .unwrap();
        }
        let ro = Store::open_read_only(&db).unwrap();
        let v = ro.clips(10).unwrap();
        assert_eq!(v["clips"].as_array().unwrap().len(), 0);
        assert_eq!(ro.clip_at(1).unwrap(), "");
        let m = ro.moment(1).unwrap();
        assert_eq!(m["clip"].as_str().unwrap(), "");
    }

    #[test]
    fn writable_open_migrates_missing_clips_table() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("rewind.db");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE frames (
                    ts INTEGER PRIMARY KEY, path TEXT NOT NULL, app TEXT, title TEXT,
                    workspace TEXT, output TEXT, width INTEGER, height INTEGER,
                    out_w INTEGER, out_h INTEGER, crop_x INTEGER, crop_y INTEGER,
                    crop_w INTEGER, crop_h INTEGER, bytes INTEGER, dhash INTEGER,
                    encoder TEXT, crop_path TEXT
                );",
            )
            .unwrap();
        }
        let store = Store::open(&db).unwrap();
        store.insert_clip(42, "text/plain", "hello").unwrap();
        let v = store.clips(10).unwrap();
        assert_eq!(v["clips"].as_array().unwrap().len(), 1);
        assert_eq!(v["clips"][0]["content"], "hello");
        drop(store);
        // Schema must be in the main file, not only WAL, so a later
        // immutable read-only open still sees clips.
        let ro = Store::open_read_only(&db).unwrap();
        let v = ro.clips(10).unwrap();
        assert_eq!(v["clips"].as_array().unwrap().len(), 1);
    }
}
