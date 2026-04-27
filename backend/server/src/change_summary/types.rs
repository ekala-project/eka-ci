//! Wire types for the package change summary; serialised to HTTP and consumed by the renderer.

use serde::Serialize;

use crate::db::model::DrvId;

/// A classified package change. A drv may produce multiple entries (e.g. version bump +
/// maintainer change). `attr_path` (= `Job.name`) is the stable cross-rev identity.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum PackageChange {
    /// Attr path appeared in head but not in base.
    Added {
        attr_path: String,
        pname: Option<String>,
        version: Option<String>,
        drv_path: DrvId,
    },

    /// Attr path appeared in base but not in head.
    Removed {
        attr_path: String,
        pname: Option<String>,
        version: Option<String>,
    },

    /// Same `attr_path`, same `pname`, different `version`.
    VersionBump {
        attr_path: String,
        pname: String,
        old: String,
        new: String,
        drv_path: DrvId,
    },

    /// Same `attr_path`, different `pname`.
    Renamed {
        attr_path: String,
        pname_old: String,
        pname_new: String,
        version: Option<String>,
        drv_path: DrvId,
    },

    /// Same `pname`, same `version`, drv hash differs and metadata
    /// (license/maintainers) is identical. Indicates a transitive change.
    RebuildOnly {
        attr_path: String,
        pname: Option<String>,
        version: Option<String>,
        drv_path: DrvId,
    },

    /// `license_json` differs. Emitted **in addition to** the primary
    /// classification (RebuildOnly / VersionBump / etc.) when applicable.
    LicenseChange {
        attr_path: String,
        pname: String,
        old: Vec<String>,
        new: Vec<String>,
        drv_path: DrvId,
    },

    /// `maintainers_json` differs. Emitted alongside the primary
    /// classification when applicable.
    MaintainerChange {
        attr_path: String,
        pname: String,
        added: Vec<String>,
        removed: Vec<String>,
        drv_path: DrvId,
    },
}

// Accessors used by the renderer and impact pass; kept on the public API.
#[allow(dead_code)]
impl PackageChange {
    /// Stable discriminant string used in metrics, logs, and renderer code
    /// paths. Mirrors the `kind` field in the serialised representation.
    pub fn kind(&self) -> &'static str {
        match self {
            PackageChange::Added { .. } => "Added",
            PackageChange::Removed { .. } => "Removed",
            PackageChange::VersionBump { .. } => "VersionBump",
            PackageChange::Renamed { .. } => "Renamed",
            PackageChange::RebuildOnly { .. } => "RebuildOnly",
            PackageChange::LicenseChange { .. } => "LicenseChange",
            PackageChange::MaintainerChange { .. } => "MaintainerChange",
        }
    }

    /// Returns the `attr_path` (= `Job.name`) regardless of variant.
    pub fn attr_path(&self) -> &str {
        match self {
            PackageChange::Added { attr_path, .. }
            | PackageChange::Removed { attr_path, .. }
            | PackageChange::VersionBump { attr_path, .. }
            | PackageChange::Renamed { attr_path, .. }
            | PackageChange::RebuildOnly { attr_path, .. }
            | PackageChange::LicenseChange { attr_path, .. }
            | PackageChange::MaintainerChange { attr_path, .. } => attr_path,
        }
    }
}

/// HTTP response body for `/v1/commits/{sha}/package-changes`.
#[derive(Debug, Clone, Serialize)]
pub struct PackageChangesResponse {
    pub head_sha: String,
    pub base_sha: String,
    pub job: String,
    /// RFC 3339 / ISO 8601 timestamp marking when the comparison was
    /// computed. Stringly-typed to avoid pulling in the `chrono/serde`
    /// feature for this single field; the renderer is free to parse it.
    pub computed_at: String,
    /// True iff package metadata (`pname`, `version`, license/maintainers)
    /// was available for at least one drv in the comparison. When false,
    /// classifications fall back to heuristic name parsing only.
    pub metadata_available: bool,
    pub package_changes: Vec<PackageChange>,
    /// True if the list was clipped to `max_packages_listed`.
    pub truncated: bool,
}

/// One entry in the per-system `top_blast_radius` ranking.
///
/// `pname` is best-effort — it's present when the head-side `Drv` row had
/// a non-NULL `pname`. Renderers should fall back to a label derived from
/// `drv_path` when absent.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TopBlastRadiusEntry {
    pub pname: Option<String>,
    pub drv_path: DrvId,
    /// Count of strict transitive dependents reachable from this drv via
    /// the `dependents` adjacency. Excludes the drv itself.
    pub blast_radius: usize,
}

/// Per-system slice of the rebuild-impact response.
///
/// `rebuild_count` is the number of `Job` rows for this system on the
/// head-jobset whose `difference` is `New` (0) or `Changed` (1) — i.e.,
/// the number of drvs that would have to build (or rebuild) on this
/// system if the PR landed.
///
/// `top_blast_radius` is the largest-impact subset, descending by radius
/// with `drv_path` as a deterministic tie-breaker, capped at `max_top_blast_radius`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PerSystemImpact {
    pub system: String,
    pub rebuild_count: usize,
    pub top_blast_radius: Vec<TopBlastRadiusEntry>,
}

/// HTTP response body for `/v1/commits/{sha}/rebuild-impact`.
#[derive(Debug, Clone, Serialize)]
pub struct RebuildImpactResponse {
    pub head_sha: String,
    pub base_sha: String,
    pub job: String,
    /// RFC 3339 / ISO 8601 timestamp; stringly-typed for the same reason
    /// as [`PackageChangesResponse::computed_at`].
    pub computed_at: String,
    /// One entry per system (deterministic alphabetical order).
    pub per_system: Vec<PerSystemImpact>,
    /// Size of the union of `reverse_reachable({all_changed_seeds})` —
    /// the total count of distinct drvs that would have to rebuild
    /// somewhere across all systems.
    pub total_unique_drvs: usize,
}

/// Aggregate response for `/v1/commits/{sha}/change-summary`: package-change list +
/// rebuild-impact + pre-rendered markdown matching the `.md` endpoint output.
#[derive(Debug, Clone, Serialize)]
pub struct ChangeSummary {
    pub head_sha: String,
    pub base_sha: String,
    pub job: String,
    pub computed_at: String,
    pub metadata_available: bool,
    pub package_changes: Vec<PackageChange>,
    pub rebuild_impact: ChangeSummaryRebuildImpact,
    /// True if either the package-change list or the rebuild-impact
    /// computation hit a configured cap (renderer surface as a footnote).
    pub truncated: bool,
    /// Set when `.ekaci/config.json` was present but failed to parse.
    /// Contains a user-facing error message. Omitted from JSON output when `None`
    /// to preserve backward compatibility with existing API consumers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_load_error: Option<String>,
    /// Pre-rendered markdown matching the GitHub-check-summary output.
    /// Populated by the renderer; the structured fields above remain
    /// authoritative for programmatic consumers.
    pub markdown: String,
}

/// The rebuild-impact slice as embedded in [`ChangeSummary`]. Drops the
/// `(head_sha, base_sha, job, computed_at)` envelope since that lives on
/// the parent struct.
#[derive(Debug, Clone, Serialize)]
pub struct ChangeSummaryRebuildImpact {
    pub per_system: Vec<PerSystemImpact>,
    pub total_unique_drvs: usize,
}
