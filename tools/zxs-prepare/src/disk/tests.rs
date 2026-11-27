use std::{
    fs::{self, File},
    io::Read,
    path::PathBuf,
};

use serde_json::Value;
use zip::ZipArchive;

use super::{Disk, DiskKind, Pending, Status, init, prepare, resume, scan};
use crate::{Result, build, invalid, paths};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Result<Self> {
        let path =
            std::env::temp_dir().join(format!("zxs-prepare-test-{}", build::identifier(None)?));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        #[cfg(windows)]
        let _ = super::remove_readonly(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn empty_disks_get_distinct_identities_without_replacing_an_existing_marker() -> Result<()> {
    let data = Fixture::new()?;
    let recycle = Fixture::new()?;
    assert!(matches!(scan(&data.0, "")?.status, Status::Empty));
    let first = init(&data.0, DiskKind::Data)?;
    let second = init(&recycle.0, DiskKind::Recycle)?;
    assert_ne!(first.disk_id, second.disk_id);
    let marker = recycle.0.join(".zxs/disk.toml");
    let contents = fs::read_to_string(&marker)?;
    assert!(contents.contains("kind = \"recycle\""));
    assert!(init(&recycle.0, DiskKind::Data).is_err());
    assert_eq!(contents, fs::read_to_string(&marker)?);
    Ok(())
}

#[test]
fn mixed_input_creates_v1_containers_and_client_readable_bundles() -> Result<()> {
    let disk = Fixture::new()?;
    let root = &disk.0;
    fs::create_dir_all(root.join("Docs/空"))?;
    fs::create_dir_all(root.join("Docs/相册.ZIP/空"))?;
    fs::write(root.join("Docs/空文件.txt"), [])?;
    fs::write(root.join("Docs/相册.ZIP/.zxsignore"), "忽略.txt\n")?;
    fs::write(root.join("Docs/相册.ZIP/忽略.txt"), "保留在备份")?;
    fs::write(root.join("Docs/相册.ZIP/照片.jpg"), b"photo")?;
    fs::write(root.join("single.txt"), b"original")?;
    let result = prepare(root, None, &["Docs/相册.ZIP".into(), "single.txt".into()])?;
    assert_eq!(result.containers, 3);
    assert!(root.join("Docs/空").is_dir());
    assert_eq!(
        fs::read(root.join("Docs/空文件.txt/v000001/空文件.txt"))?,
        b""
    );
    let published = root.join("Docs/相册.ZIP/v000001/相册.ZIP");
    let metadata: Value = serde_json::from_slice(&fs::read(published.with_file_name(".zxs"))?)?;
    assert_eq!(
        metadata["blake3"],
        blake3::hash(&fs::read(&published)?).to_hex().as_str()
    );
    let mut zip = ZipArchive::new(File::open(published)?)?;
    assert_eq!(zip.by_index(0)?.name(), ".zxsignore");
    assert!(zip.by_name("空/")?.is_dir());
    assert!(zip.by_name("忽略.txt").is_err());
    let mut photo = Vec::new();
    zip.by_name("照片.jpg")?.read_to_end(&mut photo)?;
    assert_eq!(photo, b"photo");
    let mut single = ZipArchive::new(File::open(
        root.join("single.txt.zip/v000001/single.txt.zip"),
    )?)?;
    let mut bytes = Vec::new();
    single.by_name("single.txt")?.read_to_end(&mut bytes)?;
    assert_eq!(bytes, b"original");
    assert_eq!(
        fs::read_to_string(root.join(".zxs/originals/Docs/相册.ZIP/忽略.txt"))?,
        "保留在备份"
    );
    assert!(matches!(scan(root, "")?.status, Status::Prepared));
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn bundle_selection_distinguishes_original_paths_with_equivalent_unicode() -> Result<()> {
    let disk = Fixture::new()?;
    let root = &disk.0;
    let parent = "Ce\u{301}ramique";
    for (folder, content) in [("Café", "selected"), ("Cafe\u{301}", "unselected")] {
        let source = root.join(parent).join(folder);
        fs::create_dir_all(&source)?;
        fs::write(source.join("note.txt"), content)?;
    }
    prepare(root, None, &[format!("{parent}/Café")])?;

    let payload = root.join("Céramique/Café.zip/v000001/Café.zip");
    let mut zip = ZipArchive::new(File::open(payload)?)?;
    let mut text = String::new();
    zip.by_name("note.txt")?.read_to_string(&mut text)?;
    assert_eq!(text, "selected");
    assert_eq!(
        fs::read_to_string(root.join("Céramique/Café/note.txt/v000001/note.txt"))?,
        "unselected"
    );
    Ok(())
}

#[test]
fn output_collision_keeps_originals_in_their_locations() -> Result<()> {
    let disk = Fixture::new()?;
    let file = disk.0.join("Album.zip");
    fs::create_dir(disk.0.join("Album"))?;
    fs::write(&file, b"existing ZIP")?;
    let error = prepare(&disk.0, None, &["Album".into()])
        .err()
        .ok_or_else(|| invalid("预期名称冲突"))?;
    assert!(error.to_string().contains("输出名称冲突"));
    assert!(disk.0.join("Album").is_dir());
    assert_eq!(fs::read(file)?, b"existing ZIP");
    assert!(!disk.0.join(".zxs/disk.toml").exists());
    assert!(!disk.0.join(".zxs/originals").exists());
    Ok(())
}

#[test]
fn resume_finishes_a_partially_published_disk() -> Result<()> {
    for publish_first in [false, true] {
        let fixture = Fixture::new()?;
        let root = &fixture.0;
        let disk = Disk::read(root)?;
        let lock = disk.lock()?;
        fs::write(root.join("a.txt"), b"alpha")?;
        fs::write(root.join("b.txt"), b"beta")?;
        fs::create_dir(disk.stage())?;
        let tree = build::stage(root, &disk.stage(), "disk1", &[])?;
        let record = Pending {
            kind: DiskKind::Data,
            disk_id: build::identifier(None)?,
            originals: tree.originals,
            published: tree.published,
            containers: tree.containers,
        };
        build::write_json(&disk.pending(), &record)?;
        let originals = root.join(".zxs/originals");
        fs::create_dir(&originals)?;
        paths::relocate(&root.join("a.txt"), &originals.join("a.txt"))?;
        if publish_first {
            paths::relocate(&root.join("b.txt"), &originals.join("b.txt"))?;
            paths::relocate(&disk.stage().join("a.txt"), &root.join("a.txt"))?;
        }
        drop(lock);
        assert!(matches!(scan(root, "")?.status, Status::Pending));
        let result = resume(root)?;
        assert_eq!(result.disk_id, record.disk_id);
        assert_eq!(fs::read(root.join("a.txt/v000001/a.txt"))?, b"alpha");
        assert_eq!(fs::read(root.join("b.txt/v000001/b.txt"))?, b"beta");
        assert_eq!(fs::read(originals.join("a.txt"))?, b"alpha");
        assert_eq!(fs::read(originals.join("b.txt"))?, b"beta");
        assert!(!disk.pending().exists());
    }
    Ok(())
}
