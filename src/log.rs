use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::SystemTime;

use anyhow::{Context, Result};

use crate::util::timestamp;

/// The daemon logs transitions, not ticks, so rotating once at startup is enough.
const ROTATE_BYTES: u64 = 1024 * 1024;

pub struct Log {
    file: File,
}

impl Log {
    pub fn open(path: &Path) -> Result<Self> {
        if fs::metadata(path).is_ok_and(|meta| meta.len() > ROTATE_BYTES) {
            let _ = fs::rename(path, path.with_extension("log.1"));
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        Ok(Self { file })
    }

    pub fn line(&self, message: impl AsRef<str>) {
        let _ = writeln!(&self.file, "{} {}", timestamp(SystemTime::now()), message.as_ref());
    }

    /// A handle for a child's stderr, so its complaints land in the same log.
    pub fn handle(&self) -> io::Result<File> {
        self.file.try_clone()
    }
}

pub fn tail(path: &Path, lines: usize) -> Result<Vec<String>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let all: Vec<&str> = text.lines().collect();
    Ok(all[all.len().saturating_sub(lines)..].iter().map(|line| line.to_string()).collect())
}
