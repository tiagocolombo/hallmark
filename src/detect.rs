//! `hall init` proposes gates by looking at the repository.

use std::path::Path;

/// The check commands this repo appears to use, in a sensible order.
pub fn gates(root: &Path) -> Vec<String> {
    let mut g = Vec::new();
    node(root, &mut g);
    if root.join("Cargo.toml").is_file() {
        g.push("cargo test".into());
        g.push("cargo clippy --all-targets -- -D warnings".into());
    }
    if root.join("go.mod").is_file() {
        g.push("go vet ./...".into());
        g.push("go test ./...".into());
    }
    if uses_pytest(root) {
        g.push("pytest".into());
    }
    if g.is_empty() {
        make_like(root, &mut g);
    }
    g
}

fn node(root: &Path, g: &mut Vec<String>) {
    let Ok(text) = std::fs::read_to_string(root.join("package.json")) else {
        return;
    };
    let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    let Some(scripts) = pkg.get("scripts").and_then(|s| s.as_object()) else {
        return;
    };
    let runner = if root.join("pnpm-lock.yaml").is_file() {
        "pnpm"
    } else if root.join("yarn.lock").is_file() {
        "yarn"
    } else if root.join("bun.lockb").is_file() || root.join("bun.lock").is_file() {
        "bun run"
    } else {
        "npm run"
    };
    for s in ["lint", "typecheck", "check", "test", "build"] {
        if let Some(cmd) = scripts.get(s).and_then(|v| v.as_str()) {
            // npm's placeholder test script always fails.
            if s == "test" && cmd.contains("no test specified") {
                continue;
            }
            g.push(format!("{runner} {s}"));
        }
    }
}

fn uses_pytest(root: &Path) -> bool {
    if root.join("pytest.ini").is_file() || root.join("conftest.py").is_file() {
        return true;
    }
    ["pyproject.toml", "setup.cfg", "tox.ini"].iter().any(|f| {
        std::fs::read_to_string(root.join(f))
            .map(|t| t.contains("pytest"))
            .unwrap_or(false)
    })
}

fn make_like(root: &Path, g: &mut Vec<String>) {
    let targets = ["lint", "check", "test"];
    for (file, runner) in [
        ("Makefile", "make"),
        ("justfile", "just"),
        ("Justfile", "just"),
    ] {
        let Ok(text) = std::fs::read_to_string(root.join(file)) else {
            continue;
        };
        for t in targets {
            let defined = text.lines().any(|l| {
                let l = l.trim_end();
                l.starts_with(&format!("{t}:")) || l.starts_with(&format!("{t} "))
            });
            if defined {
                g.push(format!("{runner} {t}"));
            }
        }
        if !g.is_empty() {
            return;
        }
    }
}

/// Tools that provide a repo's toolchain in a shell of their own: (name,
/// the prefix that runs a command inside it). Gates run through `sh -c`
/// from the daemon, outside any shell you have entered, so a repo whose
/// tools come from one of these needs its gates wrapped.
pub const WRAPPERS: &[(&str, &str)] = &[
    ("nix", "nix develop --command"),
    ("devbox", "devbox run --"),
    ("mise", "mise exec --"),
    ("direnv", "direnv exec ."),
];

/// The wrapper this repo appears to use, by the files it has.
pub fn env_wrapper(root: &Path) -> Option<(&'static str, &'static str)> {
    let has = |f: &str| root.join(f).is_file();
    let name = if has("devbox.json") {
        "devbox"
    } else if has("flake.nix") {
        "nix"
    } else if has("mise.toml") || has(".mise.toml") || has(".tool-versions") {
        "mise"
    } else if has(".envrc") {
        "direnv"
    } else {
        return None;
    };
    WRAPPERS.iter().copied().find(|(n, _)| *n == name)
}

/// The repo-rules files that exist.
pub fn rules(root: &Path) -> Vec<String> {
    ["CLAUDE.md", "AGENTS.md"]
        .iter()
        .filter(|f| root.join(f).is_file())
        .map(|f| f.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn detects_common_stacks() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        assert!(gates(r).is_empty());

        fs::write(
            r.join("package.json"),
            r#"{"scripts":{"test":"vitest","lint":"eslint .","dev":"vite"}}"#,
        )
        .unwrap();
        fs::write(r.join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(gates(r), vec!["pnpm lint", "pnpm test"]);

        fs::write(r.join("Cargo.toml"), "[package]").unwrap();
        assert!(gates(r).contains(&"cargo test".to_string()));
    }

    #[test]
    fn falls_back_to_make_and_just() {
        let d = tempfile::tempdir().unwrap();
        fs::write(
            d.path().join("justfile"),
            "test:\n  cargo test\nlint: fmt\n",
        )
        .unwrap();
        assert_eq!(gates(d.path()), vec!["just lint", "just test"]);
    }

    #[test]
    fn finds_the_toolchain_shell() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(env_wrapper(d.path()), None);
        fs::write(d.path().join(".envrc"), "use flake\n").unwrap();
        assert_eq!(env_wrapper(d.path()).map(|w| w.0), Some("direnv"));
        fs::write(d.path().join("flake.nix"), "{}").unwrap();
        assert_eq!(
            env_wrapper(d.path()),
            Some(("nix", "nix develop --command"))
        );
    }

    #[test]
    fn skips_npm_placeholder_test() {
        let d = tempfile::tempdir().unwrap();
        fs::write(
            d.path().join("package.json"),
            r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#,
        )
        .unwrap();
        assert!(gates(d.path()).is_empty());
    }
}
