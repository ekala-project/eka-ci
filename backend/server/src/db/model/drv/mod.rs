use std::hash::{Hash, Hasher};
use std::str::FromStr;

use sqlx::FromRow;

use crate::db::DbService;
use crate::db::model::DrvId;
use crate::db::model::build_event::DrvBuildState;

mod queries;
pub use queries::*;

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
}

impl Drv {
    /// Calls `nix derivation show` to retrieve system and requiredSystemFeatures
    pub async fn fetch_info(drv_path: &str, db_service: &DbService) -> anyhow::Result<Self> {
        use crate::nix::derivation_show::drv_output;

        // Check to see if the drv has already been encountered
        let drv_id = DrvId::from_str(drv_path)?;
        let maybe_drv = db_service.get_drv(&drv_id).await?;
        if let Some(drv) = maybe_drv {
            return Ok(drv);
        }

        // This is the first time we have encountered this drv
        let drv_output = drv_output(drv_path).await?;
        let is_fod = drv_output.is_fod();
        Ok(Drv {
            drv_path: DrvId::from_str(drv_path)?,
            system: drv_output.system,
            prefer_local_build: drv_output.prefer_local,
            required_system_features: drv_output.required_system_features_str,
            is_fod,
            build_state: DrvBuildState::Queued,
        })
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
