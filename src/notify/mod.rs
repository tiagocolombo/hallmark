//! Notifications (workstream D, ADR 0020): desktop, ntfy, Telegram, Slack,
//! the morning digest, quiet hours and reminders.
//!
//! The inbox is the source: [`tick`] reads `Index::inbox`, asks
//! [`schedule::plan`] what is due, sends it, records each send as a
//! `notified` event, and keeps per-entry state in
//! `<hallmark dir>/notify-state.json`. A failed send is recorded in the
//! state and retried next tick; it never fails the tick.

pub mod channels;
pub mod schedule;

use std::path::PathBuf;

use chrono::{DateTime, Duration, FixedOffset, Local, NaiveDate, Utc};

use crate::config::Notify;
use crate::error::Result;
use crate::event::{Body, Event};
use crate::index::{InboxEntry, Index};
use crate::item::{ItemId, State};
use crate::output::clip;
use crate::repo::Repo;
use crate::usage::fmt_tokens;
use crate::worker::{Progress, WorkerCtx};

use channels::Message;
use schedule::{NotifyState, Tracked};

pub fn state_path(repo: &Repo) -> PathBuf {
    repo.hall.root.join("notify-state.json")
}

pub fn load_state(repo: &Repo) -> NotifyState {
    std::fs::read_to_string(state_path(repo))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_state(repo: &Repo, s: &NotifyState) -> Result<()> {
    let p = state_path(repo);
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(s)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

/// Local time now. `HALL_NOTIFY_NOW` (RFC 3339) pins it, for tests.
pub fn now() -> DateTime<FixedOffset> {
    std::env::var("HALL_NOTIFY_NOW")
        .ok()
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .unwrap_or_else(|| Local::now().fixed_offset())
}

/// Sends what is new or due in this repo's inbox.
pub fn tick(ctx: &WorkerCtx) -> Result<Progress> {
    let repo = ctx.repo;
    let cfg = repo.config.notify();
    let ix = repo.index()?;
    let entries = ix.inbox()?;
    let mut state = load_state(repo);
    let now = now();
    let plan = schedule::plan(&state, &entries, now, &cfg);
    if plan.sends.is_empty() && plan.forget.is_empty() && plan.digest.is_none() {
        return Ok(Progress::Idle);
    }
    let utc = now.with_timezone(&Utc);
    let mut done = Vec::new();
    for due in &plan.sends {
        let e = &entries[due.entry];
        let msg = message(&repo.name, e, due.reminder);
        let ok = deliver(
            &cfg,
            &due.channels,
            &msg,
            state
                .entries
                .entry(due.key.clone())
                .or_insert_with(|| Tracked {
                    kind: e.kind.clone(),
                    item: e.item,
                    ..Default::default()
                }),
            utc,
        );
        if !ok.is_empty() {
            record(
                repo,
                e.item,
                Body::Notified {
                    key: due.key.clone(),
                    kind: e.kind.clone(),
                    channels: ok.clone(),
                },
            )?;
            done.push(format!(
                "{}{} via {}",
                if due.reminder { "reminded " } else { "" },
                due.key,
                ok.join(", ")
            ));
        }
    }
    for k in &plan.forget {
        state.entries.remove(k);
    }
    if let Some(chs) = &plan.digest {
        let today = now.date_naive();
        match digest(repo, &ix, &entries, now)? {
            // Nothing to report: no digest today.
            None => state.digest_day = Some(today),
            Some(msg) => {
                let mut t = Tracked::default();
                let ok = deliver(&cfg, chs, &msg, &mut t, utc);
                if !ok.is_empty() {
                    state.digest_day = Some(today);
                    record(
                        repo,
                        None,
                        Body::Notified {
                            key: format!("digest:{today}"),
                            kind: "digest".into(),
                            channels: ok.clone(),
                        },
                    )?;
                    done.push(format!("digest via {}", ok.join(", ")));
                }
            }
        }
    }
    save_state(repo, &state)?;
    if done.is_empty() {
        return Ok(Progress::Idle);
    }
    repo.hall.live(&format!("notify: {}", done.join("; ")));
    Ok(Progress::worked(format!("notified: {}", done.join("; "))))
}

/// Sends to each channel; returns the ones that worked and updates `t`.
fn deliver(
    cfg: &Notify,
    channels: &[String],
    msg: &Message,
    t: &mut Tracked,
    now: DateTime<Utc>,
) -> Vec<String> {
    let mut ok = Vec::new();
    for c in channels {
        let Some(target) = channels::target(cfg, c) else {
            t.errors
                .insert(c.clone(), format!("{c} is not configured correctly"));
            continue;
        };
        match channels::send(&target, msg) {
            Ok(()) => {
                t.channels.insert(c.clone(), now);
                t.errors.remove(c);
                ok.push(c.clone());
            }
            Err(e) => {
                t.errors.insert(c.clone(), e);
            }
        }
    }
    if !ok.is_empty() {
        t.first_sent.get_or_insert(now);
        t.last_sent = Some(now);
        t.count += 1;
    }
    ok
}

fn record(repo: &Repo, item: Option<ItemId>, body: Body) -> Result<()> {
    let e = Event::new("hall", item, body);
    match item {
        Some(id) => repo.hall.append_item(id, &e),
        None => repo.hall.append(&repo.hall.repo_events(), &e),
    }
}

fn kind_label(kind: &str) -> &str {
    match kind {
        "question" => "question",
        "escalated" => "escalated",
        "ready" => "ready?",
        "session_waiting" => "session waiting",
        "budget" => "budget stop",
        "run_failed" => "run failed",
        "ship_failed" => "ship failed",
        "conflict" => "conflict",
        "feedback" => "PR comments",
        "stuck" => "stuck",
        "daemon_stopped" => "daemon stopped",
        "learnings" => "learnings",
        other => other,
    }
}

fn tags(kind: &str) -> (Vec<String>, u8) {
    let (t, p) = match kind {
        "question" => ("question", 4),
        "escalated" => ("rotating_light", 4),
        "stuck" | "daemon_stopped" => ("warning", 4),
        "ready" => ("white_check_mark", 3),
        "budget" => ("money_with_wings", 3),
        "run_failed" | "ship_failed" => ("x", 3),
        "conflict" => ("crossed_swords", 4),
        "feedback" => ("speech_balloon", 4),
        "session_waiting" => ("hourglass", 3),
        "digest" => ("sunrise", 2),
        _ => ("bell", 3),
    };
    (vec![t.to_string(), "hallmark".into()], p)
}

/// The notification for one inbox entry: the repo, the item, what waits
/// and, for a question, its options and the answer command. Nothing else.
pub fn message(repo: &str, e: &InboxEntry, reminder: bool) -> Message {
    let what = match e.item {
        Some(i) => format!("{i} {}", kind_label(&e.kind)),
        None => kind_label(&e.kind).to_string(),
    };
    let title = format!("{}{repo}: {what}", if reminder { "Reminder: " } else { "" });
    let mut body = clip(&e.text, 300);
    if !e.options.is_empty() {
        let opts: Vec<String> = e
            .options
            .iter()
            .enumerate()
            .map(|(i, o)| format!("[{}] {}", (b'a' + (i as u8 % 26)) as char, clip(o, 60)))
            .collect();
        body.push('\n');
        body.push_str(&opts.join("  "));
    }
    if let Some(d) = &e.default {
        body.push_str(&format!(" (default: {})", clip(d, 60)));
    }
    if let Some(cmd) = e.answer_with.first() {
        body.push('\n');
        body.push_str(cmd);
    }
    let (tags, priority) = tags(&e.kind);
    Message {
        title,
        body,
        tags,
        priority,
    }
}

/// The morning digest: what waits, what closed yesterday, and yesterday's
/// usage (local days). None when there is nothing to say.
pub fn digest(
    repo: &Repo,
    ix: &Index,
    entries: &[InboxEntry],
    now: DateTime<FixedOffset>,
) -> Result<Option<Message>> {
    let today = now.date_naive();
    let yesterday = today - Duration::days(1);
    let (from, to) = (local_start(yesterday, now), local_start(today, now));
    let closed: Vec<String> = ix
        .items(true)?
        .into_iter()
        .filter(|i| i.state == State::Closed && i.state_since >= from && i.state_since < to)
        .map(|i| match &i.pr_url {
            Some(u) => format!("{} {} ({u})", i.id, clip(&i.title, 50)),
            None => format!("{} {}", i.id, clip(&i.title, 50)),
        })
        .collect();
    let (loop_tokens, loop_cost, workshop) = usage_between(ix, from, to);
    if entries.is_empty() && closed.is_empty() && loop_tokens == 0 && workshop == 0 {
        return Ok(None);
    }
    let mut body = String::new();
    if entries.is_empty() {
        body.push_str("Nothing waits for you.\n");
    } else {
        body.push_str(&format!("Waiting for you ({}):\n", entries.len()));
        for e in entries.iter().take(10) {
            let who = e.item.map(|i| format!(" {i}")).unwrap_or_default();
            body.push_str(&format!(
                "- {}{who}: {}\n",
                kind_label(&e.kind),
                clip(&e.text, 80)
            ));
        }
        if entries.len() > 10 {
            body.push_str(&format!("- and {} more: hall inbox\n", entries.len() - 10));
        }
    }
    if !closed.is_empty() {
        body.push_str(&format!("Closed yesterday ({}):\n", closed.len()));
        for c in &closed {
            body.push_str(&format!("- {c}\n"));
        }
    }
    body.push_str(&format!(
        "Yesterday: the loop used {} tokens (≈ ${loop_cost:.2}); your sessions {}.",
        fmt_tokens(loop_tokens),
        fmt_tokens(workshop)
    ));
    let (tags, priority) = tags("digest");
    Ok(Some(Message {
        title: format!("{}: morning digest", repo.name),
        body,
        tags,
        priority,
    }))
}

/// The UTC instant a local day starts, in `now`'s offset.
fn local_start(day: NaiveDate, now: DateTime<FixedOffset>) -> DateTime<Utc> {
    day.and_hms_opt(0, 0, 0)
        .and_then(|t| t.and_local_timezone(*now.offset()).single())
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|| now.with_timezone(&Utc))
}

/// (loop tokens, loop cost, workshop tokens) between two instants.
fn usage_between(ix: &Index, from: DateTime<Utc>, to: DateTime<Utc>) -> (u64, f64, u64) {
    let q = |source: &str| -> (u64, f64) {
        ix.conn()
            .query_row(
                "SELECT SUM(COALESCE(input,0) + COALESCE(output,0) + COALESCE(cache_read,0)
                          + COALESCE(cache_write,0)), SUM(cost_usd)
                 FROM usage WHERE source = ?1 AND t >= ?2 AND t < ?3",
                rusqlite::params![source, from.to_rfc3339(), to.to_rfc3339()],
                |r| {
                    Ok((
                        r.get::<_, Option<i64>>(0)?.unwrap_or(0).max(0) as u64,
                        r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                    ))
                },
            )
            .unwrap_or((0, 0.0))
    };
    let (lt, lc) = q("headless");
    let (wt, _) = q("transcript");
    (lt, lc, wt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_shows_its_options_and_nothing_else() {
        let e = InboxEntry {
            kind: "question".into(),
            item: Some(ItemId(43)),
            qid: Some("Q-12".into()),
            text: "billed usage or raw events?".into(),
            options: vec!["billed".into(), "raw".into()],
            default: Some("billed".into()),
            session: Some("s".into()),
            since: Utc::now(),
            answer_with: vec!["hall answer Q-12 billed".into()],
        };
        let m = message("platform", &e, false);
        assert_eq!(m.title, "platform: H-0043 question");
        assert_eq!(
            m.body,
            "billed usage or raw events?\n[a] billed  [b] raw (default: billed)\nhall answer Q-12 billed"
        );
        assert_eq!(m.priority, 4);
        assert!(
            message("platform", &e, true)
                .title
                .starts_with("Reminder: ")
        );
    }
}
