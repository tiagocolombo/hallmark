//! ADR 0032: an older hall never rewrites the rows of an index a newer one
//! owns. Here the newer one is simulated: the index is marked with an item
//! field this build doesn't know, so this build's `hall` is the older one.

mod c_support;
mod common;

use c_support::hall;
use common::Env;
use hallmark::index::{Index, item_fields};

fn conn(env: &Env) -> rusqlite::Connection {
    rusqlite::Connection::open(env.repo.join(".git/hallmark/index.sqlite")).unwrap()
}

fn row_json(env: &Env, id: &str) -> serde_json::Value {
    let json: String = conn(env)
        .query_row("SELECT json FROM items WHERE id = ?1", [id], |r| r.get(0))
        .unwrap();
    serde_json::from_str(&json).unwrap()
}

fn row_state(env: &Env, id: &str) -> String {
    row_json(env, id)["state"].as_str().unwrap().to_string()
}

fn add_unknown_field(env: &Env, id: &str) {
    let mut v = row_json(env, id);
    v["from_a_newer_hall"] = "kept".into();
    conn(env)
        .execute(
            "UPDATE items SET json = ?1 WHERE id = ?2",
            rusqlite::params![v.to_string(), id],
        )
        .unwrap();
}

fn meta(env: &Env, key: &str) -> Option<String> {
    conn(env)
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .ok()
}

fn newer_wrote_it(env: &Env, ago: chrono::Duration) {
    let ix = Index::open(&hall(env)).unwrap();
    let mut fields = item_fields();
    fields.push("from_a_newer_hall".into());
    ix.mark_writer(&fields, chrono::Utc::now() - ago).unwrap();
}

#[test]
fn an_older_hall_leaves_a_newer_ones_rows_alone() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Retry webhooks");
    assert_eq!(env.hall(&env.repo, &["status", "--all"]).code, 0);
    assert_eq!(row_state(&env, &id), "development");

    newer_wrote_it(&env, chrono::Duration::minutes(1));
    add_unknown_field(&env, &id);
    let r = env.hall(&env.repo, &["drop", &id, "--reason", "not needed"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    // An explicit reindex doesn't empty the index.
    let r = env.hall(&env.repo, &["reindex"]);
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("\"rebuilt\":false") || r.stdout.contains("Did not rebuild"));
    // A syncing command updates the row's state but keeps the field this
    // build doesn't know, and marks the rows stale for the owner.
    assert_eq!(env.hall(&env.repo, &["status", "--all"]).code, 0);
    assert_eq!(row_state(&env, &id), "dropped");
    assert_eq!(row_json(&env, &id)["from_a_newer_hall"], "kept");
    assert!(meta(&env, "items_stale").is_some());

    // An hour without the newer hall (a downgrade): this one takes over.
    newer_wrote_it(&env, chrono::Duration::hours(2));
    assert_eq!(env.hall(&env.repo, &["status", "--all"]).code, 0);
    assert_eq!(row_state(&env, &id), "dropped");
    assert!(meta(&env, "items_stale").is_none());
    let (fields, _) = Index::open(&hall(&env)).unwrap().writer().unwrap();
    assert_eq!(fields, item_fields());
}

#[test]
fn a_newer_hall_refolds_rows_an_older_one_wrote() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Retry webhooks");
    assert_eq!(env.hall(&env.repo, &["status", "--all"]).code, 0);

    // An older build synced last: it marked the index with fewer fields.
    let ix = Index::open(&hall(&env)).unwrap();
    let mut fields = item_fields();
    let dropped = fields.pop().unwrap();
    ix.mark_writer(&fields, chrono::Utc::now() + chrono::Duration::hours(1))
        .unwrap();
    drop(ix);
    assert_eq!(env.hall(&env.repo, &["status", "--all"]).code, 0);
    let (fields, _) = Index::open(&hall(&env)).unwrap().writer().unwrap();
    assert!(fields.contains(&dropped));
    assert_eq!(row_state(&env, &id), "development");
}

#[test]
fn reindex_force_takes_the_index_over() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Retry webhooks");
    assert_eq!(env.hall(&env.repo, &["status", "--all"]).code, 0);
    newer_wrote_it(&env, chrono::Duration::minutes(1));
    assert_eq!(env.hall(&env.repo, &["reindex", "--force"]).code, 0);
    let (fields, _) = Index::open(&hall(&env)).unwrap().writer().unwrap();
    assert_eq!(fields, item_fields());
    assert_eq!(row_state(&env, &id), "development");
}

// ADR 0035: a row is trusted only if the fold mark of the build that wrote
// it still matches. A hall from before fold marks (a terminal app left
// running) folds without the events it can't parse and advances the offset
// anyway; H-0038 stayed in Delivery after its PR merged.

fn journal_path(env: &Env, id: &str) -> std::path::PathBuf {
    env.repo
        .join(".git/hallmark/items")
        .join(id)
        .join("events.jsonl")
}

fn append_line(env: &Env, id: &str, line: serde_json::Value) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(journal_path(env, id))
        .unwrap();
    writeln!(f, "{line}").unwrap();
}

fn dropped(id: &str) -> serde_json::Value {
    serde_json::json!({
        "v": 1, "t": chrono::Utc::now().to_rfc3339(), "actor": "you", "item": id,
        "event": "dropped", "reason": "not needed", "worktree_removed": false
    })
}

fn fold_complete(env: &Env, id: &str) -> bool {
    conn(env)
        .query_row("SELECT complete FROM folds WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
}

#[test]
fn a_row_an_older_hall_left_behind_its_offset_is_refolded() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Retry webhooks");
    assert_eq!(env.hall(&env.repo, &["status", "--all"]).code, 0);
    assert_eq!(row_state(&env, &id), "development");

    // The event lands, and an older hall that can't parse it moves the
    // offset past it without changing the row or the fold mark.
    append_line(&env, &id, dropped(&id));
    let len = std::fs::metadata(journal_path(&env, &id)).unwrap().len() as i64;
    // The index keys files by the path hall resolved, which needn't spell the
    // temp dir the way the test does (Windows short names, separators).
    let n = conn(&env)
        .execute(
            "UPDATE files SET offset = ?1 WHERE path LIKE ?2",
            rusqlite::params![len, format!("%{id}%events.jsonl")],
        )
        .unwrap();
    assert_eq!(n, 1);

    assert_eq!(env.hall(&env.repo, &["status", "--all"]).code, 0);
    assert_eq!(row_state(&env, &id), "dropped");
}

#[test]
fn a_row_an_older_hall_rewrote_is_refolded() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Retry webhooks");
    let r = env.hall(&env.repo, &["drop", &id, "--reason", "not needed"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(env.hall(&env.repo, &["status", "--all"]).code, 0);
    assert_eq!(row_state(&env, &id), "dropped");

    // An older hall re-folded every journal (a reindex), skipping the line it
    // can't read: the offsets match, the row has one event fewer.
    let mut v = row_json(&env, &id);
    v["state"] = "development".into();
    v["events"] = (v["events"].as_i64().unwrap() - 1).into();
    conn(&env)
        .execute(
            "UPDATE items SET json = ?1 WHERE id = ?2",
            rusqlite::params![v.to_string(), id],
        )
        .unwrap();

    assert_eq!(env.hall(&env.repo, &["status", "--all"]).code, 0);
    assert_eq!(row_state(&env, &id), "dropped");
}

#[test]
fn a_fold_that_skipped_a_line_is_redone_by_a_build_that_reads_it() {
    let env = Env::initialized();
    let (id, _) = env.new_item("Retry webhooks");

    // This build can't read an event from the future: it still folds the
    // rest, and marks the fold partial.
    append_line(
        &env,
        &id,
        serde_json::json!({
            "v": 1, "t": chrono::Utc::now().to_rfc3339(), "actor": "hall", "item": id,
            "event": "from_a_newer_hall"
        }),
    );
    append_line(&env, &id, dropped(&id));
    assert_eq!(env.hall(&env.repo, &["status", "--all"]).code, 0);
    assert_eq!(row_state(&env, &id), "dropped");
    assert!(!fold_complete(&env, &id));
    // Folding again can't do better, so the row is left alone.
    assert_eq!(env.hall(&env.repo, &["status", "--all"]).code, 0);
    assert_eq!(row_state(&env, &id), "dropped");

    // A build that reads every line redoes a partial fold.
    let (other, _) = env.new_item("Rate limits");
    let r = env.hall(&env.repo, &["drop", &other, "--reason", "not needed"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(env.hall(&env.repo, &["status", "--all"]).code, 0);
    let mut v = row_json(&env, &other);
    v["state"] = "development".into();
    let c = conn(&env);
    c.execute(
        "UPDATE items SET json = ?1 WHERE id = ?2",
        rusqlite::params![v.to_string(), other],
    )
    .unwrap();
    c.execute("UPDATE folds SET complete = 0 WHERE id = ?1", [&other])
        .unwrap();
    assert_eq!(env.hall(&env.repo, &["status", "--all"]).code, 0);
    assert_eq!(row_state(&env, &other), "dropped");
    assert!(fold_complete(&env, &other));
}
