mod cursor;
mod model;
pub use cursor::{ContainerCursor, RecycleCursor};
mod net;
mod types;

pub use model::*;
pub use net::media_type;
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
    #[error("storage requires recovery before serving requests")]
    Recovery,
    #[error("no readable versions in {0}")]
    DamagedContainer(std::path::PathBuf),
    #[error("recycle storage is full")]
    RecycleFull,
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
