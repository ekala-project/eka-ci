use std::collections::{HashMap, HashSet};

use anyhow::Result;
use sqlx::SqlitePool;

/// Extract pname from a store path (output path, not drv path)
///
/// Examples:
/// - "/nix/store/abc123-hello-2.12.1" -> "hello"
/// - "/nix/store/abc123-python3.12-setuptools-69.0.0" -> "python3.12-setuptools"
/// - "/nix/store/abc123-source" -> "source"
///
/// This is a heuristic: we try to strip common version patterns from the end.
/// It's not perfect but should work for most packages.
fn extract_pname(store_path: &str) -> String {
    // Store path format: "/nix/store/hash-name" or just "hash-name"
    // Get the name part (after hash)
    let name_part = if let Some(stripped) = store_path.strip_prefix("/nix/store/") {
        stripped
    } else {
        store_path
    };

    // Skip the hash (32 chars) and the dash
    if name_part.len() <= 33 {
        return name_part.to_string();
    }
    let name = &name_part[33..];

    // Try to strip version patterns from the end
    // Common patterns: -1.2.3, -1.2.3.4, -20240101, -v1.2.3, -r1, etc.
    // Strategy: split by '-' and remove trailing segments that look like versions
    let parts: Vec<&str> = name.split('-').collect();

    if parts.len() == 1 {
        // No dashes, just return as-is (e.g., "source")
        return name.to_string();
    }

    // Find the last part that doesn't look like a version
    let mut keep_parts = parts.len();

    for (i, part) in parts.iter().enumerate().rev() {
        if looks_like_version(part) {
            keep_parts = i;
        } else {
            break;
        }
    }

    if keep_parts == 0 {
        // All parts look like versions? Keep everything (defensive)
        name.to_string()
    } else {
        parts[..keep_parts].join("-")
    }
}

/// Check if a string segment looks like a version identifier
fn looks_like_version(s: &str) -> bool {
    // Empty or very short strings are not versions
    if s.len() < 2 {
        return false;
    }

    // Starts with 'v' or 'r' followed by digit (v1.2.3, r1, etc.)
    if s.len() > 1 {
        let first_char = s.chars().next().unwrap();
        let second_char = s.chars().nth(1).unwrap();
        if (first_char == 'v' || first_char == 'r') && second_char.is_ascii_digit() {
            return true;
        }
    }

    // Contains only digits and dots (1.2.3, 2024.01.01, etc.)
    let has_digit = s.chars().any(|c| c.is_ascii_digit());
    let all_digits_and_dots = s.chars().all(|c| c.is_ascii_digit() || c == '.');

    if has_digit && all_digits_and_dots {
        return true;
    }

    // Starts with digit (common for versions like "3.12", "2024")
    if s.chars().next().unwrap().is_ascii_digit() {
        return true;
    }

    false
}

/// Result of comparing runtime dependencies between two derivation outputs
#[derive(Debug, Clone)]
pub struct DependencyComparison {
    pub attr_path: String,
    pub output_name: String, // Output name: "out", "dev", "doc", etc.
    pub base_drv_path: String,
    pub head_drv_path: String,
    pub base_dep_count: usize,
    pub head_dep_count: usize,
    pub added_deps: Vec<String>,   // Store paths
    pub removed_deps: Vec<String>, // Store paths
}

/// Compare runtime references per output for all packages in a jobset
///
/// This function:
/// 1. Gets all attr_paths in the head jobset
/// 2. For each attr_path that exists in both base and head:
///    - Gets all outputs for both derivations
///    - For each output that exists in both:
///      - Queries runtime references from database
///      - Compares the sets
///      - Filters out dependencies that were just version-bumped (same pname)
/// 3. Only returns output comparisons where true runtime dependencies changed
pub async fn compare_runtime_references_for_jobset(
    base_jobset_id: i64,
    head_jobset_id: i64,
    pool: &SqlitePool,
) -> Result<Vec<DependencyComparison>> {
    // Get all jobs (attr_path -> drv_path mapping) from head jobset
    let head_jobs: Vec<(String, String)> = sqlx::query_as(
        r#"
        SELECT j.name, d.drv_path
        FROM Job j
        JOIN Drv d ON d.ROWID = j.drv_id
        WHERE j.jobset = ?
        "#,
    )
    .bind(head_jobset_id)
    .fetch_all(pool)
    .await?;

    let mut comparisons = Vec::new();

    for (attr_path, head_drv_path) in head_jobs {
        // Get base drv ROWID and drv_path for same attr_path
        let base_drv: Option<(i64, String)> = sqlx::query_as(
            r#"
            SELECT d.ROWID, d.drv_path
            FROM Job j
            JOIN Drv d ON d.ROWID = j.drv_id
            WHERE j.jobset = ? AND j.name = ?
            "#,
        )
        .bind(base_jobset_id)
        .bind(&attr_path)
        .fetch_optional(pool)
        .await?;

        let Some((base_drv_id, base_drv_path)) = base_drv else {
            // New package, skip comparison (no baseline)
            continue;
        };

        // Get head drv ROWID
        let head_drv_id: Option<i64> =
            sqlx::query_scalar("SELECT ROWID FROM Drv WHERE drv_path = ?")
                .bind(&head_drv_path)
                .fetch_optional(pool)
                .await?;

        let Some(head_drv_id) = head_drv_id else {
            // Can't find head drv in database
            continue;
        };

        // Get output paths for both base and head
        let base_outputs = crate::db::runtime_refs::get_output_paths(pool, base_drv_id).await?;

        let head_outputs = crate::db::runtime_refs::get_output_paths(pool, head_drv_id).await?;

        // Skip if either has no outputs recorded
        if base_outputs.is_empty() || head_outputs.is_empty() {
            continue;
        }

        // Compare each output that exists in both base and head
        for (output_name, _head_output_path) in &head_outputs {
            // Skip if this output doesn't exist in base
            if !base_outputs.contains_key(output_name) {
                continue;
            }

            // Get runtime references for this output using drv_id
            let base_refs =
                crate::db::runtime_refs::get_runtime_references(pool, base_drv_id, output_name)
                    .await?;

            let head_refs =
                crate::db::runtime_refs::get_runtime_references(pool, head_drv_id, output_name)
                    .await?;

            // Skip if we don't have data for either commit
            if base_refs.is_empty() || head_refs.is_empty() {
                continue;
            }

            // Quick check: if counts are the same, likely no real change
            if base_refs.len() == head_refs.len() {
                continue;
            }

            // Convert to sets for diff computation
            let base_set: HashSet<_> = base_refs.into_iter().collect();
            let head_set: HashSet<_> = head_refs.into_iter().collect();

            let added_raw: Vec<_> = head_set.difference(&base_set).cloned().collect();
            let removed_raw: Vec<_> = base_set.difference(&head_set).cloned().collect();

            // Filter out version changes: if a dep appears in both added and removed
            // with the same pname, it's just a version bump
            let (added_filtered, removed_filtered) = filter_version_changes(added_raw, removed_raw);

            // Only include if there are real dependency changes after filtering
            if !added_filtered.is_empty() || !removed_filtered.is_empty() {
                comparisons.push(DependencyComparison {
                    attr_path: attr_path.clone(),
                    output_name: output_name.clone(),
                    base_drv_path: base_drv_path.clone(),
                    head_drv_path: head_drv_path.clone(),
                    base_dep_count: base_set.len(),
                    head_dep_count: head_set.len(),
                    added_deps: added_filtered,
                    removed_deps: removed_filtered,
                });
            }
        }
    }

    Ok(comparisons)
}

/// Filter out dependencies that are just version changes
///
/// If the same pname appears in both added and removed lists,
/// it's a version bump and should be filtered out.
///
/// Returns: (truly_added, truly_removed)
fn filter_version_changes(added: Vec<String>, removed: Vec<String>) -> (Vec<String>, Vec<String>) {
    // Build pname -> store_path mappings
    let mut added_by_pname: HashMap<String, String> = HashMap::new();
    let mut removed_by_pname: HashMap<String, String> = HashMap::new();

    for store_path in &added {
        let pname = extract_pname(store_path);
        added_by_pname.insert(pname, store_path.clone());
    }

    for store_path in &removed {
        let pname = extract_pname(store_path);
        removed_by_pname.insert(pname, store_path.clone());
    }

    // Find pnames that appear in both (version changes)
    let version_changed_pnames: HashSet<_> = added_by_pname
        .keys()
        .filter(|pname| removed_by_pname.contains_key(*pname))
        .cloned()
        .collect();

    // Filter out version-changed pnames from both lists
    let truly_added: Vec<String> = added
        .into_iter()
        .filter(|store_path| {
            let pname = extract_pname(store_path);
            !version_changed_pnames.contains(&pname)
        })
        .collect();

    let truly_removed: Vec<String> = removed
        .into_iter()
        .filter(|store_path| {
            let pname = extract_pname(store_path);
            !version_changed_pnames.contains(&pname)
        })
        .collect();

    (truly_added, truly_removed)
}

/// Format dependency changes as a diff-style string for display in GitHub check runs
///
/// Output format uses `attr_path!output_name` convention (aligned with Nix semantics):
/// ```text
/// ## python3.pkgs.requests!out
/// + /nix/store/abc123-pysocks-1.7.1
/// - /nix/store/def456-flask-2.3.0
///
/// ## nixpkgs.chromium!dev
/// + /nix/store/ghi789-libxml2-2.10.0
/// ```
pub fn format_dependency_changes_as_diff(comparisons: &[DependencyComparison]) -> String {
    if comparisons.is_empty() {
        return "No dependency changes detected.".to_string();
    }

    let mut output = String::new();

    for comparison in comparisons {
        // Package header with output name (attr_path!output_name format)
        output.push_str(&format!(
            "## {}!{}\n",
            comparison.attr_path, comparison.output_name
        ));
        output.push_str(&format!(
            "*Runtime dependencies: {} → {} ({}{})*\n\n",
            comparison.base_dep_count,
            comparison.head_dep_count,
            if comparison.head_dep_count > comparison.base_dep_count {
                "+"
            } else {
                ""
            },
            comparison.head_dep_count as i64 - comparison.base_dep_count as i64
        ));

        // Added dependencies
        for dep in &comparison.added_deps {
            output.push_str(&format!("+ {}\n", dep));
        }

        // Removed dependencies
        for dep in &comparison.removed_deps {
            output.push_str(&format!("- {}\n", dep));
        }

        output.push('\n');
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_pname() {
        // Standard packages with versions
        assert_eq!(
            extract_pname("/nix/store/jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1"),
            "hello"
        );

        // Python packages
        assert_eq!(
            extract_pname(
                "/nix/store/abc123def456abc123def456abc123de-python3.12-setuptools-69.0.0"
            ),
            "python3.12-setuptools"
        );

        // Source derivations
        assert_eq!(
            extract_pname("/nix/store/0aykaqxhbby7mx7lgb217m9b3gkl52fn-source"),
            "source"
        );

        // Packages with date versions
        assert_eq!(
            extract_pname(
                "/nix/store/abc123def456abc123def456abc123de-nixpkgs-unstable-2024.01.01"
            ),
            "nixpkgs-unstable"
        );

        // Packages with 'v' prefix
        assert_eq!(
            extract_pname("/nix/store/abc123def456abc123def456abc123de-rust-analyzer-v0.3.1234"),
            "rust-analyzer"
        );
    }

    #[test]
    fn test_looks_like_version() {
        assert!(looks_like_version("1.2.3"));
        assert!(looks_like_version("2024.01.01"));
        assert!(looks_like_version("v1.2.3"));
        assert!(looks_like_version("r1"));
        assert!(looks_like_version("69.0.0"));
        assert!(looks_like_version("3.12"));

        assert!(!looks_like_version("hello"));
        assert!(!looks_like_version("python3"));
        assert!(!looks_like_version("setuptools"));
        assert!(!looks_like_version("x"));
        assert!(!looks_like_version(""));
    }

    #[test]
    fn test_filter_version_changes() {
        let added = vec![
            "/nix/store/abc123def456abc123def456abc123de-urllib3-2.0.0".to_string(),
            "/nix/store/def456abc123def456abc123def456ab-certifi-2024.1.0".to_string(),
        ];

        let removed = vec![
            "/nix/store/123456789012345678901234567890ab-urllib3-1.26.0".to_string(),
            "/nix/store/234567890123456789012345678901bc-flask-2.3.0".to_string(),
        ];

        let (truly_added, truly_removed) = filter_version_changes(added, removed);

        // urllib3 appears in both (version change) - should be filtered
        // certifi only in added - should remain
        // flask only in removed - should remain
        assert_eq!(truly_added.len(), 1);
        assert!(truly_added[0].contains("certifi"));

        assert_eq!(truly_removed.len(), 1);
        assert!(truly_removed[0].contains("flask"));
    }
}
