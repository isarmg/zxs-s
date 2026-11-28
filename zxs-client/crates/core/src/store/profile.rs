use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use serde::{Deserialize, Serialize};

use super::layout::{TempDir, create_private, parent, read_small, sync_parent};
use crate::{
    domain::{Error, METADATA_LIMIT, Result, invalid},
    transport::Remote,
};

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub endpoint: String,
    password: String,
}

impl Profile {
    /// # Errors
    ///
    /// Returns an error if the origin, password, or local profile cannot be used.
    pub fn login(endpoint: &str, password: &str) -> Result<(Self, Remote)> {
        let remote = Remote::connect(endpoint)?;
        remote.use_password(password.into())?;
        remote.authenticate()?;
        let profile = Self {
            endpoint: remote.endpoint().into(),
            password: password.into(),
        };
        save_metadata(&directory()?, "profile.json", &profile)?;
        profile.observe(&remote)?;
        Ok((profile, remote))
    }

    /// # Errors
    ///
    /// Returns an error if the local profile cannot be read or decoded.
    pub fn load() -> Result<Option<Self>> {
        let path = directory()?.join("profile.json");
        match read_small(&path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// # Errors
    ///
    /// Returns an error if the configured server origin cannot be resolved or used.
    pub fn remote(&self) -> Result<Remote> {
        let remote = Remote::connect(&self.endpoint)?;
        remote.use_password(self.password.clone())?;
        self.observe(&remote)?;
        Ok(remote)
    }

    fn observe(&self, remote: &Remote) -> Result<()> {
        let current = self.clone();
        remote.on_password_rejected(Arc::new(move || current.clear()))
    }

    /// # Errors
    /// Returns an error if the current credentials cannot be checked or removed.
    pub fn clear(&self) -> Result<()> {
        if Self::load()?.as_ref() == Some(self) {
            match fs::remove_file(directory()?.join("profile.json")) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                result => result?,
            }
        }
        Ok(())
    }
}

pub(in crate::store) fn save_metadata(
    dir: &std::path::Path,
    name: &str,
    value: &impl Serialize,
) -> Result<()> {
    let metadata = serde_json::to_vec(value)?;
    if metadata.len() > METADATA_LIMIT {
        return Err(invalid("local configuration exceeds 1 MiB"));
    }
    fs::create_dir_all(parent(dir)?)?;
    create_private(dir)?;
    let stage = TempDir::create(dir)?;
    let path = stage.child(name);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    file.write_all(&metadata)?;
    file.sync_all()?;
    drop(file);
    let destination = dir.join(name);
    fs::rename(path, &destination)?;
    sync_parent(&destination)
}

pub(in crate::store) fn directory() -> Result<PathBuf> {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    if let Some(path) = PATH.get() {
        return Ok(path.clone());
    }
    let path = absolute_config_path()?;
    Ok(PATH.get_or_init(|| path).clone())
}

fn absolute_config_path() -> Result<PathBuf> {
    let path = std::path::absolute(config_path()?)?;
    for ancestor in path.ancestors().skip(1) {
        match ancestor.canonicalize() {
            Ok(resolved) => {
                let suffix = path
                    .strip_prefix(ancestor)
                    .map_err(|error| invalid(error.to_string()))?;
                return Ok(resolved.join(suffix));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(path)
}

fn config_path() -> Result<PathBuf> {
    if let Some(dir) = env::var_os("ZXS_CONFIG_DIR") {
        return Ok(dir.into());
    }
    if cfg!(windows) {
        return env::var_os("APPDATA")
            .map(|p| PathBuf::from(p).join("ZXS"))
            .ok_or_else(|| invalid("APPDATA is unavailable; set ZXS_CONFIG_DIR"));
    }
    if !cfg!(target_os = "macos")
        && let Some(dir) = env::var_os("XDG_CONFIG_HOME")
    {
        return Ok(PathBuf::from(dir).join("zxs"));
    }
    let home = PathBuf::from(env::var_os("HOME").ok_or_else(|| invalid("set ZXS_CONFIG_DIR"))?);
    Ok(home.join(if cfg!(target_os = "macos") {
        "Library/Application Support/ZXS"
    } else {
        ".config/zxs"
    }))
}
