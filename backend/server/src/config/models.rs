use std::collections::HashMap;

use anyhow::bail;
use serde::{Deserialize, Serialize};

use super::CredentialSource;

/// AI model provider type.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelProvider {
    Anthropic,
    Openai,
    OpenaiCompatible,
}

/// A single `[[models]]` entry in `ekaci.toml`.
///
/// Model IDs are referenced by repository skill definitions (like cache
/// IDs). Credentials stay server-side so untrusted PRs cannot
/// exfiltrate API keys.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ModelConfig {
    /// Unique identifier referenced from `.ekaci/config.json` skills.
    pub id: String,
    /// Whether this is the default model (used when a skill omits `model`).
    #[serde(default)]
    pub default: bool,
    /// Provider type determining the API wire format.
    pub provider: ModelProvider,
    /// Model name passed to the provider API (e.g. `claude-sonnet-4-6`).
    pub model: String,
    /// Base URL override. Required for `openai_compatible`, ignored for
    /// `anthropic` (always `https://api.anthropic.com`).
    #[serde(default)]
    pub base_url: Option<String>,
    /// Credential source for the API key.
    #[serde(default = "default_cred")]
    pub credentials: CredentialSource,
    /// Maximum tokens in the model response. Provider default if unset.
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Sampling temperature. Defaults to 0.0 for deterministic output.
    #[serde(default)]
    pub temperature: Option<f32>,
}

fn default_cred() -> CredentialSource {
    CredentialSource::None
}

/// Validated model registry keyed by model ID, plus the default model ID.
#[derive(Debug, Clone)]
pub struct ModelsRegistry {
    pub models: HashMap<String, ModelConfig>,
    pub default_model_id: Option<String>,
}

/// Validate model configurations and build the registry.
///
/// Rules:
/// 1. Model IDs must be non-empty and unique.
/// 2. At most one model may set `default = true`. If none is marked,
///    the first entry becomes the default.
/// 3. `openai_compatible` requires a `base_url`.
pub(crate) fn validate_models(raw: Vec<ModelConfig>) -> anyhow::Result<ModelsRegistry> {
    if raw.is_empty() {
        return Ok(ModelsRegistry {
            models: HashMap::new(),
            default_model_id: None,
        });
    }

    let mut models = HashMap::with_capacity(raw.len());
    let mut explicit_default: Option<String> = None;
    let first_id = raw[0].id.clone();

    for config in raw {
        if config.id.is_empty() {
            bail!("[[models]] entry has an empty id");
        }

        if config.provider == ModelProvider::OpenaiCompatible && config.base_url.is_none() {
            bail!(
                "[[models]] id={:?}: openai_compatible provider requires a base_url",
                config.id
            );
        }

        if let Some(ref url) = config.base_url {
            if url.is_empty() {
                bail!("[[models]] id={:?}: base_url must not be empty", config.id);
            }
        }

        if config.model.is_empty() {
            bail!(
                "[[models]] id={:?}: model name must not be empty",
                config.id
            );
        }

        if let Some(temp) = config.temperature {
            if !(0.0..=2.0).contains(&temp) {
                bail!(
                    "[[models]] id={:?}: temperature {} is out of range [0.0, 2.0]",
                    config.id,
                    temp,
                );
            }
        }

        if config.default {
            if let Some(ref prev) = explicit_default {
                bail!(
                    "[[models]] multiple defaults: {:?} and {:?}; at most one model may be default",
                    prev,
                    config.id,
                );
            }
            explicit_default = Some(config.id.clone());
        }

        if models.insert(config.id.clone(), config).is_some() {
            bail!("[[models]] duplicate id; each model must have a unique id");
        }
    }

    let default_model_id = explicit_default.unwrap_or(first_id);

    tracing::info!(
        event = "models_configured",
        count = models.len(),
        default = %default_model_id,
        "AI model registry loaded"
    );

    Ok(ModelsRegistry {
        models,
        default_model_id: Some(default_model_id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anthropic(id: &str) -> ModelConfig {
        ModelConfig {
            id: id.to_string(),
            default: false,
            provider: ModelProvider::Anthropic,
            model: "claude-sonnet-4-6".to_string(),
            base_url: None,
            credentials: CredentialSource::None,
            max_tokens: None,
            temperature: None,
        }
    }

    #[test]
    fn empty_is_valid() {
        let reg = validate_models(vec![]).unwrap();
        assert!(reg.models.is_empty());
        assert!(reg.default_model_id.is_none());
    }

    #[test]
    fn first_is_default_when_none_marked() {
        let reg = validate_models(vec![anthropic("a"), anthropic("b")]).unwrap();
        assert_eq!(reg.default_model_id.as_deref(), Some("a"));
    }

    #[test]
    fn explicit_default_wins() {
        let mut b = anthropic("b");
        b.default = true;
        let reg = validate_models(vec![anthropic("a"), b]).unwrap();
        assert_eq!(reg.default_model_id.as_deref(), Some("b"));
    }

    #[test]
    fn duplicate_ids_rejected() {
        assert!(validate_models(vec![anthropic("x"), anthropic("x")]).is_err());
    }

    #[test]
    fn multiple_defaults_rejected() {
        let mut a = anthropic("a");
        a.default = true;
        let mut b = anthropic("b");
        b.default = true;
        assert!(validate_models(vec![a, b]).is_err());
    }

    #[test]
    fn openai_compatible_requires_base_url() {
        let cfg = ModelConfig {
            id: "local".to_string(),
            default: false,
            provider: ModelProvider::OpenaiCompatible,
            model: "llama-3.1".to_string(),
            base_url: None,
            credentials: CredentialSource::None,
            max_tokens: None,
            temperature: None,
        };
        assert!(validate_models(vec![cfg]).is_err());
    }

    #[test]
    fn openai_compatible_with_base_url_ok() {
        let cfg = ModelConfig {
            id: "local".to_string(),
            default: false,
            provider: ModelProvider::OpenaiCompatible,
            model: "llama-3.1".to_string(),
            base_url: Some("http://localhost:8080/v1".to_string()),
            credentials: CredentialSource::None,
            max_tokens: None,
            temperature: None,
        };
        assert!(validate_models(vec![cfg]).is_ok());
    }

    #[test]
    fn temperature_out_of_range_rejected() {
        let mut cfg = anthropic("a");
        cfg.temperature = Some(3.0);
        assert!(validate_models(vec![cfg]).is_err());
    }

    #[test]
    fn empty_model_name_rejected() {
        let mut cfg = anthropic("a");
        cfg.model = String::new();
        assert!(validate_models(vec![cfg]).is_err());
    }
}
