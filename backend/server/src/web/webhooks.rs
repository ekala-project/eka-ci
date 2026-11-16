// Webhook handlers for GitHub, GitLab, and Gitea

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::Value;
use tracing::{info, warn};

use super::state::AppState;
use crate::webhook_security::verify_webhook_signature;

pub(super) async fn handle_github_webhook(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    // H1: signature verification is the primary gate. Three modes:
    //
    //   (a) secret configured            → require a valid signature
    //   (b) no secret, insecure allowed  → accept everything (dev only)
    //   (c) no secret, insecure forbidden → refuse with 503
    //
    // Mode (c) is defence in depth; `Config::from_env` already refuses
    // to start in this state, but constructing `WebService` directly
    // (e.g. in embedded integration tests) could bypass that.
    match (&state.webhook_secret, state.allow_insecure_webhooks) {
        (Some(secret), _) => {
            let signature = match headers.get("x-hub-signature-256") {
                Some(sig) => match sig.to_str() {
                    Ok(s) => s,
                    Err(e) => {
                        state
                            .webhook_metrics
                            .signature_failures_total
                            .with_label_values(&["invalid_header"])
                            .inc();
                        warn!(
                            event = "webhook_signature_invalid_header",
                            error = %e,
                            "Failed to parse X-Hub-Signature-256 header"
                        );
                        return (StatusCode::UNAUTHORIZED, "").into_response();
                    },
                },
                None => {
                    state
                        .webhook_metrics
                        .signature_failures_total
                        .with_label_values(&["missing_header"])
                        .inc();
                    warn!(
                        event = "webhook_signature_missing",
                        "Missing X-Hub-Signature-256 header on webhook request"
                    );
                    return (StatusCode::UNAUTHORIZED, "").into_response();
                },
            };

            if let Err(e) = verify_webhook_signature(secret.expose(), signature, &body) {
                state
                    .webhook_metrics
                    .signature_failures_total
                    .with_label_values(&["bad_signature"])
                    .inc();
                warn!(
                    event = "webhook_signature_verification_failed",
                    error = %e,
                    "Webhook signature verification failed"
                );
                return (StatusCode::UNAUTHORIZED, "").into_response();
            }

            state.webhook_metrics.signature_verified_total.inc();
            info!(
                event = "webhook_signature_verified",
                "Webhook signature verified successfully"
            );
        },
        (None, true) => {
            state
                .webhook_metrics
                .signature_failures_total
                .with_label_values(&["unsigned_insecure_mode"])
                .inc();
            warn!(
                event = "webhook_accepted_without_verification",
                "Accepting webhook without signature verification (insecure mode)."
            );
        },
        (None, false) => {
            warn!(
                event = "webhook_rejected_no_secret",
                "Refusing webhook: no secret configured and insecure mode disabled."
            );
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Webhook endpoint not configured",
            )
                .into_response();
        },
    }

    let (webhook_payload, repository_info, installation) =
        match parse_github_payload(&headers, &body) {
            Ok(parsed) => parsed,
            Err(rejection) => return rejection.into_response(),
        };

    crate::github::handle_webhook_payload(
        webhook_payload,
        repository_info,
        installation,
        state.git_sender,
        state.github_sender,
        state.octocrab,
        state.require_approval,
        state.merge_queue_require_approval,
        state.db_service,
        state.github_app_configs,
        state.channels,
        state.channel_sender,
        state.search_index_sender,
    )
    .await;

    StatusCode::NO_CONTENT.into_response()
}

type ParsedGithubPayload = (
    octocrab::models::webhook_events::WebhookEventPayload,
    Option<(String, String)>,
    Option<octocrab::models::webhook_events::EventInstallation>,
);

/// Parse a GitHub webhook body into a typed payload.
///
/// `WebhookEventPayload` is an externally-tagged enum, so the raw body
/// cannot be deserialized into it directly: the variant is selected by
/// the `X-GitHub-Event` header. The body is parsed into a `Value` once;
/// the top-level `repository` / `installation` fields are read from it
/// by reference and the `Value` is then consumed by the event-specific
/// payload parser.
fn parse_github_payload(
    headers: &axum::http::HeaderMap,
    body: &[u8],
) -> Result<ParsedGithubPayload, (StatusCode, &'static str)> {
    use octocrab::models::webhook_events::{EventInstallation, WebhookEventType};

    let Some(event_header) = headers.get("x-github-event").and_then(|h| h.to_str().ok()) else {
        warn!("Missing or invalid X-GitHub-Event header");
        return Err((StatusCode::BAD_REQUEST, "Missing X-GitHub-Event header"));
    };

    let event_type: WebhookEventType =
        match serde_json::from_value(Value::String(event_header.to_string())) {
            Ok(kind) => kind,
            Err(e) => {
                warn!(
                    "Unrecognised X-GitHub-Event header {:?}: {:?}",
                    event_header, e
                );
                return Err((StatusCode::BAD_REQUEST, "Invalid X-GitHub-Event header"));
            },
        };

    let webhook_json: Value = serde_json::from_slice(body).map_err(|e| {
        warn!("Failed to parse webhook JSON: {:?}", e);
        (StatusCode::BAD_REQUEST, "Invalid JSON payload")
    })?;

    let repository_info = webhook_json.get("repository").and_then(|repo| {
        let owner = repo.get("owner")?.get("login")?.as_str()?;
        let name = repo.get("name")?.as_str()?;
        Some((owner.to_string(), name.to_string()))
    });

    let installation: Option<EventInstallation> = webhook_json
        .get("installation")
        .and_then(|inst| serde_json::from_value(inst.clone()).ok());

    let payload = event_type
        .parse_specific_payload(webhook_json)
        .map_err(|e| {
            warn!(
                event = event_header,
                "Failed to deserialize webhook payload: {:?}", e
            );
            (StatusCode::BAD_REQUEST, "Invalid webhook event payload")
        })?;

    Ok((payload, repository_info, installation))
}

pub(super) async fn handle_gitlab_webhook(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    // GitLab uses X-Gitlab-Token header for webhook authentication
    let token_header = headers.get("x-gitlab-token");

    // Verify webhook secret if configured
    match (&state.webhook_secret, state.allow_insecure_webhooks) {
        (Some(secret), _) => {
            let token = match token_header.and_then(|t| t.to_str().ok()) {
                Some(t) => t,
                None => {
                    warn!("Missing or invalid X-Gitlab-Token header");
                    return (StatusCode::UNAUTHORIZED, "").into_response();
                },
            };

            if token != secret.expose() {
                warn!("GitLab webhook token verification failed");
                return (StatusCode::UNAUTHORIZED, "").into_response();
            }
        },
        (None, true) => {
            warn!("Accepting GitLab webhook without verification (insecure mode)");
        },
        (None, false) => {
            warn!("Refusing GitLab webhook: no secret configured");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Webhook endpoint not configured",
            )
                .into_response();
        },
    }

    // Get event type from X-Gitlab-Event header
    let event_type = match headers.get("x-gitlab-event").and_then(|h| h.to_str().ok()) {
        Some(et) => et,
        None => {
            warn!("Missing X-Gitlab-Event header");
            return (StatusCode::BAD_REQUEST, "Missing event type header").into_response();
        },
    };

    // Parse JSON payload
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(json) => json,
        Err(e) => {
            warn!("Failed to parse GitLab webhook JSON: {:?}", e);
            return (StatusCode::BAD_REQUEST, "Invalid JSON payload").into_response();
        },
    };

    // Forward to GitLab webhook handler
    if let Some(gitlab_sender) = &state.gitlab_sender {
        crate::gitlab::handle_webhook_payload(
            event_type,
            payload,
            state.git_sender,
            gitlab_sender.clone(),
            state.db_service,
            state.channels,
        )
        .await;
    } else {
        warn!("GitLab webhook received but GitLabService not configured");
    }

    StatusCode::NO_CONTENT.into_response()
}

pub(super) async fn handle_gitea_webhook(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    // Gitea uses X-Gitea-Signature for HMAC-SHA256 verification (similar to GitHub)
    // Signature verification is the primary security gate. Three modes:
    //   (a) secret configured            → require a valid signature
    //   (b) no secret, insecure allowed  → accept everything (dev only)
    //   (c) no secret, insecure forbidden → refuse with 503
    match (&state.webhook_secret, state.allow_insecure_webhooks) {
        (Some(secret), _) => {
            let signature = match headers.get("x-gitea-signature") {
                Some(sig) => match sig.to_str() {
                    Ok(s) => s,
                    Err(e) => {
                        state
                            .webhook_metrics
                            .signature_failures_total
                            .with_label_values(&["invalid_header"])
                            .inc();
                        warn!(
                            event = "gitea_webhook_signature_invalid_header",
                            error = %e,
                            "Failed to parse X-Gitea-Signature header"
                        );
                        return (StatusCode::UNAUTHORIZED, "").into_response();
                    },
                },
                None => {
                    state
                        .webhook_metrics
                        .signature_failures_total
                        .with_label_values(&["missing_header"])
                        .inc();
                    warn!(
                        event = "gitea_webhook_signature_missing",
                        "Missing X-Gitea-Signature header on webhook request"
                    );
                    return (StatusCode::UNAUTHORIZED, "").into_response();
                },
            };

            if let Err(e) = verify_webhook_signature(secret.expose(), signature, &body) {
                state
                    .webhook_metrics
                    .signature_failures_total
                    .with_label_values(&["bad_signature"])
                    .inc();
                warn!(
                    event = "gitea_webhook_signature_verification_failed",
                    error = %e,
                    "Gitea webhook signature verification failed"
                );
                return (StatusCode::UNAUTHORIZED, "").into_response();
            }

            state.webhook_metrics.signature_verified_total.inc();
            info!(
                event = "gitea_webhook_signature_verified",
                "Gitea webhook signature verified successfully"
            );
        },
        (None, true) => {
            state
                .webhook_metrics
                .signature_failures_total
                .with_label_values(&["unsigned_insecure_mode"])
                .inc();
            warn!(
                event = "gitea_webhook_accepted_without_verification",
                "Accepting Gitea webhook without signature verification (insecure mode)"
            );
        },
        (None, false) => {
            warn!(
                event = "gitea_webhook_rejected_no_secret",
                "Refusing Gitea webhook: no secret configured and insecure mode disabled"
            );
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Webhook endpoint not configured",
            )
                .into_response();
        },
    }

    // Get event type from X-Gitea-Event header
    let event_type = match headers.get("x-gitea-event").and_then(|h| h.to_str().ok()) {
        Some(et) => et,
        None => {
            warn!("Missing X-Gitea-Event header");
            return (StatusCode::BAD_REQUEST, "Missing event type header").into_response();
        },
    };

    // Parse JSON payload
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(json) => json,
        Err(e) => {
            warn!("Failed to parse Gitea webhook JSON: {:?}", e);
            return (StatusCode::BAD_REQUEST, "Invalid JSON payload").into_response();
        },
    };

    // Forward to Gitea webhook handler
    if let Some(gitea_sender) = &state.gitea_sender {
        crate::gitea::handle_webhook_payload(
            event_type,
            payload,
            state.git_sender,
            gitea_sender.clone(),
            state.db_service,
            state.channels,
        )
        .await;
    } else {
        warn!("Gitea webhook received but GiteaService not configured");
    }

    StatusCode::NO_CONTENT.into_response()
}
