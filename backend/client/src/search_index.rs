use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use colored::Colorize;
use search_index::generators;
use search_index::types::Manifest;

pub async fn handle_generate_search_index(
    flake_ref: String,
    system: String,
    channel: String,
    output: PathBuf,
) -> Result<()> {
    println!("{} Generating search index...", "->".blue());
    println!("   Flake ref: {}", flake_ref);
    println!("   System:    {}", system);
    println!("   Channel:   {}", channel);
    println!("   Output:    {}", output.display());
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

    // Build manifest.
    let nixpkgs_rev = extract_rev(&flake_ref);
    let manifest = Manifest {
        generated_at: chrono::Utc::now().to_rfc3339(),
        channel_name: channel,
        nixpkgs_rev,
        package_count: packages.len(),
        file_count: 0,
        option_count: 0,
    };

    // Build SQLite database directly to the output path.
    println!("{} Building SQLite database...", "->".blue());
    let out = output.clone();
    let m = manifest.clone();
    let db_size = tokio::task::spawn_blocking(move || -> Result<u64> {
        generators::sqlite::build_database_to_path(&out, &packages, &[], &[], &m)?;
        let meta = std::fs::metadata(&out).context("failed to read output file metadata")?;
        Ok(meta.len())
    })
    .await
    .context("database build task panicked")??;

    let elapsed = start.elapsed();

    println!();
    println!("{}", "Summary:".bold());
    println!("   Packages:  {}", manifest.package_count);
    println!(
        "   File size: {} bytes ({:.1} MiB)",
        db_size,
        db_size as f64 / 1_048_576.0
    );
    println!("   Output:    {}", output.display());
    println!("   Time:      {:.2}s", elapsed.as_secs_f64());

    Ok(())
}

/// Extract a revision hint from a flake reference.
///
/// For `github:owner/repo/rev`, returns `rev`.
/// For anything else, returns the full flake ref.
fn extract_rev(flake_ref: &str) -> String {
    flake_ref
        .rsplit('/')
        .next()
        .unwrap_or(flake_ref)
        .to_string()
}
