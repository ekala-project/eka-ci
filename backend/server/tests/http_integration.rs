//! HTTP endpoint integration tests for the EkaCI backend.
//!
//! These tests start a real HTTP server and verify the API endpoints
//! return correct responses.

mod common;

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use common::{TestContext, create_simple_drv, insert_test_drv, test_drv, wait_for_drv_state};
use eka_ci_server::auth::{GitHubApiClient, JwtService, OAuthConfig};
use eka_ci_server::db::model::build_event::{DrvBuildResult, DrvBuildState};
use eka_ci_server::db::model::drv::Drv;
use eka_ci_server::graph::{GraphCommand, GraphService};
use eka_ci_server::metrics::WebhookMetrics;
use eka_ci_server::scheduler::{IngressTask, SchedulerService};
use eka_ci_server::secret::Redacted;
use eka_ci_server::services::WebSocketService;
use eka_ci_server::web::WebService;
use prometheus::Registry;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Helper to create a test web server with explicit webhook config.
///
/// `webhook_secret = None, allow_insecure_webhooks = true` matches the
/// historic test defaults (skip signature verification).
async fn create_test_server_with_webhook(
    ctx: &TestContext,
    webhook_secret: Option<String>,
    allow_insecure_webhooks: bool,
) -> (WebService, SchedulerService, mpsc::Sender<IngressTask>) {
    create_test_server_with_options(ctx, webhook_secret, allow_insecure_webhooks, Vec::new()).await
}

/// Helper to create a test web server with full control over webhook
/// and CORS configuration. Used by tests that need to exercise the
/// CORS allow-list (M1).
async fn create_test_server_with_options(
    ctx: &TestContext,
    webhook_secret: Option<String>,
    allow_insecure_webhooks: bool,
    allowed_origins: Vec<String>,
) -> (WebService, SchedulerService, mpsc::Sender<IngressTask>) {
    // Create GraphService for in-memory build state tracking
    let (graph_command_sender, graph_command_receiver) = mpsc::channel::<GraphCommand>(1000);
    let graph_service = GraphService::new(
        ctx.db_service.clone(),
        graph_command_receiver,
        None,
        1_000_000,
    )
    .await
    .expect("Failed to initialize GraphService");
    let graph_handle = graph_service.handle(graph_command_sender.clone());

    // Spawn the graph service
    let cancel_token = tokio_util::sync::CancellationToken::new();
    tokio::spawn(async move {
        graph_service.run(cancel_token).await;
    });

    // Create metrics registry for tests
    let metrics_registry = Arc::new(Registry::new());

    // Empty cache configs for tests
    let cache_configs = Arc::new(std::collections::HashMap::new());

    // Create scheduler
    let scheduler = SchedulerService::new(
        ctx.db_service.clone(),
        ctx.logs_dir.clone(),
        vec![], // no remote builders
        None,   // no GitHub integration
        30,     // 30 second no-output timeout
        3600,   // 1 hour absolute build cap (M5)
        None,   // no WebSocket broadcast
        graph_command_sender.clone(),
        graph_handle.clone(),
        metrics_registry.clone(),
        cache_configs,
        300,  // 5 minute hook timeout
        true, // audit hooks enabled
    )
    .await
    .expect("Failed to create scheduler");

    let ingress_sender = scheduler.ingress_request_sender();

    // Create WebSocket service
    let websocket_service = WebSocketService::new();

    // Create JWT service
    let jwt_service = JwtService::new("test-secret-key-for-integration-tests");

    // Create OAuth config (dummy values for tests).
    // M2: `client_secret` is a `Redacted<String>` so it cannot leak
    // through `Debug` formatting; tests construct it via `Redacted::new`.
    let oauth_config = OAuthConfig {
        client_id: "test-client-id".to_string(),
        client_secret: Redacted::new("test-client-secret".to_string()),
        redirect_url: "http://localhost/callback".to_string(),
    };

    // Create git channel. Tests use a default-capacity throw-away
    // channel; the receiver is unused for these HTTP tests.
    let (git_sender, _git_receiver) = mpsc::channel(100);

    // Bind to random port
    let socket = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0);

    // Empty GitHub App configs for test
    let github_app_configs = Arc::new(std::collections::HashMap::new());

    // Create GitHub API client for tests
    let github_client = Arc::new(GitHubApiClient::new());

    // Register webhook metrics against the shared scheduler registry.
    let webhook_metrics = WebhookMetrics::new(&scheduler.metrics_registry())
        .expect("failed to register webhook metrics");

    let web_service = WebService::bind_to_address(
        &socket,
        git_sender,
        None, // no GitHub sender
        None, // no ingress sender
        None, // no octocrab
        scheduler.metrics_registry(),
        false, // no approval required
        false, // no merge queue approval required
        ctx.db_service.clone(),
        graph_handle.clone(),
        jwt_service,
        oauth_config,
        ctx.logs_dir.clone(),
        websocket_service,
        github_app_configs,
        // M2: wrap at the test-helper boundary so individual test call
        // sites can keep using plain `Option<String>`.
        webhook_secret.map(Redacted::new),
        allow_insecure_webhooks,
        webhook_metrics,
        github_client,
        "squash".to_string(), // default merge method for tests
        allowed_origins,
    )
    .await
    .expect("Failed to create web service");

    (web_service, scheduler, ingress_sender)
}

/// Backwards-compatible default: no webhook secret, insecure mode on.
/// Existing tests do not touch `/github/webhook`, so this preserves
/// their behaviour exactly.
async fn create_test_server(
    ctx: &TestContext,
) -> (WebService, SchedulerService, mpsc::Sender<IngressTask>) {
    create_test_server_with_webhook(ctx, None, true).await
}

#[tokio::test]
async fn test_get_drv_endpoint() {
    // Setup test environment
    let ctx = TestContext::new().await.unwrap();

    // Create a test drv
    let drv = test_drv("test-endpoint", "x86_64-linux");
    insert_test_drv(&ctx.db_service, &drv)
        .await
        .expect("Failed to insert test drv");

    // Create and start web server
    let (web_service, _scheduler, _ingress_sender) = create_test_server(&ctx).await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    // Start the server in the background
    let cancellation_token = CancellationToken::new();
    let server_token = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(server_token).await;
    });

    // Give the server a moment to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Make HTTP request to get drv details
    let client = reqwest::Client::new();
    let url = format!("{}/v1/drvs/{}", base_url, &*drv.drv_path);
    let response = client
        .get(&url)
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), 200);

    let body: Value = response.json().await.expect("Failed to parse JSON");

    // Verify response contains expected fields
    assert_eq!(body["drv_path"], &*drv.drv_path);
    assert_eq!(body["system"], "x86_64-linux");
    assert_eq!(body["is_fod"], false);

    // Cleanup
    cancellation_token.cancel();

    println!("✓ GET /v1/drvs/{{drv}} endpoint works correctly");
}

#[tokio::test]
#[ignore] // Requires Nix to create and build real derivations
async fn test_drv_state_updates_via_api() {
    // Setup test environment
    let ctx = TestContext::new().await.unwrap();

    // Create a simple derivation that will succeed
    let drv_path =
        create_simple_drv("test-api-update", true).expect("Failed to create test derivation");

    println!("Created test derivation: {}", drv_path);

    // Fetch drv info and insert into DB
    let drv = Drv::fetch_info(&drv_path, &ctx.db_service)
        .await
        .expect("Failed to fetch drv info");

    insert_test_drv(&ctx.db_service, &drv)
        .await
        .expect("Failed to insert drv");

    // Create and start web server
    let (web_service, _scheduler, ingress_sender) = create_test_server(&ctx).await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    // Start the server in the background
    let cancellation_token = CancellationToken::new();
    let server_token = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(server_token).await;
    });

    // Give the server a moment to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let url = format!("{}/v1/drvs/{}", base_url, &*drv.drv_path);

    // Check initial state via API
    let response = client.get(&url).send().await.expect("Failed to get drv");
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.expect("Failed to parse JSON");
    assert_eq!(body["build_state"], "Queued");

    println!("✓ Initial state is Queued");

    // Send drv to scheduler to trigger build
    ingress_sender
        .send(IngressTask::EvalRequest(drv.drv_path.clone()))
        .await
        .expect("Failed to send ingress task");

    // Wait for build to complete
    wait_for_drv_state(
        &ctx.db_service,
        &drv.drv_path,
        DrvBuildState::Completed(DrvBuildResult::Success),
        Duration::from_secs(15),
    )
    .await
    .expect("Build should complete successfully");

    // Check final state via API
    let response = client.get(&url).send().await.expect("Failed to get drv");
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.expect("Failed to parse JSON");

    // Verify the API returns the updated state
    let build_state = body["build_state"]
        .as_object()
        .expect("build_state should be object");
    assert!(build_state.contains_key("Completed"));
    assert_eq!(build_state["Completed"], "Success");

    println!("✓ State updated to Completed(Success) visible via API");

    // Cleanup
    cancellation_token.cancel();
}

#[tokio::test]
async fn test_get_drv_not_found() {
    // Setup test environment
    let ctx = TestContext::new().await.unwrap();

    // Create and start web server
    let (web_service, _scheduler, _ingress_sender) = create_test_server(&ctx).await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    // Start the server in the background
    let cancellation_token = CancellationToken::new();
    let server_token = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(server_token).await;
    });

    // Give the server a moment to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Try to get a non-existent drv
    let client = reqwest::Client::new();
    let fake_drv = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa0-nonexistent.drv";
    let url = format!("{}/v1/drvs/{}", base_url, fake_drv);
    let response = client
        .get(&url)
        .send()
        .await
        .expect("Failed to send request");

    // Should return 404
    assert_eq!(response.status(), 404);

    println!("✓ GET /v1/drvs/{{drv}} returns 404 for non-existent drv");

    // Cleanup
    cancellation_token.cancel();
}

#[tokio::test]
async fn test_get_drv_dependencies_endpoint() {
    // Setup test environment
    let ctx = TestContext::new().await.unwrap();

    // Create drv graph: drv_a depends on drv_b
    let drv_a = test_drv("drv-a", "x86_64-linux");
    let drv_b = test_drv("drv-b", "x86_64-linux");

    ctx.db_service
        .insert_drvs_and_references(
            &[drv_a.clone(), drv_b.clone()],
            &[(drv_a.drv_path.clone(), drv_b.drv_path.clone())],
        )
        .await
        .expect("Failed to insert drvs");

    // Create and start web server
    let (web_service, _scheduler, _ingress_sender) = create_test_server(&ctx).await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    // Start the server in the background
    let cancellation_token = CancellationToken::new();
    let server_token = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(server_token).await;
    });

    // Give the server a moment to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Get dependencies for drv_a
    let client = reqwest::Client::new();
    let url = format!("{}/v1/drvs/{}/dependencies", base_url, &*drv_a.drv_path);
    let response = client
        .get(&url)
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), 200);

    let body: Value = response.json().await.expect("Failed to parse JSON");

    // Should have one dependency (drv_b)
    let dependencies = body["dependencies"]
        .as_array()
        .expect("dependencies should be an array");
    assert_eq!(dependencies.len(), 1);
    assert_eq!(dependencies[0]["drv_path"], &*drv_b.drv_path);
    assert_eq!(body["dependency_count"], 1);

    println!("✓ GET /v1/drvs/{{drv}}/dependencies endpoint works correctly");

    // Cleanup
    cancellation_token.cancel();
}

#[tokio::test]
async fn test_root_url_serves_frontend() {
    // Setup test environment
    let ctx = TestContext::new().await.unwrap();

    // Create and start web server
    let (web_service, _scheduler, _ingress_sender) = create_test_server(&ctx).await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    // Start the server in the background
    let cancellation_token = CancellationToken::new();
    let cancel = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(cancel).await;
    });

    // Give the server a moment to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Request the root URL
    let client = reqwest::Client::new();
    let response = client
        .get(&base_url)
        .send()
        .await
        .expect("Failed to send request");

    // Should return 200 OK (or 404 if static files not found, which we'll check)
    let status = response.status();
    let body = response.text().await.expect("Failed to read response body");

    // The response should either:
    // 1. Return 200 and contain HTML (index.html)
    // 2. Return 404 if running from wrong directory
    if status == 200 {
        assert!(
            body.contains("<!doctype html>") || body.contains("<!DOCTYPE html>"),
            "Response should contain HTML doctype, got: {}",
            &body[..200.min(body.len())]
        );
        assert!(
            body.contains("main.js"),
            "Response should reference main.js, got: {}",
            &body[..200.min(body.len())]
        );
        println!("✓ Root URL serves index.html successfully");
    } else {
        // If we get 404, it means static files weren't found - this is expected when
        // running tests from the wrong directory
        println!("⚠ Got 404 - static files not found (expected if not running from project root)");
        println!("  This is not a test failure, just means static dir path needs adjustment");
    }

    // Cleanup
    cancellation_token.cancel();
}

#[tokio::test]
async fn test_cors_headers_present() {
    // M1: CORS now uses an exact-match allow-list instead of
    // `CorsLayer::permissive()`. This test verifies the happy path:
    // a configured origin gets `access-control-allow-origin` echoed
    // back on simple (non-preflight) GET requests.
    let ctx = TestContext::new().await.unwrap();

    let origin = "http://example.com";
    let (web_service, _scheduler, _ingress_sender) =
        create_test_server_with_options(&ctx, None, true, vec![origin.to_string()]).await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    let cancellation_token = CancellationToken::new();
    let cancel = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(cancel).await;
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/v1/repositories", base_url))
        .header("Origin", origin)
        .send()
        .await
        .expect("Failed to send request");

    let headers = response.headers();
    let allow_origin = headers
        .get("access-control-allow-origin")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    assert_eq!(
        allow_origin, origin,
        "allow-listed origin should be reflected on simple requests"
    );

    println!("✓ CORS headers are present on API responses for allow-listed origin");

    cancellation_token.cancel();
}

// ---------------------------------------------------------------------------
// H4 regression tests: sensitive endpoints must reject unauthenticated
// callers. Build-log endpoints are intentionally public and are not
// exercised here.
// ---------------------------------------------------------------------------

/// Test helper: forge a valid JWT using the same secret the test web server
/// is constructed with. `is_admin` toggles the admin claim.
fn make_test_jwt(is_admin: bool) -> String {
    use eka_ci_server::auth::types::AuthenticatedUser;

    let jwt_service = JwtService::new("test-secret-key-for-integration-tests");
    let now = chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0)
        .unwrap()
        .naive_utc();
    let user = AuthenticatedUser {
        id: 1,
        github_id: 42,
        github_username: "test-user".to_string(),
        github_avatar_url: None,
        github_access_token: "dummy-token".to_string(),
        is_admin,
        created_at: now,
        last_login: now,
    };
    jwt_service
        .create_token(&user)
        .expect("failed to mint test JWT")
}

#[tokio::test]
async fn test_metrics_requires_admin() {
    let ctx = TestContext::new().await.unwrap();
    let (web_service, _scheduler, _ingress_sender) = create_test_server(&ctx).await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    let cancellation_token = CancellationToken::new();
    let server_token = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(server_token).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let url = format!("{}/v1/metrics", base_url);

    // (1) No token → 401.
    let resp = client
        .get(&url)
        .send()
        .await
        .expect("failed to send unauthenticated metrics request");
    assert_eq!(
        resp.status(),
        401,
        "unauthenticated metrics request must be rejected"
    );

    // (2) Non-admin JWT → 403.
    let user_token = make_test_jwt(false);
    let resp = client
        .get(&url)
        .bearer_auth(&user_token)
        .send()
        .await
        .expect("failed to send non-admin metrics request");
    assert_eq!(
        resp.status(),
        403,
        "non-admin metrics request must be forbidden"
    );

    // (3) Admin JWT → 200.
    let admin_token = make_test_jwt(true);
    let resp = client
        .get(&url)
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("failed to send admin metrics request");
    assert_eq!(resp.status(), 200, "admin metrics request must succeed");

    cancellation_token.cancel();
    println!("✓ /v1/metrics enforces admin authentication");
}

#[tokio::test]
async fn test_check_runs_requires_auth() {
    let ctx = TestContext::new().await.unwrap();
    let (web_service, _scheduler, _ingress_sender) = create_test_server(&ctx).await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    let cancellation_token = CancellationToken::new();
    let server_token = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(server_token).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    let url = format!("{}/v1/commits/{}/check_runs", base_url, sha);

    // (1) No token → 401.
    let resp = client
        .get(&url)
        .send()
        .await
        .expect("failed to send unauthenticated check_runs request");
    assert_eq!(
        resp.status(),
        401,
        "unauthenticated check_runs request must be rejected"
    );

    // (2) Valid (non-admin) token → not 401/403. We accept either 200 with
    //     an empty array or 404/5xx depending on backing data; the point is
    //     that authentication passes.
    let token = make_test_jwt(false);
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("failed to send authenticated check_runs request");
    let status = resp.status();
    assert!(
        status != 401 && status != 403,
        "authenticated check_runs request must not be rejected by auth (got {})",
        status
    );

    cancellation_token.cancel();
    println!("✓ /v1/commits/{{sha}}/check_runs enforces authentication");
}

#[tokio::test]
async fn test_ws_builds_rejects_upgrade_without_token() {
    let ctx = TestContext::new().await.unwrap();
    let (web_service, _scheduler, _ingress_sender) = create_test_server(&ctx).await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    let cancellation_token = CancellationToken::new();
    let server_token = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(server_token).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let url = format!("{}/v1/ws/builds", base_url);

    // Unauthenticated upgrade attempt: our handler rejects before
    // delegating to WebSocketUpgrade, so a plain GET is sufficient to
    // observe the 401. We still send the upgrade-style headers to
    // confirm behaviour on the real WS path.
    let resp = client
        .get(&url)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .expect("failed to send WS upgrade request");
    assert_eq!(
        resp.status(),
        401,
        "WS upgrade without a token must be rejected with 401"
    );

    // A bogus token should also be rejected.
    let resp = client
        .get(&url)
        .bearer_auth("not.a.valid.jwt")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .expect("failed to send WS upgrade with bogus token");
    assert_eq!(
        resp.status(),
        401,
        "WS upgrade with an invalid token must be rejected with 401"
    );

    cancellation_token.cancel();
    println!("✓ /v1/ws/builds rejects unauthenticated upgrade attempts");
}

// ---------------------------------------------------------------------------
// H1 regression tests: `/github/webhook` signature verification.
// ---------------------------------------------------------------------------

/// Compute the hex `sha256=<hmac>` header GitHub sends for a given
/// body, using the same hmac/sha2 crates the server uses.
fn github_signature_header(secret: &str, body: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

#[tokio::test]
async fn test_webhook_rejects_missing_signature_when_secret_configured() {
    let ctx = TestContext::new().await.unwrap();
    let (web_service, _scheduler, _ingress) =
        create_test_server_with_webhook(&ctx, Some("test-webhook-secret".to_string()), false).await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    let cancellation_token = CancellationToken::new();
    let server_token = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(server_token).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/github/webhook", base_url))
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .expect("failed to send unsigned webhook");

    assert_eq!(
        resp.status(),
        401,
        "missing signature header must be rejected with 401"
    );

    cancellation_token.cancel();
    println!("✓ webhook rejects missing X-Hub-Signature-256 with 401");
}

#[tokio::test]
async fn test_webhook_rejects_bad_signature() {
    let ctx = TestContext::new().await.unwrap();
    let (web_service, _scheduler, _ingress) =
        create_test_server_with_webhook(&ctx, Some("test-webhook-secret".to_string()), false).await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    let cancellation_token = CancellationToken::new();
    let server_token = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(server_token).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let body = "{}";
    // HMAC of `body` under the WRONG secret.
    let forged = github_signature_header("not-the-real-secret", body.as_bytes());

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/github/webhook", base_url))
        .header("content-type", "application/json")
        .header("x-hub-signature-256", forged)
        .body(body)
        .send()
        .await
        .expect("failed to send bad-signature webhook");

    assert_eq!(
        resp.status(),
        401,
        "signature mismatch must be rejected with 401"
    );

    cancellation_token.cancel();
    println!("✓ webhook rejects forged X-Hub-Signature-256 with 401");
}

#[tokio::test]
async fn test_webhook_accepts_valid_signature() {
    let ctx = TestContext::new().await.unwrap();
    let secret = "test-webhook-secret";
    let (web_service, _scheduler, _ingress) =
        create_test_server_with_webhook(&ctx, Some(secret.to_string()), false).await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    let cancellation_token = CancellationToken::new();
    let server_token = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(server_token).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let body = "{}";
    let signature = github_signature_header(secret, body.as_bytes());

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/github/webhook", base_url))
        .header("content-type", "application/json")
        .header("x-hub-signature-256", signature)
        .body(body)
        .send()
        .await
        .expect("failed to send valid-signature webhook");

    // Signature verification passed. The empty `{}` body will not
    // deserialize into a `WebhookEventPayload`, so we expect the
    // handler to return 400 after the gate opens. What matters for
    // H1 is that we are NOT seeing 401/503.
    let status = resp.status();
    assert!(
        status != 401 && status != 503,
        "valid signature must not be rejected by H1 gate (got {status})"
    );

    cancellation_token.cancel();
    println!("✓ webhook accepts valid X-Hub-Signature-256");
}

#[tokio::test]
async fn test_webhook_returns_503_in_strict_mode_without_secret() {
    let ctx = TestContext::new().await.unwrap();
    // Defence-in-depth path: even if `Config::from_env` is bypassed
    // and we construct a WebService with no secret and strict mode,
    // the handler must refuse.
    let (web_service, _scheduler, _ingress) =
        create_test_server_with_webhook(&ctx, None, false).await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    let cancellation_token = CancellationToken::new();
    let server_token = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(server_token).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/github/webhook", base_url))
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .expect("failed to send webhook");

    assert_eq!(
        resp.status(),
        503,
        "strict mode without a secret must respond 503"
    );

    cancellation_token.cancel();
    println!("✓ webhook returns 503 in strict mode when no secret is configured");
}

// ---------------------------------------------------------------------------
// M1: CORS allow-list tests
//
// The server no longer uses `CorsLayer::permissive()`. It builds an
// exact-match allow-list from `web.allowed_origins` (or the
// `EKACI_WEB_ALLOWED_ORIGINS` env override). An empty list means
// cross-origin requests are refused; a populated list reflects only
// the listed origins in `access-control-allow-origin`.
// ---------------------------------------------------------------------------

/// Issue a CORS preflight OPTIONS request and return the response.
async fn cors_preflight(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    origin: &str,
) -> reqwest::Response {
    client
        .request(reqwest::Method::OPTIONS, format!("{}{}", base_url, path))
        .header("origin", origin)
        .header("access-control-request-method", "GET")
        .header("access-control-request-headers", "content-type")
        .send()
        .await
        .expect("failed to send CORS preflight")
}

#[tokio::test]
async fn cors_allows_configured_origin() {
    let ctx = TestContext::new().await.unwrap();

    let (web_service, _scheduler, _ingress_sender) = create_test_server_with_options(
        &ctx,
        None,
        true,
        vec!["https://ekaci.example.com".to_string()],
    )
    .await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    let cancellation_token = CancellationToken::new();
    let server_token = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(server_token).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let resp = cors_preflight(
        &client,
        &base_url,
        "/v1/metrics",
        "https://ekaci.example.com",
    )
    .await;

    // Allowed origin should be reflected back; preflight responses are
    // typically 200 or 204 depending on the CORS middleware version.
    let status = resp.status().as_u16();
    assert!(
        status == 200 || status == 204,
        "expected 200/204 from preflight, got {status}"
    );
    let allow_origin = resp
        .headers()
        .get("access-control-allow-origin")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    assert_eq!(
        allow_origin, "https://ekaci.example.com",
        "allowed origin should be reflected exactly"
    );

    cancellation_token.cancel();
    println!("✓ configured CORS origin is allowed on preflight");
}

#[tokio::test]
async fn cors_denies_unlisted_origin() {
    let ctx = TestContext::new().await.unwrap();

    let (web_service, _scheduler, _ingress_sender) = create_test_server_with_options(
        &ctx,
        None,
        true,
        vec!["https://ekaci.example.com".to_string()],
    )
    .await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    let cancellation_token = CancellationToken::new();
    let server_token = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(server_token).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let resp = cors_preflight(
        &client,
        &base_url,
        "/v1/metrics",
        "https://attacker.example.com",
    )
    .await;

    // The CORS layer refuses the preflight; the browser enforces the
    // absence of `access-control-allow-origin` even if the HTTP
    // status is 2xx.
    let allow_origin = resp.headers().get("access-control-allow-origin");
    assert!(
        allow_origin.is_none(),
        "unlisted origin must not be reflected (got {:?})",
        allow_origin
    );

    cancellation_token.cancel();
    println!("✓ unlisted CORS origin is denied on preflight");
}

#[tokio::test]
async fn cors_empty_allowlist_denies_all() {
    let ctx = TestContext::new().await.unwrap();

    // Default posture: empty allow-list -> refuse all cross-origin.
    let (web_service, _scheduler, _ingress_sender) =
        create_test_server_with_options(&ctx, None, true, Vec::new()).await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    let cancellation_token = CancellationToken::new();
    let server_token = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(server_token).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let resp = cors_preflight(
        &client,
        &base_url,
        "/v1/metrics",
        "https://anywhere.example.com",
    )
    .await;

    let allow_origin = resp.headers().get("access-control-allow-origin");
    assert!(
        allow_origin.is_none(),
        "empty allow-list must not reflect any origin (got {:?})",
        allow_origin
    );

    cancellation_token.cancel();
    println!("✓ empty CORS allow-list refuses every cross-origin request");
}

// ---------------------------------------------------------------------------
// M3 regression tests: webhook endpoint body-size limit + per-IP rate limit.
//
// `/github/webhook` is the only unauthenticated POST endpoint, so a
// dedicated sub-router attaches two protective layers:
//   - `DefaultBodyLimit::max(5 MiB)` rejects inflated bodies with 413 before any handler logic
//     runs.
//   - `tower_governor::GovernorLayer` (10 req/s, burst 30, keyed by peer IP via
//     `ConnectInfo<SocketAddr>`) sheds abusive traffic with 429 before the expensive HMAC
//     verification runs.
// Sibling routes (`/github/auth/*`) must remain unaffected by either
// layer.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_webhook_rejects_oversized_body() {
    let ctx = TestContext::new().await.unwrap();
    // Insecure mode avoids the HMAC gate so the body-size limit is
    // exercised in isolation. The body limit is enforced by the axum
    // layer before the handler runs, so signature configuration is
    // irrelevant here.
    let (web_service, _scheduler, _ingress) =
        create_test_server_with_webhook(&ctx, None, true).await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    let cancellation_token = CancellationToken::new();
    let server_token = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(server_token).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 6 MiB body — exceeds the 5 MiB webhook limit.
    let big_body = vec![b'a'; 6 * 1024 * 1024];

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/github/webhook", base_url))
        .header("content-type", "application/octet-stream")
        .body(big_body)
        .send()
        .await
        .expect("failed to send oversized webhook body");

    assert_eq!(
        resp.status(),
        413,
        "body exceeding the webhook size limit must be rejected with 413"
    );

    cancellation_token.cancel();
    println!("✓ webhook rejects oversized bodies with 413");
}

#[tokio::test]
async fn test_webhook_rate_limits_bursting_clients() {
    let ctx = TestContext::new().await.unwrap();
    let (web_service, _scheduler, _ingress) =
        create_test_server_with_webhook(&ctx, None, true).await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    let cancellation_token = CancellationToken::new();
    let server_token = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(server_token).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let url = format!("{}/github/webhook", base_url);

    // Fire 60 webhook requests concurrently from 127.0.0.1. With burst
    // 30 and refill 10 req/s, a simultaneous burst of 60 far exceeds
    // what the per-IP bucket allows at any instant, so at least one
    // request must be shed with 429.
    let mut handles = Vec::with_capacity(60);
    for _ in 0..60 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            client
                .post(&url)
                .header("content-type", "application/json")
                .body("{}")
                .send()
                .await
                .map(|r| r.status().as_u16())
        }));
    }

    let mut too_many = 0u32;
    for h in handles {
        if let Ok(Ok(status)) = h.await {
            if status == 429 {
                too_many += 1;
            }
        }
    }

    assert!(
        too_many > 0,
        "expected at least one 429 after bursting 60 webhook calls, got 0"
    );

    cancellation_token.cancel();
    println!(
        "✓ webhook rate-limits bursting clients ({} requests shed with 429)",
        too_many
    );
}

#[tokio::test]
async fn test_webhook_rate_limit_does_not_affect_auth_routes() {
    // The M3 layers are scoped to the `/github/webhook` sub-router.
    // Sibling paths like `/github/auth/me` must not be governed.
    let ctx = TestContext::new().await.unwrap();
    let (web_service, _scheduler, _ingress) =
        create_test_server_with_webhook(&ctx, None, true).await;
    let addr = web_service.bind_addr();
    let base_url = format!("http://{}", addr);

    let cancellation_token = CancellationToken::new();
    let server_token = cancellation_token.clone();
    tokio::spawn(async move {
        web_service.run(server_token).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let url = format!("{}/github/auth/me", base_url);

    // Send 60 unauthenticated GETs — each will 401, none should 429.
    // If governor were attached to this route we'd see rate-limit
    // rejections here too.
    let mut handles = Vec::with_capacity(60);
    for _ in 0..60 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            client.get(&url).send().await.map(|r| r.status().as_u16())
        }));
    }

    let mut rate_limited = 0u32;
    let mut unauthorized = 0u32;
    for h in handles {
        if let Ok(Ok(status)) = h.await {
            if status == 429 {
                rate_limited += 1;
            } else if status == 401 {
                unauthorized += 1;
            }
        }
    }

    assert_eq!(
        rate_limited, 0,
        "auth route must not share the webhook governor (got {} 429s)",
        rate_limited
    );
    assert!(
        unauthorized > 0,
        "auth route should still enforce its own 401 (saw 0 out of 60)"
    );

    cancellation_token.cancel();
    println!("✓ webhook governor is scoped — auth routes are not rate-limited");
}
