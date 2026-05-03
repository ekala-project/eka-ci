// Helper functions and types for GitLab service

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

/// Convert DrvBuildState to GitLab pipeline status string
pub(super) fn build_state_to_pipeline_status(
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
            "failed".to_string()
        },
        crate::db::model::build_event::DrvBuildState::TransitiveFailure => "failed".to_string(),
        crate::db::model::build_event::DrvBuildState::Interrupted(
            DrvBuildInterruptionKind::Cancelled,
        ) => "canceled".to_string(),
        crate::db::model::build_event::DrvBuildState::Interrupted(_) => "failed".to_string(),
        crate::db::model::build_event::DrvBuildState::UnsatisfiableRequirements => {
            "failed".to_string()
        },
    }
}
