use std::{
    fs,
    io::{self, Read},
    path::PathBuf,
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
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
