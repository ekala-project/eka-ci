use std::time::Duration;

use anyhow::bail;
use sandbox::eval::EvalSandboxConfig;
use sandbox::{DEFAULT_MEMORY_LIMIT_MB, DEFAULT_TIMEOUT};
use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer, Serialize};
use tracing::warn;

const TIMEOUT_MIN_S: u64 = 10;
const TIMEOUT_MAX_S: u64 = 24 * 60 * 60;
const MEMORY_MIN_MB: u64 = 1024;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct EvalConfig {
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_memory_limit_mb")]
    pub memory_limit_mb: u64,
    /// Pass `--provenance` to `iets eval-jobs` (default: false).
    #[serde(default)]
    pub provenance: bool,
    /// Removed `allowed_uris` key: Nix-level `restrict-eval` was dropped with
    /// the switch to `iets`, which evaluates in pure mode. Parsed only
    /// so that setting it logs a deprecation warning instead of being
    /// silently ignored.
    #[serde(default, skip_serializing)]
    allowed_uris: Option<Removed>,
}

/// A removed config key that is still accepted, whatever its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Removed;

impl<'de> Deserialize<'de> for Removed {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        IgnoredAny::deserialize(deserializer)?;
        Ok(Removed)
    }
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self {
            timeout_secs: default_timeout_secs(),
            memory_limit_mb: default_memory_limit_mb(),
            provenance: false,
            allowed_uris: None,
        }
    }
}

fn default_timeout_secs() -> u64 {
    DEFAULT_TIMEOUT.as_secs()
}

fn default_memory_limit_mb() -> u64 {
    DEFAULT_MEMORY_LIMIT_MB
}

pub(super) fn validate_limits(
    section: &str,
    timeout_secs: u64,
    memory_mb: u64,
) -> anyhow::Result<()> {
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
        for key in config.removed_keys() {
            warn!(
                event = "config_key_removed",
                key = %format_args!("[eval] {key}"),
                "[eval] {key} is no longer supported and has no effect: iets evaluates in pure \
                 mode, and eval-time network access is controlled by the sandbox. Remove it from \
                 the configuration."
            );
        }
        Ok(config)
    }

    /// Removed keys set in the `[eval]` section.
    fn removed_keys(&self) -> Vec<&'static str> {
        let mut keys = Vec::new();
        if self.allowed_uris.is_some() {
            keys.push("allowed_uris");
        }
        keys
    }

    pub fn shell_limits(&self) -> sandbox::check::Limits {
        sandbox::check::Limits {
            timeout: Duration::from_secs(self.timeout_secs),
            memory_limit_mb: self.memory_limit_mb,
        }
    }

    pub fn sandbox_config(&self) -> EvalSandboxConfig {
        EvalSandboxConfig {
            timeout: Duration::from_secs(self.timeout_secs),
            memory_limit_mb: self.memory_limit_mb,
            provenance: self.provenance,
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
    }

    #[test]
    fn parses_toml_section() {
        let c: EvalConfig =
            toml_from_str("timeout_secs = 60\nmemory_limit_mb = 2048\nprovenance = true");
        let s = EvalConfig::validate(Some(c)).unwrap().sandbox_config();
        assert_eq!(s.timeout, Duration::from_secs(60));
        assert_eq!(s.memory_limit_mb, 2048);
        assert!(s.provenance);
    }

    #[test]
    fn removed_allowed_uris_is_accepted_and_reported() {
        for value in ["[\"https://github.com/\"]", "[]", "\"https://example.org\""] {
            let c = toml_from_str(&format!("timeout_secs = 60\nallowed_uris = {value}"));
            assert_eq!(c.removed_keys(), vec!["allowed_uris"], "{value}");
            assert_eq!(c.timeout_secs, 60);
        }
        assert!(toml_from_str("provenance = true").removed_keys().is_empty());
        assert!(EvalConfig::default().removed_keys().is_empty());
    }

    #[test]
    fn validate_warns_about_allowed_uris() {
        let c = toml_from_str("allowed_uris = [\"https://github.com/\"]");
        let logs = capture_logs(|| {
            let validated = EvalConfig::validate(Some(c)).unwrap();
            assert_eq!(validated.sandbox_config(), EvalSandboxConfig::default());
        });
        assert!(logs.contains("WARN"), "{logs}");
        assert!(
            logs.contains("[eval] allowed_uris is no longer supported"),
            "{logs}"
        );

        let logs = capture_logs(|| {
            EvalConfig::validate(Some(EvalConfig::default())).unwrap();
        });
        assert!(!logs.contains("allowed_uris"), "{logs}");
    }

    /// The whole-file pipeline of `Config::from_env` (defaults, then the
    /// TOML file) still accepts a config that sets the removed key.
    #[test]
    fn config_file_with_allowed_uris_still_loads() {
        use figment::Figment;
        use figment::providers::{Format, Serialized, Toml};

        use crate::config::ConfigFile;

        let toml = "[eval]\ntimeout_secs = 60\nallowed_uris = [\"https://github.com/\"]\n";
        let file: ConfigFile = Figment::from(Serialized::defaults(ConfigFile::default()))
            .merge(Toml::string(toml))
            .extract()
            .unwrap();
        let eval = file.eval.expect("[eval] section");
        assert_eq!(eval.timeout_secs, 60);
        assert_eq!(eval.removed_keys(), vec!["allowed_uris"]);
    }

    /// The removed key is not written back, e.g. into figment's defaults.
    #[test]
    fn allowed_uris_is_not_serialized() {
        let c = toml_from_str("allowed_uris = []");
        let json = serde_json::to_value(&c).unwrap();
        assert!(json.get("allowed_uris").is_none(), "{json}");
    }

    /// Run `f` with a subscriber that records formatted log lines.
    fn capture_logs(f: impl FnOnce()) -> String {
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Default)]
        struct Buf(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(data);
                Ok(data.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let buf = Buf::default();
        let writer = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        let logs = buf.0.lock().unwrap().clone();
        String::from_utf8(logs).unwrap()
    }

    fn toml_from_str(s: &str) -> EvalConfig {
        use figment::Figment;
        use figment::providers::{Format, Toml};
        Figment::from(Toml::string(s)).extract().unwrap()
    }
}
