use std::collections::{HashMap, HashSet};
use std::time::Instant;

use serde::Deserialize;

use crate::config::Config;
use crate::util::fmt_duration;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThreadStatus {
    Idle,
    Working,
    Blocked,
    Done,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug)]
pub struct Thread {
    /// `server/thread`, unique across T3 servers.
    pub id: String,
    pub label: String,
    pub status: ThreadStatus,
    /// A turn identity: the same `(id, seq)` identifies one uninterrupted turn.
    pub seq: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Decision {
    pub hold: bool,
    pub reason: String,
    pub active: Vec<String>,
    pub stale: Vec<String>,
}

/// Turns successive T3 snapshots into a hold decision. It is level
/// triggered: every call looks at the whole snapshot, so a missed or
/// duplicated event can never leave it in the wrong state.
#[derive(Default)]
pub struct Tracker {
    stretches: HashMap<(String, u64), Instant>,
    last_active: Option<Instant>,
}

impl Tracker {
    /// `complete` is false when the T3 database could not be read, so an
    /// thread missing from `threads` may still be working.
    pub fn observe(&mut self, threads: &[Thread], complete: bool, now: Instant, config: &Config) -> Decision {
        let mut active = Vec::new();
        let mut stale = Vec::new();
        let mut current = HashSet::new();
        for thread in threads {
            let counts = thread.status == ThreadStatus::Working
                || (config.hold_blocked && thread.status == ThreadStatus::Blocked);
            if !counts {
                continue;
            }
            let key = (thread.id.clone(), thread.seq);
            let since = *self.stretches.entry(key.clone()).or_insert(now);
            current.insert(key);
            if now.duration_since(since) >= config.max_working() {
                stale.push(thread.label.clone());
            } else {
                active.push(thread.label.clone());
            }
        }
        // An incomplete read keeps the stretches it could not see, so a failed
        // read cannot reset their stuck timers.
        if complete {
            self.stretches.retain(|key, _| current.contains(key));
        }

        if !active.is_empty() {
            self.last_active = Some(now);
            let reason = match active.len() {
                1 => "1 thread working".to_string(),
                n => format!("{n} threads working"),
            };
            return Decision { hold: true, reason, active, stale };
        }

        let quiet_for = self.last_active.map(|at| now.duration_since(at));
        let reason = match quiet_for {
            Some(quiet) if quiet < config.grace() => {
                let why = if complete { "no threads working" } else { "T3 partly unreadable" };
                let left = fmt_duration(config.grace() - quiet);
                return Decision { hold: true, reason: format!("{why}; grace period, {left} left"), active, stale };
            }
            _ if !complete => "T3 unreadable".to_string(),
            _ if !stale.is_empty() => {
                format!("only threads working longer than {} (treated as stuck)", fmt_duration(config.max_working()))
            }
            _ => "no threads working".to_string(),
        };
        Decision { hold: false, reason, active, stale }
    }

    /// A stopped or unresponsive server cannot authorize a grace hold. Stuck
    /// timers survive: the same server may answer the next poll, and a
    /// restarted server's turns have new identities anyway.
    pub fn server_offline(&mut self) {
        self.last_active = None;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn config() -> Config {
        Config { grace_secs: 300, max_working_secs: 3600, ..Config::default() }
    }

    fn thread(id: &str, status: ThreadStatus, seq: u64) -> Thread {
        Thread { id: id.to_string(), label: id.to_string(), status, seq }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn a_working_thread_holds() {
        let t0 = Instant::now();
        let decision = Tracker::default().observe(&[thread("a", ThreadStatus::Working, 1)], true, t0, &config());
        assert!(decision.hold);
        assert_eq!(decision.reason, "1 thread working");
        assert_eq!(decision.active, ["a"]);
    }

    #[test]
    fn nothing_ever_working_does_not_hold() {
        let decision =
            Tracker::default().observe(&[thread("a", ThreadStatus::Idle, 1)], true, Instant::now(), &config());
        assert!(!decision.hold);
        assert_eq!(decision.reason, "no threads working");
    }

    #[test]
    fn the_hold_outlasts_the_last_working_thread_by_the_grace_period() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        tracker.observe(&[thread("a", ThreadStatus::Working, 1)], true, t0, &config());

        let idle = [thread("a", ThreadStatus::Idle, 2)];
        let within = tracker.observe(&idle, true, t0 + secs(299), &config());
        assert!(within.hold);
        assert_eq!(within.reason, "no threads working; grace period, 1s left");

        let after = tracker.observe(&idle, true, t0 + secs(300), &config());
        assert!(!after.hold);
    }

    #[test]
    fn any_one_of_several_threads_working_holds() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        let both = [thread("a", ThreadStatus::Working, 1), thread("b", ThreadStatus::Working, 2)];
        assert_eq!(tracker.observe(&both, true, t0, &config()).reason, "2 threads working");

        let one = [thread("a", ThreadStatus::Done, 3), thread("b", ThreadStatus::Working, 2)];
        let decision = tracker.observe(&one, true, t0 + secs(600), &config());
        assert!(decision.hold);
        assert_eq!(decision.active, ["b"]);
    }

    #[test]
    fn a_blocked_thread_counts_only_when_configured() {
        let blocked = [thread("a", ThreadStatus::Blocked, 1)];
        assert!(!Tracker::default().observe(&blocked, true, Instant::now(), &config()).hold);

        let config = Config { hold_blocked: true, ..config() };
        assert!(Tracker::default().observe(&blocked, true, Instant::now(), &config).hold);
    }

    #[test]
    fn a_stretch_longer_than_the_cap_stops_counting() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        let stuck = [thread("a", ThreadStatus::Working, 1)];
        tracker.observe(&stuck, true, t0, &config());
        assert!(tracker.observe(&stuck, true, t0 + secs(3599), &config()).hold);

        // Stale from 3600s; the grace period still runs from the last time it counted.
        let decision = tracker.observe(&stuck, true, t0 + secs(3599 + 300), &config());
        assert!(!decision.hold);
        assert_eq!(decision.stale, ["a"]);
        assert!(decision.reason.contains("treated as stuck"), "{}", decision.reason);
    }

    #[test]
    fn a_new_status_transition_restarts_the_stuck_timer() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        tracker.observe(&[thread("a", ThreadStatus::Working, 1)], true, t0, &config());
        tracker.observe(&[thread("a", ThreadStatus::Idle, 2)], true, t0 + secs(3000), &config());
        let decision = tracker.observe(&[thread("a", ThreadStatus::Working, 3)], true, t0 + secs(4000), &config());
        assert!(decision.hold);
        assert!(decision.stale.is_empty());
    }

    #[test]
    fn an_unreadable_t3_holds_only_through_the_grace_period() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        tracker.observe(&[thread("a", ThreadStatus::Working, 1)], true, t0, &config());

        let during = tracker.observe(&[], false, t0 + secs(60), &config());
        assert!(during.hold);
        assert!(during.reason.starts_with("T3 partly unreadable"), "{}", during.reason);

        let after = tracker.observe(&[], false, t0 + secs(300), &config());
        assert!(!after.hold);
        assert_eq!(after.reason, "T3 unreadable");
    }

    #[test]
    fn confirmed_work_holds_even_when_the_snapshot_is_incomplete() {
        let decision =
            Tracker::default().observe(&[thread("a", ThreadStatus::Working, 1)], false, Instant::now(), &config());
        assert!(decision.hold);
    }

    #[test]
    fn an_incomplete_read_keeps_the_stuck_timer_of_a_thread_it_could_not_see() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        let stuck = [thread("a", ThreadStatus::Working, 1)];
        tracker.observe(&stuck, true, t0, &config());
        tracker.observe(&[], false, t0 + secs(10), &config());
        let decision = tracker.observe(&stuck, true, t0 + secs(3600), &config());
        assert_eq!(decision.stale, ["a"]);
    }

    #[test]
    fn an_offline_server_ends_the_grace_period_but_not_the_stuck_timer() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        let stuck = [thread("a", ThreadStatus::Working, 1)];
        tracker.observe(&stuck, true, t0, &config());
        tracker.server_offline();
        assert!(!tracker.observe(&[], false, t0 + secs(10), &config()).hold);

        let decision = tracker.observe(&stuck, true, t0 + secs(3600), &config());
        assert_eq!(decision.stale, ["a"]);
    }
}
