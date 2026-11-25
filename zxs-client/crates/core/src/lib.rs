#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("zxs-client requires x86_64 or aarch64");

mod domain;
mod service;
mod store;
mod transport;
mod zip_bundle;

pub use domain::{
    Blake3Hash, Commit, Container, ContainerId, ContainerPage, Detail, Error, FileName, FileState,
    Filter, Folder, FolderMove, FolderPath, LocalPath, METADATA_LIMIT, NewContainer, Patch,
    RecycleFilter, RecycleId, Restore, Result, Session, SessionToken, Version, VersionNum, invalid,
    media_type,
};
pub use service::{
    Catalog, Checkout, CheckoutRequest, CheckoutResult, CommitOutcome, CommitRequest,
    ConflictChoice, SyncReport, Workspace,
};
pub use store::{Profile, random_hex};
pub use transport::{Payload, PayloadRequest, Remote, UploadFile};
