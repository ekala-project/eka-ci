use crate::db::model::build_event::DrvBuildState;
use crate::db::model::DrvId;
use sqlx::FromRow;
use std::str::FromStr;

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
    pub build_state: Option<DrvBuildState>,
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
            build_state: None,
        })
    }
}


