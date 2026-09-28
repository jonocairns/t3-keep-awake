use std::collections::{HashMap, HashSet};
use std::time::Instant;

use serde::Deserialize;

use crate::config::Config;
use crate::util::fmt_duration;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug)]
pub struct Agent {
    /// `session/pane`, unique across herdr sessions.
    pub id: String,
    pub label: String,
    pub status: AgentStatus,
    /// herdr's `state_change_seq`: changes on every status transition, so the
    /// same `(id, seq)` means one uninterrupted stretch in the same status.
    pub seq: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Decision {
    pub hold: bool,
    pub reason: String,
    pub active: Vec<String>,
    pub stale: Vec<String>,
}

/// Turns successive herdr snapshots into a hold decision. It is level
/// triggered: every call looks at the whole snapshot, so a missed or
/// duplicated event can never leave it in the wrong state.
#[derive(Default)]
pub struct Tracker {
    stretches: HashMap<(String, u64), Instant>,
    last_active: Option<Instant>,
}

impl Tracker {
    /// `complete` is false when some herdr session could not be read, so an
    /// agent missing from `agents` may still be working.
    pub fn observe(&mut self, agents: &[Agent], complete: bool, now: Instant, config: &Config) -> Decision {
        let mut active = Vec::new();
        let mut stale = Vec::new();
        let mut current = HashSet::new();
        for agent in agents {
            let counts =
                agent.status == AgentStatus::Working || (config.hold_blocked && agent.status == AgentStatus::Blocked);
            if !counts {
                continue;
            }
            let key = (agent.id.clone(), agent.seq);
            let since = *self.stretches.entry(key.clone()).or_insert(now);
            current.insert(key);
            if now.duration_since(since) >= config.max_working() {
                stale.push(agent.label.clone());
            } else {
                active.push(agent.label.clone());
            }
        }
        // An incomplete read keeps the stretches it could not see, so a flaky
        // session cannot reset their stuck timers.
        if complete {
            self.stretches.retain(|key, _| current.contains(key));
        }

        if !active.is_empty() {
            self.last_active = Some(now);
            let reason = match active.len() {
                1 => "1 agent working".to_string(),
                n => format!("{n} agents working"),
            };
            return Decision { hold: true, reason, active, stale };
        }

        let quiet_for = self.last_active.map(|at| now.duration_since(at));
        let reason = match quiet_for {
            Some(quiet) if quiet < config.grace() => {
                let why = if complete { "no agents working" } else { "herdr partly unreadable" };
                let left = fmt_duration(config.grace() - quiet);
                return Decision { hold: true, reason: format!("{why}; grace period, {left} left"), active, stale };
            }
            _ if !complete => "herdr unreadable".to_string(),
            _ if !stale.is_empty() => {
                format!("only agents working longer than {} (treated as stuck)", fmt_duration(config.max_working()))
            }
            _ => "no agents working".to_string(),
        };
        Decision { hold: false, reason, active, stale }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn config() -> Config {
        Config { grace_secs: 300, max_working_secs: 3600, ..Config::default() }
    }

    fn agent(id: &str, status: AgentStatus, seq: u64) -> Agent {
        Agent { id: id.to_string(), label: id.to_string(), status, seq }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn a_working_agent_holds() {
        let t0 = Instant::now();
        let decision = Tracker::default().observe(&[agent("a", AgentStatus::Working, 1)], true, t0, &config());
        assert!(decision.hold);
        assert_eq!(decision.reason, "1 agent working");
        assert_eq!(decision.active, ["a"]);
    }

    #[test]
    fn nothing_ever_working_does_not_hold() {
        let decision = Tracker::default().observe(&[agent("a", AgentStatus::Idle, 1)], true, Instant::now(), &config());
        assert!(!decision.hold);
        assert_eq!(decision.reason, "no agents working");
    }

    #[test]
    fn the_hold_outlasts_the_last_working_agent_by_the_grace_period() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        tracker.observe(&[agent("a", AgentStatus::Working, 1)], true, t0, &config());

        let idle = [agent("a", AgentStatus::Idle, 2)];
        let within = tracker.observe(&idle, true, t0 + secs(299), &config());
        assert!(within.hold);
        assert_eq!(within.reason, "no agents working; grace period, 1s left");

        let after = tracker.observe(&idle, true, t0 + secs(300), &config());
        assert!(!after.hold);
    }

    #[test]
    fn any_one_of_several_agents_working_holds() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        let both = [agent("a", AgentStatus::Working, 1), agent("b", AgentStatus::Working, 2)];
        assert_eq!(tracker.observe(&both, true, t0, &config()).reason, "2 agents working");

        let one = [agent("a", AgentStatus::Done, 3), agent("b", AgentStatus::Working, 2)];
        let decision = tracker.observe(&one, true, t0 + secs(600), &config());
        assert!(decision.hold);
        assert_eq!(decision.active, ["b"]);
    }

    #[test]
    fn a_blocked_agent_counts_only_when_configured() {
        let blocked = [agent("a", AgentStatus::Blocked, 1)];
        assert!(!Tracker::default().observe(&blocked, true, Instant::now(), &config()).hold);

        let config = Config { hold_blocked: true, ..config() };
        assert!(Tracker::default().observe(&blocked, true, Instant::now(), &config).hold);
    }

    #[test]
    fn a_stretch_longer_than_the_cap_stops_counting() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        let stuck = [agent("a", AgentStatus::Working, 1)];
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
        tracker.observe(&[agent("a", AgentStatus::Working, 1)], true, t0, &config());
        tracker.observe(&[agent("a", AgentStatus::Idle, 2)], true, t0 + secs(3000), &config());
        let decision = tracker.observe(&[agent("a", AgentStatus::Working, 3)], true, t0 + secs(4000), &config());
        assert!(decision.hold);
        assert!(decision.stale.is_empty());
    }

    #[test]
    fn an_unreadable_herdr_holds_only_through_the_grace_period() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        tracker.observe(&[agent("a", AgentStatus::Working, 1)], true, t0, &config());

        let during = tracker.observe(&[], false, t0 + secs(60), &config());
        assert!(during.hold);
        assert!(during.reason.starts_with("herdr partly unreadable"), "{}", during.reason);

        let after = tracker.observe(&[], false, t0 + secs(300), &config());
        assert!(!after.hold);
        assert_eq!(after.reason, "herdr unreadable");
    }

    #[test]
    fn a_readable_session_with_a_working_agent_holds_even_if_another_session_failed() {
        let decision =
            Tracker::default().observe(&[agent("a", AgentStatus::Working, 1)], false, Instant::now(), &config());
        assert!(decision.hold);
    }

    #[test]
    fn an_incomplete_read_keeps_the_stuck_timer_of_an_agent_it_could_not_see() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        let stuck = [agent("a", AgentStatus::Working, 1)];
        tracker.observe(&stuck, true, t0, &config());
        tracker.observe(&[], false, t0 + secs(10), &config());
        let decision = tracker.observe(&stuck, true, t0 + secs(3600), &config());
        assert_eq!(decision.stale, ["a"]);
    }
}
