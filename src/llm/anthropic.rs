use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use crate::error::{KleviathanError, KleviathanResult};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_TOKENS: u32 = 4096;
const TEMPERATURE: f64 = 0.0;

#[derive(Serialize)]
struct MessagesRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<OutputConfig>,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct OutputConfig {
    format: OutputFormat,
}

#[derive(Serialize)]
struct OutputFormat {
    #[serde(rename = "type")]
    format_type: String,
    schema: serde_json::Value,
}

#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct ApiErrorResponse {
    error: ApiErrorDetail,
}

#[derive(Deserialize)]
struct ApiErrorDetail {
    #[serde(rename = "type")]
    error_type: String,
    message: String,
}

fn build_output_config(schema: &serde_json::Value) -> OutputConfig {
    OutputConfig {
        format: OutputFormat {
            format_type: "json_schema".into(),
            schema: schema.clone(),
        },
    }
}

fn build_request(
    model: &str,
    system_prompt: &str,
    user_message: &str,
    json_schema: Option<&serde_json::Value>,
) -> MessagesRequest {
    MessagesRequest {
        model: model.into(),
        max_tokens: MAX_TOKENS,
        messages: vec![AnthropicMessage {
            role: "user".into(),
            content: user_message.into(),
        }],
        system: Some(system_prompt.into()),
        temperature: Some(TEMPERATURE),
        output_config: json_schema.map(build_output_config),
    }
}

pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
}

impl AnthropicProvider {
    pub fn new(api_key: &str, model: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.to_string(),
            model: model.to_string(),
        }
    }
}

impl super::LlmProvider for AnthropicProvider {
    fn chat(
        &self,
        system_prompt: &str,
        user_message: &str,
        json_schema: Option<&serde_json::Value>,
    ) -> Pin<Box<dyn Future<Output = KleviathanResult<String>> + Send + '_>> {
        let request_body = build_request(&self.model, system_prompt, user_message, json_schema);

        Box::pin(async move {
            let response = self
                .client
                .post(API_URL)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("content-type", "application/json")
                .json(&request_body)
                .send()
                .await
                .map_err(|e| KleviathanError::Llm(format!("Anthropic request failed: {e}")))?;

            let status = response.status();

            if let Some(remaining) = response
                .headers()
                .get("anthropic-ratelimit-requests-remaining")
            {
                tracing::debug!(
                    requests_remaining = ?remaining,
                    input_tokens_remaining = ?response.headers().get("anthropic-ratelimit-input-tokens-remaining"),
                    output_tokens_remaining = ?response.headers().get("anthropic-ratelimit-output-tokens-remaining"),
                    "Anthropic rate limit headers"
                );
            }

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("unknown");
                tracing::warn!(retry_after = %retry_after, "Anthropic rate limit hit (HTTP 429)");
                return Err(KleviathanError::RateLimit(
                    "Anthropic rate limit exceeded".into(),
                ));
            }

            if status.as_u16() == 529 {
                tracing::warn!("Anthropic API overloaded (HTTP 529)");
                return Err(KleviathanError::Llm("Anthropic API overloaded".into()));
            }

            if !status.is_success() {
                let error_body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "failed to read error body".into());

                let error_message =
                    if let Ok(api_error) = serde_json::from_str::<ApiErrorResponse>(&error_body) {
                        format!(
                            "Anthropic API error (HTTP {status}): {} - {}",
                            api_error.error.error_type, api_error.error.message
                        )
                    } else {
                        format!("Anthropic API error (HTTP {status}): {error_body}")
                    };

                tracing::warn!(status = %status, "{}", error_message);
                return Err(KleviathanError::Llm(error_message));
            }

            let messages_response: MessagesResponse = response
                .json()
                .await
                .map_err(|e| {
                    KleviathanError::Llm(format!("Failed to parse Anthropic response: {e}"))
                })?;

            messages_response
                .content
                .into_iter()
                .find(|block| block.block_type == "text")
                .and_then(|block| block.text)
                .ok_or_else(|| {
                    KleviathanError::Llm("Anthropic returned no text content in response".into())
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serialization_without_schema() {
        let request = build_request("claude-sonnet-4-6", "You are helpful.", "Hello", None);
        let json = serde_json::to_value(&request).unwrap();

        assert_eq!(json["model"], "claude-sonnet-4-6");
        assert_eq!(json["max_tokens"], 4096);
        assert_eq!(json["system"], "You are helpful.");
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "Hello");
        assert_eq!(json["temperature"], 0.0);
        assert!(json.get("output_config").is_none());
    }

    #[test]
    fn request_serialization_with_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "answer": { "type": "string" }
            },
            "required": ["answer"],
            "additionalProperties": false
        });

        let request = build_request("claude-sonnet-4-6", "System", "User", Some(&schema));
        let json = serde_json::to_value(&request).unwrap();

        let oc = &json["output_config"];
        assert_eq!(oc["format"]["type"], "json_schema");
        assert_eq!(oc["format"]["schema"]["type"], "object");
        assert_eq!(
            oc["format"]["schema"]["properties"]["answer"]["type"],
            "string"
        );
    }

    #[test]
    fn request_has_system_as_top_level_field() {
        let request = build_request("claude-sonnet-4-6", "Be concise.", "Hi", None);
        let json = serde_json::to_value(&request).unwrap();

        assert_eq!(json["system"], "Be concise.");
        assert_eq!(json["messages"].as_array().unwrap().len(), 1);
        assert_eq!(json["messages"][0]["role"], "user");
    }

    #[test]
    fn response_deserialization() {
        let json = serde_json::json!({
            "id": "msg_01XFDUDYJgAACzvnptvVoYEL",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "text",
                "text": "{\"key\": \"value\"}"
            }],
            "model": "claude-sonnet-4-6",
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 42,
                "output_tokens": 150
            }
        });

        let response: MessagesResponse = serde_json::from_value(json).unwrap();
        assert_eq!(response.content.len(), 1);
        assert_eq!(response.content[0].block_type, "text");
        assert_eq!(
            response.content[0].text.as_deref(),
            Some("{\"key\": \"value\"}")
        );
    }

    #[test]
    fn error_response_deserialization() {
        let json = serde_json::json!({
            "type": "error",
            "error": {
                "type": "rate_limit_error",
                "message": "Your account has hit a rate limit."
            }
        });

        let error: ApiErrorResponse = serde_json::from_value(json).unwrap();
        assert_eq!(error.error.error_type, "rate_limit_error");
        assert_eq!(error.error.message, "Your account has hit a rate limit.");
    }

    #[test]
    fn provider_uses_given_model() {
        let provider = AnthropicProvider::new("test-key", "claude-sonnet-4-6");
        assert_eq!(provider.model, "claude-sonnet-4-6");
        assert_eq!(provider.api_key, "test-key");
    }
}
