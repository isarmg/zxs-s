mod model;
mod net;
mod types;

pub use model::*;
pub use net::{check_address, media_type};
pub use types::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("authentication required")]
    Unauthorized,
    #[error("{0}")]
    Missing(&'static str),
    #[error("{0}")]
    Conflict(String),
    #[error("operation ID was already used with different parameters")]
    Reused,
    #[error("storage requires recovery before serving requests")]
    Recovery,
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("index: {0}")]
    Index(#[from] rusqlite::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("configuration: {0}")]
    Config(String),
    #[error("background task: {0}")]
    Task(#[from] tokio::task::JoinError),
}

pub type Result<T> = std::result::Result<T, Error>;
pub const BUFFER: usize = 4 * 1024 * 1024;
pub const METADATA_LIMIT: usize = 1024 * 1024;

pub fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
