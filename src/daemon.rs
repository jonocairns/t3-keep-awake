use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::decide::{Decision, Tracker};
use crate::herdr::Herdr;
use crate::keeper::{self, Keeper, Line, Phase};
use crate::log::Log;
use crate::paths::{Paths, config_file};
use crate::status::{KeeperStatus, Status};
use crate::util::fmt_duration;

const TICK: Duration = Duration::from_millis(500);
/// A burst of hook nudges costs at most one herdr read per second.
const NUDGE_DEBOUNCE: Duration = Duration::from_secs(1);
/// powershell.exe cold starts take a few seconds; far longer means it is wedged.
const START_TIMEOUT: Duration = Duration::from_secs(30);
const RELEASE_TIMEOUT: Duration = Duration::from_secs(10);
/// A keeper that held this long before failing resets the backoff.
const STABLE_AFTER: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const SOCKET_IO_TIMEOUT: Duration = Duration::from_secs(2);

static STOP_SIGNALLED: AtomicBool = AtomicBool::new(false);

enum Msg {
    Nudge,
    Stop,
    KeeperLine(u64, String),
}

pub fn run() -> Result<()> {
    let paths = Paths::resolve()?;
    let log = Log::open(&paths.log())?;
    // Hooks race to start the daemon; the lock picks one winner and the rest leave quietly.
    let Some(_lock) = try_lock(&paths.lock())? else {
        return Ok(());
    };
    let config = match config_file().and_then(|path| Config::load(&path)) {
        Ok(config) => config,
        Err(error) => {
            log.line(format!("not starting: {error:#}"));
            return Err(error);
        }
    };
    if !keeper::supported() {
        log.line("not starting: powershell.exe not found; herdr-keep-awake needs WSL interop");
        bail!("powershell.exe not found; herdr-keep-awake needs WSL interop");
    }

    let socket = paths.socket();
    // Holding the lock means any existing socket file is stale.
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).with_context(|| format!("binding {}", socket.display()))?;
    install_signal_handlers();

    let (tx, rx) = mpsc::channel();
    let status = Arc::new(Mutex::new(Status::default()));
    log.line(format!("started: pid {}, {config:?}", process::id()));

    let mut daemon = Daemon::new(config, log, tx.clone(), Arc::clone(&status));
    // Settle once before serving: clients queue in the socket backlog
    // meanwhile, so the first `status` is never a blank default.
    daemon.tick(Instant::now());
    spawn_listener(listener, tx, status);
    let result = daemon.run(&rx);
    daemon.shutdown();
    // Remove the socket before the lock drops, so a successor never sees ours.
    let _ = fs::remove_file(&socket);
    result
}

pub fn try_lock(path: &Path) -> Result<Option<File>> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error).with_context(|| format!("locking {}", path.display())),
    }
}

extern "C" fn on_signal(_: libc::c_int) {
    STOP_SIGNALLED.store(true, Ordering::SeqCst);
}

fn install_signal_handlers() {
    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
        unsafe { libc::signal(signal, handler) };
    }
}

fn spawn_listener(listener: UnixListener, tx: Sender<Msg>, status: Arc<Mutex<Status>>) {
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = serve(&stream, &tx, &status);
        }
    });
}

fn serve(stream: &UnixStream, tx: &Sender<Msg>, status: &Mutex<Status>) -> io::Result<()> {
    stream.set_read_timeout(Some(SOCKET_IO_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_IO_TIMEOUT))?;
    let mut request = String::new();
    BufReader::new(stream).read_line(&mut request)?;
    let reply = match request.trim() {
        "nudge" => {
            let _ = tx.send(Msg::Nudge);
            "ok".to_string()
        }
        "stop" => {
            let _ = tx.send(Msg::Stop);
            "ok".to_string()
        }
        "status" => {
            let status = status.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            serde_json::to_string(&*status).unwrap_or_else(|error| format!("error {error}"))
        }
        other => format!("error unknown request {other:?}"),
    };
    let mut stream = stream;
    stream.write_all(format!("{reply}\n").as_bytes())
}

enum Step {
    Nothing,
    Spawn,
    Ping,
    Kill(&'static str),
}

struct Daemon {
    config: Config,
    herdr: Herdr,
    log: Log,
    tx: Sender<Msg>,
    status: Arc<Mutex<Status>>,
    started: Instant,
    tracker: Tracker,
    decision: Decision,
    last_poll: Option<Instant>,
    poll_requested: bool,
    running_sessions: usize,
    herdr_errors: Vec<String>,
    last_server_seen: Instant,
    keeper: Option<Keeper>,
    generation: u64,
    failures: u32,
    retry_at: Option<Instant>,
}

impl Daemon {
    fn new(config: Config, log: Log, tx: Sender<Msg>, status: Arc<Mutex<Status>>) -> Self {
        let now = Instant::now();
        Self {
            config,
            herdr: Herdr::from_env(),
            log,
            tx,
            status,
            started: now,
            tracker: Tracker::default(),
            decision: Decision::default(),
            last_poll: None,
            poll_requested: true,
            running_sessions: 0,
            herdr_errors: Vec::new(),
            last_server_seen: now,
            keeper: None,
            generation: 0,
            failures: 0,
            retry_at: None,
        }
    }

    fn run(&mut self, rx: &Receiver<Msg>) -> Result<()> {
        loop {
            if STOP_SIGNALLED.load(Ordering::SeqCst) {
                self.log.line("signal received; stopping");
                return Ok(());
            }
            match rx.recv_timeout(TICK) {
                Ok(Msg::Nudge) => self.poll_requested = true,
                Ok(Msg::Stop) => {
                    self.log.line("stop requested");
                    return Ok(());
                }
                Ok(Msg::KeeperLine(generation, line)) => self.on_keeper_line(generation, &line),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => bail!("message channel closed"),
            }
            let now = Instant::now();
            self.tick(now);
            if self.keeper.is_none() && now.duration_since(self.last_server_seen) >= self.config.idle_exit() {
                self.log.line(format!(
                    "no running herdr server for {}; exiting until the next herdr hook starts me",
                    fmt_duration(self.config.idle_exit())
                ));
                return Ok(());
            }
        }
    }

    fn tick(&mut self, now: Instant) {
        if self.poll_due(now) {
            self.poll(now);
        }
        self.reconcile(now);
        self.publish_status(now);
    }

    fn poll_due(&self, now: Instant) -> bool {
        let Some(last) = self.last_poll else { return true };
        let since = now.duration_since(last);
        (self.poll_requested && since >= NUDGE_DEBOUNCE) || since >= self.config.poll()
    }

    fn poll(&mut self, now: Instant) {
        let snapshot = self.herdr.snapshot();
        self.last_poll = Some(now);
        self.poll_requested = false;
        self.running_sessions = snapshot.running_sessions;
        if snapshot.running_sessions > 0 {
            self.last_server_seen = now;
        }
        if snapshot.errors != self.herdr_errors {
            for error in &snapshot.errors {
                self.log.line(format!("herdr: {error}"));
            }
            if snapshot.errors.is_empty() {
                self.log.line("herdr: readable again");
            }
            self.herdr_errors = snapshot.errors.clone();
        }

        let decision = self.tracker.observe(&snapshot.agents, snapshot.complete(), now, &self.config);
        if decision.hold != self.decision.hold {
            let verb = if decision.hold { "hold" } else { "release" };
            self.log.line(format!("{verb}: {}", decision.reason));
        }
        if decision.active != self.decision.active && !decision.active.is_empty() {
            self.log.line(format!("working: {}", decision.active.join(", ")));
        }
        if decision.stale != self.decision.stale && !decision.stale.is_empty() {
            self.log.line(format!("ignoring as stuck: {}", decision.stale.join(", ")));
        }
        self.decision = decision;
    }

    fn reconcile(&mut self, now: Instant) {
        self.reap(now);
        if self.decision.hold {
            self.ensure_holding(now);
        } else {
            self.ensure_released(now);
        }
    }

    fn reap(&mut self, now: Instant) {
        let Some(keeper) = self.keeper.as_mut() else { return };
        let status = match keeper.try_exit() {
            Ok(Some(status)) => status,
            Ok(None) => return,
            Err(error) => {
                self.log.line(format!("checking keeper: {error:#}"));
                return;
            }
        };
        let keeper = self.keeper.take().expect("checked above");
        if matches!(keeper.phase, Phase::Releasing { .. }) {
            self.log.line(format!("released: keeper exited ({status})"));
        } else {
            self.record_failure(now, &keeper.phase, &format!("keeper exited unexpectedly ({status})"));
        }
    }

    fn ensure_holding(&mut self, now: Instant) {
        let step = match &self.keeper {
            None if self.retry_at.is_some_and(|at| now < at) => Step::Nothing,
            None => Step::Spawn,
            // Wanted again mid-release: let it exit, and the next tick spawns a fresh one.
            Some(Keeper { phase: Phase::Releasing { .. }, .. }) => Step::Nothing,
            Some(keeper @ Keeper { phase: Phase::Starting, .. }) => {
                if now.duration_since(keeper.spawned) >= START_TIMEOUT {
                    Step::Kill("keeper did not confirm the hold in time")
                } else {
                    Step::Nothing
                }
            }
            Some(keeper @ Keeper { phase: Phase::Holding { .. }, .. }) => {
                if now.duration_since(keeper.last_pong) >= self.config.keeper_timeout() {
                    Step::Kill("keeper stopped answering heartbeats")
                } else if keeper.ping_due(now, self.config.heartbeat()) {
                    Step::Ping
                } else {
                    Step::Nothing
                }
            }
        };
        match step {
            Step::Nothing => {}
            Step::Spawn => self.spawn_keeper(now),
            Step::Ping => {
                let keeper = self.keeper.as_mut().expect("ping implies a keeper");
                if keeper.ping(now).is_err() {
                    // It most likely just exited; the next reap reports why.
                    self.log.line("heartbeat write failed");
                }
            }
            Step::Kill(why) => self.kill_keeper(now, why),
        }
    }

    fn ensure_released(&mut self, now: Instant) {
        let Some(keeper) = self.keeper.as_mut() else {
            self.failures = 0;
            self.retry_at = None;
            return;
        };
        match keeper.phase {
            Phase::Releasing { since } if now.duration_since(since) >= RELEASE_TIMEOUT => {
                keeper.kill();
                self.keeper = None;
                self.log.line("released: keeper ignored stdin closing, killed it");
            }
            Phase::Releasing { .. } => {}
            _ => {
                keeper.release(now);
                self.log.line("releasing");
            }
        }
    }

    fn spawn_keeper(&mut self, now: Instant) {
        self.generation += 1;
        let generation = self.generation;
        let tx = self.tx.clone();
        let spawned = keeper::command(&self.config).and_then(|command| {
            let stderr = self.log.handle().context("opening log for keeper stderr")?;
            Keeper::spawn(command, generation, stderr, move |line| {
                let _ = tx.send(Msg::KeeperLine(generation, line));
            })
        });
        match spawned {
            Ok(keeper) => {
                self.log.line(format!("starting keeper (WSL pid {})", keeper.wsl_pid()));
                self.keeper = Some(keeper);
            }
            Err(error) => self.record_failure(now, &Phase::Starting, &format!("{error:#}")),
        }
    }

    fn kill_keeper(&mut self, now: Instant, why: &str) {
        if let Some(mut keeper) = self.keeper.take() {
            keeper.kill();
            self.record_failure(now, &keeper.phase, why);
        }
    }

    fn record_failure(&mut self, now: Instant, phase: &Phase, why: &str) {
        let held_long = matches!(phase, Phase::Holding { since, .. } if now.duration_since(*since) >= STABLE_AFTER);
        self.failures = if held_long { 1 } else { self.failures + 1 };
        let delay = MAX_BACKOFF.min(Duration::from_secs(1 << (self.failures - 1).min(6)));
        self.retry_at = Some(now + delay);
        self.log.line(format!("{why}; retrying in {}", fmt_duration(delay)));
    }

    fn on_keeper_line(&mut self, generation: u64, line: &str) {
        let now = Instant::now();
        let Some(keeper) = self.keeper.as_mut().filter(|keeper| keeper.generation == generation) else {
            return;
        };
        match keeper::parse_line(line) {
            Line::Holding(windows_pid) => {
                if keeper.phase == Phase::Starting {
                    keeper.phase = Phase::Holding { since: now, windows_pid };
                    keeper.last_pong = now;
                    let pid = windows_pid.map_or(String::new(), |pid| format!(" (Windows pid {pid})"));
                    self.log.line(format!("holding: Windows will not sleep{pid}"));
                }
            }
            Line::Pong => keeper.last_pong = now,
            Line::Released(why) => self.log.line(format!("keeper let go: {why}")),
            Line::Error(message) => self.log.line(format!("keeper error: {message}")),
            // powershell.exe wraps progress noise in CLIXML; anything else is worth seeing.
            Line::Other(text) if text.is_empty() || text.starts_with("#<") || text.starts_with("<Objs") => {}
            Line::Other(text) => self.log.line(format!("keeper: {text}")),
        }
    }

    fn publish_status(&self, now: Instant) {
        let keeper = self.keeper.as_ref().map(|keeper| {
            let (phase, since, windows_pid) = match keeper.phase {
                Phase::Starting => ("starting", keeper.spawned, None),
                Phase::Holding { since, windows_pid } => ("holding", since, windows_pid),
                Phase::Releasing { since } => ("releasing", since, None),
            };
            KeeperStatus {
                phase: phase.to_string(),
                for_secs: now.duration_since(since).as_secs(),
                wsl_pid: keeper.wsl_pid(),
                windows_pid,
            }
        });
        let status = Status {
            daemon_pid: process::id(),
            uptime_secs: now.duration_since(self.started).as_secs(),
            hold: self.decision.hold,
            reason: self.decision.reason.clone(),
            active: self.decision.active.clone(),
            stale: self.decision.stale.clone(),
            keeper,
            running_sessions: self.running_sessions,
            last_poll_secs_ago: self.last_poll.map_or(0, |at| now.duration_since(at).as_secs()),
            herdr_errors: self.herdr_errors.clone(),
            keeper_failures: self.failures,
            retry_in_secs: self.retry_at.filter(|at| *at > now).map(|at| at.duration_since(now).as_secs()),
        };
        *self.status.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = status;
    }

    fn shutdown(&mut self) {
        if let Some(mut keeper) = self.keeper.take() {
            keeper.release(Instant::now());
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match keeper.try_exit() {
                    Ok(Some(_)) => {
                        self.log.line("released: keeper exited");
                        break;
                    }
                    Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
                    _ => {
                        keeper.kill();
                        self.log.line("released: killed keeper");
                        break;
                    }
                }
            }
        }
        self.log.line("stopped");
    }
}
