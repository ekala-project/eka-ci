//! Package-change classification (A1).
//!
//! Given two jobsets (base + head), this module produces a list of
//! [`PackageChange`] entries by joining `Job.name` (= attr_path) and
//! comparing the corresponding `Drv` rows. The algorithm is documented in
//! `docs/design-package-change-rebuild-impact.md` §7.2.
//!
//! ## Layering
//!
//! - [`compute_package_changes`] is **pure**: it operates on already-loaded
//!   `Vec<JobDrvRow>` slices and is fully covered by unit tests.
//! - [`load_job_drv_rows`] is the thin DB loader that reads `Job ⋈ Drv` for
//!   a single jobset id; it is exercised by the integration tests.
//!
//! Splitting the pure logic from the IO surface keeps the classification
//! testable without spinning up a sqlite pool.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use serde::Deserialize;
use sqlx::{Pool, Sqlite};

use crate::db::model::DrvId;
use crate::dependency_comparison::{pname_from_name, version_from_name};

use super::types::PackageChange;

/// One row of `Job ⋈ Drv` for a single jobset, holding only the columns
/// classify needs. Mapping by hand (rather than reusing the full `Drv`
/// struct) keeps this independent of unrelated `Drv` field churn.
#[derive(Debug, Clone)]
pub struct JobDrvRow {
    pub attr_path: String, // `Job.name`
    pub drv_path: DrvId,
    pub name: String,                       // `Drv.name`-equivalent (last `-` segment of drv_path)
    pub pname: Option<String>,              // `Drv.pname`
    pub version: Option<String>,            // `Drv.version`
    pub license_json: Option<String>,       // `Drv.license_json`
    pub maintainers_json: Option<String>,   // `Drv.maintainers_json`
}

impl JobDrvRow {
    /// Best-effort `pname`: the stored `pname` if present, otherwise a
    /// heuristic strip of the version suffix from the drv name.
    pub fn pname_or_heuristic(&self) -> Option<String> {
        if let Some(p) = self.pname.as_ref() {
            if !p.is_empty() {
                return Some(p.clone());
            }
        }
        // `name` can be empty for highly-unusual drvs — only fall back when
        // we have something to chew on.
        if self.name.is_empty() {
            None
        } else {
            Some(pname_from_name(&self.name))
        }
    }

    /// Best-effort `version`: the stored `version` if present, otherwise
    /// a heuristic extract.
    pub fn version_or_heuristic(&self) -> Option<String> {
        if let Some(v) = self.version.as_ref() {
            if !v.is_empty() {
                return Some(v.clone());
            }
        }
        if self.name.is_empty() {
            None
        } else {
            version_from_name(&self.name)
        }
    }
}

/// Load `(Job.name, Drv)` rows for a single jobset.
///
/// Returns an empty vec if `jobset_id` does not exist (consistent with the
/// "no rows" semantics of the underlying query).
pub async fn load_job_drv_rows(
    pool: &Pool<Sqlite>,
    jobset_id: i64,
) -> anyhow::Result<Vec<JobDrvRow>> {
    let rows: Vec<(String, String, Option<String>, Option<String>, Option<String>, Option<String>)> =
        sqlx::query_as(
            r#"
            SELECT j.name, d.drv_path, d.pname, d.version, d.license_json, d.maintainers_json
            FROM Job j
            INNER JOIN Drv d ON j.drv_id = d.ROWID
            WHERE j.jobset = ?
            "#,
        )
        .bind(jobset_id)
        .fetch_all(pool)
        .await
        .context("Failed to load Job ⋈ Drv rows for classify")?;

    rows.into_iter()
        .map(|(attr_path, drv_path_str, pname, version, license_json, maintainers_json)| {
            let drv_path = DrvId::try_from(drv_path_str.as_str())
                .with_context(|| format!("Invalid drv_path in DB: {drv_path_str}"))?;
            // Derive a name-like field from the drv path tail: store paths
            // are `<hash>-<name>.drv`. This gives us a heuristic fallback
            // when `pname`/`version` columns are NULL.
            let name = drv_name_from_path(&drv_path_str);
            Ok(JobDrvRow {
                attr_path,
                drv_path,
                name,
                pname,
                version,
                license_json,
                maintainers_json,
            })
        })
        .collect()
}

/// Extract the `name` portion (between the leading `<hash>-` and trailing
/// `.drv`) from a drv store path. Returns the input unchanged if the shape
/// doesn't match — heuristic only, never panics.
fn drv_name_from_path(drv_path: &str) -> String {
    // Strip leading `/nix/store/<hash>-` and trailing `.drv`.
    let after_store = drv_path
        .rsplit('/')
        .next()
        .unwrap_or(drv_path);
    let no_drv = after_store.strip_suffix(".drv").unwrap_or(after_store);
    // Drop the leading `<hash>-` if present.
    match no_drv.split_once('-') {
        Some((_hash, rest)) => rest.to_string(),
        None => no_drv.to_string(),
    }
}

/// Pure-logic classification.
///
/// Returns the list of [`PackageChange`] entries comparing `head` to `base`,
/// in deterministic order (sorted by `attr_path`). A single drv may emit
/// multiple entries — see design §7.2.
///
/// `metadata_available` is `true` iff at least one row on either side has a
/// non-empty `pname` or `version` column populated.
pub fn compute_package_changes(
    head: &[JobDrvRow],
    base: &[JobDrvRow],
) -> (Vec<PackageChange>, bool) {
    // Build attr-path-keyed maps. `BTreeMap` so iteration order is stable
    // and the output is deterministic without an explicit sort.
    let head_by_attr: BTreeMap<&str, &JobDrvRow> =
        head.iter().map(|r| (r.attr_path.as_str(), r)).collect();
    let base_by_attr: BTreeMap<&str, &JobDrvRow> =
        base.iter().map(|r| (r.attr_path.as_str(), r)).collect();

    let metadata_available = head
        .iter()
        .chain(base.iter())
        .any(|r| r.pname.is_some() || r.version.is_some());

    let mut out: Vec<PackageChange> = Vec::new();

    // Head-side iteration handles Added / VersionBump / Renamed /
    // RebuildOnly / LicenseChange / MaintainerChange.
    for (attr, head_row) in &head_by_attr {
        match base_by_attr.get(attr) {
            None => {
                // Not in base → Added.
                out.push(PackageChange::Added {
                    attr_path: head_row.attr_path.clone(),
                    pname: head_row.pname_or_heuristic(),
                    version: head_row.version_or_heuristic(),
                    drv_path: head_row.drv_path.clone(),
                });
            }
            Some(base_row) => {
                // Same drv hash → unchanged. Note: equal `DrvId` implies
                // bit-identical drv (Nix-derivation hash includes inputs).
                if head_row.drv_path == base_row.drv_path {
                    continue;
                }
                classify_changed(head_row, base_row, &mut out);
            }
        }
    }

    // Base-side: anything missing from head is Removed.
    for (attr, base_row) in &base_by_attr {
        if !head_by_attr.contains_key(attr) {
            out.push(PackageChange::Removed {
                attr_path: base_row.attr_path.clone(),
                pname: base_row.pname_or_heuristic(),
                version: base_row.version_or_heuristic(),
            });
        }
    }

    (out, metadata_available)
}

/// Classify the `(head, base)` pair for one matched attr_path that has
/// **different** drv_paths. Pushes one *primary* entry (one of
/// `Renamed`/`VersionBump`/`RebuildOnly`) and may push *additional*
/// `LicenseChange`/`MaintainerChange` entries.
fn classify_changed(head: &JobDrvRow, base: &JobDrvRow, out: &mut Vec<PackageChange>) {
    let head_pname = head.pname_or_heuristic();
    let base_pname = base.pname_or_heuristic();
    let head_version = head.version_or_heuristic();
    let base_version = base.version_or_heuristic();

    // Step 1: name change → Renamed.
    let pname_changed = match (&head_pname, &base_pname) {
        (Some(h), Some(b)) => h != b,
        _ => false,
    };

    if pname_changed {
        // SAFETY: `pname_changed` is only true when both are Some.
        out.push(PackageChange::Renamed {
            attr_path: head.attr_path.clone(),
            pname_old: base_pname.clone().unwrap(),
            pname_new: head_pname.clone().unwrap(),
            version: head_version.clone(),
            drv_path: head.drv_path.clone(),
        });
    } else if head_version != base_version
        && head_version.is_some()
        && base_version.is_some()
    {
        // Step 2: version change → VersionBump (requires both versions
        // known; missing version on either side falls through to
        // RebuildOnly to avoid false positives).
        let pname = head_pname.clone().unwrap_or_else(|| {
            // Same-pname check already passed; if we got here without
            // pname, the attr_path is the best label we have.
            head.attr_path.clone()
        });
        out.push(PackageChange::VersionBump {
            attr_path: head.attr_path.clone(),
            pname,
            old: base_version.clone().unwrap(),
            new: head_version.clone().unwrap(),
            drv_path: head.drv_path.clone(),
        });
    } else {
        // Step 3: same name, same version (or version unknown), different
        // hash → RebuildOnly. License/maintainer diffs (next step) are
        // emitted *additionally*, not as a substitute.
        out.push(PackageChange::RebuildOnly {
            attr_path: head.attr_path.clone(),
            pname: head_pname.clone(),
            version: head_version.clone(),
            drv_path: head.drv_path.clone(),
        });
    }

    // Step 4: license diff (only meaningful when pname is known on both
    // sides — anonymous license churn is too noisy to surface).
    if let (Some(pname), false) = (
        head_pname.clone().filter(|_| !pname_changed),
        head.license_json == base.license_json,
    ) {
        if let Some(diff) = diff_license(head.license_json.as_deref(), base.license_json.as_deref())
        {
            let (old, new) = diff;
            out.push(PackageChange::LicenseChange {
                attr_path: head.attr_path.clone(),
                pname,
                old,
                new,
                drv_path: head.drv_path.clone(),
            });
        }
    }

    // Step 5: maintainer diff.
    if let (Some(pname), false) = (
        head_pname.filter(|_| !pname_changed),
        head.maintainers_json == base.maintainers_json,
    ) {
        if let Some((added, removed)) = diff_maintainers(
            head.maintainers_json.as_deref(),
            base.maintainers_json.as_deref(),
        ) {
            out.push(PackageChange::MaintainerChange {
                attr_path: head.attr_path.clone(),
                pname,
                added,
                removed,
                drv_path: head.drv_path.clone(),
            });
        }
    }
}

/// Compute a `(old, new)` pair of license display labels.
///
/// Returns `None` when both sides parse to empty/equivalent labels (the
/// JSON differed only in whitespace/key order, etc.) so we don't emit a
/// spurious `LicenseChange`.
///
/// Display label order of preference per entry: `spdxId` → `shortName` →
/// `fullName` → `"<unknown license>"`.
fn diff_license(head: Option<&str>, base: Option<&str>) -> Option<(Vec<String>, Vec<String>)> {
    let head_labels = parse_license_labels(head);
    let base_labels = parse_license_labels(base);
    if head_labels == base_labels {
        return None;
    }
    Some((base_labels, head_labels))
}

#[derive(Deserialize)]
struct LicenseEntryPartial {
    #[serde(rename = "spdxId", default)]
    spdx_id: Option<String>,
    #[serde(rename = "shortName", default)]
    short_name: Option<String>,
    #[serde(rename = "fullName", default)]
    full_name: Option<String>,
}

fn parse_license_labels(json: Option<&str>) -> Vec<String> {
    let Some(s) = json else {
        return Vec::new();
    };
    let entries: Vec<LicenseEntryPartial> = serde_json::from_str(s).unwrap_or_default();
    let mut labels: Vec<String> = entries
        .into_iter()
        .map(|e| {
            e.spdx_id
                .or(e.short_name)
                .or(e.full_name)
                .unwrap_or_else(|| "<unknown license>".to_string())
        })
        .collect();
    labels.sort();
    labels.dedup();
    labels
}

/// Compute `(added, removed)` lists of maintainer GitHub handles.
///
/// We key on `github` first (most stable identifier); when absent fall back
/// to `name`, then `email`. Returns `None` if both lists collapse to the
/// same set after normalisation.
fn diff_maintainers(
    head: Option<&str>,
    base: Option<&str>,
) -> Option<(Vec<String>, Vec<String>)> {
    let head_set = parse_maintainer_labels(head);
    let base_set = parse_maintainer_labels(base);
    if head_set == base_set {
        return None;
    }
    let added: Vec<String> = head_set.difference(&base_set).cloned().collect();
    let removed: Vec<String> = base_set.difference(&head_set).cloned().collect();
    if added.is_empty() && removed.is_empty() {
        return None;
    }
    let mut a = added;
    let mut r = removed;
    a.sort();
    r.sort();
    Some((a, r))
}

#[derive(Deserialize)]
struct MaintainerPartial {
    #[serde(default)]
    github: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

fn parse_maintainer_labels(json: Option<&str>) -> BTreeSet<String> {
    let Some(s) = json else {
        return BTreeSet::new();
    };
    let entries: Vec<MaintainerPartial> = serde_json::from_str(s).unwrap_or_default();
    entries
        .into_iter()
        .filter_map(|m| m.github.or(m.name).or(m.email))
        .filter(|l| !l.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::model::DrvId;

    /// Build a test row. `attr_path` is also used in the synthetic drv path
    /// to ensure unique drv_paths across rows.
    fn row(
        attr_path: &str,
        hash: &str,
        name: &str,
        pname: Option<&str>,
        version: Option<&str>,
        license_json: Option<&str>,
        maintainers_json: Option<&str>,
    ) -> JobDrvRow {
        let drv_str = format!("{hash}-{name}.drv");
        JobDrvRow {
            attr_path: attr_path.to_string(),
            drv_path: DrvId::try_from(drv_str.as_str()).unwrap(),
            name: name.to_string(),
            pname: pname.map(str::to_string),
            version: version.map(str::to_string),
            license_json: license_json.map(str::to_string),
            maintainers_json: maintainers_json.map(str::to_string),
        }
    }

    /// 32-char store hashes used to keep `DrvId::try_from` happy.
    const H1: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1";
    const H2: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb2";

    #[test]
    fn drv_name_from_path_extracts_name() {
        assert_eq!(
            drv_name_from_path("/nix/store/abcdef-hello-2.12.1.drv"),
            "hello-2.12.1"
        );
        assert_eq!(
            drv_name_from_path("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1-hello-2.12.1.drv"),
            "hello-2.12.1"
        );
        // Unrecognised shape — passthrough.
        assert_eq!(drv_name_from_path("weird"), "weird");
    }

    #[test]
    fn classify_added_and_removed() {
        let head = vec![row("hello", H1, "hello-2.12", Some("hello"), Some("2.12"), None, None)];
        let base = vec![row(
            "goodbye",
            H2,
            "goodbye-1.0",
            Some("goodbye"),
            Some("1.0"),
            None,
            None,
        )];
        let (changes, _) = compute_package_changes(&head, &base);
        assert_eq!(changes.len(), 2);
        assert!(matches!(changes[0], PackageChange::Added { .. }));
        assert!(matches!(changes[1], PackageChange::Removed { .. }));
    }

    #[test]
    fn classify_unchanged_omitted() {
        let r = row("hello", H1, "hello-2.12", Some("hello"), Some("2.12"), None, None);
        let head = vec![r.clone()];
        let base = vec![r];
        let (changes, _) = compute_package_changes(&head, &base);
        assert!(changes.is_empty());
    }

    #[test]
    fn classify_version_bump() {
        let head = vec![row(
            "hello",
            H1,
            "hello-2.13",
            Some("hello"),
            Some("2.13"),
            None,
            None,
        )];
        let base = vec![row(
            "hello",
            H2,
            "hello-2.12",
            Some("hello"),
            Some("2.12"),
            None,
            None,
        )];
        let (changes, _) = compute_package_changes(&head, &base);
        assert_eq!(changes.len(), 1);
        match &changes[0] {
            PackageChange::VersionBump { pname, old, new, .. } => {
                assert_eq!(pname, "hello");
                assert_eq!(old, "2.12");
                assert_eq!(new, "2.13");
            }
            other => panic!("expected VersionBump, got {other:?}"),
        }
    }

    #[test]
    fn classify_renamed() {
        let head = vec![row(
            "greet",
            H1,
            "hi-1.0",
            Some("hi"),
            Some("1.0"),
            None,
            None,
        )];
        let base = vec![row(
            "greet",
            H2,
            "hello-1.0",
            Some("hello"),
            Some("1.0"),
            None,
            None,
        )];
        let (changes, _) = compute_package_changes(&head, &base);
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0], PackageChange::Renamed { .. }));
    }

    #[test]
    fn classify_rebuild_only_when_hash_changes_but_name_version_match() {
        let head = vec![row(
            "hello",
            H1,
            "hello-2.12",
            Some("hello"),
            Some("2.12"),
            None,
            None,
        )];
        let base = vec![row(
            "hello",
            H2,
            "hello-2.12",
            Some("hello"),
            Some("2.12"),
            None,
            None,
        )];
        let (changes, _) = compute_package_changes(&head, &base);
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0], PackageChange::RebuildOnly { .. }));
    }

    #[test]
    fn classify_license_change_emitted_alongside_rebuild_only() {
        let head = vec![row(
            "hello",
            H1,
            "hello-2.12",
            Some("hello"),
            Some("2.12"),
            Some(r#"[{"spdxId":"MIT"}]"#),
            None,
        )];
        let base = vec![row(
            "hello",
            H2,
            "hello-2.12",
            Some("hello"),
            Some("2.12"),
            Some(r#"[{"spdxId":"GPL-3.0-only"}]"#),
            None,
        )];
        let (changes, _) = compute_package_changes(&head, &base);
        assert_eq!(changes.len(), 2);
        assert!(matches!(changes[0], PackageChange::RebuildOnly { .. }));
        match &changes[1] {
            PackageChange::LicenseChange { old, new, pname, .. } => {
                assert_eq!(pname, "hello");
                assert_eq!(old, &vec!["GPL-3.0-only".to_string()]);
                assert_eq!(new, &vec!["MIT".to_string()]);
            }
            other => panic!("expected LicenseChange, got {other:?}"),
        }
    }

    #[test]
    fn classify_license_unchanged_when_only_key_order_differs() {
        // Same labels (spdxId), same set; should produce only RebuildOnly.
        let head = vec![row(
            "hello",
            H1,
            "hello-2.12",
            Some("hello"),
            Some("2.12"),
            Some(r#"[{"spdxId":"MIT","shortName":"mit"}]"#),
            None,
        )];
        let base = vec![row(
            "hello",
            H2,
            "hello-2.12",
            Some("hello"),
            Some("2.12"),
            Some(r#"[{"shortName":"mit","spdxId":"MIT"}]"#),
            None,
        )];
        let (changes, _) = compute_package_changes(&head, &base);
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0], PackageChange::RebuildOnly { .. }));
    }

    #[test]
    fn classify_maintainer_change() {
        let head = vec![row(
            "hello",
            H1,
            "hello-2.12",
            Some("hello"),
            Some("2.12"),
            None,
            Some(r#"[{"github":"alice"},{"github":"bob"}]"#),
        )];
        let base = vec![row(
            "hello",
            H2,
            "hello-2.12",
            Some("hello"),
            Some("2.12"),
            None,
            Some(r#"[{"github":"alice"},{"github":"carol"}]"#),
        )];
        let (changes, _) = compute_package_changes(&head, &base);
        // RebuildOnly + MaintainerChange
        assert_eq!(changes.len(), 2);
        match &changes[1] {
            PackageChange::MaintainerChange { added, removed, .. } => {
                assert_eq!(added, &vec!["bob".to_string()]);
                assert_eq!(removed, &vec!["carol".to_string()]);
            }
            other => panic!("expected MaintainerChange, got {other:?}"),
        }
    }

    #[test]
    fn classify_renamed_suppresses_license_and_maintainer() {
        // Renaming changes everything; we only emit Renamed, not the
        // ancillary license/maintainer entries (those would be noise).
        let head = vec![row(
            "greet",
            H1,
            "hi-1.0",
            Some("hi"),
            Some("1.0"),
            Some(r#"[{"spdxId":"MIT"}]"#),
            Some(r#"[{"github":"alice"}]"#),
        )];
        let base = vec![row(
            "greet",
            H2,
            "hello-1.0",
            Some("hello"),
            Some("1.0"),
            Some(r#"[{"spdxId":"GPL-3.0-only"}]"#),
            Some(r#"[{"github":"bob"}]"#),
        )];
        let (changes, _) = compute_package_changes(&head, &base);
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0], PackageChange::Renamed { .. }));
    }

    #[test]
    fn classify_metadata_available_flag() {
        // No pname/version anywhere → false.
        let head = vec![row("a", H1, "a-1", None, None, None, None)];
        let base = vec![row("a", H2, "a-1", None, None, None, None)];
        let (_, available) = compute_package_changes(&head, &base);
        assert!(!available);

        // At least one populated → true.
        let head = vec![row("a", H1, "a-1", Some("a"), None, None, None)];
        let base = vec![row("a", H2, "a-1", None, None, None, None)];
        let (_, available) = compute_package_changes(&head, &base);
        assert!(available);
    }

    #[test]
    fn classify_heuristic_pname_when_columns_null() {
        // Both rows have pname=NULL but `name` is "hello-2.12" / "hello-2.13"
        // → heuristic should yield same pname "hello", different versions.
        let head = vec![row("hello", H1, "hello-2.13", None, None, None, None)];
        let base = vec![row("hello", H2, "hello-2.12", None, None, None, None)];
        let (changes, available) = compute_package_changes(&head, &base);
        assert!(!available); // metadata still considered unavailable
        assert_eq!(changes.len(), 1);
        match &changes[0] {
            PackageChange::VersionBump { pname, old, new, .. } => {
                assert_eq!(pname, "hello");
                assert_eq!(old, "2.12");
                assert_eq!(new, "2.13");
            }
            other => panic!("expected heuristic VersionBump, got {other:?}"),
        }
    }

    #[test]
    fn classify_deterministic_attr_order() {
        let head = vec![
            row("zeta", H1, "z-1", Some("z"), Some("1"), None, None),
            row("alpha", H2, "a-1", Some("a"), Some("1"), None, None),
        ];
        let base: Vec<JobDrvRow> = vec![];
        let (changes, _) = compute_package_changes(&head, &base);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].attr_path(), "alpha");
        assert_eq!(changes[1].attr_path(), "zeta");
    }
}
