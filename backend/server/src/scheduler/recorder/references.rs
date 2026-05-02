// Runtime reference capture and storage

use std::collections::HashMap;

use tracing::{debug, warn};

use super::RecorderWorker;
use crate::db::github::JobInfo;
use crate::db::model::DrvId;
use crate::nix::{get_drv_outputs, output_references};

impl RecorderWorker {
    /// Capture and store runtime references (retained dependencies) per output for a successfully
    /// built derivation
    ///
    /// This queries what store paths are actually referenced by each output (runtime dependencies)
    /// and stores them separately for later comparison between commits.
    pub(super) async fn capture_runtime_references(
        &self,
        drv_id: &DrvId,
        _job_infos: &[JobInfo],
    ) -> anyhow::Result<()> {
        // Get output paths with their names for this derivation
        let outputs = match get_drv_outputs(&drv_id.store_path()).await {
            Ok(outputs) if !outputs.is_empty() => outputs,
            Ok(_) => {
                debug!(
                    "No output paths found for runtime refs capture: {}",
                    drv_id.store_path()
                );
                return Ok(()); // Skip if no outputs
            },
            Err(e) => {
                debug!("Failed to query output paths for runtime refs: {}", e);
                return Ok(()); // Skip on error
            },
        };

        // Query runtime references for each output, keeping them separate
        let mut refs_by_output: HashMap<String, Vec<String>> = HashMap::new();
        for (output_name, output_path) in &outputs {
            match output_references(output_path).await {
                Ok(refs) => {
                    refs_by_output.insert(output_name.clone(), refs);
                },
                Err(e) => {
                    warn!(
                        "Failed to query runtime references for output '{}' ({}): {}",
                        output_name, output_path, e
                    );
                    // Continue with other outputs even if one fails
                },
            }
        }

        if refs_by_output.is_empty() {
            debug!("No runtime references found for {}", drv_id.store_path());
            return Ok(());
        }

        // Count total refs across all outputs for logging
        let total_refs: usize = refs_by_output.values().map(|v| v.len()).sum();
        debug!(
            "Captured {} runtime references across {} outputs for {}",
            total_refs,
            refs_by_output.len(),
            drv_id.store_path()
        );

        // Get the drv ROWID from database to use as foreign key
        let pool = &self.db_service.pool;
        let drv_rowid: Option<i64> = sqlx::query_scalar("SELECT ROWID FROM Drv WHERE drv_path = ?")
            .bind(drv_id.store_path())
            .fetch_optional(pool)
            .await?;

        let drv_rowid = match drv_rowid {
            Some(id) => id,
            None => {
                warn!(
                    "Could not find ROWID for drv_path {}, skipping runtime ref capture",
                    drv_id.store_path()
                );
                return Ok(());
            },
        };

        // Store runtime references for each output
        for (output_name, output_path) in &outputs {
            if let Some(refs) = refs_by_output.get(output_name) {
                if let Err(e) = crate::db::runtime_refs::store_runtime_references(
                    pool,
                    drv_rowid,
                    output_name,
                    output_path,
                    refs,
                )
                .await
                {
                    warn!(
                        "Failed to store runtime references for output '{}' ({}): {}",
                        output_name, output_path, e
                    );
                } else {
                    debug!(
                        "Stored {} runtime references for output '{}'",
                        refs.len(),
                        output_name
                    );
                }
            }
        }

        Ok(())
    }
}
