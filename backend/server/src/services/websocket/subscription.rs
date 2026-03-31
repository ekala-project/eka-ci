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
                // Check if subscribed to this specific drv
                self.subscriptions.contains(&Subscription::new(
                    ResourceType::Drv,
                    change.drv_path.clone(),
                ))
            },
            ServerEvent::JobComplete(complete) => {
                // Check if subscribed to this specific job
                self.subscriptions.contains(&Subscription::new(
                    ResourceType::Job,
                    complete.jobset_id.to_string(),
                ))
            },
            ServerEvent::LogLine(log) => {
                // Check if subscribed to this specific drv
                self.subscriptions
                    .contains(&Subscription::new(ResourceType::Drv, log.drv_path.clone()))
            },
        }
    }
}
