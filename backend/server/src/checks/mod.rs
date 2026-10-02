pub mod executor;
pub mod types;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

impl CheckResult {
    pub fn new(
        success: bool,
        exit_code: i32,
        stdout: String,
        stderr: String,
        duration_ms: u64,
    ) -> Self {
        Self {
            success,
            exit_code,
            stdout,
            stderr,
            duration_ms,
        }
    }
}
