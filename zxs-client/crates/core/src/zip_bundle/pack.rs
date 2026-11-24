use std::{
    fs::{File, OpenOptions},
    io::{self, BufReader, Cursor, Seek, Write},
    path::Path,
    sync::LazyLock,
};

use rayon::prelude::*;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::{
    domain::{BUFFER, Error, Member, Result, Stamp, invalid},
    store::layout::checked_join,
};

const SMALL: u64 = BUFFER as u64;
const WINDOW: u64 = 40 * 1024 * 1024;

static PACKERS: LazyLock<std::result::Result<rayon::ThreadPool, rayon::ThreadPoolBuildError>> =
    LazyLock::new(|| {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
        rayon::ThreadPoolBuilder::new().num_threads(threads).build()
    });

pub fn pack(root: &Path, members: &[Member], destination: &Path) -> Result<()> {
    let output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)?;
    let mut writer = ZipWriter::new(output);
    let mut batch = Vec::new();
    let mut bytes = 0;
    for member in members {
        let size = member.stamp.as_ref().map_or(0, |stamp| stamp.size);
        if size > SMALL || bytes + size > WINDOW || batch.len() >= 512 {
            write_batch(root, &batch, &mut writer)?;
            batch.clear();
            bytes = 0;
        }
        if size > SMALL {
            write_member(&mut writer, root, member, BUFFER)?;
        } else {
            batch.push(member);
            bytes += size;
        }
    }
    write_batch(root, &batch, &mut writer)?;
    writer.finish()?.sync_all()?;
    Ok(())
}

fn write_batch(root: &Path, members: &[&Member], writer: &mut ZipWriter<File>) -> Result<()> {
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
                write_member(&mut writer, root, member, 64 * 1024)?;
                Ok(writer.finish()?)
            })
            .collect::<Result<Vec<_>>>()
    })?;
    for mini in chunks {
        writer.merge_archive(ZipArchive::new(mini)?)?;
    }
    Ok(())
}

fn write_member<W: Write + Seek>(
    writer: &mut ZipWriter<W>,
    root: &Path,
    member: &Member,
    buffer: usize,
) -> Result<()> {
    let mut options = SimpleFileOptions::default().last_modified_time(DateTime::default());
    let Some(stamp) = &member.stamp else {
        writer.add_directory(
            format!("{}/", member.path.as_str()),
            options.unix_permissions(0o755),
        )?;
        return Ok(());
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
    let mut file = BufReader::with_capacity(buffer, file);
    let written = io::copy(&mut file, writer)?;
    if written != stamp.size || crate::store::layout::stamp(&path)? != current {
        return Err(changed(&path));
    }
    Ok(())
}

fn changed(path: &Path) -> Error {
    Error::LocalConflict(format!("file changed during packaging: {}", path.display()))
}
