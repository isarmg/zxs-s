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
    ConflictChoice, SyncReport, UploadConflict, UploadLocation, Workspace,
};
pub use store::digest::file as file_digest;
pub use store::workspace_location::{configured_workspace, select_workspace};
pub use store::{Profile, random_hex};
pub use transport::{
    Payload, PayloadRequest, Remote, TransferDirection, TransferObserver, TransferProgress,
    UploadFile,
};
pub use zip_bundle::{LocalBundle, zip_stem};
