use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::db::model::build_event::DrvBuildState;

/// Event types that can be sent from the server to WebSocket clients
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    BuildStateChange(BuildStateChange),
    JobComplete(JobComplete),
    LogLine(LogLine),
}

/// Build state change event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildStateChange {
    pub drv_path: String,
    pub old_state: DrvBuildState,
    pub new_state: DrvBuildState,
    pub timestamp: DateTime<Utc>,
}

/// Job completion event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobComplete {
    pub jobset_id: i64,
    pub conclusion: String,
    pub timestamp: DateTime<Utc>,
}

/// Log line event for streaming build logs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    pub drv_path: String,
    pub line: String,
    pub timestamp: DateTime<Utc>,
}

/// Client messages for subscribing/unsubscribing
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Subscribe(SubscribeMessage),
    Unsubscribe(UnsubscribeMessage),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeMessage {
    pub resource: ResourceType,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsubscribeMessage {
    pub resource: ResourceType,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ResourceType {
    Commit,
    Job,
    Drv,
}
