use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use sandbox::check::CheckSandbox;
use sandbox::checkout::Checkout;
use sandbox::devshell::DevShell;
use tracing::info;

use super::CheckResult;
use crate::ci::config::Check;

pub async fn execute_check(
    sandbox: &CheckSandbox,
    check: &Check,
    checkout: &Path,
    check_name: &str,
) -> Result<CheckResult> {
    info!("Executing check: {}", check_name);
    let checkout = Checkout::resolve(checkout);
    let shell = DevShell::for_check(&check.command, check.shell.as_deref(), check.shell_nix);

    let start = Instant::now();
    let out = sandbox
        .execute(&checkout, &check.command, shell, check.allow_network)
        .await?;
    let duration_ms = start.elapsed().as_millis() as u64;

    let (success, exit_code, stderr) = (out.success(), out.exit_code(), out.stderr_with_timeout());
    info!(
        "Check {} completed in {}ms with exit code {}",
        check_name, duration_ms, exit_code
    );
    Ok(CheckResult::new(
        success,
        exit_code,
        out.stdout,
        stderr,
        duration_ms,
    ))
}
