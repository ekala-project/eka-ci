use std::collections::HashMap;

use anyhow::Context;
use serde::Deserialize;
use tokio::process::Command;
use tracing::debug;

#[derive(Debug, Deserialize)]
struct DrvOutput {
    // nix derivaiton show always structures the output as:
    // { ${drv}: { ... } }
    #[serde(flatten)]
    drvs: HashMap<String, DrvAttrs>,
}

#[derive(Debug, Deserialize)]
struct DrvAttrs {
    pub env: DrvInfo,
}

#[derive(Debug, Deserialize)]
pub struct DrvInfo {
    /// to reattempt the build (depending on the interruption kind).
    pub system: String,

    #[serde(rename = "requiredSystemFeatures")]
    pub required_system_features: Option<String>,
}

/// Do `nix derivation show` but filter for the things we care about
pub async fn drv_output(drv_path: &str) -> anyhow::Result<DrvInfo> {
    use anyhow::bail;

    debug!("Fetching derivation information, {:?}", &drv_path);
    let output = Command::new("nix")
        .args(["derivation", "show", drv_path])
        .output()
        .await?
        .stdout;
    if output.is_empty() {
        bail!("failed to fetch info for {:?}", drv_path);
    } else {
        debug!("Successfully fetched info for {}", drv_path);
    }

    let str = String::from_utf8(output)?;
    let drv_output: DrvOutput = serde_json::from_str(&str)?;
    let drv_info = drv_output
        .drvs
        .into_iter()
        .next()
        .context("Invalid derivation show information")?
        .1;
    Ok(drv_info.env)
}
