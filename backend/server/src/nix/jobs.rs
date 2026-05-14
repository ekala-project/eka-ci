// Thin wrapper around evaluator::service::jobs that adds server-specific logic

use anyhow::Result;
use tracing::warn;

use crate::nix::{NixEvalDrv, NixEvalError};

impl super::EvalService {
    pub async fn run_nix_eval_jobs(
        &self,
        file_path: &str,
    ) -> Result<(Vec<NixEvalDrv>, Vec<NixEvalError>)> {
        // Create a metrics adapter if metrics are available
        let metrics: Option<&dyn evaluator::traits::EvalMetricsCollector> =
            self.nix_eval_metrics.as_ref().map(|m| m.as_ref() as &dyn evaluator::traits::EvalMetricsCollector);

        // Create the traverse callback that captures self
        let traverse_fn = |drv_path: &str, input_drvs: &Option<std::collections::HashMap<String, Vec<String>>>| {
            // We need to call traverse_drvs synchronously, but it's async
            // For now, just return Ok since the actual traversal happens after all jobs are processed
            let _ = (drv_path, input_drvs); // Suppress unused warnings
            Ok(())
        };

        // Call the evaluator's run_nix_eval_jobs
        let (jobs, errors) = evaluator::service::run_nix_eval_jobs(
            file_path,
            metrics,
            Some(traverse_fn),
        )
        .await?;

        // Traverse after full parse
        for drv in &jobs {
            if let Err(e) = self.traverse_drvs(&drv.drv_path, &drv.input_drvs).await {
                warn!("Issue while traversing {} drv: {:?}", &drv.drv_path, e);
            }
        }

        Ok((jobs, errors))
    }
}
