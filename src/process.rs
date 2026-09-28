use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::util::fmt_duration;

const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;

pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Collects a command's output within one deadline, including the time spent
/// waiting for pipe EOF. A descendant may inherit stdout after the command
/// itself exits, so waiting for reader threads would not be bounded.
pub fn output_with_timeout(mut command: Command, timeout: Duration) -> Result<Output> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().with_context(|| format!("spawning {}", command.get_program().to_string_lossy()))?;
    let result = (|| {
        let mut stdout = child.stdout.take().context("child has no stdout")?;
        let mut stderr = child.stderr.take().context("child has no stderr")?;
        set_nonblocking(stdout.as_raw_fd())?;
        set_nonblocking(stderr.as_raw_fd())?;

        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut out_eof = false;
        let mut err_eof = false;
        let mut status = None;
        loop {
            if status.is_none() {
                status = child.try_wait()?;
            }
            if !out_eof {
                out_eof = read_available(&mut stdout, &mut out)?;
            }
            if !err_eof {
                err_eof = read_available(&mut stderr, &mut err)?;
            }
            if let Some(status) = status.filter(|_| out_eof && err_eof) {
                return Ok(Output { status, stdout: out, stderr: err });
            }
            let now = Instant::now();
            if now >= deadline {
                bail!("timed out after {}", fmt_duration(timeout));
            }
            thread::sleep(Duration::from_millis(20).min(deadline - now));
        }
    })();
    if result.is_err() {
        // Do not block here: the caller must keep reconciling the Windows hold
        // even if killing or reaping this child fails.
        let _ = child.kill();
        if !matches!(child.try_wait(), Ok(Some(_))) {
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
    result
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: fd is an open pipe owned by the caller.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd remains open for the duration of this call.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn read_available(reader: &mut impl Read, output: &mut Vec<u8>) -> Result<bool> {
    let mut chunk = [0; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                if n > MAX_CAPTURE_BYTES - output.len() {
                    bail!("command output exceeded {MAX_CAPTURE_BYTES} bytes");
                }
                output.extend_from_slice(&chunk[..n]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error).context("reading command output"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo out; echo err >&2"]);
        let output = output_with_timeout(command, Duration::from_secs(5)).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"out\n");
        assert_eq!(output.stderr, b"err\n");
    }

    #[test]
    fn kills_a_child_that_overruns() {
        let mut command = Command::new("sleep");
        command.arg("30");
        let started = Instant::now();
        let error = output_with_timeout(command, Duration::from_millis(200)).err().unwrap();
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn an_inherited_pipe_cannot_extend_the_deadline() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 2 & echo done"]);
        let started = Instant::now();
        let error = output_with_timeout(command, Duration::from_millis(200)).err().unwrap();
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
