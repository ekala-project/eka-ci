//! Map `meta.position` values onto repo-relative paths from `git diff`.

use std::path::{Path, PathBuf};

/// Strip trailing `:<digits>` segments (`file:line` or `file:line:col`).
fn strip_line_suffix(position: &str) -> &str {
    let mut path = position;
    for _ in 0..2 {
        match path.rsplit_once(':') {
            Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => {
                path = head;
            },
            _ => break,
        }
    }
    path
}

/// Repo-relative path of the file a `meta.position` (or provenance `pos`)
/// value points at, for matching against `git diff --name-only`.
///
/// Handles positions inside one of `repo_roots` (the evaluated worktree,
/// which is where file-based evaluations point), inside a store copy of the
/// source (`/nix/store/<hash>-source/...`), and already-relative paths.
/// Returns `None` for empty values and absolute paths outside those roots.
pub fn position_file(position: &str, repo_roots: &[PathBuf]) -> Option<String> {
    let path = strip_line_suffix(position);
    if path.is_empty() {
        return None;
    }

    for root in repo_roots {
        if let Ok(rest) = Path::new(path).strip_prefix(root) {
            return rest.to_str().map(str::to_string);
        }
    }

    if let Some(after_store) = path.strip_prefix("/nix/store/") {
        return after_store
            .split_once('/')
            .map(|(_, rest)| rest.to_string());
    }

    if path.starts_with('/') {
        return None;
    }
    Some(path.to_string())
}

/// The roots to strip from positions: the worktree as given and, when it
/// differs, its canonical form (positions may use either spelling).
pub fn repo_roots(worktree: &Path) -> Vec<PathBuf> {
    let mut roots = vec![worktree.to_path_buf()];
    if let Ok(canonical) = worktree.canonicalize() {
        if canonical != worktree {
            roots.push(canonical);
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots() -> Vec<PathBuf> {
        vec![PathBuf::from(
            "/var/lib/ekaci/github.com/o/r/worktrees/abc123",
        )]
    }

    #[test]
    fn strips_worktree_root() {
        let pos = "/var/lib/ekaci/github.com/o/r/worktrees/abc123/pkgs/openssl/default.nix:42";
        assert_eq!(
            position_file(pos, &roots()).as_deref(),
            Some("pkgs/openssl/default.nix")
        );
    }

    #[test]
    fn strips_store_prefix_and_column() {
        let pos = "/nix/store/0123abcd-source/pkgs/git/default.nix:10:3";
        assert_eq!(
            position_file(pos, &roots()).as_deref(),
            Some("pkgs/git/default.nix")
        );
    }

    #[test]
    fn keeps_relative_paths() {
        assert_eq!(
            position_file("pkgs/a/default.nix:1", &roots()).as_deref(),
            Some("pkgs/a/default.nix")
        );
    }

    #[test]
    fn keeps_paths_without_line_numbers() {
        assert_eq!(
            position_file("pkgs/foo/default.nix", &roots()).as_deref(),
            Some("pkgs/foo/default.nix")
        );
    }

    #[test]
    fn rejects_bare_store_paths() {
        assert_eq!(position_file("/nix/store/abc123-source", &roots()), None);
    }

    #[test]
    fn rejects_foreign_absolute_and_empty_paths() {
        assert_eq!(position_file("/etc/elsewhere/a.nix:1", &roots()), None);
        assert_eq!(position_file(":1", &roots()), None);
        assert_eq!(position_file("", &roots()), None);
    }

    #[test]
    fn does_not_match_sibling_worktree_prefix() {
        let pos = "/var/lib/ekaci/github.com/o/r/worktrees/abc1234/pkgs/a.nix:1";
        assert_eq!(position_file(pos, &roots()), None);
    }
}
