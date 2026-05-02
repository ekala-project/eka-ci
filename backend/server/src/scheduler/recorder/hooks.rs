// Post-build hook execution

use anyhow::Context as _;
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

use super::RecorderWorker;
use crate::ci::config::Job;
use crate::db::model::drv_id;
use crate::hooks::types::HookTask;

impl RecorderWorker {
    /// Build a post-build hook command for pushing to a cache
    async fn build_cache_push_hook(
        cache_config: &crate::config::CacheConfig,
    ) -> anyhow::Result<crate::hooks::types::PostBuildHook> {
        use crate::config::CacheType;
        use crate::hooks::types::PostBuildHook;

        // Load credentials from configured source
        let credentials = cache_config.credentials.load().await.with_context(|| {
            format!("Failed to load credentials for cache '{}'", cache_config.id)
        })?;

        debug!(
            "Loaded credentials for cache '{}' from {:?}",
            cache_config.id, cache_config.credentials
        );

        // Build command based on cache type
        let command = match cache_config.cache_type {
            CacheType::NixCopy => {
                vec![
                    "nix".to_string(),
                    "copy".to_string(),
                    "--to".to_string(),
                    cache_config.destination.clone(),
                    "$OUT_PATHS".to_string(), // Will be expanded by HookExecutor
                ]
            },
            CacheType::Cachix => {
                vec![
                    "cachix".to_string(),
                    "push".to_string(),
                    cache_config.destination.clone(),
                    "$OUT_PATHS".to_string(),
                ]
            },
            CacheType::Attic => {
                vec![
                    "attic".to_string(),
                    "push".to_string(),
                    cache_config.destination.clone(),
                    "$OUT_PATHS".to_string(),
                ]
            },
        };

        Ok(PostBuildHook {
            name: format!("push-{}", cache_config.id),
            command,
            env: credentials,
        })
    }

    /// Execute post-build hooks for a successfully built drv
    /// This resolves cache references from job config and checks permissions
    pub(super) async fn execute_hooks_for_drv(&self, drv_id: &drv_id::DrvId) -> anyhow::Result<()> {
        use crate::cache_permissions::{PermissionContext, check_cache_permission};
        use crate::hooks::types::HookContext;

        // Return early if no hook sender configured
        let hook_sender: &mpsc::Sender<HookTask> = match &self.hook_sender {
            Some(sender) => sender,
            None => return Ok(()), // No hooks configured
        };

        // Get the job config from the database
        let config_json = match self.db_service.get_job_config_for_drv(drv_id).await? {
            Some(json) => json,
            None => return Ok(()), // No config stored for this drv
        };

        // Parse the job config
        let job: Job = serde_json::from_str(&config_json)?;

        // If no caches configured, return early
        if job.caches.is_empty() {
            return Ok(());
        }

        // Get drv info for hook context
        let drv_info = self
            .db_service
            .get_drv(drv_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Drv not found: {}", drv_id.store_path()))?;

        // Get job info for context (job name, commit sha, etc.)
        let job_infos = self.db_service.get_job_info_for_drv(drv_id).await?;
        let job_info = job_infos
            .first()
            .ok_or_else(|| anyhow::anyhow!("No job info found for drv"))?;

        // Get jobset info to extract commit SHA and repo details
        let jobset_info = self
            .db_service
            .get_jobset_by_id(job_info.jobset_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Jobset not found"))?;

        // Build permission context from jobset info
        let permission_context = PermissionContext {
            repo_owner: jobset_info.owner.clone(),
            repo_name: jobset_info.repo_name.clone(),
            branch: None, // TODO: Extract branch info from jobset if available
        };

        // Resolve cache IDs to cache configs and check permissions
        let mut hooks = Vec::new();
        for cache_id in &job.caches {
            // Look up cache config from server registry
            let cache_config = match self.cache_configs.get(cache_id) {
                Some(config) => config,
                None => {
                    warn!(
                        "Cache ID '{}' not found in server registry, skipping",
                        cache_id
                    );
                    continue;
                },
            };

            // Check if this repo/branch is allowed to use this cache
            if let Err(e) = check_cache_permission(cache_config, &permission_context) {
                warn!(
                    "Permission denied for cache '{}' in {}/{}: {}",
                    cache_id, permission_context.repo_owner, permission_context.repo_name, e
                );
                continue;
            }

            // Build actual hook command from cache config
            match Self::build_cache_push_hook(cache_config).await {
                Ok(hook) => {
                    debug!("Created cache push hook for cache '{}'", cache_id);
                    hooks.push(hook);
                },
                Err(e) => {
                    warn!(
                        "Failed to build cache push hook for '{}': {}. Skipping this cache.",
                        cache_id, e
                    );
                    // Continue to next cache instead of failing the entire hook execution
                    continue;
                },
            }
        }

        // If no hooks to execute after filtering, return early
        if hooks.is_empty() {
            return Ok(());
        }

        // Build hook context
        let context = HookContext {
            job_name: job_info.name.clone(),
            is_fod: drv_info.is_fod,
            system: drv_info.system.clone(),
            pname: None, // TODO: Query pname from DrvInfo if needed
            build_log_path: format!("logs/{}/build.log", drv_id.store_path()), /* TODO: Use actual log path */
            commit_sha: jobset_info.sha.clone(),
        };

        // Query the actual output paths from nix
        let out_paths = match crate::nix::get_drv_outputs(&drv_id.store_path()).await {
            Ok(outputs) if !outputs.is_empty() => outputs.into_values().collect::<Vec<String>>(),
            Ok(_) => {
                warn!(
                    "No output paths found for drv {}, using drv path as fallback",
                    drv_id.store_path()
                );
                vec![drv_id.store_path().to_string()]
            },
            Err(e) => {
                warn!(
                    "Failed to query output paths for drv {}: {}. Using drv path as fallback.",
                    drv_id.store_path(),
                    e
                );
                vec![drv_id.store_path().to_string()]
            },
        };

        // Create channel for receiving hook results
        let (result_sender, mut result_receiver) = mpsc::channel(hooks.len());

        // Create and send the hook task
        let hook_task = HookTask {
            drv_path: drv_id.store_path().to_string(),
            out_paths,
            hooks: hooks.clone(),
            context,
            result_sender: Some(result_sender),
        };

        hook_sender.send(hook_task).await?;

        // Spawn task to receive and store hook results
        let db_service = self.db_service.clone();
        let drv_path_for_task = drv_id.store_path().to_string();
        tokio::spawn(async move {
            while let Some(result) = result_receiver.recv().await {
                debug!(
                    "Received hook result for '{}' on drv {}",
                    result.hook_name, drv_path_for_task
                );

                // Store hook execution in database
                if let Err(e) = db_service
                    .insert_hook_execution(
                        &result.drv_path,
                        &result.hook_name,
                        result.started_at,
                        result.completed_at,
                        result.exit_code,
                        result.success,
                        &result.log_path,
                    )
                    .await
                {
                    error!(
                        "Failed to store hook execution for '{}' on drv {}: {}",
                        result.hook_name, drv_path_for_task, e
                    );
                }
            }
        });

        debug!(
            "Sent hook task for drv {} (job: {})",
            drv_id.store_path(),
            job_info.name
        );

        Ok(())
    }
}
