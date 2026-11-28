use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{self, BufWriter, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

use crate::{
    Result, bundle, invalid,
    paths::{self, BUFFER, Entry, EntryKind, SYSTEM},
};

pub struct Tree {
    pub originals: Vec<String>,
    pub published: Vec<String>,
    pub containers: usize,
}

pub fn stage(root: &Path, output: &Path, mount: &str, selected: &[String]) -> Result<Tree> {
    let remaining = selected
        .iter()
        .map(|path| selection(root, path))
        .collect::<Result<BTreeSet<_>>>()?;
    for path in &remaining {
        if path
            .match_indices('/')
            .any(|(end, _)| remaining.contains(&path[..end]))
        {
            return Err(invalid(format!(
                "找不到所选项目或它已包含在上级 ZIP 中 {path}"
            )));
        }
    }
    let mut builder = Builder {
        remaining,
        now: SystemTime::now().duration_since(UNIX_EPOCH)?,
        containers: 0,
    };
    let entries = paths::entries(root, true)?;
    let published = builder.level(&entries, output, "", mount.len() + 2)?;
    if let Some(missing) = builder.remaining.first() {
        return Err(invalid(format!("找不到所选项目 {missing}")));
    }
    Ok(Tree {
        originals: entries.into_iter().map(|entry| entry.name).collect(),
        published,
        containers: builder.containers,
    })
}

fn selection(root: &Path, input: &str) -> Result<String> {
    let input = paths::slash_path(input);
    let path = paths::without_trailing_separator(&input);
    let mut parts = path.rsplit('/');
    let leaf = parts.next().unwrap_or_default();
    let mut source = root.to_owned();
    for part in parts.rev() {
        paths::name(part)?;
        source.push(part);
    }
    let name = paths::name(leaf)?;
    source.push(leaf);
    let metadata = match fs::symlink_metadata(&source) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(invalid(format!("找不到所选项目 {path}")));
        }
        result => result?,
    };
    let kind = paths::kind(&metadata);
    if path.len() != input.len() && kind != EntryKind::Directory {
        return Err(invalid(format!("所选项目不是目录 {input}")));
    }
    if kind == EntryKind::File {
        bundle::check_file_member(leaf)?;
    }
    packed_name(kind, name)?;
    Ok(path.to_owned())
}

struct Builder {
    remaining: BTreeSet<String>,
    now: std::time::Duration,
    containers: usize,
}

impl Builder {
    fn level(
        &mut self,
        entries: &[Entry],
        output: &Path,
        source_parent: &str,
        output_prefix_bytes: usize,
    ) -> Result<Vec<String>> {
        let mut names = BTreeSet::new();
        let mut checked = Vec::with_capacity(entries.len());
        for entry in entries {
            let name = paths::name(&entry.name)?;
            let relative = paths::child(source_parent, &entry.name);
            let packed = self.remaining.remove(&relative);
            let filename = if packed {
                packed_name(entry.kind, name)?
            } else {
                name
            };
            let path_bytes = output_prefix_bytes + filename.len();
            if path_bytes > 1024 {
                return Err(invalid(format!("路径超出 ZXS 长度限制 {relative}")));
            }
            if !names.insert(paths::key(&filename)) {
                return Err(invalid(format!("输出名称冲突 {relative} 生成 {filename}")));
            }
            if entry.kind == EntryKind::Unsupported {
                return Err(invalid(format!("不支持符号链接或特殊文件 {relative}")));
            }
            checked.push((entry, relative, filename, packed, path_bytes + 1));
        }
        let published = checked
            .into_iter()
            .map(|(entry, relative, filename, packed, child_prefix_bytes)| {
                self.write_entry(
                    entry,
                    &output.join(&filename),
                    (&relative, &filename),
                    packed,
                    child_prefix_bytes,
                )?;
                Ok(filename)
            })
            .collect::<Result<_>>()?;
        paths::sync_dir(output)?;
        Ok(published)
    }

    fn write_entry(
        &mut self,
        source: &Entry,
        output: &Path,
        names: (&str, &str),
        packed: bool,
        child_prefix_bytes: usize,
    ) -> Result<()> {
        let (relative, filename) = names;
        fs::create_dir(output)?;
        if source.kind == EntryKind::Directory && !packed {
            self.level(
                &paths::entries(&source.path, false)?,
                output,
                relative,
                child_prefix_bytes,
            )?;
        } else {
            self.container(source, output, filename, packed)?;
            self.containers += 1;
        }
        Ok(())
    }

    fn container(&self, source: &Entry, output: &Path, filename: &str, packed: bool) -> Result<()> {
        let version = output.join("v000001");
        fs::create_dir(&version)?;
        let capacity = if packed { BUFFER } else { 0 };
        let mut payload = Payload::new(&version.join(filename), capacity)?;
        if packed {
            bundle::pack(source, &mut payload)?;
        } else {
            paths::copy_file(source, &mut payload)?;
        }
        let (size, blake3) = payload.finish()?;
        write_json(
            &version.join(SYSTEM),
            &VersionMeta {
                number: 1,
                created_at: self.now.as_secs(),
                name_at_commit: filename,
                size,
                blake3,
            },
        )?;
        write_json(
            &output.join(SYSTEM),
            &ContainerMeta {
                id: identifier(Some(self.now))?,
                tags: Vec::new(),
                first_retained_version: 1,
                latest_version: 1,
                created_at: self.now.as_secs(),
                updated_at: self.now.as_secs(),
            },
        )?;
        paths::sync_dir(&version)?;
        paths::sync_dir(output)
    }
}

fn packed_name(kind: EntryKind, name: String) -> Result<String> {
    if kind == EntryKind::Directory
        && name
            .rsplit_once('.')
            .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("zip"))
    {
        Ok(name)
    } else {
        paths::name(&format!("{name}.zip"))
    }
}

#[derive(Serialize)]
struct ContainerMeta {
    #[serde(rename = "container_id")]
    id: String,
    tags: Vec<String>,
    first_retained_version: u32,
    latest_version: u32,
    created_at: u64,
    updated_at: u64,
}

#[derive(Serialize)]
struct VersionMeta<'a> {
    #[serde(rename = "version")]
    number: u32,
    created_at: u64,
    name_at_commit: &'a str,
    size: u64,
    blake3: String,
}

struct Payload {
    output: BufWriter<File>,
    hasher: blake3::Hasher,
    size: u64,
}

impl Payload {
    fn new(path: &Path, capacity: usize) -> Result<Self> {
        Ok(Self {
            output: BufWriter::with_capacity(capacity, File::create_new(path)?),
            hasher: blake3::Hasher::new(),
            size: 0,
        })
    }

    fn finish(self) -> Result<(u64, String)> {
        paths::mark_readonly(&self.output.into_inner()?)?;
        Ok((self.size, self.hasher.finalize().to_hex().to_string()))
    }
}

impl Write for Payload {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.output.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        self.size += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

pub fn write_json(path: &Path, data: &impl Serialize) -> Result<()> {
    write_bytes(path, &serde_json::to_vec(data)?)
}

pub fn write_bytes(path: &Path, data: &[u8]) -> Result<()> {
    let mut file = File::create_new(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}

pub fn identifier(time: Option<std::time::Duration>) -> Result<String> {
    use std::fmt::Write as _;

    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| invalid(error.to_string()))?;
    if let Some(time) = time {
        bytes[..6].copy_from_slice(&time.as_millis().to_be_bytes()[10..]);
        bytes[6] = (bytes[6] & 0x0f) | 0x70;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
    }
    let mut id = String::with_capacity(32);
    for byte in bytes {
        write!(&mut id, "{byte:02x}")?;
    }
    Ok(id)
}
