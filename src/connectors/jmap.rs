use jmap_client::client::Client as JmapClient;

use crate::config::JmapConfig;
use crate::error::{KleviathanError, KleviathanResult};

use std::future::Future;
use std::pin::Pin;

use super::registry::{ActionDescriptor, Connector, required_str};

const JMAP_API_URL: &str = "https://api.fastmail.com/jmap/session";
const SUBMISSION_CAPABILITY: &str = "urn:ietf:params:jmap:submission";
const MAX_SEARCH_DAYS: i64 = 365;

pub struct JmapConnector {
    client: JmapClient,
}

#[derive(Debug)]
pub struct EmailSummary {
    pub id: String,
    pub subject: String,
    pub from: String,
    pub to: String,
    pub received_at: String,
    pub preview: String,
    pub body_text: String,
}

impl JmapConnector {
    pub async fn new(config: &JmapConfig) -> KleviathanResult<Self> {
        use jmap_client::client::{Client, Credentials};

        let client = Client::new()
            .credentials(Credentials::bearer(&config.api_key))
            .follow_redirects(["api.fastmail.com", "jmap.fastmail.com"])
            .connect(JMAP_API_URL)
            .await
            .map_err(|e| KleviathanError::Jmap(format!("Failed to connect: {}", e)))?;

        Self::verify_read_only(&client)?;

        Ok(Self { client })
    }

    fn verify_read_only(client: &JmapClient) -> KleviathanResult<()> {
        let session = client.session();
        let account_id = client.default_account_id();

        if let Some(account) = session.account(account_id)
            && account.capability(SUBMISSION_CAPABILITY).is_some()
        {
            return Err(KleviathanError::Jmap(
                "API key has email submission (send) capability. Use a read-only API key.".into(),
            ));
        }

        if session.submission_capabilities().is_some() {
            return Err(KleviathanError::Jmap(
                "Session reports submission capabilities available. Use a read-only API key."
                    .into(),
            ));
        }

        Ok(())
    }

    pub async fn search_emails(
        &self,
        sender: Option<&str>,
        start_days_ago: u32,
        end_days_ago: Option<u32>,
    ) -> KleviathanResult<Vec<String>> {
        use chrono::{Duration, Utc};
        use jmap_client::{core::query::Filter as CoreFilter, email};

        let now = Utc::now();
        let start = now - Duration::days(start_days_ago as i64);
        let end = match end_days_ago {
            Some(d) => now - Duration::days(d as i64),
            None => now,
        };

        let range_days = (end - start).num_days();
        if range_days > MAX_SEARCH_DAYS {
            return Err(KleviathanError::Jmap(format!(
                "Search range {} days exceeds maximum {} days",
                range_days, MAX_SEARCH_DAYS
            )));
        }

        let start_ts = start.timestamp();
        let end_ts = end.timestamp();

        let mut conditions: Vec<email::query::Filter> = Vec::new();
        if let Some(s) = sender {
            conditions.push(email::query::Filter::from(s));
        }
        conditions.push(email::query::Filter::after(start_ts));
        conditions.push(email::query::Filter::before(end_ts));

        let filter = CoreFilter::and(conditions);
        let sort = [email::query::Comparator::received_at().ascending()];

        let mut response = self
            .client
            .email_query(Some(filter), Some(sort))
            .await
            .map_err(|e| KleviathanError::Jmap(e.to_string()))?;

        Ok(response.take_ids())
    }

    pub async fn get_email(&self, email_id: &str) -> KleviathanResult<Option<EmailSummary>> {
        use jmap_client::email::Property;

        let properties = [
            Property::Id,
            Property::Subject,
            Property::From,
            Property::To,
            Property::ReceivedAt,
            Property::Preview,
            Property::TextBody,
            Property::BodyValues,
        ];

        let email = self
            .client
            .email_get(email_id, Some(properties))
            .await
            .map_err(|e| KleviathanError::Jmap(e.to_string()))?;

        let email = match email {
            Some(e) => e,
            None => return Ok(None),
        };

        let subject = email.subject().unwrap_or("").to_string();

        let from = email
            .from()
            .map(|addrs| {
                addrs
                    .iter()
                    .map(|a| a.email())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();

        let to = email
            .to()
            .map(|addrs| {
                addrs
                    .iter()
                    .map(|a| a.email())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();

        let preview = email.preview().unwrap_or("").to_string();

        let mut body_text = String::new();
        if let Some(text_parts) = email.text_body() {
            for part in text_parts {
                if let Some(part_id) = part.part_id()
                    && let Some(value) = email.body_value(part_id)
                {
                    body_text.push_str(value.value());
                }
            }
        }

        let received_at = format!("{:?}", email.received_at());

        Ok(Some(EmailSummary {
            id: email_id.to_string(),
            subject,
            from,
            to,
            received_at,
            preview,
            body_text,
        }))
    }
}

pub struct JmapConnectorProvider {
    config: crate::config::JmapConfig,
}

impl JmapConnectorProvider {
    pub fn new(config: crate::config::JmapConfig) -> Self {
        Self { config }
    }
}

impl Connector for JmapConnectorProvider {
    fn tool_name(&self) -> &str {
        "fm_jmap"
    }

    fn tool_aliases(&self) -> &[&str] {
        &["email", "jmap"]
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor {
                name: "search_emails",
                description: "Search emails by sender and date range",
                parameter_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "sender": { "type": "string" },
                        "days": { "type": "number" },
                        "end_days_ago": { "type": "number" }
                    },
                    "required": ["days"],
                    "additionalProperties": false
                }),
                constraint_note: " The search range (days minus end_days_ago, or days if end_days_ago is omitted) must not exceed 365 days. The sender field is optional; omit it to search all emails regardless of sender.",
            },
            ActionDescriptor {
                name: "get_email",
                description: "Get details of a specific email by ID",
                parameter_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "email_id": { "type": "string" }
                    },
                    "required": ["email_id"],
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
            let connector = JmapConnector::new(&self.config).await?;
            match action {
                "search_emails" => {
                    let sender = params["sender"].as_str().map(String::from);
                    let start_days_ago = params["days"].as_u64().unwrap_or(7) as u32;
                    let end_days_ago = params["end_days_ago"].as_u64().map(|v| v as u32);
                    let ids = connector
                        .search_emails(sender.as_deref(), start_days_ago, end_days_ago)
                        .await?;
                    Ok(serde_json::to_value(ids)?)
                }
                "get_email" => {
                    let email_id = required_str(&params, "email_id")?;
                    let summary = connector.get_email(&email_id).await?;
                    let value = match summary {
                        Some(s) => serde_json::json!({
                            "id": s.id,
                            "subject": s.subject,
                            "from": s.from,
                            "to": s.to,
                            "received_at": s.received_at,
                            "preview": s.preview,
                            "body_text": s.body_text,
                        }),
                        None => serde_json::Value::Null,
                    };
                    Ok(value)
                }
                other => Err(KleviathanError::TaskGraph(format!(
                    "Unknown fm_jmap action: {}",
                    other
                ))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::registry::Connector;

    #[test]
    fn jmap_provider_declares_expected_actions() {
        let provider = JmapConnectorProvider::new(crate::config::JmapConfig {
            api_key: "test".into(),
        });
        assert_eq!(provider.tool_name(), "fm_jmap");
        assert_eq!(provider.tool_aliases(), &["email", "jmap"]);
        let action_names: Vec<&str> = provider.actions().iter().map(|a| a.name).collect();
        assert_eq!(action_names, vec!["search_emails", "get_email"]);
    }
}
