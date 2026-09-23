use std::collections::HashMap;

use anyhow::{Context, Result};
use harmonia_store_remote::{ConnectionPool, DaemonStore, PoolConfig};

use crate::store::{NixStore, PathInfo};

/// Default nix daemon socket path.
const DAEMON_SOCKET: &str = "/nix/var/nix/daemon-socket/socket";

/// NixStore implementation using the nix daemon wire protocol via harmonia.
///
/// Connects to the local nix daemon over the unix socket and uses a
/// connection pool to amortize handshake overhead across thousands of
/// queries per evaluation.
pub struct DaemonNixStore {
    pool: ConnectionPool,
}

impl DaemonNixStore {
    /// Create a new DaemonNixStore with a connection pool.
    /// `pool_size` controls the maximum number of concurrent daemon connections.
    pub fn new(pool_size: usize) -> Self {
        let config = PoolConfig {
            max_size: pool_size,
            ..Default::default()
        };
        let pool = ConnectionPool::new(DAEMON_SOCKET, config);
        Self { pool }
    }

    /// Parse a store path string into harmonia's StorePath type.
    fn parse_store_path(path: &str) -> Result<harmonia_store_path::StorePath> {
        path.parse()
            .map_err(|e| anyhow::anyhow!("invalid store path '{}': {:?}", path, e))
    }
}

#[async_trait::async_trait]
impl NixStore for DaemonNixStore {
    async fn is_valid_path(&self, store_path: &str) -> Result<bool> {
        let sp = Self::parse_store_path(store_path)?;
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("daemon pool: {:?}", e))?;
        let result = conn
            .execute(|client| async move { client.is_valid_path(&sp).await })
            .await
            .map_err(|e| anyhow::anyhow!("is_valid_path failed: {:?}", e))?;
        Ok(result)
    }

    async fn query_references(&self, drv_path: &str) -> Result<Vec<String>> {
        let info = self.query_path_info(drv_path).await?;
        let drvs = info
            .references
            .into_iter()
            .filter(|r| r.ends_with(".drv"))
            .collect();
        Ok(drvs)
    }

    async fn query_requisites(&self, drv_path: &str) -> Result<Vec<String>> {
        // BFS over references (transitively).
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        let mut result = Vec::new();

        queue.push_back(drv_path.to_string());

        while let Some(path) = queue.pop_front() {
            if !visited.insert(path.clone()) {
                continue;
            }

            let info = match self.query_path_info(&path).await {
                Ok(info) => info,
                Err(_) => continue,
            };

            for reference in &info.references {
                if reference.ends_with(".drv") && !visited.contains(reference) {
                    queue.push_back(reference.clone());
                }
            }

            if path.ends_with(".drv") && path != drv_path {
                result.push(path);
            }
        }

        Ok(result)
    }

    async fn query_derivation_output_map(&self, drv_path: &str) -> Result<HashMap<String, String>> {
        let sp = Self::parse_store_path(drv_path)?;
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("daemon pool: {:?}", e))?;
        let output_map = conn
            .execute(|client| async move { client.query_derivation_output_map(&sp).await })
            .await
            .map_err(|e| anyhow::anyhow!("query_derivation_output_map: {:?}", e))?;

        Ok(output_map
            .into_iter()
            .filter_map(|(name, maybe_path)| {
                maybe_path.map(|path| (name.to_string(), path.to_string()))
            })
            .collect())
    }

    async fn query_path_info(&self, store_path: &str) -> Result<PathInfo> {
        let sp = Self::parse_store_path(store_path)?;
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("daemon pool: {:?}", e))?;
        let info = conn
            .execute(|client| async move { client.query_path_info(&sp).await })
            .await
            .map_err(|e| anyhow::anyhow!("query_path_info failed: {:?}", e))?
            .context("path not found in store")?;

        Ok(PathInfo {
            nar_size: info.nar_size,
            references: info.references.iter().map(|r| r.to_string()).collect(),
        })
    }

    async fn store_ping(&self) -> Result<()> {
        let _conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("daemon pool: {:?}", e))?;
        Ok(())
    }
}
