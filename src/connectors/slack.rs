use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::config::SlackConfig;
use crate::error::{KleviathanError, KleviathanResult};

use std::future::Future;
use std::pin::Pin;

use super::registry::{ActionDescriptor, Connector, required_str};

const SLACK_POST_MESSAGE_URL: &str = "https://slack.com/api/chat.postMessage";
const SLACK_LOOKUP_USER_BY_EMAIL_URL: &str = "https://slack.com/api/users.lookupByEmail";
const SLACK_AUTH_TEST_URL: &str = "https://slack.com/api/auth.test";
const SLACK_SET_PRESENCE_URL: &str = "https://slack.com/api/users.setPresence";
const SLACK_USERS_LIST_URL: &str = "https://slack.com/api/users.list";

const REQUIRED_SCOPES: [&str; 14] = [
    "channels:history",
    "channels:join",
    "channels:read",
    "chat:write",
    "groups:history",
    "groups:read",
    "im:history",
    "im:read",
    "im:write",
    "incoming-webhook",
    "search:read.users",
    "users:read",
    "users:read.email",
    "users:write",
];

#[derive(Serialize)]
struct SlackPostMessage<'a> {
    channel: &'a str,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_ts: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    blocks: Option<&'a serde_json::Value>,
}

#[derive(Deserialize)]
struct SlackResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    ts: Option<String>,
}

#[derive(Deserialize)]
struct SlackResponseMetadata {
    #[serde(default)]
    next_cursor: String,
}

#[derive(Deserialize)]
struct SlackUserInfo {
    id: String,
    #[serde(default)]
    profile: Option<SlackProfileDetails>,
}

#[derive(Deserialize)]
struct SlackProfileDetails {
    #[serde(default)]
    email: Option<String>,
}

#[derive(Deserialize)]
struct SlackUsersListResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    members: Vec<SlackUserInfo>,
    #[serde(default)]
    response_metadata: Option<SlackResponseMetadata>,
}

pub struct SlackConnector {
    client: reqwest::Client,
    bot_token: String,
}

pub async fn verify_scopes(config: &SlackConfig) -> KleviathanResult<()> {
    let client = reqwest::Client::new();
    let response = client
        .post(SLACK_AUTH_TEST_URL)
        .bearer_auth(&config.bot_token)
        .send()
        .await
        .map_err(|e| KleviathanError::Slack(format!("auth.test failed: {}", e)))?;

    let status = response.status();
    let scopes_header = response
        .headers()
        .get("x-oauth-scopes")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    if !status.is_success() {
        return Err(KleviathanError::Slack(format!(
            "auth.test returned status {}",
            status
        )));
    }

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| KleviathanError::Slack(format!("auth.test parse failed: {}", e)))?;

    if body["ok"].as_bool() != Some(true) {
        let error = body["error"].as_str().unwrap_or("unknown error");
        return Err(KleviathanError::Slack(format!(
            "auth.test error: {}",
            error
        )));
    }

    let scopes_header = scopes_header.ok_or_else(|| {
        KleviathanError::Slack("Missing x-oauth-scopes header in auth.test response".into())
    })?;

    let actual: BTreeSet<&str> = scopes_header.split(',').map(|s| s.trim()).collect();
    let required: BTreeSet<&str> = REQUIRED_SCOPES.iter().copied().collect();

    let missing: Vec<&&str> = required.difference(&actual).collect();
    let extra: Vec<&&str> = actual.difference(&required).collect();

    if !missing.is_empty() || !extra.is_empty() {
        let mut msg = String::from("Slack token scope mismatch.");
        if !missing.is_empty() {
            msg.push_str(&format!(
                " Missing: {}",
                missing
                    .iter()
                    .map(|s| **s)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !extra.is_empty() {
            msg.push_str(&format!(
                " Extra: {}",
                extra
                    .iter()
                    .map(|s| **s)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        return Err(KleviathanError::Slack(msg));
    }

    Ok(())
}

pub async fn set_presence(config: &SlackConfig) -> KleviathanResult<()> {
    let client = reqwest::Client::new();
    let response = client
        .post(SLACK_SET_PRESENCE_URL)
        .bearer_auth(&config.bot_token)
        .json(&serde_json::json!({"presence": "auto"}))
        .send()
        .await
        .map_err(|e| KleviathanError::Slack(format!("users.setPresence failed: {}", e)))?;

    let status = response.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry_after = response
            .headers()
            .get("Retry-After")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown");
        return Err(KleviathanError::RateLimit(format!(
            "Slack rate limit exceeded. Retry after {retry_after} seconds"
        )));
    }

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| KleviathanError::Slack(format!("users.setPresence parse failed: {}", e)))?;

    if body["ok"].as_bool() != Some(true) {
        let error = body["error"].as_str().unwrap_or("unknown error");
        return Err(KleviathanError::Slack(format!(
            "users.setPresence error: {}",
            error
        )));
    }

    Ok(())
}

impl SlackConnector {
    pub fn new(config: &SlackConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            bot_token: config.bot_token.clone(),
        }
    }

    pub async fn lookup_user_by_email(&self, email: &str) -> KleviathanResult<String> {
        let response = self
            .client
            .get(SLACK_LOOKUP_USER_BY_EMAIL_URL)
            .bearer_auth(&self.bot_token)
            .query(&[("email", email)])
            .send()
            .await
            .map_err(|e| KleviathanError::Slack(e.to_string()))?;

        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown");
            return Err(KleviathanError::RateLimit(format!(
                "Slack rate limit exceeded. Retry after {retry_after} seconds"
            )));
        }

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| KleviathanError::Slack(e.to_string()))?;

        if body["ok"].as_bool() == Some(true) {
            return body["user"]["id"]
                .as_str()
                .map(String::from)
                .ok_or_else(|| KleviathanError::Slack("Missing user ID in response".into()));
        }

        let error = body["error"]
            .as_str()
            .unwrap_or("unknown error");

        if error != "users_not_found" {
            return Err(KleviathanError::Slack(format!(
                "Slack API error: {error}"
            )));
        }

        tracing::info!(
            email = email,
            "users.lookupByEmail returned users_not_found, falling back to users.list search"
        );

        match self.search_users_list(email).await? {
            Some(user_id) => Ok(user_id),
            None => Err(KleviathanError::Slack(format!(
                "User with email {email} not found via lookupByEmail or users.list"
            ))),
        }
    }

    async fn search_users_list(&self, email: &str) -> KleviathanResult<Option<String>> {
        let mut cursor = String::new();
        let max_pages = 10;

        for _ in 0..max_pages {
            let mut query = vec![("limit", "200")];
            if !cursor.is_empty() {
                query.push(("cursor", &cursor));
            }

            let response = self
                .client
                .get(SLACK_USERS_LIST_URL)
                .bearer_auth(&self.bot_token)
                .query(&query)
                .send()
                .await
                .map_err(|e| KleviathanError::Slack(e.to_string()))?;

            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get("Retry-After")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("unknown");
                return Err(KleviathanError::RateLimit(format!(
                    "Slack rate limit exceeded. Retry after {retry_after} seconds"
                )));
            }

            let body: SlackUsersListResponse = response
                .json()
                .await
                .map_err(|e| KleviathanError::Slack(e.to_string()))?;

            if !body.ok {
                let error = body.error.unwrap_or_else(|| "unknown error".into());
                return Err(KleviathanError::Slack(format!(
                    "users.list error: {error}"
                )));
            }

            for member in &body.members {
                let matches = member
                    .profile
                    .as_ref()
                    .and_then(|p| p.email.as_deref())
                    .is_some_and(|e| e.eq_ignore_ascii_case(email));
                if matches {
                    return Ok(Some(member.id.clone()));
                }
            }

            let next = body
                .response_metadata
                .map(|m| m.next_cursor)
                .unwrap_or_default();
            if next.is_empty() {
                break;
            }
            cursor = next;
        }

        Ok(None)
    }

    pub async fn send_message(&self, channel: &str, text: &str) -> KleviathanResult<String> {
        let message = SlackPostMessage {
            channel,
            text,
            thread_ts: None,
            blocks: None,
        };

        let response = self
            .client
            .post(SLACK_POST_MESSAGE_URL)
            .bearer_auth(&self.bot_token)
            .json(&message)
            .send()
            .await
            .map_err(|e| KleviathanError::Slack(e.to_string()))?;

        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown");
            return Err(KleviathanError::RateLimit(format!(
                "Slack rate limit exceeded. Retry after {retry_after} seconds"
            )));
        }

        let slack_response: SlackResponse = response
            .json()
            .await
            .map_err(|e| KleviathanError::Slack(e.to_string()))?;

        if !slack_response.ok {
            let error = slack_response
                .error
                .unwrap_or_else(|| "unknown error".into());
            return Err(KleviathanError::Slack(format!(
                "Slack API error: {error}"
            )));
        }

        Ok(slack_response.ts.unwrap_or_default())
    }
}

pub struct SlackConnectorProvider {
    config: crate::config::SlackConfig,
}

impl SlackConnectorProvider {
    pub fn new(config: crate::config::SlackConfig) -> Self {
        Self { config }
    }
}

impl Connector for SlackConnectorProvider {
    fn tool_name(&self) -> &str {
        "slack"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor {
                name: "lookup_user_by_email",
                description: "Resolve an email address to a Slack user ID",
                parameter_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "email": { "type": "string" }
                    },
                    "required": ["email"],
                    "additionalProperties": false
                }),
                constraint_note: "",
            },
            ActionDescriptor {
                name: "send_message",
                description: "Send a message to a channel or user ID",
                parameter_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "channel": { "type": "string" },
                        "text": { "type": "string" }
                    },
                    "required": ["channel", "text"],
                    "additionalProperties": false
                }),
                constraint_note: "",
            },
        ]
    }

    fn execute<'a>(
        &'a self,
        action: &'a str,
        params: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = KleviathanResult<serde_json::Value>> + Send + 'a>> {
        Box::pin(async move {
            let connector = SlackConnector::new(&self.config);
            match action {
                "lookup_user_by_email" => {
                    let email = required_str(&params, "email")?;
                    let user_id = connector.lookup_user_by_email(&email).await?;
                    Ok(serde_json::json!({ "user_id": user_id }))
                }
                "send_message" => {
                    let channel = required_str(&params, "channel")?;
                    let text = required_str(&params, "text")?;
                    let ts = connector.send_message(&channel, &text).await?;
                    Ok(serde_json::json!({ "channel": channel, "timestamp": ts }))
                }
                other => Err(KleviathanError::TaskGraph(format!(
                    "Unknown slack action: {}",
                    other
                ))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_message_serializes_required_fields() {
        let message = SlackPostMessage {
            channel: "C024BE91L",
            text: "Hello, world!",
            thread_ts: None,
            blocks: None,
        };

        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["channel"], "C024BE91L");
        assert_eq!(json["text"], "Hello, world!");
        assert!(json.get("thread_ts").is_none());
        assert!(json.get("blocks").is_none());
    }

    #[test]
    fn post_message_serializes_optional_fields() {
        let blocks = serde_json::json!([{"type": "section", "text": {"type": "mrkdwn", "text": "Hello"}}]);
        let message = SlackPostMessage {
            channel: "C024BE91L",
            text: "fallback text",
            thread_ts: Some("1503435956.000247"),
            blocks: Some(&blocks),
        };

        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["channel"], "C024BE91L");
        assert_eq!(json["text"], "fallback text");
        assert_eq!(json["thread_ts"], "1503435956.000247");
        assert!(json["blocks"].is_array());
    }

    #[test]
    fn slack_response_deserializes_success() {
        let json = serde_json::json!({
            "ok": true,
            "channel": "C024BE91L",
            "ts": "1503435956.000247",
            "message": {
                "text": "Hello, world!",
                "type": "message",
                "ts": "1503435956.000247"
            }
        });

        let response: SlackResponse = serde_json::from_value(json).unwrap();
        assert!(response.ok);
        assert_eq!(response.ts.as_deref(), Some("1503435956.000247"));
        assert!(response.error.is_none());
    }

    #[test]
    fn slack_response_deserializes_error() {
        let json = serde_json::json!({
            "ok": false,
            "error": "channel_not_found"
        });

        let response: SlackResponse = serde_json::from_value(json).unwrap();
        assert!(!response.ok);
        assert_eq!(response.error.as_deref(), Some("channel_not_found"));
        assert!(response.ts.is_none());
    }

    #[test]
    fn module_contains_only_allowed_urls() {
        let source = include_str!("slack.rs");
        let non_test = source.split("#[cfg(test)]").next().unwrap_or(source);

        assert!(
            non_test.contains("chat.postMessage"),
            "Slack connector must contain chat.postMessage URL"
        );
        assert!(
            non_test.contains("users.lookupByEmail"),
            "Slack connector must contain users.lookupByEmail URL"
        );
        assert!(
            non_test.contains("auth.test"),
            "Slack connector must contain auth.test URL"
        );
        assert!(
            non_test.contains("users.setPresence"),
            "Slack connector must contain users.setPresence URL"
        );
        assert!(
            non_test.contains("users.list"),
            "Slack connector must contain users.list URL"
        );

        let slack_api_urls = [
            "conversations.history",
            "conversations.replies",
            "channels.history",
            "im.history",
            "chat.update",
            "chat.delete",
        ];
        for url in &slack_api_urls {
            assert!(
                !non_test.contains(url),
                "Slack connector must not contain {url}"
            );
        }
    }

    #[test]
    fn verify_scopes_rejects_extra_scopes() {
        let actual: BTreeSet<&str> = REQUIRED_SCOPES
            .iter()
            .copied()
            .chain(std::iter::once("admin.conversations:write"))
            .collect();
        let required: BTreeSet<&str> = REQUIRED_SCOPES.iter().copied().collect();

        let extra: Vec<&&str> = actual.difference(&required).collect();
        assert_eq!(extra.len(), 1);
        assert_eq!(extra[0], &"admin.conversations:write");
    }

    #[test]
    fn verify_scopes_rejects_missing_scopes() {
        let actual: BTreeSet<&str> = REQUIRED_SCOPES
            .iter()
            .copied()
            .filter(|s| *s != "chat:write")
            .collect();
        let required: BTreeSet<&str> = REQUIRED_SCOPES.iter().copied().collect();

        let missing: Vec<&&str> = required.difference(&actual).collect();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0], &"chat:write");
    }

    #[test]
    fn required_scopes_has_exactly_14_entries() {
        assert_eq!(REQUIRED_SCOPES.len(), 14);
        let unique: BTreeSet<&str> = REQUIRED_SCOPES.iter().copied().collect();
        assert_eq!(unique.len(), 14, "REQUIRED_SCOPES must not contain duplicates");
    }

    #[test]
    fn slack_users_list_response_deserializes() {
        let json = serde_json::json!({
            "ok": true,
            "members": [
                {
                    "id": "U001",
                    "profile": {
                        "email": "user@example.com"
                    }
                },
                {
                    "id": "U002",
                    "profile": {}
                }
            ],
            "response_metadata": {
                "next_cursor": "dXNlcjpVMDYxTkZUVDI="
            }
        });

        let response: SlackUsersListResponse = serde_json::from_value(json).unwrap();
        assert!(response.ok);
        assert_eq!(response.members.len(), 2);
        assert_eq!(response.members[0].id, "U001");
        assert_eq!(
            response.members[0].profile.as_ref().unwrap().email.as_deref(),
            Some("user@example.com")
        );
        assert_eq!(response.members[1].id, "U002");
        assert!(response.members[1].profile.as_ref().unwrap().email.is_none());
        assert_eq!(
            response.response_metadata.unwrap().next_cursor,
            "dXNlcjpVMDYxTkZUVDI="
        );
    }

    #[test]
    fn slack_users_list_response_deserializes_without_cursor() {
        let json = serde_json::json!({
            "ok": true,
            "members": [
                {
                    "id": "U001",
                    "profile": {
                        "email": "user@example.com"
                    }
                }
            ]
        });

        let response: SlackUsersListResponse = serde_json::from_value(json).unwrap();
        assert!(response.ok);
        assert_eq!(response.members.len(), 1);
        assert!(response.response_metadata.is_none());
    }

    #[test]
    fn slack_users_list_response_deserializes_error() {
        let json = serde_json::json!({
            "ok": false,
            "error": "invalid_auth"
        });

        let response: SlackUsersListResponse = serde_json::from_value(json).unwrap();
        assert!(!response.ok);
        assert_eq!(response.error.as_deref(), Some("invalid_auth"));
        assert!(response.members.is_empty());
    }

    #[test]
    fn slack_provider_declares_expected_actions() {
        use crate::connectors::registry::Connector;
        let provider = SlackConnectorProvider::new(crate::config::SlackConfig {
            bot_token: "test".into(),
        });
        assert_eq!(provider.tool_name(), "slack");
        assert!(provider.tool_aliases().is_empty());
        let action_names: Vec<&str> = provider.actions().iter().map(|a| a.name).collect();
        assert_eq!(action_names, vec!["lookup_user_by_email", "send_message"]);
    }
}
