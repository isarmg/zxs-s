use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};

use super::layout::{TempDir, create_private, parent, read_small};
use crate::{
    domain::{Error, Result, Session, invalid},
    transport::Remote,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub endpoint: String,
    pub allow_public_http: bool,
    pub session: Session,
}

impl Profile {
    /// # Errors
    ///
    /// Returns an error if the origin, password, or local profile cannot be used.
    pub fn login(
        endpoint: &str,
        allow_public_http: bool,
        password: &str,
    ) -> Result<(Self, Remote)> {
        let remote = Remote::connect(endpoint, allow_public_http, None)?;
        let session = remote.login(password)?;
        let profile = Self {
            endpoint: remote.endpoint().into(),
            allow_public_http,
            session,
        };
        let remote = remote.with_token(profile.session.token.clone());
        profile.save()?;
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
        Remote::connect(
            &self.endpoint,
            self.allow_public_http,
            Some(self.session.token.clone()),
        )
    }

    /// # Errors
    ///
    /// Returns an error if the session cannot be revoked or the local profile cannot be deleted.
    pub fn logout(&self, remote: &Remote) -> Result<()> {
        match remote.logout() {
            Ok(()) | Err(Error::Remote { status: 401, .. }) => {}
            Err(error) => return Err(error),
        }
        fs::remove_file(directory()?.join("profile.json"))?;
        Ok(())
    }

    fn save(&self) -> Result<()> {
        let dir = directory()?;
        fs::create_dir_all(parent(&dir)?)?;
        create_private(&dir)?;
        let stage = TempDir::create(&dir)?;
        let path = stage.child("profile.json");
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        serde_json::to_writer(&mut file, self)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        fs::rename(path, dir.join("profile.json"))?;
        Ok(())
    }
}

fn directory() -> Result<PathBuf> {
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
