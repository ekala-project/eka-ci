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
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Check {
    /// Nix packages to include in the environment (e.g., ["nixfmt", "statix"])
    pub packages: Vec<String>,
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
      "packages": ["nixfmt"],
      "command": "nixfmt --check **/*.nix",
      "allow_network": false
    },
    "cargo-test": {
      "packages": ["cargo", "rustc"],
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
    }
}
