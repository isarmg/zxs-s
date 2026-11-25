mod apply;
mod archive;
pub(crate) mod header;
mod pack;
mod rules;
#[cfg(test)]
mod tests;
mod worktree;

pub use apply::{Checkout, plan_bundle};
pub use archive::Bundle;
pub use pack::pack;
pub use rules::{collect, collect_backup, init_bundle};
pub use worktree::apply_bundle;

pub fn zip_stem(name: &str) -> Option<&str> {
    let (stem, suffix) = name.rsplit_once('.')?;
    suffix.eq_ignore_ascii_case("zip").then_some(stem)
}
