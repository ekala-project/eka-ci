use std::hash::{Hash, Hasher};
use std::str::FromStr;

use sqlx::FromRow;

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

    pub required_system_features: Option<String>,

    /// This is None when queried from Nix
    /// Otherwise, it is the latest build status
    pub build_state: DrvBuildState,
}

impl Drv {
    /// Calls `nix derivation show` to retrieve system and requiredSystemFeatures
    pub async fn fetch_info(drv_path: &str) -> anyhow::Result<Self> {
        use crate::nix::derivation_show::drv_output;
        let drv_output = drv_output(drv_path).await?;
        Ok(Drv {
            drv_path: DrvId::from_str(drv_path)?,
            system: drv_output.system,
            required_system_features: drv_output.required_system_features,
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
