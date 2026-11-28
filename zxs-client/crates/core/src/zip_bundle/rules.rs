use std::{
    collections::HashSet,
    fs::{self, File},
    io::Write,
    mem::DropGuard,
    path::Path,
};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use unicode_normalization::UnicodeNormalization;

use crate::{
    domain::{EntryKind, Error, LocalPath, Member, Result, Stamp, invalid},
    store::layout::{IGNORE_FILE, WORKSPACE_DIR, kind, move_new, read_small},
};

pub struct Rules(Gitignore, blake3::Hash);

impl Rules {
    pub fn from_file(root: &Path) -> Result<Self> {
        let file = root.join(IGNORE_FILE);
        match kind(&file)? {
            Some(EntryKind::File) => Self::from_bytes(&read_small(&file)?),
            Some(EntryKind::Directory) => Err(invalid(".zxsignore must be a regular file")),
            None => Self::from_bytes(&[]),
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(bytes).map_err(|_| invalid(".zxsignore must be UTF-8"))?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut builder = GitignoreBuilder::new("");
        for line in text.nfc().collect::<String>().lines() {
            builder
                .add_line(None, line)
                .map_err(|error| invalid(format!(".zxsignore: {error}")))?;
        }
        builder
            .build()
            .map(|rules| Self(rules, blake3::hash(bytes)))
            .map_err(|error| invalid(format!(".zxsignore: {error}")))
    }

    pub(super) fn check_replacement(&self, root: &Path, next: &Self) -> Result<()> {
        if self.0.num_ignores() != 0
            && self.1 != next.1
            && let Some(member) = inventory(root, next)?
                .into_iter()
                .find(|member| self.ignores(&member.path, member.kind == EntryKind::Directory))
        {
            return Err(Error::LocalConflict(format!(
                "新忽略规则将纳入本地文件：{}",
                member.path.as_str()
            )));
        }
        Ok(())
    }

    pub fn ignores(&self, path: &LocalPath, is_dir: bool) -> bool {
        if path.as_str() == IGNORE_FILE || path.is_root() {
            return false;
        }
        self.0.matched(Path::new(path.as_str()), is_dir).is_ignore()
            || path
                .parent()
                .is_some_and(|parent| self.ignores(&parent, true))
    }
}

/// # Errors
///
/// Returns an error if the target is unavailable or already contains a .zxsignore file.
pub fn init_bundle(root: &Path, destination: &Path) -> Result<()> {
    if kind(root)? != Some(EntryKind::Directory) {
        return Err(invalid("init-bundle requires an existing directory"));
    }
    let manifest = root.join(IGNORE_FILE);
    let mut file = File::create_new(&manifest)?;
    let created = DropGuard::new(manifest, |path| drop(fs::remove_file(path)));
    file.write_all(b"# Gitignore syntax. Only these rules control the ZXS bundle.\n.git/\n.zxs/\ntarget/\nnode_modules/\n.DS_Store\n")?;
    file.sync_all()?;
    drop(file);
    if root != destination {
        move_new(root, destination)?;
    }
    drop(DropGuard::dismiss(created));
    Ok(())
}

pub fn collect(root: &Path) -> Result<Vec<Member>> {
    let manifest = root.join(IGNORE_FILE);
    if kind(&manifest)? != Some(EntryKind::File) {
        return Err(invalid("a bundle requires a root .zxsignore file"));
    }
    inventory(root, &Rules::from_bytes(&read_small(&manifest)?)?)
}

pub fn collect_backup(root: &Path) -> Result<Vec<Member>> {
    inventory(root, &Rules::from_file(root)?)
}

fn inventory(root: &Path, rules: &Rules) -> Result<Vec<Member>> {
    let mut members = Vec::new();
    scan(root, &LocalPath::parse(".")?, rules, &mut members)?;
    members.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    let mut keys = HashSet::new();
    if members.iter().any(|item| !keys.insert(item.path.key())) {
        return Err(invalid(
            "bundle contains paths that differ only by letter case",
        ));
    }
    Ok(members)
}

fn scan(root: &Path, current: &LocalPath, rules: &Rules, members: &mut Vec<Member>) -> Result<()> {
    for entry in fs::read_dir(crate::store::layout::checked_join(root, current)?)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("bundle path is not UTF-8"))?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || name.eq_ignore_ascii_case(WORKSPACE_DIR) {
            continue;
        }
        let relative = match current.child(&name) {
            Ok(path) => path,
            Err(error) => {
                if !name.contains('\\') && raw_ignored(rules, current, &name, file_type.is_dir()) {
                    continue;
                }
                return Err(error);
            }
        };
        if rules.ignores(&relative, file_type.is_dir()) {
            continue;
        }
        if file_type.is_dir() {
            members.push(Member {
                path: relative.clone(),
                kind: EntryKind::Directory,
                stamp: None,
            });
            scan(root, &relative, rules, members)?;
        } else if file_type.is_file() {
            members.push(Member {
                path: relative,
                kind: EntryKind::File,
                stamp: Some(Stamp::from_metadata(&entry.metadata()?)?),
            });
        } else {
            return Err(invalid(format!(
                "unsupported bundle entry {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn raw_ignored(rules: &Rules, current: &LocalPath, name: &str, dir: bool) -> bool {
    let raw = Path::new(current.as_str()).join(name.nfc().collect::<String>());
    rules.0.matched(raw, dir).is_ignore()
}
