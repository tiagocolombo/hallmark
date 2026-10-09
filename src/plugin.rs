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
];

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
