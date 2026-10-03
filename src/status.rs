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
        let holding = self.keeper.as_ref().is_some_and(|keeper| keeper.phase == "holding");
        let headline = match (self.hold, holding) {
            (true, true) => "HOLDING: Windows will not sleep",
            (true, false) => "WANTED: starting the keeper",
            (false, true) => "RELEASING",
            (false, false) => "idle: Windows may sleep",
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
