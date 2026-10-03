mod client;
mod config;
mod daemon;
mod decide;
mod keeper;
mod log;
mod paths;
mod process;
mod status;
mod t3;
mod util;

use std::process::ExitCode;

use anyhow::{Result, anyhow};

const USAGE: &str = "\
t3-keep-awake: keep Windows awake while T3 Code threads are working (WSL)

Usage: t3-keep-awake <command>

Commands:
  status [--json]  Show whether Windows is being held awake, and why
  log [LINES]      Print the last LINES of the daemon log (default 20)
  probe            Read Windows' system-wide execution state
  restart          Restart the daemon (applies config and binary changes)
  stop             Stop the daemon and release any hold
  start            Start the daemon if needed and refresh thread state
  daemon           Run the daemon in the foreground

Config: ~/.config/t3-keep-awake/config.toml
State:  ~/.local/state/t3-keep-awake/
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("t3-keep-awake: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<()> {
    let flag = |name: &str| args.iter().skip(1).any(|arg| arg == name);
    match args.first().map(String::as_str) {
        Some("start") => client::nudge(),
        Some("daemon") => daemon::run(),
        Some("status") => client::status(flag("--json")),
        Some("log") => {
            let lines = args.get(1).map(|n| n.parse()).transpose()?.unwrap_or(20);
            client::print_log(lines)
        }
        Some("probe") => keeper::probe(),
        Some("restart") => client::restart(),
        Some("stop") => client::stop(),
        Some("help" | "-h" | "--help") | None => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(anyhow!("unknown command `{other}`\n\n{USAGE}")),
    }
}
