//! Eviction policy and candidate selection for LRU cache
//!
//! This module implements the tiered eviction strategy:
//! - Tier 1: TransitiveFailure (safest - dead-end state)
//! - Tier 2: Completed(Failure) (safe after propagation)
//! - Tier 3: Completed(Success) (safe after dependents processed)

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::db::model::build_event::{DrvBuildResult, DrvBuildState};
use crate::db::model::drv_id::DrvId;
use crate::graph::graph::BuildGraph;

/// Eviction tier levels, ordered from safest to least safe
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvictionTier {
    /// Tier 1: TransitiveFailure - safest, true dead-end state
    Tier1 = 1,
    /// Tier 2: Completed(Failure) - safe after propagation
    Tier2 = 2,
    /// Tier 3: Completed(Success) - safe after dependents processed
    Tier3 = 3,
}

impl EvictionTier {
    /// Get the target state for this tier
    pub fn target_state(&self) -> TargetState {
        match self {
            EvictionTier::Tier1 => TargetState::TransitiveFailure,
            EvictionTier::Tier2 => TargetState::CompletedFailure,
            EvictionTier::Tier3 => TargetState::CompletedSuccess,
        }
    }

    /// Get human-readable name for metrics labels
    pub fn name(&self) -> &'static str {
        match self {
            EvictionTier::Tier1 => "tier1_transitive_failure",
            EvictionTier::Tier2 => "tier2_completed_failure",
            EvictionTier::Tier3 => "tier3_completed_success",
        }
    }
}

/// Target states for eviction tiers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetState {
    TransitiveFailure,
    CompletedFailure,
    CompletedSuccess,
}

impl TargetState {
    /// Check if a DrvBuildState matches this target
    pub fn matches(&self, state: &DrvBuildState) -> bool {
        match (self, state) {
            (TargetState::TransitiveFailure, DrvBuildState::TransitiveFailure) => true,
            (TargetState::CompletedFailure, DrvBuildState::Completed(DrvBuildResult::Failure)) => {
                true
            },
            (TargetState::CompletedSuccess, DrvBuildState::Completed(DrvBuildResult::Success)) => {
                true
            },
            _ => false,
        }
    }
}

/// Configuration for eviction candidate selection
#[derive(Debug, Clone)]
pub struct EvictionConfig {
    /// Minimum age for Tier 1 eviction (TransitiveFailure)
    pub tier1_age_secs: u64,
    /// Minimum age for Tier 2 eviction (Completed(Failure))
    pub tier2_age_secs: u64,
    /// Minimum age for Tier 3 eviction (Completed(Success))
    pub tier3_age_secs: u64,
}

impl Default for EvictionConfig {
    fn default() -> Self {
        Self {
            tier1_age_secs: 3600,  // 1 hour
            tier2_age_secs: 21600, // 6 hours
            tier3_age_secs: 86400, // 24 hours
        }
    }
}

impl EvictionConfig {
    /// Get minimum age for a tier
    pub fn min_age_for_tier(&self, tier: EvictionTier) -> Duration {
        let secs = match tier {
            EvictionTier::Tier1 => self.tier1_age_secs,
            EvictionTier::Tier2 => self.tier2_age_secs,
            EvictionTier::Tier3 => self.tier3_age_secs,
        };
        Duration::from_secs(secs)
    }
}

/// Candidate information for eviction
#[derive(Debug, Clone)]
pub struct EvictionCandidate {
    pub drv_id: DrvId,
    pub tier: EvictionTier,
    pub age: Duration,
    pub ref_count: usize,
}

/// Selects eviction candidates using tiered LRU policy
pub struct EvictionCandidateSelector {
    config: EvictionConfig,
}

impl EvictionCandidateSelector {
    /// Create a new eviction candidate selector
    pub fn new(config: EvictionConfig) -> Self {
        Self { config }
    }

    /// Create with default configuration
    pub fn with_defaults() -> Self {
        Self::new(EvictionConfig::default())
    }

    /// Select up to `target_count` candidates for eviction using tiered LRU
    ///
    /// Strategy:
    /// 1. Try Tier 1 (TransitiveFailure) first - safest
    /// 2. If not enough, try Tier 2 (Completed(Failure))
    /// 3. If still not enough, try Tier 3 (Completed(Success))
    /// 4. Within each tier, select oldest first (LRU)
    ///
    /// Constraints:
    /// - Only select nodes with ref_count == 0
    /// - Only select nodes older than tier's minimum age
    /// - Never select non-terminal states
    pub fn select_candidates(
        &self,
        graph: &BuildGraph,
        last_accessed: &HashMap<DrvId, Instant>,
        ref_counts: &HashMap<DrvId, usize>,
        target_count: usize,
    ) -> Vec<EvictionCandidate> {
        let now = Instant::now();
        let mut candidates = Vec::new();

        // Try each tier in order until we have enough candidates
        for tier in [
            EvictionTier::Tier1,
            EvictionTier::Tier2,
            EvictionTier::Tier3,
        ] {
            if candidates.len() >= target_count {
                break;
            }

            let tier_candidates = self.select_from_tier(
                graph,
                last_accessed,
                ref_counts,
                tier,
                now,
                target_count - candidates.len(),
            );

            candidates.extend(tier_candidates);
        }

        candidates.truncate(target_count);
        candidates
    }

    /// Select candidates from a specific tier only (for Tier 1 background eviction)
    pub fn select_tier1_only(
        &self,
        graph: &BuildGraph,
        last_accessed: &HashMap<DrvId, Instant>,
        ref_counts: &HashMap<DrvId, usize>,
    ) -> Vec<EvictionCandidate> {
        let now = Instant::now();
        self.select_from_tier(
            graph,
            last_accessed,
            ref_counts,
            EvictionTier::Tier1,
            now,
            usize::MAX, // No limit for background eviction
        )
    }

    /// Select candidates from a specific tier
    fn select_from_tier(
        &self,
        graph: &BuildGraph,
        last_accessed: &HashMap<DrvId, Instant>,
        ref_counts: &HashMap<DrvId, usize>,
        tier: EvictionTier,
        now: Instant,
        limit: usize,
    ) -> Vec<EvictionCandidate> {
        let target_state = tier.target_state();
        let min_age = self.config.min_age_for_tier(tier);

        // Get all drvs in the target state
        let state_drvs = match target_state {
            TargetState::TransitiveFailure => {
                graph.get_drvs_by_state(&DrvBuildState::TransitiveFailure)
            },
            TargetState::CompletedFailure => {
                graph.get_drvs_by_state(&DrvBuildState::Completed(DrvBuildResult::Failure))
            },
            TargetState::CompletedSuccess => {
                graph.get_drvs_by_state(&DrvBuildState::Completed(DrvBuildResult::Success))
            },
        };

        // Filter and collect candidates
        let mut tier_candidates: Vec<EvictionCandidate> = state_drvs
            .into_iter()
            .filter_map(|drv_id| {
                // Must have ref_count == 0 (no in-cache dependents)
                let ref_count = ref_counts.get(&drv_id).copied().unwrap_or(0);
                if ref_count != 0 {
                    return None;
                }

                // Must be old enough
                let last_access = last_accessed.get(&drv_id).copied().unwrap_or(now);
                let age = now.duration_since(last_access);
                if age < min_age {
                    return None;
                }

                Some(EvictionCandidate {
                    drv_id,
                    tier,
                    age,
                    ref_count,
                })
            })
            .collect();

        // Sort by age descending (oldest first)
        tier_candidates.sort_by(|a, b| b.age.cmp(&a.age));

        // Take up to limit
        tier_candidates.truncate(limit);
        tier_candidates
    }

    /// Count eviction candidates by tier (for metrics)
    pub fn count_candidates_by_tier(
        &self,
        graph: &BuildGraph,
        last_accessed: &HashMap<DrvId, Instant>,
        ref_counts: &HashMap<DrvId, usize>,
    ) -> HashMap<EvictionTier, usize> {
        let now = Instant::now();
        let mut counts = HashMap::new();

        for tier in [
            EvictionTier::Tier1,
            EvictionTier::Tier2,
            EvictionTier::Tier3,
        ] {
            let candidates =
                self.select_from_tier(graph, last_accessed, ref_counts, tier, now, usize::MAX);
            counts.insert(tier, candidates.len());
        }

        counts
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::db::model::drv::Drv;

    fn make_test_drv(hash: &str, state: DrvBuildState) -> Drv {
        let drv_path = format!("{}-test.drv", hash);
        Drv {
            drv_path: DrvId::from_str(&drv_path).unwrap(),
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: state,
            output_size: None,
        }
    }

    #[test]
    fn test_tier_ordering() {
        assert!(EvictionTier::Tier1 < EvictionTier::Tier2);
        assert!(EvictionTier::Tier2 < EvictionTier::Tier3);
    }

    #[test]
    fn test_target_state_matching() {
        let tier1_state = EvictionTier::Tier1.target_state();
        assert!(tier1_state.matches(&DrvBuildState::TransitiveFailure));
        assert!(!tier1_state.matches(&DrvBuildState::Completed(DrvBuildResult::Success)));

        let tier3_state = EvictionTier::Tier3.target_state();
        assert!(tier3_state.matches(&DrvBuildState::Completed(DrvBuildResult::Success)));
        assert!(!tier3_state.matches(&DrvBuildState::TransitiveFailure));
    }

    #[test]
    fn test_select_candidates_empty_graph() {
        let selector = EvictionCandidateSelector::with_defaults();
        let graph = BuildGraph::new(1000);
        let last_accessed = HashMap::new();
        let ref_counts = HashMap::new();

        let candidates = selector.select_candidates(&graph, &last_accessed, &ref_counts, 10);
        assert_eq!(candidates.len(), 0);
    }

    #[test]
    fn test_select_candidates_respects_ref_count() {
        let mut graph = BuildGraph::new(1000);
        let drv = make_test_drv("aaa", DrvBuildState::TransitiveFailure);
        let drv_id = drv.drv_path.clone();
        graph.insert_node(drv);

        let mut last_accessed = HashMap::new();
        last_accessed.insert(
            drv_id.clone(),
            Instant::now() - Duration::from_secs(7200), // 2 hours old
        );

        // ref_count > 0 should prevent selection
        let mut ref_counts = HashMap::new();
        ref_counts.insert(drv_id.clone(), 1);

        let selector = EvictionCandidateSelector::with_defaults();
        let candidates = selector.select_candidates(&graph, &last_accessed, &ref_counts, 10);
        assert_eq!(
            candidates.len(),
            0,
            "Should not select node with ref_count > 0"
        );

        // ref_count == 0 should allow selection
        ref_counts.insert(drv_id.clone(), 0);
        let candidates = selector.select_candidates(&graph, &last_accessed, &ref_counts, 10);
        assert_eq!(
            candidates.len(),
            1,
            "Should select node with ref_count == 0"
        );
    }

    #[test]
    fn test_select_candidates_respects_age() {
        let mut graph = BuildGraph::new(1000);
        let drv = make_test_drv("aaa", DrvBuildState::TransitiveFailure);
        let drv_id = drv.drv_path.clone();
        graph.insert_node(drv);

        let ref_counts = HashMap::new();
        let selector = EvictionCandidateSelector::with_defaults();

        // Too young (30 minutes, Tier1 needs 1 hour)
        let mut last_accessed = HashMap::new();
        last_accessed.insert(drv_id.clone(), Instant::now() - Duration::from_secs(1800));
        let candidates = selector.select_candidates(&graph, &last_accessed, &ref_counts, 10);
        assert_eq!(
            candidates.len(),
            0,
            "Should not select node that's too young"
        );

        // Old enough (2 hours)
        last_accessed.insert(drv_id.clone(), Instant::now() - Duration::from_secs(7200));
        let candidates = selector.select_candidates(&graph, &last_accessed, &ref_counts, 10);
        assert_eq!(candidates.len(), 1, "Should select node that's old enough");
    }

    #[test]
    fn test_select_candidates_tier_ordering() {
        let mut graph = BuildGraph::new(1000);

        // Add one of each type
        let tf_drv = make_test_drv("tf", DrvBuildState::TransitiveFailure);
        let fail_drv = make_test_drv("fail", DrvBuildState::Completed(DrvBuildResult::Failure));
        let success_drv =
            make_test_drv("success", DrvBuildState::Completed(DrvBuildResult::Success));

        let tf_id = tf_drv.drv_path.clone();
        let fail_id = fail_drv.drv_path.clone();
        let success_id = success_drv.drv_path.clone();

        graph.insert_node(tf_drv);
        graph.insert_node(fail_drv);
        graph.insert_node(success_drv);

        // All old enough for their tiers
        let mut last_accessed = HashMap::new();
        last_accessed.insert(tf_id.clone(), Instant::now() - Duration::from_secs(7200)); // 2h
        last_accessed.insert(fail_id.clone(), Instant::now() - Duration::from_secs(25200)); // 7h
        last_accessed.insert(
            success_id.clone(),
            Instant::now() - Duration::from_secs(90000),
        ); // 25h

        let ref_counts = HashMap::new();
        let selector = EvictionCandidateSelector::with_defaults();

        // Request only 1 candidate - should get Tier1 (TransitiveFailure)
        let candidates = selector.select_candidates(&graph, &last_accessed, &ref_counts, 1);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].tier, EvictionTier::Tier1);
        assert_eq!(candidates[0].drv_id, tf_id);

        // Request 2 candidates - should get Tier1 + Tier2
        let candidates = selector.select_candidates(&graph, &last_accessed, &ref_counts, 2);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].tier, EvictionTier::Tier1);
        assert_eq!(candidates[1].tier, EvictionTier::Tier2);

        // Request 3 candidates - should get all three
        let candidates = selector.select_candidates(&graph, &last_accessed, &ref_counts, 3);
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].tier, EvictionTier::Tier1);
        assert_eq!(candidates[1].tier, EvictionTier::Tier2);
        assert_eq!(candidates[2].tier, EvictionTier::Tier3);
    }

    #[test]
    fn test_lru_ordering_within_tier() {
        let mut graph = BuildGraph::new(1000);

        // Add multiple TransitiveFailure nodes with different ages
        let drv1 = make_test_drv("old", DrvBuildState::TransitiveFailure);
        let drv2 = make_test_drv("middle", DrvBuildState::TransitiveFailure);
        let drv3 = make_test_drv("new", DrvBuildState::TransitiveFailure);

        let id1 = drv1.drv_path.clone();
        let id2 = drv2.drv_path.clone();
        let id3 = drv3.drv_path.clone();

        graph.insert_node(drv1);
        graph.insert_node(drv2);
        graph.insert_node(drv3);

        let mut last_accessed = HashMap::new();
        last_accessed.insert(id1.clone(), Instant::now() - Duration::from_secs(10800)); // 3h (oldest)
        last_accessed.insert(id2.clone(), Instant::now() - Duration::from_secs(7200)); // 2h (middle)
        last_accessed.insert(id3.clone(), Instant::now() - Duration::from_secs(3600)); // 1h (newest)

        let ref_counts = HashMap::new();
        let selector = EvictionCandidateSelector::with_defaults();

        let candidates = selector.select_candidates(&graph, &last_accessed, &ref_counts, 3);
        assert_eq!(candidates.len(), 3);

        // Should be ordered oldest first (LRU)
        assert_eq!(candidates[0].drv_id, id1, "Oldest should be first");
        assert_eq!(candidates[1].drv_id, id2, "Middle should be second");
        assert_eq!(candidates[2].drv_id, id3, "Newest should be last");
    }
}
