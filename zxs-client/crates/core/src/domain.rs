mod local;
mod model;
mod net;
mod types;

pub use local::*;
pub use model::*;
pub(crate) use net::check_address;
pub use net::media_type;
pub use types::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("HTTP {status}: {message}")]
    Remote { status: u16, message: String },
    #[error("operation {op} has no confirmed response: {source}")]
    Uncertain {
        op: OpId,
        #[source]
        source: Box<Error>,
    },
    #[error("{0}")]
    LocalConflict(String),
    #[error("HTTP 409: {0}")]
    CommitConflict(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("workspace index: {0}")]
    Index(#[from] rusqlite::Error),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("HTTP: {0}")]
    Http(#[from] Box<ureq::Error>),
    #[error("ZIP: {0}")]
    Zip(#[from] zip::result::ZipError),
}

impl Error {
    #[must_use]
    pub fn status(&self) -> u16 {
        match self {
            Self::Invalid(_) => 400,
            Self::Remote { status, .. } => *status,
            Self::LocalConflict(_) | Self::CommitConflict(_) => 409,
            Self::Uncertain { .. } | Self::Http(_) => 502,
            _ => 500,
        }
    }
}

impl From<ureq::Error> for Error {
    fn from(error: ureq::Error) -> Self {
        Self::Http(Box::new(error))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
pub const BUFFER: usize = 4 * 1024 * 1024;
pub const METADATA_LIMIT: usize = 1024 * 1024;

pub fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
