use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::util::fmt_duration;

pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Like `Command::output`, but a hung child is killed instead of hanging the
/// daemon's loop with it.
pub fn output_with_timeout(mut command: Command, timeout: Duration) -> Result<Output> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().with_context(|| format!("spawning {}", command.get_program().to_string_lossy()))?;
    // Drain both pipes while waiting so a chatty child cannot block on a full pipe.
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out after {}", fmt_duration(timeout));
        }
        thread::sleep(Duration::from_millis(20));
    };
    Ok(Output { status, stdout: stdout.join().unwrap_or_default(), stderr: stderr.join().unwrap_or_default() })
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    })
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
}
