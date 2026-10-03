use std::env;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::daemon::try_lock;
use crate::log;
use crate::paths::{Paths, service_file};
use crate::process::output_with_timeout;
use crate::status::Status;

const START_WAIT: Duration = Duration::from_secs(5);
const STOP_WAIT: Duration = Duration::from_secs(10);

fn request(paths: &Paths, command: &str) -> Result<String> {
    let stream = UnixStream::connect(paths.socket()).context("daemon is not running")?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    (&stream).write_all(format!("{command}\n").as_bytes())?;
    let mut reply = String::new();
    if BufReader::new(&stream).read_line(&mut reply)? == 0 {
        bail!("daemon closed the connection without a reply");
    }
    let reply = reply.trim();
    if let Some(error) = reply.strip_prefix("error ") {
        bail!("daemon: {error}");
    }
    match command {
        "nudge" | "stop" if reply != "ok" => bail!("unexpected daemon reply: {reply:?}"),
        "status" => {
            let _: Status = serde_json::from_str(reply).context("invalid daemon status reply")?;
        }
        _ => {}
    }
    Ok(reply.to_string())
}

/// Refresh the daemon, starting it if it is not running.
pub fn nudge() -> Result<()> {
    let paths = Paths::resolve()?;
    if service_action("start")? {
        return wait_for_daemon(&paths);
    }
    if request(&paths, "nudge").is_ok() {
        return Ok(());
    }
    start(&paths)
}

pub fn status(json: bool) -> Result<()> {
    let paths = Paths::resolve()?;
    let reply =
        request(&paths, "status").with_context(|| format!("daemon is not running; see {}", paths.log().display()))?;
    if json {
        println!("{reply}");
    } else {
        let status: Status = serde_json::from_str(&reply).context("parsing status")?;
        print!("{}", status.render());
    }
    Ok(())
}

pub fn stop() -> Result<()> {
    let paths = Paths::resolve()?;
    if service_action("stop")? {
        println!("stopped");
        return Ok(());
    }
    if stop_daemon(&paths)? {
        println!("stopped");
    } else {
        println!("not running");
    }
    Ok(())
}

pub fn restart() -> Result<()> {
    let paths = Paths::resolve()?;
    if service_action("restart")? {
        wait_for_daemon(&paths)?;
        println!("started");
        return Ok(());
    }
    stop_daemon(&paths)?;
    start(&paths)?;
    println!("started");
    Ok(())
}

pub fn print_log(lines: usize) -> Result<()> {
    let paths = Paths::resolve()?;
    for line in log::tail(&paths.log(), lines)? {
        println!("{line}");
    }
    Ok(())
}

/// Returns whether a daemon was running.
fn stop_daemon(paths: &Paths) -> Result<bool> {
    if request(paths, "stop").is_err() {
        return Ok(false);
    }
    // The lock outlives the socket, so wait on the lock: starting a successor
    // before it is free would lose the race and leave nothing running.
    if !wait_until(STOP_WAIT, || try_lock(&paths.lock()).is_ok_and(|lock| lock.is_some())) {
        bail!("daemon did not stop within {}s", STOP_WAIT.as_secs());
    }
    Ok(true)
}

fn start(paths: &Paths) -> Result<()> {
    spawn_daemon(paths)?;
    wait_for_daemon(paths)
}

fn wait_for_daemon(paths: &Paths) -> Result<()> {
    if !wait_until(START_WAIT, || request(paths, "nudge").is_ok()) {
        let tail = log::tail(&paths.log(), 5).unwrap_or_default().join("\n");
        bail!("daemon did not come up; last log lines:\n{tail}");
    }
    Ok(())
}

/// Installed lifecycle commands go through systemd so a restart cannot leave
/// the replacement daemon outside its supervisor. Isolated/manual state dirs
/// use the socket controller instead and never affect the installed service.
fn service_action(action: &str) -> Result<bool> {
    if env::var_os("T3_KEEP_AWAKE_STATE_DIR").is_some() || !service_file()?.exists() {
        return Ok(false);
    }
    let mut command = Command::new("systemctl");
    command.args(["--user", action, "t3-keep-awake.service"]);
    let output = output_with_timeout(command, Duration::from_secs(20))?;
    if !output.status.success() {
        bail!("systemctl {action}: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(true)
}

fn spawn_daemon(paths: &Paths) -> Result<()> {
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log())
        .with_context(|| format!("opening {}", paths.log().display()))?;
    let mut command = Command::new(env::current_exe().context("locating own binary")?);
    command.arg("daemon").stdin(Stdio::null()).stdout(Stdio::null()).stderr(log);
    // SAFETY: setsid is async-signal-safe. A new session detaches the daemon
    // from the CLI, so terminal teardown does not take it along.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().context("starting daemon")?;
    Ok(())
}

fn wait_until(timeout: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if ready() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
}
