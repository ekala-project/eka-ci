mod config;
mod executor;
mod nix_shell;
mod simple_executor;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use colored::Colorize;

use self::config::load_config;
use self::executor::CheckExecutor;

/// Check subcommands
#[derive(Debug, clap::Subcommand)]
pub enum CheckCommand {
    /// Run checks locally with CI-equivalent environment
    Run {
        /// Path to repository (default: current dir)
        #[arg(long)]
        repo_path: Option<PathBuf>,

        /// Specific check to run (default: all)
        #[arg(long)]
        check: Option<String>,

        /// Show what would run without executing
        #[arg(long)]
        dry_run: bool,

        /// Verbose output
        #[arg(short, long)]
        verbose: bool,
    },

    /// List available checks
    List {
        /// Path to repository (default: current dir)
        #[arg(long)]
        repo_path: Option<PathBuf>,
    },
}

/// Handle check command
pub async fn handle_check_command(cmd: CheckCommand) -> Result<()> {
    match cmd {
        CheckCommand::Run {
            repo_path,
            check,
            dry_run,
            verbose,
        } => run_checks(repo_path, check, dry_run, verbose).await,

        CheckCommand::List { repo_path } => list_checks(repo_path).await,
    }
}

/// Run checks
async fn run_checks(
    repo_path: Option<PathBuf>,
    check_filter: Option<String>,
    dry_run: bool,
    verbose: bool,
) -> Result<()> {
    let repo_path = repo_path
        .or_else(|| std::env::current_dir().ok())
        .context("failed to determine repository path")?;

    println!(
        "{} {}",
        "→".blue(),
        format!("Running checks in {}", repo_path.display()).dimmed()
    );

    // Load configuration
    let config = load_config(&repo_path).await?;

    if config.checks.is_empty() {
        anyhow::bail!("No checks defined in .ekaci/config.json");
    }

    // Filter checks if specified
    let checks_to_run: Vec<_> = if let Some(filter) = &check_filter {
        config
            .checks
            .iter()
            .filter(|(name, _)| name.as_str() == filter)
            .collect()
    } else {
        config.checks.iter().collect()
    };

    if checks_to_run.is_empty() {
        if let Some(filter) = check_filter {
            anyhow::bail!("No check found with name '{}'", filter);
        } else {
            anyhow::bail!("No checks found matching criteria");
        }
    }

    println!(
        "{} Found {} check(s) to run\n",
        "→".blue(),
        checks_to_run.len()
    );

    if dry_run {
        for (name, check) in checks_to_run {
            println!("Would run: {}", name.bold());
            println!("  Command: {}", check.command);
            if let Some(shell) = &check.shell {
                println!("  Shell: {}", shell);
            }
            println!(
                "  Shell type: {}",
                if check.shell_nix {
                    "shell.nix"
                } else {
                    "flake"
                }
            );
            println!("  Allow network: {}", check.allow_network);
            println!();
        }
        return Ok(());
    }

    // Execute checks
    let executor = CheckExecutor::new(repo_path.clone());
    let mut results = Vec::new();

    for (name, check) in checks_to_run {
        print!("{} {} ... ", "→".blue(), name.bold());
        std::io::Write::flush(&mut std::io::stdout())?;

        let result = executor.execute_check(check, name).await?;
        results.push(result);

        let last = results.last().unwrap();
        if last.success {
            println!("{} ({:.2}s)", "✓".green(), last.duration.as_secs_f64());
        } else {
            println!("{} ({:.2}s)", "✗".red(), last.duration.as_secs_f64());
        }

        if verbose || !last.success {
            if !last.stdout.is_empty() {
                println!("{}", "  stdout:".dimmed());
                for line in last.stdout.lines() {
                    println!("    {}", line);
                }
            }
            if !last.stderr.is_empty() {
                println!("{}", "  stderr:".dimmed());
                for line in last.stderr.lines() {
                    println!("    {}", line.red());
                }
            }
            println!();
        }
    }

    // Summary
    let passed = results.iter().filter(|r| r.success).count();
    let failed = results.len() - passed;
    let total_time: Duration = results.iter().map(|r| r.duration).sum();

    println!("\n{}", "Summary:".bold());
    println!(
        "  {} passed, {} failed",
        passed.to_string().green(),
        failed.to_string().red()
    );
    println!("  Total time: {:.2}s", total_time.as_secs_f64());

    if failed > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// List available checks
async fn list_checks(repo_path: Option<PathBuf>) -> Result<()> {
    let repo_path = repo_path
        .or_else(|| std::env::current_dir().ok())
        .context("failed to determine repository path")?;

    let config = load_config(&repo_path).await?;

    if config.checks.is_empty() {
        println!("No checks defined in .ekaci/config.json");
        return Ok(());
    }

    println!("{} Available checks:", "→".blue());
    for (name, check) in config.checks {
        println!("  {}", name.bold());
        println!("    Command: {}", check.command.dimmed());
        if let Some(shell) = check.shell {
            println!("    Shell: {}", shell.dimmed());
        }
        println!(
            "    Shell type: {}",
            if check.shell_nix {
                "shell.nix"
            } else {
                "flake"
            }
            .dimmed()
        );
        println!(
            "    Allow network: {}",
            check.allow_network.to_string().dimmed()
        );
        println!();
    }

    Ok(())
}
