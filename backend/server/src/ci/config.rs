use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::hooks::types::PostBuildHook;

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
    /// Post-build hooks to run after successful builds
    #[serde(default)]
    pub post_build_hooks: Vec<PostBuildHook>,
    /// Additional post-build hooks to run for FODs (fixed-output derivations)
    /// These run in addition to regular post_build_hooks
    #[serde(default)]
    pub fod_post_build_hooks: Vec<PostBuildHook>,
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
    fn test_deserialization_with_hooks() {
        let example_config = r#"{
  "jobs": {
    "my-package": {
      "file": "default.nix",
      "post_build_hooks": [
        {
          "name": "push-to-cache",
          "command": ["nix", "copy", "--to", "s3://my-cache"]
        }
      ],
      "fod_post_build_hooks": [
        {
          "name": "push-fods",
          "command": ["cachix", "push", "public"],
          "env": {
            "CACHIX_AUTH_TOKEN": "secret"
          }
        }
      ]
    }
  }
}"#;
        let config = serde_json::from_str::<CIConfig>(example_config)
            .expect("Failed to deserialize config with hooks");

        assert_eq!(config.jobs.len(), 1);
        let job = config.jobs.get("my-package").unwrap();
        assert_eq!(job.post_build_hooks.len(), 1);
        assert_eq!(job.fod_post_build_hooks.len(), 1);
        assert_eq!(job.post_build_hooks[0].name, "push-to-cache");
        assert_eq!(job.fod_post_build_hooks[0].name, "push-fods");
        assert_eq!(
            job.fod_post_build_hooks[0].env.get("CACHIX_AUTH_TOKEN"),
            Some(&"secret".to_string())
        );
    }
}
