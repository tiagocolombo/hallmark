//! The `reverts` worker (ADR 0061): a merged PR that was reverted on its
//! base is a signal the loop missed something, and the one Anthropic turns
//! into an eval set and an incident review. Hallmark records it on the
//! item, puts it in your inbox, and the librarian distils it again, asking
//! which layer should have caught it.
//!
//! A script, never a model. For each base with a merged, unreverted item
//! delivered within [`WINDOW_DAYS`], it reads the base's recent history
//! (after the Delivery office's fetch of `origin/<base>`) at most every
//! [`EVERY`] seconds, finds the commit each PR landed as, and looks for a
//! later commit that reverts it: one whose body says "This reverts commit
//! <sha>" (what `git revert` and GitHub's Revert button write), or whose
//! subject is `Revert "<subject>"`.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::event::{Body, Event};
use crate::git;
use crate::item::{Item, Reverted, State};
use crate::repo::Repo;
use crate::worker::{Progress, WorkerCtx};

pub const ACTOR: &str = "reverts";
/// How long after an item closes its base is watched for a revert.
pub const WINDOW_DAYS: i64 = 30;
/// Seconds between two reads of the same base.
pub const EVERY: i64 = 600;
/// Commits of the base read per pass.
const DEPTH: usize = 400;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Seen {
    #[serde(default)]
    bases: BTreeMap<String, DateTime<Utc>>,
}

impl Seen {
    fn path(repo: &Repo) -> std::path::PathBuf {
        repo.hall.root.join("reverts.json")
    }

    fn load(repo: &Repo) -> Seen {
        std::fs::read_to_string(Self::path(repo))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, repo: &Repo) {
        if let Ok(s) = serde_json::to_string(self) {
            let _ = std::fs::write(Self::path(repo), s);
        }
    }
}

/// A merged item still worth a look: closed, delivered as merged, not yet
/// known to be reverted, closed within the window.
pub fn watched(i: &Item, now: DateTime<Utc>) -> bool {
    i.state == State::Closed
        && i.delivered.as_deref() == Some("merged")
        && i.reverted.is_none()
        && i.pr_url.as_deref().is_some_and(|u| u.contains("/pull/"))
        && now - i.delivered_at.unwrap_or(i.state_since) <= Duration::days(WINDOW_DAYS)
}

/// Whether `base` was read less than [`EVERY`] seconds ago.
fn throttled(seen: &Seen, base: &str, now: DateTime<Utc>) -> bool {
    seen.bases
        .get(base)
        .is_some_and(|t| now - *t < Duration::seconds(EVERY))
}

/// One commit of the base, newest first in the list.
#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    pub sha: String,
    pub subject: String,
    pub body: String,
}

/// Parses `git log --format=%H%x1f%s%x1f%b%x1e`.
pub fn parse_log(text: &str) -> Vec<Commit> {
    text.split('\x1e')
        .filter_map(|rec| {
            let rec = rec.trim_start_matches('\n');
            let mut parts = rec.splitn(3, '\x1f');
            let sha = parts.next()?.trim();
            if sha.is_empty() {
                return None;
            }
            Some(Commit {
                sha: sha.to_string(),
                subject: parts.next().unwrap_or("").trim().to_string(),
                body: parts.next().unwrap_or("").trim().to_string(),
            })
        })
        .collect()
}

/// The PR's number from its URL.
pub fn pr_number(url: &str) -> Option<u64> {
    url.rsplit('/').next()?.parse().ok()
}

/// The commit a PR landed as, in a log that is newest first: the merge
/// commit ("Merge pull request #N"), the squash commit ("… (#N)"), or the
/// PR's own head when it was rebased or fast-forwarded. The subject match
/// comes first: under a merge commit the head is in the history too, as its
/// second parent, but it is the merge commit that gets reverted. The oldest
/// match wins, so a later revert PR that names the number can't be mistaken
/// for the landing.
pub fn landed<'a>(log: &'a [Commit], number: u64, head: Option<&str>) -> Option<&'a Commit> {
    let squash = format!("(#{number})");
    let merge = format!("Merge pull request #{number} ");
    log.iter()
        .rev()
        .find(|c| {
            c.subject.starts_with(&merge)
                || (c.subject.ends_with(&squash) && !c.subject.starts_with("Revert \""))
        })
        .or_else(|| log.iter().rev().find(|c| head.is_some_and(|h| c.sha == h)))
}

/// A commit newer than `landed` that reverts it.
pub fn revert_of<'a>(log: &'a [Commit], landed: &Commit) -> Option<&'a Commit> {
    let marker = format!("This reverts commit {}", landed.sha);
    let short = format!(
        "This reverts commit {}",
        &landed.sha[..landed.sha.len().min(7)]
    );
    let subject = format!("Revert \"{}\"", landed.subject);
    log.iter().take_while(|c| c.sha != landed.sha).find(|c| {
        c.body.contains(&marker) || c.body.contains(&short) || c.subject.starts_with(&subject)
    })
}

pub fn tick(ctx: &WorkerCtx) -> Result<Progress> {
    let repo = ctx.repo;
    let now = Utc::now();
    let items = repo.index()?.items(true)?;
    let mut by_base: BTreeMap<&str, Vec<&Item>> = BTreeMap::new();
    for i in items.iter().filter(|i| watched(i, now)) {
        by_base.entry(i.base.as_str()).or_default().push(i);
    }
    if by_base.is_empty() {
        return Ok(Progress::Idle);
    }
    let mut seen = Seen::load(repo);
    let mut done = Vec::new();
    for (base, items) in by_base {
        if throttled(&seen, base, now) {
            continue;
        }
        seen.bases.insert(base.to_string(), now);
        seen.save(repo);
        let Some(target) = crate::delivery::base_ref(&repo.main_root, base) else {
            continue;
        };
        let log = match read_log(&repo.main_root, &target.git_ref) {
            Some(l) => l,
            None => continue,
        };
        for item in items {
            let Some(url) = item.pr_url.as_deref() else {
                continue;
            };
            let Some(number) = pr_number(url) else {
                continue;
            };
            let Some(l) = landed(
                &log,
                number,
                item.merged.as_deref().or(item.pr_head.as_deref()),
            ) else {
                continue;
            };
            let Some(r) = revert_of(&log, l) else {
                continue;
            };
            if record(repo, item, url, &target.name, l, r)? {
                let what = format!(
                    "{} was reverted on {}: {} \"{}\" undoes {}",
                    item.id,
                    target.name,
                    crate::event::short(&r.sha),
                    crate::output::clip(&r.subject, 60),
                    crate::event::short(&l.sha)
                );
                repo.hall.live(&what);
                done.push(what);
            }
        }
    }
    Ok(if done.is_empty() {
        Progress::Idle
    } else {
        Progress::worked(done.join("; "))
    })
}

fn read_log(dir: &Path, git_ref: &str) -> Option<Vec<Commit>> {
    let n = format!("-n{DEPTH}");
    let text = git::run(dir, &["log", &n, "--format=%H%x1f%s%x1f%b%x1e", git_ref]).ok()?;
    Some(parse_log(&text))
}

/// Appends `reverted` once, under the journal's lock.
fn record(
    repo: &Repo,
    item: &Item,
    url: &str,
    base: &str,
    landed: &Commit,
    revert: &Commit,
) -> Result<bool> {
    let (_, recorded) = repo.hall.transact(item.id, |it, _| {
        if it.reverted.is_some() {
            return Ok((vec![], false));
        }
        let body = Body::Reverted {
            pr_url: url.to_string(),
            base: base.to_string(),
            sha: landed.sha.clone(),
            revert_sha: revert.sha.clone(),
            subject: revert.subject.clone(),
        };
        Ok((vec![Event::new(ACTOR, Some(item.id), body)], true))
    })?;
    Ok(recorded)
}

/// The inbox line for a reverted item.
pub fn inbox_text(item: &Item, r: &Reverted) -> String {
    format!(
        "{}: its PR was reverted on {} by {} \"{}\". Which layer should have caught it?",
        item.title,
        r.base,
        crate::event::short(&r.revert_sha),
        crate::output::clip(&r.subject, 80)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log() -> Vec<Commit> {
        parse_log(
            "ccc3\x1fRevert \"Retry webhooks (#7)\" (#9)\x1fThis reverts commit aaa1.\n\x1e\n\
             bbb2\x1fchore: release (#8)\x1f\x1e\n\
             aaa1\x1fRetry webhooks (#7)\x1fBody\x1e\n\
             0000\x1finit\x1f\x1e",
        )
    }

    #[test]
    fn the_log_parses_newest_first_with_bodies() {
        let l = log();
        assert_eq!(l.len(), 4);
        assert_eq!(l[0].sha, "ccc3");
        assert_eq!(l[0].subject, "Revert \"Retry webhooks (#7)\" (#9)");
        assert_eq!(l[0].body, "This reverts commit aaa1.");
        assert_eq!(l[2].subject, "Retry webhooks (#7)");
        assert_eq!(l[3].body, "");
        assert!(parse_log("").is_empty());
    }

    #[test]
    fn a_squash_landing_is_found_and_its_revert_too() {
        let l = log();
        let l7 = landed(&l, 7, None).unwrap();
        assert_eq!(
            l7.sha, "aaa1",
            "the revert PR that names #7 is not the landing"
        );
        let r = revert_of(&l, l7).unwrap();
        assert_eq!(r.sha, "ccc3");
        // PR #8 landed and was not reverted; #12 never landed.
        assert!(revert_of(&l, landed(&l, 8, None).unwrap()).is_none());
        assert!(landed(&l, 12, None).is_none());
    }

    #[test]
    fn merge_commits_rebased_heads_and_subject_only_reverts_count() {
        let l = parse_log(
            "r1\x1fRevert \"feat: thing\"\x1f\x1e\
             m1\x1fMerge pull request #3 from acme/x\x1fthing\x1e\
             h1\x1ffeat: thing\x1f\x1e",
        );
        assert_eq!(landed(&l, 3, None).unwrap().sha, "m1");
        let h = landed(&l, 99, Some("h1")).unwrap();
        assert_eq!(h.sha, "h1");
        assert_eq!(revert_of(&l, h).unwrap().sha, "r1");
        // A commit older than the landing is never its revert.
        let old = parse_log("l1\x1fx (#4)\x1f\x1e r0\x1fRevert \"x (#4)\"\x1f\x1e");
        assert!(revert_of(&old, landed(&old, 4, None).unwrap()).is_none());
    }

    #[test]
    fn a_merge_commit_is_reverted_not_its_head() {
        // `git revert -m 1 M`: the head h1 sits after M in the log.
        let l = parse_log(
            "r1\x1fRevert \"Merge pull request #3 from acme/x\"\x1fThis reverts commit m1, reversing changes made to p0.\x1e\
             m1\x1fMerge pull request #3 from acme/x\x1fthing\x1e\
             h1\x1ffeat: thing\x1f\x1e\
             p0\x1finit\x1f\x1e",
        );
        let m = landed(&l, 3, Some("h1")).unwrap();
        assert_eq!(m.sha, "m1");
        assert_eq!(revert_of(&l, m).unwrap().sha, "r1");
    }

    fn shipped(delivered_ago: i64, state_ago: i64) -> Item {
        let now = Utc::now();
        let t = now - Duration::days(state_ago);
        Item {
            id: crate::item::ItemId(1),
            title: "t".into(),
            state: State::Closed,
            branch: "b".into(),
            worktree: "w".into(),
            base: "main".into(),
            issue: None,
            created_at: t,
            updated_at: t,
            state_since: t,
            round: 1,
            rejects: 0,
            check_failures: 0,
            frozen_sha: None,
            hallmark_sha: None,
            pr_url: Some("https://github.com/acme/app/pull/3".into()),
            pr_head: None,
            summary: None,
            drop_reason: None,
            adopted: false,
            events: 2,
            parent: None,
            follow_up: None,
            ci: None,
            rebase: None,
            feedback: None,
            ci_fix: None,
            ci_fixes: 0,
            ci_declined: None,
            delivered: Some("merged".into()),
            delivered_at: Some(now - Duration::days(delivered_ago)),
            merged: None,
            merge_approved: None,
            reverted: None,
            own_worktree: true,
            swept: false,
            hold: None,
            stuck_since: None,
            reviewed_sha: None,
            findings: Default::default(),
        }
    }

    #[test]
    fn the_window_counts_from_the_merge() {
        let now = Utc::now();
        // Shipped 40 days ago, merged 2 days ago: watched.
        assert!(watched(&shipped(2, 40), now));
        // Shipped 40 days ago, merged 31 days ago: not.
        assert!(!watched(&shipped(31, 40), now));
        assert!(watched(&shipped(29, 29), now));
    }

    #[test]
    fn a_fresh_read_throttles_the_base() {
        let now = Utc::now();
        let mut seen = Seen::default();
        assert!(!throttled(&seen, "main", now));
        seen.bases
            .insert("main".into(), now - Duration::seconds(EVERY - 60));
        assert!(throttled(&seen, "main", now));
        assert!(!throttled(&seen, "dev", now));
        seen.bases
            .insert("main".into(), now - Duration::seconds(EVERY + 1));
        assert!(!throttled(&seen, "main", now));
    }

    #[test]
    fn pr_numbers_come_from_the_url() {
        assert_eq!(pr_number("https://github.com/acme/app/pull/131"), Some(131));
        assert_eq!(pr_number("https://example.com/merge_requests/x"), None);
    }
}
