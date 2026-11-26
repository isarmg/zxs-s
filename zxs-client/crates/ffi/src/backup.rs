use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Mutex,
};

use serde::Deserialize;
use serde_json::{Value, json};
use zxs_core::{
    Container, Error, FileName, Filter, FolderPath, NewContainer, Remote, Result, UploadFile,
    invalid,
};

use super::Connection;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Backup {
    connection: Connection,
    file: PathBuf,
    name: FileName,
    folder: FolderPath,
    batch: Option<String>,
}

struct Batch {
    connection: Connection,
    id: String,
    inventory: Inventory,
}

static BATCH: Mutex<Option<Batch>> = Mutex::new(None);

pub(super) fn close_batch() -> Result<()> {
    *BATCH.lock().map_err(|_| invalid("相册备份不可用"))? = None;
    Ok(())
}

impl Backup {
    pub(super) fn run(self) -> Result<Value> {
        let remote = self.connection.open()?;
        let mut file = UploadFile::open(&self.file)?.with_digest()?;
        let names = Names::new(&self.name);
        let Some(id) = self.batch else {
            let mut inventory = Inventory::load(&remote, self.folder, false)?;
            return save(&remote, &mut file, &names, &mut inventory);
        };
        let mut batch = BATCH.lock().map_err(|_| invalid("相册备份不可用"))?;
        if batch.as_ref().is_none_or(|active| {
            active.id != id
                || active.connection != self.connection
                || active.inventory.folder != self.folder
        }) {
            *batch = Some(Batch {
                connection: self.connection,
                id,
                inventory: Inventory::load(&remote, self.folder, true)?,
            });
        }
        let active = batch.as_mut().ok_or_else(|| invalid("相册备份不可用"))?;
        save(&remote, &mut file, &names, &mut active.inventory)
    }
}

fn save(
    remote: &Remote,
    file: &mut UploadFile,
    names: &Names<'_>,
    inventory: &mut Inventory,
) -> Result<Value> {
    inventory.select(remote, names)?;
    let mut last_conflict = None;
    loop {
        let unused = match inventory.target(names, file)? {
            Target::Present(entry) => {
                if let Some(current) = inventory.confirm(remote, entry)? {
                    return Ok(json!({"already_present": true, "container": current}));
                }
                continue;
            }
            Target::Available(name) => name,
        };
        if let Some((attempted, error)) = last_conflict.take()
            && attempted == unused
        {
            return Err(error);
        }
        let result = remote.create(
            &NewContainer {
                folder: inventory.folder.clone(),
                name: unused.clone(),
                tags: Vec::new(),
            },
            file,
        );
        match result {
            Ok(commit) => {
                inventory
                    .entries
                    .insert(commit.container.name.key(), Some(commit.container.clone()));
                return Ok(serde_json::to_value(commit)?);
            }
            Err(error @ Error::Remote { status: 409, .. }) => {
                *inventory = Inventory::load(remote, inventory.folder.clone(), inventory.complete)?;
                inventory.select(remote, names)?;
                last_conflict = Some((unused, error));
            }
            Err(error) => return Err(error),
        }
    }
}

enum Target {
    Present(Container),
    Available(FileName),
}

struct Inventory {
    folder: FolderPath,
    complete: bool,
    entries: BTreeMap<String, Option<Container>>,
}

impl Inventory {
    fn load(remote: &Remote, folder: FolderPath, batch: bool) -> Result<Self> {
        let mut inventory = Self {
            folder,
            complete: false,
            entries: BTreeMap::new(),
        };
        match remote.folders(&inventory.folder) {
            Ok(page) => {
                for entry in page.folders {
                    let name = entry.folder.as_str().rsplit('/').next().unwrap_or_default();
                    inventory.entries.insert(FileName::parse(name)?.key(), None);
                }
            }
            Err(Error::Remote { status: 404, .. }) => {}
            Err(error) => return Err(error),
        }
        if batch {
            let page = remote.list(&Filter {
                folder: Some(inventory.folder.clone()),
                ..Filter::default()
            })?;
            if page.next_cursor.is_none() {
                inventory.complete = true;
                inventory.add_containers(page.containers);
            }
        }
        Ok(inventory)
    }

    fn select(&mut self, remote: &Remote, names: &Names<'_>) -> Result<()> {
        if self.complete {
            return Ok(());
        }
        self.entries.retain(|_, entry| entry.is_none());
        let mut filter = Filter {
            folder: Some(self.folder.clone()),
            q: Some(names.query()),
            limit: Some(1000),
            ..Filter::default()
        };
        loop {
            let page = remote.list(&filter)?;
            self.add_containers(page.containers);
            let Some(cursor) = page.next_cursor else {
                break;
            };
            filter.cursor = Some(cursor);
        }
        Ok(())
    }

    fn add_containers(&mut self, containers: Vec<Container>) {
        self.entries.extend(
            containers
                .into_iter()
                .map(|entry| (entry.name.key(), Some(entry))),
        );
    }

    fn target(&self, names: &Names<'_>, local: &UploadFile) -> Result<Target> {
        let prefix = names.query();
        let candidates = self
            .entries
            .range(prefix.clone()..)
            .take_while(|(key, _)| key.starts_with(&prefix));
        let mut occupied = BTreeSet::new();
        for (key, entry) in candidates {
            if let Some(index) = names.index(key) {
                occupied.insert(index);
                if let Some(entry) = entry
                    && Some(&entry.latest_blake3) == local.hash.as_ref()
                {
                    return Ok(Target::Present(entry.clone()));
                }
            }
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

    fn confirm(&mut self, remote: &Remote, previous: Container) -> Result<Option<Container>> {
        if !self.complete {
            return Ok(Some(previous));
        }
        let mut latest = remote.lookup_containers(std::iter::once(&previous.container_id))?;
        self.entries.remove(&previous.name.key());
        let Some(current) = latest
            .remove(&previous.container_id)
            .filter(|current| current.folder == self.folder)
        else {
            return Ok(None);
        };
        let name = current.name.key();
        self.entries.insert(name.clone(), Some(current.clone()));
        Ok(
            (name == previous.name.key() && current.latest_blake3 == previous.latest_blake3)
                .then_some(current),
        )
    }
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

    fn query(&self) -> String {
        prefix(split(&self.original.key()).0, 32).into()
    }

    fn index(&self, key: &str) -> Option<u32> {
        if key == self.original.key() {
            return Some(1);
        }
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
