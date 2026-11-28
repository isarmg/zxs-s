use std::collections::BTreeSet;

use serde::Deserialize;
use zxs_core::{
    Error, FileName, Filter, FolderPath, NewContainer, Remote, Result, UploadFile, invalid,
};

use super::upload::{self, Outcome};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Photo {
    name: String,
    folder: FolderPath,
}

impl Photo {
    pub(super) fn upload(self, remote: &Remote, file: &mut UploadFile) -> Result<Outcome> {
        let Ok(name) = FileName::parse(self.name) else {
            return Ok(upload::invalid_name());
        };
        let names = Names::new(&name);
        let mut occupied = match remote.folders(&self.folder) {
            Ok(page) => page
                .folders
                .iter()
                .filter_map(|folder| {
                    let (_, name) = folder.folder.as_str().rsplit_once('/')?;
                    names.index(&FileName::parse(name).ok()?.key())
                })
                .collect(),
            Err(Error::Remote { status: 404, .. }) => BTreeSet::new(),
            Err(error) => return Err(error),
        };
        let mut filter = Filter {
            folder: Some(self.folder.clone()),
            q: Some(names.query()?),
            limit: Some(1000),
            ..Filter::default()
        };
        loop {
            let page = remote.list(&filter)?;
            for current in page.containers {
                if let Some(index) = names.index(&current.name.key()) {
                    if file.matches_digest(&current.latest_blake3)? {
                        let Some(current) = remote.find_name(&self.folder, &current.name)? else {
                            continue;
                        };
                        if file.matches_digest(&current.latest_blake3)? {
                            return Ok(Outcome::AlreadyPresent { current });
                        }
                    }
                    occupied.insert(index);
                }
            }
            let Some(cursor) = page.next_cursor else {
                break;
            };
            filter.cursor = Some(cursor);
        }
        let mut index = 1;
        loop {
            if !occupied.contains(&index) {
                let params = NewContainer {
                    folder: self.folder.clone(),
                    name: names.candidate(index)?,
                    tags: Vec::new(),
                };
                let outcome = upload::create(remote, file, &params)?;
                if !matches!(outcome, Outcome::Conflict { .. }) {
                    return Ok(outcome);
                }
            }
            index = index
                .checked_add(1)
                .ok_or_else(|| invalid("no free backup file name"))?;
        }
    }
}

struct Names<'a> {
    original: &'a FileName,
    key: String,
    stem: &'a str,
    extension: &'a str,
}

impl<'a> Names<'a> {
    fn new(original: &'a FileName) -> Self {
        let (stem, extension) = split(original.as_str());
        Self {
            original,
            key: original.key(),
            stem,
            extension,
        }
    }

    fn candidate(&self, index: u32) -> Result<FileName> {
        if index == 1 {
            return Ok(self.original.clone());
        }
        let number = format!(" ({index})");
        let stem = prefix(self.stem, 255 - self.extension.len() - number.len());
        FileName::parse(format!("{stem}{number}{}", self.extension))
    }

    fn query(&self) -> Result<String> {
        let stem = split(&self.key).0;
        // A numbered name can have a shorter stem and a different Unicode lowercase form.
        let numbered = self.candidate(u32::MAX)?.key();
        let shared = stem
            .bytes()
            .zip(numbered.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        Ok(prefix(stem, shared.min(32)).into())
    }

    fn index(&self, key: &str) -> Option<u32> {
        if key == self.key {
            return Some(1);
        }
        let stem = if self.extension.is_empty() {
            key
        } else {
            key.rsplit_once('.')?.0
        };
        let (_, number) = stem.strip_suffix(')')?.rsplit_once(" (")?;
        let index = number.parse::<u32>().ok()?;
        (index > 1 && self.candidate(index).ok()?.key() == key).then_some(index)
    }
}

fn split(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 && name.len() - dot <= 210 => name.split_at(dot),
        _ => (name, ""),
    }
}

fn prefix(text: &str, limit: usize) -> &str {
    &text[..text.floor_char_boundary(limit)]
}
