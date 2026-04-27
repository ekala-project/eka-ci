//! Pure markdown rendering for the package-change + rebuild-impact summary.
//! Drops content in priority order to fit a 60,000-byte working budget below
//! GitHub's 65,535-char `output.summary` ceiling. Output is deterministic
//! (alphabetic system order, lexicographic drv_path tie-break, declaration
//! order for change kinds) — snapshot tests rely on that.

use std::fmt::Write;

use super::types::{
    ChangeSummary, ChangeSummaryRebuildImpact, PackageChange, PerSystemImpact, TopBlastRadiusEntry,
};

/// Working budget below GitHub's hard ceiling, with a 5,535-byte cushion for re-render drift.
pub const GITHUB_CHECK_SUMMARY_SOFT_LIMIT: usize = 60_000;

/// Hard ceiling enforced by the GitHub Checks API on `output.summary`.
pub const GITHUB_CHECK_SUMMARY_HARD_LIMIT: usize = 65_535;

/// What the renderer dropped, reported back so callers can log/surface it.
/// Drop priority: maintainers → license → rebuild-only → collapse-to-counts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenderTruncation {
    /// Maintainer-change rows omitted from the package table.
    pub dropped_maintainers: bool,
    /// License-change rows omitted from the package table.
    pub dropped_license: bool,
    /// The rebuild-only count line was dropped.
    pub dropped_rebuild_only: bool,
    /// The package table was collapsed to a "Summary: N bumps, …" line.
    pub collapsed_to_counts: bool,
}

impl RenderTruncation {
    /// Did any drop step fire? Useful for the renderer's footer banner.
    pub fn any(&self) -> bool {
        self.dropped_maintainers
            || self.dropped_license
            || self.dropped_rebuild_only
            || self.collapsed_to_counts
    }
}

/// Caller-supplied renderer knobs.
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Include the "_N packages will rebuild without source changes._" line.
    /// Truncation may still drop it if the rendered output is too large.
    pub include_rebuild_only: bool,
    /// Config parse error message to display in a banner. When `Some`, a warning
    /// banner is prepended to the rendered output.
    pub config_load_error: Option<String>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            include_rebuild_only: true,
            config_load_error: None,
        }
    }
}

/// Render a [`ChangeSummary`] to GitHub-flavoured Markdown, applying
/// truncation as needed to stay within
/// [`GITHUB_CHECK_SUMMARY_SOFT_LIMIT`].
///
/// Returns the rendered markdown plus a [`RenderTruncation`] describing
/// what was dropped (so callers can surface "(truncated)" in the API
/// response or in observability metrics).
///
/// The full-fidelity API path (`GET /v1/commits/{sha}/change-summary`)
/// is always returned untruncated by the orchestrator — this function
/// only governs what fits into a single GitHub check posting.
pub fn render(summary: &ChangeSummary, options: &RenderOptions) -> (String, RenderTruncation) {
    // Strategy: render the most ambitious version first; if it overflows,
    // drop columns in priority order until it fits, then collapse if
    // still too large. We cap at four passes (drop maintainers, drop
    // license, drop rebuild-only, collapse) so the worst case is O(4 *
    // markdown_size), trivially bounded.

    let mut truncation = RenderTruncation::default();

    let mut config = RenderConfig::default();
    // Caller asked us to suppress the rebuild-only count line up-front.
    config.include_rebuild_only_line = options.include_rebuild_only;
    let initial = render_with_config(summary, &config, &truncation, options);
    if initial.len() <= GITHUB_CHECK_SUMMARY_SOFT_LIMIT {
        return (initial, truncation);
    }

    // Pass 1: drop maintainers
    config.include_maintainers = false;
    truncation.dropped_maintainers = true;
    let pass1 = render_with_config(summary, &config, &truncation, options);
    if pass1.len() <= GITHUB_CHECK_SUMMARY_SOFT_LIMIT {
        return (pass1, truncation);
    }

    // Pass 2: also drop license
    config.include_license = false;
    truncation.dropped_license = true;
    let pass2 = render_with_config(summary, &config, &truncation, options);
    if pass2.len() <= GITHUB_CHECK_SUMMARY_SOFT_LIMIT {
        return (pass2, truncation);
    }

    // Pass 3: also drop rebuild-only line. Only mark as a truncation drop
    // if the line was actually on — otherwise the footer would lie.
    if config.include_rebuild_only_line {
        config.include_rebuild_only_line = false;
        truncation.dropped_rebuild_only = true;
    }
    let pass3 = render_with_config(summary, &config, &truncation, options);
    if pass3.len() <= GITHUB_CHECK_SUMMARY_SOFT_LIMIT {
        return (pass3, truncation);
    }

    // Pass 4: collapse table to counts.
    config.collapse_table = true;
    truncation.collapsed_to_counts = true;
    let pass4 = render_with_config(summary, &config, &truncation, options);

    // We always return *something* — even pass4 is bounded (a constant
    // "Summary: N bumps, M added, …" header + impact table). If somehow
    // the per-system table alone overflows the hard limit, hard-truncate
    // with a UTF-8-safe slice rather than violating the API ceiling.
    let final_str = if pass4.len() <= GITHUB_CHECK_SUMMARY_HARD_LIMIT {
        pass4
    } else {
        hard_truncate(&pass4, GITHUB_CHECK_SUMMARY_HARD_LIMIT)
    };

    (final_str, truncation)
}

/// Truncate `s` to at most `limit` bytes, preserving UTF-8 by walking
/// back to the nearest char boundary. Appends a clear marker.
fn hard_truncate(s: &str, limit: usize) -> String {
    const MARKER: &str = "\n\n_…(truncated, full payload via API)_\n";
    if s.len() <= limit {
        return s.to_string();
    }
    let budget = limit.saturating_sub(MARKER.len());
    let mut cut = budget.min(s.len());
    while !s.is_char_boundary(cut) && cut > 0 {
        cut -= 1;
    }
    let mut out = String::with_capacity(limit);
    out.push_str(&s[..cut]);
    out.push_str(MARKER);
    out
}

#[derive(Debug, Clone)]
struct RenderConfig {
    include_maintainers: bool,
    include_license: bool,
    include_rebuild_only_line: bool,
    collapse_table: bool,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            include_maintainers: true,
            include_license: true,
            include_rebuild_only_line: true,
            collapse_table: false,
        }
    }
}

fn render_with_config(
    summary: &ChangeSummary,
    config: &RenderConfig,
    truncation: &RenderTruncation,
    options: &RenderOptions,
) -> String {
    let mut out = String::with_capacity(2048);

    // Config parse error banner at the very top (if present)
    if let Some(ref error) = options.config_load_error {
        // Truncate to ~200 chars to stay within GitHub check budget
        const MAX_ERROR_LEN: usize = 200;
        let truncated_error = if error.len() > MAX_ERROR_LEN {
            format!("{}…", &error[..MAX_ERROR_LEN])
        } else {
            error.clone()
        };
        let _ = writeln!(
            out,
            "> ⚠ `.ekaci/config.json` failed to parse — using defaults. Error: {}\n",
            truncated_error
        );
    }

    let head = short_sha(&summary.head_sha);
    let base = short_sha(&summary.base_sha);
    let _ = writeln!(out, "## Change summary for `{head}` ← `{base}`");
    out.push('\n');

    if !summary.metadata_available {
        out.push_str(
            "> ⚠ Package metadata unavailable for this commit (eval failed); package names below \
             are best-effort heuristics from drv paths.\n\n",
        );
    }

    render_packages_section(&mut out, summary, config);
    out.push('\n');
    render_impact_section(&mut out, &summary.rebuild_impact);

    if truncation.any() {
        out.push('\n');
        render_truncation_footer(&mut out, truncation, &summary.head_sha);
    }

    out
}

fn render_packages_section(out: &mut String, summary: &ChangeSummary, config: &RenderConfig) {
    let counts = ChangeCounts::from_changes(&summary.package_changes);

    let visible_count = counts.total_visible(config);
    let _ = writeln!(out, "### Packages changed ({visible_count})");
    out.push('\n');

    if config.collapse_table {
        let _ = writeln!(
            out,
            "**Summary:** {summary_line}",
            summary_line = counts.summary_line()
        );
        if config.include_rebuild_only_line && counts.rebuild_only > 0 {
            let _ = writeln!(
                out,
                "\n_{n} packages will rebuild without source changes._",
                n = counts.rebuild_only
            );
        }
        return;
    }

    let _ = writeln!(out, "| Status | Package | Change |");
    let _ = writeln!(out, "|--------|---------|--------|");

    for change in &summary.package_changes {
        match change {
            PackageChange::RebuildOnly { .. } => {
                // RebuildOnly is collapsed to a count line below by default.
            },
            PackageChange::LicenseChange { .. } if !config.include_license => {},
            PackageChange::MaintainerChange { .. } if !config.include_maintainers => {},
            _ => {
                let row = render_package_row(change);
                out.push_str(&row);
                out.push('\n');
            },
        }
    }

    if config.include_rebuild_only_line && counts.rebuild_only > 0 {
        out.push('\n');
        let _ = writeln!(
            out,
            "**Plus {n} package{plural} will rebuild without source changes.**",
            n = counts.rebuild_only,
            plural = if counts.rebuild_only == 1 { "" } else { "s" }
        );
    }
}

/// Render one row of the packages-changed table (no trailing newline).
fn render_package_row(change: &PackageChange) -> String {
    match change {
        PackageChange::Added {
            attr_path,
            pname,
            version,
            ..
        } => format!(
            "| Added | `{pkg}` | {ver} |",
            pkg = pname.as_deref().unwrap_or(attr_path.as_str()),
            ver = version.as_deref().unwrap_or("(no version)"),
        ),
        PackageChange::Removed {
            attr_path,
            pname,
            version,
        } => format!(
            "| Removed | `{pkg}` | (was {ver}) |",
            pkg = pname.as_deref().unwrap_or(attr_path.as_str()),
            ver = version.as_deref().unwrap_or("?"),
        ),
        PackageChange::VersionBump {
            pname, old, new, ..
        } => format!("| Bump | `{pname}` | {old} → {new} |"),
        PackageChange::Renamed {
            pname_old,
            pname_new,
            version,
            ..
        } => format!(
            "| Renamed | `{old} → {new}` | {ver} |",
            old = pname_old,
            new = pname_new,
            ver = version.as_deref().unwrap_or("?"),
        ),
        PackageChange::RebuildOnly { .. } => {
            // Collapsed to count line — never row-rendered.
            String::new()
        },
        PackageChange::LicenseChange {
            pname, old, new, ..
        } => format!(
            "| License | `{pname}` | {old} → {new} |",
            old = format_str_list(old),
            new = format_str_list(new),
        ),
        PackageChange::MaintainerChange {
            pname,
            added,
            removed,
            ..
        } => format!(
            "| Maintainers | `{pname}` | {diff} |",
            diff = format_maintainer_diff(added, removed),
        ),
    }
}

fn format_str_list(items: &[String]) -> String {
    if items.is_empty() {
        "(none)".to_string()
    } else {
        items.join(", ")
    }
}

fn format_maintainer_diff(added: &[String], removed: &[String]) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(added.len() + removed.len());
    for a in added {
        parts.push(format!("+{a}"));
    }
    for r in removed {
        parts.push(format!("-{r}"));
    }
    if parts.is_empty() {
        "(no change)".to_string()
    } else {
        parts.join(", ")
    }
}

fn render_impact_section(out: &mut String, impact: &ChangeSummaryRebuildImpact) {
    out.push_str("### Rebuild impact\n\n");

    if impact.per_system.is_empty() {
        out.push_str("_No rebuild impact computed for this commit._\n");
        return;
    }

    let _ = writeln!(out, "| System | Rebuilds | Top blast radius |");
    let _ = writeln!(out, "|--------|----------|------------------|");
    for sys in &impact.per_system {
        let _ = writeln!(
            out,
            "| `{system}` | {count} | {top} |",
            system = sys.system,
            count = sys.rebuild_count,
            top = render_top_blast_radius(sys),
        );
    }

    out.push('\n');
    let _ = writeln!(
        out,
        "**Total unique drvs that would rebuild somewhere:** {total}",
        total = impact.total_unique_drvs
    );
}

fn render_top_blast_radius(sys: &PerSystemImpact) -> String {
    if sys.top_blast_radius.is_empty() {
        return "_(none)_".to_string();
    }
    sys.top_blast_radius
        .iter()
        .map(|entry| format_blast_entry(entry))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_blast_entry(entry: &TopBlastRadiusEntry) -> String {
    let label = entry
        .pname
        .clone()
        .unwrap_or_else(|| extract_pname_from_drv(entry.drv_path.as_ref()));
    format!("`{label}` ({n})", label = label, n = entry.blast_radius)
}

/// Best-effort label extraction from a `/nix/store/<hash>-<name>.drv`
/// path when the row didn't carry an explicit pname. Strips the store
/// prefix, hash, and `.drv` suffix; returns the original string on any
/// shape mismatch so we never panic on rendering.
fn extract_pname_from_drv(drv_path: &str) -> String {
    let stem = drv_path
        .rsplit('/')
        .next()
        .unwrap_or(drv_path)
        .trim_end_matches(".drv");
    // Skip the leading "<hash>-" prefix.
    match stem.split_once('-') {
        Some((_hash, rest)) if !rest.is_empty() => rest.to_string(),
        _ => stem.to_string(),
    }
}

fn render_truncation_footer(out: &mut String, truncation: &RenderTruncation, head_sha: &str) {
    out.push_str("---\n\n");
    out.push_str("_⚠ Output truncated to fit GitHub's check-summary limit._");
    let mut dropped: Vec<&'static str> = Vec::new();
    if truncation.dropped_maintainers {
        dropped.push("maintainer changes");
    }
    if truncation.dropped_license {
        dropped.push("license changes");
    }
    if truncation.dropped_rebuild_only {
        dropped.push("rebuild-only count");
    }
    if truncation.collapsed_to_counts {
        dropped.push("per-package table");
    }
    if !dropped.is_empty() {
        let _ = write!(out, " Dropped: {}.", dropped.join(", "));
    }
    let _ = writeln!(
        out,
        " Full payload: `/v1/commits/{head}/change-summary`.",
        head = head_sha,
    );
}

fn short_sha(sha: &str) -> &str {
    if sha.len() > 12 { &sha[..12] } else { sha }
}

#[derive(Debug, Default, Clone, Copy)]
struct ChangeCounts {
    added: usize,
    removed: usize,
    bumped: usize,
    renamed: usize,
    rebuild_only: usize,
    license: usize,
    maintainers: usize,
}

impl ChangeCounts {
    fn from_changes(changes: &[PackageChange]) -> Self {
        let mut c = Self::default();
        for change in changes {
            match change {
                PackageChange::Added { .. } => c.added += 1,
                PackageChange::Removed { .. } => c.removed += 1,
                PackageChange::VersionBump { .. } => c.bumped += 1,
                PackageChange::Renamed { .. } => c.renamed += 1,
                PackageChange::RebuildOnly { .. } => c.rebuild_only += 1,
                PackageChange::LicenseChange { .. } => c.license += 1,
                PackageChange::MaintainerChange { .. } => c.maintainers += 1,
            }
        }
        c
    }

    /// Visible (non-rebuild-only) change count, gated by which columns
    /// the renderer is currently configured to emit.
    fn total_visible(&self, config: &RenderConfig) -> usize {
        if config.collapse_table {
            // In collapsed mode the count is informational only —
            // mirror the same primary-change set.
            return self.added + self.removed + self.bumped + self.renamed;
        }
        let mut total = self.added + self.removed + self.bumped + self.renamed;
        if config.include_license {
            total += self.license;
        }
        if config.include_maintainers {
            total += self.maintainers;
        }
        total
    }

    fn summary_line(&self) -> String {
        let parts = [
            (self.bumped, "bump"),
            (self.added, "added"),
            (self.removed, "removed"),
            (self.renamed, "renamed"),
            (self.license, "license change"),
            (self.maintainers, "maintainer change"),
        ];
        let pieces: Vec<String> = parts
            .iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, label)| {
                format!(
                    "{n} {label}{plural}",
                    plural = if *n == 1 { "" } else { "s" }
                )
            })
            .collect();
        if pieces.is_empty() {
            "no source changes".to_string()
        } else {
            pieces.join(", ")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::change_summary::types::{
        ChangeSummary, ChangeSummaryRebuildImpact, PackageChange, PerSystemImpact,
        TopBlastRadiusEntry,
    };
    use crate::db::model::DrvId;

    fn pad_hash(prefix: &str) -> String {
        let sanitized: String = prefix
            .chars()
            .map(|c| match c {
                'e' => 'f',
                'o' => 'p',
                't' => 's',
                'u' => 'v',
                other => other,
            })
            .collect();
        format!("{:0>32}", sanitized)
    }

    fn drv(prefix: &str, name: &str) -> DrvId {
        let hash = pad_hash(prefix);
        DrvId::from_str(&format!("/nix/store/{hash}-{name}.drv")).unwrap()
    }

    fn empty_summary() -> ChangeSummary {
        ChangeSummary {
            head_sha: "abc123abc123abc123".to_string(),
            base_sha: "def456def456def456".to_string(),
            job: "ci".to_string(),
            computed_at: "2026-04-25T00:00:00Z".to_string(),
            metadata_available: true,
            package_changes: vec![],
            rebuild_impact: ChangeSummaryRebuildImpact {
                per_system: vec![],
                total_unique_drvs: 0,
            },
            truncated: false,
            config_load_error: None,
            markdown: String::new(),
        }
    }

    #[test]
    fn render_empty_summary_includes_headers() {
        let summary = empty_summary();
        let (md, trunc) = render(&summary, &RenderOptions::default());
        assert!(md.contains("## Change summary for `abc123abc123` ← `def456def456`"));
        assert!(md.contains("### Packages changed (0)"));
        assert!(md.contains("### Rebuild impact"));
        assert!(md.contains("_No rebuild impact computed for this commit._"));
        assert!(!trunc.any());
    }

    #[test]
    fn render_includes_metadata_unavailable_banner() {
        let mut summary = empty_summary();
        summary.metadata_available = false;
        let (md, _) = render(&summary, &RenderOptions::default());
        assert!(md.contains("⚠ Package metadata unavailable"));
    }

    #[test]
    fn render_version_bump_row() {
        let mut summary = empty_summary();
        summary.package_changes.push(PackageChange::VersionBump {
            attr_path: "hello".to_string(),
            pname: "hello".to_string(),
            old: "2.12".to_string(),
            new: "2.13".to_string(),
            drv_path: drv("aaa1", "hello-2.13"),
        });
        let (md, _) = render(&summary, &RenderOptions::default());
        assert!(md.contains("| Bump | `hello` | 2.12 → 2.13 |"));
        assert!(md.contains("### Packages changed (1)"));
    }

    #[test]
    fn render_added_removed_renamed_rows() {
        let mut summary = empty_summary();
        summary.package_changes = vec![
            PackageChange::Added {
                attr_path: "new-pkg".to_string(),
                pname: Some("new-pkg".to_string()),
                version: Some("1.0".to_string()),
                drv_path: drv("aaa1", "new-pkg-1.0"),
            },
            PackageChange::Removed {
                attr_path: "old-pkg".to_string(),
                pname: Some("old-pkg".to_string()),
                version: Some("0.9".to_string()),
            },
            PackageChange::Renamed {
                attr_path: "rn".to_string(),
                pname_old: "tool-old".to_string(),
                pname_new: "tool-new".to_string(),
                version: Some("2.0".to_string()),
                drv_path: drv("bbb1", "tool-new-2.0"),
            },
        ];
        let (md, _) = render(&summary, &RenderOptions::default());
        assert!(md.contains("| Added | `new-pkg` | 1.0 |"));
        assert!(md.contains("| Removed | `old-pkg` | (was 0.9) |"));
        assert!(md.contains("| Renamed | `tool-old → tool-new` | 2.0 |"));
    }

    #[test]
    fn render_license_and_maintainer_rows() {
        let mut summary = empty_summary();
        summary.package_changes = vec![
            PackageChange::LicenseChange {
                attr_path: "lib-x".to_string(),
                pname: "lib-x".to_string(),
                old: vec!["MIT".to_string()],
                new: vec!["BSD-3-Clause".to_string()],
                drv_path: drv("ccc1", "lib-x-1.0"),
            },
            PackageChange::MaintainerChange {
                attr_path: "tool-y".to_string(),
                pname: "tool-y".to_string(),
                added: vec!["alice".to_string()],
                removed: vec!["bob".to_string()],
                drv_path: drv("ddd1", "tool-y-2.0"),
            },
        ];
        let (md, _) = render(&summary, &RenderOptions::default());
        assert!(md.contains("| License | `lib-x` | MIT → BSD-3-Clause |"));
        assert!(md.contains("| Maintainers | `tool-y` | +alice, -bob |"));
    }

    #[test]
    fn render_rebuild_only_collapses_to_count_line() {
        let mut summary = empty_summary();
        summary.package_changes = vec![
            PackageChange::RebuildOnly {
                attr_path: "a".to_string(),
                pname: Some("a".to_string()),
                version: Some("1.0".to_string()),
                drv_path: drv("eee1", "a-1.0"),
            },
            PackageChange::RebuildOnly {
                attr_path: "b".to_string(),
                pname: Some("b".to_string()),
                version: Some("1.0".to_string()),
                drv_path: drv("eee2", "b-1.0"),
            },
        ];
        let (md, _) = render(&summary, &RenderOptions::default());
        assert!(md.contains("**Plus 2 packages will rebuild without source changes.**"));
        // RebuildOnly entries must NOT appear as table rows.
        assert!(!md.contains("| RebuildOnly |"));
    }

    #[test]
    fn render_impact_section_lists_systems_with_top_blast() {
        let mut summary = empty_summary();
        summary.rebuild_impact.per_system = vec![PerSystemImpact {
            system: "x86_64-linux".to_string(),
            rebuild_count: 4_312,
            top_blast_radius: vec![TopBlastRadiusEntry {
                pname: Some("openssl".to_string()),
                drv_path: drv("ffff", "openssl-3.0.14"),
                blast_radius: 3_871,
            }],
        }];
        summary.rebuild_impact.total_unique_drvs = 10_842;
        let (md, _) = render(&summary, &RenderOptions::default());
        assert!(md.contains("| `x86_64-linux` | 4312 | `openssl` (3871) |"));
        assert!(md.contains("**Total unique drvs that would rebuild somewhere:** 10842"));
    }

    #[test]
    fn render_truncation_drops_maintainers_then_license_then_collapses() {
        // Build a giant change list to force truncation. We construct
        // ~200KB of maintainer changes; that easily blows past 60KB.
        let mut summary = empty_summary();
        for i in 0..2000 {
            summary
                .package_changes
                .push(PackageChange::MaintainerChange {
                    attr_path: format!("pkg{i}"),
                    pname: format!("pkg{i}"),
                    added: vec!["alice-with-a-fairly-long-handle".to_string()],
                    removed: vec!["bob-with-a-fairly-long-handle".to_string()],
                    drv_path: drv(&format!("aa{i}"), &format!("pkg{i}-1.0")),
                });
        }
        let (md, trunc) = render(&summary, &RenderOptions::default());
        assert!(
            md.len() <= GITHUB_CHECK_SUMMARY_HARD_LIMIT,
            "rendered {} bytes",
            md.len()
        );
        // First drop step *will* fire because the un-dropped variant is > soft limit.
        assert!(trunc.dropped_maintainers, "maintainers should drop first");
        // The footer should mention the truncation.
        assert!(md.contains("Output truncated"));
    }

    #[test]
    fn render_short_sha_truncates_to_12_chars() {
        assert_eq!(short_sha("abcdef1234567890"), "abcdef123456");
        assert_eq!(short_sha("abc"), "abc");
    }

    #[test]
    fn extract_pname_handles_well_formed_drv_path() {
        let p = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello-2.13.drv";
        assert_eq!(extract_pname_from_drv(p), "hello-2.13");
    }

    #[test]
    fn render_suppresses_rebuild_only_line_when_option_disabled() {
        let mut summary = empty_summary();
        summary.package_changes = vec![PackageChange::RebuildOnly {
            attr_path: "a".to_string(),
            pname: Some("a".to_string()),
            version: Some("1.0".to_string()),
            drv_path: drv("eee1", "a-1.0"),
        }];
        let opts = RenderOptions {
            include_rebuild_only: false,
            config_load_error: None,
        };
        let (md, trunc) = render(&summary, &opts);
        assert!(!md.contains("rebuild without source changes"));
        // The line was off by request, not dropped by truncation — footer must not lie.
        assert!(!trunc.dropped_rebuild_only);
    }

    #[test]
    fn render_options_default_includes_rebuild_only() {
        assert!(RenderOptions::default().include_rebuild_only);
    }

    #[test]
    fn render_truncation_collapses_table_when_oversize_persists() {
        // Force every drop to fire by stuffing the *primary* changes
        // (Added) — these survive maintainers/license drops, so we
        // exercise the rebuild-only-then-collapse path.
        let mut summary = empty_summary();
        for i in 0..2500 {
            summary.package_changes.push(PackageChange::Added {
                attr_path: format!("verbose-package-attr-path-{i}"),
                pname: Some(format!("verbose-package-attr-path-{i}")),
                version: Some(format!("1.{i}.0-with-extra-padding-to-bloat-row-size")),
                drv_path: drv(&format!("a{i}"), &format!("pkg{i}-1.0")),
            });
        }
        let (md, trunc) = render(&summary, &RenderOptions::default());
        assert!(md.len() <= GITHUB_CHECK_SUMMARY_HARD_LIMIT);
        assert!(trunc.collapsed_to_counts, "expected collapse to fire");
        assert!(md.contains("**Summary:**"));
    }

    /// H6: config parse error banner appears in rendered markdown.
    #[test]
    fn render_includes_config_error_banner() {
        let summary = empty_summary();
        let opts = RenderOptions {
            include_rebuild_only: true,
            config_load_error: Some(
                "/path/to/config.json: expected value at line 1 column 10".to_string(),
            ),
        };

        let (md, _trunc) = render(&summary, &opts);

        // Banner should be present
        assert!(
            md.contains("`.ekaci/config.json` failed to parse"),
            "Banner warning not found in output"
        );
        assert!(
            md.contains("using defaults"),
            "Default fallback message not found"
        );
        assert!(
            md.contains("expected value at line 1"),
            "Error message not included in banner"
        );

        // Banner should appear before the ## heading
        let heading_pos = md
            .find("## Change summary")
            .expect("heading should be present");
        let banner_pos = md
            .find("`.ekaci/config.json` failed to parse")
            .expect("banner should be present");
        assert!(
            banner_pos < heading_pos,
            "Banner should appear before heading"
        );
    }

    #[test]
    fn render_omits_banner_when_no_config_error() {
        let summary = empty_summary();
        let opts = RenderOptions {
            include_rebuild_only: true,
            config_load_error: None,
        };

        let (md, _trunc) = render(&summary, &opts);

        // Banner should NOT be present
        assert!(
            !md.contains("`.ekaci/config.json` failed to parse"),
            "Banner should not appear when config_load_error is None"
        );
    }
}
