//! Filesystem path safety helpers used before invoking external tools
//! (`nix eval`, `nix-eval-jobs`, etc.) on paths influenced by untrusted
//! sources (e.g. PR branch content or the `.ekaci/config.json` file read
//! from a checked-out worktree).
//!
//! The goal is straightforward: after canonicalization (which resolves
//! `..` components and follows symlinks), the path must be a descendant
//! of a designated workspace root. Anything that resolves outside that
//! root is rejected with an error, preventing a malicious repository
//! from coercing the server into evaluating or building arbitrary files
//! elsewhere on disk.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Canonicalize `path` and assert it is a descendant of (canonical form
/// of) `root`. Returns the canonicalized path on success.
///
/// This resolves `..` components and follows symlinks on both sides.
/// If the final resolved path is not lexically under `root`, returns an
/// error. Non-existing paths also return an error because
/// `fs::canonicalize` requires the target to exist — callers that must
/// handle missing paths should check existence first.
pub fn canonical_within(path: &Path, root: &Path) -> Result<PathBuf> {
    let canonical = std::fs::canonicalize(path)
        .with_context(|| format!("failed to canonicalize path {}", path.display()))?;
    let canonical_root = std::fs::canonicalize(root)
        .with_context(|| format!("failed to canonicalize workspace root {}", root.display()))?;
    if !canonical.starts_with(&canonical_root) {
        bail!(
            "path {} is outside workspace root {}",
            canonical.display(),
            canonical_root.display()
        );
    }
    Ok(canonical)
}

/// Reject path components that would allow traversal above the caller's
/// intended base (i.e. `..`). This is a lexical pre-check that does not
/// touch the filesystem; it is intended as a cheap first gate before a
/// subsequent [`canonical_within`] call.
pub fn reject_parent_components(path: &Path) -> Result<()> {
    for comp in path.components() {
        if matches!(comp, Component::ParentDir) {
            bail!(
                "path {} contains a parent-directory ('..') component",
                path.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn canonical_within_accepts_descendant() {
        let root = tempdir().unwrap();
        let sub = root.path().join("sub");
        fs::create_dir(&sub).unwrap();

        let resolved = canonical_within(&sub, root.path()).unwrap();
        assert!(resolved.starts_with(fs::canonicalize(root.path()).unwrap()));
    }

    #[test]
    fn canonical_within_rejects_sibling_directory() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();

        let result = canonical_within(outside.path(), root.path());
        assert!(
            result.is_err(),
            "paths outside the workspace root must be rejected"
        );
    }

    #[test]
    fn canonical_within_rejects_symlink_escape() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let target_file = outside.path().join("secret");
        fs::write(&target_file, b"secret").unwrap();

        // Place a symlink inside `root` that points to a file outside it.
        let link = root.path().join("escape");
        std::os::unix::fs::symlink(&target_file, &link).unwrap();

        let result = canonical_within(&link, root.path());
        assert!(
            result.is_err(),
            "symlinks pointing outside the workspace root must be rejected"
        );
    }

    #[test]
    fn canonical_within_errors_on_missing_path() {
        let root = tempdir().unwrap();
        let missing = root.path().join("does-not-exist");

        let result = canonical_within(&missing, root.path());
        assert!(
            result.is_err(),
            "non-existent paths must be rejected at canonicalization time"
        );
    }

    #[test]
    fn reject_parent_components_flags_traversal() {
        assert!(reject_parent_components(Path::new("../etc/passwd")).is_err());
        assert!(reject_parent_components(Path::new("a/../b")).is_err());
        assert!(reject_parent_components(Path::new("a/./b")).is_ok());
        assert!(reject_parent_components(Path::new("a/b/c")).is_ok());
    }
}
