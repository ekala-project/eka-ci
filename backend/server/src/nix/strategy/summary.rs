//! Eval-gate summaries describing what a rebuild strategy chose to build.

use std::fmt::Write;

use super::{RebuildPlan, RebuildSelection, SeedReason};

/// Seeds listed in the summary; the rest are counted.
const MAX_LISTED_SEEDS: usize = 50;

impl SeedReason {
    fn describe(self) -> &'static str {
        match self {
            SeedReason::New => "new attribute",
            SeedReason::Edited => "definition edited",
            SeedReason::VersionChanged => "version changed",
            SeedReason::Root => "none of its dependencies were rebuilt",
        }
    }
}

impl RebuildPlan {
    /// Markdown for the eval gate's output, or `None` for the `all` strategy.
    pub fn summary(&self) -> Option<String> {
        match self {
            RebuildPlan::All => None,
            RebuildPlan::Fallback(reason) => Some(format!(
                "Rebuild strategy `direct-referrers` could not be applied, so every rebuilt job \
                 is built: {reason}."
            )),
            RebuildPlan::Selective(selection) => Some(selective_summary(selection)),
        }
    }
}

fn selective_summary(selection: &RebuildSelection) -> String {
    let selected = selection.selected.len();
    let mut out = format!(
        "Rebuild strategy `direct-referrers`: building {selected} of {} rebuilt jobs ({} seeds \
         and their direct referrers); {} skipped. Dependencies of the selected jobs are still \
         built as needed.\n",
        selection.rebuilt,
        selection.seeds.len(),
        selection.rebuilt.saturating_sub(selected),
    );
    if selection.seeds.is_empty() {
        return out;
    }

    out.push_str("\nSeeds:\n");
    for seed in selection.seeds.iter().take(MAX_LISTED_SEEDS) {
        // Writing to a String cannot fail.
        writeln!(out, "- `{}`: {}", seed.attr, seed.reason.describe()).ok();
    }
    let unlisted = selection.seeds.len().saturating_sub(MAX_LISTED_SEEDS);
    if unlisted > 0 {
        writeln!(out, "- … and {unlisted} more").ok();
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::nix::strategy::Seed;

    fn seed(attr: &str, reason: SeedReason) -> Seed {
        Seed {
            attr: attr.to_string(),
            reason,
        }
    }

    #[test]
    fn all_strategy_has_no_summary() {
        assert_eq!(RebuildPlan::All.summary(), None);
    }

    #[test]
    fn fallback_explains_reason() {
        let plan = RebuildPlan::Fallback("the base commit has no jobset".to_string());
        let summary = plan.summary().unwrap();
        assert!(summary.contains("could not be applied"));
        assert!(summary.ends_with("the base commit has no jobset."));
    }

    #[test]
    fn selective_summary_counts_and_lists_seeds() {
        let plan = RebuildPlan::Selective(RebuildSelection {
            selected: HashSet::from(["openssl".into(), "curl".into(), "newpkg".into()]),
            seeds: vec![
                seed("newpkg", SeedReason::New),
                seed("openssl", SeedReason::Edited),
            ],
            rebuilt: 10,
        });
        let summary = plan.summary().unwrap();
        assert!(summary.contains("building 3 of 10 rebuilt jobs (2 seeds"));
        assert!(summary.contains("7 skipped"));
        assert!(summary.contains("- `newpkg`: new attribute\n"));
        assert!(summary.contains("- `openssl`: definition edited\n"));
    }

    #[test]
    fn long_seed_lists_are_truncated() {
        let seeds: Vec<Seed> = (0..60)
            .map(|i| seed(&format!("pkg{i}"), SeedReason::Root))
            .collect();
        let selected = seeds.iter().map(|s| s.attr.clone()).collect();
        let plan = RebuildPlan::Selective(RebuildSelection {
            selected,
            seeds,
            rebuilt: 60,
        });
        let summary = plan.summary().unwrap();
        assert_eq!(summary.matches("none of its dependencies").count(), 50);
        assert!(summary.contains("… and 10 more"));
    }
}
