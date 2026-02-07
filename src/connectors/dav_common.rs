use crate::error::{KleviathanError, KleviathanResult};

pub const CALDAV_BASE_URL: &str = "https://caldav.fastmail.com";
pub const CARDDAV_BASE_URL: &str = "https://carddav.fastmail.com";

pub async fn propfind(
    client: &reqwest::Client,
    url: &str,
    username: &str,
    password: &str,
    body: &str,
    depth: &str,
) -> KleviathanResult<reqwest::Response> {
    let response = client
        .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), url)
        .header("Content-Type", "application/xml; charset=utf-8")
        .header("Depth", depth)
        .basic_auth(username, Some(password))
        .body(body.to_string())
        .send()
        .await
        .map_err(KleviathanError::Http)?;

    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(KleviathanError::RateLimit(
            "DAV server returned 429 Too Many Requests".into(),
        ));
    }

    Ok(response)
}

pub async fn report(
    client: &reqwest::Client,
    url: &str,
    username: &str,
    password: &str,
    body: &str,
) -> KleviathanResult<reqwest::Response> {
    let response = client
        .request(reqwest::Method::from_bytes(b"REPORT").unwrap(), url)
        .header("Content-Type", "application/xml; charset=utf-8")
        .header("Depth", "1")
        .basic_auth(username, Some(password))
        .body(body.to_string())
        .send()
        .await
        .map_err(KleviathanError::Http)?;

    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(KleviathanError::RateLimit(
            "DAV server returned 429 Too Many Requests".into(),
        ));
    }

    Ok(response)
}

pub async fn check_write_access(
    client: &reqwest::Client,
    url: &str,
    username: &str,
    password: &str,
) -> KleviathanResult<()> {
    let body = concat!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
        "<d:propfind xmlns:d=\"DAV:\">",
        "<d:prop>",
        "<d:current-user-privilege-set/>",
        "</d:prop>",
        "</d:propfind>"
    );

    let response = propfind(client, url, username, password, body, "0").await?;
    let status = response.status();

    if !status.is_success() && status.as_u16() != 207 {
        return Err(KleviathanError::Config(
            "DAV endpoint is not accessible. Check credentials and URL.".into(),
        ));
    }

    let xml = response
        .text()
        .await
        .map_err(|e| KleviathanError::Config(format!("Failed to read DAV response: {}", e)))?;

    if !has_write_privilege(&xml) {
        return Err(KleviathanError::Config(
            "DAV endpoint does not grant write access. Use an app password with read/write access.".into(),
        ));
    }

    Ok(())
}

fn has_write_privilege(xml: &str) -> bool {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

    let mut reader = Reader::from_str(xml);
    let mut in_privilege_set = false;
    let mut in_privilege = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                match local {
                    "current-user-privilege-set" => in_privilege_set = true,
                    "privilege" if in_privilege_set => in_privilege = true,
                    "write" | "write-content" if in_privilege => return true,
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                match local {
                    "current-user-privilege-set" => in_privilege_set = false,
                    "privilege" => in_privilege = false,
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }

    false
}

pub async fn verify_no_cross_access(
    client: &reqwest::Client,
    other_base_url: &str,
    username: &str,
    password: &str,
    own_protocol_name: &str,
    other_protocol_name: &str,
) -> KleviathanResult<()> {
    let url = format!("{}/dav/", other_base_url);
    let body = "<?xml version=\"1.0\" encoding=\"utf-8\"?><d:propfind xmlns:d=\"DAV:\"><d:prop><d:resourcetype/></d:prop></d:propfind>";

    let response = propfind(client, &url, username, password, body, "0").await?;
    let status = response.status();

    if status.is_success() || status.as_u16() == 207 {
        return Err(KleviathanError::Config(format!(
            "{} app password also has {} access. Use an app password with only {} access.",
            own_protocol_name, other_protocol_name, own_protocol_name
        )));
    }

    Ok(())
}

pub fn parse_multistatus_items(xml: &str, data_tag: &str) -> Vec<(String, String)> {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

    let mut reader = Reader::from_str(xml);
    let mut results = Vec::new();

    let mut in_response = false;
    let mut in_href = false;
    let mut in_data = false;
    let mut current_href = String::new();
    let mut current_data = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                if local == "response" {
                    in_response = true;
                    current_href.clear();
                    current_data.clear();
                } else if local == "href" && in_response {
                    in_href = true;
                } else if local == data_tag && in_response {
                    in_data = true;
                }
            }
            Ok(Event::Text(e)) => {
                let text = String::from_utf8_lossy(e.as_ref());
                if in_href {
                    current_href.push_str(&text);
                } else if in_data {
                    current_data.push_str(&text);
                }
            }
            Ok(Event::CData(e)) => {
                let text = String::from_utf8_lossy(e.as_ref());
                if in_href {
                    current_href.push_str(&text);
                } else if in_data {
                    current_data.push_str(&text);
                }
            }
            Ok(Event::End(ref e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                if local == "response" {
                    if in_response && !current_data.is_empty() {
                        results.push((current_href.clone(), current_data.clone()));
                    }
                    in_response = false;
                } else if local == "href" {
                    in_href = false;
                } else if local == data_tag {
                    in_data = false;
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }

    results
}

pub fn extract_id_segment(href: &str) -> String {
    href.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string()
}

pub fn normalize_resource_id(raw: &str) -> String {
    if raw.contains('/') {
        extract_id_segment(raw)
    } else {
        raw.to_string()
    }
}

fn local_name(full: &[u8]) -> &str {
    let s = std::str::from_utf8(full).unwrap_or("");
    s.rsplit_once(':').map(|(_, local)| local).unwrap_or(s)
}

pub fn handle_dav_response_status(
    status: reqwest::StatusCode,
    protocol: &str,
) -> KleviathanResult<()> {
    if status.is_success() || status.as_u16() == 207 {
        return Ok(());
    }

    let make_err = |msg: String| -> KleviathanError {
        match protocol {
            "CalDAV" => KleviathanError::CalDav(msg),
            "CardDAV" => KleviathanError::CardDav(msg),
            _ => KleviathanError::Config(msg),
        }
    };

    match status.as_u16() {
        401 => Err(make_err("Invalid credentials".into())),
        403 => Err(make_err("Insufficient permissions".into())),
        404 => Err(make_err("Resource not found".into())),
        429 => Err(KleviathanError::RateLimit(format!(
            "{} server returned 429 Too Many Requests",
            protocol
        ))),
        _ => Err(make_err(format!("HTTP error: {}", status))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_id_segment_from_trailing_slash() {
        assert_eq!(
            extract_id_segment("/dav/calendars/user/foo/default/"),
            "default"
        );
    }

    #[test]
    fn extract_id_segment_from_no_trailing_slash() {
        assert_eq!(
            extract_id_segment("/dav/calendars/user/foo/default"),
            "default"
        );
    }

    #[test]
    fn extract_id_segment_from_bare_name() {
        assert_eq!(extract_id_segment("default"), "default");
    }

    #[test]
    fn extract_id_segment_from_empty() {
        assert_eq!(extract_id_segment(""), "");
    }

    #[test]
    fn normalize_resource_id_passes_through_bare_id() {
        assert_eq!(normalize_resource_id("default"), "default");
    }

    #[test]
    fn normalize_resource_id_extracts_from_path() {
        assert_eq!(
            normalize_resource_id("/dav/calendars/user/foo/default/"),
            "default"
        );
    }

    #[test]
    fn normalize_resource_id_extracts_from_path_no_slash() {
        assert_eq!(
            normalize_resource_id("/dav/addressbooks/user/bar/contacts"),
            "contacts"
        );
    }

    #[test]
    fn parse_multistatus_items_handles_cdata() {
        let xml = concat!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
            "<d:multistatus xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\">",
            "<d:response>",
            "<d:href>/dav/calendars/user/test/default/event1.ics</d:href>",
            "<d:propstat><d:prop>",
            "<c:calendar-data><![CDATA[BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:cdata-1\r\nSUMMARY:CDATA Event\r\nEND:VEVENT\r\nEND:VCALENDAR]]></c:calendar-data>",
            "</d:prop></d:propstat>",
            "</d:response>",
            "</d:multistatus>"
        );
        let items = parse_multistatus_items(xml, "calendar-data");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].0, "/dav/calendars/user/test/default/event1.ics");
        assert!(items[0].1.contains("CDATA Event"));
    }

    #[test]
    fn parse_multistatus_items_handles_plain_text() {
        let xml = concat!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
            "<d:multistatus xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\">",
            "<d:response>",
            "<d:href>/dav/calendars/user/test/default/event2.ics</d:href>",
            "<d:propstat><d:prop>",
            "<c:calendar-data>BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:text-1\r\nSUMMARY:Plain Event\r\nEND:VEVENT\r\nEND:VCALENDAR</c:calendar-data>",
            "</d:prop></d:propstat>",
            "</d:response>",
            "</d:multistatus>"
        );
        let items = parse_multistatus_items(xml, "calendar-data");
        assert_eq!(items.len(), 1);
        assert!(items[0].1.contains("Plain Event"));
    }

    #[test]
    fn parse_multistatus_items_handles_mixed_cdata_and_text() {
        let xml = concat!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
            "<d:multistatus xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\">",
            "<d:response>",
            "<d:href>/dav/event1.ics</d:href>",
            "<d:propstat><d:prop>",
            "<c:calendar-data><![CDATA[BEGIN:VCALENDAR\r\nUID:cdata-ev\r\nEND:VCALENDAR]]></c:calendar-data>",
            "</d:prop></d:propstat>",
            "</d:response>",
            "<d:response>",
            "<d:href>/dav/event2.ics</d:href>",
            "<d:propstat><d:prop>",
            "<c:calendar-data>BEGIN:VCALENDAR\r\nUID:text-ev\r\nEND:VCALENDAR</c:calendar-data>",
            "</d:prop></d:propstat>",
            "</d:response>",
            "</d:multistatus>"
        );
        let items = parse_multistatus_items(xml, "calendar-data");
        assert_eq!(items.len(), 2);
        assert!(items[0].1.contains("cdata-ev"));
        assert!(items[1].1.contains("text-ev"));
    }

    #[test]
    fn parse_multistatus_items_works_for_carddav_cdata() {
        let xml = concat!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
            "<d:multistatus xmlns:d=\"DAV:\" xmlns:card=\"urn:ietf:params:xml:ns:carddav\">",
            "<d:response>",
            "<d:href>/dav/contacts/user/test/default/contact1.vcf</d:href>",
            "<d:propstat><d:prop>",
            "<card:address-data><![CDATA[BEGIN:VCARD\r\nFN:Jane Doe\r\nEND:VCARD]]></card:address-data>",
            "</d:prop></d:propstat>",
            "</d:response>",
            "</d:multistatus>"
        );
        let items = parse_multistatus_items(xml, "address-data");
        assert_eq!(items.len(), 1);
        assert!(items[0].1.contains("Jane Doe"));
    }
}
