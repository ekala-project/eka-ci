// Helper functions and types for Gitea service

/// Authorization outcome for comment-driven merge commands
pub(super) enum Authorization {
    Granted {
        #[allow(dead_code)]
        has_write: bool,
    },
    Denied,
    Abort,
}

/// Short SHA for display (7 characters)
pub(super) fn short_sha(sha: &str) -> &str {
    if sha.len() > 7 { &sha[..7] } else { sha }
}

/// Convert DrvBuildState to Gitea state string for database storage
pub(super) fn build_state_to_gitea_state(
    state: &crate::db::model::build_event::DrvBuildState,
) -> String {
    use crate::db::model::build_event::{DrvBuildInterruptionKind, DrvBuildResult};

    match state {
        crate::db::model::build_event::DrvBuildState::Queued
        | crate::db::model::build_event::DrvBuildState::Buildable
        | crate::db::model::build_event::DrvBuildState::Blocked
        | crate::db::model::build_event::DrvBuildState::FailedRetry => "pending".to_string(),
        crate::db::model::build_event::DrvBuildState::Building => "running".to_string(),
        crate::db::model::build_event::DrvBuildState::Completed(DrvBuildResult::Success) => {
            "success".to_string()
        },
        crate::db::model::build_event::DrvBuildState::Completed(DrvBuildResult::Failure) => {
            "failure".to_string()
        },
        crate::db::model::build_event::DrvBuildState::TransitiveFailure => "failure".to_string(),
        crate::db::model::build_event::DrvBuildState::Interrupted(
            DrvBuildInterruptionKind::Cancelled,
        ) => "cancelled".to_string(),
        crate::db::model::build_event::DrvBuildState::Interrupted(_) => "failure".to_string(),
        crate::db::model::build_event::DrvBuildState::UnsatisfiableRequirements => {
            "failure".to_string()
        },
    }
}

/// Convert DrvBuildState to status and conclusion strings for database storage
#[allow(dead_code)]
pub(super) fn status_to_strings(
    state: &crate::db::model::build_event::DrvBuildState,
) -> (String, Option<String>) {
    use crate::db::model::build_event::{DrvBuildInterruptionKind, DrvBuildResult};

    match state {
        crate::db::model::build_event::DrvBuildState::Queued
        | crate::db::model::build_event::DrvBuildState::Buildable
        | crate::db::model::build_event::DrvBuildState::Blocked
        | crate::db::model::build_event::DrvBuildState::FailedRetry => ("queued".to_string(), None),
        crate::db::model::build_event::DrvBuildState::Building => ("in_progress".to_string(), None),
        crate::db::model::build_event::DrvBuildState::Completed(DrvBuildResult::Success) => {
            ("completed".to_string(), Some("success".to_string()))
        },
        crate::db::model::build_event::DrvBuildState::Completed(DrvBuildResult::Failure) => {
            ("completed".to_string(), Some("failure".to_string()))
        },
        crate::db::model::build_event::DrvBuildState::TransitiveFailure => {
            ("completed".to_string(), Some("failure".to_string()))
        },
        crate::db::model::build_event::DrvBuildState::Interrupted(
            DrvBuildInterruptionKind::Cancelled,
        ) => ("completed".to_string(), Some("cancelled".to_string())),
        crate::db::model::build_event::DrvBuildState::Interrupted(_) => {
            ("completed".to_string(), Some("failure".to_string()))
        },
        crate::db::model::build_event::DrvBuildState::UnsatisfiableRequirements => {
            ("completed".to_string(), Some("failure".to_string()))
        },
    }
}
