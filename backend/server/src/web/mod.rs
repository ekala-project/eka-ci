// Web service module - HTTP API and webhooks

mod handlers;
mod logs;
mod maintainers;
mod pull_requests;
mod repositories;
mod responses;
mod routes;
mod security;
mod state;
mod webhooks;

use std::net::{SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use prometheus::Registry;
use routes::{api_routes, gitea_routes, github_routes, gitlab_routes};
use security::build_cors_layer;
use state::AppState;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tower_http::services::{ServeDir, ServeFile};
use tracing::{error, info, warn};

use crate::auth::{JwtService, OAuthConfig};
use crate::git::GitTask;
use crate::github::GitHubTask;
use crate::metrics::{ChangeSummaryMetrics, WebhookMetrics};
use crate::scheduler::IngressTask;

pub struct WebService {
    listener: TcpListener,
    state: AppState,
}

impl WebService {
    #[allow(clippy::too_many_arguments)]
    pub async fn bind_to_address(
        socket: &SocketAddrV4,
        git_sender: mpsc::Sender<GitTask>,
        github_sender: Option<mpsc::Sender<GitHubTask>>,
        gitlab_sender: Option<mpsc::Sender<crate::gitlab::GitLabTask>>,
        gitea_sender: Option<mpsc::Sender<crate::gitea::GiteaTask>>,
        ingress_sender: Option<mpsc::Sender<IngressTask>>,
        octocrab: Option<octocrab::Octocrab>,
        metrics_registry: Arc<Registry>,
        require_approval: bool,
        merge_queue_require_approval: bool,
        db_service: crate::db::DbService,
        graph_handle: crate::graph::GraphServiceHandle,
        jwt_service: JwtService,
        oauth_config: OAuthConfig,
        logs_dir: PathBuf,
        static_dir: PathBuf,
        websocket_service: crate::services::WebSocketService,
        github_app_configs: Arc<std::collections::HashMap<String, crate::config::GitHubAppConfig>>,
        webhook_secret: Option<crate::secret::Redacted<String>>,
        allow_insecure_webhooks: bool,
        webhook_metrics: Arc<WebhookMetrics>,
        github_client: Arc<crate::auth::GitHubApiClient>,
        default_merge_method: String,
        allowed_origins: Vec<String>,
        change_summary_metrics: Option<Arc<ChangeSummaryMetrics>>,
    ) -> Result<Self> {
        let listener = TcpListener::bind(socket)
            .await
            .context(format!("failed to bind to tcp socket at {socket}"))?;

        match (&webhook_secret, allow_insecure_webhooks) {
            (Some(_), _) => {
                info!(event = "webhook_signature_verification_enabled");
            },
            (None, true) => {
                warn!(
                    event = "webhook_signature_verification_disabled",
                    "Webhook signature verification is DISABLED (allow_insecure_webhooks=true)."
                );
            },
            (None, false) => {
                warn!(
                    event = "webhook_secret_missing_strict_mode",
                    "No webhook secret configured; incoming webhooks will be rejected with 503."
                );
            },
        }

        Ok(Self {
            listener,
            state: AppState {
                git_sender,
                github_sender,
                gitlab_sender,
                gitea_sender,
                ingress_sender,
                octocrab,
                metrics_registry,
                require_approval,
                merge_queue_require_approval,
                db_service,
                graph_handle,
                jwt_service,
                oauth_config,
                logs_dir,
                static_dir,
                websocket_service,
                github_app_configs,
                webhook_secret,
                allow_insecure_webhooks,
                webhook_metrics,
                github_client,
                default_merge_method,
                allowed_origins,
                change_summary_metrics,
            },
        })
    }

    pub fn bind_addr(&self) -> SocketAddr {
        self.listener
            .local_addr()
            .expect("getsockname should always succeed on a properly initialized listener")
    }

    pub async fn run(self, cancellation_token: CancellationToken) {
        let static_dir = &self.state.static_dir;

        info!("Serving static files from: {:?}", static_dir);

        let serve_dir = ServeDir::new(&static_dir)
            .not_found_service(ServeFile::new(static_dir.join("index.html")));

        let cors = build_cors_layer(&self.state.allowed_origins);

        let app = Router::new()
            .nest("/v1", api_routes())
            .nest("/github", github_routes())
            .nest("/gitlab", gitlab_routes())
            .nest("/gitea", gitea_routes())
            .fallback_service(serve_dir)
            .layer(cors)
            .with_state(self.state);

        if let Err(e) = axum::serve(
            self.listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            cancellation_token.cancelled().await;
            info!("Web service shutting down")
        })
        .await
        {
            error!(error = %e, "Failed to start web service");
            return;
        }

        info!("Web service shutdown gracefully")
    }
}
