#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("zxs-client requires x86_64 or aarch64");

mod domain;
mod service;
mod store;
mod transport;
mod zip_bundle;

pub use domain::{
    Blake3Hash, Commit, Container, ContainerId, ContainerPage, Detail, Error, FileName, Filter,
    FolderPath, METADATA_LIMIT, NewContainer, OpId, Patch, Receipt, RecycleFilter, RecycleId,
    Restore, Result, Session, SessionToken, Version, VersionNum, invalid, media_type,
    portable_component,
};
pub use service::{
    Checkout, CheckoutRequest, CheckoutResult, CommitOutcome, CommitRequest, ConflictChoice,
    Workspace, init_bundle,
};
pub use store::{Profile, new_op, random_hex};
pub use transport::{HashedFile, Payload, PayloadRequest, Remote};
