//! A minimal in-memory *ClickHouse* MCP server for tests and offline demos.
//!
//! It mirrors `mock_mcp_server` but wears ClickHouse's clothes: it exposes the
//! `run_select_query` tool the real `mcp-clickhouse` server uses, and returns a
//! small time-series rollup — the kind of columnar workload ClickHouse is for.
//! Together with `mock_mcp_server` (Postgres) it lets the whole
//! Postgres + ClickHouse copilot demo run with zero external setup.
//!
//! Point a `mcp_servers` entry at it with `"dialect": "clickhouse"`:
//!   `{ "name": "ch", "command": "target/debug/mock_ch_server", "dialect": "clickhouse" }`

use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

/// A daily sales rollup — what a ClickHouse materialized view would serve.
const CANNED_ROWS: &str = r#"[
  {"day":"2026-08-14","revenue":1200,"orders":42},
  {"day":"2026-08-15","revenue":1550,"orders":51},
  {"day":"2026-08-16","revenue":1830,"orders":63}
]"#;

/// Healthy Kafka-consumer state, as `system.kafka_consumers` would report it,
/// so `cdc inspect` can show the fan-out path's health offline.
const KAFKA_CONSUMER_ROWS: &str = r#"[
  {"table":"orders_queue","consumer_id":"ch-1","is_currently_used":true,"last_exception":"","num_messages_read":15230},
  {"table":"order_items_queue","consumer_id":"ch-2","is_currently_used":true,"last_exception":"","num_messages_read":41988}
]"#;

/// Total revenue from the ClickHouse rollup, for the cross-engine parity demo:
/// 1200 + 1550 + 1830 = 4580, which must equal the Postgres source total.
const PARITY_ROWS: &str = r#"[{"revenue_total":4580}]"#;

/// Pick canned rows based on the SQL so the CDC fan-out demo works offline.
fn rows_for_sql(sql: &str) -> &'static str {
    let s = sql.to_lowercase();
    if s.contains("kafka_consumers") || s.contains("system.kafka") {
        KAFKA_CONSUMER_ROWS
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
                "serverInfo": { "name": "mock-ch-server", "version": "0.1.0" }
            }),
            "tools/list" => json!({
                "tools": [{
                    "name": "run_select_query",
                    "description": "Run a read-only SELECT against ClickHouse (returns canned rollup rows). Prefer for large aggregations and time-series scans.",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "query": { "type": "string" } },
                        "required": ["query"]
                    }
                }]
            }),
            "tools/call" => {
                let name = msg
                    .get("params")
                    .and_then(|p| p.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if name == "run_select_query" {
                    let sql = msg
                        .get("params")
                        .and_then(|p| p.get("arguments"))
                        .and_then(|a| a.get("query"))
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
