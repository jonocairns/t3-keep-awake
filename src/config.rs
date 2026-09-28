use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Read once when the daemon starts; `herdr-keep-awake restart` applies edits.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// How often herdr is polled when no event arrives.
    pub poll_secs: u64,
    /// How long the hold outlasts the last working agent. Bridges the gaps
    /// between turns and tool calls, and covers a briefly unreadable herdr.
    pub grace_secs: u64,
    /// One uninterrupted working stretch longer than this is treated as a
    /// stuck status and stops counting.
    pub max_working_secs: u64,
    /// Whether an agent waiting on a permission prompt keeps Windows awake.
    pub hold_blocked: bool,
    /// Keep the display on as well, not just the system.
    pub keep_display_on: bool,
    /// How often the daemon tells the keeper it is still alive.
    pub heartbeat_secs: u64,
    /// The keeper releases the hold if it hears nothing for this long.
    pub keeper_timeout_secs: u64,
    /// The daemon exits after this long without a running herdr server.
    pub idle_exit_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            poll_secs: 10,
            grace_secs: 300,
            max_working_secs: 8 * 60 * 60,
            hold_blocked: false,
            keep_display_on: false,
            heartbeat_secs: 15,
            keeper_timeout_secs: 60,
            idle_exit_secs: 600,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        let config: Self = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        config.validate().with_context(|| format!("validating {}", path.display()))?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.poll_secs == 0 || self.heartbeat_secs == 0 {
            bail!("poll_secs and heartbeat_secs must be at least 1");
        }
        if self.keeper_timeout_secs < self.heartbeat_secs * 2 {
            bail!("keeper_timeout_secs must be at least twice heartbeat_secs");
        }
        Ok(())
    }

    pub fn poll(&self) -> Duration {
        Duration::from_secs(self.poll_secs)
    }

    pub fn grace(&self) -> Duration {
        Duration::from_secs(self.grace_secs)
    }

    pub fn max_working(&self) -> Duration {
        Duration::from_secs(self.max_working_secs)
    }

    pub fn heartbeat(&self) -> Duration {
        Duration::from_secs(self.heartbeat_secs)
    }

    pub fn keeper_timeout(&self) -> Duration {
        Duration::from_secs(self.keeper_timeout_secs)
    }

    pub fn idle_exit(&self) -> Duration {
        Duration::from_secs(self.idle_exit_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Config> {
        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    #[test]
    fn a_missing_file_uses_the_defaults() {
        let config = Config::load(Path::new("/nonexistent/herdr-keep-awake.toml")).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn a_partial_file_overrides_only_what_it_names() {
        let config = parse("grace_secs = 60\nhold_blocked = true\n").unwrap();
        assert_eq!(config.grace_secs, 60);
        assert!(config.hold_blocked);
        assert_eq!(config.poll_secs, Config::default().poll_secs);
    }

    #[test]
    fn a_misspelled_key_is_rejected() {
        assert!(parse("grace_seconds = 60\n").is_err());
    }

    #[test]
    fn a_keeper_timeout_that_a_single_late_heartbeat_would_trip_is_rejected() {
        assert!(parse("heartbeat_secs = 15\nkeeper_timeout_secs = 20\n").is_err());
        assert!(parse("poll_secs = 0\n").is_err());
    }
}
