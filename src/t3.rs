use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;

use crate::config::Config;
use crate::decide::{Thread, ThreadStatus};
use crate::util::truncate;

const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const DATABASE_TIMEOUT: Duration = Duration::from_millis(500);

pub struct Snapshot {
    pub running_servers: usize,
    pub threads: Vec<Thread>,
    pub errors: Vec<String>,
}

impl Snapshot {
    pub fn complete(&self) -> bool {
        self.errors.is_empty()
    }

    fn offline() -> Self {
        Self { running_servers: 0, threads: Vec::new(), errors: Vec::new() }
    }
}

pub struct T3 {
    data_dir: PathBuf,
}

#[derive(Deserialize)]
struct Runtime {
    version: u32,
    pid: u32,
    port: u16,
    #[serde(rename = "startedAt")]
    started_at: String,
}

impl T3 {
    pub fn from_config(config: &Config) -> Result<Self> {
        let data_dir = match env::var_os("T3_KEEP_AWAKE_DATA_DIR").filter(|value| !value.is_empty()) {
            Some(path) => PathBuf::from(path),
            None => match &config.t3_data_dir {
                Some(path) => path.clone(),
                None => PathBuf::from(env::var_os("HOME").context("HOME is not set")?).join(".t3/userdata"),
            },
        };
        Ok(Self { data_dir })
    }

    pub fn snapshot(&self) -> Snapshot {
        match self.read() {
            Ok(snapshot) => snapshot,
            Err(error) => Snapshot {
                running_servers: 0,
                threads: Vec::new(),
                errors: vec![format!("{}: {error:#}", self.data_dir.display())],
            },
        }
    }

    fn read(&self) -> Result<Snapshot> {
        let text = match fs::read(self.data_dir.join("server-runtime.json")) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Snapshot::offline()),
            Err(error) => return Err(error).context("reading server-runtime.json"),
        };
        let runtime: Runtime = serde_json::from_slice(&text).context("parsing server-runtime.json")?;
        if runtime.version != 1 || runtime.pid == 0 || runtime.port == 0 || runtime.started_at.is_empty() {
            bail!("unsupported or incomplete T3 server runtime metadata");
        }
        let Some(process_start) = listening_process(&runtime, Path::new("/proc"))? else {
            return Ok(Snapshot::offline());
        };
        // A listening socket also survives SIGSTOP. Require an HTTP response so
        // a hung server cannot keep renewing a persisted running turn.
        if !server_responds(runtime.port) {
            return Ok(Snapshot::offline());
        }

        let database = self.data_dir.join("state.sqlite");
        let connection =
            Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
                .with_context(|| format!("opening {} read-only", database.display()))?;
        connection.busy_timeout(DATABASE_TIMEOUT)?;
        let server_id = format!("{}:{}:{process_start}", self.data_dir.display(), runtime.started_at);
        let threads = read_threads(&connection, &server_id, &runtime.started_at)?;

        // The process may have exited while SQLite was being read.
        if listening_process(&runtime, Path::new("/proc"))? != Some(process_start) {
            return Ok(Snapshot::offline());
        }
        Ok(Snapshot { running_servers: 1, threads, errors: Vec::new() })
    }
}

fn read_threads(connection: &Connection, server_id: &str, started_at: &str) -> Result<Vec<Thread>> {
    // Only a current running turn confirmed by this live server counts. Old
    // turns, idle provider processes, deleted threads, and pre-restart runtime
    // rows do not. Async user questions can coexist with active work; only a
    // pending permission approval classifies a running turn as blocked.
    let mut query = connection
        .prepare(
            "SELECT t.thread_id, t.title, r.provider_name, v.row_id,
                    t.pending_approval_count > 0
             FROM projection_threads t
             JOIN projection_thread_sessions s USING (thread_id)
             JOIN provider_session_runtime r USING (thread_id)
             JOIN projection_turns v ON v.thread_id = t.thread_id AND v.turn_id = s.active_turn_id
             WHERE t.deleted_at IS NULL AND s.status = 'running'
               AND r.status = 'running' AND v.state = 'running'
               AND r.last_seen_at >= ?1
               AND json_extract(r.runtime_payload_json, '$.activeTurnId') = s.active_turn_id
             ORDER BY t.thread_id",
        )
        .context("reading T3 thread state (the database schema may have changed)")?;
    let threads = query.query_map([started_at], |row| {
        let id: String = row.get(0)?;
        let title: String = row.get(1)?;
        let provider: String = row.get(2)?;
        let blocked: bool = row.get(4)?;
        Ok(Thread {
            id: format!("{server_id}/{id}"),
            label: format!("{} {provider} \"{}\"", truncate(&id, 8), truncate(&title, 48)),
            status: if blocked { ThreadStatus::Blocked } else { ThreadStatus::Working },
            seq: u64::try_from(row.get::<_, i64>(3)?).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Integer, Box::new(error))
            })?,
        })
    })?;
    threads.collect::<rusqlite::Result<_>>().context("decoding T3 thread state")
}

/// Check that the runtime PID owns the advertised listening socket. A PID
/// existing alone is insufficient: stale runtime files and PID reuse must not
/// authorize a hold. Include Linux's process start ticks in turn identities.
fn listening_process(runtime: &Runtime, proc_dir: &Path) -> Result<Option<u64>> {
    let process_dir = proc_dir.join(runtime.pid.to_string());
    let stat = match fs::read_to_string(process_dir.join("stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("reading T3 process state"),
    };
    let (_, fields) = stat.rsplit_once(')').context("invalid T3 process stat")?;
    let fields: Vec<_> = fields.split_whitespace().collect();
    if matches!(fields.first(), Some(&"Z" | &"X")) {
        return Ok(None);
    }
    let start_ticks: u64 = fields.get(19).context("missing T3 process start time")?.parse()?;
    let descriptors = match fs::read_dir(process_dir.join("fd")) {
        Ok(descriptors) => descriptors,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("reading T3 process sockets"),
    };
    let sockets: HashSet<_> = descriptors
        .filter_map(|entry| fs::read_link(entry.ok()?.path()).ok())
        .filter_map(|path| path.to_str()?.strip_prefix("socket:[")?.strip_suffix(']').map(str::to_owned))
        .collect();
    for table in ["tcp", "tcp6"] {
        let text = match fs::read_to_string(process_dir.join("net").join(table)) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("reading T3 TCP listeners"),
        };
        for line in text.lines().skip(1) {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.get(3) != Some(&"0A") {
                continue;
            }
            let port = fields
                .get(1)
                .and_then(|address| address.rsplit_once(':'))
                .and_then(|(_, port)| u16::from_str_radix(port, 16).ok());
            if port == Some(runtime.port) && fields.get(9).is_some_and(|inode| sockets.contains(*inode)) {
                return Ok(Some(start_ticks));
            }
        }
    }
    Ok(None)
}

fn server_responds(port: u16) -> bool {
    let deadline = Instant::now() + HEALTH_TIMEOUT;
    for ip in [IpAddr::V4(Ipv4Addr::LOCALHOST), IpAddr::V6(Ipv6Addr::LOCALHOST)] {
        if http_response(SocketAddr::new(ip, port), deadline).is_ok() {
            return true;
        }
    }
    false
}

fn http_response(address: SocketAddr, deadline: Instant) -> Result<()> {
    let remaining = || deadline.checked_duration_since(Instant::now()).context("T3 health check timed out");
    let mut stream = TcpStream::connect_timeout(&address, remaining()?)?;
    stream.set_write_timeout(Some(remaining()?))?;
    stream.write_all(format!("HEAD / HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n").as_bytes())?;
    let mut line = Vec::new();
    while line.len() < 128 {
        stream.set_read_timeout(Some(remaining()?))?;
        let mut byte = [0];
        if stream.read(&mut byte)? == 0 {
            break;
        }
        line.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
    }
    let line = String::from_utf8(line)?;
    let mut fields = line.split_whitespace();
    if !matches!(fields.next(), Some("HTTP/1.0" | "HTTP/1.1"))
        || !fields.next().is_some_and(|status| status.parse::<u16>().is_ok_and(|code| (200..500).contains(&code)))
    {
        bail!("T3 server did not return a healthy HTTP response");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(include_str!("../tests/fixtures/schema.sql")).unwrap();
        connection
            .execute_batch(
                "INSERT INTO projection_threads(thread_id, title) VALUES ('thread', 'Fix the bug');
             INSERT INTO projection_thread_sessions VALUES ('thread', 'running', 'turn');
             INSERT INTO provider_session_runtime VALUES
               ('thread', 'codex', 'running', '2026-10-03T10:00:00.000Z', '{\"activeTurnId\":\"turn\"}');
             INSERT INTO projection_turns(thread_id, turn_id, state) VALUES ('thread', 'turn', 'running');",
            )
            .unwrap();
        connection
    }

    fn threads(connection: &Connection) -> Vec<Thread> {
        read_threads(connection, "server", "2026-10-03T09:00:00.000Z").unwrap()
    }

    #[test]
    fn only_the_current_active_turn_counts_even_when_an_old_turn_is_still_running() {
        let connection = database();
        connection
            .execute_batch(
                "INSERT INTO projection_turns(thread_id, turn_id, state) VALUES ('thread', 'old', 'running');",
            )
            .unwrap();
        let running = threads(&connection);
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].seq, 1);
        connection
            .execute_batch("UPDATE projection_thread_sessions SET status = 'ready', active_turn_id = NULL;")
            .unwrap();
        assert!(threads(&connection).is_empty(), "an idle provider process does not count as work");
    }

    #[test]
    fn deleted_stopped_completed_or_unconfirmed_turns_do_not_count() {
        for mutation in [
            "UPDATE projection_threads SET deleted_at = '2026-10-03T10:01:00.000Z';",
            "UPDATE projection_turns SET state = 'completed';",
            "UPDATE provider_session_runtime SET status = 'stopped';",
            "UPDATE provider_session_runtime SET last_seen_at = '2026-10-03T08:00:00.000Z';",
            "UPDATE provider_session_runtime SET runtime_payload_json = '{\"activeTurnId\":\"other\"}';",
            "UPDATE projection_thread_sessions SET status = 'stopped';",
        ] {
            let connection = database();
            connection.execute_batch(mutation).unwrap();
            assert!(threads(&connection).is_empty(), "mutation: {mutation}");
        }
    }

    #[test]
    fn permissions_are_blocked_but_async_questions_do_not_hide_active_work() {
        let connection = database();
        connection.execute_batch("UPDATE projection_threads SET pending_user_input_count = 1;").unwrap();
        assert_eq!(threads(&connection)[0].status, ThreadStatus::Working);
        connection.execute_batch("UPDATE projection_threads SET pending_approval_count = 1;").unwrap();
        assert_eq!(threads(&connection)[0].status, ThreadStatus::Blocked);
    }

    #[test]
    fn a_new_turn_has_a_new_identity_for_the_stuck_timer() {
        let connection = database();
        let first = threads(&connection).remove(0);
        connection
            .execute_batch(
                "INSERT INTO projection_turns(thread_id, turn_id, state) VALUES ('thread', 'next', 'running');
             UPDATE projection_thread_sessions SET active_turn_id = 'next';
             UPDATE provider_session_runtime SET runtime_payload_json = '{\"activeTurnId\":\"next\"}';",
            )
            .unwrap();
        let next = threads(&connection).remove(0);
        assert_eq!(first.id, next.id);
        assert_ne!(first.seq, next.seq);
        let restarted = read_threads(&connection, "restarted-server", "2026-10-03T09:00:00.000Z").unwrap();
        assert_ne!(next.id, restarted[0].id);
    }

    #[test]
    fn schema_changes_are_reported_as_errors() {
        let connection = Connection::open_in_memory().unwrap();
        let error = read_threads(&connection, "server", "2026-10-03T09:00:00.000Z").unwrap_err();
        assert!(error.to_string().contains("database schema may have changed"));
    }
}
