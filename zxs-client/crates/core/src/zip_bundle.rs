mod apply;
mod archive;
pub(crate) mod header;
mod pack;
mod rules;
#[cfg(test)]
mod tests;
mod worktree;

pub use apply::{Checkout, plan_bundle, plan_retirement};
pub use archive::{Bundle, LocalBundle};
pub use pack::pack;
pub use rules::{collect, init_bundle};
pub use worktree::{BundleInstall, apply_bundle, remove_empty, retain_members};

#[must_use]
pub fn zip_stem(name: &str) -> Option<&str> {
    let (stem, suffix) = name.rsplit_once('.')?;
    suffix.eq_ignore_ascii_case("zip").then_some(stem)
}

pub(crate) fn matches_bundle_stem(
    local: &crate::domain::LocalPath,
    published: &crate::domain::LocalPath,
) -> bool {
    zip_stem(published.as_str()) == Some(local.as_str())
}

pub(crate) fn bundle_name(
    name: &crate::domain::FileName,
) -> crate::domain::Result<crate::domain::FileName> {
    if zip_stem(name.as_str()).is_some() {
        return Ok(name.clone());
    }
    crate::domain::FileName::parse(format!("{name}.zip"))
}
