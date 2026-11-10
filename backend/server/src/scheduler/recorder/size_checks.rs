// Output and closure size validation

use std::collections::HashMap;

use tracing::{debug, warn};

use super::RecorderWorker;
use super::size_eval::{self, SizeWarning};
use crate::db::github::JobInfo;
use crate::db::model::DrvId;
use crate::db::size::SizeMetric;

/// Sizes of one metric measured for a derivation's outputs.
struct Measurement {
    /// Size of the derivation as a whole.
    total: u64,
    /// Size per output name.
    per_output: HashMap<String, u64>,
}

impl RecorderWorker {
    /// Measure and record the output and closure sizes of a successful
    /// build and compare them against the base-branch baseline of each job
    /// it belongs to.
    ///
    /// Returns the size warning to report on the derivation's check runs: of
    /// all jobs and both metrics, the one with the largest increase over its
    /// baseline. The caller sends it in place of the plain success update,
    /// so the two don't race.
    pub(super) async fn check_sizes(
        &self,
        drv_id: &DrvId,
        job_infos: &[JobInfo],
    ) -> anyhow::Result<Option<SizeWarning>> {
        // Get output name → path mapping for this derivation
        let outputs = match crate::nix::get_drv_outputs(&drv_id.store_path()).await {
            Ok(outputs) if !outputs.is_empty() => outputs,
            Ok(_) => {
                debug!(
                    "No output paths found for size check: {}",
                    drv_id.store_path()
                );
                return Ok(None);
            },
            Err(e) => {
                debug!("Failed to query output paths for size check: {}", e);
                return Ok(None);
            },
        };

        let pool = &self.db_service.pool;
        let mut worst = None;
        for metric in SizeMetric::ALL {
            let measurement = match measure(metric, &outputs) {
                Ok(measurement) => measurement,
                Err(e) => {
                    warn!(
                        "Failed to calculate {:?} size for {}: {}",
                        metric,
                        drv_id.store_path(),
                        e
                    );
                    continue;
                },
            };
            debug!(
                "Calculated {:?} size for {}: {}",
                metric,
                drv_id.store_path(),
                crate::nix::size::format_size(measurement.total)
            );

            let total = measurement.total;
            size_eval::persist_size(pool, drv_id, metric, total, &measurement.per_output).await?;
            let warning = size_eval::evaluate_size(pool, drv_id, job_infos, metric, total).await?;
            worst = SizeWarning::worse(worst, warning);
        }
        Ok(worst)
    }
}

/// Measure `metric` for the outputs (output name → store path) with
/// `nix path-info`.
///
/// The derivation's closure size is that of all outputs together, so
/// runtime dependencies shared by several outputs are counted once; the
/// per-output closure sizes each include them.
fn measure(metric: SizeMetric, outputs: &HashMap<String, String>) -> anyhow::Result<Measurement> {
    use crate::nix::size::{get_closure_sizes, get_combined_closure_size, get_output_sizes};

    let paths: Vec<String> = outputs.values().cloned().collect();
    let by_path = match metric {
        SizeMetric::Output => get_output_sizes(&paths)?,
        SizeMetric::Closure => get_closure_sizes(&paths)?,
    };
    let total = match metric {
        SizeMetric::Output => by_path.values().sum(),
        SizeMetric::Closure => get_combined_closure_size(&paths)?,
    };
    let per_output = outputs
        .iter()
        .filter_map(|(name, path)| Some((name.clone(), *by_path.get(path)?)))
        .collect();
    Ok(Measurement { total, per_output })
}
