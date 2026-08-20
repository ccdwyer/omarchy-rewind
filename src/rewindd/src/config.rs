use crate::perms;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

pub const DEFAULT_BYTE_CAP: i64 = 2 * 1024 * 1024 * 1024;
pub const DEFAULT_CADENCE_MS: u64 = 3000;
pub const DEFAULT_IDLE_PAUSE_SEC: u64 = 120;

pub fn default_excludes() -> Vec<String> {
    [
        "keepassxc",
        "1Password",
        "1password",
        "Bitwarden",
        "bitwarden",
        "seahorse",
        "polkit-gnome-authentication-agent-1",
        "polkit-kde-authentication-agent-1",
        "lxqt-policykit-agent",
        "mate-polkit",
        "xfce-polkit",
    ]
    .into_iter()
    .map(|s| s.to_string())
    .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub byte_cap: i64,
    pub cadence_ms: u64,
    pub idle_pause_sec: u64,
    pub exclude_apps: Vec<String>,
    pub title_pause_patterns: Vec<String>,
    pub arm_on_login: bool,
    pub armed: bool,
    pub consent_at: i64,
    pub skip_unchanged: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            byte_cap: DEFAULT_BYTE_CAP,
            cadence_ms: DEFAULT_CADENCE_MS,
            idle_pause_sec: DEFAULT_IDLE_PAUSE_SEC,
            exclude_apps: default_excludes(),
            title_pause_patterns: Vec::new(),
            arm_on_login: false,
            armed: false,
            consent_at: 0,
            skip_unchanged: true,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut s: Settings = serde_json::from_str(&raw).unwrap_or_default();
        if s.exclude_apps.is_empty() {
            s.exclude_apps = default_excludes();
        }
        if s.byte_cap <= 0 {
            s.byte_cap = DEFAULT_BYTE_CAP;
        }
        Ok(s)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            perms::secure_dir(parent).map_err(|e| e.to_string())?;
        }
        let body = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_file_name(format!(
            ".state-{}.tmp",
            std::process::id()
        ));
        let write_tmp = (|| -> Result<(), String> {
            let mut f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)
                .map_err(|e| e.to_string())?;
            f.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
            f.flush().map_err(|e| e.to_string())?;
            f.sync_all().map_err(|e| e.to_string())?;
            Ok(())
        })();
        if let Err(e) = write_tmp {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        perms::secure_file(&tmp).map_err(|e| e.to_string())?;
        if let Err(e) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.to_string());
        }
        perms::secure_file(path).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn merge_json(&mut self, v: &Value) {
        if let Some(n) = number_of(v, &["byteCap", "byteCapGb", "byte_cap"]) {
            if v.get("byteCapGb").is_some() || v.get("byte_cap_gb").is_some() {
                self.byte_cap = (n * 1024.0 * 1024.0 * 1024.0) as i64;
            } else if n > 0.0 && n < 128.0 {
                // Small numbers arriving from the bar schema are treated as GB.
                self.byte_cap = (n * 1024.0 * 1024.0 * 1024.0) as i64;
            } else {
                self.byte_cap = n as i64;
            }
        }
        if let Some(n) = number_of(v, &["cadenceMs", "cadence_ms"]) {
            self.cadence_ms = n.max(1000.0) as u64;
        }
        if let Some(n) = number_of(v, &["idlePauseSec", "idle_pause_sec"]) {
            self.idle_pause_sec = n.max(30.0) as u64;
        }
        if let Some(list) = string_list(v, &["excludeApps", "exclude_apps", "exclude"]) {
            if !list.is_empty() {
                self.exclude_apps = list;
            }
        }
        if let Some(list) = string_list(
            v,
            &["titlePausePatterns", "title_pause_patterns", "titlePause"],
        ) {
            self.title_pause_patterns = list;
        }
        if let Some(b) = v.get("armOnLogin").and_then(|x| x.as_bool()) {
            self.arm_on_login = b;
        }
        if let Some(b) = v.get("skipUnchanged").and_then(|x| x.as_bool()) {
            self.skip_unchanged = b;
        }
    }
}

fn number_of(v: &Value, keys: &[&str]) -> Option<f64> {
    for k in keys {
        if let Some(n) = v.get(*k).and_then(|x| x.as_f64()) {
            return Some(n);
        }
        if let Some(n) = v.get(*k).and_then(|x| x.as_i64()) {
            return Some(n as f64);
        }
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            if let Ok(n) = s.parse::<f64>() {
                return Some(n);
            }
        }
    }
    None
}

fn string_list(v: &Value, keys: &[&str]) -> Option<Vec<String>> {
    for k in keys {
        if let Some(arr) = v.get(*k).and_then(|x| x.as_array()) {
            return Some(
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .filter(|s| !s.is_empty())
                    .collect(),
            );
        }
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            return Some(split_csv(s));
        }
    }
    None
}

pub fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gb_schema_becomes_bytes() {
        let mut s = Settings::default();
        s.merge_json(&serde_json::json!({"byteCapGb": 2}));
        assert_eq!(s.byte_cap, DEFAULT_BYTE_CAP);
    }

    #[test]
    fn csv_excludes() {
        let mut s = Settings::default();
        s.merge_json(&serde_json::json!({"excludeApps": "foo, bar"}));
        assert_eq!(s.exclude_apps, vec!["foo", "bar"]);
    }

    #[test]
    fn save_replaces_via_temp_rename() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut s = Settings::default();
        s.consent_at = 99;
        s.save(&path).unwrap();
        let parsed: Settings =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.consent_at, 99);
        let tmps: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .collect();
        assert!(tmps.is_empty());
    }
}
