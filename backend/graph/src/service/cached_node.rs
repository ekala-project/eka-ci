// Cached read-only node data for lockfree concurrent access

use std::sync::Arc;

use shared::types::{Drv, DrvBuildState, DrvId};

/// Cached read-only node data for lockfree concurrent access
#[derive(Debug, Clone)]
pub struct CachedNode {
    pub drv_id: DrvId,
    pub system: String,
    pub required_system_features: Option<String>,
    pub is_fod: bool,
    pub build_state: DrvBuildState,
    /// Immutable shared reference to dependencies for cheap cloning
    pub dependencies: Arc<[DrvId]>,
}

impl CachedNode {
    pub(super) fn from_graph_node(node: &crate::graph::GraphNode) -> Self {
        Self {
            drv_id: node.drv_id.clone(),
            system: node.system.clone(),
            required_system_features: node.required_system_features.clone(),
            is_fod: node.is_fod,
            build_state: node.build_state.clone(),
            dependencies: node.dependencies.clone().into(),
        }
    }

    /// Convert CachedNode to Drv
    /// Note: prefer_local_build is always false as it's not persisted in DB
    pub fn to_drv(&self) -> Drv {
        Drv {
            drv_path: self.drv_id.clone(),
            system: self.system.clone(),
            prefer_local_build: false,
            required_system_features: self.required_system_features.clone(),
            is_fod: self.is_fod,
            build_state: self.build_state.clone(),
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
}
