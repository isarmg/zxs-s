use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::{
    domain::{EntryKind, LocalPath, Member, Result, Stamp, invalid},
    store::layout::{IGNORE_FILE, WORKSPACE_DIR, kind, read_small},
};

pub struct Rules(Gitignore);

impl Rules {
    pub fn from_file(root: &Path) -> Result<Self> {
        let file = root.join(IGNORE_FILE);
        match kind(&file)? {
            Some(EntryKind::File) => Self::from_bytes(&read_small(&file)?),
            Some(EntryKind::Directory) => Err(invalid(".zxsignore must be a regular file")),
            None => Ok(Self(Gitignore::empty())),
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(bytes).map_err(|_| invalid(".zxsignore must be UTF-8"))?;
        let mut builder = GitignoreBuilder::new("");
        for line in text.lines() {
            builder
                .add_line(None, line)
                .map_err(|error| invalid(format!(".zxsignore: {error}")))?;
        }
        builder
            .build()
            .map(Self)
            .map_err(|error| invalid(format!(".zxsignore: {error}")))
    }

    pub fn ignores(&self, path: &LocalPath, is_dir: bool) -> bool {
        if path.as_str() == IGNORE_FILE {
            return false;
        }
        if path.is_root() {
            return false;
        }
        self.0.matched(Path::new(path.as_str()), is_dir).is_ignore()
            || path
                .parent()
                .into_iter()
                .any(|parent| self.ignores(&parent, true))
    }
}

/// # Errors
///
/// Returns an error if the target is unavailable or already contains a .zxsignore file.
pub fn init_bundle(root: &Path) -> Result<()> {
    if kind(root)? != Some(EntryKind::Directory) {
        return Err(invalid("init-bundle requires an existing directory"));
    }
    let path = root.join(IGNORE_FILE);
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(b"# Gitignore syntax. Only these rules control the ZXS bundle.\n.git/\n.zxs/\ntarget/\nnode_modules/\n.DS_Store\n")?;
    file.sync_all()?;
    Ok(())
}

pub fn collect(root: &Path) -> Result<Vec<Member>> {
    if kind(&root.join(IGNORE_FILE))? != Some(EntryKind::File) {
        return Err(invalid("a bundle requires a root .zxsignore file"));
    }
    let rules = Rules::from_file(root)?;
    let mut members = Vec::new();
    scan(root, &LocalPath::parse(".")?, &rules, &mut members)?;
    members.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    let mut keys = HashSet::new();
    if members
        .iter()
        .any(|item| !keys.insert(item.path.as_str().to_lowercase()))
    {
        return Err(invalid(
            "bundle contains paths that differ only by letter case",
        ));
    }
    Ok(members)
}

fn scan(root: &Path, current: &LocalPath, rules: &Rules, members: &mut Vec<Member>) -> Result<()> {
    for entry in fs::read_dir(root.join(current.as_str()))? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("bundle path is not UTF-8"))?;
        let file_type = entry.file_type()?;
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
        if current.is_root() && name.eq_ignore_ascii_case(WORKSPACE_DIR) {
            return Err(invalid(
                "exclude the workspace .zxs directory in .zxsignore",
            ));
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
    let raw = Path::new(current.as_str()).join(name);
    rules.0.matched(raw, dir).is_ignore()
}
