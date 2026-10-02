use std::time::Duration;

use sandbox::check::Limits;
use sandbox::{DEFAULT_MEMORY_LIMIT_MB, DEFAULT_TIMEOUT};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ChecksConfig {
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_memory_limit_mb")]
    pub memory_limit_mb: u64,
}

impl Default for ChecksConfig {
    fn default() -> Self {
        Self {
            timeout_secs: default_timeout_secs(),
            memory_limit_mb: default_memory_limit_mb(),
        }
    }
}

fn default_timeout_secs() -> u64 {
    DEFAULT_TIMEOUT.as_secs()
}

fn default_memory_limit_mb() -> u64 {
    DEFAULT_MEMORY_LIMIT_MB
}

impl ChecksConfig {
    pub(crate) fn validate(raw: Option<ChecksConfig>) -> anyhow::Result<ChecksConfig> {
        let config = raw.unwrap_or_default();
        super::eval::validate_limits("checks", config.timeout_secs, config.memory_limit_mb)?;
        Ok(config)
    }

    pub fn limits(&self) -> Limits {
        Limits {
            timeout: Duration::from_secs(self.timeout_secs),
            memory_limit_mb: self.memory_limit_mb,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_bounds() {
        let c = ChecksConfig::validate(None).unwrap();
        assert_eq!(c.limits(), Limits::default());
        let bad = |t, m| {
            let c = ChecksConfig {
                timeout_secs: t,
                memory_limit_mb: m,
            };
            ChecksConfig::validate(Some(c)).is_err()
        };
        assert!(bad(0, 8192));
        assert!(bad(u64::MAX, 8192));
        assert!(bad(60, 128));
        assert!(!bad(60, 2048));
    }
}
