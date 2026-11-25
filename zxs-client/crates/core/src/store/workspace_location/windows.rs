use std::{
    io::{self, ErrorKind},
    path::Path,
    thread,
    time::Duration,
};

use winsafe::{MoveFile, co::ERROR};

pub(super) fn rename_directory(from: &Path, to: &Path) -> io::Result<()> {
    let source = windows_path(from)?;
    let destination = windows_path(to)?;
    let mut delays = [40, 80, 160, 320].into_iter();
    loop {
        let Err(code) = MoveFile(source, destination) else {
            return Ok(());
        };
        let raw: u32 = code.into();
        let error = io::Error::from_raw_os_error(i32::from_ne_bytes(raw.to_ne_bytes()));
        if code == ERROR::NOT_SAME_DEVICE {
            return Err(io::Error::new(ErrorKind::CrossesDevices, error));
        }
        if !matches!(
            code,
            ERROR::ACCESS_DENIED | ERROR::SHARING_VIOLATION | ERROR::LOCK_VIOLATION
        ) {
            return Err(error);
        }
        let Some(delay) = delays.next() else {
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "Windows 无法移动工作区目录。请关闭正在使用该目录或其中文件的程序，并确认能够写入目标文件夹。\n{} → {} · {error}",
                    display_path(source),
                    display_path(destination),
                ),
            ));
        };
        // Shell and antivirus handles can be released shortly after the write.
        thread::sleep(Duration::from_millis(delay));
    }
}

fn windows_path(path: &Path) -> io::Result<&str> {
    path.to_str()
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "工作区路径不是有效的 Unicode"))
}

fn display_path(path: &str) -> String {
    if let Some(share) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{share}");
    }
    if let Some(drive) = path.strip_prefix(r"\\?\")
        && drive.as_bytes().get(1) == Some(&b':')
    {
        return drive.into();
    }
    path.into()
}
