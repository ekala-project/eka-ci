// Types for the search-index generation subsystem.
//
// Kept as a plain-data module so the generator and upload modules can
// reference these types without pulling in the service machinery.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::config::ChannelConfig;

/// Tasks accepted by the SearchIndexService over its mpsc channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SearchIndexTask {
    /// Generate and upload search indexes for a newly-promoted channel.
    GenerateIndexes {
        /// Stable channel identifier (e.g. "github/ekacorp/ekapkgs/unstable").
        channel_id: String,
        /// Channel configuration at the time of promotion.
        channel: ChannelConfig,
        /// The commit SHA that was promoted.
        sha: String,
    },
}

/// A single entry in `packages.json.zst`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageEntry {
    /// Attribute path (e.g. "hello", "python3Packages.requests").
    pub attr: String,
    /// Package name.
    pub pname: String,
    /// Package version.
    pub version: String,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Output names (e.g. ["out", "dev"]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,
    /// Primary binary name from `meta.mainProgram`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_program: Option<String>,
}

/// A single entry in `files.json.zst`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative path within the store output (e.g. "bin/hello").
    pub file: String,
    /// Attribute path of the containing package.
    pub package: String,
    /// Output name (usually "out").
    pub output: String,
}

/// A single entry in `options.json.zst`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionEntry {
    /// Option path (e.g. "services.openssh.enable").
    pub name: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Option type (e.g. "boolean", "list of string").
    #[serde(default, rename = "type")]
    pub type_name: String,
    /// JSON-serialized default value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// JSON-serialized example value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example: Option<String>,
    /// Source file declarations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declarations: Vec<String>,
    /// Whether the option is read-only.
    #[serde(default)]
    pub read_only: bool,
}

/// Per-index metadata in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexInfo {
    /// Compressed size in bytes.
    pub size: u64,
    /// Number of entries in the index.
    pub entries: usize,
}

/// Top-level manifest (`manifest.json`) describing the current set of
/// indexes at a given URL prefix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// ISO 8601 timestamp of generation.
    pub generated_at: String,
    /// Channel name that produced this index (e.g. "unstable").
    pub channel_name: String,
    /// The nixpkgs revision (commit SHA) these indexes are built against.
    pub nixpkgs_rev: String,
    /// Per-index metadata keyed by index name ("packages", "files", etc.).
    pub indexes: HashMap<String, IndexInfo>,
}

/// Response payload for `GET /v1/search-index`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchIndexInfo {
    /// Publicly-reachable base URL for index downloads.
    /// Clients fetch `{public_url}/{channel_name}/packages.json.zst`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    /// Available channels with their latest index metadata.
    pub channels: Vec<ChannelIndexInfo>,
}

/// Per-channel index metadata returned in the discovery endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelIndexInfo {
    /// Channel name (e.g. "unstable").
    pub name: String,
    /// Full channel identifier (e.g. "github/ekacorp/ekapkgs/unstable").
    pub channel_id: String,
    /// URL path segment for this channel's indexes.
    /// Full URL: `{public_url}/{url_prefix}/packages.json.zst`.
    pub url_prefix: String,
}
