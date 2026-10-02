use std::time::Duration;

use anyhow::bail;
use sandbox::eval::EvalSandboxConfig;
use sandbox::{DEFAULT_MEMORY_LIMIT_MB, DEFAULT_TIMEOUT};
use serde::{Deserialize, Serialize};

const TIMEOUT_MIN_S: u64 = 10;
const TIMEOUT_MAX_S: u64 = 24 * 60 * 60;
const MEMORY_MIN_MB: u64 = 1024;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct EvalConfig {
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_memory_limit_mb")]
    pub memory_limit_mb: u64,
    #[serde(default)]
    pub allowed_uris: Vec<String>,
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self {
            timeout_secs: default_timeout_secs(),
            memory_limit_mb: default_memory_limit_mb(),
            allowed_uris: Vec::new(),
        }
    }
}

fn default_timeout_secs() -> u64 {
    DEFAULT_TIMEOUT.as_secs()
}

fn default_memory_limit_mb() -> u64 {
    DEFAULT_MEMORY_LIMIT_MB
}

fn validate_limits(section: &str, timeout_secs: u64, memory_mb: u64) -> anyhow::Result<()> {
    if !(TIMEOUT_MIN_S..=TIMEOUT_MAX_S).contains(&timeout_secs) {
        bail!(
            "[{section}] timeout_secs must be between {TIMEOUT_MIN_S} and {TIMEOUT_MAX_S}, got \
             {timeout_secs}"
        );
    }
    if memory_mb < MEMORY_MIN_MB {
        bail!("[{section}] memory_limit_mb must be at least {MEMORY_MIN_MB}, got {memory_mb}");
    }
    Ok(())
}

impl EvalConfig {
    pub(crate) fn validate(raw: Option<EvalConfig>) -> anyhow::Result<EvalConfig> {
        let config = raw.unwrap_or_default();
        validate_limits("eval", config.timeout_secs, config.memory_limit_mb)?;
        if let Some(uri) = config
            .allowed_uris
            .iter()
            .find(|u| u.trim().is_empty() || u.contains(char::is_whitespace))
        {
            bail!(
                "[eval] allowed_uris entries must be non-empty and contain no whitespace: {uri:?}"
            );
        }
        Ok(config)
    }

    pub fn sandbox_config(&self) -> EvalSandboxConfig {
        EvalSandboxConfig {
            timeout: Duration::from_secs(self.timeout_secs),
            memory_limit_mb: self.memory_limit_mb,
            allowed_uris: self.allowed_uris.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_section_uses_defaults() {
        let c = EvalConfig::validate(None).unwrap();
        assert_eq!(c, EvalConfig::default());
        assert_eq!(c.sandbox_config(), EvalSandboxConfig::default());
    }

    #[test]
    fn rejects_out_of_range_values() {
        let bad = |f: fn(&mut EvalConfig)| {
            let mut c = EvalConfig::default();
            f(&mut c);
            EvalConfig::validate(Some(c)).is_err()
        };
        assert!(bad(|c| c.timeout_secs = 0));
        assert!(bad(|c| c.timeout_secs = u64::MAX));
        assert!(bad(|c| c.memory_limit_mb = 256));
        assert!(bad(|c| c.allowed_uris = vec!["a b".into()]));
        assert!(bad(|c| c.allowed_uris = vec![String::new()]));
    }

    #[test]
    fn parses_toml_section() {
        let c: EvalConfig = toml_from_str(
            "timeout_secs = 60\nmemory_limit_mb = 2048\nallowed_uris = [\"https://github.com/\"]",
        );
        let s = EvalConfig::validate(Some(c)).unwrap().sandbox_config();
        assert_eq!(s.timeout, Duration::from_secs(60));
        assert_eq!(s.memory_limit_mb, 2048);
        assert_eq!(s.allowed_uris, vec!["https://github.com/".to_string()]);
    }

    fn toml_from_str(s: &str) -> EvalConfig {
        use figment::Figment;
        use figment::providers::{Format, Toml};
        Figment::from(Toml::string(s)).extract().unwrap()
    }
}
