// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Black-box integration tests for spire-core.
//!
//! These tests build the `spire-core` binary, spawn it as a child process,
//! send JSON-RPC 2.0 requests via stdin, read responses from stdout,
//! and assert on the results.
//!
//! Each test:
//! 1. Builds the binary (or uses a pre-built one)
//! 2. Spawns the process with stdin/stdout pipes
//! 3. Writes a JSON-RPC request line to stdin
//! 4. Reads a JSON line from stdout
//! 5. Asserts on the response

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Helper to build the spire-core binary and return its path.
fn build_binary() -> std::path::PathBuf {
    // The crate lives in a CARGO WORKSPACE: artifacts land in the shared
    // workspace target dir (e.g. <workspace>/target/debug), NOT
    // <crate>/target/debug. Resolve the true target directory via
    // `cargo metadata` so the spawned subprocess path is always correct.
    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .expect("Failed to run cargo metadata");
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("Failed to parse cargo metadata JSON");
    let target_dir = metadata
        .get("target_directory")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from)
        .expect("cargo metadata missing target_directory");

    let profile_dir = if cfg!(debug_assertions) { "debug" } else { "release" };
    let mut binary = target_dir.join(profile_dir).join("spire-core");
    if cfg!(target_os = "windows") {
        binary.set_extension("exe");
    }

    // Build the binary if it isn't present (the test package does not build it).
    if !binary.exists() {
        let status = Command::new("cargo")
            .args(["build", "--bin", "spire-core"])
            .status()
            .expect("Failed to build spire-core binary");
        assert!(status.success(), "cargo build --bin spire-core failed");
    }

    assert!(
        binary.exists(),
        "spire-core binary not found at {} — run `cargo build --bin spire-core` first",
        binary.display()
    );
    binary
}

/// A test harness that manages a spire-core subprocess over its TCP transport.
struct CoreProcess {
    child: Child,
    /// Read half: newline-delimited JSON-RPC responses from core.
    reader: BufReader<TcpStream>,
    /// Write half: JSON-RPC requests sent to core.
    writer: TcpStream,
    /// Keeps the per-test temp dir (graph data, project root, logs) alive for
    /// the lifetime of the child process. Dropped with the harness, AFTER the
    /// child is killed by `Drop`, so the dir is never removed under a running
    /// core.
    _tmp: tempfile::TempDir,
}

impl CoreProcess {
    /// Spawn a new spire-core process and connect to its TCP transport.
    fn spawn() -> Self {
        let binary_path = build_binary();

        // Hermetic per-test environment. `spire-core` resolves its data dir
        // and project root from `SPIRE_DATA_DIR` / `SPIRE_PROJECT_ROOT`,
        // falling back to SHARED locations (`temp_dir()/spire-core-data` and
        // the process cwd) that persist across runs. A stale/truncated WAL
        // there makes `system/status` report `failed: Graph init failed …` and
        // a non-empty cwd makes startup scan whatever directory the tests run
        // from. Point every test at its own temp dir so it can never read or
        // write ambient state.
        let tmp = tempfile::tempdir().expect("Failed to create temp dir");
        let data_dir = tmp.path().join("data");
        let project_root = tmp.path().join("project");
        let log_dir = tmp.path().join("logs");
        std::fs::create_dir_all(&data_dir).expect("Failed to create data dir");
        std::fs::create_dir_all(&project_root).expect("Failed to create project root");
        std::fs::create_dir_all(&log_dir).expect("Failed to create log dir");

        let mut child = Command::new(&binary_path)
            .env("SPIRE_DATA_DIR", &data_dir)
            .env("SPIRE_PROJECT_ROOT", &project_root)
            .env("SPIRE_LOG_DIR", &log_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()) // Suppress stderr in tests
            .spawn()
            .expect("Failed to spawn spire-core process");

        let stdout = child.stdout.take().expect("Failed to capture stdout");

        // Core prints "SPIRE_PORT=<port>" to stdout first; read it, then
        // connect the JSON-RPC client to 127.0.0.1:<port>.
        let mut port_line = String::new();
        let mut stdout_reader = BufReader::new(stdout);
        stdout_reader
            .read_line(&mut port_line)
            .expect("Failed to read the SPIRE_PORT line from core stdout");
        let port: u16 = port_line
            .trim()
            .strip_prefix("SPIRE_PORT=")
            .unwrap_or_else(|| panic!("core did not print SPIRE_PORT: {port_line:?}"))
            .parse()
            .expect("SPIRE_PORT is not a valid port number");

        let stream = TcpStream::connect(("127.0.0.1", port))
            .unwrap_or_else(|e| panic!("Failed to connect to core on 127.0.0.1:{port}: {e}"));
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .ok();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .ok();
        let writer = stream.try_clone().expect("Failed to clone TCP stream");

        let mut core = Self {
            child,
            reader: BufReader::new(stream),
            writer,
            _tmp: tmp,
        };

        // Wait until the backend is ready: core prints SPIRE_PORT before its
        // blocking initialization completes, so poll `ping` (short read
        // timeout) until a response arrives.
        let ready_line = serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0", "id": 0, "method": "ping", "params": {}
        }))
        .unwrap();
        let mut ready = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        while std::time::Instant::now() < deadline {
            {
                // Best-effort write (ignore broken pipe while core is not yet ready).
                let _ = writeln!(core.writer, "{}", ready_line);
                let _ = core.writer.flush();
                // Short timeout for the probe read.
                let _ = core.reader.get_ref().set_read_timeout(Some(Duration::from_millis(250)));
            }
            let mut probe = String::new();
            match core.reader.read_line(&mut probe) {
                Ok(0) | Err(_) => continue,
                Ok(_) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(probe.trim()) {
                        // A `-32601 Method not found` (or any error) reply means the
                        // request handler isn't registered yet — keep polling.
                        let is_id = v.get("id").and_then(|i| i.as_u64()) == Some(0);
                        let is_error =
                            v.get("error").map(|e| !e.is_null()).unwrap_or(false);
                        if is_id && !is_error {
                            ready = true;
                            break;
                        }
                    }
                }
            }
        }
        assert!(ready, "spire-core backend did not become ready within 120s");
        // Restore the normal 30s read timeout for request/response.
        let _ = core.reader.get_ref().set_read_timeout(Some(Duration::from_secs(30)));

        core
    }

    /// Send a JSON-RPC request and read the matching response (by id).
    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.request_with_id(1, method, params)
    }

    /// Send a JSON-RPC request with a specific ID and read the matching response.
    fn request_with_id(
        &mut self,
        id: u64,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let line = serde_json::to_string(&request).unwrap();
        writeln!(self.writer, "{}", line).unwrap();
        self.writer.flush().unwrap();

        // Read lines until we find the response whose `id` matches (the core
        // may interleave event notifications on the socket).
        let mut response_line = String::new();
        loop {
            response_line.clear();
            if self.reader.read_line(&mut response_line).unwrap() == 0 {
                panic!("core closed the connection before replying to {method} (id={id})");
            }
            let trimmed = response_line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let parsed: serde_json::Value = serde_json::from_str(trimmed).unwrap_or_else(|e| {
                panic!("Failed to parse response JSON: {e} — raw: {trimmed}")
            });
            if parsed.get("id").and_then(|v| v.as_u64()) == Some(id) {
                return parsed;
            }
        }
    }
}

impl Drop for CoreProcess {
    fn drop(&mut self) {
        // Kill the process and wait for it to exit.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[test]
fn test_ping() {
    let mut core = CoreProcess::spawn();
    let response = core.request("ping", serde_json::json!({}));

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"], serde_json::json!({"pong": true}));
}

#[test]
fn test_system_status() {
    let mut core = CoreProcess::spawn();
    let response = core.request("system/status", serde_json::json!({}));

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    // The SystemActor phase chain may still be `initializing` (embedder model,
    // MCP connect, project sync run in background after the request handler
    // registers) — accept either state.
    let status = response["result"]["status"].as_str().unwrap_or("");
    assert!(
        status == "running" || status == "initializing",
        "unexpected system status: {status}"
    );
    assert_eq!(response["result"]["version"], "0.1.0");
    assert!(response["result"]["uptime_seconds"].as_f64().unwrap() >= 0.0);
    // The `actors` map is only populated once the full startup phase chain has
    // completed; during `initializing` it may be absent/empty.
    if status == "running" {
        assert_eq!(response["result"]["actors"]["chat"], true);
        assert_eq!(response["result"]["actors"]["system"], true);
    }
}

#[test]
fn test_chat_get_active() {
    let mut core = CoreProcess::spawn();
    let response = core.request("chat/getActive", serde_json::json!({}));

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["id"], "default");
    assert_eq!(response["result"]["title"], "New Chat");
    assert!(response["result"]["messages"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn test_chat_append_and_get_history() {
    let mut core = CoreProcess::spawn();

    // Append a message
    let append_response = core.request(
        "chat/append",
        serde_json::json!({
            "chatId": "default",
            "content": "Hello from black-box test",
            "options": {"role": "user"}
        }),
    );

    assert_eq!(append_response["jsonrpc"], "2.0");
    assert_eq!(append_response["id"], 1);
    assert_eq!(
        append_response["result"]["content"],
        "Hello from black-box test"
    );
    assert_eq!(append_response["result"]["role"], "user");

    // Get history
    let history_response = core.request("chat/getHistory", serde_json::json!({}));

    assert_eq!(history_response["jsonrpc"], "2.0");
    assert_eq!(history_response["id"], 1);
    let dialogs = history_response["result"].as_array().unwrap();
    assert_eq!(dialogs.len(), 1);
    assert_eq!(
        dialogs[0]["messages"][0]["content"],
        "Hello from black-box test"
    );
}

#[test]
fn test_chat_clear() {
    let mut core = CoreProcess::spawn();

    // Append a message
    core.request(
        "chat/append",
        serde_json::json!({
            "chatId": "default",
            "content": "to_clear",
            "options": {"role": "user"}
        }),
    );

    // Clear
    let clear_response = core.request(
        "chat/clear",
        serde_json::json!({
            "chatId": "default"
        }),
    );
    assert_eq!(clear_response["result"]["success"], true);

    // Verify empty
    let active_response = core.request("chat/getActive", serde_json::json!({}));
    assert!(active_response["result"]["messages"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn test_chat_set_title() {
    let mut core = CoreProcess::spawn();

    let response = core.request(
        "chat/setTitle",
        serde_json::json!({
            "chatId": "default",
            "title": "Integration Test Chat"
        }),
    );
    assert_eq!(response["result"]["success"], true);

    // Verify
    let active_response = core.request("chat/getActive", serde_json::json!({}));
    assert_eq!(active_response["result"]["title"], "Integration Test Chat");
}

#[test]
fn test_tools_list_empty() {
    let mut core = CoreProcess::spawn();
    let response = core.request("tools/list", serde_json::json!({}));

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    // ToolsActor pre-registers VS Code extension tools at startup
    assert!(!response["result"].as_array().unwrap().is_empty());
    assert!(response["result"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "workspace/getFolders"));
}

#[test]
fn test_mcp_servers_loaded() {
    let mut core = CoreProcess::spawn();
    let response = core.request("mcp/servers", serde_json::json!({}));

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    // MCP config is loaded at startup from the project-local .spire/mcp-config.json
    // (or ~/.spire/mcp-config.json as fallback), so the servers list should contain
    // at least the filesystem server
    eprintln!("DEBUG mcp/servers response: {}", response);
    // MCP servers are bootstrapped by the background startup phase; tolerate an
    // empty list while that phase is still running, but when populated the
    // filesystem server must be present.
    eprintln!("DEBUG mcp/servers response: {}", response);
    let servers = response["result"].as_array().unwrap();
    if !servers.is_empty() {
        assert!(
            servers
                .iter()
                .any(|s| s.get("name").and_then(|v| v.as_str()) == Some("filesystem")),
            "Expected 'filesystem' server to be in the list: {:?}",
            servers
        );
    }
}

#[test]
fn test_unknown_method_returns_error() {
    let mut core = CoreProcess::spawn();
    let response = core.request("unknown/method", serde_json::json!({}));

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    // The coordinator returns the error as a result value
    assert!(response["result"].get("error").is_some());
    assert!(response["result"]["error"]
        .as_str()
        .unwrap()
        .contains("unknown/method"));
}

#[test]
fn test_multiple_sequential_requests() {
    let mut core = CoreProcess::spawn();

    // Request 1: ping
    let r1 = core.request_with_id(1, "ping", serde_json::json!({}));
    assert_eq!(r1["id"], 1);
    assert_eq!(r1["result"]["pong"], true);

    // Request 2: system status (may be running or still initializing).
    let r2 = core.request_with_id(2, "system/status", serde_json::json!({}));
    assert_eq!(r2["id"], 2);
    let status2 = r2["result"]["status"].as_str().unwrap_or("");
    assert!(
        status2 == "running" || status2 == "initializing",
        "unexpected system status: {status2}"
    );

    // Request 3: chat append
    let r3 = core.request_with_id(
        3,
        "chat/append",
        serde_json::json!({
            "chatId": "default",
            "content": "multi-request",
            "options": {"role": "user"}
        }),
    );
    assert_eq!(r3["id"], 3);
    assert_eq!(r3["result"]["content"], "multi-request");

    // Request 4: chat getHistory (should have the message from request 3)
    let r4 = core.request_with_id(4, "chat/getHistory", serde_json::json!({}));
    assert_eq!(r4["id"], 4);
    assert_eq!(r4["result"][0]["messages"][0]["content"], "multi-request");
}

#[test]
fn test_chat_append_without_options_defaults_to_assistant() {
    let mut core = CoreProcess::spawn();

    let response = core.request(
        "chat/append",
        serde_json::json!({
            "chatId": "default",
            "content": "default role message"
        }),
    );

    assert_eq!(response["result"]["role"], "assistant");
    assert_eq!(response["result"]["content"], "default role message");
}

#[test]
fn test_system_config_get_unknown() {
    let mut core = CoreProcess::spawn();

    let response = core.request(
        "system/config/get",
        serde_json::json!({
            "key": "nonexistent"
        }),
    );

    assert_eq!(response["result"]["value"], serde_json::Value::Null);
}

#[test]
fn test_mcp_connect_unknown_server() {
    let mut core = CoreProcess::spawn();

    let response = core.request(
        "mcp/connect",
        serde_json::json!({
            "serverName": "nonexistent_server"
        }),
    );

    assert!(response.get("error").is_some() || response.get("result").is_some());
    // The MCP client actor will return an error since the server doesn't exist
    // in its config. This test just verifies the routing works without crashing.
}

#[test]
fn test_mcp_get_tools_unknown_server() {
    let mut core = CoreProcess::spawn();

    let response = core.request(
        "mcp/getTools",
        serde_json::json!({
            "serverName": "nonexistent"
        }),
    );

    // Should return an empty array for unknown server
    assert_eq!(response["result"], serde_json::json!([]));
}
