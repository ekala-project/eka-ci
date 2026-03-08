pub mod executor;
pub mod service;

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

impl CheckResult {
    pub fn new(
        success: bool,
        exit_code: i32,
        stdout: String,
        stderr: String,
        duration_ms: u64,
    ) -> Self {
        Self {
            success,
            exit_code,
            stdout,
            stderr,
            duration_ms,
        }
    }
}

/// Parse environment variables from the output of `nix-shell --run 'env'`
pub fn parse_env_output(output: &str) -> HashMap<String, String> {
    let mut env_vars = HashMap::new();

    for line in output.lines() {
        if let Some((key, value)) = line.split_once('=') {
            env_vars.insert(key.to_string(), value.to_string());
        }
    }

    env_vars
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_env_output() {
        let output = "PATH=/nix/store/abc-hello/bin:/usr/bin\nHOME=/home/user\nFOO=bar";
        let env = parse_env_output(output);

        assert_eq!(
            env.get("PATH"),
            Some(&"/nix/store/abc-hello/bin:/usr/bin".to_string())
        );
        assert_eq!(env.get("HOME"), Some(&"/home/user".to_string()));
        assert_eq!(env.get("FOO"), Some(&"bar".to_string()));
    }
}

/// Get the cache directory for EkaCI gcroots following XDG Base Directory specification
/// Returns $XDG_CACHE_HOME/eka-ci if XDG_CACHE_HOME is set,
/// otherwise $HOME/.cache/eka-ci if HOME is set,
/// otherwise /tmp/eka-ci-cache as a fallback
pub fn get_cache_dir() -> PathBuf {
    if let Ok(xdg_cache) = std::env::var("XDG_CACHE_HOME") {
        PathBuf::from(xdg_cache).join("eka-ci")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".cache").join("eka-ci")
    } else {
        PathBuf::from("/tmp/eka-ci-cache")
    }
}
