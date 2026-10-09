//! Path helpers that behave the same on macOS (/private/tmp), Linux and
//! Windows (no `\\?\` prefixes).

use std::path::{Component, Path, PathBuf};

/// Canonicalizes as much of `p` as exists, then appends the rest. Works for
/// files that don't exist yet, such as the target of a Write.
pub fn normalize(p: &Path) -> PathBuf {
    let p = lexical(p);
    if let Ok(c) = dunce::canonicalize(&p) {
        return c;
    }
    let mut existing = p.as_path();
    let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
    while let Some(parent) = existing.parent() {
        if let Some(name) = existing.file_name() {
            rest.push(name);
        }
        existing = parent;
        if let Ok(c) = dunce::canonicalize(existing) {
            let mut out = c;
            for r in rest.iter().rev() {
                out.push(r);
            }
            return out;
        }
    }
    p
}

/// Resolves `.` and `..` without touching the disk.
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Joins a possibly relative path onto `base`, then normalizes.
pub fn resolve(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        normalize(p)
    } else {
        normalize(&base.join(p))
    }
}

/// True when `p` is `dir` or inside it. Both should be normalized.
pub fn is_within(p: &Path, dir: &Path) -> bool {
    p.starts_with(dir)
}

/// Paths as strings in JSON, with forward slashes on every platform.
pub fn display(p: &Path) -> String {
    let s = p.to_string_lossy().to_string();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_missing_tails() {
        let dir = tempfile::tempdir().unwrap();
        let real = dunce::canonicalize(dir.path()).unwrap();
        let p = dir.path().join("a/../b/new.txt");
        assert_eq!(normalize(&p), real.join("b").join("new.txt"));
        assert!(is_within(&normalize(&p), &real));
    }
}
