// Thin wrapper around evaluator::service::jobs that adds server-specific logic

use std::path::PathBuf;

use anyhow::Result;

use crate::nix::{NixEvalDrv, NixEvalError};

impl super::EvalService {
    /// Run `iets eval-jobs` and optionally traverse all evaluated drvs to
    /// populate the dependency graph and ingress queue.
    ///
    /// `traverse` should be `true` for head-commit (PR) evaluations
    /// that feed into the build scheduler, and `false` for base-commit
    /// evaluations that only need the evaluated attr/drv list for
    /// jobset diff computation.
    pub async fn run_eval_jobs(
        &self,
        file_path: &str,
        extra_roots: &[PathBuf],
        traverse: bool,
    ) -> Result<(Vec<NixEvalDrv>, Vec<NixEvalError>)> {
        // Create a metrics adapter if metrics are available
        let metrics: Option<&dyn evaluator::traits::EvalMetricsCollector> = self
            .nix_eval_metrics
            .as_ref()
            .map(|m| m.as_ref() as &dyn evaluator::traits::EvalMetricsCollector);

        // Create the traverse callback that captures self
        let traverse_fn =
            |drv_path: &str,
             input_drvs: &Option<std::collections::HashMap<String, Vec<String>>>| {
                let _ = (drv_path, input_drvs); // Suppress unused warnings
                Ok(())
            };

        let (jobs, errors) = evaluator::service::run_eval_jobs(
            &self.eval_sandbox,
            file_path,
            extra_roots,
            metrics,
            Some(traverse_fn),
        )
        .await?;

        // Traverse after full parse — only when building (head commit).
        // Base-commit evals skip this since they only need attr/drv
        // data for jobset diff computation, not dependency graphs.
        if traverse {
            self.batch_traverse(&jobs).await?;
        }

        Ok((jobs, errors))
    }
}
