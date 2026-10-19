use anyhow::{Context, bail};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::provider::{EvalRequest, ModelProviderClient};
use super::types::{SkillVerdict, VERDICT_SCHEMA_PROMPT};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// OpenAI Chat Completions client.
///
/// Also used for OpenAI-compatible providers (vLLM, Ollama, etc.) by
/// overriding `base_url`. When `native_structured_output` is false,
/// the verdict schema is injected into the system prompt instead of
/// using `response_format`.
pub struct OpenAiClient {
    client: Client,
    api_key: String,
    base_url: String,
    native_structured_output: bool,
}

impl OpenAiClient {
    /// Create a client for the official OpenAI API (native structured
    /// output enabled).
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url: DEFAULT_BASE_URL.to_string(),
            native_structured_output: true,
        }
    }

    /// Create a client for an OpenAI-compatible endpoint. Native
    /// structured output is disabled since most compatible servers do
    /// not support `response_format.json_schema`.
    pub fn new_compatible(api_key: String, base_url: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url,
            native_structured_output: false,
        }
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    temperature: f32,
    messages: Vec<ChatMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
}

fn verdict_json_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "skill_verdict",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "decision": { "type": "string", "enum": ["pass", "fail"] },
                    "reason": { "type": "string" },
                    "findings": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "severity": { "type": "string", "enum": ["critical", "warning", "info"] },
                                "description": { "type": "string" },
                                "file": { "type": ["string", "null"] },
                                "line": { "type": ["integer", "null"] }
                            },
                            "required": ["severity", "description", "file", "line"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["decision", "reason", "findings"],
                "additionalProperties": false
            }
        }
    })
}

#[async_trait]
impl ModelProviderClient for OpenAiClient {
    async fn evaluate(&self, req: &EvalRequest<'_>) -> anyhow::Result<SkillVerdict> {
        let system_content;
        let system_prompt = if self.native_structured_output {
            req.system_prompt
        } else {
            system_content = format!("{}\n\n{}", req.system_prompt, VERDICT_SCHEMA_PROMPT);
            &system_content
        };

        let response_format = if self.native_structured_output {
            Some(verdict_json_schema())
        } else {
            None
        };

        let body = ChatRequest {
            model: req.model,
            max_tokens: req.max_tokens,
            temperature: req.temperature,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: system_prompt,
                },
                ChatMessage {
                    role: "user",
                    content: req.user_content,
                },
            ],
            response_format,
        };

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let resp = self
            .client
            .post(&url)
            .header("authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("failed to send request to OpenAI-compatible API")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable>".to_string());
            bail!("OpenAI API returned {status}: {body}");
        }

        let response: ChatResponse = resp
            .json()
            .await
            .context("failed to parse OpenAI API response")?;

        let text = response
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .context("OpenAI response contained no message content")?;

        parse_verdict(&text)
    }
}

/// Parse a verdict from model output text, stripping any markdown
/// fencing or surrounding whitespace.
fn parse_verdict(text: &str) -> anyhow::Result<SkillVerdict> {
    let trimmed = text.trim();
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
