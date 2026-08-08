//! Black-box compatibility checks for the MCP stdio process.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

const MODERN_VERSION: &str = "2026-07-28";
const LEGACY_VERSION: &str = "2025-11-25";

struct Companion {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Companion {
    fn sanitized() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_nobox-agent"))
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start nobox-agent");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin: Some(stdin),
            stdout,
        }
    }

    fn send(&mut self, message: Value) -> Option<Value> {
        let stdin = self.stdin.as_mut().expect("open stdin");
        writeln!(stdin, "{message}").expect("write request");
        stdin.flush().expect("flush request");
        message.get("id")?;
        let mut line = String::new();
        assert_ne!(self.stdout.read_line(&mut line).expect("read response"), 0);
        Some(serde_json::from_str(&line).expect("JSON response"))
    }
}

impl Drop for Companion {
    fn drop(&mut self) {
        self.stdin.take();
        let status = self.child.wait().expect("wait for nobox-agent");
        assert!(status.success(), "nobox-agent exited with {status}");
    }
}

fn modern_request(id: u64, method: &str, mut params: Value) -> Value {
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": MODERN_VERSION,
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": { "name": "stdio-test", "version": "1" },
    });
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn assert_server_instructions(result: &Value) {
    let instructions = result["instructions"].as_str().expect("instructions");
    assert!(
        instructions.len() <= 1_000,
        "server instructions used {} bytes",
        instructions.len()
    );
    let prefix = instructions
        .get(..512)
        .expect("the first 512 instruction bytes must be complete UTF-8");
    for topic in [
        "permission-scoped",
        "desktop_snapshot",
        "desktop_subscribe",
        "resync_required",
        "client_capture",
    ] {
        assert!(prefix.contains(topic), "front matter is missing {topic}");
    }
}

#[test]
fn print_mcp_config_needs_no_desktop_and_is_copyable_json() {
    let output = Command::new(env!("CARGO_BIN_EXE_nobox-agent"))
        .arg("--print-mcp-config")
        .env_clear()
        .output()
        .expect("run nobox-agent");

    assert!(output.status.success(), "{:?}", output.status);
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let config: Value = serde_json::from_slice(&output.stdout).expect("JSON config");
    assert_eq!(config["mcpServers"]["nobox"]["command"], "nobox-agent");
}

#[test]
fn modern_discovery_and_tool_listing_need_no_desktop_environment() {
    let mut companion = Companion::sanitized();

    let discover = companion
        .send(modern_request(1, "server/discover", json!({})))
        .expect("response");
    assert_eq!(discover["result"]["supportedVersions"][0], MODERN_VERSION);
    assert_eq!(discover["result"]["serverInfo"]["name"], "nobox-agent");
    assert_server_instructions(&discover["result"]);

    let listing = companion
        .send(modern_request(2, "tools/list", json!({})))
        .expect("response");
    assert!(
        listing["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .any(|tool| tool["name"] == "seat_status")
    );

    let call = companion
        .send(modern_request(
            3,
            "tools/call",
            json!({ "name": "desktop_snapshot", "arguments": {} }),
        ))
        .expect("response");
    assert_eq!(call["result"]["isError"], true);
    assert!(
        call["result"]["content"][0]["text"]
            .as_str()
            .expect("failure text")
            .contains("no live agent seat is advertised")
    );
}

#[test]
fn legacy_initialize_lifecycle_works_without_a_seat_socket() {
    let mut companion = Companion::sanitized();
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": LEGACY_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "legacy-test", "version": "1" },
        },
    });

    let initialized = companion.send(initialize.clone()).expect("response");
    assert_eq!(initialized["result"]["protocolVersion"], LEGACY_VERSION);
    assert!(initialized["result"]["capabilities"]["tools"].is_object());
    assert_server_instructions(&initialized["result"]);

    let early = companion
        .send(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {},
        }))
        .expect("response");
    assert_eq!(early["error"]["code"], -32600);

    companion.send(json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {},
    }));
    let listing = companion
        .send(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/list",
            "params": {},
        }))
        .expect("response");
    assert!(listing["result"]["tools"].is_array());
    assert!(listing["result"].get("resultType").is_none());

    let repeated = companion.send(initialize).expect("response");
    assert_eq!(repeated["error"]["code"], -32600);
}
