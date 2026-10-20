// Types for the search-index generation subsystem.
//
// Server-specific types live here. Pure-data types (PackageEntry,
// FileEntry, etc.) are re-exported from the `search_index` crate.

use serde::{Deserialize, Serialize};

use crate::config::ChannelConfig;

// Re-export pure-data types from the extracted crate.
pub use search_index::types::{FileEntry, Manifest, OptionEntry, PackageEntry};

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

/// Response payload for `GET /v1/search-index`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchIndexInfo {
    /// Publicly-reachable base URL for index downloads.
    /// Clients fetch `{public_url}/{channel_name}/search.db.zst`.
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
    /// Full URL: `{public_url}/{url_prefix}/search.db`.
    pub url_prefix: String,
}
