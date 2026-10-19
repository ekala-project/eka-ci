use anyhow::{Context, bail};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::provider::{EvalRequest, ModelProviderClient};
use super::types::{SkillVerdict, VERDICT_SCHEMA_PROMPT};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";

/// Anthropic Messages API client.
pub struct AnthropicClient {
    client: Client,
    api_key: String,
}

impl AnthropicClient {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
        }
    }
}

#[derive(Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    temperature: f32,
    system: &'a str,
    messages: Vec<Message<'a>>,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
}

#[async_trait]
impl ModelProviderClient for AnthropicClient {
    async fn evaluate(&self, req: &EvalRequest<'_>) -> anyhow::Result<SkillVerdict> {
        // Append the verdict schema instruction to the system prompt so
        // the model returns parseable JSON even without native structured
        // output (Anthropic supports it but the schema-free approach
        // keeps us compatible with older API versions).
        let system = format!("{}\n\n{}", req.system_prompt, VERDICT_SCHEMA_PROMPT);

        let body = MessagesRequest {
            model: req.model,
            max_tokens: req.max_tokens,
            temperature: req.temperature,
            system: &system,
            messages: vec![Message {
                role: "user",
                content: req.user_content,
            }],
        };

        let resp = self
            .client
            .post(API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("failed to send request to Anthropic API")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable>".to_string());
            bail!("Anthropic API returned {status}: {body}");
        }

        let response: MessagesResponse = resp
            .json()
            .await
            .context("failed to parse Anthropic API response")?;

        let text = response
            .content
            .into_iter()
            .find_map(|block| match block {
                ContentBlock::Text { text } => Some(text),
            })
            .context("Anthropic response contained no text content")?;

        parse_verdict(&text)
    }
}

/// Parse a verdict from model output text, stripping any markdown
/// fencing or surrounding whitespace.
fn parse_verdict(text: &str) -> anyhow::Result<SkillVerdict> {
    let trimmed = text.trim();

    // Strip markdown JSON fencing if present
    let json_str = if trimmed.starts_with("```") {
        let start = trimmed
            .find('{')
            .context("markdown-fenced response contains no JSON object")?;
        let end = trimmed
            .rfind('}')
            .context("markdown-fenced response contains no closing brace")?;
        &trimmed[start..=end]
    } else {
        trimmed
    };

    serde_json::from_str(json_str).context("failed to parse model output as SkillVerdict JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::types::SkillDecision;

    #[test]
    fn parse_clean_json() {
        let text = r#"{"decision":"pass","reason":"ok","findings":[]}"#;
        let v = parse_verdict(text).unwrap();
        assert_eq!(v.decision, SkillDecision::Pass);
    }

    #[test]
    fn parse_fenced_json() {
        let text = "```json\n{\"decision\":\"fail\",\"reason\":\"bad\",\"findings\":[]}\n```";
        let v = parse_verdict(text).unwrap();
        assert_eq!(v.decision, SkillDecision::Fail);
    }

    #[test]
    fn parse_with_whitespace() {
        let text = "  \n{\"decision\":\"pass\",\"reason\":\"ok\"}\n  ";
        let v = parse_verdict(text).unwrap();
        assert_eq!(v.decision, SkillDecision::Pass);
    }

    #[test]
    fn parse_garbage_fails() {
        assert!(parse_verdict("not json at all").is_err());
    }
}
