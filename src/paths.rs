use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// The daemon and every client share one fixed directory, so a manual CLI
/// call, the service, and the daemon always agree on the socket and lock.
pub struct Paths {
    dir: PathBuf,
}

impl Paths {
    pub fn resolve() -> Result<Self> {
        let dir = match env::var_os("T3_KEEP_AWAKE_STATE_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => xdg_dir("XDG_STATE_HOME", ".local/state")?.join("t3-keep-awake"),
        };
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self { dir })
    }

    pub fn socket(&self) -> PathBuf {
        self.dir.join("daemon.sock")
    }

    pub fn lock(&self) -> PathBuf {
        self.dir.join("daemon.lock")
    }

    pub fn log(&self) -> PathBuf {
        self.dir.join("daemon.log")
    }
}

pub fn config_file() -> Result<PathBuf> {
    match env::var_os("T3_KEEP_AWAKE_CONFIG") {
        Some(path) => Ok(PathBuf::from(path)),
        None => Ok(xdg_dir("XDG_CONFIG_HOME", ".config")?.join("t3-keep-awake/config.toml")),
    }
}

pub fn service_file() -> Result<PathBuf> {
    Ok(xdg_dir("XDG_CONFIG_HOME", ".config")?.join("systemd/user/t3-keep-awake.service"))
}

fn xdg_dir(var: &str, fallback: &str) -> Result<PathBuf> {
    if let Some(dir) = env::var_os(var).filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(fallback))
}
