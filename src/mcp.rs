//! Minimal MCP client over the stdio transport.
//!
//! MCP's stdio transport is newline-delimited JSON-RPC 2.0: one JSON object per
//! line on the child's stdin/stdout. We implement just enough of it to
//! `initialize`, `tools/list`, and `tools/call` against an existing Postgres
//! MCP server.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};

const PROTOCOL_VERSION: &str = "2024-11-05";

/// A single tool advertised by the MCP server.
#[derive(Debug, Clone)]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's arguments (the MCP `inputSchema`).
    pub input_schema: Value,
}

pub struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: i64,
}

/// Resolve a server command across platforms. Configs may point at a local
/// build artifact with (or without) a `.exe` suffix; the suffix only exists on
/// Windows, so a config written on one OS would fail to spawn on the other.
/// If the exact path doesn't exist, try toggling the `.exe` suffix. PATH-based
/// commands (`uvx`, `npx`) don't exist as relative files, so they fall through
/// unchanged.
fn resolve_command(command: &str) -> String {
    if Path::new(command).exists() {
        return command.to_string();
    }
    let alt = match command.strip_suffix(".exe") {
        Some(stripped) => stripped.to_string(),
        None => format!("{command}.exe"),
    };
    if Path::new(&alt).exists() {
        return alt;
    }
    command.to_string()
}

impl McpClient {
    /// Spawn the server process and complete the MCP initialize handshake.
    pub async fn connect(
        command: &str,
        args: &[String],
        envs: &[(String, String)],
    ) -> Result<Self> {
        let command = resolve_command(command);
        let mut cmd = tokio::process::Command::new(&command);
        cmd.args(args)
            .envs(envs.iter().cloned())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()); // let the server log to our stderr

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn MCP server: {command}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("no stdin on child"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("no stdout on child"))?;
        let stdout = BufReader::new(stdout).lines();

        let mut client = McpClient {
            child,
            stdin,
            stdout,
            next_id: 1,
        };

        client.initialize().await?;
        Ok(client)
    }

    async fn initialize(&mut self) -> Result<()> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "pg-mcp-agent", "version": "0.1.0" }
        });
        let _ = self.request("initialize", params).await?;
        // Per spec, tell the server we're ready.
        self.notify("notifications/initialized", json!({})).await?;
        Ok(())
    }

    /// Parse a `tools` result value into a Vec<Tool>.
    ///
    /// This is separated out so it can be unit-tested without spawning a child
    /// process.
    pub(crate) fn parse_tools_from_value(value: &Value) -> Result<Vec<Tool>> {
        let arr = value
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("tools/list returned no `tools` array"))?;

        let mut tools = Vec::with_capacity(arr.len());
        for t in arr {
            let name = t
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("tool missing name"))?
                .to_string();
            let description = t
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let input_schema = t
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object" }));
            tools.push(Tool {
                name,
                description,
                input_schema,
            });
        }
        Ok(tools)
    }

    /// List the tools the server exposes.
    pub async fn list_tools(&mut self) -> Result<Vec<Tool>> {
        let result = self.request("tools/list", json!({})).await?;
        Self::parse_tools_from_value(&result)
    }

    /// Call a tool and return its textual result.
    ///
    /// MCP tool results come back as a `content` array of typed parts; we
    /// concatenate the text parts, which is what a chat model needs to read.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<String> {
        let params = json!({ "name": name, "arguments": arguments });
        let result = self.request("tools/call", params).await?;

        if result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let text = extract_text(&result);
            bail!("tool `{name}` reported an error: {text}");
        }
        Ok(extract_text(&result))
    }

    // --- JSON-RPC plumbing -------------------------------------------------

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.write_message(&msg).await?;

        // Read lines until we get the response with our id, skipping any
        // notifications or log lines the server interleaves.
        loop {
            let line = self
                .stdout
                .next_line()
                .await
                .context("reading from MCP server")?
                .ok_or_else(|| anyhow!("MCP server closed the connection"))?;

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                // Not JSON (stray log line on stdout); ignore.
                continue;
            };

            match value.get("id").and_then(Value::as_i64) {
                Some(resp_id) if resp_id == id => {
                    if let Some(err) = value.get("error") {
                        bail!("MCP error on `{method}`: {err}");
                    }
                    return value
                        .get("result")
                        .cloned()
                        .ok_or_else(|| anyhow!("response had neither result nor error"));
                }
                _ => continue, // a notification or a response to another id
            }
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.write_message(&msg).await
    }

    async fn write_message(&mut self, msg: &Value) -> Result<()> {
        let mut line = serde_json::to_string(msg)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    pub async fn shutdown(mut self) {
        // Dropping stdin signals EOF; give the child a moment, then reap.
        drop(self.stdin);
        let _ = self.child.wait().await;
    }
}

/// Pull the concatenated text out of an MCP `content` array.
fn extract_text(result: &Value) -> String {
    let Some(parts) = result.get("content").and_then(Value::as_array) else {
        return result.to_string();
    };
    let mut out = String::new();
    for part in parts {
        if part.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(t) = part.get("text").and_then(Value::as_str) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            }
        }
    }
    if out.is_empty() {
        result.to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_command_passes_through_pathless_commands() {
        // PATH-based commands don't exist as relative files, so they're unchanged.
        assert_eq!(resolve_command("uvx"), "uvx");
        assert_eq!(resolve_command("npx"), "npx");
    }

    #[test]
    fn resolve_command_toggles_exe_suffix_to_find_the_built_binary() {
        // The current cargo build produces this test binary's crate binaries.
        // Whichever suffix is right for the host, resolve_command should land on
        // an existing file when the other suffix is given.
        let base = "target/debug/mock_mcp_server";
        let with_exe = format!("{base}.exe");
        let resolved_from_bare = resolve_command(base);
        let resolved_from_exe = resolve_command(&with_exe);
        // At least one form exists after a build; both inputs resolve to it.
        if Path::new(base).exists() || Path::new(&with_exe).exists() {
            assert!(Path::new(&resolved_from_bare).exists());
            assert!(Path::new(&resolved_from_exe).exists());
        }
    }

    #[test]
    fn extract_text_returns_full_json_when_no_content() {
        let v = json!({ "foo": "bar" });
        let got = extract_text(&v);
        assert_eq!(got, v.to_string());
    }

    #[test]
    fn extract_text_concatenates_text_parts_with_newlines() {
        let v = json!({
            "content": [
                { "type": "text", "text": "hello" },
                { "type": "text", "text": "world" }
            ]
        });
        let got = extract_text(&v);
        assert_eq!(got, "hello\nworld");
    }

    #[test]
    fn extract_text_ignores_non_text_parts_and_falls_back() {
        // If there are no text parts, we fall back to JSON string.
        let v = json!({
            "content": [
                { "type": "image", "url": "http://example" }
            ]
        });
        let got = extract_text(&v);
        assert_eq!(got, v.to_string());
    }

    #[test]
    fn parse_tools_from_value_parses_tools() {
        let v = json!({
            "tools": [
                { "name": "tool_a", "description": "A tool", "inputSchema": { "type": "object" } },
                { "name": "tool_b" }
            ]
        });

        let tools = McpClient::parse_tools_from_value(&v).expect("parse should succeed");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "tool_a");
        assert_eq!(tools[0].description, "A tool");
        assert!(tools[0].input_schema.is_object());

        assert_eq!(tools[1].name, "tool_b");
        // missing description defaults to empty string
        assert_eq!(tools[1].description, "");
        // missing inputSchema defaults to an object schema
        assert!(tools[1].input_schema.is_object());
    }

    #[tokio::test]
    async fn mcp_integration_with_mock_server() {
        // Spawn the Python mock MCP server included in the repo under tests/.
        let args = vec!["tests/mock_mcp_server.py".to_string()];
        let envs: &[(String, String)] = &[];

        let mut client = McpClient::connect("python", &args, envs)
            .await
            .expect("connect to mock server");

        let tools = client.list_tools().await.expect("list tools");
        assert_eq!(tools.len(), 2);
        let names: Vec<String> = tools.into_iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["echo".to_string(), "error".to_string()]);

        let out = client
            .call_tool("echo", json!({"msg": "hello"}))
            .await
            .expect("call echo");
        assert_eq!(out, "echoed: hello");

        // The `error` tool returns isError=true; call_tool should return Err.
        let err = client.call_tool("error", json!({})).await;
        assert!(err.is_err());

        client.shutdown().await;
    }
}
