//! `hall notify test` and `hall statusline` (workstream D).

use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use crate::cli::{NotifyAction, NotifyArgs};
use crate::config::{Notify, UserConfig};
use crate::error::{HallError, Result};
use crate::notify::channels::{self, Message};
use crate::output::{Render, Response};
use crate::repo::Repo;

#[derive(Debug, Serialize, JsonSchema)]
pub struct NotifyOut {
    pub sent: Vec<Sent>,
    /// A new private ntfy topic, written to your user config because no
    /// phone channel was set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_topic: Option<String>,
    /// How to receive it on your phone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscribe: Option<String>,
    /// The user config file, when it was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_config: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Sent {
    /// desktop, phone, slack.
    pub channel: String,
    pub ok: bool,
    /// True when nothing was sent (not configured, or a new topic nobody
    /// has subscribed to yet).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Render for NotifyOut {
    fn render(&self) -> String {
        let mut s = String::new();
        for x in &self.sent {
            let status = match (x.ok, x.skipped) {
                (_, true) => "skipped",
                (true, _) => "sent",
                (false, _) => "failed",
            };
            s.push_str(&format!("{:8} {status}", x.channel));
            if let Some(d) = &x.detail {
                s.push_str(&format!(" · {d}"));
            }
            s.push('\n');
        }
        if let Some(sub) = &self.subscribe {
            s.push_str(&format!("\n{sub}\n"));
        }
        s
    }
}

const CHANNELS: [&str; 3] = ["desktop", "phone", "slack"];

pub fn notify(cwd: &Path, a: NotifyArgs) -> Result<Response> {
    match a.action {
        NotifyAction::Test { channel } => test(cwd, channel),
    }
}

fn test(cwd: &Path, channel: Option<String>) -> Result<Response> {
    if let Some(c) = &channel
        && !CHANNELS.contains(&c.as_str())
    {
        return Err(HallError::usage(format!(
            "--channel {c}: use desktop, phone or slack"
        )));
    }
    let repo = Repo::discover(cwd).ok().filter(|r| r.initialized());
    let repo_notify = repo.as_ref().and_then(|r| r.config.notify.clone());
    let mut cfg: Notify = match &repo {
        Some(r) => r.config.notify(),
        None => UserConfig::load()?.notify.unwrap_or_default(),
    };
    let wants = |c: &str| channel.as_deref().is_none_or(|x| x == c);
    let mut out = NotifyOut {
        sent: Vec::new(),
        created_topic: None,
        subscribe: None,
        user_config: None,
    };
    // No phone yet: make a private topic, so the phone works once you
    // subscribe. A new topic has no subscribers, so it is only tested when
    // asked for by name.
    let mut fresh_topic = false;
    if cfg.phone.is_none() && wants("phone") {
        let topic = random_topic()?;
        let url = format!("ntfy://ntfy.sh/{topic}");
        let path = write_user_phone(&url)?;
        out.subscribe = Some(format!(
            "Your phone channel is the private ntfy topic {topic}. Install the ntfy app \
             (https://ntfy.sh), subscribe to {topic}, then run `hall notify test --channel phone`. \
             Keep the name to yourself: anyone who knows it can read the topic.{}",
            if repo_notify.is_some() {
                " This repo's .hallmark.toml has its own [notify], which wins over the user config: \
                 copy the phone line there too."
            } else {
                ""
            }
        ));
        out.created_topic = Some(topic);
        out.user_config = Some(crate::paths::display(&path));
        cfg.phone = Some(url);
        fresh_topic = channel.is_none();
    }
    let name = repo.as_ref().map(|r| r.name.clone()).unwrap_or_default();
    let msg = Message {
        title: "Hallmark: test".into(),
        body: if name.is_empty() {
            "Notifications from Hallmark reach you here.".into()
        } else {
            format!("Notifications from Hallmark about {name} reach you here.")
        },
        tags: vec!["bell".into(), "hallmark".into()],
        priority: 3,
    };
    for c in CHANNELS {
        if !wants(c) {
            continue;
        }
        let Some(target) = channels::target(&cfg, c) else {
            let set = match c {
                "phone" => cfg.phone.is_some(),
                "slack" => cfg.slack.is_some(),
                _ => false,
            };
            let detail = if c == "desktop" {
                "desktop notifications are off ([notify] desktop = false)".to_string()
            } else if set {
                format!("{c} is set but not understood; see `hall schema config`")
            } else {
                format!("{c} is not configured ([notify] {c} = …)")
            };
            if channel.is_some() || c == "desktop" || set {
                out.sent.push(Sent {
                    channel: c.into(),
                    ok: false,
                    skipped: true,
                    detail: Some(detail),
                });
            }
            continue;
        };
        if c == "phone" && fresh_topic {
            out.sent.push(Sent {
                channel: c.into(),
                ok: true,
                skipped: true,
                detail: Some(
                    "new topic: subscribe first, then test it with --channel phone".into(),
                ),
            });
            continue;
        }
        let r = channels::send(&target, &msg);
        out.sent.push(Sent {
            channel: c.into(),
            ok: r.is_ok(),
            skipped: false,
            detail: Some(match r {
                Ok(()) => target.describe(),
                Err(e) => e,
            }),
        });
    }
    let mut next = vec!["hall inbox".to_string()];
    if out.created_topic.is_some() {
        next.insert(0, "hall notify test --channel phone".into());
    }
    Ok(Response::new(out, next))
}

/// `hallmark-` and 10 random lowercase letters and digits.
pub fn random_topic() -> Result<String> {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut buf = [0u8; 10];
    getrandom::fill(&mut buf)
        .map_err(|e| HallError::internal(format!("no randomness for a topic: {e}")))?;
    // 252 is the largest multiple of 36 under 256; rejecting above it keeps
    // the letters uniform.
    let mut out = String::from("hallmark-");
    let mut i = 0;
    while out.len() < "hallmark-".len() + 10 {
        if i == buf.len() {
            getrandom::fill(&mut buf)
                .map_err(|e| HallError::internal(format!("no randomness for a topic: {e}")))?;
            i = 0;
        }
        let b = buf[i];
        i += 1;
        if b < 252 {
            out.push(ALPHABET[(b % 36) as usize] as char);
        }
    }
    Ok(out)
}

/// Sets `[notify] phone` in the user config with toml_edit, creating the
/// file and keeping whatever else it holds.
fn write_user_phone(url: &str) -> Result<PathBuf> {
    let path = UserConfig::path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| HallError::internal(format!("{} doesn't parse: {e}", path.display())))?;
    let notify = doc
        .entry("notify")
        .or_insert(toml_edit::table())
        .as_table_like_mut()
        .ok_or_else(|| HallError::internal("[notify] in the user config is not a table"))?;
    notify.insert("phone", toml_edit::value(url));
    let out = doc.to_string();
    toml::from_str::<UserConfig>(&out)?;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&path, out)?;
    Ok(path)
}

/// The status line Claude Code shows: printed as plain text, never an error.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct StatuslineOut(pub String);

/// Claude Code's `statusLine` command. Reads its JSON on stdin (`cwd`,
/// `workspace.current_dir`, `session_id`; anything missing is fine) and
/// prints one plain line, or nothing outside a Hallmark repo. No network,
/// no model.
pub fn statusline(cwd: &Path) -> Result<Response> {
    let mut buf = String::new();
    if !std::io::stdin().is_terminal() {
        let _ = std::io::stdin().read_to_string(&mut buf);
    }
    let input: Value = serde_json::from_str(&buf).unwrap_or(Value::Null);
    if let Some(line) = statusline_text(&input, cwd) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
    }
    Ok(Response::raw(None))
}

/// The line for a status-line input, or None outside a Hallmark repo.
pub fn statusline_text(input: &Value, cwd: &Path) -> Option<String> {
    fn s(v: Option<&Value>) -> Option<&str> {
        v.and_then(Value::as_str).filter(|x| !x.is_empty())
    }
    let dir = s(input.get("cwd"))
        .or_else(|| s(input.pointer("/workspace/current_dir")))
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.to_path_buf());
    let repo = Repo::discover(&dir).ok().filter(|r| r.initialized())?;
    let ix = repo.index().ok()?;
    let items = ix.items(false).ok()?;
    let mut parts = vec![repo.name.clone()];
    let item = repo.item_at(&items, &dir).or_else(|| {
        let sid = s(input.get("session_id"))?;
        let bound = repo.hall.session(sid).item?;
        items.iter().find(|i| i.id == bound).cloned()
    });
    if let Some(i) = item {
        let mut p = format!("{} {}", i.id, i.state);
        if let Some(h) = &i.hold {
            p.push_str(&format!(" (held: {})", h.kind.as_str()));
        }
        parts.push(p);
    }
    let waiting = ix.inbox().map(|v| v.len()).unwrap_or(0);
    if waiting > 0 {
        parts.push(format!(
            "⚑ {waiting} need{} you",
            if waiting == 1 { "s" } else { "" }
        ));
    }
    if let Some(st) = crate::daemon::state::load() {
        let now = chrono::Utc::now();
        if let Some(p) = &st.paused
            && p.until.is_none_or(|u| u > now)
        {
            parts.push("daemon paused".into());
        } else if st.stopped_unexpectedly(now).is_some() {
            parts.push("daemon stopped".into());
        }
    }
    if crate::budget::daily_stop(&ix, &repo.config.budget, &crate::budget::today()).is_some() {
        parts.push("daily budget spent".into());
    }
    Some(parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_are_random_and_well_formed() {
        let a = random_topic().unwrap();
        let b = random_topic().unwrap();
        assert_ne!(a, b);
        for t in [a, b] {
            let rest = t.strip_prefix("hallmark-").unwrap();
            assert_eq!(rest.len(), 10);
            assert!(
                rest.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            );
        }
    }
}
