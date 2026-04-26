use std::sync::Arc;

use anyhow::{Context, Result};
use prometheus::Registry;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc::{Sender, channel};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::auth::{JwtService, OAuthConfig};
use crate::checks::types::CheckTask;
use crate::ci::RepoReader;
use crate::client::UnixService;
use crate::config::Config;
use crate::db::DbService;
use crate::git::GitService;
use crate::github::{self, GitHubService};
use crate::graph::{GraphCommand, GraphService, GraphServiceHandle};
use crate::metrics::{ChangeSummaryMetrics, GraphMetrics, NixEvalMetrics, WebhookMetrics};
use crate::nix::{EvalService, EvalTask};
use crate::scheduler::{IngressTask, SchedulerService};
use crate::services::checks::ChecksExecutor;
use crate::web::WebService;

mod async_service;
pub use async_service::AsyncService;

mod checks;

pub mod websocket;
pub use websocket::WebSocketService;

pub async fn start_services(config: Config) -> Result<()> {
    let db_service = DbService::new(&config.db_path)
        .await
        .context("attempted to create DB pool")?;

    let db_pool = db_service.pool.clone();

    // Prune stale RebuildImpactCache rows on startup. The cache is a
    // memoisation layer for `/v1/commits/{sha}/rebuild-impact`; rows
    // older than 7 days are very unlikely to be queried again and
    // letting the table grow unbounded would slowly bloat the SQLite
    // file. Cleanup is best-effort: a failure here logs at WARN but
    // does not abort startup.
    match crate::change_summary::cache::cleanup_old_entries(
        &db_pool,
        crate::change_summary::cache::DEFAULT_CACHE_TTL_DAYS,
    )
    .await
    {
        Ok(0) => {},
        Ok(n) => info!("Pruned {n} stale RebuildImpactCache rows on startup"),
        Err(err) => warn!("RebuildImpactCache startup cleanup failed: {err:?}"),
    }

    // Wrap GitHub App configs in Arc for sharing across services
    let github_app_configs = Arc::new(config.github_apps.clone());

    // Try to register GitHub App from configuration first, then fall back to env vars
    let github_app_config = if !config.github_apps.is_empty() {
        // Use the first configured GitHub App for backward compatibility
        // Permission checks will be enforced in webhook handlers
        config.github_apps.values().next()
    } else {
        None
    };

    // Register GitHub app first (but don't create service yet)
    let maybe_octocrab = match github::register_app_from_config(github_app_config).await {
        Ok(octocrab) => Some(octocrab),
        Err(e) => {
            // In dev environments, there usually is no authentication, but the server should
            // still be runnable. If someone however tried to configure
            // authentication, make sure to tell them load and clear if there
            // was a problem.
            if matches!(e, github::AppRegistrationError::InvalidEnv(_)) {
                warn!(
                    "Skipping GitHub app registration: {}",
                    anyhow::Chain::new(&e)
                        .map(|e| e.to_string())
                        .collect::<Vec<_>>()
                        .join(": ")
                );
            } else {
                Err(e).context("failed to register GitHub app")?;
            }
            None
        },
    };

    // Create shared metrics registry for all services
    let metrics_registry = Arc::new(Registry::new());

    // Create GraphService for in-memory build state tracking
    let (graph_command_sender, graph_command_receiver) = channel::<GraphCommand>(1000);

    // Create GraphMetrics and register with shared registry
    let graph_metrics =
        GraphMetrics::new(&metrics_registry).context("failed to create GraphMetrics")?;

    let graph_service = GraphService::new(
        db_service.clone(),
        graph_command_receiver,
        Some(graph_metrics),
        config.graph_lru_capacity,
    )
    .await
    .context("failed to initialize GraphService")?;
    let graph_handle = graph_service.handle(graph_command_sender.clone());

    // Register ChangeSummaryMetrics on the shared registry so the
    // change-summary endpoints + GitHub posting handler can observe.
    let change_summary_metrics = ChangeSummaryMetrics::new(&metrics_registry)
        .context("failed to register change-summary metrics")?;

    // Create GitHubService
    let maybe_github_service = if let Some(ref octocrab) = maybe_octocrab {
        Some(
            GitHubService::new(
                db_service.clone(),
                octocrab.clone(),
                graph_handle.clone(),
                Some(change_summary_metrics.clone()),
            )
            .await?,
        )
    } else {
        None
    };
    info!(
        "GraphService initialized with {} drvs",
        graph_handle.node_count()
    );

    // Create WebSocket service for real-time updates
    let websocket_service = WebSocketService::new();
    let websocket_sender = Some(websocket_service.event_sender());

    let maybe_github_sender = maybe_github_service.as_ref().map(|x| x.get_sender());

    // Wrap cache configs in Arc for sharing across services
    let cache_configs = Arc::new(config.caches.clone());

    let scheduler_service = SchedulerService::new(
        db_service.clone(),
        config.logs_dir.clone(),
        config.remote_builders,
        maybe_github_sender.clone(),
        config.build_no_output_timeout_seconds,
        config.build_max_duration_seconds,
        websocket_sender,
        graph_command_sender.clone(),
        graph_handle.clone(),
        metrics_registry.clone(),
        cache_configs,
        config.security.max_hook_timeout_seconds,
        config.security.audit_hooks,
    )
    .await?;
    let (eval_sender, eval_receiver) = channel::<EvalTask>(1000);

    // M4: register nix-eval-jobs observability metrics on the shared
    // Prometheus registry so `/v1/metrics` exposes truncation counters
    // and output-size histograms to operators.
    let nix_eval_metrics = NixEvalMetrics::new(&scheduler_service.metrics_registry())
        .context("failed to register nix-eval-jobs metrics")?;

    let eval_service = EvalService::new(
        eval_receiver,
        db_service.clone(),
        scheduler_service.ingress_request_sender(),
        maybe_github_sender.clone(),
        graph_command_sender.clone(),
        Some(nix_eval_metrics),
    );

    // Create ChecksExecutor service
    let (check_sender, check_receiver) = channel::<CheckTask>(1000);
    let checks_service = ChecksExecutor::new(
        check_receiver,
        db_service.clone(),
        maybe_github_sender.clone(),
    );

    let maybe_github_sender = maybe_github_service.as_ref().map(|x| x.get_sender());
    let repo_service = RepoReader::new(
        eval_sender.clone(),
        Some(check_sender.clone()),
        maybe_github_sender.clone(),
        db_service.clone(),
    )?;
    let repo_sender = repo_service.get_sender();

    let git_service = GitService::new(repo_sender.clone())?;

    // Create JWT service and OAuth config for authentication.
    // M2: the JWT secret is stored as `Redacted<String>` so that it
    // cannot leak through `Debug` formatting of `Config`; expose the
    // raw string here because `JwtService::new` takes `&str`.
    let jwt_service = JwtService::new(config.oauth.jwt_secret.expose());
    let oauth_config = OAuthConfig {
        client_id: config.oauth.client_id.clone(),
        client_secret: config.oauth.client_secret.clone(),
        redirect_url: config.oauth.redirect_url.clone(),
    };

    // Initialize GitHub API client for permission checking
    let github_client = Arc::new(crate::auth::GitHubApiClient::new());

    // Register WebhookMetrics with the scheduler's metrics registry so
    // `/v1/metrics` scrapes include webhook signature counters.
    let webhook_metrics = WebhookMetrics::new(&scheduler_service.metrics_registry())
        .context("failed to register webhook metrics")?;

    let web_service = WebService::bind_to_address(
        &config.web.address,
        git_service.get_sender(),
        maybe_github_sender.clone(),
        Some(scheduler_service.ingress_request_sender()),
        maybe_octocrab,
        scheduler_service.metrics_registry(),
        config.require_approval,
        config.merge_queue_require_approval,
        db_service.clone(),
        graph_handle.clone(),
        jwt_service,
        oauth_config,
        config.logs_dir.clone(),
        websocket_service.clone(),
        github_app_configs.clone(),
        config.security.webhook_secret.clone(),
        config.security.allow_insecure_webhooks,
        webhook_metrics,
        github_client,
        config.default_merge_method.clone(),
        config.web.allowed_origins.clone(),
        Some(change_summary_metrics.clone()),
    )
    .await
    .context("failed to start web service")?;

    let unix_service = UnixService::bind_to_path(
        &config.unix.socket_path,
        eval_sender.clone(),
        repo_sender.clone(),
        db_service.clone(),
        git_service.get_sender(),
        scheduler_service.ingress_request_sender(),
    )
    .await
    .context("failed to start unix service")?;

    // Use `bind_addr` instead of the `addr` + `port` given by the user, to ensure the printed
    // address is always correct (even for funny things like setting the port to 0).
    info!(
        "Serving Eka CI web service on http://{}",
        web_service.bind_addr(),
    );
    info!(
        "Listening for client connection on {}",
        unix_service
            .bind_addr()
            .as_pathname()
            .map_or("<<unnamed socket>>".to_owned(), |path| path
                .display()
                .to_string())
    );

    let cancellation_token = CancellationToken::new();

    let graph_handle_task = tokio::spawn(graph_service.run(cancellation_token.clone()));
    let git_handle = tokio::spawn(git_service.run(cancellation_token.clone()));
    let repo_handle = tokio::spawn(repo_service.run(cancellation_token.clone()));
    let eval_handle = tokio::spawn(eval_service.run(cancellation_token.clone()));
    let checks_handle = tokio::spawn(checks_service.run(cancellation_token.clone()));
    let unix_handle = tokio::spawn(unix_service.run(cancellation_token.clone()));
    let web_handle = tokio::spawn(web_service.run(cancellation_token.clone()));
    let github_handle =
        maybe_github_service.map(|svc| tokio::spawn(svc.run(cancellation_token.clone())));

    let mut sigterm = signal(SignalKind::terminate()).context("failed to get sigterm handle")?;
    let mut sigint = signal(SignalKind::interrupt()).context("failed to get sigint handle")?;

    enqueue_buildable_builds(
        graph_handle.clone(),
        scheduler_service.ingress_request_sender(),
    )
    .await?;

    tokio::select! {
        biased;
        _ = sigterm.recv() => {
            info!("Received SIGTERM, gracefully shutting down");
        }
        _ = sigint.recv() => {
            info!("Received SIGINT, gracefully shutting down");
        }
    }

    cancellation_token.cancel();

    // Wait for the services to shutdown
    _ = tokio::join!(
        graph_handle_task,
        eval_handle,
        checks_handle,
        unix_handle,
        web_handle,
        repo_handle,
        git_handle
    );

    // The GitHub service is only spawned when configured; await it
    // explicitly here so shutdown waits for a clean exit and any join
    // error is surfaced.
    if let Some(handle) = github_handle {
        if let Err(e) = handle.await {
            warn!("GitHub service task exited with error: {:?}", e);
        }
    }

    db_pool.close().await;

    info!("Database service pool closed");
    info!("All services shutdown gracefully");

    Ok(())
}

async fn enqueue_buildable_builds(
    graph_handle: GraphServiceHandle,
    ingress_sender: Sender<IngressTask>,
) -> Result<()> {
    let queued_drvs = graph_handle.get_buildable_drvs().await?;

    info!("Checking {} drvs for build candidates", queued_drvs.len());
    for drv_id in queued_drvs {
        let ingress_task = IngressTask::CheckBuildable(std::sync::Arc::new(drv_id));
        ingress_sender.send(ingress_task).await?;
    }

    Ok(())
}
