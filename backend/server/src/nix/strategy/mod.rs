//! Rebuild strategies: which of a PR's rebuilt jobs a jobset builds.
//!
//! With the default strategy (`all`) every new or changed job is traversed
//! into the dependency graph and built. The `direct-referrers` strategy picks
//! a subset after evaluation (see [`select`]) and traverses only that subset;
//! the remaining jobs are recorded with `Job.skipped = 1`, so they never enter
//! the graph and are excluded from dispatch, check runs and jobset completion.

mod eval;
mod position;
mod select;
mod summary;

use std::collections::{BTreeMap, HashMap, HashSet};

pub use position::{position_file, repo_roots};
pub use select::{SelectionInput, select_direct_referrers};
use sha2::{Digest, Sha256};

/// Why a rebuilt job was chosen as a seed of the `direct-referrers` strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SeedReason {
    /// The attr does not exist in the base jobset.
    New,
    /// The file holding the job's `meta.position` was edited in the PR.
    Edited,
    /// The job's version differs from the base jobset.
    VersionChanged,
    /// No rebuilt job is among the job's direct `inputDrvs`.
    Root,
}

/// A rebuilt job whose direct referrers are built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seed {
    pub attr: String,
    pub reason: SeedReason,
}

/// The jobs picked by the `direct-referrers` strategy.
#[derive(Debug, Clone, Default)]
pub struct RebuildSelection {
    /// Attr names to build: the seeds and their direct referrers, including
    /// every attr that shares a drv with one of them.
    pub selected: HashSet<String>,
    /// Seeds, sorted by attr name.
    pub seeds: Vec<Seed>,
    /// Number of jobs whose outputs differ from the base jobset.
    pub rebuilt: usize,
}

/// What a jobset builds, as decided after evaluation.
#[derive(Debug, Clone)]
pub enum RebuildPlan {
    /// Build every new or changed job (the `all` strategy, or a base eval).
    All,
    /// `direct-referrers` was configured but could not be applied, so every
    /// new or changed job is built. Holds the reason.
    Fallback(String),
    /// Build only the selected jobs.
    Selective(RebuildSelection),
}

impl RebuildPlan {
    /// Attr names to build, or `None` when every job is built.
    pub fn into_selected(self) -> Option<HashSet<String>> {
        match self {
            RebuildPlan::Selective(selection) => Some(selection.selected),
            RebuildPlan::All | RebuildPlan::Fallback(_) => None,
        }
    }
}

/// SHA-256 over a job's sorted `output=path` lines.
///
/// Two jobs with the same hash produce the same outputs, so the job does not
/// need rebuilding even if its drv path changed (e.g. fixed-output
/// derivation churn, which leaves input-addressed output paths unchanged).
pub fn outputs_hash(outputs: &HashMap<String, String>) -> String {
    let sorted: BTreeMap<&str, &str> = outputs
        .iter()
        .map(|(name, path)| (name.as_str(), path.as_str()))
        .collect();
    let mut hasher = Sha256::new();
    for (name, path) in sorted {
        hasher.update(name.as_bytes());
        hasher.update(b"=");
        hasher.update(path.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outputs(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(n, p)| (n.to_string(), p.to_string()))
            .collect()
    }

    #[test]
    fn outputs_hash_ignores_map_order() {
        let a = outputs(&[("out", "/nix/store/a-x"), ("dev", "/nix/store/b-x-dev")]);
        let b = outputs(&[("dev", "/nix/store/b-x-dev"), ("out", "/nix/store/a-x")]);
        assert_eq!(outputs_hash(&a), outputs_hash(&b));
    }

    #[test]
    fn outputs_hash_changes_with_any_path() {
        let a = outputs(&[("out", "/nix/store/a-x"), ("dev", "/nix/store/b-x-dev")]);
        let b = outputs(&[("out", "/nix/store/a-x"), ("dev", "/nix/store/c-x-dev")]);
        assert_ne!(outputs_hash(&a), outputs_hash(&b));
        assert_eq!(outputs_hash(&a).len(), 64);
    }
}
