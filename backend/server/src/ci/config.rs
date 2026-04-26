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
    /// Optional size check configuration for detecting installation bloat
    #[serde(default)]
    pub size_check: Option<SizeCheck>,
}

/// Configuration for build output size checks
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SizeCheck {
    /// Maximum percentage increase allowed compared to baseline (base branch)
    /// Example: 10.0 means build fails if output is >10% larger than base branch
    pub max_increase_percent: f64,
    /// Base branch to compare against (default: "main")
    #[serde(default = "default_main_branch")]
    pub base_branch: String,
}

fn default_main_branch() -> String {
    "main".to_string()
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

/// Configuration for flake checks integration
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FlakeChecksConfig {
    /// Enable automatic CI gates for all flake checks
    #[serde(default = "default_false")]
    pub enable: bool,
}

/// Configuration for flake packages integration
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FlakePackagesConfig {
    /// Enable automatic CI gates for all flake packages
    #[serde(default = "default_false")]
    pub enable: bool,
}

/// Configuration for flake-based CI integration
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FlakeConfig {
    /// Configuration for flake checks
    #[serde(default)]
    pub checks: FlakeChecksConfig,
    /// Configuration for flake packages
    #[serde(default)]
    pub packages: FlakePackagesConfig,
}

/// Per-repo overrides for the package-change summary renderer.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PackageChangeSummaryConfig {
    /// Render the package-changes section in the GitHub check.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Soft cap on table rows before collapsing to a counts-only summary.
    #[serde(default = "default_max_packages_listed")]
    pub max_packages_listed: usize,
    /// Show `RebuildOnly` rows alongside Added/Removed/Bumped.
    #[serde(default = "default_false")]
    pub include_rebuild_only: bool,
}

fn default_max_packages_listed() -> usize {
    100
}

impl Default for PackageChangeSummaryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_packages_listed: default_max_packages_listed(),
            include_rebuild_only: false,
        }
    }
}

/// Per-repo overrides for the rebuild-impact analyzer.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RebuildImpactConfig {
    /// Render the rebuild-impact section in the GitHub check.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Number of top-blast-radius packages reported.
    #[serde(default = "default_max_top_blast_radius")]
    pub max_top_blast_radius: usize,
    /// Walk the full transitive dependent set instead of seeds-only.
    #[serde(default = "default_false")]
    pub compute_full_blast_radius: bool,
}

fn default_max_top_blast_radius() -> usize {
    5
}

impl Default for RebuildImpactConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_top_blast_radius: default_max_top_blast_radius(),
            compute_full_blast_radius: false,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CIConfig {
    #[serde(default)]
    pub jobs: HashMap<String, Job>,
    #[serde(default)]
    pub checks: HashMap<String, Check>,
    #[serde(default)]
    pub flake: Option<FlakeConfig>,
    /// Overrides for the package-changes section; absent ⇒ engine defaults.
    #[serde(default)]
    pub package_change_summary: Option<PackageChangeSummaryConfig>,
    /// Overrides for the rebuild-impact section; absent ⇒ engine defaults.
    #[serde(default)]
    pub rebuild_impact: Option<RebuildImpactConfig>,
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

    #[test]
    fn test_deserialization_with_size_check() {
        let example_config = r#"{
  "jobs": {
    "my-package": {
      "file": "default.nix",
      "size_check": {
        "max_increase_percent": 5.0,
        "base_branch": "main"
      }
    },
    "no-size-check": {
      "file": "other.nix"
    }
  }
}"#;
        let config = serde_json::from_str::<CIConfig>(example_config)
            .expect("Failed to deserialize config with size_check");

        assert_eq!(config.jobs.len(), 2);

        // Verify size check configured correctly
        let job = config.jobs.get("my-package").unwrap();
        assert!(job.size_check.is_some());
        let size_check = job.size_check.as_ref().unwrap();
        assert_eq!(size_check.max_increase_percent, 5.0);
        assert_eq!(size_check.base_branch, "main");

        // Verify size check is optional
        let job2 = config.jobs.get("no-size-check").unwrap();
        assert!(job2.size_check.is_none());
    }

    #[test]
    fn test_deserialization_with_flake() {
        let example_config = r#"{
  "jobs": {
    "my-package": {
      "file": "default.nix"
    }
  },
  "flake": {
    "checks": {
      "enable": true
    },
    "packages": {
      "enable": true
    }
  }
}"#;
        let config = serde_json::from_str::<CIConfig>(example_config)
            .expect("Failed to deserialize config with flake");

        assert!(config.flake.is_some());
        let flake = config.flake.as_ref().unwrap();
        assert_eq!(flake.checks.enable, true);
        assert_eq!(flake.packages.enable, true);
    }

    #[test]
    fn test_deserialization_without_flake() {
        let example_config = r#"{
  "jobs": {
    "my-package": {
      "file": "default.nix"
    }
  }
}"#;
        let config = serde_json::from_str::<CIConfig>(example_config)
            .expect("Failed to deserialize config without flake");

        assert!(config.flake.is_none());
    }

    #[test]
    fn test_deserialization_with_package_change_summary() {
        let example_config = r#"{
  "jobs": {
    "stdenv": { "file": "../stdenv.nix" }
  },
  "package_change_summary": {
    "enabled": true,
    "max_packages_listed": 250,
    "include_rebuild_only": true
  },
  "rebuild_impact": {
    "enabled": false,
    "max_top_blast_radius": 10,
    "compute_full_blast_radius": true
  }
}"#;
        let config = serde_json::from_str::<CIConfig>(example_config)
            .expect("Failed to deserialize config with change-summary block");

        let pcs = config
            .package_change_summary
            .as_ref()
            .expect("package_change_summary should be present");
        assert_eq!(pcs.enabled, true);
        assert_eq!(pcs.max_packages_listed, 250);
        assert_eq!(pcs.include_rebuild_only, true);

        let ri = config
            .rebuild_impact
            .as_ref()
            .expect("rebuild_impact should be present");
        assert_eq!(ri.enabled, false);
        assert_eq!(ri.max_top_blast_radius, 10);
        assert_eq!(ri.compute_full_blast_radius, true);
    }

    #[test]
    fn test_deserialization_change_summary_blocks_absent() {
        let example_config = r#"{
  "jobs": { "stdenv": { "file": "../stdenv.nix" } }
}"#;
        let config = serde_json::from_str::<CIConfig>(example_config)
            .expect("Failed to deserialize minimal config");

        assert!(config.package_change_summary.is_none());
        assert!(config.rebuild_impact.is_none());
    }

    #[test]
    fn test_deserialization_change_summary_partial_uses_defaults() {
        // Only `enabled` set; other fields fall back to defaults.
        let example_config = r#"{
  "package_change_summary": { "enabled": false },
  "rebuild_impact": { "enabled": false }
}"#;
        let config = serde_json::from_str::<CIConfig>(example_config)
            .expect("Failed to deserialize partial change-summary block");

        let pcs = config.package_change_summary.as_ref().unwrap();
        assert_eq!(pcs.enabled, false);
        assert_eq!(pcs.max_packages_listed, 100);
        assert_eq!(pcs.include_rebuild_only, false);

        let ri = config.rebuild_impact.as_ref().unwrap();
        assert_eq!(ri.enabled, false);
        assert_eq!(ri.max_top_blast_radius, 5);
        assert_eq!(ri.compute_full_blast_radius, false);
    }

    #[test]
    fn test_default_impls_match_engine_defaults() {
        let pcs = PackageChangeSummaryConfig::default();
        assert_eq!(pcs.enabled, true);
        assert_eq!(pcs.max_packages_listed, 100);
        assert_eq!(pcs.include_rebuild_only, false);

        let ri = RebuildImpactConfig::default();
        assert_eq!(ri.enabled, true);
        assert_eq!(ri.max_top_blast_radius, 5);
        assert_eq!(ri.compute_full_blast_radius, false);
    }
}
