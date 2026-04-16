use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// Represents a flake output (check or package) to be used as a CI gate
#[derive(Debug, Clone)]
pub struct FlakeOutput {
    /// The type of output (checks or packages)
    pub output_type: FlakeOutputType,
    /// The system (e.g., "x86_64-linux")
    pub system: String,
    /// The name of the check or package
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FlakeOutputType {
    Check,
    Package,
}

impl FlakeOutput {
    /// Returns the full flake attribute path for this output
    /// e.g., "checks.x86_64-linux.formatter" or "packages.x86_64-linux.hello"
    pub fn attr_path(&self) -> String {
        let output_type_str = match self.output_type {
            FlakeOutputType::Check => "checks",
            FlakeOutputType::Package => "packages",
        };
        format!("{}.{}.{}", output_type_str, self.system, self.name)
    }

    /// Returns the check name to use in the CI system
    /// e.g., "flake.checks.x86_64-linux.formatter"
    pub fn check_name(&self) -> String {
        format!("flake.{}", self.attr_path())
    }

    /// Returns the nix build command for this output
    /// e.g., "nix build .#checks.x86_64-linux.formatter"
    pub fn build_command(&self) -> String {
        format!("nix build .#{}", self.attr_path())
    }
}

/// Enumerates all flake checks available in the given directory
pub fn enumerate_flake_checks(repo_path: &Path) -> Result<Vec<FlakeOutput>, String> {
    enumerate_flake_outputs(repo_path, FlakeOutputType::Check)
}

/// Enumerates all flake packages available in the given directory
pub fn enumerate_flake_packages(repo_path: &Path) -> Result<Vec<FlakeOutput>, String> {
    enumerate_flake_outputs(repo_path, FlakeOutputType::Package)
}

/// Internal function to enumerate flake outputs of a specific type
fn enumerate_flake_outputs(
    repo_path: &Path,
    output_type: FlakeOutputType,
) -> Result<Vec<FlakeOutput>, String> {
    let output_type_str = match output_type {
        FlakeOutputType::Check => "checks",
        FlakeOutputType::Package => "packages",
    };

    // Run nix eval to get the structure of checks/packages
    // Format: { "x86_64-linux": { "check1": ..., "check2": ... }, "aarch64-linux": { ... } }
    let output = Command::new("nix")
        .arg("eval")
        .arg("--json")
        .arg("--apply")
        .arg(format!(
            "flake: builtins.mapAttrs (system: _: builtins.attrNames flake.{}.\"${{system}}\") \
             (flake.{} or {{}})",
            output_type_str, output_type_str
        ))
        .arg(format!("{}#", repo_path.display()))
        .output()
        .map_err(|e| format!("Failed to execute nix eval: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "nix eval failed for {}: {}",
            output_type_str, stderr
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse the JSON output
    // Expected format: { "x86_64-linux": ["check1", "check2"], "aarch64-linux": [...] }
    let systems_map: HashMap<String, Vec<String>> = serde_json::from_str(&stdout)
        .map_err(|e| format!("Failed to parse nix eval output: {}", e))?;

    // Convert to FlakeOutput structs
    let mut outputs = Vec::new();
    for (system, names) in systems_map {
        for name in names {
            outputs.push(FlakeOutput {
                output_type: output_type.clone(),
                system: system.clone(),
                name,
            });
        }
    }

    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flake_output_attr_path() {
        let output = FlakeOutput {
            output_type: FlakeOutputType::Check,
            system: "x86_64-linux".to_string(),
            name: "formatter".to_string(),
        };
        assert_eq!(output.attr_path(), "checks.x86_64-linux.formatter");

        let output = FlakeOutput {
            output_type: FlakeOutputType::Package,
            system: "aarch64-darwin".to_string(),
            name: "hello".to_string(),
        };
        assert_eq!(output.attr_path(), "packages.aarch64-darwin.hello");
    }

    #[test]
    fn test_flake_output_check_name() {
        let output = FlakeOutput {
            output_type: FlakeOutputType::Check,
            system: "x86_64-linux".to_string(),
            name: "formatter".to_string(),
        };
        assert_eq!(output.check_name(), "flake.checks.x86_64-linux.formatter");
    }

    #[test]
    fn test_flake_output_build_command() {
        let output = FlakeOutput {
            output_type: FlakeOutputType::Check,
            system: "x86_64-linux".to_string(),
            name: "formatter".to_string(),
        };
        assert_eq!(
            output.build_command(),
            "nix build .#checks.x86_64-linux.formatter"
        );
    }
}
