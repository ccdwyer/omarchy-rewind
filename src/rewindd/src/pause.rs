use crate::exclude::PrivateHit;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PauseReason {
    Disarmed,
    Locked,
    Idle,
    Overlay,
    Excluded,
    Portal,
    PrivateBrowsing,
    TitlePattern,
}

impl PauseReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            PauseReason::Disarmed => "disarmed",
            PauseReason::Locked => "locked",
            PauseReason::Idle => "idle",
            PauseReason::Overlay => "overlay",
            PauseReason::Excluded => "excluded",
            PauseReason::Portal => "portal",
            PauseReason::PrivateBrowsing => "private-browsing",
            PauseReason::TitlePattern => "title-pattern",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PauseInput {
    pub armed: bool,
    pub locked: bool,
    pub idle_ms: u64,
    pub idle_limit_ms: u64,
    pub overlay_open: bool,
    pub excluded: Option<String>,
    pub portal_active: bool,
    pub private_browsing: Option<PrivateHit>,
    pub title_pause: Option<String>,
}

pub fn evaluate(input: &PauseInput) -> Option<PauseReason> {
    if !input.armed {
        return Some(PauseReason::Disarmed);
    }
    if input.locked {
        return Some(PauseReason::Locked);
    }
    if input.overlay_open {
        return Some(PauseReason::Overlay);
    }
    if input.portal_active {
        return Some(PauseReason::Portal);
    }
    if input.excluded.is_some() {
        return Some(PauseReason::Excluded);
    }
    if input.private_browsing.is_some() {
        return Some(PauseReason::PrivateBrowsing);
    }
    if input.title_pause.is_some() {
        return Some(PauseReason::TitlePattern);
    }
    if input.idle_limit_ms > 0 && input.idle_ms >= input.idle_limit_ms {
        return Some(PauseReason::Idle);
    }
    None
}

pub fn self_test() -> Result<(), String> {
    let mut i = PauseInput {
        armed: true,
        idle_limit_ms: 120_000,
        ..PauseInput::default()
    };
    if evaluate(&i).is_some() {
        return Err("idle machine should record".into());
    }
    i.locked = true;
    if evaluate(&i) != Some(PauseReason::Locked) {
        return Err("lock must force 0 fps".into());
    }
    i.locked = false;
    i.armed = false;
    if evaluate(&i) != Some(PauseReason::Disarmed) {
        return Err("disarm must force 0 fps".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn armed() -> PauseInput {
        PauseInput {
            armed: true,
            idle_limit_ms: 120_000,
            ..PauseInput::default()
        }
    }

    #[test]
    fn disarmed_wins() {
        let mut i = armed();
        i.armed = false;
        i.locked = true;
        assert_eq!(evaluate(&i), Some(PauseReason::Disarmed));
    }

    #[test]
    fn lock_idle_overlay_portal_exclusion() {
        let cases = [
            (
                PauseInput {
                    locked: true,
                    ..armed()
                },
                PauseReason::Locked,
            ),
            (
                PauseInput {
                    overlay_open: true,
                    ..armed()
                },
                PauseReason::Overlay,
            ),
            (
                PauseInput {
                    portal_active: true,
                    ..armed()
                },
                PauseReason::Portal,
            ),
            (
                PauseInput {
                    excluded: Some("keepassxc".into()),
                    ..armed()
                },
                PauseReason::Excluded,
            ),
            (
                PauseInput {
                    idle_ms: 180_000,
                    ..armed()
                },
                PauseReason::Idle,
            ),
        ];
        for (input, want) in cases {
            assert_eq!(evaluate(&input), Some(want));
        }
    }

    #[test]
    fn idle_under_limit_records() {
        let i = PauseInput {
            idle_ms: 30_000,
            ..armed()
        };
        assert_eq!(evaluate(&i), None);
    }
}
