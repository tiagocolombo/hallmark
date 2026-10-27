//! The Claude Code plugin, embedded in the binary so `hall setup claude`
//! installs exactly the version that matches it.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::Result;

pub const MARKETPLACE: &str = "hallmark";
pub const PLUGIN: &str = "hallmark";

/// (path inside the plugin, contents)
pub const FILES: &[(&str, &str)] = &[
    (
        ".claude-plugin/plugin.json",
        include_str!("../plugin/.claude-plugin/plugin.json"),
    ),
    (
        "hooks/hooks.json",
        include_str!("../plugin/hooks/hooks.json"),
    ),
    (
        "skills/hallmark/SKILL.md",
        include_str!("../plugin/skills/hallmark/SKILL.md"),
    ),
    (
        "skills/principal-review/SKILL.md",
        include_str!("../plugin/skills/principal-review/SKILL.md"),
    ),
    (
        "skills/hallmark-fix/SKILL.md",
        include_str!("../plugin/skills/hallmark-fix/SKILL.md"),
    ),
    (
        "skills/hallmark-setup/SKILL.md",
        include_str!("../plugin/skills/hallmark-setup/SKILL.md"),
    ),
    (
        "agents/principal-engineer.md",
        include_str!("../plugin/agents/principal-engineer.md"),
    ),
    ("agents/fixer.md", include_str!("../plugin/agents/fixer.md")),
    (
        "agents/verifier.md",
        include_str!("../plugin/agents/verifier.md"),
    ),
    (
        "schemas/verification.json",
        include_str!("../plugin/schemas/verification.json"),
    ),
    (
        "agents/intake.md",
        include_str!("../plugin/agents/intake.md"),
    ),
    (
        "agents/librarian.md",
        include_str!("../plugin/agents/librarian.md"),
    ),
    (
        "schemas/verdict.json",
        include_str!("../plugin/schemas/verdict.json"),
    ),
    (
        "schemas/fix.json",
        include_str!("../plugin/schemas/fix.json"),
    ),
    (
        "schemas/plan.json",
        include_str!("../plugin/schemas/plan.json"),
    ),
    (
        "schemas/learnings.json",
        include_str!("../plugin/schemas/learnings.json"),
    ),
];

/// An embedded plugin file's contents, such as `schemas/verdict.json`.
pub fn file(rel: &str) -> &'static str {
    FILES
        .iter()
        .find(|(p, _)| *p == rel)
        .map(|(_, c)| *c)
        .unwrap_or_else(|| panic!("no embedded plugin file {rel}"))
}

/// The plugin directory for headless runs (`claude -p --plugin-dir`):
/// `HALL_PLUGIN_DIR`, else a copy extracted from this binary under the user
/// data dir, refreshed when the binary changes.
pub fn runtime_dir() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("HALL_PLUGIN_DIR") {
        return Ok(PathBuf::from(p));
    }
    let exe = crate::paths::normalize(&std::env::current_exe()?);
    let root = crate::repo::user_dir().join("runtime-plugin");
    let stamp = format!(
        "{} {} {}",
        env!("CARGO_PKG_VERSION"),
        exe.display(),
        FILES.iter().map(|(_, c)| c.len()).sum::<usize>()
    );
    let marker = root.join(".stamp");
    if fs::read_to_string(&marker).ok().as_deref() != Some(stamp.as_str()) {
        extract(&root, &exe)?;
        fs::write(&marker, &stamp)?;
    }
    Ok(root.join("plugins").join(PLUGIN))
}

/// hooks.json with `hall` replaced by this binary's absolute path, so the
/// hooks work even when Claude Code's PATH lacks `hall`.
pub fn hooks_json(exe: &Path) -> String {
    let src = FILES
        .iter()
        .find(|(p, _)| *p == "hooks/hooks.json")
        .map(|(_, c)| *c)
        .unwrap_or("");
    let exe = exe.to_string_lossy();
    let quoted = if exe.contains(' ') {
        format!("\"{exe}\"")
    } else {
        exe.to_string()
    };
    // The value is inside a JSON string, so escape it as JSON.
    let escaped = serde_json::to_string(&quoted).unwrap_or_default();
    let escaped = &escaped[1..escaped.len() - 1];
    src.replace("\"hall hook ", &format!("\"{escaped} hook "))
}

/// Writes a local marketplace: `<dir>/.claude-plugin/marketplace.json` and
/// the plugin under `<dir>/plugins/hallmark`. Returns the files written.
pub fn extract(dir: &Path, exe: &Path) -> Result<Vec<PathBuf>> {
    let plugin_dir = dir.join("plugins").join(PLUGIN);
    let mut written = Vec::new();
    for (rel, contents) in FILES {
        let p = plugin_dir.join(rel);
        fs::create_dir_all(p.parent().expect("has parent"))?;
        let body = if *rel == "hooks/hooks.json" {
            hooks_json(exe)
        } else {
            contents.to_string()
        };
        fs::write(&p, body)?;
        written.push(p);
    }
    let version = env!("CARGO_PKG_VERSION");
    let market = serde_json::json!({
        "name": MARKETPLACE,
        "owner": { "name": "Hallmark" },
        "metadata": { "description": "The Hallmark plugin, installed by `hall setup claude`.", "version": version },
        "plugins": [{
            "name": PLUGIN,
            "source": "./plugins/hallmark",
            "description": "Hallmark: agentic development, automated review, a full record.",
            "version": version
        }]
    });
    let mp = dir.join(".claude-plugin").join("marketplace.json");
    fs::create_dir_all(mp.parent().expect("has parent"))?;
    fs::write(&mp, serde_json::to_string_pretty(&market)?)?;
    written.push(mp);
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_call_the_binary() {
        let v: serde_json::Value =
            serde_json::from_str(&hooks_json(Path::new("/opt/hall bin/hall"))).unwrap();
        let cmd = v["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(cmd, "\"/opt/hall bin/hall\" hook pre-tool-use");
        let v: serde_json::Value =
            serde_json::from_str(&hooks_json(Path::new(r"C:\Program Files\hall\hall.exe")))
                .unwrap();
        let cmd = v["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(cmd, r#""C:\Program Files\hall\hall.exe" hook stop"#);
    }

    #[test]
    fn every_plugin_file_is_embedded() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/plugin");
        let mut on_disk = Vec::new();
        fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
            for e in fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, root, out);
                } else {
                    let rel = p
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.push(rel);
                }
            }
        }
        walk(Path::new(root), Path::new(root), &mut on_disk);
        for f in on_disk {
            assert!(
                FILES.iter().any(|(p, _)| *p == f),
                "{f} is not in plugin::FILES"
            );
        }
    }

    #[test]
    fn agents_have_frontmatter() {
        for (p, c) in FILES {
            if p.starts_with("agents/") || p.ends_with("SKILL.md") {
                assert!(c.starts_with("---\nname: "), "{p}");
            }
            if p.ends_with(".json") {
                serde_json::from_str::<serde_json::Value>(c).unwrap_or_else(|e| panic!("{p}: {e}"));
            }
        }
    }

    #[test]
    fn every_hook_event_is_wired() {
        let v: serde_json::Value = serde_json::from_str(&hooks_json(Path::new("hall"))).unwrap();
        for e in [
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "Stop",
            "Notification",
        ] {
            let cmd = v["hooks"][e][0]["hooks"][0]["command"]
                .as_str()
                .unwrap_or_default();
            assert!(cmd.starts_with("hall hook "), "{e}: {cmd}");
        }
    }
}
