use std::{
    fs::{File, OpenOptions},
    io::{Cursor, Seek, Write},
    path::Path,
    sync::LazyLock,
};

use rayon::prelude::*;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::{
    domain::{BUFFER, Blake3Hash, Error, Member, MemberHashes, Result, Stamp, invalid},
    store::{
        digest,
        layout::{IGNORE_FILE, checked_join},
    },
};

const SMALL: u64 = BUFFER as u64;
const WINDOW: u64 = 40 * 1024 * 1024;

static PACKERS: LazyLock<std::result::Result<rayon::ThreadPool, rayon::ThreadPoolBuildError>> =
    LazyLock::new(|| {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
        rayon::ThreadPoolBuilder::new().num_threads(threads).build()
    });

pub fn pack(root: &Path, members: &[Member], destination: &Path) -> Result<MemberHashes> {
    let output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)?;
    let mut writer = ZipWriter::new(output);
    let mut hashes = MemberHashes::new();
    if let Some(manifest) = members
        .iter()
        .find(|member| member.path.as_str() == IGNORE_FILE)
        && let Some(hash) = write_member(&mut writer, root, manifest, 64 * 1024)?
    {
        hashes.insert(manifest.path.clone(), hash);
    }
    let mut batch = Vec::new();
    let mut bytes = 0;
    for member in members
        .iter()
        .filter(|member| member.path.as_str() != IGNORE_FILE)
    {
        let size = member.stamp.as_ref().map_or(0, |stamp| stamp.size);
        if size > SMALL || bytes + size > WINDOW || batch.len() >= 512 {
            write_batch(root, &batch, &mut writer, &mut hashes)?;
            batch.clear();
            bytes = 0;
        }
        if size > SMALL {
            if let Some(hash) = write_member(&mut writer, root, member, BUFFER)? {
                hashes.insert(member.path.clone(), hash);
            }
        } else {
            batch.push(member);
            bytes += size;
        }
    }
    write_batch(root, &batch, &mut writer, &mut hashes)?;
    writer.finish()?.sync_all()?;
    Ok(hashes)
}

fn write_batch(
    root: &Path,
    members: &[&Member],
    writer: &mut ZipWriter<File>,
    hashes: &mut MemberHashes,
) -> Result<()> {
    if members.is_empty() {
        return Ok(());
    }
    let pool = PACKERS
        .as_ref()
        .map_err(|error| invalid(format!("compression thread pool: {error}")))?;
    let chunks = pool.install(|| {
        members
            .par_iter()
            .map(|member| {
                let size = usize::try_from(member.stamp.as_ref().map_or(0, |stamp| stamp.size))
                    .map_err(|_| invalid("bundle member exceeds addressable memory"))?;
                let capacity = size + size / 32 + 16 * 1024;
                let mut writer = ZipWriter::new(Cursor::new(Vec::with_capacity(capacity)));
                let hash = write_member(&mut writer, root, member, 64 * 1024)?;
                Ok((writer.finish()?, hash))
            })
            .collect::<Result<Vec<_>>>()
    })?;
    for (member, (mini, hash)) in members.iter().zip(chunks) {
        writer.merge_archive(ZipArchive::new(mini)?)?;
        if let Some(hash) = hash {
            hashes.insert(member.path.clone(), hash);
        }
    }
    Ok(())
}

fn write_member<W: Write + Seek>(
    writer: &mut ZipWriter<W>,
    root: &Path,
    member: &Member,
    buffer: usize,
) -> Result<Option<Blake3Hash>> {
    let mut options = SimpleFileOptions::default().last_modified_time(DateTime::default());
    let Some(stamp) = &member.stamp else {
        writer.add_directory(
            format!("{}/", member.path.as_str()),
            options.unix_permissions(0o755),
        )?;
        return Ok(None);
    };
    if stamp.size > (u64::from(u32::MAX) - 4 * 1024 * 1024) {
        options = options.large_file(true);
    }
    options = options
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(1))
        .unix_permissions(0o644);
    let path = checked_join(root, &member.path)?;
    let file = File::open(&path)?;
    let current = Stamp::from_metadata(&file.metadata()?)?;
    if stamp != &current {
        return Err(changed(&path));
    }
    writer.start_file(member.path.as_str(), options)?;
    let (written, hash) = digest::copy(file, writer, buffer)?;
    if written != stamp.size || crate::store::layout::stamp(&path)? != current {
        return Err(changed(&path));
    }
    Ok(Some(hash))
}

fn changed(path: &Path) -> Error {
    Error::LocalConflict(format!("file changed during packaging: {}", path.display()))
}
