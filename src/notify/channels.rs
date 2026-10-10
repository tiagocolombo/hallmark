//! Where a notification goes: the desktop (notify-rust), ntfy, Telegram or
//! a Slack webhook. Messages carry only the item, the question and its
//! options, never code, diffs or file contents.
//!
//! Test hooks, so tests never need a desktop or the network:
//! - `HALL_NOTIFY_LOG=<file>` appends desktop notifications to a JSONL file
//!   instead of showing them;
//! - `HALL_NTFY_URL` and `HALL_TELEGRAM_URL` replace `https://<ntfy host>`
//!   and `https://api.telegram.org`.

use std::io::Write;
use std::time::Duration;

use serde::Serialize;

/// One notification.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Message {
    pub title: String,
    pub body: String,
    /// ntfy tags (emoji short codes), such as `question`.
    pub tags: Vec<String>,
    /// ntfy priority, 1 (min) to 5 (max).
    pub priority: u8,
}

/// A configured destination.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Desktop,
    /// POST to `url` (`https://ntfy.sh/<topic>`).
    Ntfy {
        url: String,
    },
    Telegram {
        token: String,
        chat: String,
    },
    Slack {
        url: String,
    },
}

impl Target {
    pub fn describe(&self) -> String {
        match self {
            Target::Desktop => "desktop".into(),
            Target::Ntfy { url } => format!("ntfy {url}"),
            Target::Telegram { chat, .. } => format!("telegram chat {chat}"),
            Target::Slack { .. } => "slack webhook".into(),
        }
    }
}

/// `ntfy://ntfy.sh/<topic>` (or an `https://` ntfy URL) and
/// `telegram://<bot-token>@<chat-id>`.
pub fn parse_phone(s: &str) -> Option<Target> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("ntfy://") {
        let (host, topic) = rest.split_once('/')?;
        let topic = topic.trim_matches('/');
        if host.is_empty() || topic.is_empty() {
            return None;
        }
        let base = std::env::var("HALL_NTFY_URL")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| format!("https://{host}"));
        return Some(Target::Ntfy {
            url: format!("{}/{topic}", base.trim_end_matches('/')),
        });
    }
    if let Some(rest) = s.strip_prefix("telegram://") {
        let (token, chat) = rest.rsplit_once('@')?;
        if token.is_empty() || chat.is_empty() {
            return None;
        }
        return Some(Target::Telegram {
            token: token.into(),
            chat: chat.into(),
        });
    }
    if s.starts_with("https://") || s.starts_with("http://") {
        return Some(Target::Ntfy { url: s.into() });
    }
    None
}

/// The target for a route channel name, if it is configured.
pub fn target(cfg: &crate::config::Notify, channel: &str) -> Option<Target> {
    match channel {
        "desktop" if cfg.desktop => Some(Target::Desktop),
        "phone" => cfg.phone.as_deref().and_then(parse_phone),
        "slack" => cfg
            .slack
            .as_deref()
            .filter(|u| !u.trim().is_empty())
            .map(|u| Target::Slack {
                url: u.trim().into(),
            }),
        _ => None,
    }
}

/// Sends `m`. Errors are text for the state file; they never fail a tick.
pub fn send(t: &Target, m: &Message) -> Result<(), String> {
    match t {
        Target::Desktop => desktop(m),
        Target::Ntfy { url } => {
            let tags = m.tags.join(",");
            agent()
                .post(url)
                .header("Title", &ascii(&m.title))
                .header("Tags", &ascii(&tags))
                .header("Priority", &m.priority.to_string())
                .send(m.body.as_str())
                .map(|_| ())
                .map_err(|e| format!("ntfy: {e}"))
        }
        Target::Telegram { token, chat } => {
            let base = std::env::var("HALL_TELEGRAM_URL")
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "https://api.telegram.org".into());
            let url = format!("{}/bot{token}/sendMessage", base.trim_end_matches('/'));
            agent()
                .post(&url)
                .send_json(serde_json::json!({
                    "chat_id": chat,
                    "text": format!("{}\n{}", m.title, m.body),
                    "disable_web_page_preview": true,
                }))
                .map(|_| ())
                // The URL holds the bot token: never echo it.
                .map_err(|e| {
                    format!(
                        "telegram: {}",
                        e.to_string().replace(token.as_str(), "<token>")
                    )
                })
        }
        Target::Slack { url } => agent()
            .post(url)
            .send_json(serde_json::json!({ "text": format!("*{}*\n{}", m.title, m.body) }))
            .map(|_| ())
            .map_err(|e| format!("slack: {e}")),
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .new_agent()
}

/// HTTP headers must be ASCII: drop anything else.
fn ascii(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn desktop(m: &Message) -> Result<(), String> {
    if let Some(log) = std::env::var_os("HALL_NOTIFY_LOG") {
        let line = serde_json::json!({"channel": "desktop", "title": m.title, "body": m.body});
        return std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .and_then(|mut f| writeln!(f, "{line}"))
            .map_err(|e| format!("desktop log: {e}"));
    }
    #[cfg(target_os = "macos")]
    {
        // Deliver as Terminal so the notification has an app to open. A
        // second call reports "already set", which is fine.
        let _ = notify_rust::set_application(&notify_rust::get_bundle_identifier_or_default(
            "Terminal",
        ));
    }
    notify_rust::Notification::new()
        .appname("Hallmark")
        .summary(&m.title)
        .body(&m.body)
        .show()
        .map(|_| ())
        .map_err(|e| format!("desktop: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_urls() {
        assert_eq!(
            parse_phone("telegram://123:ABC-def@-100200"),
            Some(Target::Telegram {
                token: "123:ABC-def".into(),
                chat: "-100200".into()
            })
        );
        assert_eq!(parse_phone("telegram://nochat"), None);
        assert_eq!(parse_phone("ntfy://ntfy.sh/"), None);
        assert_eq!(parse_phone("sms://123"), None);
        assert_eq!(
            parse_phone("https://ntfy.example.com/team"),
            Some(Target::Ntfy {
                url: "https://ntfy.example.com/team".into()
            })
        );
        // ntfy:// URLs are covered by the integration tests, which set
        // HALL_NTFY_URL; reading it here would race other unit tests.
    }

    #[test]
    fn headers_are_ascii() {
        assert_eq!(ascii("app · H-0001 question"), "app H-0001 question");
    }
}
