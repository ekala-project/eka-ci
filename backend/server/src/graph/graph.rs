use std::collections::{HashMap, HashSet, VecDeque};

use crate::db::model::build_event::{DrvBuildResult, DrvBuildState};
use crate::db::model::drv::Drv;
use crate::db::model::drv_id::DrvId;

/// A node in the build graph representing a single derivation
#[derive(Debug, Clone)]
pub struct GraphNode {
    pub drv_id: DrvId,
    pub system: String,
    pub required_system_features: Option<String>,
    pub is_fod: bool,
    pub build_state: DrvBuildState,

    /// Direct dependencies (drvs that this drv depends on)
    pub dependencies: Vec<DrvId>,

    /// Direct dependents (drvs that depend on this drv)
    pub dependents: Vec<DrvId>,

    /// Set of failed dependencies blocking this drv
    pub blocking_failures: HashSet<DrvId>,
}

impl GraphNode {
    /// Create a new graph node from a Drv
    pub fn from_drv(drv: Drv) -> Self {
        Self {
            drv_id: drv.drv_path,
            system: drv.system,
            required_system_features: drv.required_system_features,
            is_fod: drv.is_fod,
            build_state: drv.build_state,
            dependencies: Vec::new(),
            dependents: Vec::new(),
            blocking_failures: HashSet::new(),
        }
    }
}

/// In-memory directed acyclic graph (DAG) of derivation dependencies and build states
#[derive(Debug)]
pub struct BuildGraph {
    /// All nodes in the graph, indexed by drv_id
    nodes: HashMap<DrvId, GraphNode>,

    /// Index of drvs by their build state for fast queries
    by_state: HashMap<DrvBuildState, HashSet<DrvId>>,

    /// Set of drvs that have permanently failed
    failed_drvs: HashSet<DrvId>,

    /// Map from failed drv to all drvs transitively blocked by it
    transitive_failure_map: HashMap<DrvId, HashSet<DrvId>>,
}

impl BuildGraph {
    /// Create a new empty build graph
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            by_state: HashMap::new(),
            failed_drvs: HashSet::new(),
            transitive_failure_map: HashMap::new(),
        }
    }

    /// Insert a new node into the graph
    pub fn insert_node(&mut self, drv: Drv) {
        let drv_id = drv.drv_path.clone();
        let state = drv.build_state.clone();

        // Create node
        let node = GraphNode::from_drv(drv);

        // Update state index
        self.by_state
            .entry(state.clone())
            .or_insert_with(HashSet::new)
            .insert(drv_id.clone());

        // Track if failed
        if state == DrvBuildState::Completed(DrvBuildResult::Failure) {
            self.failed_drvs.insert(drv_id.clone());
        }

        // Insert into main map
        self.nodes.insert(drv_id, node);
    }

    /// Add a dependency edge: referrer depends on reference
    /// referrer -> reference (referrer needs reference to build)
    pub fn add_edge(&mut self, referrer: DrvId, reference: DrvId) {
        // Add to referrer's dependencies
        if let Some(node) = self.nodes.get_mut(&referrer) {
            if !node.dependencies.contains(&reference) {
                node.dependencies.push(reference.clone());
            }
        }

        // Add to reference's dependents
        if let Some(node) = self.nodes.get_mut(&reference) {
            if !node.dependents.contains(&referrer) {
                node.dependents.push(referrer);
            }
        }
    }

    /// Get a node by drv_id
    pub fn get_node(&self, drv_id: &DrvId) -> Option<&GraphNode> {
        self.nodes.get(drv_id)
    }

    /// Get all drvs in a particular state
    pub fn get_drvs_by_state(&self, state: &DrvBuildState) -> Vec<DrvId> {
        self.by_state
            .get(state)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Update the build state of a drv
    pub fn update_state(&mut self, drv_id: &DrvId, new_state: DrvBuildState) {
        let Some(node) = self.nodes.get_mut(drv_id) else {
            return;
        };

        let old_state = std::mem::replace(&mut node.build_state, new_state.clone());

        // Update state index
        if let Some(old_set) = self.by_state.get_mut(&old_state) {
            old_set.remove(drv_id);
        }

        self.by_state
            .entry(new_state.clone())
            .or_insert_with(HashSet::new)
            .insert(drv_id.clone());

        // Update failed tracking
        match new_state {
            DrvBuildState::Completed(DrvBuildResult::Failure) => {
                self.failed_drvs.insert(drv_id.clone());
            }
            DrvBuildState::Completed(DrvBuildResult::Success) => {
                self.failed_drvs.remove(drv_id);
            }
            _ => {}
        }
    }

    /// Get the direct dependencies of a drv
    pub fn get_dependencies(&self, drv_id: &DrvId) -> Vec<DrvId> {
        self.nodes
            .get(drv_id)
            .map(|node| node.dependencies.clone())
            .unwrap_or_default()
    }

    /// Get the direct dependents (referrers) of a drv
    pub fn get_dependents(&self, drv_id: &DrvId) -> Vec<DrvId> {
        self.nodes
            .get(drv_id)
            .map(|node| node.dependents.clone())
            .unwrap_or_default()
    }

    /// Check if a drv is buildable (all dependencies completed successfully)
    pub fn is_buildable(&self, drv_id: &DrvId) -> bool {
        let Some(node) = self.nodes.get(drv_id) else {
            return false;
        };

        // Check if all dependencies are successfully completed
        node.dependencies.iter().all(|dep_id| {
            self.nodes.get(dep_id).map_or(false, |dep| {
                dep.build_state == DrvBuildState::Completed(DrvBuildResult::Success)
            })
        })
    }

    /// Get all transitive dependents of a drv using BFS
    pub fn get_all_transitive_dependents(&self, drv_id: &DrvId) -> Vec<DrvId> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();

        queue.push_back(drv_id.clone());
        visited.insert(drv_id.clone());

        while let Some(current) = queue.pop_front() {
            let Some(node) = self.nodes.get(&current) else {
                continue;
            };

            for dependent_id in &node.dependents {
                if visited.insert(dependent_id.clone()) {
                    queue.push_back(dependent_id.clone());
                }
            }
        }

        // Remove the starting node itself
        visited.remove(drv_id);
        visited.into_iter().collect()
    }

    /// Propagate failure from a drv to all its transitive dependents
    /// Returns the list of drvs that were marked as TransitiveFailure
    pub fn propagate_failure(&mut self, failed_drv: &DrvId) -> Vec<DrvId> {
        let blocked = self.get_all_transitive_dependents(failed_drv);

        for blocked_id in &blocked {
            // Update state to TransitiveFailure
            if let Some(node) = self.nodes.get_mut(blocked_id) {
                let old_state = std::mem::replace(
                    &mut node.build_state,
                    DrvBuildState::TransitiveFailure,
                );

                // Update state index
                if let Some(old_set) = self.by_state.get_mut(&old_state) {
                    old_set.remove(blocked_id);
                }

                // Add blocking failure
                node.blocking_failures.insert(failed_drv.clone());
            }

            // Add to TransitiveFailure state index
            self.by_state
                .entry(DrvBuildState::TransitiveFailure)
                .or_insert_with(HashSet::new)
                .insert(blocked_id.clone());
        }

        // Record the transitive failure mapping
        self.transitive_failure_map
            .insert(failed_drv.clone(), blocked.iter().cloned().collect());

        blocked
    }

    /// Clear failure status from a drv that now succeeds
    /// Returns the list of drvs that were unblocked
    pub fn clear_failure(&mut self, formerly_failed: &DrvId) -> Vec<DrvId> {
        let Some(blocked_set) = self.transitive_failure_map.remove(formerly_failed) else {
            return Vec::new();
        };

        let mut unblocked = Vec::new();

        for blocked_id in blocked_set {
            let Some(node) = self.nodes.get_mut(&blocked_id) else {
                continue;
            };

            // Remove this failure from the blocking set
            node.blocking_failures.remove(formerly_failed);

            // If no other failures block it, mark as queued
            if node.blocking_failures.is_empty() {
                let old_state = std::mem::replace(&mut node.build_state, DrvBuildState::Queued);

                // Update state indexes
                if let Some(old_set) = self.by_state.get_mut(&old_state) {
                    old_set.remove(&blocked_id);
                }

                self.by_state
                    .entry(DrvBuildState::Queued)
                    .or_insert_with(HashSet::new)
                    .insert(blocked_id.clone());

                unblocked.push(blocked_id);
            }
        }

        unblocked
    }

    /// Get the failed dependencies blocking a drv
    pub fn get_failed_dependencies(&self, drv_id: &DrvId) -> Vec<DrvId> {
        self.nodes
            .get(drv_id)
            .map(|node| node.blocking_failures.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get count of nodes in the graph
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Initialize the graph from database on startup
    /// Normalizes transient states to Queued and recomputes transitive failures
    pub async fn from_database(db_service: &crate::db::DbService) -> anyhow::Result<Self> {
        let pool = &db_service.pool;

        // Load all drvs using query_as to properly deserialize
        let drvs: Vec<Drv> = sqlx::query_as(
            "SELECT drv_path, system, required_system_features, is_fod, build_state FROM Drv",
        )
        .fetch_all(pool)
        .await?;

        let mut graph = BuildGraph::new();

        // Insert all nodes, normalizing transient states
        for mut drv in drvs {
            // Normalize transient states to Queued
            drv.build_state = match drv.build_state {
                DrvBuildState::Building
                | DrvBuildState::Buildable
                | DrvBuildState::Queued
                | DrvBuildState::FailedRetry => DrvBuildState::Queued,
                terminal => terminal,
            };

            // Set prefer_local_build to false (it's skipped in FromRow)
            drv.prefer_local_build = false;

            graph.insert_node(drv);
        }

        // Load all edges
        let refs: Vec<(String, String)> =
            sqlx::query_as("SELECT referrer, reference FROM DrvRefs")
                .fetch_all(pool)
                .await?;

        for (referrer, reference) in refs {
            let referrer_id = std::str::FromStr::from_str(&referrer)?;
            let reference_id = std::str::FromStr::from_str(&reference)?;

            graph.add_edge(referrer_id, reference_id);
        }

        // Recompute transitive failures from permanent failures
        let failed_drvs: Vec<DrvId> = graph.failed_drvs.iter().cloned().collect();
        for failed_drv in failed_drvs {
            graph.propagate_failure(&failed_drv);
        }

        Ok(graph)
    }
}

impl Default for BuildGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_drv(hash: &str, state: DrvBuildState) -> Drv {
        use std::str::FromStr;

        let drv_path = format!("{}-test.drv", hash);
        Drv {
            drv_path: DrvId::from_str(&drv_path).unwrap(),
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: state,
        }
    }

    #[test]
    fn test_insert_and_get_node() {
        let mut graph = BuildGraph::new();
        let drv = make_test_drv("abc123", DrvBuildState::Queued);
        let drv_id = drv.drv_path.clone();

        graph.insert_node(drv);

        assert!(graph.get_node(&drv_id).is_some());
        assert_eq!(graph.node_count(), 1);
    }

    #[test]
    fn test_add_edge() {
        let mut graph = BuildGraph::new();

        let drv_a = make_test_drv("aaa", DrvBuildState::Queued);
        let drv_b = make_test_drv("bbb", DrvBuildState::Completed(DrvBuildResult::Success));

        let id_a = drv_a.drv_path.clone();
        let id_b = drv_b.drv_path.clone();

        graph.insert_node(drv_a);
        graph.insert_node(drv_b);

        // A depends on B
        graph.add_edge(id_a.clone(), id_b.clone());

        let deps = graph.get_dependencies(&id_a);
        assert_eq!(deps.len(), 1);
        assert!(deps.contains(&id_b));

        let dependents = graph.get_dependents(&id_b);
        assert_eq!(dependents.len(), 1);
        assert!(dependents.contains(&id_a));
    }

    #[test]
    fn test_is_buildable() {
        let mut graph = BuildGraph::new();

        let drv_a = make_test_drv("aaa", DrvBuildState::Queued);
        let drv_b = make_test_drv("bbb", DrvBuildState::Completed(DrvBuildResult::Success));
        let drv_c = make_test_drv("ccc", DrvBuildState::Queued);

        let id_a = drv_a.drv_path.clone();
        let id_b = drv_b.drv_path.clone();
        let id_c = drv_c.drv_path.clone();

        graph.insert_node(drv_a);
        graph.insert_node(drv_b);
        graph.insert_node(drv_c);

        // A depends on B (success) and C (queued)
        graph.add_edge(id_a.clone(), id_b.clone());
        graph.add_edge(id_a.clone(), id_c.clone());

        // A is not buildable because C is not complete
        assert!(!graph.is_buildable(&id_a));

        // Update C to success
        graph.update_state(&id_c, DrvBuildState::Completed(DrvBuildResult::Success));

        // Now A is buildable
        assert!(graph.is_buildable(&id_a));
    }

    #[test]
    fn test_propagate_failure() {
        let mut graph = BuildGraph::new();

        // Create a chain: A -> B -> C (A depends on B, B depends on C)
        let drv_a = make_test_drv("aaa", DrvBuildState::Queued);
        let drv_b = make_test_drv("bbb", DrvBuildState::Queued);
        let drv_c = make_test_drv("ccc", DrvBuildState::Queued);

        let id_a = drv_a.drv_path.clone();
        let id_b = drv_b.drv_path.clone();
        let id_c = drv_c.drv_path.clone();

        graph.insert_node(drv_a);
        graph.insert_node(drv_b);
        graph.insert_node(drv_c);

        graph.add_edge(id_a.clone(), id_b.clone());
        graph.add_edge(id_b.clone(), id_c.clone());

        // C fails
        graph.update_state(&id_c, DrvBuildState::Completed(DrvBuildResult::Failure));
        let blocked = graph.propagate_failure(&id_c);

        // Both A and B should be blocked
        assert_eq!(blocked.len(), 2);
        assert!(blocked.contains(&id_a));
        assert!(blocked.contains(&id_b));

        // Check states
        assert_eq!(
            graph.get_node(&id_a).unwrap().build_state,
            DrvBuildState::TransitiveFailure
        );
        assert_eq!(
            graph.get_node(&id_b).unwrap().build_state,
            DrvBuildState::TransitiveFailure
        );
    }

    #[test]
    fn test_clear_failure() {
        let mut graph = BuildGraph::new();

        let drv_a = make_test_drv("aaa", DrvBuildState::Queued);
        let drv_b = make_test_drv("bbb", DrvBuildState::Completed(DrvBuildResult::Failure));

        let id_a = drv_a.drv_path.clone();
        let id_b = drv_b.drv_path.clone();

        graph.insert_node(drv_a);
        graph.insert_node(drv_b);

        graph.add_edge(id_a.clone(), id_b.clone());

        // Propagate failure from B
        graph.propagate_failure(&id_b);

        assert_eq!(
            graph.get_node(&id_a).unwrap().build_state,
            DrvBuildState::TransitiveFailure
        );

        // B now succeeds
        graph.update_state(&id_b, DrvBuildState::Completed(DrvBuildResult::Success));
        let unblocked = graph.clear_failure(&id_b);

        // A should be unblocked
        assert_eq!(unblocked.len(), 1);
        assert!(unblocked.contains(&id_a));
        assert_eq!(
            graph.get_node(&id_a).unwrap().build_state,
            DrvBuildState::Queued
        );
    }
}
