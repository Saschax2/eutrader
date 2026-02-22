use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Config error: {0}")]
    Config(String),

    #[error("Feed error: {0}")]
    Feed(String),

    #[error("Execution error: {0}")]
    Execution(String),

    #[error("Strategy error: {0}")]
    Strategy(String),

    #[error("Risk limit breached: {0}")]
    RiskBreach(String),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
