use std::fmt::Write;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::util::fmt_duration;

/// What `status` reports. The daemon republishes it every tick.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Status {
    pub daemon_pid: u32,
    pub uptime_secs: u64,
    pub hold: bool,
    pub reason: String,
    pub active: Vec<String>,
    pub stale: Vec<String>,
    pub keeper: Option<KeeperStatus>,
    pub running_servers: usize,
    pub last_poll_secs_ago: u64,
    pub t3_errors: Vec<String>,
    /// Older daemons do not report warnings.
    #[serde(default)]
    pub t3_warnings: Vec<String>,
    pub keeper_failures: u32,
    pub retry_in_secs: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct KeeperStatus {
    pub phase: String,
    pub for_secs: u64,
    pub wsl_pid: u32,
    pub windows_pid: Option<u32>,
}

impl Status {
    pub fn render(&self) -> String {
        let mut out = String::new();
        let phase = self.keeper.as_ref().map(|keeper| keeper.phase.as_str());
        let headline = match (self.hold, phase) {
            (true, Some("holding")) => "HOLDING: Windows will not sleep",
            (true, Some("releasing")) => "WANTED: waiting for the old keeper to exit",
            (true, _) => "WANTED: starting the keeper",
            (false, None) => "idle: Windows may sleep",
            // Until the keeper exits, Windows may still be held.
            (false, _) => "RELEASING",
        };
        let _ = writeln!(out, "state    {headline}");
        let _ = writeln!(out, "reason   {}", self.reason);
        list(&mut out, "working", &self.active);
        list(&mut out, "stuck", &self.stale);
        match &self.keeper {
            Some(keeper) => {
                let windows = keeper.windows_pid.map_or(String::new(), |pid| format!(", Windows pid {pid}"));
                let _ = writeln!(
                    out,
                    "keeper   {} for {} (WSL pid {}{windows})",
                    keeper.phase,
                    secs(keeper.for_secs),
                    keeper.wsl_pid
                );
            }
            None => {
                let _ = writeln!(out, "keeper   not running");
            }
        }
        if self.keeper_failures > 0 {
            let retry = self.retry_in_secs.map_or(String::new(), |s| format!(", retrying in {}", secs(s)));
            let _ = writeln!(out, "failures {} in a row{retry}", self.keeper_failures);
        }
        let _ = writeln!(
            out,
            "T3       {} live server(s), polled {} ago",
            self.running_servers,
            secs(self.last_poll_secs_ago)
        );
        list(&mut out, "errors", &self.t3_errors);
        list(&mut out, "warnings", &self.t3_warnings);
        let _ = writeln!(out, "daemon   pid {}, up {}", self.daemon_pid, secs(self.uptime_secs));
        out
    }
}

fn list(out: &mut String, label: &str, items: &[String]) {
    for (i, item) in items.iter().enumerate() {
        let label = if i == 0 { label } else { "" };
        let _ = writeln!(out, "{label:<8} {item}");
    }
}

fn secs(n: u64) -> String {
    fmt_duration(Duration::from_secs(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headline(hold: bool, phase: Option<&str>) -> String {
        let keeper =
            phase.map(|phase| KeeperStatus { phase: phase.into(), for_secs: 0, wsl_pid: 1, windows_pid: None });
        Status { hold, keeper, ..Status::default() }.render().lines().next().unwrap().to_string()
    }

    #[test]
    fn the_headline_follows_the_keeper_as_well_as_the_wanted_hold() {
        assert_eq!(headline(true, Some("holding")), "state    HOLDING: Windows will not sleep");
        assert_eq!(headline(true, Some("releasing")), "state    WANTED: waiting for the old keeper to exit");
        assert_eq!(headline(true, None), "state    WANTED: starting the keeper");
        assert_eq!(headline(false, Some("releasing")), "state    RELEASING");
        assert_eq!(headline(false, None), "state    idle: Windows may sleep");
    }
}
