use anyhow::Result;
use async_trait::async_trait;

use super::types::SkillVerdict;

/// Parameters for a skill evaluation request.
pub struct EvalRequest<'a> {
    pub system_prompt: &'a str,
    pub user_content: &'a str,
    pub model: &'a str,
    pub max_tokens: u32,
    pub temperature: f32,
}

/// Trait abstracting an AI model provider's HTTP API.
///
/// Each implementation handles the wire format differences (Anthropic
/// Messages API, OpenAI Chat Completions, etc.) and returns a parsed
/// `SkillVerdict`. When the provider supports native structured output
/// the implementation should use it; otherwise it falls back to
/// prompt-based JSON extraction.
#[async_trait]
pub trait ModelProviderClient: Send + Sync {
    /// Send a skill evaluation request and parse the structured verdict.
    async fn evaluate(&self, req: &EvalRequest<'_>) -> Result<SkillVerdict>;
}
