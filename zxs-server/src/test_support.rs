use std::{
    fs,
    io::{self, Read},
    path::PathBuf,
};

use crate::{
    domain::Result,
    store::{DiskKind, Layout, Store},
};

pub struct TestDir(pub PathBuf);

impl TestDir {
    pub fn create() -> io::Result<Self> {
        let mut nonce = [0; 8];
        fs::File::open("/dev/urandom")?.read_exact(&mut nonce)?;
        let path =
            std::env::temp_dir().join(format!("zxs-test-{:016x}", u64::from_le_bytes(nonce)));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    pub fn store(&self) -> Result<Store> {
        let data = self.0.join("data");
        let recycle = self.0.join("recycle");
        fs::create_dir_all(data.join("disk1"))?;
        fs::create_dir_all(recycle.join("rec1"))?;
        let mut store = Store {
            paths: Layout::new(data, recycle),
        };
        store.paths.data_disks = store.register_disks(DiskKind::Data, &[])?;
        store.paths.recycle_disks = store.register_disks(DiskKind::Recycle, &[])?;
        store.prepare()?;
        Ok(store)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
