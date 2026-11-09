//! A minimal MCP server over stdio (newline-delimited JSON-RPC 2.0) so an
//! agent can turn always-on on and off. It talks to the daemon through the
//! same socket requests as the CLI.

use std::io::{self, BufRead, Write};
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::client;
use crate::util::parse_duration;

const DEFAULT_PROTOCOL: &str = "2025-06-18";
const INSTRUCTIONS: &str = "Controls t3-keep-awake on this WSL machine's Windows host. By default it keeps \
Windows awake only while T3 Code threads are working. `always_on` keeps Windows awake regardless, until \
`always_off` or for a duration; the display can still turn off and lock.";

pub trait Backend {
    fn status(&self) -> Result<String>;
    fn always_on(&self, duration: Option<Duration>) -> Result<String>;
    fn always_off(&self) -> Result<String>;
}

struct Daemon;

impl Backend for Daemon {
    fn status(&self) -> Result<String> {
        client::status_text()
    }

    fn always_on(&self, duration: Option<Duration>) -> Result<String> {
        client::always_on(duration)
    }

    fn always_off(&self) -> Result<String> {
        client::always_off()
    }
}

pub fn serve() -> Result<()> {
    let mut stdout = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle_line(&line, &Daemon) {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn handle_line(line: &str, backend: &impl Backend) -> Option<Value> {
    match serde_json::from_str::<Value>(line) {
        Ok(Value::Array(_)) => Some(error(Value::Null, -32600, "batches are not supported")),
        Ok(message) => handle(&message, backend),
        Err(_) => Some(error(Value::Null, -32700, "parse error")),
    }
}

/// Notifications (no `id`) get no reply.
fn handle(message: &Value, backend: &impl Backend) -> Option<Value> {
    let id = message.get("id")?.clone();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match message.get("method").and_then(Value::as_str) {
        Some("initialize") => json!({
            "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(DEFAULT_PROTOCOL),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "t3-keep-awake", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        }),
        Some("ping") => json!({}),
        Some("tools/list") => json!({ "tools": tools() }),
        Some("tools/call") => call(&params, backend),
        Some(method) => return Some(error(id, -32601, &format!("method not found: {method}"))),
        None => return Some(error(id, -32600, "missing method")),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn tools() -> Value {
    let no_arguments = json!({ "type": "object", "properties": {}, "additionalProperties": false });
    json!([
        {
            "name": "status",
            "description": "Show whether Windows is being kept awake and why: always-on, T3 threads working, \
                            or idle. Includes how long a timed always-on has left.",
            "inputSchema": no_arguments,
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "always_on",
            "description": "Keep Windows awake regardless of T3 activity. Without a duration it stays on, across \
                            restarts and reboots, until always_off. The display can still turn off and lock. \
                            Calling it again replaces the previous duration.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "duration": {
                        "type": "string",
                        "description": "How long to stay on, with a unit: 90m, 2h, 1h30m, 1d. Omit to stay on \
                                        until always_off.",
                    },
                },
                "additionalProperties": false,
            },
            "annotations": { "idempotentHint": true, "destructiveHint": false },
        },
        {
            "name": "always_off",
            "description": "Turn always-on off. Windows then stays awake only while T3 threads are working, and \
                            follows its normal sleep settings otherwise.",
            "inputSchema": no_arguments,
            "annotations": { "idempotentHint": true, "destructiveHint": false },
        },
    ])
}

fn call(params: &Value, backend: &impl Backend) -> Value {
    let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let outcome = match params.get("name").and_then(Value::as_str) {
        Some("status") => backend.status(),
        Some("always_on") => match arguments.get("duration") {
            None | Some(Value::Null) => backend.always_on(None),
            Some(Value::String(text)) if text.trim().is_empty() => backend.always_on(None),
            Some(Value::String(text)) => {
                parse_duration(text).map_err(|error| anyhow!(error)).and_then(|d| backend.always_on(Some(d)))
            }
            Some(_) => Err(anyhow!("duration must be a string such as \"2h\"")),
        },
        Some("always_off") => backend.always_off(),
        Some(other) => Err(anyhow!("unknown tool {other:?}")),
        None => Err(anyhow!("missing tool name")),
    };
    match outcome {
        Ok(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
        Err(error) => json!({ "content": [{ "type": "text", "text": format!("{error:#}") }], "isError": true }),
    }
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<String>>,
    }

    impl Backend for Fake {
        fn status(&self) -> Result<String> {
            self.calls.borrow_mut().push("status".into());
            Ok("state    idle\n".into())
        }

        fn always_on(&self, duration: Option<Duration>) -> Result<String> {
            self.calls.borrow_mut().push(format!("on {duration:?}"));
            Ok("on".into())
        }

        fn always_off(&self) -> Result<String> {
            Err(anyhow!("daemon is not running"))
        }
    }

    fn send(fake: &Fake, message: Value) -> Option<Value> {
        handle_line(&message.to_string(), fake)
    }

    fn call_tool(fake: &Fake, name: &str, arguments: Value) -> Value {
        let message = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
                             "params": {"name": name, "arguments": arguments}});
        send(fake, message).unwrap()["result"].clone()
    }

    #[test]
    fn it_negotiates_and_lists_its_tools() {
        let fake = Fake::default();
        let init = send(
            &fake,
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}}),
        )
        .unwrap();
        assert_eq!(init["id"], 1);
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "t3-keep-awake");
        assert!(send(&fake, json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
        let list = send(&fake, json!({"jsonrpc": "2.0", "id": "a", "method": "tools/list"})).unwrap();
        let names: Vec<_> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].clone()).collect();
        assert_eq!(names, ["status", "always_on", "always_off"]);
    }

    #[test]
    fn tools_reach_the_backend_with_parsed_durations() {
        let fake = Fake::default();
        assert_eq!(call_tool(&fake, "status", json!({}))["content"][0]["text"], "state    idle\n");
        call_tool(&fake, "always_on", json!({}));
        call_tool(&fake, "always_on", json!({"duration": "1h30m"}));
        assert_eq!(*fake.calls.borrow(), ["status", "on None", "on Some(5400s)"]);
    }

    #[test]
    fn failures_are_tool_errors_not_protocol_errors() {
        let fake = Fake::default();
        let bad = call_tool(&fake, "always_on", json!({"duration": "90"}));
        assert_eq!(bad["isError"], true);
        assert!(bad["content"][0]["text"].as_str().unwrap().contains("invalid duration"));
        assert_eq!(call_tool(&fake, "always_on", json!({"duration": 90}))["isError"], true);
        assert_eq!(call_tool(&fake, "always_off", json!({}))["content"][0]["text"], "daemon is not running");
        assert_eq!(call_tool(&fake, "reboot", json!({}))["isError"], true);
        assert_eq!(fake.calls.borrow().len(), 0, "nothing reached the backend");
    }

    #[test]
    fn malformed_messages_get_json_rpc_errors() {
        let fake = Fake::default();
        assert_eq!(handle_line("{nope", &fake).unwrap()["error"]["code"], -32700);
        assert_eq!(handle_line("[]", &fake).unwrap()["error"]["code"], -32600);
        let unknown = send(&fake, json!({"jsonrpc": "2.0", "id": 3, "method": "resources/list"})).unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
        assert_eq!(unknown["id"], 3);
    }
}
