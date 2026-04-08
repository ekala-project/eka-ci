use serde::{Deserialize, Serialize};

use crate::ci::config::Check;

/// Task to execute a check for a specific commit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckTask {
    pub check_name: String,
    pub owner: String,
    pub repo_name: String,
    pub sha: String,
    pub config: Check,
}

/// Result of a check execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResultMessage {
    pub check_name: String,
    pub owner: String,
    pub repo_name: String,
    pub sha: String,
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: i64,
    pub check_run_id: i64,
}
