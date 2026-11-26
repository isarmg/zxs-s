mod apply;
mod archive;
pub(crate) mod header;
mod pack;
mod remote;
mod rules;
#[cfg(test)]
mod tests;
mod worktree;

pub use apply::{Checkout, plan_bundle, plan_retirement};
pub use archive::Bundle;
pub use pack::pack;
pub use remote::RemoteBundle;
pub use rules::{collect, init_bundle};
pub use worktree::{BundleInstall, apply_bundle, remove_empty, retain_members};

pub fn zip_stem(name: &str) -> Option<&str> {
    let (stem, suffix) = name.rsplit_once('.')?;
    suffix.eq_ignore_ascii_case("zip").then_some(stem)
}
