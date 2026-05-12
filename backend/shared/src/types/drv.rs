use std::hash::{Hash, Hasher};

use super::{DrvBuildState, DrvId};

#[derive(Debug, Clone)]
pub struct Drv {
    /// Derivation identifier.
    pub drv_path: DrvId,

    /// System platform this derivation targets (e.g., "x86_64-linux")
    pub system: String,

    /// Whether this derivation prefers to be built locally rather than remotely
    pub prefer_local_build: bool,

    /// Required system features (comma-separated) for building this derivation
    pub required_system_features: Option<String>,

    /// Whether this is a Fixed-Output Derivation (FOD)
    /// FODs have a known output hash (e.g., fetchurl, fetchgit)
    pub is_fod: bool,

    /// Current build status
    pub build_state: DrvBuildState,

    /// Output size in bytes (NAR size of all outputs)
    /// None if not yet calculated or build hasn't completed
    pub output_size: Option<i64>,

    /// Closure size in bytes (size of output + all runtime dependencies)
    /// None if not yet calculated or build hasn't completed
    pub closure_size: Option<i64>,

    /// Package name (e.g., "hello"), from `meta.pname` or heuristically
    /// extracted from `name`. None for derivations without parseable names.
    pub pname: Option<String>,

    /// Package version (e.g., "2.12.1"), from `meta.version` or heuristically
    /// extracted from `name`. None when no version segment can be identified.
    pub version: Option<String>,

    /// Normalized JSON list of license entries:
    /// `[{"spdxId"?, "shortName"?, "fullName"?, "free"?}]`.
    /// None when meta is unavailable.
    pub license_json: Option<String>,

    /// Normalized JSON list of maintainer entries:
    /// `[{"github"?, "name"?, "email"?}]`. None when meta is unavailable.
    pub maintainers_json: Option<String>,

    /// "file:line" reference to the meta declaration site, from `meta.position`.
    pub meta_position: Option<String>,

    /// Marked broken by upstream `meta.broken`.
    pub broken: Option<bool>,

    /// Marked insecure by upstream `meta.insecure`.
    pub insecure: Option<bool>,
}

impl PartialEq for Drv {
    fn eq(&self, other: &Self) -> bool {
        self.drv_path == other.drv_path
    }
}

impl Eq for Drv {}

impl Hash for Drv {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Only hash drvId as it should always be unique
        self.drv_path.hash(state);
    }
}
