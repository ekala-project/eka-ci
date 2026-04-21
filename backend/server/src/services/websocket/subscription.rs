use std::collections::HashSet;

use super::events::{ResourceType, ServerEvent};

/// Represents a subscription to a specific resource
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Subscription {
    pub resource: ResourceType,
    pub id: String,
}

impl Subscription {
    pub fn new(resource: ResourceType, id: String) -> Self {
        Self { resource, id }
    }
}

/// Manages subscriptions for a single WebSocket connection
#[derive(Debug, Default)]
pub struct SubscriptionManager {
    subscriptions: HashSet<Subscription>,
}

impl SubscriptionManager {
    pub fn new() -> Self {
        Self {
            subscriptions: HashSet::new(),
        }
    }

    /// Add a subscription
    pub fn subscribe(&mut self, resource: ResourceType, id: String) {
        self.subscriptions.insert(Subscription::new(resource, id));
    }

    /// Remove a subscription
    pub fn unsubscribe(&mut self, resource: ResourceType, id: String) {
        self.subscriptions.remove(&Subscription::new(resource, id));
    }

    /// Check if an event matches any active subscriptions
    pub fn matches(&self, event: &ServerEvent) -> bool {
        match event {
            ServerEvent::BuildStateChange(change) => {
                // Check if subscribed to this specific drv or all builds
                self.subscriptions.contains(&Subscription::new(
                    ResourceType::Drv,
                    change.drv_path.clone(),
                )) || self.subscriptions.contains(&Subscription::new(
                    ResourceType::AllBuilds,
                    "all".to_string(),
                ))
            },
            ServerEvent::JobComplete(complete) => {
                // Check if subscribed to this specific job or all builds
                self.subscriptions.contains(&Subscription::new(
                    ResourceType::Job,
                    complete.jobset_id.to_string(),
                )) || self.subscriptions.contains(&Subscription::new(
                    ResourceType::AllBuilds,
                    "all".to_string(),
                ))
            },
            ServerEvent::LogLine(log) => {
                // Check if subscribed to this specific drv
                self.subscriptions
                    .contains(&Subscription::new(ResourceType::Drv, log.drv_path.clone()))
            },
            ServerEvent::JobStatsUpdate(update) => {
                // Check if subscribed to this specific job or all builds
                self.subscriptions.contains(&Subscription::new(
                    ResourceType::Job,
                    update.jobset_id.to_string(),
                )) || self.subscriptions.contains(&Subscription::new(
                    ResourceType::AllBuilds,
                    "all".to_string(),
                ))
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::super::events::{
        BuildStateChange, JobComplete, JobStatsUpdate, LogLine, ServerEvent,
    };
    use super::*;
    use crate::db::model::build_event::DrvBuildState;

    fn build_state_change(drv: &str) -> ServerEvent {
        ServerEvent::BuildStateChange(BuildStateChange {
            drv_path: drv.to_string(),
            old_state: DrvBuildState::Queued,
            new_state: DrvBuildState::Building,
            timestamp: Utc::now(),
        })
    }

    fn job_complete(jobset_id: i64) -> ServerEvent {
        ServerEvent::JobComplete(JobComplete {
            jobset_id,
            conclusion: "success".to_string(),
            timestamp: Utc::now(),
        })
    }

    fn log_line(drv: &str) -> ServerEvent {
        ServerEvent::LogLine(LogLine {
            drv_path: drv.to_string(),
            line: "hello".to_string(),
            timestamp: Utc::now(),
        })
    }

    fn job_stats(jobset_id: i64) -> ServerEvent {
        ServerEvent::JobStatsUpdate(JobStatsUpdate {
            jobset_id,
            total_drvs: 0,
            queued_drvs: 0,
            buildable_drvs: 0,
            building_drvs: 0,
            completed_success_drvs: 0,
            completed_failure_drvs: 0,
            failed_retry_drvs: 0,
            transitive_failure_drvs: 0,
            blocked_drvs: 0,
            interrupted_drvs: 0,
            timestamp: Utc::now(),
        })
    }

    #[test]
    fn empty_manager_drops_all_events() {
        let manager = SubscriptionManager::new();
        assert!(!manager.matches(&build_state_change("/nix/store/a.drv")));
        assert!(!manager.matches(&job_complete(1)));
        assert!(!manager.matches(&log_line("/nix/store/a.drv")));
        assert!(!manager.matches(&job_stats(1)));
    }

    #[test]
    fn drv_subscription_matches_state_change_and_log() {
        let mut manager = SubscriptionManager::new();
        manager.subscribe(ResourceType::Drv, "/nix/store/a.drv".to_string());

        assert!(manager.matches(&build_state_change("/nix/store/a.drv")));
        assert!(manager.matches(&log_line("/nix/store/a.drv")));

        // Different drv path should not match
        assert!(!manager.matches(&build_state_change("/nix/store/b.drv")));
        assert!(!manager.matches(&log_line("/nix/store/b.drv")));
    }

    #[test]
    fn job_subscription_matches_job_events() {
        let mut manager = SubscriptionManager::new();
        manager.subscribe(ResourceType::Job, "42".to_string());

        assert!(manager.matches(&job_complete(42)));
        assert!(manager.matches(&job_stats(42)));

        // Unrelated jobset id
        assert!(!manager.matches(&job_complete(7)));
        assert!(!manager.matches(&job_stats(7)));

        // Job subscription does not fan out to drv-scoped events
        assert!(!manager.matches(&build_state_change("/nix/store/a.drv")));
        assert!(!manager.matches(&log_line("/nix/store/a.drv")));
    }

    #[test]
    fn all_builds_subscription_matches_broadcast_events_but_not_logs() {
        let mut manager = SubscriptionManager::new();
        manager.subscribe(ResourceType::AllBuilds, "all".to_string());

        // AllBuilds is a firehose for non-log aggregate events
        assert!(manager.matches(&build_state_change("/nix/store/anything.drv")));
        assert!(manager.matches(&job_complete(1)));
        assert!(manager.matches(&job_stats(1)));

        // Log lines are intentionally NOT delivered via AllBuilds: they are
        // only streamed to clients that have explicitly subscribed to the
        // specific drv to avoid firehose-style log volume.
        assert!(!manager.matches(&log_line("/nix/store/anything.drv")));
    }

    #[test]
    fn unsubscribe_removes_matching() {
        let mut manager = SubscriptionManager::new();
        manager.subscribe(ResourceType::Drv, "/nix/store/a.drv".to_string());
        assert!(manager.matches(&build_state_change("/nix/store/a.drv")));

        manager.unsubscribe(ResourceType::Drv, "/nix/store/a.drv".to_string());
        assert!(!manager.matches(&build_state_change("/nix/store/a.drv")));
    }
}
