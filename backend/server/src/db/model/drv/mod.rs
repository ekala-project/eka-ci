use std::hash::{Hash, Hasher};

use sqlx::FromRow;

use crate::db::model::DrvId;
use crate::db::model::build_event::DrvBuildState;

mod insert;
mod queries;
mod traversal;
pub use insert::*;
pub use queries::*;
pub use traversal::*;

#[derive(Debug, Clone, FromRow)]
pub struct Drv {
    /// Derivation identifier.
    pub drv_path: DrvId,

    /// to reattempt the build (depending on the interruption kind).
    pub system: String,

    #[sqlx(skip)]
    pub prefer_local_build: bool,
    pub required_system_features: Option<String>,

    /// Whether this is a Fixed-Output Derivation (FOD)
    /// FODs have a known output hash (e.g., fetchurl, fetchgit)
    pub is_fod: bool,

    /// This is None when queried from Nix
    /// Otherwise, it is the latest build status
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

impl Drv {
    /// Build a new, queued `Drv` row from parsed `nix derivation show`
    /// output (see `crate::nix::traverse`, which batches the lookups).
    pub fn from_info(drv_path: DrvId, info: &crate::nix::DrvInfo) -> Self {
        Drv {
            drv_path,
            system: info.system.clone(),
            prefer_local_build: info.prefer_local,
            required_system_features: info.required_system_features_str.clone(),
            is_fod: info.is_fod(),
            build_state: DrvBuildState::Queued,
            output_size: None,  // Will be calculated after successful build
            closure_size: None, // Will be calculated after successful build
            // Package metadata is populated later when an evaluation provides
            // it via NixEvalDrv. `nix derivation show` does not surface meta,
            // so we leave these as None for now (insert_drv keeps existing
            // values via COALESCE on conflict).
            pname: None,
            version: None,
            license_json: None,
            maintainers_json: None,
            meta_position: None,
            broken: None,
            insecure: None,
        }
    }
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
