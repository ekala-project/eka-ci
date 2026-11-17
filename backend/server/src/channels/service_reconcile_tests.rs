//! Tests for `ChannelTask::Reconcile` handling.

use std::collections::HashMap;
use std::sync::Arc;

use super::ChannelService;
use crate::channels::types::PromotionStatus;
use crate::config::{ChannelConfig, ChannelForge};
use crate::db::DbService;

fn channel(required: &[&str]) -> ChannelConfig {
    ChannelConfig {
        forge: ChannelForge::GitHub,
        owner: "ekacorp".to_string(),
        repo: "ekapkgs".to_string(),
        name: "stable".to_string(),
        tracking_branch: "master".to_string(),
        target_branch: "stable-unstable".to_string(),
        required: required.iter().map(|s| s.to_string()).collect(),
        packages: Vec::new(),
        dry_run: true,
    }
}

fn service(db: &DbService, ch: &ChannelConfig) -> ChannelService {
    let registry = HashMap::from([(ch.channel_id(), ch.clone())]);
    ChannelService::new(db.clone(), Arc::new(registry), None, None, None)
}

async fn count(db: &DbService, ch: &ChannelConfig, status: PromotionStatus) -> i64 {
    crate::db::channels::count_by_status(&ch.channel_id(), status, &db.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn reconcile_evaluates_a_missed_push() {
    let db = DbService::new_in_memory().await.unwrap();
    let ch = channel(&[]);
    let svc = service(&db, &ch);

    svc.handle_reconcile(ch.clone(), "sha-missed".to_string())
        .await
        .unwrap();

    assert_eq!(count(&db, &ch, PromotionStatus::Promoted).await, 1);
}

#[tokio::test]
async fn reconcile_finishes_in_flight_whose_jobset_complete_was_lost() {
    // An Evaluating row whose jobs have all concluded, but whose
    // JobsetComplete never arrived: only a re-check can finish it.
    let db = DbService::new_in_memory().await.unwrap();
    let ch = channel(&[]);
    crate::db::channels::insert_evaluating(&ch.channel_id(), "sha-1", &ch.target_branch, &db.pool)
        .await
        .unwrap();
    let svc = service(&db, &ch);

    svc.handle_reconcile(ch.clone(), "sha-1".to_string())
        .await
        .unwrap();

    assert_eq!(count(&db, &ch, PromotionStatus::Promoted).await, 1);
    assert_eq!(count(&db, &ch, PromotionStatus::Evaluating).await, 0);
}

#[tokio::test]
async fn reconcile_restores_pending_head_lost_on_restart() {
    let db = DbService::new_in_memory().await.unwrap();
    let ch = channel(&["coreutils"]);
    let before_restart = service(&db, &ch);
    before_restart
        .handle_evaluate_push(ch.clone(), "sha-old".to_string())
        .await
        .unwrap();

    // A fresh service has lost the in-memory pending SHA.
    let after_restart = service(&db, &ch);
    after_restart
        .handle_reconcile(ch.clone(), "sha-new".to_string())
        .await
        .unwrap();

    let pending = after_restart.pending.lock().await;
    assert_eq!(
        pending.get(&ch.channel_id()).map(String::as_str),
        Some("sha-new")
    );
    assert_eq!(count(&db, &ch, PromotionStatus::Evaluating).await, 1);
}

#[tokio::test]
async fn reconcile_is_idempotent_for_a_decided_head() {
    let db = DbService::new_in_memory().await.unwrap();
    let ch = channel(&[]);
    let svc = service(&db, &ch);
    svc.handle_evaluate_push(ch.clone(), "sha-1".to_string())
        .await
        .unwrap();

    svc.handle_reconcile(ch.clone(), "sha-1".to_string())
        .await
        .unwrap();

    assert_eq!(count(&db, &ch, PromotionStatus::Promoted).await, 1);
}
