use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for a post-build hook
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostBuildHook {
    /// Name of the hook (used for logging and identification)
    pub name: String,

    /// Command and arguments to execute
    /// Environment variables can be referenced with shell syntax (e.g., "$OUT_PATHS")
    pub command: Vec<String>,

    /// Additional environment variables to set
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Task sent to the HookExecutor service
#[derive(Debug, Clone)]
pub struct HookTask {
    /// Path to the derivation that was built
    pub drv_path: String,

    /// Output store paths from the build
    pub out_paths: Vec<String>,

    /// Hooks to execute (regular + FOD-specific if applicable)
    pub hooks: Vec<PostBuildHook>,

    /// Additional context about the build
    pub context: HookContext,
}

/// Context information provided to hooks
#[derive(Debug, Clone)]
pub struct HookContext {
    /// Name of the job from config
    pub job_name: String,

    /// Whether this is a fixed-output derivation
    pub is_fod: bool,

    /// Build system (e.g., "x86_64-linux")
    pub system: String,

    /// Package name (if available)
    pub pname: Option<String>,

    /// Path to the build log file
    pub build_log_path: String,

    /// Git commit SHA being built
    pub commit_sha: String,
}

/// Result of executing a hook
#[derive(Debug, Clone)]
pub struct HookResult {
    /// Name of the hook that was executed
    pub hook_name: String,

    /// Exit code from the hook command
    pub exit_code: Option<i32>,

    /// Whether the hook execution succeeded
    pub success: bool,

    /// Path to the hook's log file
    pub log_path: String,
}
