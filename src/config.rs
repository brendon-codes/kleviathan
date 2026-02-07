use serde::Deserialize;
use std::path::PathBuf;

use crate::error::{KleviathanError, KleviathanResult};

fn jsonc_value_to_serde(value: jsonc_parser::JsonValue) -> serde_json::Value {
    match value {
        jsonc_parser::JsonValue::String(s) => serde_json::Value::String(s.into_owned()),
        jsonc_parser::JsonValue::Number(n) => {
            serde_json::from_str(n).unwrap_or(serde_json::Value::Null)
        }
        jsonc_parser::JsonValue::Boolean(b) => serde_json::Value::Bool(b),
        jsonc_parser::JsonValue::Null => serde_json::Value::Null,
        jsonc_parser::JsonValue::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(jsonc_value_to_serde).collect())
        }
        jsonc_parser::JsonValue::Object(obj) => {
            let map = obj
                .take_inner()
                .into_iter()
                .map(|(k, v)| (k, jsonc_value_to_serde(v)))
                .collect();
            serde_json::Value::Object(map)
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub matrix: MatrixConfig,
    pub trello: TrelloConfig,
    pub fm_jmap: JmapConfig,
    pub fm_caldav: FmCalDavConfig,
    pub fm_carddav: FmCardDavConfig,
    pub slack: SlackConfig,
    pub llm: LlmConfig,
}

#[derive(Debug, Deserialize)]
pub struct MatrixConfig {
    pub homeserver_url: String,
    pub username: String,
    pub password: String,
    pub allowed_sender: String,
    pub store_passphrase: String,
    pub enable_matrix_logs: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrelloConfig {
    pub api_key: String,
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JmapConfig {
    pub api_key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SlackConfig {
    pub bot_token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FmCalDavConfig {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FmCardDavConfig {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct LlmConfig {
    pub model: LlmModel,
    pub api_keys: ApiKeys,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmModel {
    AnthropicSonnet46,
    AnthropicOpus46,
    AnthropicHaiku45,
    OpenAiGpt52,
}

impl LlmModel {
    pub fn provider(&self) -> LlmProviderKind {
        match self {
            LlmModel::AnthropicSonnet46
            | LlmModel::AnthropicOpus46
            | LlmModel::AnthropicHaiku45 => LlmProviderKind::Anthropic,
            LlmModel::OpenAiGpt52 => LlmProviderKind::OpenAi,
        }
    }

    pub fn api_model_id(&self) -> &str {
        match self {
            LlmModel::AnthropicSonnet46 => "claude-sonnet-4-6",
            LlmModel::AnthropicOpus46 => "claude-opus-4-6",
            LlmModel::AnthropicHaiku45 => "claude-haiku-4-5-20251001",
            LlmModel::OpenAiGpt52 => "gpt-5.2",
        }
    }
}

impl<'de> Deserialize<'de> for LlmModel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "anthropic.sonnet46" => Ok(LlmModel::AnthropicSonnet46),
            "anthropic.opus46" => Ok(LlmModel::AnthropicOpus46),
            "anthropic.haiku45" => Ok(LlmModel::AnthropicHaiku45),
            "openai.gpt52" => Ok(LlmModel::OpenAiGpt52),
            other => Err(serde::de::Error::custom(format!(
                "unknown model: {other}. Expected one of: anthropic.sonnet46, anthropic.opus46, anthropic.haiku45, openai.gpt52"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProviderKind {
    OpenAi,
    Anthropic,
}

#[derive(Debug, Deserialize)]
pub struct ApiKeys {
    pub openai: Option<String>,
    pub anthropic: Option<String>,
}

fn config_dir() -> KleviathanResult<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| KleviathanError::Config("HOME environment variable not set".into()))?;
    Ok(PathBuf::from(home).join(".kleviathan"))
}

fn config_path() -> KleviathanResult<PathBuf> {
    Ok(config_dir()?.join("kleviathan.jsonc"))
}

pub fn load_config() -> KleviathanResult<Config> {
    let path = config_path()?;
    let contents = std::fs::read_to_string(&path).map_err(|e| {
        KleviathanError::Config(format!(
            "Failed to read config at {}: {}. Run `kleviathan make-config` to create one.",
            path.display(),
            e
        ))
    })?;

    let parsed = jsonc_parser::parse_to_value(&contents, &Default::default())
        .map_err(|e| KleviathanError::Config(format!("Failed to parse JSONC: {e}")))?
        .ok_or_else(|| KleviathanError::Config("Config file is empty".into()))?;

    let serde_value = jsonc_value_to_serde(parsed);
    serde_json::from_value(serde_value)
        .map_err(|e| KleviathanError::Config(format!("Failed to deserialize config: {e}")))
}

pub fn make_config() -> KleviathanResult<()> {
    let dir = config_dir()?;
    let path = config_path()?;

    if path.exists() {
        println!("Config file already exists at {}", path.display());
        return Ok(());
    }

    std::fs::create_dir_all(&dir)?;

    let template = include_str!("../tpl/kleviathan.jsonc");

    std::fs::write(&path, template)?;
    println!("Created config file at {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Config, LlmModel, MatrixConfig, jsonc_value_to_serde};

    #[test]
    fn matrix_config_deserializes_with_enable_matrix_logs_enabled() {
        let json = r#"{
            "homeserver_url": "https://matrix.example.org",
            "username": "bot",
            "password": "pass",
            "allowed_sender": "@user:example.org",
            "store_passphrase": "secret",
            "enable_matrix_logs": true
        }"#;

        let config: MatrixConfig = serde_json::from_str(json).unwrap();

        assert_eq!(config.homeserver_url, "https://matrix.example.org");
        assert_eq!(config.allowed_sender, "@user:example.org");
        assert!(config.enable_matrix_logs);
    }

    #[test]
    fn matrix_config_deserializes_with_enable_matrix_logs_disabled() {
        let json = r#"{
            "homeserver_url": "https://matrix.example.org",
            "username": "bot",
            "password": "pass",
            "allowed_sender": "@user:example.org",
            "store_passphrase": "secret",
            "enable_matrix_logs": false
        }"#;

        let config: MatrixConfig = serde_json::from_str(json).unwrap();

        assert!(!config.enable_matrix_logs);
    }

    #[test]
    fn matrix_config_requires_enable_matrix_logs() {
        let json = r#"{
            "homeserver_url": "https://matrix.example.org",
            "username": "bot",
            "password": "pass",
            "allowed_sender": "@user:example.org",
            "store_passphrase": "secret"
        }"#;

        let error = serde_json::from_str::<MatrixConfig>(json).unwrap_err();

        assert!(error.to_string().contains("enable_matrix_logs"));
    }

    #[test]
    fn bundled_template_deserializes_as_config() {
        let template = include_str!("../tpl/kleviathan.jsonc");
        let parsed = jsonc_parser::parse_to_value(template, &Default::default())
            .unwrap()
            .expect("template should not be empty");
        let serde_value = jsonc_value_to_serde(parsed);
        let config: Config = serde_json::from_value(serde_value).unwrap();

        assert_eq!(config.matrix.homeserver_url, "https://matrix.example.org");
        assert_eq!(config.fm_jmap.api_key, "your-jmap-api-key");
        assert_eq!(config.fm_caldav.username, "you@fastmail.com");
        assert_eq!(config.fm_carddav.username, "you@fastmail.com");
        assert_eq!(config.slack.bot_token, "xoxb-your-slack-bot-token");
        assert!(matches!(config.llm.model, LlmModel::AnthropicSonnet46));
        assert_eq!(
            config.llm.api_keys.openai.as_deref(),
            Some("xxxxxxxxxxxxxxxx")
        );
        assert_eq!(
            config.llm.api_keys.anthropic.as_deref(),
            Some("xxxxxxxxxxxxxxxx")
        );
    }
}
