use std::path::PathBuf;

use anyhow::Context;
use sandbox::{Ipv4Cidr, NetworkPolicy};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct SandboxConfig {
    #[serde(default)]
    pub network_allow: Vec<String>,
    #[serde(default)]
    pub network_deny: Vec<String>,
    #[serde(default)]
    pub helper_path: Option<PathBuf>,
}

impl SandboxConfig {
    pub(crate) fn validate(raw: Option<SandboxConfig>) -> anyhow::Result<SandboxConfig> {
        let config = raw.unwrap_or_default();
        config.network_policy()?;
        Ok(config)
    }

    pub fn network_policy(&self) -> anyhow::Result<NetworkPolicy> {
        Ok(NetworkPolicy {
            allow: parse_all(&self.network_allow, "network_allow")?,
            deny: parse_all(&self.network_deny, "network_deny")?,
        })
    }
}

fn parse_all(entries: &[String], field: &str) -> anyhow::Result<Vec<Ipv4Cidr>> {
    entries
        .iter()
        .map(|e| e.parse().with_context(|| format!("[sandbox] {field}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_section_has_empty_policy() {
        let c = SandboxConfig::validate(None).unwrap();
        assert_eq!(c.network_policy().unwrap(), NetworkPolicy::default());
    }

    #[test]
    fn parses_and_rejects_cidrs() {
        let c = SandboxConfig {
            network_allow: vec!["10.1.0.0/16".into(), "192.168.1.5".into()],
            network_deny: vec!["8.8.8.8/32".into()],
            ..Default::default()
        };
        let p = SandboxConfig::validate(Some(c))
            .unwrap()
            .network_policy()
            .unwrap();
        assert_eq!(p.allow.len(), 2);
        assert_eq!(p.deny[0].to_string(), "8.8.8.8/32");
        for bad in ["10.0.0.1/8", "fd00::/8", "nope"] {
            let c = SandboxConfig {
                network_allow: vec![bad.into()],
                ..Default::default()
            };
            assert!(SandboxConfig::validate(Some(c)).is_err(), "{bad}");
        }
    }
}
