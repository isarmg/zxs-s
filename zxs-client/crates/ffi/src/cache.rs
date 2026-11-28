use std::{
    fs::{self, OpenOptions},
    io::{self, Read},
    mem::DropGuard,
    path::{Path, PathBuf},
    time::SystemTime,
};

use zxs_core::{
    Blake3Hash, ContainerId, FileName, Remote, Result, Version, VersionNum, file_digest,
};

const MAX_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FILES: usize = 64;

pub(super) fn document(
    remote: &Remote,
    root: &Path,
    id: &ContainerId,
    version: &Version,
    name: &FileName,
) -> Result<PathBuf> {
    let key = format!("{id}-{}-{}", version.version, version.blake3);
    let expected = (version.size, Some(&version.blake3));
    materialize(root, &key, name, expected, |path| {
        remote.download_new(id, version, path)
    })
}

pub(super) fn materialize(
    root: &Path,
    key: &str,
    name: &FileName,
    (size, digest): (u64, Option<&Blake3Hash>),
    write: impl FnOnce(&Path) -> Result<()>,
) -> Result<PathBuf> {
    fs::create_dir_all(root)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(".lock"))?;
    lock.lock()?;
    let stage_path = root.join(".download");
    remove_directory(&stage_path)?;
    let directory = root.join(key);
    let path = directory.join(name.as_str());
    let reusable = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && metadata.len() == size => {
            let actual = file_digest(&path)?;
            match digest {
                Some(expected) => actual == *expected,
                None => fs::File::open(directory.join(".zxs"))
                    .and_then(|file| io::read_to_string(file.take(65)))
                    .is_ok_and(|stored| stored == actual.as_str()),
            }
        }
        Ok(_) => false,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    if !reusable {
        fs::create_dir(&stage_path)?;
        let stage = DropGuard::new(stage_path, |path| {
            let _ = remove_directory(&path);
        });
        let staged = stage.join(name.as_str());
        write(&staged)?;
        if digest.is_none() {
            fs::write(stage.join(".zxs"), file_digest(&staged)?.as_str())?;
        }
        remove_directory(&directory)?;
        fs::rename(stage.as_path(), &directory)?;
        drop(DropGuard::dismiss(stage));
    }
    OpenOptions::new()
        .write(true)
        .open(&path)?
        .set_modified(SystemTime::now())?;
    prune(root, &directory, MAX_BYTES, MAX_FILES)?;
    Ok(path)
}

fn remove_directory(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

struct Entry {
    directory: PathBuf,
    size: u64,
    last_used: SystemTime,
}

fn prune(root: &Path, keep: &Path, byte_limit: u64, file_limit: usize) -> Result<()> {
    let mut entries = Vec::new();
    let mut size = 0_u64;
    for directory in fs::read_dir(root)? {
        let directory = directory?;
        let name = directory.file_name();
        if name.to_str().is_some_and(cache_key) && directory.file_type()?.is_dir() {
            let entry = measure(directory.path())?;
            size = size.saturating_add(entry.size);
            entries.push(entry);
        }
    }
    let mut count = entries.len();
    entries.sort_unstable_by_key(|entry| entry.last_used);
    // The active preview may temporarily exceed the cache budget.
    for entry in entries.into_iter().filter(|entry| entry.directory != keep) {
        if size <= byte_limit && count <= file_limit {
            break;
        }
        remove_directory(&entry.directory)?;
        size = size.saturating_sub(entry.size);
        count -= 1;
    }
    Ok(())
}

fn measure(directory: PathBuf) -> Result<Entry> {
    let mut size = 0_u64;
    let mut last_used = SystemTime::UNIX_EPOCH;
    for file in fs::read_dir(&directory)? {
        let metadata = file?.metadata()?;
        if metadata.is_file() {
            size = size.saturating_add(metadata.len());
            last_used = last_used.max(metadata.modified()?);
        }
    }
    Ok(Entry {
        directory,
        size,
        last_used,
    })
}

fn cache_key(name: &str) -> bool {
    if let Some((id, rest)) = name.split_once('-')
        && let Some((number, hash)) = rest.split_once('-')
    {
        return ContainerId::parse(id).is_ok()
            && number.parse::<VersionNum>().is_ok()
            && Blake3Hash::parse(hash).is_ok();
    }
    false
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        path::{Path, PathBuf},
        time::{Duration, SystemTime},
    };

    use zxs_core::{Result, random_hex};

    use super::prune;

    struct TestDir(PathBuf);

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn entry(root: &Path, id: &str, bytes: &[u8], touched: u64) -> Result<PathBuf> {
        let directory = root.join(format!("{}-1-{}", id.repeat(32), "f".repeat(64)));
        fs::create_dir(&directory)?;
        let file = directory.join("document.txt");
        fs::write(&file, bytes)?;
        OpenOptions::new()
            .write(true)
            .open(file)?
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(touched))?;
        Ok(directory)
    }

    #[test]
    fn eviction_respects_byte_and_entry_limits() -> Result<()> {
        let root =
            TestDir(std::env::temp_dir().join(format!("zxs-cache-test-{}", random_hex::<16>()?)));
        fs::create_dir(&root.0)?;
        let oldest = entry(&root.0, "a", b"four", 1)?;
        let recent = entry(&root.0, "b", b"two", 2)?;
        let active = entry(&root.0, "c", b"x", 3)?;
        let unrelated = root.0.join("d".repeat(32));
        fs::create_dir(&unrelated)?;
        prune(&root.0, &active, 4, 2)?;
        assert!(!oldest.exists());
        assert!(recent.is_dir() && active.is_dir());
        prune(&root.0, &active, 4, 1)?;
        assert!(!recent.exists());
        assert!(active.is_dir() && unrelated.is_dir());
        Ok(())
    }
}
