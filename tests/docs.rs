//! The reference docs keep up with the CLI: every command `hall help --json`
//! lists is in docs/commands.md, and every key `hall config list` offers is in
//! docs/configuration.md. A new command or setting fails here until it is
//! documented.

mod common;

use common::Env;
use serde_json::Value;

fn doc(name: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// `word` appears in `text` with no identifier character on either side.
fn has_word(text: &str, word: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    text.match_indices(word).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + word.len()..].chars().next();
        !before.is_some_and(ident) && !after.is_some_and(ident)
    })
}

#[test]
fn every_command_is_in_the_command_reference() {
    let env = Env::new();
    let r = env.hall(&env.repo, &["help", "--json"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let text = doc("commands.md");
    let mut missing = Vec::new();
    for c in r.json["commands"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        let found = match name.split_once(' ') {
            None => text.contains(&format!("`hall {name}")),
            // A subcommand ("daemon stop") is named on a line about its
            // parent: "`hall daemon start\|stop`" or "`hall config` … `set`".
            Some((parent, sub)) => {
                let head = format!("`hall {parent}");
                text.lines().any(|l| l.contains(&head) && has_word(l, sub))
            }
        };
        if !found {
            missing.push(format!("hall {name}"));
        }
    }
    assert!(
        missing.is_empty(),
        "docs/commands.md doesn't mention: {missing:?}"
    );
}

#[test]
fn every_setting_is_in_the_configuration_reference() {
    let env = Env::initialized();
    let r = env.hall(&env.repo, &["config", "list"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let text = doc("configuration.md");
    let missing: Vec<&str> = r.json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e: &Value| e["key"].as_str())
        // The docs show keys inside their TOML tables, so look for the leaf.
        .filter(|k| !has_word(&text, k.rsplit('.').next().unwrap()))
        .collect();
    assert!(
        missing.is_empty(),
        "docs/configuration.md doesn't mention: {missing:?}"
    );
}
