use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use crate::error::{KleviathanError, KleviathanResult};

const API_URL: &str = "https://api.openai.com/v1/chat/completions";
const MAX_TOKENS: u32 = 4096;
const TEMPERATURE: f32 = 0.0;

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: Option<String>,
}

fn build_response_format(schema: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "response",
            "strict": true,
            "schema": schema
        }
    })
}

fn build_request(
    model: &str,
    system_prompt: &str,
    user_message: &str,
    json_schema: Option<&serde_json::Value>,
) -> ChatRequest {
    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: system_prompt.into(),
        },
        ChatMessage {
            role: "user".into(),
            content: user_message.into(),
        },
    ];

    ChatRequest {
        model: model.into(),
        messages,
        temperature: Some(TEMPERATURE),
        max_tokens: Some(MAX_TOKENS),
        response_format: json_schema.map(build_response_format),
    }
}

pub struct OpenAiProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
}

impl OpenAiProvider {
    pub fn new(api_key: &str, model: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.to_string(),
            model: model.to_string(),
        }
    }
}

impl super::LlmProvider for OpenAiProvider {
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
                .bearer_auth(&self.api_key)
                .json(&request_body)
                .send()
                .await
                .map_err(|e| KleviathanError::Llm(format!("OpenAI request failed: {e}")))?;

            let status = response.status();

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                tracing::warn!("OpenAI rate limit hit (HTTP 429)");
                return Err(KleviathanError::RateLimit(
                    "OpenAI rate limit exceeded".into(),
                ));
            }

            if !status.is_success() {
                let error_body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "failed to read error body".into());
                tracing::warn!(status = %status, body = %error_body, "OpenAI API error");
                return Err(KleviathanError::Llm(format!(
                    "OpenAI API error (HTTP {status}): {error_body}"
                )));
            }

            let chat_response: ChatResponse = response
                .json()
                .await
                .map_err(|e| KleviathanError::Llm(format!("Failed to parse OpenAI response: {e}")))?;

            chat_response
                .choices
                .into_iter()
                .next()
                .and_then(|choice| choice.message.content)
                .ok_or_else(|| {
                    KleviathanError::Llm("OpenAI returned no content in response".into())
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serialization_without_schema() {
        let request = build_request("gpt-4o-mini", "You are helpful.", "Hello", None);
        let json = serde_json::to_value(&request).unwrap();

        assert_eq!(json["model"], "gpt-4o-mini");
        assert_eq!(json["messages"][0]["role"], "system");
        assert_eq!(json["messages"][0]["content"], "You are helpful.");
        assert_eq!(json["messages"][1]["role"], "user");
        assert_eq!(json["messages"][1]["content"], "Hello");
        assert_eq!(json["temperature"], 0.0);
        assert_eq!(json["max_tokens"], 4096);
        assert!(json.get("response_format").is_none());
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

        let request = build_request("gpt-4o-mini", "System", "User", Some(&schema));
        let json = serde_json::to_value(&request).unwrap();

        let rf = &json["response_format"];
        assert_eq!(rf["type"], "json_schema");
        assert_eq!(rf["json_schema"]["name"], "response");
        assert_eq!(rf["json_schema"]["strict"], true);
        assert_eq!(rf["json_schema"]["schema"]["type"], "object");
        assert_eq!(
            rf["json_schema"]["schema"]["properties"]["answer"]["type"],
            "string"
        );
    }

    #[test]
    fn response_deserialization() {
        let json = serde_json::json!({
            "id": "chatcmpl-abc123",
            "object": "chat.completion",
            "created": 1709123456,
            "model": "gpt-4o-mini-2024-07-18",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "{\"key\": \"value\"}"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 19,
                "completion_tokens": 10,
                "total_tokens": 29
            }
        });

        let response: ChatResponse = serde_json::from_value(json).unwrap();
        assert_eq!(response.choices.len(), 1);
        assert_eq!(
            response.choices[0].message.content.as_deref(),
            Some("{\"key\": \"value\"}")
        );
    }

    #[test]
    fn provider_uses_given_model() {
        let provider = OpenAiProvider::new("test-key", "gpt-5.2");
        assert_eq!(provider.model, "gpt-5.2");
        assert_eq!(provider.api_key, "test-key");
    }
}
