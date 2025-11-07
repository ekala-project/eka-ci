use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::value::Value;
use tokio::process::Command;
use tracing::debug;

#[derive(Debug, Deserialize)]
pub(crate) struct DrvOutput {
    // nix derivaiton show always structures the output as:
    // { ${drv}: { ... } }
    #[serde(flatten)]
    pub drvs: HashMap<String, RawDrvInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum EnvAttrs {
    StructuredAttrs { __json: Value },
    LegacyAttrs(LegacyAttrsStruct),
}

#[derive(Debug, Deserialize)]
pub(crate) struct LegacyAttrsStruct {
    pub(crate) name: String,
    pub(crate) pname: Option<String>,
    #[serde(rename = "preferLocalBuild")]
    pub(crate) prefer_local: Option<String>,
    #[serde(rename = "outputHash")]
    pub(crate) output_hash: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RawDrvInfo {
    /// to reattempt the build (depending on the interruption kind).
    pub system: String,

    pub env: EnvAttrs,

    #[serde(rename = "requiredSystemFeatures")]
    pub required_system_features: Option<String>,
}

impl RawDrvInfo {
    pub fn into_drv_info(self) -> Result<DrvInfo> {
        let (name, pname, prefer_local, output_hash) = match self.env {
            // TODO: properly deserialize this. serde_json is getting caught up on
            // some 'unexpected integer'
            EnvAttrs::StructuredAttrs { __json: _ } => {
                ("<structuredAttrs>".to_owned(), None, false, None)
            },
            EnvAttrs::LegacyAttrs(attrs) => {
                println!("attrs: {:?}", &attrs);
                let prefer_local = attrs.prefer_local.map(|x| x == "1").unwrap_or(false);
                (attrs.name, attrs.pname, prefer_local, attrs.output_hash)
            },
        };

        let required_system_features_str = self.required_system_features.clone();
        let required_system_features = self.required_system_features.map(|x| {
            let mut set = std::collections::HashSet::new();
            let features = x.split(",");
            for feature in features {
                set.insert(feature.to_owned());
            }
            set
        });

        let info = DrvInfo {
            name,
            pname,
            prefer_local,
            output_hash,
            required_system_features,
            required_system_features_str,
            system: self.system,
        };

        Ok(info)
    }
}

/// Cleaned up version of the drv we care about, details about
/// structuredAttrs vs legacy have been resolved
#[allow(dead_code)]
pub struct DrvInfo {
    pub name: String,
    pub pname: Option<String>,
    pub prefer_local: bool,
    pub required_system_features: Option<std::collections::HashSet<String>>,
    pub output_hash: Option<String>,
    // This feels redundant, but this is to avoid ordering changing from serializing/deserializing
    // to a HashSet
    pub required_system_features_str: Option<String>,
    pub system: String,
}

impl DrvInfo {
    #[allow(dead_code)]
    pub fn is_fod(&self) -> bool {
        self.output_hash.is_some()
    }
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
    drv_info.into_drv_info()
}
