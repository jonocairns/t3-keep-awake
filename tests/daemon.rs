//! Drive the real binary against T3's SQLite projections and a live HTTP
//! listener, using a fake keeper so tests never change Windows power state.

use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_t3-keep-awake");
const STARTED_AT: &str = "2026-10-03T09:00:00.000Z";
const FAKE_KEEPER: &str = r#"#!/bin/sh
echo "start $$" >> "$FAKE_DIR/keeper.log"
echo "holding $$"
while IFS= read -r line; do echo pong; done
# Release before writing to a possibly closed stdout.
echo "exit $$" >> "$FAKE_DIR/keeper.log"
echo "released stdin-closed"
"#;
const CONFIG: &str = "poll_secs = 1\nidle_poll_secs = 1\ngrace_secs = 2\nheartbeat_secs = 1\nkeeper_timeout_secs = 3\n";

struct Harness {
    dir: PathBuf,
    database: Connection,
    port: u16,
    respond: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    server: Option<thread::JoinHandle<()>>,
}

impl Harness {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tka-{name}-{}", std::process::id()));
        fs::create_dir_all(dir.join("state")).unwrap();
        fs::create_dir_all(dir.join("userdata")).unwrap();
        fs::write(dir.join("keeper"), FAKE_KEEPER).unwrap();
        fs::set_permissions(dir.join("keeper"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(dir.join("config.toml"), CONFIG).unwrap();
        let database = Connection::open(dir.join("userdata/state.sqlite")).unwrap();
        database.pragma_update(None, "journal_mode", "WAL").unwrap();
        database.execute_batch(include_str!("fixtures/schema.sql")).unwrap();

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let respond = Arc::new(AtomicBool::new(true));
        let shutdown = Arc::new(AtomicBool::new(false));
        let server_respond = Arc::clone(&respond);
        let server_shutdown = Arc::clone(&shutdown);
        let server = thread::spawn(move || {
            while !server_shutdown.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
                        stream.set_write_timeout(Some(Duration::from_millis(200))).unwrap();
                        let mut request = [0; 512];
                        let _ = stream.read(&mut request);
                        if server_respond.load(Ordering::SeqCst) {
                            let _ =
                                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("test server: {error}"),
                }
            }
        });
        let harness = Self { dir, database, port, respond, shutdown, server: Some(server) };
        harness.set_server_running(true);
        harness
    }

    fn set_server_running(&self, running: bool) {
        let path = self.dir.join("userdata/server-runtime.json");
        if !running {
            fs::remove_file(path).unwrap();
            return;
        }
        let runtime = serde_json::json!({
            "version": 1, "pid": std::process::id(), "port": self.port, "startedAt": STARTED_AT
        });
        fs::write(path.with_extension("tmp"), runtime.to_string()).unwrap();
        fs::rename(path.with_extension("tmp"), path).unwrap();
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        command
            .args(args)
            .env("T3_KEEP_AWAKE_STATE_DIR", self.dir.join("state"))
            .env("T3_KEEP_AWAKE_CONFIG", self.dir.join("config.toml"))
            .env("T3_KEEP_AWAKE_KEEPER", self.dir.join("keeper"))
            .env("T3_KEEP_AWAKE_DATA_DIR", self.dir.join("userdata"))
            .env("FAKE_DIR", &self.dir);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        let output = self.command(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}: {}{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn set_threads(&self, threads: &[(&str, &str)]) {
        let transaction = self.database.unchecked_transaction().unwrap();
        transaction
            .execute_batch(
                "DELETE FROM projection_threads; DELETE FROM projection_thread_sessions;
             DELETE FROM provider_session_runtime; DELETE FROM projection_turns;",
            )
            .unwrap();
        for (id, state) in threads {
            let turn = format!("turn-{id}");
            let running = matches!(*state, "working" | "blocked");
            transaction
                .execute(
                    "INSERT INTO projection_threads(thread_id,title,pending_approval_count) VALUES (?1,?1,?2)",
                    rusqlite::params![id, *state == "blocked"],
                )
                .unwrap();
            transaction
                .execute(
                    "INSERT INTO projection_thread_sessions VALUES (?1,?2,?3)",
                    rusqlite::params![id, if running { "running" } else { "ready" }, turn],
                )
                .unwrap();
            transaction
                .execute(
                    "INSERT INTO provider_session_runtime VALUES (?1,'codex','running',?2,?3)",
                    rusqlite::params![id, STARTED_AT, serde_json::json!({"activeTurnId":turn}).to_string()],
                )
                .unwrap();
            transaction
                .execute(
                    "INSERT INTO projection_turns(thread_id,turn_id,state) VALUES (?1,?2,?3)",
                    rusqlite::params![id, turn, if running { "running" } else { "completed" }],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
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
            assert!(Instant::now() < deadline, "waiting for {what}; status: {:?}\nlog:\n{}", self.status(), self.log());
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn keeper_log(&self) -> String {
        fs::read_to_string(self.dir.join("keeper.log")).unwrap_or_default()
    }

    fn wait_for_keeper_exit(&self, pid: u64) {
        let needle = format!("exit {pid}");
        let deadline = Instant::now() + Duration::from_secs(15);
        while !self.keeper_log().contains(&needle) {
            assert!(Instant::now() < deadline, "keeper did not release: {}", self.keeper_log());
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn log(&self) -> String {
        fs::read_to_string(self.dir.join("state/daemon.log")).unwrap_or_default()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.command(&["stop"]).output();
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
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
fn holds_for_running_turns_and_releases_when_the_last_turn_finishes() {
    let h = Harness::new("hold");
    h.set_threads(&[("thread-1", "working"), ("thread-2", "working")]);
    h.run(&["start"]);
    let status = h.wait_for("both turns", |s| holding(s) && s["active"].as_array().unwrap().len() == 2);
    let pid = keeper_pid(&status);
    h.set_threads(&[("thread-1", "done"), ("thread-2", "working")]);
    h.wait_for("the remaining turn", |s| holding(s) && s["active"].as_array().unwrap().len() == 1);
    h.set_threads(&[("thread-1", "done"), ("thread-2", "done")]);
    h.wait_for("the grace period", |s| s["hold"] == true && s["reason"].as_str().unwrap().contains("grace period"));
    h.wait_for("the release", |s| s["hold"] == false && s["keeper"].is_null());
    h.wait_for_keeper_exit(pid);
}

#[test]
fn an_open_idle_t3_process_does_not_keep_windows_awake() {
    let h = Harness::new("idle");
    h.set_threads(&[("idle-thread", "idle")]);
    h.run(&["start"]);
    h.wait_for("the idle server", |s| s["running_servers"] == 1 && s["hold"] == false);
    assert!(h.keeper_log().is_empty());
}

#[test]
fn permission_prompts_do_not_hold_by_default_and_can_be_enabled() {
    let h = Harness::new("blocked");
    h.set_threads(&[("blocked-thread", "blocked")]);
    h.run(&["start"]);
    h.wait_for("the blocked turn", |s| s["running_servers"] == 1 && s["hold"] == false);
    fs::write(h.dir.join("config.toml"), format!("{CONFIG}hold_blocked = true\n")).unwrap();
    h.run(&["restart"]);
    h.wait_for("the optional blocked hold", holding);
}

#[test]
fn a_keeper_that_dies_is_replaced() {
    let h = Harness::new("respawn");
    h.set_threads(&[("thread", "working")]);
    h.run(&["start"]);
    let first = keeper_pid(&h.wait_for("the first keeper", holding));
    kill(first);
    let next = h.wait_for("the replacement", |s| holding(s) && keeper_pid(s) != first);
    assert_ne!(keeper_pid(&next), first);
    assert!(h.log().contains("keeper exited unexpectedly"));
}

#[test]
fn a_killed_daemon_releases_its_keeper_and_start_recovers_it() {
    let h = Harness::new("orphan");
    h.set_threads(&[("thread", "working")]);
    h.run(&["start"]);
    let status = h.wait_for("the hold", holding);
    kill(status["daemon_pid"].as_u64().unwrap());
    h.wait_for_keeper_exit(keeper_pid(&status));
    h.run(&["start"]);
    let restarted = h.wait_for("the new daemon", holding);
    assert_ne!(restarted["daemon_pid"], status["daemon_pid"]);
}

#[test]
fn concurrent_starts_create_exactly_one_controller() {
    let h = Harness::new("race");
    let clients: Vec<_> = (0..8).map(|_| h.command(&["start"]).spawn().unwrap()).collect();
    for mut client in clients {
        assert!(client.wait().unwrap().success());
    }
    h.wait_for("the first snapshot", |s| s["running_servers"] == 1);
    assert_eq!(h.log().matches("started: pid").count(), 1);
}

#[test]
fn stop_releases_the_hold_and_restart_resumes_it() {
    let h = Harness::new("stop");
    h.set_threads(&[("thread", "working")]);
    h.run(&["start"]);
    let first = h.wait_for("the hold", holding);
    h.run(&["restart"]);
    h.wait_for_keeper_exit(keeper_pid(&first));
    let next = h.wait_for("the hold after restart", holding);
    assert_ne!(next["daemon_pid"], first["daemon_pid"]);
    h.run(&["stop"]);
    h.wait_for_keeper_exit(keeper_pid(&next));
    assert!(h.status().is_none());
}

#[test]
fn an_incompatible_database_releases_and_recovers_without_restart() {
    let h = Harness::new("schema");
    h.set_threads(&[("thread", "working")]);
    h.run(&["start"]);
    h.wait_for("the hold", holding);
    h.database.execute_batch("ALTER TABLE projection_threads RENAME TO incompatible_threads;").unwrap();
    h.wait_for("the release", |s| s["hold"] == false && s["reason"] == "T3 unreadable");
    assert!(h.log().contains("database schema may have changed"));
    h.database.execute_batch("ALTER TABLE incompatible_threads RENAME TO projection_threads;").unwrap();
    h.wait_for("the recovered hold", holding);
}

#[test]
fn the_controller_discovers_a_returning_server_without_hooks() {
    let h = Harness::new("discovery");
    h.set_server_running(false);
    h.set_threads(&[("thread", "working")]);
    h.run(&["start"]);
    let idle = h.wait_for("the absent server", |s| s["reason"] == "T3 server not running");
    h.set_server_running(true);
    let active = h.wait_for("the discovered turn", holding);
    assert_eq!(idle["daemon_pid"], active["daemon_pid"]);
}

#[test]
fn a_stopped_or_unresponsive_server_cannot_hold_using_persisted_running_turns() {
    let h = Harness::new("liveness");
    h.set_threads(&[("thread", "working")]);
    h.run(&["start"]);
    let first = h.wait_for("the initial hold", holding);
    h.respond.store(false, Ordering::SeqCst);
    h.wait_for("release while the socket still exists", |s| {
        s["hold"] == false && s["reason"] == "T3 server not running"
    });
    h.wait_for_keeper_exit(keeper_pid(&first));
    h.respond.store(true, Ordering::SeqCst);
    h.wait_for("the recovered server", holding);
    h.set_server_running(false);
    h.wait_for("the stopped server", |s| s["hold"] == false && s["reason"] == "T3 server not running");
}

#[test]
fn a_runtime_pid_that_does_not_own_the_listener_is_ignored() {
    let h = Harness::new("pid");
    h.set_threads(&[("thread", "working")]);
    let mut unrelated = Command::new("sleep").arg("20").spawn().unwrap();
    let runtime = serde_json::json!({"version": 1, "pid": unrelated.id(), "port": h.port, "startedAt": STARTED_AT});
    fs::write(h.dir.join("userdata/server-runtime.json"), runtime.to_string()).unwrap();
    h.run(&["start"]);
    h.wait_for("the rejected runtime", |s| s["hold"] == false && s["reason"] == "T3 server not running");
    assert!(h.keeper_log().is_empty());
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
}
