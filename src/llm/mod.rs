pub mod anthropic;
pub mod openai;

use std::future::Future;
use std::pin::Pin;

use crate::config::LlmProviderKind;
use crate::error::KleviathanResult;

pub trait LlmProvider: Send + Sync {
    fn chat(
        &self,
        system_prompt: &str,
        user_message: &str,
        json_schema: Option<&serde_json::Value>,
    ) -> Pin<Box<dyn Future<Output = KleviathanResult<String>> + Send + '_>>;
}

pub fn create_provider(
    config: &crate::config::LlmConfig,
) -> KleviathanResult<Box<dyn LlmProvider>> {
    let model_id = config.model.api_model_id();

    match config.model.provider() {
        LlmProviderKind::OpenAi => {
            let api_key = config.api_keys.openai.as_deref().ok_or_else(|| {
                crate::error::KleviathanError::Config(
                    "OpenAI API key required for the selected model".into(),
                )
            })?;
            Ok(Box::new(openai::OpenAiProvider::new(api_key, model_id)))
        }
        LlmProviderKind::Anthropic => {
            let api_key = config.api_keys.anthropic.as_deref().ok_or_else(|| {
                crate::error::KleviathanError::Config(
                    "Anthropic API key required for the selected model".into(),
                )
            })?;
            Ok(Box::new(anthropic::AnthropicProvider::new(
                api_key, model_id,
            )))
        }
    }
}
