//! Git operations library for EkaCI
//!
//! Provides utilities for managing git repositories, worktrees, and checkouts.

mod actions;
mod types;

// Re-export public types and functions
pub use actions::{add_git_worktree, clone_git_repo, fetch_remote_repo};
pub use types::{GitProtocol, GitRepo, GitWorkspace, workspace_root};
