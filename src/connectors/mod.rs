pub mod dav_common;
pub mod fm_caldav;
pub mod fm_carddav;
pub mod jmap;
pub mod matrix;
pub mod registry;
pub mod slack;
pub mod trello;

pub fn build_registry(config: &crate::config::Config) -> registry::ConnectorRegistry {
    let mut reg = registry::ConnectorRegistry::new();
    reg.register(Box::new(trello::TrelloConnectorProvider::new(
        config.trello.clone(),
    )));
    reg.register(Box::new(jmap::JmapConnectorProvider::new(
        config.fm_jmap.clone(),
    )));
    reg.register(Box::new(slack::SlackConnectorProvider::new(
        config.slack.clone(),
    )));
    reg.register(Box::new(fm_caldav::FmCalDavConnectorProvider::new(
        config.fm_caldav.clone(),
    )));
    reg.register(Box::new(fm_carddav::FmCardDavConnectorProvider::new(
        config.fm_carddav.clone(),
    )));
    reg
}
