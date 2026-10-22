use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use colored::Colorize;
use search_index::generators;
use search_index::types::{Manifest, PackageEntry};
use tokio::sync::Semaphore;

pub async fn handle_generate_search_index(
    flake_ref: String,
    system: String,
    channel: String,
    output: PathBuf,
    with_files: bool,
    jobs: usize,
) -> Result<()> {
    println!("{} Generating search index...", "->".blue());
    println!("   Flake ref:   {}", flake_ref);
    println!("   System:      {}", system);
    println!("   Channel:     {}", channel);
    println!("   Output:      {}", output.display());
    println!("   With files:  {}", with_files);
    if with_files {
        println!("   Build jobs:  {}", jobs);
    }
    println!();

    let start = Instant::now();

    // Generate package entries via `nix search`.
    println!(
        "{} Running nix search (this may take a while)...",
        "->".blue()
    );
    let packages = generators::packages::generate_package_entries(&flake_ref, &system)
        .await
        .context("failed to generate package entries")?;
    println!("   Found {} packages", packages.len().to_string().green());

    // Optionally build packages and index their files.
    let file_entries = if with_files {
        println!();
        println!(
            "{} Building packages and indexing files...",
            "->".blue()
        );
        let store_outputs =
            build_and_collect_outputs(&flake_ref, &packages, jobs).await;

        let succeeded: usize = store_outputs.len();
        println!(
            "   Built {} of {} packages",
            succeeded.to_string().green(),
            packages.len()
        );

        println!("{} Walking store paths...", "->".blue());
        let files = generators::files::generate_file_entries(&store_outputs)
            .await
            .context("failed to generate file entries")?;
        println!(
            "   Indexed {} files",
            files.len().to_string().green()
        );
        files
    } else {
        Vec::new()
    };

    // Build manifest.
    let nixpkgs_rev = extract_rev(&flake_ref);
    let manifest = Manifest {
        generated_at: chrono::Utc::now().to_rfc3339(),
        channel_name: channel,
        nixpkgs_rev,
        package_count: packages.len(),
        file_count: file_entries.len(),
        option_count: 0,
    };

    // Build SQLite database directly to the output path.
    println!("{} Building SQLite database...", "->".blue());
    let out = output.clone();
    let m = manifest.clone();
    let db_size = tokio::task::spawn_blocking(move || -> Result<u64> {
        generators::sqlite::build_database_to_path(&out, &packages, &file_entries, &[], &m)?;
        let meta = std::fs::metadata(&out).context("failed to read output file metadata")?;
        Ok(meta.len())
    })
    .await
    .context("database build task panicked")??;

    let elapsed = start.elapsed();

    println!();
    println!("{}", "Summary:".bold());
    println!("   Packages:  {}", manifest.package_count);
    println!("   Files:     {}", manifest.file_count);
    println!(
        "   File size: {} bytes ({:.1} MiB)",
        db_size,
        db_size as f64 / 1_048_576.0
    );
    println!("   Output:    {}", output.display());
    println!("   Time:      {:.2}s", elapsed.as_secs_f64());

    Ok(())
}

/// Build each package and collect store output paths.
///
/// Runs `nix build <flake>#<attr> --no-link --print-out-paths` for each
/// package, limited to `max_jobs` concurrent builds. Failures are skipped.
async fn build_and_collect_outputs(
    flake_ref: &str,
    packages: &[PackageEntry],
    max_jobs: usize,
) -> HashMap<String, Vec<(String, String)>> {
    let total = packages.len();
    let completed = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let semaphore = Arc::new(Semaphore::new(max_jobs));

    let mut handles = Vec::with_capacity(total);

    for pkg in packages {
        let attr = pkg.attr.clone();
        let flake = flake_ref.to_string();
        let sem = semaphore.clone();
        let completed = completed.clone();
        let failed = failed.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            let installable = format!("{flake}#{attr}");

            let result = tokio::process::Command::new("nix")
                .args(["build", &installable, "--no-link", "--print-out-paths"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .output()
                .await;

            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;

            match result {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let paths: Vec<(String, String)> = stdout
                        .lines()
                        .filter(|l| !l.is_empty())
                        .map(|p| ("out".to_string(), p.to_string()))
                        .collect();
                    if done % 100 == 0 || done == total {
                        let fail_count = failed.load(Ordering::Relaxed);
                        eprint!(
                            "\r   [{done}/{total}] {} built, {} failed",
                            done - fail_count,
                            fail_count
                        );
                    }
                    Some((attr, paths))
                },
                _ => {
                    failed.fetch_add(1, Ordering::Relaxed);
                    let fail_count = failed.load(Ordering::Relaxed);
                    if done % 100 == 0 || done == total {
                        eprint!(
                            "\r   [{done}/{total}] {} built, {} failed",
                            done - fail_count,
                            fail_count
                        );
                    }
                    None
                },
            }
        });

        handles.push(handle);
    }

    let mut store_outputs: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for handle in handles {
        if let Ok(Some((attr, paths))) = handle.await {
            if !paths.is_empty() {
                store_outputs.insert(attr, paths);
            }
        }
    }

    eprintln!(); // newline after progress
    store_outputs
}

/// Extract a revision hint from a flake reference.
fn extract_rev(flake_ref: &str) -> String {
    flake_ref
        .rsplit('/')
        .next()
        .unwrap_or(flake_ref)
        .to_string()
}
