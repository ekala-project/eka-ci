//! Package change summary and rebuild impact analysis.
//!
//! Composes a per-PR view from the structured package diff ([`classify`]),
//! per-system rebuild + blast-radius numbers ([`impact`]), and a markdown
//! rendering ([`render`]) suitable for posting as a GitHub/GitLab/Gitea check or comment.
//!
//! This crate provides platform-agnostic change summary functionality that can be
//! used by any platform service (GitHub, GitLab, Gitea).

pub mod builder;
pub mod cache;
pub mod classify;
pub mod impact;
pub mod options;
pub mod render;
pub mod types;

// Re-export main builder functions
pub use builder::{
    build_change_summary_from_jobset_ids,
    build_package_changes_from_jobset_ids,
};

// Re-export options and configuration
pub use options::{
    ChangeSummaryOptions,
    ConfigLoadStatus,
};

// Re-export public types
pub use types::{
    ChangeSummary,
    ChangeSummaryRebuildImpact,
    PackageChange,
    PerSystemImpact,
    RebuildImpactResponse,
    TopBlastRadiusEntry,
};

/// Default cap on package-change rows surfaced to the renderer.
pub const DEFAULT_MAX_PACKAGES_LISTED: usize = 100;
