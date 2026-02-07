use crate::config::FmCardDavConfig;
use crate::error::{KleviathanError, KleviathanResult};
use std::future::Future;
use std::pin::Pin;

use super::dav_common;
use super::registry::{ActionDescriptor, Connector, required_str};

const CARDDAV_BASE_URL: &str = "https://carddav.fastmail.com";

#[derive(Debug, serde::Serialize)]
pub struct AddressbookInfo {
    pub id: String,
    pub href: String,
    pub display_name: String,
}

#[derive(Debug, serde::Serialize)]
pub struct ContactSummary {
    pub uid: String,
    pub full_name: String,
    pub emails: Vec<String>,
    pub phones: Vec<String>,
    pub organization: String,
}

pub struct FmCardDavConnector {
    client: reqwest::Client,
    username: String,
    password: String,
    addressbook_home: String,
}

impl FmCardDavConnector {
    pub async fn new(config: &FmCardDavConfig) -> KleviathanResult<Self> {
        let client = reqwest::Client::new();
        let addressbook_home = format!(
            "{}/dav/addressbooks/user/{}/",
            CARDDAV_BASE_URL, config.username
        );
        let connector = Self {
            client,
            username: config.username.clone(),
            password: config.password.clone(),
            addressbook_home,
        };
        connector.validate_access().await?;
        Ok(connector)
    }

    async fn validate_access(&self) -> KleviathanResult<()> {
        dav_common::check_write_access(
            &self.client,
            &self.addressbook_home,
            &self.username,
            &self.password,
        )
        .await?;
        dav_common::verify_no_cross_access(
            &self.client,
            dav_common::CALDAV_BASE_URL,
            &self.username,
            &self.password,
            "CardDAV",
            "CalDAV",
        )
        .await?;
        Ok(())
    }

    pub async fn list_addressbooks(&self) -> KleviathanResult<Vec<AddressbookInfo>> {
        let body = concat!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
            "<d:propfind xmlns:d=\"DAV:\" xmlns:card=\"urn:ietf:params:xml:ns:carddav\">",
            "<d:prop>",
            "<d:displayname/>",
            "<d:resourcetype/>",
            "</d:prop>",
            "</d:propfind>"
        );

        let response = dav_common::propfind(
            &self.client,
            &self.addressbook_home,
            &self.username,
            &self.password,
            body,
            "1",
        )
        .await?;

        let status = response.status();
        dav_common::handle_dav_response_status(status, "CardDAV")?;
        let xml = response
            .text()
            .await
            .map_err(|e| KleviathanError::CardDav(format!("Failed to read response: {}", e)))?;

        parse_addressbook_list(&xml)
    }

    pub async fn search_contacts(
        &self,
        addressbook_id: &str,
        query: Option<&str>,
    ) -> KleviathanResult<Vec<ContactSummary>> {
        match self.search_contacts_with_id(addressbook_id, query).await {
            Err(KleviathanError::CardDav(ref msg)) if msg == "Resource not found" => {
                tracing::warn!(original_addressbook_id = %addressbook_id, "Address book not found, attempting auto-discovery");
                let addressbooks = self.list_addressbooks().await?;
                let fallback = addressbooks.first().ok_or_else(||
                    KleviathanError::CardDav("Resource not found and no address books available".into()))?;
                self.search_contacts_with_id(&fallback.id, query).await
            }
            other => other,
        }
    }

    async fn search_contacts_with_id(
        &self,
        addressbook_id: &str,
        query: Option<&str>,
    ) -> KleviathanResult<Vec<ContactSummary>> {
        let body = match query {
            Some(q) => format!(
                concat!(
                    "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
                    "<card:addressbook-query xmlns:d=\"DAV:\" xmlns:card=\"urn:ietf:params:xml:ns:carddav\">",
                    "<d:prop>",
                    "<d:getetag/>",
                    "<card:address-data/>",
                    "</d:prop>",
                    "<card:filter test=\"anyof\">",
                    "<card:prop-filter name=\"FN\">",
                    "<card:text-match collation=\"i;unicode-casemap\" match-type=\"contains\">{}</card:text-match>",
                    "</card:prop-filter>",
                    "<card:prop-filter name=\"EMAIL\">",
                    "<card:text-match collation=\"i;unicode-casemap\" match-type=\"contains\">{}</card:text-match>",
                    "</card:prop-filter>",
                    "<card:prop-filter name=\"TEL\">",
                    "<card:text-match collation=\"i;unicode-casemap\" match-type=\"contains\">{}</card:text-match>",
                    "</card:prop-filter>",
                    "</card:filter>",
                    "</card:addressbook-query>"
                ),
                q, q, q
            ),
            None => concat!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
                "<card:addressbook-query xmlns:d=\"DAV:\" xmlns:card=\"urn:ietf:params:xml:ns:carddav\">",
                "<d:prop>",
                "<d:getetag/>",
                "<card:address-data/>",
                "</d:prop>",
                "</card:addressbook-query>"
            )
            .to_string(),
        };

        let normalized_id = dav_common::normalize_resource_id(addressbook_id);
        let url = format!("{}{}/", self.addressbook_home, normalized_id);
        let response = dav_common::report(
            &self.client,
            &url,
            &self.username,
            &self.password,
            &body,
        )
        .await?;

        let status = response.status();
        dav_common::handle_dav_response_status(status, "CardDAV")?;
        let xml = response
            .text()
            .await
            .map_err(|e| KleviathanError::CardDav(format!("Failed to read response: {}", e)))?;

        let items = dav_common::parse_multistatus_items(&xml, "address-data");
        let contacts = items
            .iter()
            .map(|(_, vcard)| parse_vcard(vcard))
            .collect();

        Ok(contacts)
    }

    pub async fn add_contact(
        &self,
        addressbook_id: &str,
        full_name: &str,
        email: &str,
        phone: Option<&str>,
        organization: Option<&str>,
    ) -> KleviathanResult<String> {
        match self.add_contact_with_id(addressbook_id, full_name, email, phone, organization).await {
            Err(KleviathanError::CardDav(ref msg)) if msg == "Resource not found" => {
                tracing::warn!(original_addressbook_id = %addressbook_id, "Address book not found, attempting auto-discovery");
                let addressbooks = self.list_addressbooks().await?;
                let fallback = addressbooks.first().ok_or_else(||
                    KleviathanError::CardDav("Resource not found and no address books available".into()))?;
                self.add_contact_with_id(&fallback.id, full_name, email, phone, organization).await
            }
            other => other,
        }
    }

    async fn add_contact_with_id(
        &self,
        addressbook_id: &str,
        full_name: &str,
        email: &str,
        phone: Option<&str>,
        organization: Option<&str>,
    ) -> KleviathanResult<String> {
        let uid = uuid::Uuid::new_v4().to_string();
        let rev = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();

        let (last, first) = full_name
            .rsplit_once(' ')
            .map(|(f, l)| (l, f))
            .unwrap_or((full_name, ""));

        let phone_line = phone
            .filter(|p| !p.is_empty())
            .map(|p| format!("TEL;TYPE=CELL:{}\r\n", p))
            .unwrap_or_default();

        let org_line = organization
            .filter(|o| !o.is_empty())
            .map(|o| format!("ORG:{}\r\n", o))
            .unwrap_or_default();

        let vcard_body = format!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{}\r\nFN:{}\r\nN:{};{};;;\r\nEMAIL;TYPE=INTERNET:{}\r\n{}{}REV:{}\r\nEND:VCARD\r\n",
            uid, full_name, last, first, email, phone_line, org_line, rev
        );

        let normalized_id = dav_common::normalize_resource_id(addressbook_id);
        let url = format!("{}{}/{}.vcf", self.addressbook_home, normalized_id, uid);
        let response = self
            .client
            .put(&url)
            .header("Content-Type", "text/vcard")
            .basic_auth(&self.username, Some(&self.password))
            .body(vcard_body)
            .send()
            .await
            .map_err(KleviathanError::Http)?;

        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(KleviathanError::RateLimit(
                "CardDAV server returned 429 Too Many Requests".into(),
            ));
        }

        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(KleviathanError::CardDav("Resource not found".into()));
        }

        if status == reqwest::StatusCode::CREATED || status == reqwest::StatusCode::NO_CONTENT {
            Ok(uid)
        } else {
            Err(KleviathanError::CardDav(format!(
                "Failed to create contact: HTTP {}",
                status
            )))
        }
    }
}

fn parse_addressbook_list(xml: &str) -> KleviathanResult<Vec<AddressbookInfo>> {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

    let mut reader = Reader::from_str(xml);
    let mut addressbooks = Vec::new();

    let mut in_response = false;
    let mut in_resourcetype = false;
    let mut is_addressbook = false;
    let mut in_href = false;
    let mut in_displayname = false;
    let mut current_href = String::new();
    let mut current_displayname = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                match local {
                    "response" => {
                        in_response = true;
                        current_href.clear();
                        current_displayname.clear();
                        is_addressbook = false;
                    }
                    "resourcetype" if in_response => {
                        in_resourcetype = true;
                    }
                    "addressbook" if in_resourcetype => {
                        is_addressbook = true;
                    }
                    "href" if in_response && !in_resourcetype => {
                        in_href = true;
                    }
                    "displayname" if in_response => {
                        in_displayname = true;
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(e)) => {
                let text = String::from_utf8_lossy(e.as_ref());
                if in_href {
                    current_href.push_str(&text);
                } else if in_displayname {
                    current_displayname.push_str(&text);
                }
            }
            Ok(Event::End(ref e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                match local {
                    "response" => {
                        if in_response && is_addressbook && !current_href.is_empty() {
                            addressbooks.push(AddressbookInfo {
                                id: dav_common::extract_id_segment(&current_href),
                                href: current_href.clone(),
                                display_name: current_displayname.clone(),
                            });
                        }
                        in_response = false;
                    }
                    "resourcetype" => {
                        in_resourcetype = false;
                    }
                    "href" => {
                        in_href = false;
                    }
                    "displayname" => {
                        in_displayname = false;
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }

    Ok(addressbooks)
}

fn unfold_vcard_lines(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with(' ') || line.starts_with('\t') {
            result.push_str(&line[1..]);
        } else {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(line);
        }
    }
    result
}

fn parse_vcard(vcard_text: &str) -> ContactSummary {
    let unfolded = unfold_vcard_lines(vcard_text);
    let mut uid = String::new();
    let mut full_name = String::new();
    let mut emails = Vec::new();
    let mut phones = Vec::new();
    let mut organization = String::new();

    for line in unfolded.lines() {
        if let Some((prop_part, value)) = line.split_once(':') {
            let prop_name = prop_part
                .split_once(';')
                .map(|(name, _)| name)
                .unwrap_or(prop_part);
            match prop_name {
                "UID" => uid = value.to_string(),
                "FN" => full_name = value.to_string(),
                "EMAIL" => emails.push(value.to_string()),
                "TEL" => phones.push(value.to_string()),
                "ORG" => organization = value.to_string(),
                _ => {}
            }
        }
    }

    ContactSummary {
        uid,
        full_name,
        emails,
        phones,
        organization,
    }
}

fn local_name(full: &[u8]) -> &str {
    let s = std::str::from_utf8(full).unwrap_or("");
    s.rsplit_once(':').map(|(_, local)| local).unwrap_or(s)
}

pub struct FmCardDavConnectorProvider {
    config: FmCardDavConfig,
}

impl FmCardDavConnectorProvider {
    pub fn new(config: FmCardDavConfig) -> Self {
        Self { config }
    }
}

impl Connector for FmCardDavConnectorProvider {
    fn tool_name(&self) -> &str {
        "fm_carddav"
    }

    fn tool_aliases(&self) -> &[&str] {
        &["carddav", "contacts"]
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor {
                name: "list_addressbooks",
                description: "List all address books for the user",
                parameter_schema: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                    "additionalProperties": false
                }),
                constraint_note: "",
            },
            ActionDescriptor {
                name: "search_contacts",
                description: "Search contacts in an address book by name, email, or phone",
                parameter_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "addressbook_id": { "type": "string" },
                        "query": { "type": "string" }
                    },
                    "required": ["addressbook_id"],
                    "additionalProperties": false
                }),
                constraint_note: " The addressbook_id is the address book path segment (not the full URL). The optional query parameter searches across name, email, and phone fields. Omit query to list all contacts.",
            },
            ActionDescriptor {
                name: "add_contact",
                description: "Create a new contact in an address book",
                parameter_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "addressbook_id": { "type": "string" },
                        "full_name": { "type": "string" },
                        "email": { "type": "string" },
                        "phone": { "type": "string" },
                        "organization": { "type": "string" }
                    },
                    "required": ["addressbook_id", "full_name", "email"],
                    "additionalProperties": false
                }),
                constraint_note: " Addressbook ID, full name, and email are required. Phone and organization are optional.",
            },
        ]
    }

    fn execute<'a>(
        &'a self,
        action: &'a str,
        params: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = KleviathanResult<serde_json::Value>> + Send + 'a>> {
        Box::pin(async move {
            let connector = FmCardDavConnector::new(&self.config).await?;
            match action {
                "list_addressbooks" => {
                    let addressbooks = connector.list_addressbooks().await?;
                    Ok(serde_json::to_value(addressbooks)?)
                }
                "search_contacts" => {
                    let addressbook_id = required_str(&params, "addressbook_id")?;
                    let query = params["query"].as_str();
                    let contacts = connector.search_contacts(&addressbook_id, query).await?;
                    Ok(serde_json::to_value(contacts)?)
                }
                "add_contact" => {
                    let addressbook_id = required_str(&params, "addressbook_id")?;
                    let full_name = required_str(&params, "full_name")?;
                    let email = required_str(&params, "email")?;
                    let phone = params["phone"].as_str();
                    let organization = params["organization"].as_str();
                    let uid = connector
                        .add_contact(&addressbook_id, &full_name, &email, phone, organization)
                        .await?;
                    Ok(serde_json::json!({ "uid": uid, "addressbook_id": addressbook_id }))
                }
                other => Err(KleviathanError::TaskGraph(format!(
                    "Unknown fm_carddav action: {}", other
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
    fn carddav_provider_declares_expected_actions() {
        let provider = FmCardDavConnectorProvider::new(crate::config::FmCardDavConfig {
            username: "test".into(),
            password: "test".into(),
        });
        assert_eq!(provider.tool_name(), "fm_carddav");
        assert_eq!(provider.tool_aliases(), &["carddav", "contacts"]);
        let action_names: Vec<&str> = provider.actions().iter().map(|a| a.name).collect();
        assert_eq!(action_names, vec!["list_addressbooks", "search_contacts", "add_contact"]);
    }

    #[test]
    fn vcard_parses_standard_format() {
        let vcard = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:abc-123\r\nFN:John Doe\r\nN:Doe;John;;;\r\nEMAIL;TYPE=INTERNET:john@example.com\r\nTEL;TYPE=CELL:+1234567890\r\nORG:Acme Corp\r\nEND:VCARD";
        let contact = parse_vcard(vcard);
        assert_eq!(contact.uid, "abc-123");
        assert_eq!(contact.full_name, "John Doe");
        assert_eq!(contact.emails, vec!["john@example.com"]);
        assert_eq!(contact.phones, vec!["+1234567890"]);
        assert_eq!(contact.organization, "Acme Corp");
    }

    #[test]
    fn vcard_parses_multiple_emails_and_phones() {
        let vcard = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:multi-456\r\nFN:Jane Smith\r\nEMAIL;TYPE=WORK:jane@work.com\r\nEMAIL;TYPE=HOME:jane@home.com\r\nTEL;TYPE=WORK:+1111111111\r\nTEL;TYPE=CELL:+2222222222\r\nORG:Widget Inc\r\nEND:VCARD";
        let contact = parse_vcard(vcard);
        assert_eq!(contact.uid, "multi-456");
        assert_eq!(contact.full_name, "Jane Smith");
        assert_eq!(contact.emails, vec!["jane@work.com", "jane@home.com"]);
        assert_eq!(contact.phones, vec!["+1111111111", "+2222222222"]);
        assert_eq!(contact.organization, "Widget Inc");
    }

    #[test]
    fn vcard_parses_minimal() {
        let vcard = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:min-789\r\nFN:Solo Name\r\nEND:VCARD";
        let contact = parse_vcard(vcard);
        assert_eq!(contact.uid, "min-789");
        assert_eq!(contact.full_name, "Solo Name");
        assert!(contact.emails.is_empty());
        assert!(contact.phones.is_empty());
        assert_eq!(contact.organization, "");
    }

    #[test]
    fn vcard_handles_line_folding() {
        let vcard = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:fold-001\r\nFN:A Very Long\r\n  Name That Wraps\r\nEMAIL:test@example.com\r\nEND:VCARD";
        let contact = parse_vcard(vcard);
        assert_eq!(contact.uid, "fold-001");
        assert_eq!(contact.full_name, "A Very Long Name That Wraps");
        assert_eq!(contact.emails, vec!["test@example.com"]);
    }

    #[test]
    fn module_contains_no_prohibited_operations() {
        let source = include_str!("fm_carddav.rs");
        let non_test = source.split("#[cfg(test)]").next().unwrap_or(source);

        let delete_needle = [".del", "ete("].concat();
        assert!(
            !non_test.contains(&delete_needle),
            "CardDAV connector must not contain HTTP DELETE calls"
        );

        let patch_needle = [".pat", "ch("].concat();
        assert!(
            !non_test.contains(&patch_needle),
            "CardDAV connector must not contain HTTP PATCH calls"
        );
    }

    #[test]
    fn addressbook_info_serializes_to_json() {
        let info = AddressbookInfo {
            id: "default".into(),
            href: "/dav/addressbooks/user/test/default/".into(),
            display_name: "My Contacts".into(),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["id"], "default");
        assert_eq!(json["href"], "/dav/addressbooks/user/test/default/");
        assert_eq!(json["display_name"], "My Contacts");
    }

    #[test]
    fn contact_summary_serializes_to_json() {
        let contact = ContactSummary {
            uid: "test-uid".into(),
            full_name: "Test User".into(),
            emails: vec!["a@b.com".into(), "c@d.com".into()],
            phones: vec!["+1111".into(), "+2222".into()],
            organization: "TestOrg".into(),
        };
        let json = serde_json::to_value(&contact).unwrap();
        assert_eq!(json["uid"], "test-uid");
        assert_eq!(json["full_name"], "Test User");
        assert_eq!(json["emails"], serde_json::json!(["a@b.com", "c@d.com"]));
        assert_eq!(json["phones"], serde_json::json!(["+1111", "+2222"]));
        assert_eq!(json["organization"], "TestOrg");
    }
}
