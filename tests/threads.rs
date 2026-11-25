//! `hall threads` replies (ADR 0070): the JSON shapes and the text renders
//! are pinned exactly, built from fixed threads so nothing varies.

use chrono::{DateTime, TimeZone, Utc};
use hallmark::commands::threads::{Tally, ThreadOut, ThreadRow, ThreadsList};
use hallmark::item::State;
use hallmark::output::Response;
use hallmark::threads::{Kind, Post, PostKind, Thread, ThreadStatus};
use serde_json::json;

fn at(min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 3, 1, 10, min, 0).unwrap()
}

fn post(min: u32, author: &str, kind: PostKind, body: &str) -> Post {
    Post {
        t: at(min),
        author: author.into(),
        kind,
        body: body.into(),
        sha: None,
        round: None,
        accepted: false,
    }
}

fn finding() -> Thread {
    let mut q = post(
        0,
        "reviewer (opus)",
        PostKind::Question,
        "When x, then y.\n\nSuggested fix: Check x.",
    );
    q.round = Some(1);
    q.sha = Some("aaa1111222".into());
    let mut c = post(
        1,
        "verifier (sonnet)",
        PostKind::Comment,
        "Checked it and couldn't show it was wrong: it stands.",
    );
    c.round = Some(1);
    let mut a = post(5, "fixer", PostKind::Answer, "Fixed in bbb2222.");
    a.sha = Some("bbb2222333".into());
    a.accepted = true;
    let mut r = post(9, "reviewer (opus)", PostKind::Ruling, "Fixed at bbb2222.");
    r.round = Some(2);
    r.sha = Some("bbb2222333".into());
    Thread {
        id: "H-0001/F1".into(),
        item: "H-0001".parse().unwrap(),
        item_title: "Retry webhooks".into(),
        item_state: State::Review,
        kind: Kind::Finding,
        status: ThreadStatus::Resolved,
        title: "F1 breaks".into(),
        asked_by: "reviewer (opus)".into(),
        asked_at: at(0),
        last_at: at(9),
        tags: vec!["correctness".into(), "major".into(), "src/a.rs:12".into()],
        replies: 2,
        posts: vec![q, c, a, r],
    }
}

fn question() -> Thread {
    Thread {
        id: "H-0001/q-q1".into(),
        item: "H-0001".parse().unwrap(),
        item_title: "Retry webhooks".into(),
        item_state: State::Review,
        kind: Kind::Question,
        status: ThreadStatus::Open,
        title: "Six attempts or ten?".into(),
        asked_by: "agent".into(),
        asked_at: at(20),
        last_at: at(20),
        tags: vec!["question".into()],
        replies: 0,
        posts: vec![post(
            20,
            "agent",
            PostKind::Question,
            "Six attempts or ten?\n\nOptions: 6 · 10",
        )],
    }
}

fn list(total: usize) -> ThreadsList {
    let threads = [question(), finding()];
    ThreadsList {
        item: Some("H-0001".parse().unwrap()),
        total,
        tally: Tally {
            open: 1,
            resolved: 1,
            ..Tally::default()
        },
        threads: threads.iter().map(ThreadRow::from).collect(),
        unreadable: vec![],
    }
}

#[test]
fn the_thread_reply_is_pinned() {
    let r = Response::new(
        ThreadOut { thread: finding() },
        vec!["hall threads H-0001".into()],
    );
    assert_eq!(
        r.value,
        json!({
            "id": "H-0001/F1",
            "item": "H-0001",
            "item_title": "Retry webhooks",
            "item_state": "review",
            "kind": "finding",
            "status": "resolved",
            "title": "F1 breaks",
            "asked_by": "reviewer (opus)",
            "asked_at": "2026-03-01T10:00:00Z",
            "last_at": "2026-03-01T10:09:00Z",
            "tags": ["correctness", "major", "src/a.rs:12"],
            "replies": 2,
            "posts": [
                {"t": "2026-03-01T10:00:00Z", "author": "reviewer (opus)", "kind": "question",
                 "body": "When x, then y.\n\nSuggested fix: Check x.",
                 "sha": "aaa1111222", "round": 1},
                {"t": "2026-03-01T10:01:00Z", "author": "verifier (sonnet)", "kind": "comment",
                 "body": "Checked it and couldn't show it was wrong: it stands.", "round": 1},
                {"t": "2026-03-01T10:05:00Z", "author": "fixer", "kind": "answer",
                 "body": "Fixed in bbb2222.", "sha": "bbb2222333", "accepted": true},
                {"t": "2026-03-01T10:09:00Z", "author": "reviewer (opus)", "kind": "ruling",
                 "body": "Fixed at bbb2222.", "sha": "bbb2222333", "round": 2}
            ],
            "next": ["hall threads H-0001"]
        })
    );
    let expected = "\
H-0001/F1 · resolved · finding on H-0001 \"Retry webhooks\"
F1 breaks
tags: correctness · major · src/a.rs:12

? question · reviewer (opus) · round 1 · aaa1111 · 2026-03-01 10:00
  When x, then y.

  Suggested fix: Check x.

· comment · verifier (sonnet) · round 1 · 2026-03-01 10:01
  Checked it and couldn't show it was wrong: it stands.

✓ answer · fixer · bbb2222 · 2026-03-01 10:05
  Fixed in bbb2222.

§ ruling · reviewer (opus) · round 2 · bbb2222 · 2026-03-01 10:09
  Fixed at bbb2222.

next:
  hall threads H-0001
";
    assert_eq!(r.text, expected);
}

const LIST_TEXT: &str = "\
2 threads on H-0001 · 1 open · 0 answered · 1 resolved · 0 withdrawn · 0 refuted · 0 closed
  open      H-0001/q-q1    Six attempts or ten? [question]
                           asked by agent · 0 replies · last 2026-03-01 10:20
  resolved  H-0001/F1      F1 breaks [correctness · major · src/a.rs:12]
                           asked by reviewer (opus) · 2 replies · last 2026-03-01 10:09
";

#[test]
fn the_list_reply_is_pinned() {
    let r = Response::new(list(2), vec!["hall threads H-0001/q-q1".into()]);
    let v = &r.value;
    assert_eq!(v["item"], "H-0001");
    assert_eq!(v["total"], 2);
    assert_eq!(
        v["tally"],
        json!({"open": 1, "answered": 0, "resolved": 1, "withdrawn": 0, "refuted": 0, "closed": 0})
    );
    assert_eq!(v["next"], json!(["hall threads H-0001/q-q1"]));
    assert!(v.get("unreadable").is_none());
    assert_eq!(
        v["threads"][1],
        json!({
            "id": "H-0001/F1",
            "item": "H-0001",
            "item_title": "Retry webhooks",
            "item_state": "review",
            "kind": "finding",
            "status": "resolved",
            "title": "F1 breaks",
            "asked_by": "reviewer (opus)",
            "asked_at": "2026-03-01T10:00:00Z",
            "last_at": "2026-03-01T10:09:00Z",
            "tags": ["correctness", "major", "src/a.rs:12"],
            "replies": 2
        })
    );
    assert_eq!(
        r.text,
        format!("{LIST_TEXT}\nnext:\n  hall threads H-0001/q-q1\n")
    );
}

#[test]
fn a_list_cut_short_says_how_many_more() {
    let r = Response::new(list(5), vec![]);
    assert_eq!(
        r.text,
        LIST_TEXT.replacen("2 threads", "5 threads", 1) + "… 3 more; --limit shows more\n"
    );
}

#[test]
fn an_empty_list_says_how_threads_start() {
    let r = Response::new(
        ThreadsList {
            item: None,
            total: 0,
            tally: Tally::default(),
            threads: vec![],
            unreadable: vec!["H-0007".into()],
        },
        vec![],
    );
    assert_eq!(
        r.text,
        "\
0 threads · 0 open · 0 answered · 0 resolved · 0 withdrawn · 0 refuted · 0 closed
No threads yet: they start when an item is planned, reviewed or asks you something.
H-0007: its journal could not be read
"
    );
}
