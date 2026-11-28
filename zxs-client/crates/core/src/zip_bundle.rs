mod apply;
mod archive;
mod header;
mod pack;
mod rules;
#[cfg(test)]
mod tests;
mod worktree;

pub use apply::{Checkout, plan_bundle, plan_retirement};
pub use archive::{Bundle, LocalBundle};
pub use pack::pack;
pub use rules::{collect, init_bundle, manifest_path};
pub use worktree::{BundleInstall, apply_bundle, remove_empty, retain_members};

#[must_use]
pub fn zip_stem(name: &str) -> Option<&str> {
    let (stem, suffix) = name.rsplit_once('.')?;
    suffix.eq_ignore_ascii_case("zip").then_some(stem)
}

pub(crate) fn require_bundle_suffix(name: &str) -> crate::domain::Result<()> {
    zip_stem(name).map(|_| ()).ok_or_else(|| {
        crate::domain::Error::LocalConflict(format!("目录包名称须以 .zip 结尾，请检查「{name}」"))
    })
}

pub(crate) fn bundle_name(
    name: &crate::domain::FileName,
) -> crate::domain::Result<crate::domain::FileName> {
    if zip_stem(name.as_str()).is_some() {
        return Ok(name.clone());
    }
    crate::domain::FileName::parse(format!("{name}.zip"))
}
