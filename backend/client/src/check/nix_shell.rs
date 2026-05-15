use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result};
use tracing::debug;

/// Get environment variables from nix shell (either flake or shell.nix)
pub async fn get_nix_shell_env(
    shell: Option<&str>,
    shell_nix: bool,
    repo_path: &Path,
) -> Result<HashMap<String, String>> {
    if shell_nix {
        get_shell_nix_env(shell, repo_path).await
    } else {
        get_flake_shell_env(shell, repo_path).await
    }
}

/// Get environment from flake-based dev shell
async fn get_flake_shell_env(
    shell: Option<&str>,
    repo_path: &Path,
) -> Result<HashMap<String, String>> {
    let flake_path = repo_path.join("flake.nix");
    if !flake_path.exists() {
        anyhow::bail!(
            "No flake.nix found in repository. The checks feature requires a flake.nix file. \
             Please add a flake.nix with devShells or set shell_nix to true to use shell.nix."
        );
    }

    // Build the flake reference
    let flake_ref = match shell {
        Some(shell_name) => format!(".#{}", shell_name),
        None => ".".to_string(),
    };

    debug!("Running nix develop {} --command env", flake_ref);

    let output = tokio::process::Command::new("nix")
        .env_clear()
        .current_dir(repo_path)
        .arg("develop")
        .arg(&flake_ref)
        .arg("--command")
        .arg("env")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to execute nix develop")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("nix develop failed: {}", stderr);
    }

    let stdout =
        String::from_utf8(output.stdout).context("Failed to parse nix develop output as UTF-8")?;

    Ok(parse_env_output(&stdout))
}

/// Get environment from shell.nix
async fn get_shell_nix_env(
    shell: Option<&str>,
    repo_path: &Path,
) -> Result<HashMap<String, String>> {
    let shell_nix_path = repo_path.join("shell.nix");
    if !shell_nix_path.exists() {
        anyhow::bail!(
            "No shell.nix found in repository. When shell_nix is true, a shell.nix file is \
             required. Please add shell.nix or set shell_nix to false."
        );
    }

    debug!(
        "Running nix-shell shell.nix{} --run env",
        shell.map(|s| format!(" -A {}", s)).unwrap_or_default()
    );

    let mut cmd = tokio::process::Command::new("nix-shell");
    cmd.env_clear().current_dir(repo_path).arg("shell.nix");

    if let Some(shell_name) = shell {
        cmd.arg("-A").arg(shell_name);
    }

    cmd.arg("--run")
        .arg("env")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = cmd.output().await.context("Failed to execute nix-shell")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("nix-shell failed: {}", stderr);
    }

    let stdout =
        String::from_utf8(output.stdout).context("Failed to parse nix-shell output as UTF-8")?;

    Ok(parse_env_output(&stdout))
}

/// Parse environment variables from the output of `env` command
fn parse_env_output(output: &str) -> HashMap<String, String> {
    let mut env_vars = HashMap::new();

    for line in output.lines() {
        if let Some((key, value)) = line.split_once('=') {
            env_vars.insert(key.to_string(), value.to_string());
        }
    }

    env_vars
}
