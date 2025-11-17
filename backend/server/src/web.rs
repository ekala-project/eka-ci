use std::net::{SocketAddr, SocketAddrV4};

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Json, State};
use axum::routing::{get, post};
use octocrab::models::webhook_events::WebhookEventPayload as WEP;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::git::GitTask;

pub struct WebService {
    listener: TcpListener,
    git_sender: mpsc::Sender<GitTask>,
}

impl WebService {
    pub async fn bind_to_address(
        socket: &SocketAddrV4,
        git_sender: mpsc::Sender<GitTask>,
    ) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind(socket)
            .await
            .context(format!("failed to bind to tcp socket at {socket}"))?;

        Ok(Self {
            listener,
            git_sender,
        })
    }

    pub fn bind_addr(&self) -> SocketAddr {
        // If the call fails either the system ran out of resources or libc is broken, for both of
        // these cases a panic seems appropiate.
        self.listener
            .local_addr()
            .expect("getsockname should always succeed on a properly initialized listener")
    }

    pub async fn run(self, cancellation_token: CancellationToken) {
        let app = Router::new()
            .nest("/v1", api_routes())
            .with_state(self.git_sender);

        if let Err(e) = axum::serve(self.listener, app)
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

fn api_routes() -> Router<mpsc::Sender<GitTask>> {
    Router::new()
        .route("/logs/{drv}", get(get_derivation_log))
        .route("/github/webhook", post(handle_github_webhook))
}

async fn handle_github_webhook(
    State(git_sender): State<mpsc::Sender<GitTask>>,
    Json(webhook_payload): Json<WEP>,
) {
    // We can assume that handling this route means that the git sender is available
    crate::github::handle_webhook_payload(webhook_payload, git_sender).await;
}

async fn get_derivation_log(axum::extract::Path(drv): axum::extract::Path<String>) -> String {
    format!("Dummy log data for {drv}")
}
