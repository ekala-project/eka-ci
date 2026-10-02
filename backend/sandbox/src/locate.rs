use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::HELPER_BIN;

pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

pub fn resolve_program(name: &str) -> Result<PathBuf> {
    let found = find_in_path(name).with_context(|| format!("`{name}` not found in PATH"))?;
    found
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", found.display()))
}

pub(crate) fn default_helper() -> Result<PathBuf> {
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(HELPER_BIN)))
        .filter(|p| is_executable(p));
    let found = sibling.or_else(|| find_in_path(HELPER_BIN));
    match found {
        Some(p) => Ok(p.canonicalize().unwrap_or(p)),
        None => bail!(
            "`{HELPER_BIN}` not found next to the server binary or in PATH; set `[eval] \
             helper_path`"
        ),
    }
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
