// Output and closure size validation

use std::sync::Arc;

use tracing::{debug, info, warn};

use super::RecorderWorker;
use crate::db::github::JobInfo;
use crate::db::model::DrvId;
use crate::github::GitHubTask;

impl RecorderWorker {
    /// Check output size and send warning if threshold exceeded
    ///
    /// This calculates the output size for a successful build, stores it in the database,
    /// and compares it against the baseline (base branch) if size checks are configured.
    /// If the size increase exceeds the threshold, sends a neutral GitHub check with warning.
    pub(super) async fn check_output_size(
        &self,
        drv_id: &DrvId,
        job_infos: &[JobInfo],
    ) -> anyhow::Result<()> {
        use crate::ci::config::CIConfig;
        use crate::db::size::{
            get_baseline_output_size, store_output_size, update_drv_output_size,
        };
        use crate::nix::size::get_output_sizes;

        // Get output name → path mapping for this derivation
        let outputs = match crate::nix::get_drv_outputs(&drv_id.store_path()).await {
            Ok(outputs) if !outputs.is_empty() => outputs,
            Ok(_) => {
                debug!(
                    "No output paths found for size check: {}",
                    drv_id.store_path()
                );
                return Ok(()); // Skip size check if no outputs
            },
            Err(e) => {
                debug!("Failed to query output paths for size check: {}", e);
                return Ok(()); // Skip size check on error
            },
        };

        let output_paths: Vec<String> = outputs.values().cloned().collect();

        // Calculate per-path output sizes using nix path-info
        let sizes_by_path = match get_output_sizes(&output_paths) {
            Ok(sizes) => sizes,
            Err(e) => {
                warn!(
                    "Failed to calculate output sizes for {}: {}",
                    drv_id.store_path(),
                    e
                );
                return Ok(()); // Skip size check if calculation fails
            },
        };

        // Aggregate total for Drv-level summary
        let output_size: u64 = sizes_by_path.values().sum();

        debug!(
            "Calculated output size for {}: {} bytes ({})",
            drv_id.store_path(),
            output_size,
            crate::nix::size::format_size(output_size)
        );

        // Store size in database for historical tracking
        let pool = &self.db_service.pool;

        // Update the Drv table
        if let Err(e) = update_drv_output_size(pool, &drv_id.store_path(), output_size).await {
            warn!("Failed to update drv output size: {}", e);
        }

        // Persist per-output sizes into DrvRuntimeRefs (requires capture_runtime_references
        // to have already created rows for each output).
        let drv_rowid: Option<i64> = sqlx::query_scalar("SELECT ROWID FROM Drv WHERE drv_path = ?")
            .bind(drv_id.store_path())
            .fetch_optional(pool)
            .await?;
        if let Some(drv_rowid) = drv_rowid {
            for (output_name, output_path) in &outputs {
                if let Some(size) = sizes_by_path.get(output_path) {
                    if let Err(e) = crate::db::runtime_refs::update_output_size(
                        pool,
                        drv_rowid,
                        output_name,
                        *size as i64,
                    )
                    .await
                    {
                        warn!(
                            "Failed to update per-output size for '{}' ({}): {}",
                            output_name, output_path, e
                        );
                    }
                }
            }
        }

        // For each job this drv belongs to, check if we have metadata to store historical size
        for job_info in job_infos {
            // Get git metadata for this build from the jobset
            let jobset_info =
                match crate::db::github::get_jobset_info(job_info.jobset_id, pool).await {
                    Ok(info) => info,
                    Err(e) => {
                        debug!(
                            "No jobset info found for jobset {}, skipping: {}",
                            job_info.jobset_id, e
                        );
                        continue;
                    },
                };

            // Construct git_repo URL
            let git_repo = format!(
                "https://github.com/{}/{}",
                jobset_info.owner, jobset_info.repo_name
            );

            // Store in historical table
            if let Err(e) = store_output_size(
                pool,
                &drv_id.store_path(),
                output_size,
                &jobset_info.sha,
                &git_repo,
            )
            .await
            {
                warn!("Failed to store output size history: {}", e);
            }

            // Get job config from jobset
            let job_config: Option<String> =
                sqlx::query_scalar("SELECT config_json FROM GitHubJobSets WHERE ROWID = ?")
                    .bind(job_info.jobset_id)
                    .fetch_optional(pool)
                    .await?
                    .flatten();

            let job_config = match job_config {
                Some(config_json) => config_json,
                None => {
                    debug!(
                        "No job config found for jobset {}, skipping size check",
                        job_info.jobset_id
                    );
                    continue;
                },
            };

            let ci_config: CIConfig = match serde_json::from_str(&job_config) {
                Ok(config) => config,
                Err(e) => {
                    warn!("Failed to parse job config for size check: {}", e);
                    continue;
                },
            };

            // Find the job configuration
            let job = match ci_config.jobs.get(&job_info.name) {
                Some(job) => job,
                None => {
                    debug!("Job {} not found in config", job_info.name);
                    continue;
                },
            };

            // Check if size check is configured
            let size_check = match &job.size_check {
                Some(sc) => sc,
                None => {
                    debug!("No size check configured for job {}", job_info.name);
                    continue;
                },
            };

            debug!(
                "Size check configured for job {}: max_increase={}%, base_branch={}",
                job_info.name, size_check.max_increase_percent, size_check.base_branch
            );

            // Get baseline size from base branch
            let baseline_size = match get_baseline_output_size(
                pool,
                &drv_id.store_path(),
                &git_repo,
                &size_check.base_branch,
            )
            .await?
            {
                Some(size) => size,
                None => {
                    debug!(
                        "No baseline size found for {} on branch {}, skipping size check",
                        drv_id.store_path(),
                        size_check.base_branch
                    );
                    continue;
                },
            };

            // Calculate percentage increase
            let increase_percent = if baseline_size > 0 {
                ((output_size as f64 - baseline_size as f64) / baseline_size as f64) * 100.0
            } else {
                0.0
            };

            debug!(
                "Size comparison for {}: baseline={} current={} increase={:.1}%",
                drv_id.store_path(),
                crate::nix::size::format_size(baseline_size),
                crate::nix::size::format_size(output_size),
                increase_percent
            );

            // Check if threshold exceeded
            if increase_percent > size_check.max_increase_percent {
                info!(
                    "Size threshold exceeded for {}: {:.1}% > {:.1}% (baseline={}, current={})",
                    drv_id.store_path(),
                    increase_percent,
                    size_check.max_increase_percent,
                    crate::nix::size::format_size(baseline_size),
                    crate::nix::size::format_size(output_size)
                );

                // Send GitHub task with size warning
                if let Some(github_sender) = &self.github_sender {
                    use crate::db::model::build_event::DrvBuildState;

                    let task = GitHubTask::UpdateBuildStatusWithSizeWarning {
                        drv_id: Arc::new(drv_id.clone()),
                        status: DrvBuildState::Completed(
                            crate::db::model::build_event::DrvBuildResult::Success,
                        ),
                        baseline_size,
                        current_size: output_size,
                        increase_percent,
                        threshold_percent: size_check.max_increase_percent,
                    };

                    github_sender.send(task).await?;
                    debug!("Sent size warning GitHub task for {}", drv_id.store_path());
                } else {
                    debug!("No GitHub sender available for size warning");
                }
            } else {
                debug!(
                    "Size check passed for {}: {:.1}% <= {:.1}%",
                    drv_id.store_path(),
                    increase_percent,
                    size_check.max_increase_percent
                );
            }
        }

        Ok(())
    }

    /// Check closure size and send warning if threshold exceeded
    ///
    /// This calculates the closure size for a successful build, stores it in the database,
    /// and compares it against the baseline (base branch) if size checks are configured.
    /// If the size increase exceeds the threshold, sends a neutral GitHub check with warning.
    // TODO: closure size will be a future feature
    #[allow(dead_code)]
    pub(super) async fn check_closure_size(
        &self,
        drv_id: &DrvId,
        job_infos: &[JobInfo],
    ) -> anyhow::Result<()> {
        use evaluator::nix_utils::size::get_closure_sizes;

        use crate::ci::config::CIConfig;
        use crate::db::size::{
            get_baseline_closure_size, store_closure_size, update_drv_closure_size,
        };

        // Get output name → path mapping for this derivation
        let outputs = match crate::nix::get_drv_outputs(&drv_id.store_path()).await {
            Ok(outputs) if !outputs.is_empty() => outputs,
            Ok(_) => {
                debug!(
                    "No output paths found for closure size check: {}",
                    drv_id.store_path()
                );
                return Ok(()); // Skip closure size check if no outputs
            },
            Err(e) => {
                debug!("Failed to query output paths for closure size check: {}", e);
                return Ok(()); // Skip closure size check on error
            },
        };

        let output_paths: Vec<String> = outputs.values().cloned().collect();

        // Calculate per-path closure sizes using nix path-info -S
        let closures_by_path = match get_closure_sizes(&output_paths) {
            Ok(sizes) => sizes,
            Err(e) => {
                warn!(
                    "Failed to calculate closure sizes for {}: {}",
                    drv_id.store_path(),
                    e
                );
                return Ok(()); // Skip closure size check if calculation fails
            },
        };

        // Aggregate total across all outputs for Drv-level summary
        let closure_size: u64 = closures_by_path.values().sum();

        debug!(
            "Calculated closure size for {}: {} bytes ({})",
            drv_id.store_path(),
            closure_size,
            crate::nix::size::format_size(closure_size)
        );

        // Store size in database for historical tracking
        let pool = &self.db_service.pool;

        // Update the Drv table
        if let Err(e) = update_drv_closure_size(pool, &drv_id.store_path(), closure_size).await {
            warn!("Failed to update drv closure size: {}", e);
        }

        // Persist per-output closure sizes into DrvRuntimeRefs (requires
        // capture_runtime_references to have already created rows per output).
        let drv_rowid: Option<i64> = sqlx::query_scalar("SELECT ROWID FROM Drv WHERE drv_path = ?")
            .bind(drv_id.store_path())
            .fetch_optional(pool)
            .await?;
        if let Some(drv_rowid) = drv_rowid {
            for (output_name, output_path) in &outputs {
                if let Some(size) = closures_by_path.get(output_path) {
                    if let Err(e) = crate::db::runtime_refs::update_closure_size(
                        pool,
                        drv_rowid,
                        output_name,
                        *size as i64,
                    )
                    .await
                    {
                        warn!(
                            "Failed to update per-output closure size for '{}' ({}): {}",
                            output_name, output_path, e
                        );
                    }
                }
            }
        }

        // For each job this drv belongs to, check if we have metadata to store historical size
        for job_info in job_infos {
            // Get git metadata for this build from the jobset
            let jobset_info =
                match crate::db::github::get_jobset_info(job_info.jobset_id, pool).await {
                    Ok(info) => info,
                    Err(e) => {
                        debug!(
                            "No jobset info found for jobset {}, skipping: {}",
                            job_info.jobset_id, e
                        );
                        continue;
                    },
                };

            // Construct git_repo URL
            let git_repo = format!(
                "https://github.com/{}/{}",
                jobset_info.owner, jobset_info.repo_name
            );

            // Store in historical table
            if let Err(e) = store_closure_size(
                pool,
                &drv_id.store_path(),
                closure_size,
                &jobset_info.sha,
                &git_repo,
            )
            .await
            {
                warn!("Failed to store closure size history: {}", e);
            }

            // Get job config from jobset
            let job_config: Option<String> =
                sqlx::query_scalar("SELECT config_json FROM GitHubJobSets WHERE ROWID = ?")
                    .bind(job_info.jobset_id)
                    .fetch_optional(pool)
                    .await?
                    .flatten();

            let job_config = match job_config {
                Some(config_json) => config_json,
                None => {
                    debug!(
                        "No job config found for jobset {}, skipping closure size check",
                        job_info.jobset_id
                    );
                    continue;
                },
            };

            let ci_config: CIConfig = match serde_json::from_str(&job_config) {
                Ok(config) => config,
                Err(e) => {
                    warn!("Failed to parse job config for closure size check: {}", e);
                    continue;
                },
            };

            // Find the job configuration
            let job = match ci_config.jobs.get(&job_info.name) {
                Some(job) => job,
                None => {
                    debug!("Job {} not found in config", job_info.name);
                    continue;
                },
            };

            // Check if size check is configured
            let size_check = match &job.size_check {
                Some(sc) => sc,
                None => {
                    debug!("No closure size check configured for job {}", job_info.name);
                    continue;
                },
            };

            debug!(
                "Closure size check configured for job {}: max_increase={}%, base_branch={}",
                job_info.name, size_check.max_increase_percent, size_check.base_branch
            );

            // Get baseline closure size from base branch
            let baseline_size = match get_baseline_closure_size(
                pool,
                &drv_id.store_path(),
                &git_repo,
                &size_check.base_branch,
            )
            .await?
            {
                Some(size) => size,
                None => {
                    debug!(
                        "No baseline closure size found for {} on branch {}, skipping closure \
                         size check",
                        drv_id.store_path(),
                        size_check.base_branch
                    );
                    continue;
                },
            };

            // Calculate percentage increase
            let increase_percent = if baseline_size > 0 {
                ((closure_size as f64 - baseline_size as f64) / baseline_size as f64) * 100.0
            } else {
                0.0
            };

            debug!(
                "Closure size comparison for {}: baseline={} current={} increase={:.1}%",
                drv_id.store_path(),
                crate::nix::size::format_size(baseline_size),
                crate::nix::size::format_size(closure_size),
                increase_percent
            );

            // Check if threshold exceeded
            if increase_percent > size_check.max_increase_percent {
                info!(
                    "Closure size threshold exceeded for {}: {:.1}% > {:.1}% (baseline={}, \
                     current={})",
                    drv_id.store_path(),
                    increase_percent,
                    size_check.max_increase_percent,
                    crate::nix::size::format_size(baseline_size),
                    crate::nix::size::format_size(closure_size)
                );

                // Note: Currently reusing the same size warning mechanism
                // In the future, we could create a separate GitHub check for closure size
                debug!(
                    "Closure size warning for {} would be sent to GitHub (if implemented)",
                    drv_id.store_path()
                );
            } else {
                debug!(
                    "Closure size check passed for {}: {:.1}% <= {:.1}%",
                    drv_id.store_path(),
                    increase_percent,
                    size_check.max_increase_percent
                );
            }
        }

        Ok(())
    }
}
