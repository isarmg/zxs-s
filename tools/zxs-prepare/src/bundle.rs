use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{Read, Seek, Write},
    path::Path,
};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use unicode_normalization::UnicodeNormalization;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::{
    Result, invalid,
    paths::{self, Entry, EntryKind},
};

const MANIFEST: &str = ".zxsignore";
const MANIFEST_LIMIT: u64 = 1024 * 1024;
const MEMBER_ERROR: &str = "此项目无法作为独立的 ZIP 成员";

pub fn check_file_member(name: &str) -> Result<()> {
    if name.eq_ignore_ascii_case(MANIFEST) {
        return Err(invalid(MEMBER_ERROR));
    }
    Ok(())
}

pub fn pack(source: &Entry, output: impl Write) -> Result<()> {
    let text = manifest(source)?;
    let mut rules = GitignoreBuilder::new("");
    let patterns = text.strip_prefix('\u{feff}').unwrap_or(&text);
    for line in patterns.nfc().collect::<String>().lines() {
        rules.add_line(None, line)?;
    }
    let mut bundle = Bundle {
        zip: ZipWriter::new_stream(output),
        rules: rules.build()?,
        entries: 1,
        bytes: text.len() as u64,
    };
    bundle.zip.start_file(
        MANIFEST,
        SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .unix_permissions(0o644),
    )?;
    bundle.zip.write_all(text.as_bytes())?;
    match source.kind {
        EntryKind::Directory => bundle.level(&source.path, "")?,
        EntryKind::File => {
            check_file_member(&source.name)?;
            bundle.file(source, &paths::name(&source.name)?)?;
        }
        EntryKind::Unsupported => return Err(invalid(MEMBER_ERROR)),
    }
    bundle.zip.finish()?;
    Ok(())
}

fn manifest(source: &Entry) -> Result<String> {
    let path = source.path.join(MANIFEST);
    if source.kind != EntryKind::Directory || !paths::exists(&path)? {
        return Ok(String::new());
    }
    if paths::kind(&fs::symlink_metadata(&path)?) != EntryKind::File {
        return Err(invalid(format!("{MANIFEST} 必须是普通文件")));
    }
    let mut text = String::new();
    File::open(&path)?
        .take(MANIFEST_LIMIT + 1)
        .read_to_string(&mut text)
        .map_err(|error| invalid(format!("{} · {error}", path.display())))?;
    if text.len() as u64 > MANIFEST_LIMIT {
        return Err(invalid(format!("{MANIFEST} 超过 1 MiB")));
    }
    Ok(text)
}

struct Bundle<W: Write + Seek> {
    zip: ZipWriter<W>,
    rules: Gitignore,
    entries: usize,
    bytes: u64,
}

impl<W: Write + Seek> Bundle<W> {
    fn level(&mut self, directory: &Path, parent: &str) -> Result<()> {
        let mut names = BTreeSet::new();
        for entry in paths::entries(directory, false)? {
            if parent.is_empty() && entry.name == MANIFEST {
                continue;
            }
            let name: String = entry.name.nfc().collect();
            let relative = paths::child(parent, &name);
            let is_directory = entry.kind == EntryKind::Directory;
            if self
                .rules
                .matched(Path::new(&relative), is_directory)
                .is_ignore()
            {
                continue;
            }
            paths::name(&name)?;
            if relative.len() > 1024
                || !names.insert(paths::key(&name))
                || (parent.is_empty() && name.eq_ignore_ascii_case(MANIFEST))
            {
                return Err(invalid(format!("ZIP 成员路径超限或冲突 {relative}")));
            }
            match entry.kind {
                EntryKind::Directory => {
                    self.add_entry(0)?;
                    self.zip.add_directory(
                        format!("{relative}/"),
                        SimpleFileOptions::default().unix_permissions(0o755),
                    )?;
                    self.level(&entry.path, &relative)?;
                }
                EntryKind::File => self.file(&entry, &relative)?,
                EntryKind::Unsupported => {
                    return Err(invalid(format!("不支持的 ZIP 成员 {relative}")));
                }
            }
        }
        Ok(())
    }

    fn file(&mut self, source: &Entry, relative: &str) -> Result<()> {
        self.add_entry(source.bytes)?;
        let options = SimpleFileOptions::default()
            .unix_permissions(0o644)
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(1))
            .large_file(true);
        self.zip.start_file(relative, options)?;
        paths::copy_file(source, &mut self.zip)
    }

    fn add_entry(&mut self, bytes: u64) -> Result<()> {
        self.entries += 1;
        self.bytes = self.bytes.saturating_add(bytes);
        if self.entries > 200_000 || self.bytes > 64 * 1024 * 1024 * 1024 {
            return Err(invalid("ZIP 超过客户端默认的 200,000 项或 64 GiB 解压上限"));
        }
        Ok(())
    }
}
