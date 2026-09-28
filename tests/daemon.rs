//! End-to-end tests of the real binary against a fake `herdr` and a fake
//! keeper that speaks the keeper protocol. Windows itself is out of scope;
//! `herdr-keep-awake probe` covers that on a real machine.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_herdr-keep-awake");

const FAKE_HERDR: &str = r#"#!/bin/sh
if [ "$1 $2 $3" = "session list --json" ]; then exec cat "$FAKE_DIR/sessions.json"; fi
if [ "$1" = "--session" ] && [ "$3 $4" = "agent list" ]; then exec cat "$FAKE_DIR/agents-$2.json"; fi
echo "fake herdr: unexpected args: $*" >&2
exit 2
"#;

const FAKE_KEEPER: &str = r#"#!/bin/sh
echo "start $$" >> "$FAKE_DIR/keeper.log"
echo "holding $$"
while IFS= read -r line; do echo pong; done
# Record the exit first: with the daemon dead, the next write raises SIGPIPE.
# keeper.ps1 likewise releases before it says so.
echo "exit $$" >> "$FAKE_DIR/keeper.log"
echo "released stdin-closed"
"#;

const CONFIG: &str = "poll_secs = 1\ngrace_secs = 2\nheartbeat_secs = 1\nkeeper_timeout_secs = 3\n";

struct Harness {
    dir: PathBuf,
}

impl Harness {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hka-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("state")).unwrap();
        write_executable(&dir.join("herdr"), FAKE_HERDR);
        write_executable(&dir.join("keeper"), FAKE_KEEPER);
        fs::write(dir.join("config.toml"), CONFIG).unwrap();
        fs::write(dir.join("sessions.json"), r#"{"sessions":[{"default":true,"name":"default","running":true}]}"#)
            .unwrap();
        let harness = Self { dir };
        harness.set_agents(&[]);
        harness
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        command
            .args(args)
            .env("HERDR_KEEP_AWAKE_STATE_DIR", self.dir.join("state"))
            .env("HERDR_KEEP_AWAKE_CONFIG", self.dir.join("config.toml"))
            .env("HERDR_KEEP_AWAKE_KEEPER", self.dir.join("keeper"))
            .env("HERDR_BIN_PATH", self.dir.join("herdr"))
            .env("FAKE_DIR", &self.dir);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        let output = self.command(args).output().unwrap();
        assert!(
            output.status.success(),
            "`{}` failed: {}{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    /// `(pane, status)` pairs; `seq` is fixed per pane so a stretch is stable.
    fn set_agents(&self, agents: &[(&str, &str)]) {
        let agents: Vec<Value> = agents
            .iter()
            .enumerate()
            .map(|(i, (pane, status))| {
                serde_json::json!({"agent": "claude", "pane_id": pane, "agent_status": status, "state_change_seq": i})
            })
            .collect();
        let json = serde_json::json!({"id": "cli:agent:list", "result": {"agents": agents}});
        // Write then rename, so the fake never serves a half-written file.
        let tmp = self.dir.join("agents.tmp");
        fs::write(&tmp, json.to_string()).unwrap();
        fs::rename(&tmp, self.dir.join("agents-default.json")).unwrap();
    }

    fn status(&self) -> Option<Value> {
        let output = self.command(&["status", "--json"]).output().unwrap();
        output.status.success().then(|| serde_json::from_slice(&output.stdout).unwrap())
    }

    fn wait_for(&self, what: &str, ready: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.status().filter(|status| ready(status)) {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; last status: {:?}\nlog:\n{}",
                self.status(),
                self.log()
            );
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_for_keeper_log(&self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !self.keeper_log().contains(needle) {
            assert!(Instant::now() < deadline, "keeper log never contained {needle:?}:\n{}", self.keeper_log());
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn keeper_log(&self) -> String {
        fs::read_to_string(self.dir.join("keeper.log")).unwrap_or_default()
    }

    fn log(&self) -> String {
        fs::read_to_string(self.dir.join("state/daemon.log")).unwrap_or_default()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.command(&["stop"]).output();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn write_executable(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn holding(status: &Value) -> bool {
    status["hold"] == true && status["keeper"]["phase"] == "holding"
}

fn keeper_pid(status: &Value) -> u64 {
    status["keeper"]["wsl_pid"].as_u64().unwrap()
}

fn kill(pid: u64) {
    assert!(Command::new("kill").args(["-9", &pid.to_string()]).status().unwrap().success());
}

#[test]
fn holds_while_any_agent_works_and_releases_after_the_grace_period() {
    let h = Harness::new("hold");
    h.set_agents(&[("w1:p1", "idle"), ("w1:p2", "working")]);
    h.run(&["event"]);
    let status = h.wait_for("the hold", holding);
    assert_eq!(status["reason"], "1 agent working");
    assert_eq!(status["active"][0], "default/w1:p2 claude");

    h.set_agents(&[("w1:p1", "idle"), ("w1:p2", "done")]);
    h.run(&["event"]);
    let status = h.wait_for("the grace period", |s| s["reason"].as_str().unwrap().contains("grace period"));
    assert_eq!(status["hold"], true);

    h.wait_for("the release", |s| s["hold"] == false && s["keeper"].is_null());
    let pid = keeper_pid(&status);
    h.wait_for_keeper_log(&format!("exit {pid}"));
}

#[test]
fn a_keeper_that_dies_is_replaced() {
    let h = Harness::new("respawn");
    h.set_agents(&[("w1:p1", "working")]);
    h.run(&["event"]);
    let first = keeper_pid(&h.wait_for("the first keeper", holding));

    kill(first);
    let second = h.wait_for("a replacement keeper", |s| holding(s) && keeper_pid(s) != first);
    assert_eq!(second["keeper_failures"], 1);
    assert!(h.log().contains("keeper exited unexpectedly"));
}

#[test]
fn a_killed_daemon_releases_its_keeper_and_the_next_hook_restarts_it() {
    let h = Harness::new("orphan");
    h.set_agents(&[("w1:p1", "working")]);
    h.run(&["event"]);
    let status = h.wait_for("the hold", holding);

    kill(status["daemon_pid"].as_u64().unwrap());
    // No cleanup ran: the keeper lets go only because its stdin closed.
    h.wait_for_keeper_log(&format!("exit {}", keeper_pid(&status)));

    h.run(&["event"]);
    let restarted = h.wait_for("a new daemon holding", holding);
    assert_ne!(restarted["daemon_pid"], status["daemon_pid"]);
}

#[test]
fn concurrent_hooks_start_exactly_one_daemon() {
    let h = Harness::new("race");
    let hooks: Vec<_> = (0..8).map(|_| h.command(&["event"]).spawn().unwrap()).collect();
    for mut hook in hooks {
        assert!(hook.wait().unwrap().success());
    }
    // The hook returns once the daemon answers, and it only answers after its
    // first poll, so status is already real here.
    let status = h.status().expect("daemon answering");
    assert_ne!(status["daemon_pid"], 0);
    assert_eq!(status["running_sessions"], 1);
    assert_eq!(h.log().matches("started: pid").count(), 1, "log:\n{}", h.log());
}

#[test]
fn stop_releases_the_hold_and_restart_resumes_it() {
    let h = Harness::new("stop");
    h.set_agents(&[("w1:p1", "working")]);
    h.run(&["event"]);
    let status = h.wait_for("the hold", holding);

    let output = h.run(&["restart"]);
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "started");
    h.wait_for_keeper_log(&format!("exit {}", keeper_pid(&status)));
    let restarted = h.wait_for("the hold after restart", holding);
    assert_ne!(restarted["daemon_pid"], status["daemon_pid"]);

    let output = h.run(&["stop"]);
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "stopped");
    h.wait_for_keeper_log(&format!("exit {}", keeper_pid(&restarted)));
    assert!(h.status().is_none());
}

#[test]
fn an_unreadable_herdr_releases_after_the_grace_period() {
    let h = Harness::new("broken");
    h.set_agents(&[("w1:p1", "working")]);
    h.run(&["event"]);
    h.wait_for("the hold", holding);

    fs::write(h.dir.join("agents-default.json"), "not json").unwrap();
    h.wait_for("the release", |s| s["hold"] == false && s["reason"] == "herdr unreadable");
    assert!(h.log().contains("herdr: session default: parsing agent list"));
}
