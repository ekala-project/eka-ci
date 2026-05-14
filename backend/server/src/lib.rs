//! EkaCI Server Library
//!
//! This library crate exposes the server modules for testing.

pub mod auth;
pub mod auto_merge;
pub mod cache_permissions;
pub mod change_summary;
pub mod channels;
pub mod checks;
pub mod ci;
pub mod client;
pub mod config;
pub mod db;
pub mod dependency_comparison;
pub mod git;
pub mod gitea;
pub mod github;
pub mod github_permissions;
pub mod gitlab;
// Re-export the graph crate to maintain backward compatibility with existing imports
pub use graph;
pub mod graph_compat;
pub mod hooks;
pub mod jobset_data;
pub mod metrics;
pub mod nix;
pub mod path_safety;
pub mod scheduler;
pub mod secret;
pub mod services;
pub mod web;
pub mod webhook_security;
