use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use crate::error::{KleviathanError, KleviathanResult};

pub struct ActionDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub parameter_schema: serde_json::Value,
    pub constraint_note: &'static str,
}

pub trait Connector: Send + Sync {
    fn tool_name(&self) -> &str;
    fn tool_aliases(&self) -> &[&str] {
        &[]
    }
    fn actions(&self) -> Vec<ActionDescriptor>;
    fn execute<'a>(
        &'a self,
        action: &'a str,
        params: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = KleviathanResult<serde_json::Value>> + Send + 'a>>;
}

pub struct ConnectorRegistry {
    connectors: Vec<Box<dyn Connector>>,
    name_to_idx: HashMap<String, usize>,
}

impl ConnectorRegistry {
    pub fn new() -> Self {
        Self {
            connectors: Vec::new(),
            name_to_idx: HashMap::new(),
        }
    }

    pub fn register(&mut self, connector: Box<dyn Connector>) {
        let idx = self.connectors.len();
        self.name_to_idx
            .insert(connector.tool_name().to_string(), idx);
        for alias in connector.tool_aliases() {
            self.name_to_idx.insert(alias.to_string(), idx);
        }
        self.connectors.push(connector);
    }

    pub fn available_tools(&self) -> Vec<&str> {
        self.connectors.iter().map(|c| c.tool_name()).collect()
    }

    pub fn action_selection_schema(&self) -> serde_json::Value {
        let tool_names: Vec<serde_json::Value> = self
            .connectors
            .iter()
            .map(|c| serde_json::Value::String(c.tool_name().to_string()))
            .collect();
        let mut seen = std::collections::HashSet::new();
        let action_names: Vec<serde_json::Value> = self
            .connectors
            .iter()
            .flat_map(|c| c.actions())
            .filter(|a| seen.insert(a.name))
            .map(|a| serde_json::Value::String(a.name.to_string()))
            .collect();

        serde_json::json!({
            "type": "object",
            "properties": {
                "tool": {
                    "type": "string",
                    "enum": tool_names
                },
                "action": {
                    "type": "string",
                    "enum": action_names
                }
            },
            "required": ["tool", "action"],
            "additionalProperties": false
        })
    }

    pub fn tool_action_descriptions(&self) -> String {
        self.connectors
            .iter()
            .map(|c| {
                let actions: Vec<String> = c
                    .actions()
                    .iter()
                    .map(|a| {
                        if a.constraint_note.is_empty() {
                            format!("{} ({})", a.name, a.description)
                        } else {
                            format!("{} ({}.{})", a.name, a.description, a.constraint_note)
                        }
                    })
                    .collect();
                format!("{} has {}", c.tool_name(), actions.join(", "))
            })
            .collect::<Vec<_>>()
            .join(". ")
    }

    pub fn parameter_schema_for(
        &self,
        tool: &str,
        action: &str,
    ) -> KleviathanResult<serde_json::Value> {
        let idx = self
            .name_to_idx
            .get(tool)
            .ok_or_else(|| KleviathanError::TaskGraph(format!("Unknown tool: {}", tool)))?;
        self.connectors[*idx]
            .actions()
            .into_iter()
            .find(|a| a.name == action)
            .map(|a| a.parameter_schema)
            .ok_or_else(|| {
                KleviathanError::TaskGraph(format!(
                    "Unknown tool/action pair: {}/{}",
                    tool, action
                ))
            })
    }

    pub fn constraint_note_for(&self, tool: &str, action: &str) -> &str {
        let idx = match self.name_to_idx.get(tool) {
            Some(i) => *i,
            None => return "",
        };
        self.connectors[idx]
            .actions()
            .into_iter()
            .find(|a| a.name == action)
            .map(|a| a.constraint_note)
            .unwrap_or("")
    }

    pub async fn execute(
        &self,
        tool: &str,
        action: &str,
        params: serde_json::Value,
    ) -> KleviathanResult<serde_json::Value> {
        let idx = self
            .name_to_idx
            .get(tool)
            .ok_or_else(|| KleviathanError::TaskGraph(format!("Unknown tool: {}", tool)))?;
        self.connectors[*idx].execute(action, params).await
    }
}

pub fn required_str(params: &serde_json::Value, field: &str) -> KleviathanResult<String> {
    params[field]
        .as_str()
        .map(String::from)
        .ok_or_else(|| {
            KleviathanError::TaskGraph(format!("Missing required parameter: {}", field))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FmCalDavConfig, FmCardDavConfig, JmapConfig, SlackConfig, TrelloConfig};
    use crate::connectors::{fm_caldav, fm_carddav, jmap, slack, trello};

    fn test_registry() -> ConnectorRegistry {
        let mut registry = ConnectorRegistry::new();
        registry.register(Box::new(trello::TrelloConnectorProvider::new(
            TrelloConfig {
                api_key: "test".into(),
                token: "test".into(),
            },
        )));
        registry.register(Box::new(jmap::JmapConnectorProvider::new(JmapConfig {
            api_key: "test".into(),
        })));
        registry.register(Box::new(slack::SlackConnectorProvider::new(SlackConfig {
            bot_token: "test".into(),
        })));
        registry.register(Box::new(fm_caldav::FmCalDavConnectorProvider::new(
            FmCalDavConfig {
                username: "test".into(),
                password: "test".into(),
            },
        )));
        registry.register(Box::new(fm_carddav::FmCardDavConnectorProvider::new(
            FmCardDavConfig {
                username: "test".into(),
                password: "test".into(),
            },
        )));
        registry
    }

    #[test]
    fn available_tools_returns_primary_names() {
        let registry = test_registry();
        let tools = registry.available_tools();
        assert_eq!(tools, vec!["trello", "fm_jmap", "slack", "fm_caldav", "fm_carddav"]);
    }

    #[test]
    fn alias_resolves_to_connector() {
        let registry = test_registry();
        let email_schema = registry.parameter_schema_for("email", "search_emails");
        assert!(email_schema.is_ok(), "email alias should resolve to fm_jmap connector");
        let jmap_schema = registry.parameter_schema_for("jmap", "search_emails");
        assert!(jmap_schema.is_ok(), "jmap alias should resolve to fm_jmap connector");
        let caldav_schema = registry.parameter_schema_for("caldav", "list_calendars");
        assert!(caldav_schema.is_ok(), "caldav alias should resolve to fm_caldav connector");
        let calendar_schema = registry.parameter_schema_for("calendar", "search_events");
        assert!(calendar_schema.is_ok(), "calendar alias should resolve to fm_caldav connector");
        let carddav_schema = registry.parameter_schema_for("carddav", "list_addressbooks");
        assert!(carddav_schema.is_ok(), "carddav alias should resolve to fm_carddav connector");
        let contacts_schema = registry.parameter_schema_for("contacts", "search_contacts");
        assert!(contacts_schema.is_ok(), "contacts alias should resolve to fm_carddav connector");
    }

    #[test]
    fn unknown_tool_returns_error() {
        let registry = test_registry();
        let result = registry.parameter_schema_for("unknown", "action");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Unknown tool"));
    }

    #[test]
    fn unknown_action_returns_error() {
        let registry = test_registry();
        let result = registry.parameter_schema_for("trello", "nonexistent");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Unknown tool/action pair"));
    }

    #[test]
    fn action_selection_schema_has_all_tools_and_actions() {
        let registry = test_registry();
        let schema = registry.action_selection_schema();

        let tool_enum = schema["properties"]["tool"]["enum"]
            .as_array()
            .expect("tool should have enum");
        let action_enum = schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action should have enum");

        let tools: Vec<&str> = tool_enum.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(tools, vec!["trello", "fm_jmap", "slack", "fm_caldav", "fm_carddav"]);

        let actions: Vec<&str> = action_enum.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(
            actions,
            vec![
                "create_card",
                "search_cards",
                "get_card",
                "search_emails",
                "get_email",
                "lookup_user_by_email",
                "send_message",
                "list_calendars",
                "search_events",
                "add_event",
                "list_addressbooks",
                "search_contacts",
                "add_contact"
            ]
        );
    }

    #[test]
    fn action_selection_schema_has_additional_properties_false() {
        let registry = test_registry();
        let schema = registry.action_selection_schema();
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn all_per_action_schemas_have_additional_properties_false() {
        let registry = test_registry();
        let pairs = [
            ("trello", "create_card"),
            ("trello", "search_cards"),
            ("trello", "get_card"),
            ("fm_jmap", "search_emails"),
            ("fm_jmap", "get_email"),
            ("slack", "lookup_user_by_email"),
            ("slack", "send_message"),
            ("fm_caldav", "list_calendars"),
            ("fm_caldav", "search_events"),
            ("fm_caldav", "add_event"),
            ("fm_carddav", "list_addressbooks"),
            ("fm_carddav", "search_contacts"),
            ("fm_carddav", "add_contact"),
        ];
        for (tool, action) in pairs {
            let schema = registry
                .parameter_schema_for(tool, action)
                .unwrap_or_else(|_| panic!("schema for {}/{} should exist", tool, action));
            assert_eq!(
                schema["additionalProperties"], false,
                "{}/{} must have additionalProperties: false",
                tool, action
            );
        }
    }

    #[test]
    fn per_action_schemas_have_bounded_optionality() {
        let registry = test_registry();
        let pairs = [
            ("trello", "create_card"),
            ("trello", "search_cards"),
            ("trello", "get_card"),
            ("fm_jmap", "search_emails"),
            ("fm_jmap", "get_email"),
            ("slack", "lookup_user_by_email"),
            ("slack", "send_message"),
            ("fm_caldav", "list_calendars"),
            ("fm_caldav", "search_events"),
            ("fm_caldav", "add_event"),
            ("fm_carddav", "list_addressbooks"),
            ("fm_carddav", "search_contacts"),
            ("fm_carddav", "add_contact"),
        ];
        for (tool, action) in pairs {
            let schema = registry.parameter_schema_for(tool, action).unwrap();
            let all_props = schema["properties"]
                .as_object()
                .expect("should have properties");
            let required = schema["required"]
                .as_array()
                .expect("should have required");
            let required_set: std::collections::HashSet<&str> =
                required.iter().map(|v| v.as_str().unwrap()).collect();
            let optional_count = all_props
                .keys()
                .filter(|k| !required_set.contains(k.as_str()))
                .count();
            let combinations = 1usize << optional_count;
            assert!(
                combinations < 8,
                "{}/{} has {} optional fields (2^{} = {} combinations, must be < 8)",
                tool,
                action,
                optional_count,
                optional_count,
                combinations
            );
        }
    }

    #[test]
    fn tool_action_descriptions_contains_all_connectors() {
        let registry = test_registry();
        let desc = registry.tool_action_descriptions();
        assert!(desc.contains("trello has"));
        assert!(desc.contains("fm_jmap has"));
        assert!(desc.contains("slack has"));
        assert!(desc.contains("fm_caldav has"));
        assert!(desc.contains("fm_carddav has"));
        assert!(desc.contains("create_card"));
        assert!(desc.contains("search_emails"));
        assert!(desc.contains("send_message"));
        assert!(desc.contains("list_calendars"));
        assert!(desc.contains("search_events"));
        assert!(desc.contains("add_event"));
        assert!(desc.contains("list_addressbooks"));
        assert!(desc.contains("search_contacts"));
        assert!(desc.contains("add_contact"));
    }

    #[test]
    fn constraint_note_for_jmap_search_emails_is_nonempty() {
        let registry = test_registry();
        let note = registry.constraint_note_for("fm_jmap", "search_emails");
        assert!(
            !note.is_empty(),
            "jmap/search_emails should have a constraint note"
        );
        assert!(note.contains("365"));
    }

    #[test]
    fn constraint_note_for_unknown_returns_empty() {
        let registry = test_registry();
        let note = registry.constraint_note_for("trello", "create_card");
        assert!(note.is_empty());
    }

    #[test]
    fn alias_constraint_note_resolves() {
        let registry = test_registry();
        let note = registry.constraint_note_for("email", "search_emails");
        assert!(!note.is_empty(), "email alias should resolve constraint note");
    }

    #[test]
    fn alias_parameter_schema_matches_primary() {
        let registry = test_registry();
        let primary = registry.parameter_schema_for("fm_jmap", "search_emails").unwrap();
        let alias = registry.parameter_schema_for("email", "search_emails").unwrap();
        assert_eq!(primary, alias);
        let jmap_alias = registry.parameter_schema_for("jmap", "search_emails").unwrap();
        assert_eq!(primary, jmap_alias);
    }

    #[test]
    fn constraint_note_for_caldav_search_events_is_nonempty() {
        let registry = test_registry();
        let note = registry.constraint_note_for("fm_caldav", "search_events");
        assert!(!note.is_empty(), "fm_caldav/search_events should have a constraint note");
    }

    #[test]
    fn constraint_note_for_caldav_add_event_is_nonempty() {
        let registry = test_registry();
        let note = registry.constraint_note_for("fm_caldav", "add_event");
        assert!(!note.is_empty(), "fm_caldav/add_event should have a constraint note");
    }

    #[test]
    fn constraint_note_for_carddav_search_contacts_is_nonempty() {
        let registry = test_registry();
        let note = registry.constraint_note_for("fm_carddav", "search_contacts");
        assert!(!note.is_empty(), "fm_carddav/search_contacts should have a constraint note");
    }

    #[test]
    fn constraint_note_for_carddav_add_contact_is_nonempty() {
        let registry = test_registry();
        let note = registry.constraint_note_for("fm_carddav", "add_contact");
        assert!(!note.is_empty(), "fm_carddav/add_contact should have a constraint note");
    }

    #[test]
    fn caldav_alias_parameter_schema_matches_primary() {
        let registry = test_registry();
        let primary = registry.parameter_schema_for("fm_caldav", "search_events").unwrap();
        let alias = registry.parameter_schema_for("caldav", "search_events").unwrap();
        assert_eq!(primary, alias);
    }

    #[test]
    fn carddav_alias_parameter_schema_matches_primary() {
        let registry = test_registry();
        let primary = registry.parameter_schema_for("fm_carddav", "search_contacts").unwrap();
        let alias = registry.parameter_schema_for("carddav", "search_contacts").unwrap();
        assert_eq!(primary, alias);
    }
}
