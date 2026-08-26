//! A minimal in-memory MCP server for tests and offline smoke-testing.
//!
//! It speaks the same stdio transport a real Postgres MCP server does
//! (newline-delimited JSON-RPC 2.0) and exposes one `execute_sql` tool that
//! returns a small canned result set. No database required.
//!
//! Use it two ways:
//! * integration tests spawn it via `CARGO_BIN_EXE_mock_mcp_server`;
//! * point `config.json` at it to try the agent without Postgres, e.g.
//!   `"mcp_server": { "command": "target/debug/mock_mcp_server", "args": [] }`.
//!
//! The canned data is three rows of regional sales so analytics (group_by, top,
//! describe, and the DataFusion `op=sql` path) have something real to chew on.

use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

const CANNED_ROWS: &str = r#"[
  {"region":"east","product":"widget","sales":100,"orders":3},
  {"region":"east","product":"gadget","sales":50,"orders":1},
  {"region":"west","product":"widget","sales":200,"orders":4}
]"#;

/// A healthy `wal_level` for the CDC-inspect demo.
const WAL_LEVEL_ROWS: &str = r#"[{"wal_level":"logical"}]"#;

/// A healthy replication slot for the CDC-inspect demo (active, tiny lag).
const SLOT_ROWS: &str = r#"[
  {"slot_name":"clickhouse_analytics","plugin":"pgoutput","slot_type":"logical","active":true,"lag_bytes":8192}
]"#;

/// Total revenue as the Postgres source of truth reports it, for the
/// cross-engine parity demo. Must equal the ClickHouse mock's parity total.
const PARITY_ROWS: &str = r#"[{"revenue_total":4580}]"#;

/// Pick canned rows based on the SQL so the CDC-inspect demo works offline.
fn rows_for_sql(sql: &str) -> &'static str {
    let s = sql.to_lowercase();
    if s.contains("wal_level") {
        WAL_LEVEL_ROWS
    } else if s.contains("pg_replication_slots") {
        SLOT_ROWS
    } else if s.contains("revenue_total") {
        PARITY_ROWS
    } else {
        CANNED_ROWS
    }
}

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(trimmed) else {
            continue; // ignore garbage
        };

        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let id = msg.get("id").cloned();

        // Notifications (no id) get no response.
        let Some(id) = id else { continue };

        let result = match method {
            "initialize" => json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "mock-mcp-server", "version": "0.1.0" }
            }),
            "tools/list" => json!({
                "tools": [{
                    "name": "execute_sql",
                    "description": "Run SQL against the mock dataset (returns canned rows).",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "sql": { "type": "string" } },
                        "required": ["sql"]
                    }
                }]
            }),
            "tools/call" => {
                let name = msg
                    .get("params")
                    .and_then(|p| p.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if name == "execute_sql" {
                    let sql = msg
                        .get("params")
                        .and_then(|p| p.get("arguments"))
                        .and_then(|a| a.get("sql"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    json!({ "content": [{ "type": "text", "text": rows_for_sql(sql) }] })
                } else {
                    json!({
                        "content": [{ "type": "text", "text": format!("unknown tool: {name}") }],
                        "isError": true
                    })
                }
            }
            _ => json!({ "content": [{ "type": "text", "text": "" }] }),
        };

        let response = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        let mut out = serde_json::to_string(&response).unwrap();
        out.push('\n');
        if stdout.write_all(out.as_bytes()).is_err() {
            break;
        }
        let _ = stdout.flush();
    }
}
