// Types for the search-index generation subsystem.
//
// Pure-data types used by the generators and consumed by the CLI
// and server. No server-specific dependencies.

use serde::{Deserialize, Serialize};

/// A single entry in the `packages` table.
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

/// A single entry in the `files` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative path within the store output (e.g. "bin/hello").
    pub file: String,
    /// Attribute path of the containing package.
    pub package: String,
    /// Output name (usually "out").
    pub output: String,
}

/// A single entry in the `options` table.
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

/// Top-level manifest stored in the `metadata` table of the SQLite
/// database and also returned by the discovery endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// ISO 8601 timestamp of generation.
    pub generated_at: String,
    /// Channel name that produced this index (e.g. "unstable").
    pub channel_name: String,
    /// The nixpkgs revision (commit SHA) these indexes are built against.
    pub nixpkgs_rev: String,
    /// Number of packages in the database.
    pub package_count: usize,
    /// Number of file entries in the database.
    pub file_count: usize,
    /// Number of option entries in the database.
    pub option_count: usize,
}
