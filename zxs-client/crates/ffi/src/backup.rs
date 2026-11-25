use std::{collections::BTreeSet, path::Path};

use serde_json::{Value, json};
use zxs_core::{
    Error, FileName, Filter, FolderPath, NewContainer, Remote, Result, UploadFile, invalid,
};

pub fn save(
    remote: &Remote,
    file: &Path,
    original: &FileName,
    folder: &FolderPath,
) -> Result<Value> {
    let family = Names::new(original);
    let mut last_conflict = None;
    loop {
        let mut hashed = UploadFile::open(file)?.with_digest()?;
        let unused = match find_target(remote, folder, &family, &hashed)? {
            Target::Present(value) => return Ok(value),
            Target::Available(name) => name,
        };
        if let Some((attempted, error)) = last_conflict.take()
            && attempted == unused
        {
            return Err(error);
        }
        let result = remote.create(
            &NewContainer {
                folder: folder.clone(),
                name: unused.clone(),
                tags: Vec::new(),
            },
            &mut hashed,
        );
        match result {
            Ok(commit) => return Ok(serde_json::to_value(commit)?),
            Err(error @ Error::Remote { status: 409, .. }) => last_conflict = Some((unused, error)),
            Err(error) => return Err(error),
        }
    }
}

enum Target {
    Present(Value),
    Available(FileName),
}

fn find_target(
    remote: &Remote,
    folder: &FolderPath,
    names: &Names<'_>,
    local: &UploadFile,
) -> Result<Target> {
    let mut occupied = BTreeSet::new();
    match remote.folders(folder) {
        Ok(page) => {
            for entry in page.folders {
                if let Some(index) = entry
                    .folder
                    .as_str()
                    .rsplit('/')
                    .next()
                    .and_then(|name| names.index(name))
                {
                    occupied.insert(index);
                }
            }
        }
        Err(Error::Remote { status: 404, .. }) => {}
        Err(error) => return Err(error),
    }
    let mut filter = Filter {
        folder: Some(folder.clone()),
        q: Some(prefix(names.stem, 32).into()),
        ..Filter::default()
    };
    loop {
        let page = remote.list(&filter)?;
        for entry in page.containers {
            let Some(index) = names.index(entry.name.as_str()) else {
                continue;
            };
            occupied.insert(index);
            if entry.latest_blake3.as_ref() == local.hash.as_ref() {
                return Ok(Target::Present(
                    json!({"already_present": true, "container": entry}),
                ));
            }
        }
        let Some(cursor) = page.next_cursor else {
            break;
        };
        filter.cursor = Some(cursor);
    }
    let mut next = 1u32;
    for index in occupied {
        if index != next {
            break;
        }
        next = next
            .checked_add(1)
            .ok_or_else(|| invalid("no free backup file name"))?;
    }
    Ok(Target::Available(names.candidate(next)?))
}

struct Names<'a> {
    original: &'a FileName,
    stem: &'a str,
    extension: &'a str,
}

impl<'a> Names<'a> {
    fn new(original: &'a FileName) -> Self {
        let (stem, extension) = split(original.as_str());
        Self {
            original,
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

    fn index(&self, name: &str) -> Option<u32> {
        if name.to_lowercase() == self.original.key() {
            return Some(1);
        }
        let key = name.to_lowercase();
        let extension = self.extension.to_lowercase();
        let stem = key.strip_suffix(&extension)?;
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
    let mut end = limit.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
