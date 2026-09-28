use std::env;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::decide::{Agent, AgentStatus};
use crate::process::output_with_timeout;
use crate::util::truncate;

const CALL_TIMEOUT: Duration = Duration::from_secs(5);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Snapshot {
    pub running_sessions: usize,
    pub agents: Vec<Agent>,
    pub errors: Vec<String>,
}

impl Snapshot {
    pub fn complete(&self) -> bool {
        self.errors.is_empty()
    }
}

pub struct Herdr {
    bin: Option<OsString>,
}

impl Herdr {
    pub fn from_env() -> Self {
        Self { bin: env::var_os("HERDR_BIN_PATH").filter(|bin| !bin.is_empty()) }
    }

    fn program(&self) -> &OsStr {
        // Long-lived agent sessions may retain a path to a Herdr executable
        // that was replaced during an update. Check it on every call.
        self.bin.as_deref().filter(|bin| Path::new(bin).is_file()).unwrap_or(OsStr::new("herdr"))
    }

    /// Every agent in every running local session. A session that cannot be
    /// read is reported in `errors` rather than silently treated as idle.
    pub fn snapshot(&self) -> Snapshot {
        let deadline = Instant::now() + SNAPSHOT_TIMEOUT;
        let sessions =
            match self.call(&["session", "list", "--json"], CALL_TIMEOUT).and_then(|out| parse_sessions(&out)) {
                Ok(sessions) => sessions,
                Err(error) => {
                    return Snapshot {
                        running_sessions: 0,
                        agents: Vec::new(),
                        errors: vec![format!("session list: {error:#}")],
                    };
                }
            };
        let mut snapshot = Snapshot { running_sessions: sessions.len(), agents: Vec::new(), errors: Vec::new() };
        for session in sessions {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                snapshot.errors.push("snapshot deadline reached before every session could be read".into());
                break;
            }
            match self
                .call(&["--session", &session, "agent", "list"], CALL_TIMEOUT.min(remaining))
                .and_then(|out| parse_agents(&session, &out))
            {
                Ok(agents) => snapshot.agents.extend(agents),
                Err(error) => snapshot.errors.push(format!("session {session}: {error:#}")),
            }
        }
        snapshot
    }

    fn call(&self, args: &[&str], timeout: Duration) -> Result<String> {
        let mut command = Command::new(self.program());
        command.args(args);
        // The daemon inherits the HERDR_* context of whichever hook started
        // it. Drop it so `--session` alone decides which server answers.
        for (key, _) in env::vars_os() {
            if key.to_string_lossy().starts_with("HERDR_") {
                command.env_remove(key);
            }
        }
        let output = output_with_timeout(command, timeout)?;
        if !output.status.success() {
            bail!("{}: {}", output.status, String::from_utf8_lossy(&output.stderr).trim());
        }
        String::from_utf8(output.stdout).context("output is not UTF-8")
    }
}

#[derive(Deserialize)]
struct SessionList {
    sessions: Vec<Session>,
}

#[derive(Deserialize)]
struct Session {
    name: String,
    running: bool,
}

fn parse_sessions(json: &str) -> Result<Vec<String>> {
    let list: SessionList = serde_json::from_str(json).context("parsing session list")?;
    Ok(list.sessions.into_iter().filter(|session| session.running).map(|session| session.name).collect())
}

#[derive(Deserialize)]
struct AgentList {
    result: AgentListResult,
}

#[derive(Deserialize)]
struct AgentListResult {
    agents: Vec<RawAgent>,
}

#[derive(Deserialize)]
struct RawAgent {
    pane_id: String,
    agent: Option<String>,
    agent_status: Option<AgentStatus>,
    state_change_seq: Option<u64>,
    terminal_title_stripped: Option<String>,
}

fn parse_agents(session: &str, json: &str) -> Result<Vec<Agent>> {
    let list: AgentList = serde_json::from_str(json).context("parsing agent list")?;
    Ok(list
        .result
        .agents
        .into_iter()
        .map(|raw| {
            let mut label = format!("{session}/{} {}", raw.pane_id, raw.agent.as_deref().unwrap_or("agent"));
            if let Some(title) = raw.terminal_title_stripped.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                label.push_str(&format!(" \"{}\"", truncate(title, 48)));
            }
            Agent {
                id: format!("{session}/{}", raw.pane_id),
                label,
                status: raw.agent_status.unwrap_or(AgentStatus::Unknown),
                seq: raw.state_change_seq.unwrap_or(0),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_removed_herdr_binary_falls_back_to_path() {
        let herdr = Herdr { bin: Some("/not/a/real/herdr (deleted)".into()) };
        assert_eq!(herdr.program(), OsStr::new("herdr"));
    }

    #[test]
    fn only_running_sessions_are_read() {
        let json = r#"{"sessions":[
            {"default":true,"name":"default","running":true,"session_dir":"/x","socket_path":"/x/herdr.sock"},
            {"default":false,"name":"old","running":false,"session_dir":"/y","socket_path":"/y/herdr.sock"}]}"#;
        assert_eq!(parse_sessions(json).unwrap(), ["default"]);
    }

    #[test]
    fn agents_are_keyed_by_session_and_pane() {
        // Trimmed from real `herdr agent list` output.
        let json = r#"{"id":"cli:agent:list","result":{"agents":[
            {"agent":"claude","agent_status":"working","cwd":"/home/u/tskr","focused":false,"pane_id":"wE:p1",
             "revision":581,"state_change_seq":352,"terminal_title_stripped":"tskr package updates","tokens":{}},
            {"agent":"codex","agent_status":"blocked","pane_id":"wE:p5","state_change_seq":354},
            {"agent_status":"something-new","pane_id":"wE:p6"}],"type":"agent_list"}}"#;
        let agents = parse_agents("default", json).unwrap();
        assert_eq!(agents.len(), 3);
        assert_eq!(agents[0].id, "default/wE:p1");
        assert_eq!(agents[0].label, r#"default/wE:p1 claude "tskr package updates""#);
        assert_eq!(agents[0].status, AgentStatus::Working);
        assert_eq!(agents[0].seq, 352);
        assert_eq!(agents[1].status, AgentStatus::Blocked);
        assert_eq!(agents[2].status, AgentStatus::Unknown);
        assert_eq!(agents[2].label, "default/wE:p6 agent");
    }

    #[test]
    fn an_error_response_is_an_error_not_an_empty_list() {
        assert!(parse_agents("default", r#"{"id":"x","error":{"message":"nope"}}"#).is_err());
    }
}
