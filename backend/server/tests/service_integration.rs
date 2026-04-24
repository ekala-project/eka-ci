//! Service-level integration tests for the EkaCI backend.
//!
//! These tests verify the build queue, scheduler, and state transitions
//! work correctly together.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{TestContext, create_simple_drv, insert_test_drv, test_drv, wait_for_drv_state};
use eka_ci_server::db::model::build_event::{DrvBuildResult, DrvBuildState};
use eka_ci_server::db::model::drv::Drv;
use eka_ci_server::graph::{GraphCommand, GraphService};
use eka_ci_server::scheduler::{IngressTask, SchedulerService};
use prometheus::Registry;
use tokio::sync::mpsc::channel;

#[tokio::test]
#[ignore] // Requires Nix to create and build real derivations
async fn test_build_simple_drv_success() {
    // Setup test environment
    let ctx = TestContext::new().await.unwrap();

    // Create a simple derivation that should succeed
    let drv_path =
        create_simple_drv("test-success", true).expect("Failed to create test derivation");

    println!("Created test derivation: {}", drv_path);

    // Fetch derivation info and insert into DB
    let drv = Drv::fetch_info(&drv_path, &ctx.db_service)
        .await
        .expect("Failed to fetch drv info");

    assert_eq!(drv.build_state, DrvBuildState::Queued);

    insert_test_drv(&ctx.db_service, &drv)
        .await
        .expect("Failed to insert drv");

    // Create GraphService for in-memory build state tracking
    let (graph_command_sender, graph_command_receiver) = channel::<GraphCommand>(1000);
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

    // Start the scheduler service
    let scheduler = SchedulerService::new(
        ctx.db_service.clone(),
        ctx.logs_dir.clone(),
        vec![], // no remote builders
        None,   // no GitHub integration
        30,     // 30 second no-output timeout
        3600,   // 1 hour absolute build cap (M5)
        None,   // no WebSocket
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

    // Send the drv to the scheduler
    ingress_sender
        .send(IngressTask::EvalRequest(std::sync::Arc::new(
            drv.drv_path.clone(),
        )))
        .await
        .expect("Failed to send ingress task");

    // Wait for the build to complete successfully
    // Note: We skip checking for Buildable and Building states because
    // simple derivations build so fast these states are transient
    wait_for_drv_state(
        &ctx.db_service,
        &drv.drv_path,
        DrvBuildState::Completed(DrvBuildResult::Success),
        Duration::from_secs(10),
    )
    .await
    .expect("Build should complete successfully");

    // Verify final state in database
    let final_drv = ctx
        .db_service
        .get_drv(&drv.drv_path)
        .await
        .expect("Failed to get drv")
        .expect("Drv should exist");

    assert_eq!(
        final_drv.build_state,
        DrvBuildState::Completed(DrvBuildResult::Success)
    );

    println!("✓ Build completed successfully");
}

#[tokio::test]
#[ignore] // Requires Nix to create and build real derivations
async fn test_build_failure_retry_logic() {
    // Setup test environment
    let ctx = TestContext::new().await.unwrap();

    // Create a simple derivation that will fail
    let drv_path =
        create_simple_drv("test-failure", false).expect("Failed to create test derivation");

    println!("Created test derivation that will fail: {}", drv_path);

    // Fetch derivation info and insert into DB
    let drv = Drv::fetch_info(&drv_path, &ctx.db_service)
        .await
        .expect("Failed to fetch drv info");

    insert_test_drv(&ctx.db_service, &drv)
        .await
        .expect("Failed to insert drv");

    // Create GraphService for in-memory build state tracking
    let (graph_command_sender, graph_command_receiver) = channel::<GraphCommand>(1000);
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

    // Start the scheduler service
    let scheduler = SchedulerService::new(
        ctx.db_service.clone(),
        ctx.logs_dir.clone(),
        vec![], // no remote builders
        None,   // no GitHub integration
        30,     // 30 second no-output timeout
        3600,   // 1 hour absolute build cap (M5)
        None,   // no WebSocket
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

    // Send the drv to the scheduler
    ingress_sender
        .send(IngressTask::EvalRequest(std::sync::Arc::new(
            drv.drv_path.clone(),
        )))
        .await
        .expect("Failed to send ingress task");

    // Wait for first failure (should transition to FailedRetry)
    // Note: We skip checking for Buildable and Building states because
    // builds happen so fast these states are transient
    // Increased timeout to account for build time
    wait_for_drv_state(
        &ctx.db_service,
        &drv.drv_path,
        DrvBuildState::FailedRetry,
        Duration::from_secs(15),
    )
    .await
    .expect("First failure should result in FailedRetry");

    println!("✓ First build failed, drv in FailedRetry state");

    // Wait for second failure (should transition to permanent Completed(Failure))
    // Note: The retry happens automatically and the Building state is too transient to observe
    wait_for_drv_state(
        &ctx.db_service,
        &drv.drv_path,
        DrvBuildState::Completed(DrvBuildResult::Failure),
        Duration::from_secs(15),
    )
    .await
    .expect("Second failure should result in permanent failure");

    // Verify final state in database
    let final_drv = ctx
        .db_service
        .get_drv(&drv.drv_path)
        .await
        .expect("Failed to get drv")
        .expect("Drv should exist");

    assert_eq!(
        final_drv.build_state,
        DrvBuildState::Completed(DrvBuildResult::Failure)
    );

    println!("✓ Build failed permanently after retry");
}

#[tokio::test]
async fn test_drv_state_persistence() {
    // Setup test environment
    let ctx = TestContext::new().await.unwrap();

    // Create a test drv directly (without nix)
    let drv = test_drv("test-persistence", "x86_64-linux");
    let drv_id = drv.drv_path.clone();

    // Insert it into the database
    insert_test_drv(&ctx.db_service, &drv)
        .await
        .expect("Failed to insert test drv");

    // Verify it was inserted with Queued state
    let retrieved_drv = ctx
        .db_service
        .get_drv(&drv_id)
        .await
        .expect("Failed to get drv")
        .expect("Drv should exist");

    assert_eq!(retrieved_drv.build_state, DrvBuildState::Queued);
    assert_eq!(retrieved_drv.system, "x86_64-linux");
    assert!(!retrieved_drv.is_fod);

    println!("✓ Drv state persists correctly in database");
}

#[tokio::test]
async fn test_drv_dependencies() {
    // Setup test environment
    let ctx = TestContext::new().await.unwrap();

    // Create two drvs: drv_a depends on drv_b
    let drv_a = test_drv("drv-a", "x86_64-linux");
    let drv_b = test_drv("drv-b", "x86_64-linux");

    // Insert both drvs with dependency relationship
    ctx.db_service
        .insert_drvs_and_references(
            &[drv_a.clone(), drv_b.clone()],
            &[(drv_a.drv_path.clone(), drv_b.drv_path.clone())],
        )
        .await
        .expect("Failed to insert drvs and refs");

    // Create GraphService for in-memory dependency tracking
    let (graph_command_sender, graph_command_receiver) = channel::<GraphCommand>(1000);
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

    // Give the graph service time to load from database
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify drv_a has drv_b as a dependency using the graph service
    let dependents = graph_handle
        .get_dependents(&drv_b.drv_path)
        .await
        .expect("Failed to get dependents");

    assert_eq!(dependents.len(), 1);
    assert_eq!(dependents[0], drv_a.drv_path);

    println!("✓ Drv dependency graph stored correctly");
}
