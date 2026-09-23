use std::collections::HashMap;

use anyhow::Result;

/// Unified interface for Nix store operations.
///
/// Two implementations exist:
/// - `SubprocessNixStore` — shells out to nix-store/nix CLI (fallback)
/// - `DaemonNixStore` — speaks the daemon wire protocol via harmonia
#[async_trait::async_trait]
pub trait NixStore: Send + Sync {
    /// Check whether a store path exists in the local store.
    async fn is_valid_path(&self, store_path: &str) -> Result<bool>;

    /// Query the direct build-time dependencies of a derivation.
    /// Only returns `.drv` paths (filters out inputSrcs).
    async fn query_references(&self, drv_path: &str) -> Result<Vec<String>>;

    /// Query all transitive dependencies of a derivation.
    /// Only returns `.drv` paths (filters out inputSrcs).
    async fn query_requisites(&self, drv_path: &str) -> Result<Vec<String>>;

    /// Query the output map of a derivation (output name → store path).
    async fn query_derivation_output_map(&self, drv_path: &str) -> Result<HashMap<String, String>>;

    /// Query path info: NAR size and number of references.
    async fn query_path_info(&self, store_path: &str) -> Result<PathInfo>;

    /// Ping the store to check availability. Returns Ok(()) if reachable.
    async fn store_ping(&self) -> Result<()>;
}

/// Subset of ValidPathInfo relevant to EkaCI.
#[derive(Debug, Clone)]
pub struct PathInfo {
    pub nar_size: u64,
    pub references: Vec<String>,
}
