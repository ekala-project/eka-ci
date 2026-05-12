//! Integration tests for the ChannelService release promotion flow.
//!
//! These tests verify that the end-to-end promotion pipeline works
//! correctly: push webhook → jobset evaluation → required jobs succeed
//! → fast-forward push (dry-run).

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::{TestContext, insert_test_drv, test_drv};
use eka_ci_server::channels::ChannelService;
use eka_ci_server::channels::types::PromotionStatus;
use eka_ci_server::config::{ChannelConfig, ChannelForge};
use eka_ci_server::db::model::build_event::{DrvBuildResult, DrvBuildState};
use eka_ci_server::services::AsyncService;
use tokio_util::sync::CancellationToken;

/// Helper to create a test channel configuration.
fn test_channel_config(name: &str, required: &[&str], packages: &[&str]) -> ChannelConfig {
    ChannelConfig {
        forge: ChannelForge::GitHub,
        owner: "testorg".to_string(),
        repo: "testrepo".to_string(),
        name: name.to_string(),
        tracking_branch: "main".to_string(),
        target_branch: format!("{}-unstable", name),
        required: required.iter().map(|s| s.to_string()).collect(),
        packages: packages.iter().map(|s| s.to_string()).collect(),
        dry_run: true, // Always use dry-run in tests
    }
}

#[tokio::test]
async fn test_channel_promotion_on_successful_required_jobs() {
    // This test verifies the full promotion pipeline:
    // 1. A channel is configured to watch a repo with required jobs
    // 2. A push webhook arrives (simulated by EvaluatePush task)
    // 3. Jobs are evaluated and succeed
    // 4. ChannelService promotes the SHA to the target branch

    let ctx = TestContext::new().await.unwrap();

    // Create a channel that requires "coreutils" and "bash" to succeed
    let channel = test_channel_config("stable", &["coreutils", "bash"], &[]);

    let mut channels = HashMap::new();
    channels.insert(channel.channel_id(), channel.clone());

    let channel_service = ChannelService::new(
        ctx.db_service.clone(),
        Arc::new(channels),
        None, // No octocrab in tests
        None, // No GitHub sender in tests
    );

    let channel_sender = channel_service.get_sender();

    // Spawn the channel service in the background
    let cancel_token = CancellationToken::new();
    tokio::spawn(async move {
        let _ = channel_service.run(cancel_token).await;
    });

    let sha = "abc123def456";

    // Step 1: Simulate a push webhook by creating a GitHubJobSet
    sqlx::query("INSERT INTO GitHubJobSets (owner, repo_name, sha, job) VALUES (?, ?, ?, ?)")
        .bind(&channel.owner)
        .bind(&channel.repo)
        .bind(sha)
        .bind("test-jobset")
        .execute(&ctx.db_service.pool)
        .await
        .expect("Failed to insert jobset");

    let jobset_id: i64 = sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ?")
        .bind(sha)
        .fetch_one(&ctx.db_service.pool)
        .await
        .expect("Failed to get jobset ROWID");

    // Step 2: Create successful derivations for the required jobs
    for job_name in &["coreutils", "bash"] {
        let mut drv = test_drv(job_name, "x86_64-linux");
        drv.build_state = DrvBuildState::Completed(DrvBuildResult::Success);

        insert_test_drv(&ctx.db_service, &drv)
            .await
            .expect("Failed to insert test drv");

        let drv_rowid: i64 = sqlx::query_scalar("SELECT ROWID FROM Drv WHERE drv_path = ?")
            .bind(&*drv.drv_path)
            .fetch_one(&ctx.db_service.pool)
            .await
            .expect("Failed to get drv ROWID");

        sqlx::query("INSERT INTO Job (jobset, name, drv_id) VALUES (?, ?, ?)")
            .bind(jobset_id)
            .bind(job_name)
            .bind(drv_rowid)
            .execute(&ctx.db_service.pool)
            .await
            .expect("Failed to insert job");
    }

    // Step 3: Trigger channel evaluation via EvaluatePush
    use eka_ci_server::channels::types::ChannelTask;

    channel_sender
        .send(ChannelTask::EvaluatePush {
            channel: channel.clone(),
            sha: sha.to_string(),
        })
        .await
        .expect("Failed to send EvaluatePush task");

    // Give the service a moment to process
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Step 4: Verify a Promoted row was written
    let promoted_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ChannelPromotion WHERE channel_id = ? AND status = ?",
    )
    .bind(&channel.channel_id())
    .bind(PromotionStatus::Promoted.as_i64())
    .fetch_one(&ctx.db_service.pool)
    .await
    .expect("Failed to query promotion status");

    assert_eq!(
        promoted_count, 1,
        "Channel should have promoted the SHA after required jobs succeeded"
    );

    // Step 5: Verify the promotion row has the correct SHA
    let promotion_row = eka_ci_server::db::channels::get_latest_for_sha(
        &channel.channel_id(),
        sha,
        &ctx.db_service.pool,
    )
    .await
    .expect("Failed to get promotion row")
    .expect("Promotion row should exist");

    assert_eq!(promotion_row.tracking_sha, sha);
    assert_eq!(promotion_row.status, PromotionStatus::Promoted.as_i64());
}

#[tokio::test]
async fn test_channel_blocked_on_failed_required_job() {
    // This test verifies that channels are blocked when required jobs fail.

    let ctx = TestContext::new().await.unwrap();

    let channel = test_channel_config("stable", &["failing-job"], &[]);

    let mut channels = HashMap::new();
    channels.insert(channel.channel_id(), channel.clone());

    let channel_service =
        ChannelService::new(ctx.db_service.clone(), Arc::new(channels), None, None);

    let channel_sender = channel_service.get_sender();

    // Spawn the channel service in the background
    let cancel_token = CancellationToken::new();
    tokio::spawn(async move {
        let _ = channel_service.run(cancel_token).await;
    });

    let sha = "failed123abc";

    // Create jobset
    sqlx::query("INSERT INTO GitHubJobSets (owner, repo_name, sha, job) VALUES (?, ?, ?, ?)")
        .bind(&channel.owner)
        .bind(&channel.repo)
        .bind(sha)
        .bind("test-jobset")
        .execute(&ctx.db_service.pool)
        .await
        .expect("Failed to insert jobset");

    let jobset_id: i64 = sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ?")
        .bind(sha)
        .fetch_one(&ctx.db_service.pool)
        .await
        .expect("Failed to get jobset ROWID");

    // Create a FAILED derivation for the required job
    let mut drv = test_drv("failing-job", "x86_64-linux");
    drv.build_state = DrvBuildState::Completed(DrvBuildResult::Failure);

    insert_test_drv(&ctx.db_service, &drv)
        .await
        .expect("Failed to insert test drv");

    let drv_rowid: i64 = sqlx::query_scalar("SELECT ROWID FROM Drv WHERE drv_path = ?")
        .bind(&*drv.drv_path)
        .fetch_one(&ctx.db_service.pool)
        .await
        .expect("Failed to get drv ROWID");

    sqlx::query("INSERT INTO Job (jobset, name, drv_id) VALUES (?, ?, ?)")
        .bind(jobset_id)
        .bind("failing-job")
        .bind(drv_rowid)
        .execute(&ctx.db_service.pool)
        .await
        .expect("Failed to insert job");

    // Trigger channel evaluation
    use eka_ci_server::channels::types::ChannelTask;

    channel_sender
        .send(ChannelTask::EvaluatePush {
            channel: channel.clone(),
            sha: sha.to_string(),
        })
        .await
        .expect("Failed to send EvaluatePush task");

    // Give the service time to process
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Verify the channel was BLOCKED, not promoted
    let blocked_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ChannelPromotion WHERE channel_id = ? AND status = ?",
    )
    .bind(&channel.channel_id())
    .bind(PromotionStatus::Blocked.as_i64())
    .fetch_one(&ctx.db_service.pool)
    .await
    .expect("Failed to query blocked status");

    assert_eq!(
        blocked_count, 1,
        "Channel should be blocked when required jobs fail"
    );

    let promoted_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ChannelPromotion WHERE channel_id = ? AND status = ?",
    )
    .bind(&channel.channel_id())
    .bind(PromotionStatus::Promoted.as_i64())
    .fetch_one(&ctx.db_service.pool)
    .await
    .expect("Failed to query promoted status");

    assert_eq!(
        promoted_count, 0,
        "Channel should NOT be promoted when required jobs fail"
    );
}
