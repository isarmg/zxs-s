use std::{
    fs::{self, File},
    io::Read,
    path::PathBuf,
};

use serde_json::Value;
use zip::ZipArchive;

use super::{Disk, DiskKind, Pending, Status, init, prepare, resume, scan};
use crate::{Result, build, bundle, invalid, paths};

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
    fs::create_dir_all(root.join("Docs/空/.zxsignore"))?;
    fs::create_dir_all(root.join("Docs/相册.ZIP/空"))?;
    fs::write(root.join(".zxsignore"), b"ordinary file")?;
    fs::write(root.join("Docs/空文件.txt"), [])?;
    let manifest = "\u{feff}忽略.txt\r\nCafe\u{301}.txt\r\n";
    fs::write(root.join("Docs/相册.ZIP/.zxsignore"), manifest)?;
    fs::write(root.join("Docs/相册.ZIP/忽略.txt"), "保留在备份")?;
    fs::write(root.join("Docs/相册.ZIP/Cafe\u{301}.txt"), "also ignored")?;
    fs::write(root.join("Docs/相册.ZIP/照片.jpg"), b"photo")?;
    fs::write(root.join("single.zip"), b"original")?;
    let result = prepare(root, None, &["Docs/相册.ZIP".into(), "single.zip".into()])?;
    assert_eq!(result.containers, 4);
    assert!(root.join("Docs/空/.zxsignore").is_dir());
    assert_eq!(
        fs::read(root.join(".zxsignore/v000001/.zxsignore"))?,
        b"ordinary file"
    );
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
    assert!(zip.by_name("Café.txt").is_err());
    let mut rules = String::new();
    zip.by_name(".zxsignore")?.read_to_string(&mut rules)?;
    assert_eq!(rules, manifest);
    let mut photo = Vec::new();
    zip.by_name("照片.jpg")?.read_to_end(&mut photo)?;
    assert_eq!(photo, b"photo");
    let mut single = ZipArchive::new(File::open(
        root.join("single.zip.zip/v000001/single.zip.zip"),
    )?)?;
    let mut bytes = Vec::new();
    single.by_name("single.zip")?.read_to_end(&mut bytes)?;
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
fn nested_scan_and_selection_accept_native_separators() -> Result<()> {
    let disk = Fixture::new()?;
    let root = &disk.0;
    let separator = std::path::MAIN_SEPARATOR;
    let native = format!("Docs{separator}Photos");
    let directory = root.join(&native);
    fs::create_dir_all(&directory)?;
    fs::write(directory.join("image.jpg"), b"photo")?;

    let complete = format!("{native}{separator}");
    for path in [&native, &complete] {
        let listing = scan(root, path)?;
        let paths = listing.entries.into_iter().map(|entry| entry.path);
        assert!(paths.eq(["Docs/Photos/image.jpg"]));
    }
    for path in [
        "/",
        "/Docs/Photos/",
        "Docs//Photos/",
        "Docs/Photos//",
        "Docs/../",
    ] {
        assert!(scan(root, path).is_err(), "{path}");
        assert!(prepare(root, None, &[path.into()]).is_err(), "{path}");
    }
    assert!(scan(root, "Docs/Photos/image.jpg/").is_err());
    prepare(root, None, &[complete])?;
    assert!(root.join("Docs/Photos.zip/v000001/Photos.zip").is_file());
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
fn invalid_bundle_selections_leave_staging_empty() -> Result<()> {
    let fixture = Fixture::new()?;
    let source = fixture.0.join("source");
    let stage = fixture.0.join("stage");
    fs::create_dir_all(source.join("Album"))?;
    fs::create_dir(&stage)?;
    fs::write(source.join("Album/notes.txt"), b"original")?;
    fs::write(source.join(".zxsignore"), b"ordinary file")?;
    let long_name = "文".repeat(84);
    let oversized = format!("{long_name}.zip");
    fs::write(source.join(&long_name), b"normal long name")?;

    for (selection, message) in [
        (vec!["missing".into()], "找不到所选项目 missing"),
        (
            vec!["Album/notes.txt".into(), "Album".into()],
            "已包含在上级 ZIP 中 Album/notes.txt",
        ),
        (vec!["Album/notes.txt/".into()], "所选项目不是目录"),
        (vec![".zxsignore".into()], "无法作为独立的 ZIP 成员"),
        (vec![long_name.clone()], oversized.as_str()),
    ] {
        let error = build::stage(&source, &stage, "disk1", &selection)
            .err()
            .ok_or_else(|| invalid("预期拒绝所选路径"))?;
        assert!(error.to_string().contains(message));
        assert!(paths::entries(&stage, false)?.is_empty());
    }
    build::stage(&source, &stage, "disk1", &[])?;
    assert_eq!(
        fs::read(stage.join(&long_name).join("v000001").join(long_name))?,
        b"normal long name"
    );
    Ok(())
}

#[test]
fn bundle_limit_is_checked_before_opening_an_oversized_member() -> Result<()> {
    let disk = Fixture::new()?;
    let source = paths::Entry {
        name: "image.raw".into(),
        path: disk.0.join("image.raw"),
        kind: paths::EntryKind::File,
        bytes: 65 * 1024 * 1024 * 1024,
    };
    let error = bundle::pack(&source, std::io::sink())
        .err()
        .ok_or_else(|| invalid("预期 ZIP 体积超限"))?;
    assert!(error.to_string().contains("64 GiB"));
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
        paths::rename_new(&root.join("a.txt"), &originals.join("a.txt"))?;
        if publish_first {
            paths::rename_new(&root.join("b.txt"), &originals.join("b.txt"))?;
            paths::rename_new(&disk.stage().join("a.txt"), &root.join("a.txt"))?;
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

#[test]
fn resume_cleans_a_published_disk_after_server_changes() -> Result<()> {
    let fixture = Fixture::new()?;
    let root = &fixture.0;
    fs::write(root.join("notes.txt"), "original")?;
    let report = prepare(root, None, &[])?;
    let disk = Disk::read(root)?;
    fs::create_dir(disk.stage())?;
    build::write_json(
        &disk.pending(),
        &Pending {
            kind: DiskKind::Data,
            disk_id: report.disk_id.clone(),
            originals: vec!["notes.txt".into()],
            published: vec!["notes.txt".into()],
            containers: 1,
        },
    )?;
    fs::rename(root.join("notes.txt"), root.join("renamed.txt"))?;
    fs::create_dir(root.join("July"))?;

    assert_eq!(resume(root)?.disk_id, report.disk_id);
    assert!(matches!(scan(root, "")?.status, Status::Prepared));
    assert!(!disk.stage().exists());
    assert!(!disk.pending().exists());
    assert!(root.join("July").is_dir());
    assert_eq!(
        fs::read_to_string(root.join("renamed.txt/v000001/notes.txt"))?,
        "original"
    );
    assert_eq!(
        fs::read_to_string(root.join(".zxs/originals/notes.txt"))?,
        "original"
    );
    Ok(())
}
