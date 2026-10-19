mod anthropic;
mod openai;
pub mod provider;
pub mod types;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;

use crate::config::{ModelConfig, ModelProvider, ModelsRegistry};
use anthropic::AnthropicClient;
use openai::OpenAiClient;
use provider::ModelProviderClient;
pub use types::{Finding, Severity, SkillDecision, SkillVerdict};

/// Facade over the model registry, routing requests to the right
/// provider client based on model ID.
pub struct AiClient {
    clients: HashMap<String, (Arc<dyn ModelProviderClient>, ModelConfig)>,
    default_model_id: Option<String>,
}

impl AiClient {
    /// Build an `AiClient` from validated model configs.
    ///
    /// Credential loading is async because some sources (Vault, AWS
    /// Secrets Manager) require network calls.
    pub async fn from_registry(registry: &ModelsRegistry) -> anyhow::Result<Self> {
        let mut clients: HashMap<String, (Arc<dyn ModelProviderClient>, ModelConfig)> =
            HashMap::with_capacity(registry.models.len());

        for (id, config) in &registry.models {
            let creds = config
                .credentials
                .load()
                .await
                .with_context(|| format!("loading credentials for model {id:?}"))?;

            let client: Arc<dyn ModelProviderClient> = match config.provider {
                ModelProvider::Anthropic => {
                    let api_key = creds.get("ANTHROPIC_API_KEY").cloned().unwrap_or_default();
                    Arc::new(AnthropicClient::new(api_key))
                },
                ModelProvider::Openai => {
                    let api_key = creds.get("OPENAI_API_KEY").cloned().unwrap_or_default();
                    Arc::new(OpenAiClient::new(api_key))
                },
                ModelProvider::OpenaiCompatible => {
                    let base_url = config
                        .base_url
                        .clone()
                        .expect("validated: openai_compatible has base_url");
                    // Use any key present; many local servers don't need one.
                    let api_key = creds.values().next().cloned().unwrap_or_default();
                    Arc::new(OpenAiClient::new_compatible(api_key, base_url))
                },
            };

            clients.insert(id.clone(), (client, config.clone()));
        }

        Ok(Self {
            clients,
            default_model_id: registry.default_model_id.clone(),
        })
    }

    /// Evaluate a skill prompt against a diff. Resolves the model ID
    /// (falling back to the default) and dispatches to the right
    /// provider.
    pub async fn evaluate_skill(
        &self,
        model_id: Option<&str>,
        system_prompt: &str,
        user_content: &str,
    ) -> anyhow::Result<SkillVerdict> {
        let resolved_id = match model_id {
            Some(id) => id.to_string(),
            None => self
                .default_model_id
                .clone()
                .context("no model specified and no default model configured")?,
        };

        let (client, config) = self
            .clients
            .get(&resolved_id)
            .with_context(|| format!("model {resolved_id:?} not found in server model registry"))?;

        let req = provider::EvalRequest {
            system_prompt,
            user_content,
            model: &config.model,
            max_tokens: config.max_tokens.unwrap_or(4096),
            temperature: config.temperature.unwrap_or(0.0),
        };

        client.evaluate(&req).await
    }

    /// Returns true if any models are configured.
    pub fn has_models(&self) -> bool {
        !self.clients.is_empty()
    }
}
