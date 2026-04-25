use std::collections::{HashMap, HashSet, VecDeque};
use std::num::NonZeroUsize;

use lru::LruCache;

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
    /// All nodes in the graph, indexed by drv_id with LRU eviction policy
    pub(crate) nodes: LruCache<DrvId, GraphNode>,

    /// Index of drvs by their build state for fast queries
    by_state: HashMap<DrvBuildState, HashSet<DrvId>>,

    /// Set of drvs that have permanently failed
    failed_drvs: HashSet<DrvId>,

    /// Map from failed drv to all drvs transitively blocked by it
    transitive_failure_map: HashMap<DrvId, HashSet<DrvId>>,

    /// Set of pinned nodes that should never be evicted (non-terminal states)
    pinned: HashSet<DrvId>,
}

impl BuildGraph {
    /// Create a new empty build graph with LRU cache
    pub fn new(capacity: usize) -> Self {
        Self {
            nodes: LruCache::new(NonZeroUsize::new(capacity).unwrap()),
            by_state: HashMap::new(),
            failed_drvs: HashSet::new(),
            transitive_failure_map: HashMap::new(),
            pinned: HashSet::new(),
        }
    }

    /// Check if a state is terminal (can be evicted)
    fn is_terminal_state(state: &DrvBuildState) -> bool {
        matches!(
            state,
            DrvBuildState::Completed(_) | DrvBuildState::TransitiveFailure
        )
    }

    /// Insert a new node into the graph
    /// Returns the evicted node (if any) for metrics tracking
    pub fn insert_node(&mut self, drv: Drv) -> Option<(DrvId, GraphNode)> {
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

        // Pin non-terminal states to prevent eviction
        if !Self::is_terminal_state(&state) {
            self.pinned.insert(drv_id.clone());
        }

        // Insert into LRU cache (push returns the evicted entry if any)
        self.nodes.push(drv_id, node)
    }

    /// Add a dependency edge: referrer depends on reference
    /// referrer -> reference (referrer needs reference to build)
    pub fn add_edge(&mut self, referrer: DrvId, reference: DrvId) {
        // Add to referrer's dependencies (use peek_mut to avoid LRU update)
        if let Some(node) = self.nodes.peek_mut(&referrer) {
            if !node.dependencies.contains(&reference) {
                node.dependencies.push(reference.clone());
            }
        }

        // Add to reference's dependents (use peek_mut to avoid LRU update)
        if let Some(node) = self.nodes.peek_mut(&reference) {
            if !node.dependents.contains(&referrer) {
                node.dependents.push(referrer);
            }
        }
    }

    /// Get a node by drv_id (doesn't update LRU)
    pub fn get_node(&self, drv_id: &DrvId) -> Option<&GraphNode> {
        self.nodes.peek(drv_id)
    }

    /// Get the count of pinned nodes
    pub fn pinned_count(&self) -> usize {
        self.pinned.len()
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
        let Some(node) = self.nodes.peek_mut(drv_id) else {
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
            },
            DrvBuildState::Completed(DrvBuildResult::Success) => {
                self.failed_drvs.remove(drv_id);
            },
            _ => {},
        }

        // Update pinned set: pin non-terminal, unpin terminal
        if Self::is_terminal_state(&new_state) {
            self.pinned.remove(drv_id);
        } else {
            self.pinned.insert(drv_id.clone());
        }
    }

    /// Get the direct dependencies of a drv
    pub fn get_dependencies(&self, drv_id: &DrvId) -> Vec<DrvId> {
        self.nodes
            .peek(drv_id)
            .map(|node| node.dependencies.clone())
            .unwrap_or_default()
    }

    /// Get the direct dependents (referrers) of a drv
    pub fn get_dependents(&self, drv_id: &DrvId) -> Vec<DrvId> {
        self.nodes
            .peek(drv_id)
            .map(|node| node.dependents.clone())
            .unwrap_or_default()
    }

    /// Check if a drv is buildable (all dependencies completed successfully).
    ///
    /// Only used by unit tests that exercise the authoritative `BuildGraph`
    /// dependency logic. Production code paths read from the lock-free
    /// `GraphServiceHandle::is_buildable` mirror (`graph/service.rs`), which
    /// is populated from `BuildGraph` but queried via `DashMap` to stay off
    /// the service's message-passing loop.
    #[cfg(test)]
    pub fn is_buildable(&self, drv_id: &DrvId) -> bool {
        let Some(node) = self.nodes.peek(drv_id) else {
            return false;
        };

        // Check if all dependencies are successfully completed
        node.dependencies.iter().all(|dep_id| {
            self.nodes.peek(dep_id).map_or(false, |dep| {
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
            let Some(node) = self.nodes.peek(&current) else {
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

    /// Multi-source reverse-reachability BFS.
    ///
    /// Computes the union of transitive dependents reachable from any drv in
    /// `seeds`, traversing the `dependents` adjacency. Uses a single shared
    /// `visited` set so each node is visited at most once across the whole
    /// fan-out — significantly cheaper than running independent BFSes when
    /// seeds share descendants.
    ///
    /// The seeds themselves are included in the returned set when they are
    /// present in the graph; callers wanting "strict descendants only" should
    /// subtract the seed set.
    ///
    /// Seeds whose `DrvId` is not in the graph are silently skipped (they
    /// contribute nothing to the BFS). This matches the semantics of
    /// [`Self::get_all_transitive_dependents`].
    ///
    /// Used by [`crate::change_summary`] (A2 rebuild impact) for the
    /// "blast radius" computation described in design §8.2.
    pub fn reverse_reachable_from_set(&self, seeds: &[DrvId]) -> HashSet<DrvId> {
        let mut visited: HashSet<DrvId> = HashSet::new();
        let mut queue: VecDeque<DrvId> = VecDeque::new();

        for seed in seeds {
            // Only enqueue seeds that exist in the graph; missing seeds are a
            // no-op (matches `get_all_transitive_dependents`).
            if self.nodes.peek(seed).is_some() && visited.insert(seed.clone()) {
                queue.push_back(seed.clone());
            }
        }

        while let Some(current) = queue.pop_front() {
            let Some(node) = self.nodes.peek(&current) else {
                continue;
            };

            for dependent_id in &node.dependents {
                if visited.insert(dependent_id.clone()) {
                    queue.push_back(dependent_id.clone());
                }
            }
        }

        visited
    }

    /// Per-seed blast radius: for each seed, the count of strict transitive
    /// dependents reachable from that seed.
    ///
    /// Runs an independent BFS for each seed because seeds can share
    /// descendants — a shared-visited multi-source BFS would under-count
    /// individual seeds' radii. The seed itself is excluded from its own
    /// count (so a leaf drv has radius `0`).
    ///
    /// Seeds whose `DrvId` is not in the graph map to `0`.
    ///
    /// Used to identify "critical path" packages — those whose change forces
    /// the largest dependent set rebuild.
    pub fn blast_radius_per_seed(&self, seeds: &[DrvId]) -> HashMap<DrvId, usize> {
        let mut out = HashMap::with_capacity(seeds.len());
        for seed in seeds {
            // Reuse the existing per-seed BFS; it already excludes the seed.
            let count = if self.nodes.peek(seed).is_some() {
                self.get_all_transitive_dependents(seed).len()
            } else {
                0
            };
            // Last writer wins on duplicate seeds — count is deterministic
            // either way since BFS is pure.
            out.insert(seed.clone(), count);
        }
        out
    }

    /// Propagate failure from a drv to all its transitive dependents
    /// Returns the list of drvs that were marked as TransitiveFailure
    pub fn propagate_failure(&mut self, failed_drv: &DrvId) -> Vec<DrvId> {
        let blocked = self.get_all_transitive_dependents(failed_drv);

        for blocked_id in &blocked {
            // Update state to TransitiveFailure
            if let Some(node) = self.nodes.peek_mut(blocked_id) {
                let old_state =
                    std::mem::replace(&mut node.build_state, DrvBuildState::TransitiveFailure);

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
            let Some(node) = self.nodes.peek_mut(&blocked_id) else {
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
            .peek(drv_id)
            .map(|node| node.blocking_failures.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get count of nodes in the graph
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Get the capacity of the LRU cache
    pub fn capacity(&self) -> usize {
        self.nodes.cap().get()
    }

    /// Get capacity utilization as a percentage (0.0 - 1.0)
    pub fn utilization(&self) -> f64 {
        self.nodes.len() as f64 / self.nodes.cap().get() as f64
    }

    /// Estimate memory usage of the graph in bytes
    pub fn estimate_memory_bytes(&self) -> usize {
        let mut total = 0;

        for (_id, node) in &self.nodes {
            // Base node size estimate
            total += 300;

            // DrvId storage in vectors
            total += node.dependencies.len() * 80;
            total += node.dependents.len() * 80;

            // Blocking failures HashSet
            total += node.blocking_failures.len() * 80;
        }

        // by_state index overhead
        for (_state, set) in &self.by_state {
            total += set.len() * 80;
        }

        // failed_drvs set
        total += self.failed_drvs.len() * 80;

        // transitive_failure_map
        for (_drv, blocked_set) in &self.transitive_failure_map {
            total += 80; // Key
            total += blocked_set.len() * 80; // Value set
        }

        total
    }

    /// Initialize the graph from database on startup
    /// Normalizes transient states to Queued and recomputes transitive failures
    pub async fn from_database(
        db_service: &crate::db::DbService,
        capacity: usize,
    ) -> anyhow::Result<Self> {
        let pool = &db_service.pool;

        // Load all drvs using query_as to properly deserialize
        let drvs: Vec<Drv> = sqlx::query_as(
            "SELECT drv_path, system, required_system_features, is_fod, build_state, output_size, \
             closure_size, pname, version, license_json, maintainers_json, meta_position, broken, \
             insecure FROM Drv",
        )
        .fetch_all(pool)
        .await?;

        let mut graph = BuildGraph::new(capacity);

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
        let refs: Vec<(String, String)> = sqlx::query_as("SELECT referrer, reference FROM DrvRefs")
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
        Self::new(100_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_drv(hash: &str, state: DrvBuildState) -> Drv {
        use std::str::FromStr;

        // Pad the test-provided short tag out to a valid 32-char nix
        // base32 hash. The test code only needs distinct hashes, not
        // cryptographic validity, so we left-extend with `0` (which is
        // part of the nix base32 alphabet) and map any characters that
        // fall outside nix's custom alphabet (`e`, `o`, `t`, `u`) to the
        // nearest alphabet byte so that call sites can keep using
        // readable tags like `"old"`, `"new"`, `"success"`.
        const HASH_LEN: usize = 32;
        assert!(hash.len() <= HASH_LEN, "test hash tag too long");
        let sanitized: String = hash
            .chars()
            .map(|c| match c {
                'e' => 'f',
                'o' => 'p',
                't' => 's',
                'u' => 'v',
                other => other,
            })
            .collect();
        let padded_hash: String = std::iter::repeat('0')
            .take(HASH_LEN - sanitized.len())
            .chain(sanitized.chars())
            .collect();
        let drv_path = format!("{}-test.drv", padded_hash);
        Drv {
            drv_path: DrvId::from_str(&drv_path).unwrap(),
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: state,
            output_size: None,
            closure_size: None,
            pname: None,
            version: None,
            license_json: None,
            maintainers_json: None,
            meta_position: None,
            broken: None,
            insecure: None,
        }
    }

    #[test]
    fn test_insert_and_get_node() {
        let mut graph = BuildGraph::new(1000);
        let drv = make_test_drv("abc123", DrvBuildState::Queued);
        let drv_id = drv.drv_path.clone();

        graph.insert_node(drv);

        assert!(graph.get_node(&drv_id).is_some());
        assert_eq!(graph.node_count(), 1);
    }

    #[test]
    fn test_add_edge() {
        let mut graph = BuildGraph::new(1000);

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
        let mut graph = BuildGraph::new(1000);

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
        let mut graph = BuildGraph::new(1000);

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
    fn test_reverse_reachable_from_set_single_seed() {
        let mut graph = BuildGraph::new(1000);

        // A -> B -> C  (A depends on B, B depends on C)
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

        // From C we should reach C, B, A
        let reachable = graph.reverse_reachable_from_set(&[id_c.clone()]);
        assert_eq!(reachable.len(), 3);
        assert!(reachable.contains(&id_a));
        assert!(reachable.contains(&id_b));
        assert!(reachable.contains(&id_c));
    }

    #[test]
    fn test_reverse_reachable_from_set_shared_descendants() {
        // Diamond:
        //       A
        //      / \
        //     B   C
        //      \ /
        //       D     (A depends on B and C; B and C depend on D)
        let mut graph = BuildGraph::new(1000);
        let drv_a = make_test_drv("aaa", DrvBuildState::Queued);
        let drv_b = make_test_drv("bbb", DrvBuildState::Queued);
        let drv_c = make_test_drv("ccc", DrvBuildState::Queued);
        let drv_d = make_test_drv("ddd", DrvBuildState::Queued);
        let id_a = drv_a.drv_path.clone();
        let id_b = drv_b.drv_path.clone();
        let id_c = drv_c.drv_path.clone();
        let id_d = drv_d.drv_path.clone();
        graph.insert_node(drv_a);
        graph.insert_node(drv_b);
        graph.insert_node(drv_c);
        graph.insert_node(drv_d);
        graph.add_edge(id_a.clone(), id_b.clone());
        graph.add_edge(id_a.clone(), id_c.clone());
        graph.add_edge(id_b.clone(), id_d.clone());
        graph.add_edge(id_c.clone(), id_d.clone());

        // Seeds = {B, C}. Union covers B, C, A (shared dependent).
        let reachable = graph.reverse_reachable_from_set(&[id_b.clone(), id_c.clone()]);
        assert_eq!(reachable.len(), 3);
        assert!(reachable.contains(&id_a));
        assert!(reachable.contains(&id_b));
        assert!(reachable.contains(&id_c));
        assert!(!reachable.contains(&id_d));
    }

    #[test]
    fn test_reverse_reachable_from_set_missing_seed() {
        use std::str::FromStr;

        let mut graph = BuildGraph::new(1000);
        let drv_a = make_test_drv("aaa", DrvBuildState::Queued);
        let id_a = drv_a.drv_path.clone();
        graph.insert_node(drv_a);

        let phantom =
            DrvId::from_str("/nix/store/00000000000000000000000000000099-phantom.drv").unwrap();

        // Mix of valid + invalid seed: only the valid one should contribute.
        let reachable = graph.reverse_reachable_from_set(&[id_a.clone(), phantom.clone()]);
        assert_eq!(reachable.len(), 1);
        assert!(reachable.contains(&id_a));
        assert!(!reachable.contains(&phantom));
    }

    #[test]
    fn test_reverse_reachable_from_set_empty_seeds() {
        let graph = BuildGraph::new(1000);
        let reachable = graph.reverse_reachable_from_set(&[]);
        assert!(reachable.is_empty());
    }

    #[test]
    fn test_blast_radius_per_seed() {
        // A -> B -> C  ;  D (isolated leaf)
        let mut graph = BuildGraph::new(1000);
        let drv_a = make_test_drv("aaa", DrvBuildState::Queued);
        let drv_b = make_test_drv("bbb", DrvBuildState::Queued);
        let drv_c = make_test_drv("ccc", DrvBuildState::Queued);
        let drv_d = make_test_drv("ddd", DrvBuildState::Queued);
        let id_a = drv_a.drv_path.clone();
        let id_b = drv_b.drv_path.clone();
        let id_c = drv_c.drv_path.clone();
        let id_d = drv_d.drv_path.clone();
        graph.insert_node(drv_a);
        graph.insert_node(drv_b);
        graph.insert_node(drv_c);
        graph.insert_node(drv_d);
        graph.add_edge(id_a.clone(), id_b.clone());
        graph.add_edge(id_b.clone(), id_c.clone());

        let radii =
            graph.blast_radius_per_seed(&[id_a.clone(), id_b.clone(), id_c.clone(), id_d.clone()]);

        // A has nothing depending on it => 0.
        assert_eq!(radii.get(&id_a).copied(), Some(0));
        // B has A => 1.
        assert_eq!(radii.get(&id_b).copied(), Some(1));
        // C has B and A => 2.
        assert_eq!(radii.get(&id_c).copied(), Some(2));
        // D has nothing => 0.
        assert_eq!(radii.get(&id_d).copied(), Some(0));
    }

    #[test]
    fn test_blast_radius_per_seed_missing_seed() {
        use std::str::FromStr;

        let graph = BuildGraph::new(1000);
        let phantom =
            DrvId::from_str("/nix/store/00000000000000000000000000000099-phantom.drv").unwrap();
        let radii = graph.blast_radius_per_seed(&[phantom.clone()]);
        assert_eq!(radii.get(&phantom).copied(), Some(0));
    }

    #[test]
    fn test_clear_failure() {
        let mut graph = BuildGraph::new(1000);

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
