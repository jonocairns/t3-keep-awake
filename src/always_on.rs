use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::util::{fmt_duration, timestamp};

/// A manual hold that keeps Windows awake whatever T3 is doing. It is saved
/// to disk so an open-ended one survives daemon restarts and reboots; one with
/// an end time lapses on its own, even if it ends while the daemon is down.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AlwaysOn {
    /// Unix seconds; `None` holds until turned off.
    pub until: Option<u64>,
}

impl AlwaysOn {
    pub fn starting(now: SystemTime, duration: Option<Duration>) -> Self {
        Self { until: duration.map(|duration| unix_secs(now).saturating_add(duration.as_secs())) }
    }

    pub fn load(path: &Path) -> Result<Option<Self>> {
        match fs::read(path) {
            Ok(text) => serde_json::from_slice(&text).map(Some).with_context(|| format!("parsing {}", path.display())),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let temporary = path.with_extension("tmp");
        fs::write(&temporary, serde_json::to_vec(self)?).with_context(|| format!("writing {}", temporary.display()))?;
        fs::rename(&temporary, path).with_context(|| format!("replacing {}", path.display()))
    }

    pub fn clear(path: &Path) -> Result<()> {
        match fs::remove_file(path) {
            Err(error) if error.kind() != ErrorKind::NotFound => {
                Err(error).with_context(|| format!("removing {}", path.display()))
            }
            _ => Ok(()),
        }
    }

    pub fn expired(&self, now: SystemTime) -> bool {
        self.until.is_some_and(|until| unix_secs(now) >= until)
    }

    pub fn left(&self, now: SystemTime) -> Option<Duration> {
        self.until.map(|until| Duration::from_secs(until.saturating_sub(unix_secs(now))))
    }

    pub fn until_timestamp(&self) -> Option<String> {
        self.until.map(|until| timestamp(UNIX_EPOCH + Duration::from_secs(until)))
    }

    pub fn describe(&self, now: SystemTime) -> String {
        match (self.left(now), self.until_timestamp()) {
            (Some(left), Some(until)) => format!("always on for {} more (until {until})", fmt_duration(left)),
            _ => "always on until turned off".to_string(),
        }
    }
}

fn unix_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn a_timed_hold_lapses_and_an_open_one_does_not() {
        let timed = AlwaysOn::starting(at(1000), Some(Duration::from_secs(3600)));
        assert_eq!(timed.until, Some(4600));
        assert!(!timed.expired(at(4599)));
        assert_eq!(timed.left(at(4599)), Some(Duration::from_secs(1)));
        assert!(timed.expired(at(4600)));

        let open = AlwaysOn::starting(at(1000), None);
        assert!(!open.expired(at(u64::MAX / 2)));
        assert_eq!(open.describe(at(1000)), "always on until turned off");
        assert_eq!(timed.describe(at(1000)), "always on for 1h00m more (until 1970-01-01T01:16:40Z)");
    }

    #[test]
    fn it_round_trips_through_its_file() {
        let dir = std::env::temp_dir().join(format!("tka-always-on-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("always-on.json");
        assert_eq!(AlwaysOn::load(&path).unwrap(), None);
        let hold = AlwaysOn { until: Some(42) };
        hold.save(&path).unwrap();
        assert_eq!(AlwaysOn::load(&path).unwrap(), Some(hold));
        AlwaysOn::clear(&path).unwrap();
        AlwaysOn::clear(&path).unwrap();
        assert_eq!(AlwaysOn::load(&path).unwrap(), None);
        fs::write(&path, "{\"until\": \"soon\"}").unwrap();
        assert!(AlwaysOn::load(&path).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
