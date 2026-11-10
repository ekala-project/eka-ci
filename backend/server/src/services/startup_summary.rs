use tracing::{info, warn};

use crate::config::Config;

pub(crate) struct StartupSummary {
    version: &'static str,
    github_apps: Vec<String>,
    gitea_instances: Vec<String>,
    gitlab_instances: Vec<String>,
    channels: Vec<(String, String, String)>,
    caches: Vec<String>,
    mcp_enabled: bool,
    search_index_enabled: bool,
    graph_lru_capacity: usize,
}

impl StartupSummary {
    pub(crate) fn capture(config: &Config) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            github_apps: config.github_apps.keys().cloned().collect(),
            gitea_instances: config.gitea_instances.keys().cloned().collect(),
            gitlab_instances: config.gitlab_instances.keys().cloned().collect(),
            channels: config
                .channels
                .values()
                .map(|c| {
                    (
                        c.channel_id(),
                        c.tracking_branch.clone(),
                        c.target_branch.clone(),
                    )
                })
                .collect(),
            caches: config.caches.keys().cloned().collect(),
            mcp_enabled: config.mcp.as_ref().is_some_and(|m| m.enabled),
            search_index_enabled: config.search_index.is_some(),
            graph_lru_capacity: config.graph_lru_capacity,
        }
    }

    pub(crate) fn log(&self) {
        info!("──────────────────────────────────────────────");
        info!("Eka CI v{} — startup complete", self.version);
        info!("──────────────────────────────────────────────");

        if self.github_apps.is_empty()
            && self.gitea_instances.is_empty()
            && self.gitlab_instances.is_empty()
        {
            warn!("No forge integrations configured — webhooks will not be processed");
        } else {
            for id in &self.github_apps {
                info!("  GitHub App: {id}");
            }
            for domain in &self.gitea_instances {
                info!("  Gitea instance: {domain}");
            }
            for domain in &self.gitlab_instances {
                info!("  GitLab instance: {domain}");
            }
        }

        if self.channels.is_empty() {
            info!("  Channels: none (gated promotion disabled)");
        } else {
            for (id, tracking, target) in &self.channels {
                info!("  Channel {id}: {tracking} -> {target}");
            }
        }

        if !self.caches.is_empty() {
            for id in &self.caches {
                info!("  Cache: {id}");
            }
        }

        if self.mcp_enabled {
            info!("  MCP endpoint: enabled at /v1/mcp");
        }
        if self.search_index_enabled {
            info!("  Search index generation: enabled");
        }

        info!("  Graph LRU capacity: {}", self.graph_lru_capacity);
        info!("──────────────────────────────────────────────");
        info!("Ready — waiting for webhook events");
    }
}
