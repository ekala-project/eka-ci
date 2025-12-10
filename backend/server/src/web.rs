use std::net::{SocketAddr, SocketAddrV4};

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Json, Path, State};
use axum::routing::{get, post};
use octocrab::models::webhook_events::WebhookEventPayload as WEP;
use sqlx::{Pool, Sqlite};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::db::github::CheckRun;
use crate::git::GitTask;
use crate::github::GitHubTask;

#[derive(Clone)]
struct AppState {
    git_sender: mpsc::Sender<GitTask>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    db_pool: Pool<Sqlite>,
}

pub struct WebService {
    listener: TcpListener,
    state: AppState,
}

impl WebService {
    pub async fn bind_to_address(
        socket: &SocketAddrV4,
        git_sender: mpsc::Sender<GitTask>,
        github_sender: Option<mpsc::Sender<GitHubTask>>,
        db_pool: Pool<Sqlite>,
    ) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind(socket)
            .await
            .context(format!("failed to bind to tcp socket at {socket}"))?;

        Ok(Self {
            listener,
            state: AppState {
                git_sender,
                github_sender,
                db_pool,
            },
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
            .with_state(self.state);

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

fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/logs/{drv}", get(get_derivation_log))
        .route("/github/webhook", post(handle_github_webhook))
        .route("/commits/{sha}/check_runs", get(get_check_runs_for_commit))
}

async fn handle_github_webhook(State(state): State<AppState>, Json(webhook_payload): Json<WEP>) {
    use crate::github::handle_webhook_payload;

    handle_webhook_payload(webhook_payload, state.git_sender, state.github_sender).await;
}

async fn get_derivation_log(axum::extract::Path(drv): axum::extract::Path<String>) -> String {
    format!("Dummy log data for {drv}")
}

async fn get_check_runs_for_commit(
    State(state): State<AppState>,
    Path(sha): Path<String>,
) -> Json<Vec<CheckRun>> {
    // Query for all check_runs for this commit (not just active ones)
    let check_runs = sqlx::query_as::<Sqlite, CheckRun>(
        r#"
        SELECT DISTINCT c.check_run_id, c.repo_name, c.repo_owner, d.build_state, d.drv_path
        FROM CheckRunInfo c
        INNER JOIN Drv d ON c.drv_id = d.ROWID
        INNER JOIN Job j ON j.drv_id = d.ROWID
        INNER JOIN GitHubJobSets g ON j.jobset = g.ROWID
        WHERE g.sha = ?
        "#,
    )
    .bind(&sha)
    .fetch_all(&state.db_pool)
    .await
    .unwrap_or_else(|e| {
        error!("Failed to fetch check_runs for commit {}: {}", sha, e);
        vec![]
    });

    Json(check_runs)
}
