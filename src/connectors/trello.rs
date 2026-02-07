use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::TrelloConfig;
use crate::error::{KleviathanError, KleviathanResult};

use std::future::Future;
use std::pin::Pin;

use super::registry::{ActionDescriptor, Connector, required_str};

const BASE_URL: &str = "https://api.trello.com/1";

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrelloCard {
    pub id: String,
    pub name: String,
    pub desc: String,
    pub closed: bool,
    pub date_last_activity: DateTime<Utc>,
    pub id_board: String,
    pub id_list: String,
    pub short_url: String,
    pub url: String,
    pub labels: Option<Vec<TrelloLabel>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TrelloLabel {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCardRequest {
    pub id_list: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pos: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id_labels: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id_members: Option<String>,
}

#[derive(Deserialize)]
struct SearchResponse {
    cards: Vec<TrelloCard>,
}

pub struct TrelloConnector {
    client: reqwest::Client,
    api_key: String,
    token: String,
}

impl TrelloConnector {
    pub fn new(config: &TrelloConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: config.api_key.clone(),
            token: config.token.clone(),
        }
    }

    fn auth_params(&self) -> [(&str, &str); 2] {
        [("key", &self.api_key), ("token", &self.token)]
    }

    pub async fn create_card(&self, request: &CreateCardRequest) -> KleviathanResult<TrelloCard> {
        let response = self
            .client
            .post(format!("{BASE_URL}/cards"))
            .query(&self.auth_params())
            .json(request)
            .send()
            .await?;
        Self::handle_response(response).await
    }

    pub async fn get_card(&self, card_id: &str) -> KleviathanResult<TrelloCard> {
        let response = self
            .client
            .get(format!("{BASE_URL}/cards/{card_id}"))
            .query(&self.auth_params())
            .query(&[(
                "fields",
                "id,name,desc,closed,dateLastActivity,idBoard,idList,labels,shortUrl,url",
            )])
            .send()
            .await?;
        Self::handle_response(response).await
    }

    pub async fn search_cards_by_days(
        &self,
        board_id: &str,
        days: u32,
    ) -> KleviathanResult<Vec<TrelloCard>> {
        let query = format!("edited:{days}");
        let response = self
            .client
            .get(format!("{BASE_URL}/search"))
            .query(&self.auth_params())
            .query(&[
                ("query", query.as_str()),
                ("idBoards", board_id),
                ("modelTypes", "cards"),
                ("cards_limit", "1000"),
                (
                    "card_fields",
                    "id,name,desc,closed,dateLastActivity,idBoard,idList,labels,shortUrl,url",
                ),
            ])
            .send()
            .await?;
        let search: SearchResponse = Self::handle_response(response).await?;
        Ok(search.cards)
    }

    async fn handle_response<T: serde::de::DeserializeOwned>(
        response: reqwest::Response,
    ) -> KleviathanResult<T> {
        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(KleviathanError::RateLimit(
                "Trello API rate limit exceeded".into(),
            ));
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(KleviathanError::Trello(format!("HTTP {status}: {body}")));
        }
        response
            .json()
            .await
            .map_err(|e| KleviathanError::Trello(e.to_string()))
    }
}

pub struct TrelloConnectorProvider {
    config: crate::config::TrelloConfig,
}

impl TrelloConnectorProvider {
    pub fn new(config: crate::config::TrelloConfig) -> Self {
        Self { config }
    }
}

impl Connector for TrelloConnectorProvider {
    fn tool_name(&self) -> &str {
        "trello"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor {
                name: "create_card",
                description: "Create a new Trello card in a specified list",
                parameter_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "list_id": { "type": "string" },
                        "name": { "type": "string" },
                        "description": { "type": "string" }
                    },
                    "required": ["list_id", "name"],
                    "additionalProperties": false
                }),
                constraint_note: "",
            },
            ActionDescriptor {
                name: "search_cards",
                description: "Search for cards edited within a given number of days on a board",
                parameter_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "board_id": { "type": "string" },
                        "days": { "type": "number" }
                    },
                    "required": ["board_id", "days"],
                    "additionalProperties": false
                }),
                constraint_note: "",
            },
            ActionDescriptor {
                name: "get_card",
                description: "Get details of a specific Trello card by ID",
                parameter_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "card_id": { "type": "string" }
                    },
                    "required": ["card_id"],
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
            let connector = TrelloConnector::new(&self.config);
            match action {
                "create_card" => {
                    let request = CreateCardRequest {
                        id_list: required_str(&params, "list_id")?,
                        name: Some(required_str(&params, "name")?),
                        desc: params["description"].as_str().map(String::from),
                        pos: None,
                        due: None,
                        id_labels: None,
                        id_members: None,
                    };
                    let card = connector.create_card(&request).await?;
                    Ok(serde_json::to_value(card)?)
                }
                "search_cards" => {
                    let board_id = required_str(&params, "board_id")?;
                    let days = params["days"].as_u64().unwrap_or(1) as u32;
                    let cards = connector.search_cards_by_days(&board_id, days).await?;
                    let values = cards
                        .into_iter()
                        .map(serde_json::to_value)
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(serde_json::Value::Array(values))
                }
                "get_card" => {
                    let card_id = required_str(&params, "card_id")?;
                    let card = connector.get_card(&card_id).await?;
                    Ok(serde_json::to_value(card)?)
                }
                other => Err(KleviathanError::TaskGraph(format!(
                    "Unknown trello action: {}",
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
    fn create_card_request_serializes_camel_case() {
        let request = CreateCardRequest {
            id_list: "list123".into(),
            name: Some("Test Card".into()),
            desc: Some("A description".into()),
            pos: None,
            due: None,
            id_labels: Some("label1,label2".into()),
            id_members: None,
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["idList"], "list123");
        assert_eq!(json["name"], "Test Card");
        assert_eq!(json["desc"], "A description");
        assert_eq!(json["idLabels"], "label1,label2");
        assert!(json.get("pos").is_none());
        assert!(json.get("due").is_none());
        assert!(json.get("idMembers").is_none());
    }

    #[test]
    fn trello_card_deserializes_from_api_response() {
        let json = serde_json::json!({
            "id": "card123",
            "name": "My Card",
            "desc": "Card description",
            "closed": false,
            "dateLastActivity": "2026-02-20T10:30:00.000Z",
            "idBoard": "board456",
            "idList": "list789",
            "shortUrl": "https://trello.com/c/abc123",
            "url": "https://trello.com/c/abc123/1-my-card",
            "labels": [
                {
                    "id": "label1",
                    "name": "Urgent",
                    "color": "red"
                }
            ]
        });

        let card: TrelloCard = serde_json::from_value(json).unwrap();
        assert_eq!(card.id, "card123");
        assert_eq!(card.name, "My Card");
        assert_eq!(card.desc, "Card description");
        assert!(!card.closed);
        assert_eq!(card.id_board, "board456");
        assert_eq!(card.id_list, "list789");
        assert_eq!(card.short_url, "https://trello.com/c/abc123");
        let labels = card.labels.unwrap();
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].name, "Urgent");
        assert_eq!(labels[0].color.as_deref(), Some("red"));
    }

    #[test]
    fn trello_card_deserializes_without_labels() {
        let json = serde_json::json!({
            "id": "card123",
            "name": "My Card",
            "desc": "",
            "closed": true,
            "dateLastActivity": "2026-01-15T08:00:00.000Z",
            "idBoard": "board456",
            "idList": "list789",
            "shortUrl": "https://trello.com/c/xyz",
            "url": "https://trello.com/c/xyz/2-my-card",
            "labels": null
        });

        let card: TrelloCard = serde_json::from_value(json).unwrap();
        assert!(card.closed);
        assert!(card.labels.is_none());
    }

    #[test]
    fn module_contains_no_delete_calls() {
        let source = include_str!("trello.rs");
        let needle = [".del", "ete("].concat();
        let non_test = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            !non_test.contains(&needle),
            "Trello connector must not contain any HTTP DELETE calls"
        );
    }

    #[test]
    fn trello_provider_declares_expected_actions() {
        use crate::connectors::registry::Connector;
        let provider = TrelloConnectorProvider::new(crate::config::TrelloConfig {
            api_key: "test".into(),
            token: "test".into(),
        });
        assert_eq!(provider.tool_name(), "trello");
        assert!(provider.tool_aliases().is_empty());
        let action_names: Vec<&str> = provider.actions().iter().map(|a| a.name).collect();
        assert_eq!(action_names, vec!["create_card", "search_cards", "get_card"]);
    }
}
