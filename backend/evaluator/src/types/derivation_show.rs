use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;
use tracing::debug;

fn deserialize_json_string<'de, D>(deserializer: D) -> Result<AttrsStruct, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    serde_json::from_str(&s).map_err(serde::de::Error::custom)
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum DrvOutput {
    // nix >= 2.28 wraps the output in a `derivations` key with a `version` field:
    // { "derivations": { ${drv}: { ... } }, "version": 4 }
    Versioned {
        derivations: HashMap<String, RawDrvInfo>,
    },
    // older nix versions use a flat map:
    // { ${drv}: { ... } }
    Legacy(HashMap<String, RawDrvInfo>),
}

impl DrvOutput {
    pub fn into_drvs(self) -> HashMap<String, RawDrvInfo> {
        match self {
            DrvOutput::Versioned { derivations } => derivations,
            DrvOutput::Legacy(drvs) => drvs,
        }
    }
}

/// In the legacy nix format, `env.__json` contains an escaped JSON string with
/// the structured attrs. In the versioned format, structured attrs are a
/// top-level `structuredAttrs` field with already-parsed JSON.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum EnvAttrs {
    StructuredAttrs {
        // nix derivation show renders `__structuredAttrs` as an escaped JSON string
        // which contains the inner env attr set. So we have to deserialize it twice.
        #[serde(deserialize_with = "deserialize_json_string")]
        __json: AttrsStruct,
    },
    LegacyAttrs(AttrsStruct),
}

/// The fields we care about from the derivation's attributes, whether they come
/// from `env` (legacy), `env.__json` (legacy structured), or `structuredAttrs` (v4).
#[derive(Debug, Deserialize)]
pub struct AttrsStruct {
    pub name: String,
    pub pname: Option<String>,
    #[serde(rename = "preferLocalBuild")]
    pub prefer_local: Option<PreferLocalValue>,
    #[serde(rename = "outputHash")]
    pub output_hash: Option<String>,
}

/// In legacy format, preferLocalBuild is a string ("1" or "").
/// In versioned structuredAttrs format, it's a boolean.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum PreferLocalValue {
    Bool(bool),
    Str(String),
}

impl PreferLocalValue {
    pub fn is_true(&self) -> bool {
        match self {
            PreferLocalValue::Bool(b) => *b,
            PreferLocalValue::Str(s) => s == "1",
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct RawDrvInfo {
    pub system: String,

    /// In legacy format, env contains all attrs (or __json for structured attrs).
    /// In versioned format for structured attrs, env only contains output paths.
    pub env: Value,

    /// In versioned format (v4), structured attrs are a top-level field with
    /// already-parsed JSON (not an escaped string).
    #[serde(rename = "structuredAttrs")]
    pub structured_attrs: Option<AttrsStruct>,

    /// The derivation name, available at top level in versioned format.
    pub name: Option<String>,

    #[serde(rename = "requiredSystemFeatures")]
    pub required_system_features: Option<String>,

    /// Map of output names to their store paths
    pub outputs: Option<HashMap<String, DrvOutputInfo>>,
}

#[derive(Debug, Deserialize)]
pub struct DrvOutputInfo {
    /// Store path of the output. Absent for fixed-output derivations in v4
    /// format, which use `hash` + `method` instead.
    pub path: Option<String>,
}

impl RawDrvInfo {
    pub fn into_drv_info(self) -> Result<DrvInfo> {
        let attrs = if let Some(sa) = self.structured_attrs {
            // Versioned format (v4) with top-level structuredAttrs
            sa
        } else {
            // Legacy format: env contains attrs directly or via __json
            let env_attrs: EnvAttrs =
                serde_json::from_value(self.env).context("Failed to parse env attrs")?;
            match env_attrs {
                EnvAttrs::StructuredAttrs { __json: a } => a,
                EnvAttrs::LegacyAttrs(a) => a,
            }
        };

        let prefer_local = attrs.prefer_local.map(|x| x.is_true()).unwrap_or(false);

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
            name: attrs.name,
            pname: attrs.pname,
            prefer_local,
            output_hash: attrs.output_hash,
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
    let drv_output: DrvOutput = serde_json::from_str(&str).with_context(|| {
        let snippet = if str.len() > 200 { &str[..200] } else { &str };
        format!(
            "Failed to parse `nix derivation show` output for {}: {}...",
            drv_path, snippet
        )
    })?;
    let drv_info = drv_output
        .into_drvs()
        .into_iter()
        .next()
        .with_context(|| format!("Empty `nix derivation show` output for {}", drv_path))?
        .1;
    drv_info
        .into_drv_info()
        .with_context(|| format!("Failed to extract drv info for {}", drv_path))
}
