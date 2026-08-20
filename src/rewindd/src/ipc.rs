use crate::store::FrameInsert;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub enum Command {
    Hello {
        id: u64,
    },
    Arm {
        id: u64,
        settings: Value,
    },
    Disarm {
        id: u64,
    },
    Consent {
        id: u64,
        arm_now: bool,
        arm_on_login: bool,
    },
    SetPause {
        id: u64,
        reason: String,
        paused: bool,
    },
    Configure {
        id: u64,
        settings: Value,
    },
    Query {
        id: u64,
        q: String,
        limit: usize,
        from: i64,
        to: i64,
    },
    Timeline {
        id: u64,
        from: i64,
        to: i64,
        limit: usize,
    },
    Moment {
        id: u64,
        ts: i64,
    },
    Clips {
        id: u64,
        limit: usize,
    },
    ReopenPlan {
        id: u64,
        ts: i64,
    },
    ReopenWindow {
        id: u64,
        ts: i64,
        target: Value,
    },
    ReopenExec {
        id: u64,
        plan: Value,
    },
    Wipe {
        id: u64,
        scope: String,
        from: i64,
        to: i64,
    },
    CopyClip {
        id: u64,
        ts: i64,
    },
    Stats {
        id: u64,
    },
    Shutdown {
        id: u64,
    },
}

impl Command {
    pub fn parse(line: &str) -> Result<Self, String> {
        let v: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
        let cmd = v
            .get("cmd")
            .or_else(|| v.get("type"))
            .and_then(|c| c.as_str())
            .ok_or_else(|| "missing cmd".to_string())?;
        let id = v.get("id").and_then(|i| i.as_u64()).unwrap_or(0);
        match cmd {
            "hello" => Ok(Command::Hello { id }),
            "arm" => Ok(Command::Arm {
                id,
                settings: v.clone(),
            }),
            "disarm" => Ok(Command::Disarm { id }),
            "consent" => Ok(Command::Consent {
                id,
                arm_now: v.get("armNow").and_then(|x| x.as_bool()).unwrap_or(false),
                arm_on_login: v
                    .get("armOnLogin")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false),
            }),
            "set-pause" | "setPause" => Ok(Command::SetPause {
                id,
                reason: v
                    .get("reason")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                paused: v.get("paused").and_then(|x| x.as_bool()).unwrap_or(true),
            }),
            "configure" => Ok(Command::Configure {
                id,
                settings: v.clone(),
            }),
            "query" => Ok(Command::Query {
                id,
                q: v.get("q")
                    .or_else(|| v.get("query"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                limit: v.get("limit").and_then(|x| x.as_u64()).unwrap_or(50) as usize,
                from: v.get("from").and_then(|x| x.as_i64()).unwrap_or(0),
                to: v.get("to").and_then(|x| x.as_i64()).unwrap_or(0),
            }),
            "timeline" => Ok(Command::Timeline {
                id,
                from: v.get("from").and_then(|x| x.as_i64()).unwrap_or(0),
                to: v.get("to").and_then(|x| x.as_i64()).unwrap_or(0),
                limit: v.get("limit").and_then(|x| x.as_u64()).unwrap_or(400) as usize,
            }),
            "moment" => Ok(Command::Moment {
                id,
                ts: v.get("ts").and_then(|x| x.as_i64()).unwrap_or(0),
            }),
            "clips" => Ok(Command::Clips {
                id,
                limit: v.get("limit").and_then(|x| x.as_u64()).unwrap_or(100) as usize,
            }),
            "reopen-plan" | "reopenPlan" => Ok(Command::ReopenPlan {
                id,
                ts: v.get("ts").and_then(|x| x.as_i64()).unwrap_or(0),
            }),
            "reopen-window" | "reopenWindow" => Ok(Command::ReopenWindow {
                id,
                ts: v.get("ts").and_then(|x| x.as_i64()).unwrap_or(0),
                target: v.get("target").cloned().unwrap_or(json!({})),
            }),
            "reopen-exec" | "reopenExec" => Ok(Command::ReopenExec {
                id,
                plan: v.get("plan").cloned().unwrap_or(json!({})),
            }),
            "wipe" => Ok(Command::Wipe {
                id,
                scope: v
                    .get("scope")
                    .and_then(|x| x.as_str())
                    .unwrap_or("today")
                    .to_string(),
                from: v.get("from").and_then(|x| x.as_i64()).unwrap_or(0),
                to: v.get("to").and_then(|x| x.as_i64()).unwrap_or(0),
            }),
            "copy-clip" | "copyClip" => Ok(Command::CopyClip {
                id,
                ts: v.get("ts").and_then(|x| x.as_i64()).unwrap_or(0),
            }),
            "stats" | "status" => Ok(Command::Stats { id }),
            "shutdown" => Ok(Command::Shutdown { id }),
            other => Err(format!("unknown cmd: {other}")),
        }
    }
}

pub struct Event {
    pub value: Value,
}

impl Event {
    pub fn to_json(&self) -> String {
        self.value.to_string()
    }

    pub fn ready(armed: bool, consent: bool, encoder: &str) -> Self {
        Self {
            value: json!({
                "event": "ready",
                "armed": armed,
                "consentShown": consent,
                "helper": "rewindd",
                "encoder": encoder
            }),
        }
    }

    pub fn reply(id: u64, data: Value) -> Self {
        Self {
            value: json!({"event":"reply","id": id, "ok": true, "data": data}),
        }
    }

    pub fn error(id: Option<u64>, error: String) -> Self {
        Self {
            value: json!({"event":"error","id": id, "ok": false, "error": error}),
        }
    }

    pub fn from_stats(data: Value) -> Self {
        let mut v = data;
        if let Some(obj) = v.as_object_mut() {
            obj.insert("event".into(), json!("stats"));
        }
        Self { value: v }
    }

    pub fn frame_written(f: &FrameInsert) -> Self {
        Self {
            value: json!({
                "event": "frame-written",
                "ts": f.ts,
                "path": f.path,
                "app": f.app,
                "title": f.title,
                "workspace": f.workspace,
                "bytes": f.bytes,
                "encoder": f.encoder
            }),
        }
    }

    pub fn ocr_progress(done: i64, queued: i64) -> Self {
        Self {
            value: json!({"event":"ocr-progress","done": done, "queued": queued}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arm_and_query() {
        let a = Command::parse(r#"{"cmd":"arm","id":2,"byteCapGb":2}"#).unwrap();
        match a {
            Command::Arm { id, settings } => {
                assert_eq!(id, 2);
                assert_eq!(settings["byteCapGb"], 2);
            }
            _ => panic!("not arm"),
        }
        let q = Command::parse(r#"{"cmd":"query","id":3,"q":"hello"}"#).unwrap();
        match q {
            Command::Query { q, .. } => assert_eq!(q, "hello"),
            _ => panic!("not query"),
        }
    }
}
