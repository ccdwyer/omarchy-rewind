use crate::hypr::Client;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrivateHit {
    pub class: String,
    pub title: String,
    pub marker: String,
    pub heuristic: bool,
}

pub fn matches_class(class: &str, exclude: &[String]) -> bool {
    let c = class.to_ascii_lowercase();
    exclude.iter().any(|ex| {
        let e = ex.to_ascii_lowercase();
        !e.is_empty() && (c == e || c.contains(&e) || e.contains(&c))
    })
}

pub fn client_visible(c: &Client) -> bool {
    c.mapped && !c.hidden
}

pub fn excluded_visible(clients: &[Client], exclude: &[String]) -> Option<String> {
    for c in clients {
        if client_visible(c) && matches_class(&c.class, exclude) {
            return Some(c.class.clone());
        }
    }
    None
}

/// Documented title markers for the top-3 browsers. Labeled heuristic.
pub fn private_markers() -> &'static [(&'static str, &'static str)] {
    &[
        ("firefox", "(Private Browsing)"),
        ("firefox", "Private Browsing"),
        ("librewolf", "Private Browsing"),
        ("google-chrome", "Incognito"),
        ("google-chrome", "(Incognito)"),
        ("chromium", "Incognito"),
        ("chromium", "(Incognito)"),
        ("chrome", "Incognito"),
        ("brave-browser", "Private"),
        ("brave", "Private Window"),
        ("brave", "Private"),
    ]
}

pub fn private_browsing(clients: &[Client]) -> Option<PrivateHit> {
    for c in clients {
        if !client_visible(c) {
            continue;
        }
        let class = c.class.to_ascii_lowercase();
        for (browser, marker) in private_markers() {
            if class.contains(browser) && c.title.contains(marker) {
                return Some(PrivateHit {
                    class: c.class.clone(),
                    title: c.title.clone(),
                    marker: (*marker).to_string(),
                    heuristic: true,
                });
            }
        }
    }
    None
}

pub fn title_pause(clients: &[Client], patterns: &[String]) -> Option<String> {
    if patterns.is_empty() {
        return None;
    }
    for c in clients {
        if !client_visible(c) {
            continue;
        }
        let hay = format!("{} {}", c.class, c.title).to_ascii_lowercase();
        for p in patterns {
            let pat = p.to_ascii_lowercase();
            if !pat.is_empty() && hay.contains(&pat) {
                return Some(p.clone());
            }
        }
    }
    None
}

pub fn self_test() -> Result<(), String> {
    let keep = Client {
        class: "org.keepassxc.KeePassXC".into(),
        title: "KeePassXC".into(),
        mapped: true,
        hidden: false,
        ..Client::default()
    };
    let term = Client {
        class: "kitty".into(),
        title: "zsh".into(),
        mapped: true,
        hidden: false,
        ..Client::default()
    };
    let exclude = vec!["keepassxc".to_string()];
    if excluded_visible(&[keep.clone(), term.clone()], &exclude).is_none() {
        return Err("keepassxc on any output should pause".into());
    }
    if excluded_visible(std::slice::from_ref(&term), &exclude).is_some() {
        return Err("unrelated class must not pause".into());
    }
    let incog = Client {
        class: "google-chrome".into(),
        title: "Secret — Google Chrome (Incognito)".into(),
        mapped: true,
        hidden: false,
        ..Client::default()
    };
    if private_browsing(&[incog]).is_none() {
        return Err("chrome incognito marker missed".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(class: &str, title: &str, mapped: bool) -> Client {
        Client {
            class: class.into(),
            title: title.into(),
            mapped,
            hidden: false,
            ..Client::default()
        }
    }

    #[test]
    fn visible_anywhere_not_just_focused() {
        let clients = vec![
            win("kitty", "demo", true),
            win("keepassxc", "db", true),
        ];
        assert_eq!(
            excluded_visible(&clients, &["keepassxc".into()]).as_deref(),
            Some("keepassxc")
        );
    }

    #[test]
    fn hidden_excluded_does_not_pause() {
        let mut c = win("keepassxc", "db", true);
        c.hidden = true;
        assert!(excluded_visible(&[c], &["keepassxc".into()]).is_none());
    }

    #[test]
    fn firefox_private_is_heuristic() {
        let c = win("firefox", "Bank — Mozilla Firefox (Private Browsing)", true);
        let hit = private_browsing(&[c]).unwrap();
        assert!(hit.heuristic);
        assert_eq!(hit.marker, "(Private Browsing)");
    }

    #[test]
    fn user_title_pattern() {
        let c = win("slack", "ACME standup", true);
        assert_eq!(
            title_pause(&[c], &["standup".into()]).as_deref(),
            Some("standup")
        );
    }
}
