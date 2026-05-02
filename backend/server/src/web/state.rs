// AppState and related types for the web service

use std::path::PathBuf;
use std::sync::Arc;

use prometheus::Registry;
use tokio::sync::mpsc;

use crate::auth::{JwtService, OAuthConfig};
use crate::git::GitTask;
use crate::github::GitHubTask;
use crate::metrics::{ChangeSummaryMetrics, WebhookMetrics};
use crate::scheduler::IngressTask;

#[derive(Clone)]
pub(super) struct AppState {
    pub(super) git_sender: mpsc::Sender<GitTask>,
    pub(super) github_sender: Option<mpsc::Sender<GitHubTask>>,
    pub(super) gitlab_sender: Option<mpsc::Sender<crate::gitlab::GitLabTask>>,
    pub(super) gitea_sender: Option<mpsc::Sender<crate::gitea::GiteaTask>>,
    pub(super) ingress_sender: Option<mpsc::Sender<IngressTask>>,
    pub(super) octocrab: Option<octocrab::Octocrab>,
    pub(super) metrics_registry: Arc<Registry>,
    pub(super) require_approval: bool,
    pub(super) merge_queue_require_approval: bool,
    pub(super) db_service: crate::db::DbService,
    pub(super) graph_handle: crate::graph::GraphServiceHandle,
    pub(super) jwt_service: JwtService,
    pub(super) oauth_config: OAuthConfig,
    pub(super) logs_dir: PathBuf,
    pub(super) static_dir: PathBuf,
    pub(super) websocket_service: crate::services::WebSocketService,
    pub(super) github_app_configs:
        Arc<std::collections::HashMap<String, crate::config::GitHubAppConfig>>,
    // M2: wrap so the secret cannot leak through any future `Debug`
    // formatting of `AppState` or a struct embedding it.
    pub(super) webhook_secret: Option<crate::secret::Redacted<String>>,
    /// H1: opt-in escape hatch. When `true` and `webhook_secret` is
    /// `None`, the webhook handler accepts unsigned payloads (for
    /// local development only). Production startup refuses to set
    /// both simultaneously unless the operator has explicitly asked
    /// for it.
    pub(super) allow_insecure_webhooks: bool,
    pub(super) webhook_metrics: Arc<WebhookMetrics>,
    pub(super) github_client: Arc<crate::auth::GitHubApiClient>,
    pub(super) default_merge_method: String,
    /// M1: configured CORS allow-list. Each entry is a full origin
    /// (scheme + host + optional port). Exact-match; no wildcards. An
    /// empty list means no cross-origin requests are allowed.
    pub(super) allowed_origins: Vec<String>,
    /// Optional metrics for change-summary endpoint observability.
    pub(super) change_summary_metrics: Option<Arc<ChangeSummaryMetrics>>,
}

// Implement FromRef so extractors can access JwtService from AppState
impl axum::extract::FromRef<AppState> for JwtService {
    fn from_ref(state: &AppState) -> Self {
        state.jwt_service.clone()
    }
}

impl AppState {
    /// Build the narrower `crate::auth::oauth::AppState` used by OAuth handlers.
    pub(super) fn oauth_state(&self) -> crate::auth::oauth::AppState {
        crate::auth::oauth::AppState {
            db: self.db_service.pool.clone(),
            jwt_service: self.jwt_service.clone(),
            oauth_config: self.oauth_config.clone(),
        }
    }
}
