//! Rules the sources keep, checked by reading them.

use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// Every child process starts in `process::command` (ADR 0054): on
/// Windows, a `Command::new` elsewhere opens a console window for each
/// command the daemon runs.
#[test]
fn every_child_process_starts_in_process_command() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut offenders = Vec::new();
    for p in files {
        if p.ends_with("process.rs") && p.parent() == Some(src.as_path()) {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap();
        for (i, line) in text.lines().enumerate() {
            if line.contains("Command::new(") {
                let rel = p.strip_prefix(&src).unwrap().display();
                offenders.push(format!("src/{rel}:{}: {}", i + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "use crate::process::command instead of Command::new:\n{}",
        offenders.join("\n")
    );
}
