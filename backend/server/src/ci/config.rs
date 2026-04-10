use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Job {
    pub file: PathBuf,
    #[serde(default = "default_true")]
    pub allow_eval_failures: bool,
    /// Cache IDs to push build outputs to (references server-configured caches)
    /// These are resolved server-side for security - no arbitrary commands allowed
    #[serde(default)]
    pub caches: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Check {
    /// Optional shell name
    /// - When shell_nix=false: maps to flake devShells (e.g., "foo" → `nix develop .#foo`)
    /// - When shell_nix=true: maps to shell.nix attribute (e.g., "foo" → `nix-shell shell.nix -A
    ///   foo`)
    /// - If not specified: uses default shell
    #[serde(default)]
    pub shell: Option<String>,
    /// Whether to use shell.nix instead of flake devShells
    /// - false (default): use `nix develop` with flake.nix
    /// - true: use `nix-shell` with shell.nix
    #[serde(default = "default_false")]
    pub shell_nix: bool,
    /// Command to execute in the sandboxed checkout
    pub command: String,
    /// Whether to allow network access in the sandbox
    #[serde(default = "default_false")]
    pub allow_network: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CIConfig {
    #[serde(default)]
    pub jobs: HashMap<String, Job>,
    #[serde(default)]
    pub checks: HashMap<String, Check>,
}

impl CIConfig {
    pub fn from_str(string: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str::<Self>(string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialization() {
        let example_config = r#"{
  "jobs": {
    "stdenv": {
      "file": "../stdenv.nix",
      "allow-eval-failures": false
    }
  }
}"#;
        serde_json::from_str::<CIConfig>(example_config)
            .expect("Failed to deserialize example config");
    }

    #[test]
    fn test_deserialization_with_checks() {
        let example_config = r#"{
  "jobs": {
    "stdenv": {
      "file": "../stdenv.nix",
      "allow-eval-failures": false
    }
  },
  "checks": {
    "nixfmt": {
      "shell": "formatting",
      "command": "nixfmt --check **/*.nix",
      "allow_network": false
    },
    "cargo-test": {
      "command": "cargo test --workspace",
      "allow_network": true
    }
  }
}"#;
        let config = serde_json::from_str::<CIConfig>(example_config)
            .expect("Failed to deserialize example config with checks");

        assert_eq!(config.checks.len(), 2);
        assert!(config.checks.contains_key("nixfmt"));
        assert!(config.checks.contains_key("cargo-test"));

        // Verify shell field
        assert_eq!(
            config.checks.get("nixfmt").unwrap().shell,
            Some("formatting".to_string())
        );
        assert_eq!(config.checks.get("cargo-test").unwrap().shell, None);

        // Verify shell_nix defaults to false
        assert_eq!(config.checks.get("nixfmt").unwrap().shell_nix, false);
        assert_eq!(config.checks.get("cargo-test").unwrap().shell_nix, false);
    }

    #[test]
    fn test_deserialization_with_caches() {
        let example_config = r#"{
  "jobs": {
    "my-package": {
      "file": "default.nix",
      "caches": ["production-s3", "public-cachix"]
    }
  }
}"#;
        let config = serde_json::from_str::<CIConfig>(example_config)
            .expect("Failed to deserialize config with caches");

        assert_eq!(config.jobs.len(), 1);
        let job = config.jobs.get("my-package").unwrap();
        assert_eq!(job.caches.len(), 2);
        assert_eq!(job.caches[0], "production-s3");
        assert_eq!(job.caches[1], "public-cachix");
    }
}
