use thiserror::Error;

#[derive(Error, Debug)]
pub enum KleviathanError {
    #[error("Configuration error: {0}")]
    Config(String),
    #[error("Matrix connector error: {0}")]
    Matrix(String),
    #[error("Trello connector error: {0}")]
    Trello(String),
    #[error("JMAP connector error: {0}")]
    Jmap(String),
    #[error("Slack connector error: {0}")]
    Slack(String),
    #[error("CalDAV connector error: {0}")]
    CalDav(String),
    #[error("CardDAV connector error: {0}")]
    CardDav(String),
    #[error("LLM provider error: {0}")]
    Llm(String),
    #[error("Rate limit exceeded: {0}")]
    RateLimit(String),
    #[error("Abuse detected: {0}")]
    AbuseDetected(String),
    #[error("Code injection detected: {0}")]
    InjectionDetected(String),
    #[error("Task graph error: {0}")]
    TaskGraph(String),
    #[error("Container enforcement error: {0}")]
    NotInContainer(String),
    #[error("Docker error: {0}")]
    Docker(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type KleviathanResult<T> = Result<T, KleviathanError>;
