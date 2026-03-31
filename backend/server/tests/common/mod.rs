//! Common test utilities for integration tests.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use eka_ci_server::db::DbService;
use eka_ci_server::db::model::build_event::DrvBuildState;
use eka_ci_server::db::model::drv::Drv;
use eka_ci_server::db::model::drv_id::DrvId;
use tempfile::TempDir;
use tokio::time::sleep;

/// Test context containing temporary directories and services.
pub struct TestContext {
    pub temp_dir: TempDir,
    pub db_service: DbService,
    pub logs_dir: PathBuf,
}

impl TestContext {
    /// Create a new test context with isolated DB and file system.
    pub async fn new() -> Result<Self> {
        let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
        let db_path = temp_dir.path().join("test.db");
        let logs_dir = temp_dir.path().join("logs");

        std::fs::create_dir_all(&logs_dir)?;

        let db_service = DbService::new(&db_path)
            .await
            .context("Failed to initialize test database")?;

        Ok(Self {
            temp_dir,
            db_service,
            logs_dir,
        })
    }

    /// Get the path to the test database.
    pub fn db_path(&self) -> PathBuf {
        self.temp_dir.path().join("test.db")
    }
}

/// Create a simple Nix derivation for testing.
///
/// Returns the derivation path (.drv file path).
pub fn create_simple_drv(name: &str, should_succeed: bool) -> Result<String> {
    let nix_expr = if should_succeed {
        format!(
            r#"derivation {{
              name = "{}";
              system = "{}";
              builder = "/bin/sh";
              args = ["-c" "echo success > $out"];
            }}"#,
            name,
            current_system()
        )
    } else {
        format!(
            r#"derivation {{
              name = "{}";
              system = "{}";
              builder = "/bin/sh";
              args = ["-c" "exit 1"];
            }}"#,
            name,
            current_system()
        )
    };

    let output = Command::new("nix-instantiate")
        .arg("-E")
        .arg(&nix_expr)
        .output()
        .context("Failed to execute nix-instantiate")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("nix-instantiate failed: {}", stderr));
    }

    let drv_path = String::from_utf8(output.stdout)
        .context("Invalid UTF-8 from nix-instantiate")?
        .trim()
        .to_string();

    Ok(drv_path)
}

/// Get the current system architecture for Nix.
fn current_system() -> &'static str {
    if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        "x86_64-linux"
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "aarch64") {
        "aarch64-linux"
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "x86_64") {
        "x86_64-darwin"
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        "aarch64-darwin"
    } else {
        "x86_64-linux" // default fallback
    }
}

/// Create a test Drv struct (without calling real nix).
///
/// Useful for unit tests where you don't need actual nix derivations.
pub fn test_drv(name: &str, system: &str) -> Drv {
    let drv_id_str = format!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa0-{}.drv", name);
    Drv {
        drv_path: DrvId::try_from(drv_id_str.as_str()).unwrap(),
        system: system.to_string(),
        prefer_local_build: false,
        required_system_features: None,
        is_fod: false,
        build_state: DrvBuildState::Queued,
    }
}

/// Insert a test drv into the database.
pub async fn insert_test_drv(db: &DbService, drv: &Drv) -> Result<()> {
    db.insert_drvs_and_references(&[drv.clone()], &[])
        .await
        .context("Failed to insert test drv")
}

/// Wait for a drv to reach the expected state, with timeout.
///
/// Polls the database every 100ms until the drv reaches the expected state
/// or the timeout is reached.
pub async fn wait_for_drv_state(
    db: &DbService,
    drv_path: &DrvId,
    expected_state: DrvBuildState,
    timeout: Duration,
) -> Result<()> {
    let start = std::time::Instant::now();

    loop {
        if start.elapsed() > timeout {
            // Get current state for error message
            let current_state = db
                .get_drv(drv_path)
                .await?
                .map(|d| format!("{:?}", d.build_state))
                .unwrap_or_else(|| "not found".to_string());

            return Err(anyhow!(
                "Timeout waiting for drv {} to reach state {:?} (current: {})",
                &**drv_path,
                expected_state,
                current_state
            ));
        }

        let maybe_drv = db.get_drv(drv_path).await?;

        if let Some(drv) = maybe_drv {
            if drv.build_state == expected_state {
                return Ok(());
            }
        }

        sleep(Duration::from_millis(100)).await;
    }
}

/// Build a Nix derivation using `nix build`.
///
/// Returns true if build succeeded, false if it failed.
pub async fn build_drv(drv_path: &str) -> Result<bool> {
    let output = tokio::process::Command::new("nix")
        .arg("build")
        .arg(drv_path)
        .arg("--no-link")
        .output()
        .await
        .context("Failed to execute nix build")?;

    Ok(output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_context_creation() {
        let ctx = TestContext::new().await.unwrap();
        assert!(ctx.db_path().exists());
        assert!(ctx.logs_dir.exists());
    }

    #[test]
    fn test_drv_creation() {
        let drv = test_drv("test", "x86_64-linux");
        assert_eq!(drv.system, "x86_64-linux");
        assert_eq!(drv.build_state, DrvBuildState::Queued);
        assert!(!drv.is_fod);
    }
}
