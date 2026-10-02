use std::path::{Component, Path, PathBuf};

use tracing::warn;

#[derive(Debug, Clone)]
pub struct Checkout {
    root: PathBuf,
    git: Vec<PathBuf>,
    unmounted_git: Option<Option<PathBuf>>,
}

impl Checkout {
    // The checkout is writable from the sandbox, so `.git` is read once, before anything
    // sandboxed runs; a `.git` written later must not decide what gets mounted.
    pub fn resolve(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let git = git_paths(&root);
        let dot_git = root.join(".git");
        let unmounted_git = match dot_git.symlink_metadata() {
            Err(_) => Some(None),
            Ok(m) if m.is_symlink() => Some(std::fs::read_link(&dot_git).ok()),
            Ok(_) => None,
        };
        Self {
            root,
            git,
            unmounted_git,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn git_paths(&self) -> &[PathBuf] {
        &self.git
    }

    // A `.git` planted in the sandbox would run its hooks or fsmonitor in the user's next `git`
    // on the host. Mounting a placeholder instead would turn every flake into a git flake.
    pub fn restore_git(&self) {
        let Some(original) = &self.unmounted_git else {
            return;
        };
        let dot_git = self.root.join(".git");
        let current = dot_git.symlink_metadata().ok();
        let link = current
            .as_ref()
            .filter(|m| m.is_symlink())
            .and_then(|_| std::fs::read_link(&dot_git).ok());
        if current.is_none() && original.is_none() || link.is_some() && link == *original {
            return;
        }
        warn!(
            "restoring {} changed by a sandboxed command",
            dot_git.display()
        );
        let removed = match current {
            Some(m) if m.is_dir() => std::fs::remove_dir_all(&dot_git),
            Some(_) => std::fs::remove_file(&dot_git),
            None => Ok(()),
        };
        let restored = removed.and_then(|()| match original {
            Some(target) => std::os::unix::fs::symlink(target, &dot_git),
            None => Ok(()),
        });
        if let Err(e) = restored {
            warn!("failed to restore {}: {e}", dot_git.display());
        }
    }
}

fn git_paths(root: &Path) -> Vec<PathBuf> {
    let dot_git = root.join(".git");
    match dot_git.symlink_metadata() {
        Ok(m) if m.is_file() => {
            let mut out = linked_git_dirs(root, &dot_git);
            out.insert(0, dot_git);
            out
        },
        Ok(m) if !m.is_symlink() => vec![dot_git],
        _ => Vec::new(),
    }
}

// A `.git` file left by an earlier sandboxed run can name any path; only a linked worktree whose
// git dir points back at this `.git` mounts anything outside the checkout. The dirs are bound at
// the path the `.git` file names, since its canonical form may cross symlinks the sandbox lacks.
fn linked_git_dirs(root: &Path, dot_git: &Path) -> Vec<PathBuf> {
    let (Ok(given), Ok(real)) = (std::path::absolute(root), root.canonicalize()) else {
        return Vec::new();
    };
    let given = lexical(&given);
    let inside = |p: &Path| p.starts_with(&given) || p.starts_with(&real);
    let written = std::fs::read_to_string(dot_git).ok().and_then(|c| {
        let dir = c.trim().strip_prefix("gitdir:")?.trim().to_owned();
        Some(lexical(&given.join(dir)))
    });
    let Some((written, gitdir)) = written.and_then(|w| Some((w.clone(), w.canonicalize().ok()?)))
    else {
        return Vec::new();
    };
    let back = std::fs::read_to_string(gitdir.join("gitdir"))
        .ok()
        .and_then(|b| gitdir.join(b.trim()).canonicalize().ok());
    if inside(&written) || inside(&gitdir) || back != Some(real.join(".git")) {
        return Vec::new();
    }
    let common = std::fs::read_to_string(gitdir.join("commondir"))
        .ok()
        .map(|c| lexical(&written.join(c.trim())))
        .filter(|c| !inside(c) && c.canonicalize().is_ok_and(|r| r != gitdir && !inside(&r)));
    std::iter::once(written).chain(common).collect()
}

fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            },
            Component::CurDir => {},
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linked_worktree(back: impl FnOnce(&Path) -> String) -> (tempfile::TempDir, PathBuf) {
        let repo = tempfile::tempdir().unwrap();
        let wt_git = repo.path().join("main/.git/worktrees/wt");
        std::fs::create_dir_all(&wt_git).unwrap();
        std::fs::write(wt_git.join("commondir"), "../..\n").unwrap();
        let wt = repo.path().join("wt");
        std::fs::create_dir(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", wt_git.display())).unwrap();
        std::fs::write(wt_git.join("gitdir"), back(&wt)).unwrap();
        (repo, wt)
    }

    fn worktree_paths(base: &Path) -> [PathBuf; 3] {
        [
            base.join("wt/.git"),
            base.join("main/.git/worktrees/wt"),
            base.join("main/.git"),
        ]
    }

    #[test]
    fn plain_repo_binds_dot_git() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Checkout::resolve(dir.path()).git_paths().is_empty());
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert_eq!(
            Checkout::resolve(dir.path()).git_paths(),
            [dir.path().join(".git")]
        );
    }

    #[test]
    fn linked_worktree_binds_its_git_dirs() {
        let (repo, wt) = linked_worktree(|wt| format!("{}\n", wt.join(".git").display()));
        assert_eq!(
            Checkout::resolve(&wt).git_paths(),
            worktree_paths(repo.path())
        );
    }

    #[test]
    fn relative_back_link_is_resolved_from_the_git_dir() {
        let (repo, wt) = linked_worktree(|_| "../../../../wt/.git\n".into());
        assert_eq!(
            Checkout::resolve(&wt).git_paths(),
            worktree_paths(repo.path())
        );
    }

    #[test]
    fn git_dirs_are_bound_at_the_written_path() {
        let (repo, _) = linked_worktree(|wt| format!("{}\n", wt.join(".git").display()));
        let link = tempfile::tempdir().unwrap();
        let via = link.path().join("via");
        std::os::unix::fs::symlink(repo.path(), &via).unwrap();
        std::fs::write(via.join("wt/.git"), "gitdir: ../main/.git/worktrees/wt\n").unwrap();
        assert_eq!(
            Checkout::resolve(via.join("wt")).git_paths(),
            worktree_paths(&via)
        );
    }

    #[test]
    fn forged_git_file_binds_nothing_outside() {
        let (_repo, wt) = linked_worktree(|_| "/elsewhere/.git\n".into());
        assert_eq!(Checkout::resolve(&wt).git_paths(), [wt.join(".git")]);
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", outside.path().display()),
        )
        .unwrap();
        assert_eq!(Checkout::resolve(&wt).git_paths(), [wt.join(".git")]);
    }

    #[test]
    fn symlinked_dot_git_binds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join(".git")).unwrap();
        assert!(Checkout::resolve(dir.path()).git_paths().is_empty());
    }

    #[test]
    fn restores_an_absent_git() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let dot_git = dir.path().join(".git");
        let absent = Checkout::resolve(dir.path());
        std::fs::create_dir_all(dot_git.join("hooks")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dot_git.join("hooks/out")).unwrap();
        absent.restore_git();
        assert!(dot_git.symlink_metadata().is_err());
        assert!(outside.path().exists());
        std::os::unix::fs::symlink(outside.path(), &dot_git).unwrap();
        absent.restore_git();
        assert!(dot_git.symlink_metadata().is_err());
        assert!(outside.path().exists());
        std::fs::create_dir(&dot_git).unwrap();
        Checkout::resolve(dir.path()).restore_git();
        assert!(dot_git.is_dir());
    }

    #[test]
    fn restores_a_symlinked_git() {
        let dir = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let dot_git = dir.path().join(".git");
        std::os::unix::fs::symlink(target.path(), &dot_git).unwrap();
        let linked = Checkout::resolve(dir.path());
        linked.restore_git();
        assert_eq!(std::fs::read_link(&dot_git).unwrap(), target.path());
        std::fs::remove_file(&dot_git).unwrap();
        std::fs::create_dir_all(dot_git.join("hooks")).unwrap();
        linked.restore_git();
        assert_eq!(std::fs::read_link(&dot_git).unwrap(), target.path());
        assert!(target.path().read_dir().unwrap().next().is_none());
    }
}
