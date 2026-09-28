use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};

use crate::util::timestamp;

const ROTATE_BYTES: u64 = 1024 * 1024;

pub struct Log {
    file: File,
    path: PathBuf,
}

impl Log {
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        let log = Self { file, path: path.to_path_buf() };
        let _ = log.rotate_if_large();
        Ok(log)
    }

    pub fn line(&self, message: impl AsRef<str>) {
        let _ = self.rotate_if_large();
        let _ = writeln!(&self.file, "{} {}", timestamp(SystemTime::now()), message.as_ref());
    }

    fn rotate_if_large(&self) -> io::Result<()> {
        if self.file.metadata()?.len() >= ROTATE_BYTES {
            // Keep the inode open: the keeper's cloned stderr handle must keep
            // writing to the current log after rotation.
            fs::copy(&self.path, self.path.with_extension("log.1"))?;
            self.file.set_len(0)?;
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use std::process;

    use super::*;

    #[test]
    fn rotates_while_running_and_keeps_the_keepers_stderr_on_the_current_log() {
        let dir = std::env::temp_dir().join(format!("hka-log-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("daemon.log");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("log.1"));

        let log = Log::open(&path).unwrap();
        let mut keeper_stderr = log.handle().unwrap();
        log.line("x".repeat(ROTATE_BYTES as usize));
        log.line("after rotation");
        writeln!(keeper_stderr, "keeper stderr").unwrap();

        assert!(fs::read_to_string(path.with_extension("log.1")).unwrap().contains("xxx"));
        let current = fs::read_to_string(&path).unwrap();
        assert!(current.contains("after rotation"));
        assert!(current.contains("keeper stderr"));
        let _ = fs::remove_dir_all(dir);
    }
}
