use std::sync::OnceLock;

use anyhow::Result;

/// Lazily-initialized XDG base directories for ekaci.
///
/// This uses the standard XDG Base Directory specification with the "ekaci" prefix,
/// typically resolving to:
/// - Config: `~/.config/ekaci/`
/// - Data: `~/.local/share/ekaci/`
/// - Cache: `~/.cache/ekaci/`
/// - Runtime: `$XDG_RUNTIME_DIR/ekaci/` (usually `/run/user/{uid}/ekaci/`)
///
/// The directories are created on first access if they don't exist.
///
/// We store a Result to allow error propagation while still caching the initialization attempt.
static EKA_DIRS: OnceLock<Result<xdg::BaseDirectories, String>> = OnceLock::new();

/// Get or initialize the ekaci XDG base directories.
///
/// This function is lazy - it only initializes the directories on first call,
/// then caches the result for subsequent calls (whether success or error).
///
/// # Errors
///
/// Returns an error if:
/// - The XDG base directories cannot be determined (e.g., $HOME is not set)
/// - The directories cannot be created due to permission issues
///
/// # Future work
///
/// TODO: Implement fallback to system-wide locations when user directories fail:
/// - `/var/lib/ekaci/` for data
/// - `/etc/ekaci/` for config
/// - `/run/ekaci/` for runtime files (when running as root or system service)
pub fn eka_dirs() -> Result<&'static xdg::BaseDirectories> {
    EKA_DIRS
        .get_or_init(|| {
            xdg::BaseDirectories::with_prefix("ekaci").map_err(|e| {
                format!(
                    "Failed to initialize XDG base directories: {}. Ensure $HOME is set and you \
                     have write permissions to create directories in:\n- $XDG_DATA_HOME (default: \
                     ~/.local/share/)\n- $XDG_CONFIG_HOME (default: ~/.config/)\n- \
                     $XDG_CACHE_HOME (default: ~/.cache/)\n- $XDG_RUNTIME_DIR (usually: \
                     /run/user/$UID/)",
                    e
                )
            })
        })
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))
}
