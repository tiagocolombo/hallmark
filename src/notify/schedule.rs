//! What is due, as a pure function of (state, inbox, now, config), so the
//! cadence can be tested with any clock.
//!
//! - A new inbox entry goes to the channels its kind routes to.
//! - An entry still waiting after `remind` is sent again (per channel).
//! - During quiet hours only the desktop shows; the phone and Slack wait
//!   and get it when quiet hours end.
//! - An entry that left the inbox is forgotten.
//! - The morning digest is due once per local day, from `digest` on.

use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Notify;
use crate::index::InboxEntry;
use crate::item::ItemId;

/// `<hallmark dir>/notify-state.json`: what was sent, per inbox entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NotifyState {
    #[serde(default)]
    pub entries: BTreeMap<String, Tracked>,
    /// The local date of the last morning digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest_day: Option<NaiveDate>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tracked {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_sent: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sent: Option<DateTime<Utc>>,
    /// Sends that reached at least one channel.
    #[serde(default)]
    pub count: u32,
    /// The last successful send per channel.
    #[serde(default)]
    pub channels: BTreeMap<String, DateTime<Utc>>,
    /// The last failure per channel, retried on the next tick.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub errors: BTreeMap<String, String>,
}

/// A stable key for an inbox entry: kind, item, then the question or the
/// waiting session.
pub fn key(e: &InboxEntry) -> String {
    let mut k = e.kind.clone();
    if let Some(i) = e.item {
        k.push(':');
        k.push_str(&i.to_string());
    }
    if let Some(q) = &e.qid {
        k.push(':');
        k.push_str(q);
    } else if e.kind == "session_waiting"
        && let Some(s) = &e.session
    {
        k.push(':');
        k.push_str(s);
    }
    k
}

/// One entry's sends this tick.
#[derive(Debug, Clone, PartialEq)]
pub struct Due {
    pub key: String,
    /// Index into the entries passed to [`plan`].
    pub entry: usize,
    pub channels: Vec<String>,
    /// It was sent before: this is a reminder.
    pub reminder: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub sends: Vec<Due>,
    /// Keys no longer in the inbox.
    pub forget: Vec<String>,
    /// The digest's channels, when it is due.
    pub digest: Option<Vec<String>>,
}

/// Channels that only wait during quiet hours.
pub fn is_remote(channel: &str) -> bool {
    channel != "desktop"
}

/// The configured channels: desktop (when on), phone, slack.
pub fn available(cfg: &Notify) -> Vec<String> {
    let mut v = Vec::new();
    if cfg.desktop {
        v.push("desktop".to_string());
    }
    if cfg.phone.as_deref().is_some_and(|p| !p.trim().is_empty()) {
        v.push("phone".into());
    }
    if cfg.slack.as_deref().is_some_and(|p| !p.trim().is_empty()) {
        v.push("slack".into());
    }
    v
}

/// "22:00-07:30" → (start, end); may wrap midnight.
pub fn parse_quiet(s: &str) -> Option<(NaiveTime, NaiveTime)> {
    let (a, b) = s.split_once('-')?;
    Some((parse_time(a)?, parse_time(b)?))
}

/// "08:30" or "8:30".
pub fn parse_time(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()
}

/// Is `t` inside quiet hours? The start is inside, the end is not.
pub fn in_quiet(quiet: Option<&str>, t: NaiveTime) -> bool {
    let Some((start, end)) = quiet.and_then(parse_quiet) else {
        return false;
    };
    if start <= end {
        t >= start && t < end
    } else {
        t >= start || t < end
    }
}

/// The reminder interval in seconds; None turns reminders off ("off",
/// "never", "0").
pub fn remind_secs(cfg: &Notify) -> Option<i64> {
    let r = cfg.remind.trim().to_ascii_lowercase();
    if matches!(r.as_str(), "off" | "never" | "none" | "0" | "") {
        return None;
    }
    Some(
        crate::config::parse_duration(&r)
            .filter(|s| *s > 0)
            .unwrap_or(7200),
    )
}

/// The digest's channels: the phone and Slack when configured, else the
/// desktop.
pub fn digest_channels(available: &[String]) -> Vec<String> {
    let remote: Vec<String> = available.iter().filter(|c| is_remote(c)).cloned().collect();
    if !remote.is_empty() {
        remote
    } else if available.iter().any(|c| c == "desktop") {
        vec!["desktop".into()]
    } else {
        Vec::new()
    }
}

pub fn plan(
    state: &NotifyState,
    entries: &[InboxEntry],
    now: DateTime<FixedOffset>,
    cfg: &Notify,
) -> Plan {
    let avail = available(cfg);
    let utc = now.with_timezone(&Utc);
    let quiet = in_quiet(cfg.quiet_hours.as_deref(), now.time());
    let remind = remind_secs(cfg);
    let mut p = Plan::default();
    let mut live = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let k = key(e);
        live.push(k.clone());
        let tracked = state.entries.get(&k);
        let mut channels = Vec::new();
        for c in cfg.route.channels(&e.kind) {
            if !avail.contains(c) || channels.contains(c) {
                continue;
            }
            if quiet && is_remote(c) {
                continue;
            }
            let due = match tracked.and_then(|t| t.channels.get(c)) {
                None => true,
                Some(last) => remind.is_some_and(|r| (utc - *last).num_seconds() >= r),
            };
            if due {
                channels.push(c.clone());
            }
        }
        if !channels.is_empty() {
            p.sends.push(Due {
                key: k,
                entry: i,
                channels,
                reminder: tracked.is_some_and(|t| t.count > 0),
            });
        }
    }
    p.forget = state
        .entries
        .keys()
        .filter(|k| !live.contains(k))
        .cloned()
        .collect();
    if let Some(at) = parse_time(&cfg.digest)
        && now.time() >= at
        && state.digest_day != Some(now.date_naive())
    {
        let ch: Vec<String> = digest_channels(&avail)
            .into_iter()
            .filter(|c| !(quiet && is_remote(c)))
            .collect();
        if !ch.is_empty() {
            p.digest = Some(ch);
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(h: u32, m: u32) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(2 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 10, 9, h, m, 0)
            .unwrap()
    }

    fn entry(kind: &str, item: u32, qid: Option<&str>) -> InboxEntry {
        InboxEntry {
            kind: kind.into(),
            item: Some(ItemId(item)),
            qid: qid.map(String::from),
            text: "billed or raw?".into(),
            options: vec!["billed".into(), "raw".into()],
            default: None,
            session: None,
            since: Utc::now(),
            answer_with: vec![],
        }
    }

    fn cfg() -> Notify {
        Notify {
            phone: Some("ntfy://ntfy.sh/hallmark-test".into()),
            quiet_hours: Some("22:00-07:30".into()),
            digest: String::new(),
            ..Notify::default()
        }
    }

    /// Applies a plan as if every send succeeded.
    fn sent(state: &mut NotifyState, p: &Plan, entries: &[InboxEntry], now: DateTime<FixedOffset>) {
        let utc = now.with_timezone(&Utc);
        for d in &p.sends {
            let t = state.entries.entry(d.key.clone()).or_default();
            t.kind = entries[d.entry].kind.clone();
            t.first_sent.get_or_insert(utc);
            t.last_sent = Some(utc);
            t.count += 1;
            for c in &d.channels {
                t.channels.insert(c.clone(), utc);
            }
        }
        for k in &p.forget {
            state.entries.remove(k);
        }
        if p.digest.is_some() {
            state.digest_day = Some(now.date_naive());
        }
    }

    #[test]
    fn keys_are_stable() {
        assert_eq!(
            key(&entry("question", 3, Some("Q-7"))),
            "question:H-0003:Q-7"
        );
        assert_eq!(key(&entry("ready", 3, None)), "ready:H-0003");
        let mut s = entry("session_waiting", 3, None);
        s.session = Some("abc".into());
        assert_eq!(key(&s), "session_waiting:H-0003:abc");
    }

    #[test]
    fn new_then_reminders_then_forget() {
        let c = cfg();
        let mut st = NotifyState::default();
        let es = vec![entry("question", 1, Some("Q-1")), entry("ready", 2, None)];
        let p = plan(&st, &es, at(10, 0), &c);
        assert_eq!(p.sends.len(), 2);
        assert_eq!(p.sends[0].channels, ["desktop", "phone"]);
        assert!(!p.sends[0].reminder);
        assert_eq!(
            p.sends[1].channels,
            ["desktop"],
            "ready routes to the desktop"
        );
        sent(&mut st, &p, &es, at(10, 0));
        // Nothing new an hour later.
        assert_eq!(plan(&st, &es, at(11, 0), &c).sends, vec![]);
        // Two hours later: reminders.
        let p = plan(&st, &es, at(12, 0), &c);
        assert_eq!(p.sends.len(), 2);
        assert!(p.sends.iter().all(|d| d.reminder));
        sent(&mut st, &p, &es, at(12, 0));
        // The question is answered: forgotten.
        let es2 = vec![entry("ready", 2, None)];
        let p = plan(&st, &es2, at(12, 5), &c);
        assert_eq!(p.forget, vec!["question:H-0001:Q-1".to_string()]);
        assert!(p.sends.is_empty());
    }

    #[test]
    fn quiet_hours_hold_the_phone() {
        let c = cfg();
        let mut st = NotifyState::default();
        let es = vec![entry("question", 1, Some("Q-1"))];
        let p = plan(&st, &es, at(23, 0), &c);
        assert_eq!(p.sends[0].channels, ["desktop"]);
        sent(&mut st, &p, &es, at(23, 0));
        let next_day = |h, m| at(h, m) + chrono::Duration::days(1);
        // 00:30: the desktop's reminder isn't due yet, the phone is quiet.
        let p = plan(&st, &es, next_day(0, 30), &c);
        assert!(p.sends.is_empty(), "{p:?}");
        // 07:00: the desktop reminds; the phone still waits.
        let p = plan(&st, &es, next_day(7, 0), &c);
        assert_eq!(p.sends[0].channels, ["desktop"]);
        sent(&mut st, &p, &es, next_day(7, 0));
        // 07:30: quiet hours end and the phone gets it.
        let p = plan(&st, &es, next_day(7, 30), &c);
        assert_eq!(p.sends[0].channels, ["phone"]);
        assert!(in_quiet(
            Some("22:00-07:30"),
            NaiveTime::from_hms_opt(3, 0, 0).unwrap()
        ));
        assert!(!in_quiet(
            Some("22:00-07:30"),
            NaiveTime::from_hms_opt(7, 30, 0).unwrap()
        ));
        assert!(in_quiet(
            Some("12:00-13:00"),
            NaiveTime::from_hms_opt(12, 30, 0).unwrap()
        ));
        assert!(!in_quiet(
            Some("nonsense"),
            NaiveTime::from_hms_opt(12, 30, 0).unwrap()
        ));
    }

    #[test]
    fn digest_once_a_day() {
        let mut c = cfg();
        c.digest = "08:30".into();
        let mut st = NotifyState::default();
        assert_eq!(plan(&st, &[], at(8, 0), &c).digest, None, "too early");
        let p = plan(&st, &[], at(8, 30), &c);
        assert_eq!(p.digest, Some(vec!["phone".to_string()]));
        sent(&mut st, &p, &[], at(8, 30));
        assert_eq!(plan(&st, &[], at(18, 0), &c).digest, None, "once a day");
        let tomorrow = at(9, 0) + chrono::Duration::days(1);
        assert!(plan(&st, &[], tomorrow, &c).digest.is_some());
        // Without a phone or Slack it goes to the desktop.
        c.phone = None;
        assert_eq!(
            plan(&NotifyState::default(), &[], at(9, 0), &c).digest,
            Some(vec!["desktop".to_string()])
        );
        // An empty digest setting turns it off.
        c.digest = String::new();
        assert_eq!(
            plan(&NotifyState::default(), &[], at(9, 0), &c).digest,
            None
        );
    }

    #[test]
    fn unconfigured_channels_and_reminders_off() {
        let mut c = cfg();
        c.phone = None;
        c.desktop = false;
        let es = vec![entry("question", 1, Some("Q-1"))];
        assert!(
            plan(&NotifyState::default(), &es, at(10, 0), &c)
                .sends
                .is_empty()
        );
        let mut c = cfg();
        c.remind = "off".into();
        let mut st = NotifyState::default();
        let p = plan(&st, &es, at(10, 0), &c);
        sent(&mut st, &p, &es, at(10, 0));
        assert!(plan(&st, &es, at(20, 0), &c).sends.is_empty());
        assert_eq!(remind_secs(&Notify::default()), Some(7200));
    }
}
