use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::iter;
use std::os::unix::fs::FileTypeExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::process::output_with_timeout;

const KEEPER_SCRIPT: &str = include_str!("keeper.ps1");
const PROBE_SCRIPT: &str = include_str!("probe.ps1");
/// Relative to a Windows drive root.
const POWERSHELL: &str = "Windows/System32/WindowsPowerShell/v1.0/powershell.exe";

// SetThreadExecutionState flags.
const ES_CONTINUOUS: u32 = 0x8000_0000;
const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
const ES_DISPLAY_REQUIRED: u32 = 0x0000_0002;
const ES_AWAYMODE_REQUIRED: u32 = 0x0000_0040;

/// Test seam: a program speaking the keeper protocol instead of powershell.exe.
const FAKE_KEEPER_ENV: &str = "T3_KEEP_AWAKE_KEEPER";

pub fn supported() -> bool {
    env::var_os(FAKE_KEEPER_ENV).is_some() || find_powershell().is_some()
}

pub fn command(config: &Config) -> Result<Command> {
    if let Some(fake) = env::var_os(FAKE_KEEPER_ENV) {
        return Ok(Command::new(fake));
    }
    powershell(&keeper_script(config))
}

fn keeper_script(config: &Config) -> String {
    let display = if config.keep_display_on { ES_DISPLAY_REQUIRED } else { 0 };
    KEEPER_SCRIPT
        .replace("__HOLD_FLAGS__", &(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | display).to_string())
        .replace("__RELEASE_FLAGS__", &ES_CONTINUOUS.to_string())
        .replace("__TIMEOUT_MS__", &config.keeper_timeout().as_millis().to_string())
}

fn powershell(script: &str) -> Result<Command> {
    let exe = find_powershell().context("powershell.exe not found; t3-keep-awake needs WSL interop")?;
    let mut command = Command::new(exe);
    // User services do not inherit a terminal's WSL_INTEROP. Prefer WSL's
    // stable init socket, resolving it again for every Windows invocation.
    let init_socket = PathBuf::from("/run/WSL/1_interop");
    if fs::metadata(&init_socket).is_ok_and(|metadata| metadata.file_type().is_socket()) {
        command.env("WSL_INTEROP", init_socket);
    }
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", &encode_command(script)]);
    Ok(command)
}

fn find_powershell() -> Option<PathBuf> {
    let on_path = env::var_os("PATH")
        .and_then(|path| env::split_paths(&path).map(|dir| dir.join("powershell.exe")).find(|exe| exe.is_file()));
    // User services do not inherit WSL's Windows PATH. Windows is usually C:
    // under /mnt, but the drive letter and automount root can differ.
    on_path.or_else(|| {
        let mounts = fs::read_to_string("/proc/self/mounts").unwrap_or_default();
        iter::once(PathBuf::from("/mnt/c"))
            .chain(windows_drives(&mounts))
            .map(|drive| drive.join(POWERSHELL))
            .find(|exe| exe.is_file())
    })
}

/// Mount points of whole Windows drives, whose source WSL lists as `C:\`.
fn windows_drives(mounts: &str) -> Vec<PathBuf> {
    mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace().map(unescape);
            let (source, target) = (fields.next()?, fields.next()?);
            let drive = source.strip_suffix('\\').unwrap_or(&source).as_bytes();
            matches!(drive, [letter, b':'] if letter.is_ascii_alphabetic()).then(|| PathBuf::from(target))
        })
        .collect()
}

/// Undoes the octal escapes /proc/mounts uses for whitespace and backslashes.
fn unescape(field: &str) -> String {
    field.replace("\\040", " ").replace("\\011", "\t").replace("\\012", "\n").replace("\\134", "\\")
}

/// `-EncodedCommand` takes base64 of UTF-16LE. Passing the script inline
/// sidesteps execution policy and `\\wsl.localhost` script paths.
fn encode_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&bytes)
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = u32::from(chunk[0]) << 16
            | u32::from(*chunk.get(1).unwrap_or(&0)) << 8
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[derive(Debug, PartialEq)]
pub enum Line {
    Holding(Option<u32>),
    Pong,
    Released(String),
    Error(String),
    Other(String),
}

pub fn parse_line(line: &str) -> Line {
    let line = line.trim();
    if line == "pong" {
        Line::Pong
    } else if let Some(pid) = line.strip_prefix("holding") {
        Line::Holding(pid.trim().parse().ok())
    } else if let Some(why) = line.strip_prefix("released ") {
        Line::Released(why.to_string())
    } else if let Some(message) = line.strip_prefix("error ") {
        Line::Error(message.to_string())
    } else {
        Line::Other(line.to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    /// Spawned; waiting for `holding`.
    Starting,
    Holding {
        since: Instant,
        windows_pid: Option<u32>,
    },
    /// stdin closed; waiting for it to exit.
    Releasing {
        since: Instant,
    },
}

/// One `powershell.exe` holding Windows awake. It lives only while a hold is
/// wanted: releasing means closing its stdin, and killing the WSL-side
/// interop process also ends the Windows process.
pub struct Keeper {
    pub generation: u64,
    child: Child,
    stdin: Option<ChildStdin>,
    pub spawned: Instant,
    pub phase: Phase,
    last_ping: Instant,
    pub last_pong: Instant,
}

impl Keeper {
    pub fn spawn(
        mut command: Command,
        generation: u64,
        stderr: File,
        on_line: impl Fn(String) + Send + 'static,
    ) -> Result<Self> {
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(stderr);
        let mut child = command.spawn().context("spawning keeper")?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().context("keeper has no stdout")?;
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut buf = Vec::new();
            while reader.read_until(b'\n', &mut buf).is_ok_and(|n| n > 0) {
                on_line(String::from_utf8_lossy(&buf).into_owned());
                buf.clear();
            }
        });
        let now = Instant::now();
        Ok(Self { generation, child, stdin, spawned: now, phase: Phase::Starting, last_ping: now, last_pong: now })
    }

    pub fn wsl_pid(&self) -> u32 {
        self.child.id()
    }

    pub fn ping_due(&self, now: Instant, every: Duration) -> bool {
        now.duration_since(self.last_ping) >= every
    }

    pub fn ping(&mut self, now: Instant) -> Result<()> {
        let stdin = self.stdin.as_mut().context("stdin already closed")?;
        stdin.write_all(b"ping\n")?;
        stdin.flush()?;
        self.last_ping = now;
        Ok(())
    }

    /// Closing stdin is the release signal.
    pub fn release(&mut self, now: Instant) {
        self.stdin = None;
        self.phase = Phase::Releasing { since: now };
    }

    pub fn try_exit(&mut self) -> Result<Option<ExitStatus>> {
        Ok(self.child.try_wait()?)
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Reads Windows' system-wide execution state (every process, not only ours).
pub fn probe() -> Result<()> {
    let output = output_with_timeout(powershell(PROBE_SCRIPT)?, Duration::from_secs(30))?;
    if !output.status.success() {
        bail!("Windows probe failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(fields) = stdout.lines().find_map(|line| line.trim().strip_prefix("state ")) else {
        bail!("unexpected probe output: {}{}", stdout.trim(), String::from_utf8_lossy(&output.stderr).trim());
    };
    let mut fields = fields.split_whitespace().map(str::parse::<u32>);
    let (Some(Ok(rc)), Some(Ok(state))) = (fields.next(), fields.next()) else {
        bail!("unexpected probe output: {}", stdout.trim());
    };
    if rc != 0 {
        bail!("CallNtPowerInformation failed with NTSTATUS 0x{rc:08X}");
    }
    println!("Windows execution state: 0x{state:08X} ({})", describe(state));
    println!("This is the union of every Windows process's request, not only t3-keep-awake's.");
    Ok(())
}

fn describe(state: u32) -> String {
    let names: Vec<&str> = [
        (ES_SYSTEM_REQUIRED, "system required: sleep blocked"),
        (ES_DISPLAY_REQUIRED, "display required"),
        (ES_AWAYMODE_REQUIRED, "away mode"),
    ]
    .into_iter()
    .filter(|(flag, _)| state & flag != 0)
    .map(|(_, name)| name)
    .collect();
    if names.is_empty() { "nothing is blocking sleep".to_string() } else { names.join(", ") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_placeholder_is_filled_with_decimal_flags() {
        let script = keeper_script(&Config::default());
        assert!(!script.contains("__"), "unfilled placeholder in:\n{script}");
        assert!(script.contains("$hold = [uint32]2147483649"));
        assert!(script.contains("$release = [uint32]2147483648"));
        assert!(script.contains("$timeoutMs = 60000"));

        let display = keeper_script(&Config { keep_display_on: true, ..Config::default() });
        assert!(display.contains("$hold = [uint32]2147483651"));
    }

    #[test]
    fn encoded_commands_are_base64_utf16le() {
        assert_eq!(encode_command("ab"), "YQBiAA==");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn windows_drives_are_found_wherever_wsl_mounts_them() {
        let mounts = r"
            drivers /usr/lib/wsl/drivers 9p ro,nosuid,nodev,noatime,aname=drivers 0 0
            C:\134 /mnt/c 9p rw,noatime,aname=drvfs;path=C:\;uid=1000 0 0
            D:\134 /win\040drives/d 9p rw,noatime,aname=drvfs;path=D:\;uid=1000 0 0
            C:\134Program\040Files\134Docker /Docker/host 9p rw,noatime,aname=drvfs 0 0
            E: /e drvfs rw,noatime 0 0
        ";
        let drives = windows_drives(mounts);
        assert_eq!(drives, [PathBuf::from("/mnt/c"), PathBuf::from("/win drives/d"), PathBuf::from("/e")]);
    }

    #[test]
    fn keeper_lines_are_parsed_tolerating_crlf() {
        assert_eq!(parse_line("holding 17528\r\n"), Line::Holding(Some(17528)));
        assert_eq!(parse_line("pong\r\n"), Line::Pong);
        assert_eq!(parse_line("released stdin-closed\r\n"), Line::Released("stdin-closed".into()));
        assert_eq!(
            parse_line("error SetThreadExecutionState failed"),
            Line::Error("SetThreadExecutionState failed".into())
        );
        assert_eq!(parse_line("#< CLIXML"), Line::Other("#< CLIXML".into()));
    }

    #[test]
    fn execution_state_is_described() {
        assert_eq!(describe(0), "nothing is blocking sleep");
        assert_eq!(describe(3), "system required: sleep blocked, display required");
    }
}
