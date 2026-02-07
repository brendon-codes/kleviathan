use crate::config::FmCalDavConfig;
use crate::error::{KleviathanError, KleviathanResult};
use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use icalendar::{Calendar, Component, Event, EventLike, Property};

use super::dav_common;
use super::registry::{ActionDescriptor, Connector, required_str};

const SUMMARY_MAX_CHARS: usize = 160;
const LOCATION_MAX_CHARS: usize = 160;
const DESCRIPTION_MAX_CHARS: usize = 4096;
const DESCRIPTION_TRUNCATION_SUFFIX: &str = " [truncated]";

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedEventDateTime {
    Date(NaiveDate),
    Utc(DateTime<Utc>),
}

#[derive(Debug, serde::Serialize)]
pub struct CalendarInfo {
    pub id: String,
    pub href: String,
    pub display_name: String,
}

#[derive(Debug, serde::Serialize)]
pub struct EventSummary {
    pub uid: String,
    pub summary: String,
    pub dtstart: String,
    pub dtend: String,
    pub description: String,
    pub location: String,
}

pub struct FmCalDavConnector {
    client: reqwest::Client,
    username: String,
    password: String,
    calendar_home: String,
}

impl FmCalDavConnector {
    pub async fn new(config: &FmCalDavConfig) -> KleviathanResult<Self> {
        let client = reqwest::Client::new();
        let calendar_home = format!(
            "{}/dav/calendars/user/{}/",
            dav_common::CALDAV_BASE_URL,
            config.username
        );
        let connector = Self {
            client,
            username: config.username.clone(),
            password: config.password.clone(),
            calendar_home,
        };
        connector.validate_access().await?;
        Ok(connector)
    }

    async fn validate_access(&self) -> KleviathanResult<()> {
        dav_common::check_write_access(
            &self.client,
            &self.calendar_home,
            &self.username,
            &self.password,
        )
        .await?;
        dav_common::verify_no_cross_access(
            &self.client,
            dav_common::CARDDAV_BASE_URL,
            &self.username,
            &self.password,
            "CalDAV",
            "CardDAV",
        )
        .await?;
        Ok(())
    }

    pub async fn list_calendars(&self) -> KleviathanResult<Vec<CalendarInfo>> {
        let body = concat!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
            "<d:propfind xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\" xmlns:cs=\"http://calendarserver.org/ns/\">",
            "<d:prop>",
            "<d:displayname/>",
            "<d:resourcetype/>",
            "<cs:getctag/>",
            "</d:prop>",
            "</d:propfind>"
        );

        let response = dav_common::propfind(
            &self.client,
            &self.calendar_home,
            &self.username,
            &self.password,
            body,
            "1",
        )
        .await?;

        let status = response.status();
        dav_common::handle_dav_response_status(status, "CalDAV")?;
        let xml = response
            .text()
            .await
            .map_err(|e| KleviathanError::CalDav(format!("Failed to read response: {}", e)))?;

        parse_calendar_list(&xml)
    }

    pub async fn search_events(
        &self,
        calendar_id: &str,
        start_date: &str,
        end_date: &str,
        query: Option<&str>,
    ) -> KleviathanResult<Vec<EventSummary>> {
        match self
            .search_events_with_id(calendar_id, start_date, end_date, query)
            .await
        {
            Err(KleviathanError::CalDav(ref msg)) if msg == "Resource not found" => {
                tracing::warn!(original_calendar_id = %calendar_id, "Calendar not found, attempting auto-discovery");
                let calendars = self.list_calendars().await?;
                let fallback = calendars.first().ok_or_else(|| {
                    KleviathanError::CalDav("Resource not found and no calendars available".into())
                })?;
                self.search_events_with_id(&fallback.id, start_date, end_date, query)
                    .await
            }
            other => other,
        }
    }

    async fn search_events_with_id(
        &self,
        calendar_id: &str,
        start_date: &str,
        end_date: &str,
        query: Option<&str>,
    ) -> KleviathanResult<Vec<EventSummary>> {
        let start_parsed =
            chrono::NaiveDate::parse_from_str(start_date, "%Y-%m-%d").map_err(|e| {
                KleviathanError::CalDav(format!("Invalid start_date '{}': {}", start_date, e))
            })?;
        let end_parsed = chrono::NaiveDate::parse_from_str(end_date, "%Y-%m-%d").map_err(|e| {
            KleviathanError::CalDav(format!("Invalid end_date '{}': {}", end_date, e))
        })?;

        let start_str = format!("{}T000000Z", start_parsed.format("%Y%m%d"));
        let end_str = format!(
            "{}T000000Z",
            (end_parsed + chrono::Duration::days(1)).format("%Y%m%d")
        );

        let body = format!(
            concat!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
                "<c:calendar-query xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\">",
                "<d:prop>",
                "<d:getetag/>",
                "<c:calendar-data/>",
                "</d:prop>",
                "<c:filter>",
                "<c:comp-filter name=\"VCALENDAR\">",
                "<c:comp-filter name=\"VEVENT\">",
                "<c:time-range start=\"{}\" end=\"{}\"/>",
                "</c:comp-filter>",
                "</c:comp-filter>",
                "</c:filter>",
                "</c:calendar-query>"
            ),
            start_str, end_str
        );

        let normalized_id = dav_common::normalize_resource_id(calendar_id);
        let url = format!("{}{}/", self.calendar_home, normalized_id);
        let response =
            dav_common::report(&self.client, &url, &self.username, &self.password, &body).await?;

        let status = response.status();
        dav_common::handle_dav_response_status(status, "CalDAV")?;
        let xml = response
            .text()
            .await
            .map_err(|e| KleviathanError::CalDav(format!("Failed to read response: {}", e)))?;

        let items = dav_common::parse_multistatus_items(&xml, "calendar-data");
        let events: Vec<EventSummary> = items
            .iter()
            .map(|(_, ical)| parse_ical_event(ical))
            .collect();

        match query {
            Some(q) => {
                let q_lower = q.to_lowercase();
                Ok(events
                    .into_iter()
                    .filter(|e| {
                        e.summary.to_lowercase().contains(&q_lower)
                            || e.description.to_lowercase().contains(&q_lower)
                    })
                    .collect())
            }
            None => Ok(events),
        }
    }

    pub async fn add_event(
        &self,
        calendar_id: &str,
        summary: &str,
        description: &str,
        start: &str,
        end: &str,
        location: Option<&str>,
    ) -> KleviathanResult<String> {
        match self
            .add_event_with_id(calendar_id, summary, description, start, end, location)
            .await
        {
            Err(KleviathanError::CalDav(ref msg)) if msg == "Resource not found" => {
                tracing::warn!(original_calendar_id = %calendar_id, "Calendar not found, attempting auto-discovery");
                let calendars = self.list_calendars().await?;
                let fallback = calendars.first().ok_or_else(|| {
                    KleviathanError::CalDav("Resource not found and no calendars available".into())
                })?;
                self.add_event_with_id(&fallback.id, summary, description, start, end, location)
                    .await
            }
            other => other,
        }
    }

    async fn add_event_with_id(
        &self,
        calendar_id: &str,
        summary: &str,
        description: &str,
        start: &str,
        end: &str,
        location: Option<&str>,
    ) -> KleviathanResult<String> {
        let uid = uuid::Uuid::new_v4().to_string();
        let ical_body =
            build_ical_body(summary, description, start, end, location, &uid, Utc::now())?;

        let normalized_id = dav_common::normalize_resource_id(calendar_id);
        let url = format!("{}{}/{}.ics", self.calendar_home, normalized_id, uid);
        let response = self
            .client
            .put(&url)
            .header("Content-Type", "text/calendar")
            .basic_auth(&self.username, Some(&self.password))
            .body(ical_body)
            .send()
            .await
            .map_err(KleviathanError::Http)?;

        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(KleviathanError::RateLimit(
                "CalDAV server returned 429 Too Many Requests".into(),
            ));
        }

        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(KleviathanError::CalDav("Resource not found".into()));
        }

        if status == reqwest::StatusCode::CREATED || status == reqwest::StatusCode::NO_CONTENT {
            Ok(uid)
        } else {
            Err(KleviathanError::CalDav(format!(
                "Failed to create event: HTTP {}",
                status
            )))
        }
    }
}

fn build_ical_body(
    summary: &str,
    description: &str,
    start: &str,
    end: &str,
    location: Option<&str>,
    uid: &str,
    dtstamp: DateTime<Utc>,
) -> KleviathanResult<String> {
    let normalized_summary = normalize_required_summary(summary)?;
    let normalized_description = normalize_description(description);
    let normalized_location = location
        .map(|value| normalize_single_line_text(value, LOCATION_MAX_CHARS))
        .filter(|value| !value.is_empty());

    let start = parse_event_datetime(start)?;
    let end = parse_event_datetime(end)?;

    let mut event = Event::new();
    event
        .uid(uid)
        .summary(&normalized_summary)
        .timestamp(dtstamp);

    match (start, end) {
        (ParsedEventDateTime::Utc(start), ParsedEventDateTime::Utc(end)) => {
            event.starts(start).ends(end);
        }
        (ParsedEventDateTime::Date(start), ParsedEventDateTime::Date(end)) => {
            event.starts(start).ends(end);
        }
        _ => {
            return Err(KleviathanError::CalDav(
                "Invalid event datetime format: start and end must both be UTC datetimes or both be all-day dates".into(),
            ));
        }
    }

    if !normalized_description.is_empty() {
        event.description(&normalized_description);
    }

    if let Some(location) = normalized_location.as_deref() {
        event.location(location);
    }

    let mut calendar = Calendar::empty();
    calendar.append_property(Property::new("VERSION", "2.0"));
    calendar.append_property(Property::new("PRODID", "-//Kleviathan//EN"));
    calendar.append_property(Property::new("CALSCALE", "GREGORIAN"));
    calendar.push(event.done());
    Ok(calendar.to_string())
}

fn normalize_required_summary(value: &str) -> KleviathanResult<String> {
    let normalized = normalize_single_line_text(value, SUMMARY_MAX_CHARS);
    if normalized.is_empty() {
        return Err(KleviathanError::CalDav(
            "Invalid event summary: value is empty after normalization".into(),
        ));
    }
    Ok(normalized)
}

fn normalize_single_line_text(value: &str, max_chars: usize) -> String {
    let sanitized = sanitize_event_text(value, false);
    let collapsed = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(collapsed.trim(), max_chars, None)
}

fn normalize_description(value: &str) -> String {
    let sanitized = sanitize_event_text(value, true);
    let trimmed = sanitized
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    truncate_chars(
        &trimmed,
        DESCRIPTION_MAX_CHARS,
        Some(DESCRIPTION_TRUNCATION_SUFFIX),
    )
}

fn sanitize_event_text(value: &str, preserve_newlines: bool) -> String {
    normalize_line_endings(value)
        .chars()
        .filter_map(|ch| sanitize_char(ch, preserve_newlines))
        .collect()
}

fn sanitize_char(ch: char, preserve_newlines: bool) -> Option<char> {
    if is_removed_unicode(ch) {
        return None;
    }

    match ch {
        '\n' if preserve_newlines => Some('\n'),
        '\n' | '\t' => Some(' '),
        _ if ch.is_control() => None,
        _ => Some(ch),
    }
}

fn normalize_line_endings(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

fn is_removed_unicode(ch: char) -> bool {
    matches!(
        ch,
        '\u{200B}'
            | '\u{200C}'
            | '\u{200D}'
            | '\u{FEFF}'
            | '\u{202A}'
            | '\u{202B}'
            | '\u{202C}'
            | '\u{202D}'
            | '\u{202E}'
            | '\u{2066}'
            | '\u{2067}'
            | '\u{2068}'
            | '\u{2069}'
    )
}

fn truncate_chars(value: &str, max_chars: usize, suffix: Option<&str>) -> String {
    let value_chars = value.chars().count();
    if value_chars <= max_chars {
        return value.to_string();
    }

    match suffix {
        Some(suffix) => {
            let suffix_chars = suffix.chars().count();
            if max_chars <= suffix_chars {
                return suffix.chars().take(max_chars).collect();
            }

            let mut truncated: String = value.chars().take(max_chars - suffix_chars).collect();
            truncated.push_str(suffix);
            truncated
        }
        None => value.chars().take(max_chars).collect(),
    }
}

fn parse_event_datetime(value: &str) -> KleviathanResult<ParsedEventDateTime> {
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y%m%d") {
        return Ok(ParsedEventDateTime::Date(date));
    }

    if let Ok(datetime) = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%SZ") {
        return Ok(ParsedEventDateTime::Utc(datetime.and_utc()));
    }

    Err(KleviathanError::CalDav(
        "Invalid event datetime format".into(),
    ))
}

fn parse_calendar_list(xml: &str) -> KleviathanResult<Vec<CalendarInfo>> {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

    let mut reader = Reader::from_str(xml);
    let mut calendars = Vec::new();

    let mut in_response = false;
    let mut in_resourcetype = false;
    let mut is_calendar = false;
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
                        is_calendar = false;
                    }
                    "resourcetype" if in_response => {
                        in_resourcetype = true;
                    }
                    "calendar" if in_resourcetype => {
                        is_calendar = true;
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
                        if in_response && is_calendar && !current_href.is_empty() {
                            calendars.push(CalendarInfo {
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

    Ok(calendars)
}

fn unfold_ical_lines(text: &str) -> String {
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

fn parse_ical_event(ical_text: &str) -> EventSummary {
    let mut uid = String::new();
    let mut summary = String::new();
    let mut dtstart = String::new();
    let mut dtend = String::new();
    let mut description = String::new();
    let mut location = String::new();

    let unfolded = unfold_ical_lines(ical_text);
    for line in unfolded.lines() {
        let line = line.trim_end_matches('\r');
        if let Some((prop_part, value)) = line.split_once(':') {
            let prop_name = prop_part
                .split_once(';')
                .map(|(name, _)| name)
                .unwrap_or(prop_part);
            match prop_name {
                "UID" => uid = value.to_string(),
                "SUMMARY" => summary = unescape_ical_text(value),
                "DTSTART" => dtstart = value.to_string(),
                "DTEND" => dtend = value.to_string(),
                "DESCRIPTION" => description = unescape_ical_text(value),
                "LOCATION" => location = unescape_ical_text(value),
                _ => {}
            }
        }
    }

    EventSummary {
        uid,
        summary,
        dtstart,
        dtend,
        description,
        location,
    }
}

fn local_name(full: &[u8]) -> &str {
    let s = std::str::from_utf8(full).unwrap_or("");
    s.rsplit_once(':').map(|(_, local)| local).unwrap_or(s)
}

fn unescape_ical_text(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars();

    while let Some(ch) = chars.next() {
        if ch != '\\' {
            result.push(ch);
            continue;
        }

        match chars.next() {
            Some('n') | Some('N') => result.push('\n'),
            Some('\\') => result.push('\\'),
            Some(',') => result.push(','),
            Some(';') => result.push(';'),
            Some(other) => {
                result.push(other);
            }
            None => result.push('\\'),
        }
    }

    result
}

pub struct FmCalDavConnectorProvider {
    config: FmCalDavConfig,
}

impl FmCalDavConnectorProvider {
    pub fn new(config: FmCalDavConfig) -> Self {
        Self { config }
    }
}

impl Connector for FmCalDavConnectorProvider {
    fn tool_name(&self) -> &str {
        "fm_caldav"
    }

    fn tool_aliases(&self) -> &[&str] {
        &["caldav", "calendar"]
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor {
                name: "list_calendars",
                description: "List all calendars for the user",
                parameter_schema: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                    "additionalProperties": false
                }),
                constraint_note: "",
            },
            ActionDescriptor {
                name: "search_events",
                description: "Search calendar events by date range and optional text query",
                parameter_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "calendar_id": { "type": "string" },
                        "start_date": { "type": "string" },
                        "end_date": { "type": "string" },
                        "query": { "type": "string" }
                    },
                    "required": ["calendar_id", "start_date", "end_date"],
                    "additionalProperties": false
                }),
                constraint_note: " The calendar_id is the calendar path segment (not the full URL) or the full href from list_calendars. Dates must be in YYYY-MM-DD format. The range includes all events overlapping the start_date through end_date (inclusive). The optional query parameter filters events by summary or description text.",
            },
            ActionDescriptor {
                name: "add_event",
                description: "Create a new calendar event",
                parameter_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "calendar_id": { "type": "string" },
                        "summary": { "type": "string" },
                        "description": { "type": "string" },
                        "start": { "type": "string" },
                        "end": { "type": "string" },
                        "location": { "type": "string" }
                    },
                    "required": ["calendar_id", "summary", "start", "end"],
                    "additionalProperties": false
                }),
                constraint_note: " Start and end must be in iCalendar datetime format (YYYYMMDDTHHmmssZ for UTC, or YYYYMMDD for all-day events). Events are created without invitees.",
            },
        ]
    }

    fn execute<'a>(
        &'a self,
        action: &'a str,
        params: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = KleviathanResult<serde_json::Value>> + Send + 'a>> {
        Box::pin(async move {
            let connector = FmCalDavConnector::new(&self.config).await?;
            match action {
                "list_calendars" => {
                    let calendars = connector.list_calendars().await?;
                    Ok(serde_json::to_value(calendars)?)
                }
                "search_events" => {
                    let calendar_id = required_str(&params, "calendar_id")?;
                    let start_date = required_str(&params, "start_date")?;
                    let end_date = required_str(&params, "end_date")?;
                    let query = params["query"].as_str();
                    let events = connector
                        .search_events(&calendar_id, &start_date, &end_date, query)
                        .await?;
                    Ok(serde_json::to_value(events)?)
                }
                "add_event" => {
                    let calendar_id = required_str(&params, "calendar_id")?;
                    let summary = required_str(&params, "summary")?;
                    let description = params["description"].as_str().unwrap_or("");
                    let start = required_str(&params, "start")?;
                    let end = required_str(&params, "end")?;
                    let location = params["location"].as_str();
                    let uid = connector
                        .add_event(&calendar_id, &summary, description, &start, &end, location)
                        .await?;
                    Ok(serde_json::json!({ "uid": uid, "calendar_id": calendar_id }))
                }
                other => Err(KleviathanError::TaskGraph(format!(
                    "Unknown fm_caldav action: {}",
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

    fn fixed_dtstamp() -> DateTime<Utc> {
        NaiveDateTime::parse_from_str("20260320T120000Z", "%Y%m%dT%H%M%SZ")
            .unwrap()
            .and_utc()
    }

    fn build_body(
        summary: &str,
        description: &str,
        start: &str,
        end: &str,
        location: Option<&str>,
    ) -> String {
        build_ical_body(
            summary,
            description,
            start,
            end,
            location,
            "test-uid",
            fixed_dtstamp(),
        )
        .unwrap()
    }

    #[test]
    fn caldav_provider_declares_expected_actions() {
        let provider = FmCalDavConnectorProvider::new(crate::config::FmCalDavConfig {
            username: "test".into(),
            password: "test".into(),
        });
        assert_eq!(provider.tool_name(), "fm_caldav");
        assert_eq!(provider.tool_aliases(), &["caldav", "calendar"]);
        let action_names: Vec<&str> = provider.actions().iter().map(|a| a.name).collect();
        assert_eq!(
            action_names,
            vec!["list_calendars", "search_events", "add_event"]
        );
    }

    #[test]
    fn search_events_schema_uses_date_range() {
        let provider = FmCalDavConnectorProvider::new(crate::config::FmCalDavConfig {
            username: "test".into(),
            password: "test".into(),
        });
        let actions = provider.actions();
        let search = actions.iter().find(|a| a.name == "search_events").unwrap();
        let required = search.parameter_schema["required"].as_array().unwrap();
        let required_names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_names.contains(&"start_date"));
        assert!(required_names.contains(&"end_date"));
        assert!(!required_names.contains(&"days"));
        let props = search.parameter_schema["properties"].as_object().unwrap();
        assert!(props.contains_key("start_date"));
        assert!(props.contains_key("end_date"));
        assert!(!props.contains_key("days"));
    }

    #[test]
    fn ical_event_parses_standard_format() {
        let ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:abc-123\r\nSUMMARY:Team Standup\r\nDTSTART:20260320T100000Z\r\nDTEND:20260320T103000Z\r\nDESCRIPTION:Daily standup meeting\r\nLOCATION:Conference Room A\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let event = parse_ical_event(ical);
        assert_eq!(event.uid, "abc-123");
        assert_eq!(event.summary, "Team Standup");
        assert_eq!(event.dtstart, "20260320T100000Z");
        assert_eq!(event.dtend, "20260320T103000Z");
        assert_eq!(event.description, "Daily standup meeting");
        assert_eq!(event.location, "Conference Room A");
    }

    #[test]
    fn ical_event_parses_with_timezone_params() {
        let ical = "BEGIN:VEVENT\r\nUID:tz-456\r\nSUMMARY:Lunch\r\nDTSTART;TZID=America/New_York:20260320T120000\r\nDTEND;TZID=America/New_York:20260320T130000\r\nEND:VEVENT";
        let event = parse_ical_event(ical);
        assert_eq!(event.uid, "tz-456");
        assert_eq!(event.summary, "Lunch");
        assert_eq!(event.dtstart, "20260320T120000");
        assert_eq!(event.dtend, "20260320T130000");
    }

    #[test]
    fn ical_event_parses_minimal() {
        let ical = "BEGIN:VEVENT\r\nUID:min-789\r\nSUMMARY:Quick Note\r\nEND:VEVENT";
        let event = parse_ical_event(ical);
        assert_eq!(event.uid, "min-789");
        assert_eq!(event.summary, "Quick Note");
        assert_eq!(event.dtstart, "");
        assert_eq!(event.dtend, "");
        assert_eq!(event.description, "");
        assert_eq!(event.location, "");
    }

    #[test]
    fn single_line_text_normalizes_whitespace_and_removes_hidden_chars() {
        let result = normalize_single_line_text(
            "  Team\r\n\tSync\u{200B}\u{202E}\u{0000}  Today  ",
            SUMMARY_MAX_CHARS,
        );
        assert_eq!(result, "Team Sync Today");
    }

    #[test]
    fn single_line_text_truncates_to_max_chars() {
        let result = normalize_single_line_text("abcdefghij", 6);
        assert_eq!(result, "abcdef");
    }

    #[test]
    fn required_summary_rejects_empty_after_normalization() {
        let error = normalize_required_summary(" \r\n\t\u{200B}\u{202E}\u{0000} ").unwrap_err();
        match error {
            KleviathanError::CalDav(message) => {
                assert_eq!(
                    message,
                    "Invalid event summary: value is empty after normalization"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn description_normalization_preserves_newlines() {
        let result = normalize_description("First\tline\r\nSecond\u{200B} line\rThird line  ");
        assert_eq!(result, "First line\nSecond line\nThird line");
    }

    #[test]
    fn description_normalization_truncates_with_suffix() {
        let result = normalize_description(&"a".repeat(DESCRIPTION_MAX_CHARS + 32));
        assert_eq!(result.chars().count(), DESCRIPTION_MAX_CHARS);
        assert!(result.ends_with(DESCRIPTION_TRUNCATION_SUFFIX));
    }

    #[test]
    fn build_ical_body_serializes_utc_datetimes() {
        let body = build_body(
            "Team Standup",
            "Daily sync",
            "20260320T100000Z",
            "20260320T103000Z",
            Some("Room 1"),
        );
        let event = parse_ical_event(&body);
        assert_eq!(event.uid, "test-uid");
        assert_eq!(event.summary, "Team Standup");
        assert_eq!(event.dtstart, "20260320T100000Z");
        assert_eq!(event.dtend, "20260320T103000Z");
        assert_eq!(event.description, "Daily sync");
        assert_eq!(event.location, "Room 1");
        assert!(body.contains("PRODID:-//Kleviathan//EN"));
    }

    #[test]
    fn build_ical_body_serializes_all_day_dates() {
        let body = build_body("Out of Office", "", "20260320", "20260321", None);
        let event = parse_ical_event(&body);
        assert_eq!(event.dtstart, "20260320");
        assert_eq!(event.dtend, "20260321");
        assert!(!body.contains("DTSTART:20260320T"));
        assert!(!body.contains("DTEND:20260321T"));
    }

    #[test]
    fn build_ical_body_rejects_mixed_datetime_formats() {
        let error = build_ical_body(
            "Meeting",
            "",
            "20260320",
            "20260320T100000Z",
            None,
            "test-uid",
            fixed_dtstamp(),
        )
        .unwrap_err();
        match error {
            KleviathanError::CalDav(message) => {
                assert_eq!(
                    message,
                    "Invalid event datetime format: start and end must both be UTC datetimes or both be all-day dates"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn build_ical_body_rejects_invalid_datetime_format() {
        let error = build_ical_body(
            "Meeting",
            "",
            "2026-03-20T10:00:00Z",
            "20260320T110000Z",
            None,
            "test-uid",
            fixed_dtstamp(),
        )
        .unwrap_err();
        match error {
            KleviathanError::CalDav(message) => {
                assert_eq!(message, "Invalid event datetime format");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn build_ical_body_omits_empty_location_after_normalization() {
        let body = build_body(
            "Meeting",
            "",
            "20260320T100000Z",
            "20260320T103000Z",
            Some(" \r\n\t\u{200B}\u{202E} "),
        );
        let event = parse_ical_event(&body);
        assert_eq!(event.location, "");
        assert!(!body.contains("\r\nLOCATION"));
    }

    #[test]
    fn build_ical_body_blocks_property_injection() {
        let body = build_body(
            "Team Sync\r\nATTENDEE:mailto:bad@example.com",
            "Discuss roadmap\r\nLOCATION:Injected",
            "20260320T100000Z",
            "20260320T103000Z",
            Some("HQ\r\nORGANIZER:mailto:bad@example.com"),
        );
        let event = parse_ical_event(&body);
        assert_eq!(event.summary, "Team Sync ATTENDEE:mailto:bad@example.com");
        assert_eq!(event.description, "Discuss roadmap\nLOCATION:Injected");
        assert_eq!(event.location, "HQ ORGANIZER:mailto:bad@example.com");
        assert!(!body.contains("\r\nATTENDEE:"));
        assert!(!body.contains("\r\nORGANIZER:"));
        assert_eq!(body.matches("\r\nLOCATION").count(), 1);
    }

    #[test]
    fn build_ical_body_escapes_text_values() {
        let body = build_body(
            "Quarterly, Review; Backslash\\Test",
            "Line one\nLine two, still here; path \\server",
            "20260320T100000Z",
            "20260320T103000Z",
            Some("Room, 1; A\\B"),
        );
        let event = parse_ical_event(&body);
        assert_eq!(event.summary, "Quarterly, Review; Backslash\\Test");
        assert_eq!(
            event.description,
            "Line one\nLine two, still here; path \\server"
        );
        assert_eq!(event.location, "Room, 1; A\\B");
        assert!(body.contains("SUMMARY:Quarterly\\, Review\\; Backslash\\\\Test"));
        assert!(body.contains("DESCRIPTION:Line one\\nLine two\\, still here\\; path \\\\server"));
        assert!(body.contains("LOCATION:Room\\, 1\\; A\\\\B"));
    }

    #[test]
    fn module_contains_no_prohibited_operations() {
        let source = include_str!("fm_caldav.rs");
        let non_test = source.split("#[cfg(test)]").next().unwrap_or(source);

        let delete_needle = [".del", "ete("].concat();
        assert!(
            !non_test.contains(&delete_needle),
            "CalDAV connector must not contain HTTP DELETE calls"
        );

        let patch_needle = [".pat", "ch("].concat();
        assert!(
            !non_test.contains(&patch_needle),
            "CalDAV connector must not contain HTTP PATCH calls"
        );

        assert!(
            !non_test.contains("ATTENDEE"),
            "CalDAV connector must not add event invites (ATTENDEE)"
        );

        assert!(
            !non_test.contains("ORGANIZER"),
            "CalDAV connector must not add event organizer (ORGANIZER)"
        );
    }

    #[test]
    fn calendar_info_serializes_to_json() {
        let info = CalendarInfo {
            id: "default".into(),
            href: "/dav/calendars/user/test/default/".into(),
            display_name: "My Calendar".into(),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["id"], "default");
        assert_eq!(json["href"], "/dav/calendars/user/test/default/");
        assert_eq!(json["display_name"], "My Calendar");
    }

    #[test]
    fn event_summary_serializes_to_json() {
        let event = EventSummary {
            uid: "test-uid".into(),
            summary: "Meeting".into(),
            dtstart: "20260320T100000Z".into(),
            dtend: "20260320T110000Z".into(),
            description: "A meeting".into(),
            location: "Room 1".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["uid"], "test-uid");
        assert_eq!(json["summary"], "Meeting");
        assert_eq!(json["dtstart"], "20260320T100000Z");
        assert_eq!(json["dtend"], "20260320T110000Z");
        assert_eq!(json["description"], "A meeting");
        assert_eq!(json["location"], "Room 1");
    }

    #[test]
    fn unfold_ical_lines_joins_continuation_lines() {
        let folded = "DESCRIPTION:This is a long\r\n  description that spans\r\n  multiple lines";
        let result = unfold_ical_lines(folded);
        assert_eq!(
            result,
            "DESCRIPTION:This is a long description that spans multiple lines"
        );
    }

    #[test]
    fn unfold_ical_lines_preserves_unfolded_content() {
        let plain = "UID:abc-123\nSUMMARY:Test Event\nDTSTART:20260320T100000Z";
        let result = unfold_ical_lines(plain);
        assert_eq!(result, plain);
    }

    #[test]
    fn unfold_ical_lines_handles_tab_continuation() {
        let folded = "SUMMARY:Tab\r\n\tcontinued line";
        let result = unfold_ical_lines(folded);
        assert_eq!(result, "SUMMARY:Tabcontinued line");
    }

    #[test]
    fn ical_event_parses_folded_lines() {
        let ical = "BEGIN:VEVENT\r\nUID:fold-001\r\nSUMMARY:Folded\r\n  Summary Value\r\nDTSTART:20260320T100000Z\r\nDESCRIPTION:A very long\r\n  description field\r\nEND:VEVENT";
        let event = parse_ical_event(ical);
        assert_eq!(event.uid, "fold-001");
        assert_eq!(event.summary, "Folded Summary Value");
        assert_eq!(event.description, "A very long description field");
    }

    #[test]
    fn ical_event_unescapes_text_values() {
        let ical = "BEGIN:VEVENT\r\nUID:esc-001\r\nSUMMARY:Quarterly\\, Review\\; Backslash\\\\Test\r\nDESCRIPTION:Line one\\nLine two\r\nLOCATION:Room\\, 1\\; A\\\\B\r\nEND:VEVENT";
        let event = parse_ical_event(ical);
        assert_eq!(event.summary, "Quarterly, Review; Backslash\\Test");
        assert_eq!(event.description, "Line one\nLine two");
        assert_eq!(event.location, "Room, 1; A\\B");
    }
}
