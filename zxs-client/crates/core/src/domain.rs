mod local;
mod model;
mod net;
mod types;
pub(crate) use types::file_name_key;

pub use local::*;
pub use model::*;
pub use net::media_type;
pub use types::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("symbolic links are not part of a workspace: {0}")]
    SymbolicLink(std::path::PathBuf),
    #[error("HTTP {status}: {message}")]
    Remote { status: u16, message: String },
    #[error("HTTP 401: {0}")]
    PasswordRejected(String),
    #[error("{0}")]
    LocalConflict(String),
    #[error("HTTP 409: {0}")]
    CommitConflict(String),
    #[error("本地内容需要先提交或备份后才能检出")]
    CheckoutConflict,
    #[error("上次检出中断，请核对或备份改动后重新检出")]
    CheckoutInterrupted,
    #[error("下载已取消")]
    DownloadCancelled,
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
            Self::Invalid(_) | Self::SymbolicLink(_) => 400,
            Self::Remote { status, .. } => *status,
            Self::PasswordRejected(_) => 401,
            Self::LocalConflict(_)
            | Self::CommitConflict(_)
            | Self::CheckoutConflict
            | Self::CheckoutInterrupted => 409,
            Self::DownloadCancelled => 499,
            Self::Http(_) => 502,
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
