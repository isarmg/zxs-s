use std::path::Path;

use crate::domain::{Result, invalid};

pub fn require(path: &Path, bytes: u64) -> Result<()> {
    let volume = path
        .ancestors()
        .find(|parent| parent.is_dir())
        .ok_or_else(|| invalid("cannot find destination volume"))?;
    let free = available(volume)?;
    if free < bytes {
        return Err(invalid(format!(
            "解包需要 {bytes} 字节，目标盘仅剩 {free} 字节"
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn available(path: &Path) -> Result<u64> {
    let stat = rustix::fs::statvfs(path).map_err(std::io::Error::from)?;
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

#[cfg(windows)]
fn available(path: &Path) -> Result<u64> {
    let mut free = 0;
    let path = path
        .to_str()
        .ok_or_else(|| invalid("destination path is not UTF-8"))?;
    winsafe::GetDiskFreeSpaceEx(Some(path), Some(&mut free), None, None)
        .map_err(|error| invalid(format!("cannot read available disk space: {error}")))?;
    Ok(free)
}
