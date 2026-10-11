//! ADR 0032: an older hall never rewrites the rows of an index a newer one
//! owns. Here the newer one is simulated: the index is marked with an item
//! field this build doesn't know, so this build's `hall` is the older one.

mod c_support;
mod common;

use c_support::hall;
use common::Env;
use hallmark::index::{Index, item_fields};

fn row_state(env: &Env, id: &str) -> String {
    let c = rusqlite::Connection::open(env.repo.join(".git/hallmark/index.sqlite")).unwrap();
    let json: String = c
        .query_row("SELECT json FROM items WHERE id = ?1", [id], |r| r.get(0))
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    v["state"].as_str().unwrap().to_string()
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
    let r = env.hall(&env.repo, &["drop", &id, "--reason", "not needed"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    // The journal has it; the row is the newer hall's to rewrite.
    assert_eq!(row_state(&env, &id), "development");
    // An explicit reindex doesn't empty it either.
    let r = env.hall(&env.repo, &["reindex"]);
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("\"rebuilt\":false") || r.stdout.contains("Did not rebuild"));
    assert_eq!(row_state(&env, &id), "development");

    // An hour without the newer hall (a downgrade): this one takes over.
    newer_wrote_it(&env, chrono::Duration::hours(2));
    assert_eq!(env.hall(&env.repo, &["status", "--all"]).code, 0);
    assert_eq!(row_state(&env, &id), "dropped");
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
