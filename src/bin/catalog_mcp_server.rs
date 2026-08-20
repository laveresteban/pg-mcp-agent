//! A data-catalog MCP server.
//!
//! It loads a catalog JSON file (path from the first CLI arg, else the
//! `CATALOG_FILE` env var, else `catalog.json`) and serves it over the MCP
//! stdio protocol. Point the agent at it (alongside a Postgres server) so the
//! semantic layer can be grounded in real business metadata.
//!
//! Tools:
//!   * `list_tables`       — tables with descriptions
//!   * `describe_table`    — columns (name/type/description) + lineage for a table
//!   * `get_glossary`      — business terms and definitions
//!   * `catalog_dump`      — the whole catalog as one JSON object (used by init-specs)

use pg_mcp_agent::catalog::Catalog;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

fn main() {
    let path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("CATALOG_FILE").ok())
        .unwrap_or_else(|| "catalog.json".to_string());

    let catalog = match Catalog::load(std::path::Path::new(&path)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("catalog_mcp_server: {e:#}");
            // Serve an empty catalog rather than crashing the transport.
            Catalog::default()
        }
    };

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let Some(id) = msg.get("id").cloned() else {
            continue;
        };

        let result = handle(&catalog, method, &msg);
        let response = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        let mut out = serde_json::to_string(&response).unwrap();
        out.push('\n');
        if stdout.write_all(out.as_bytes()).is_err() {
            break;
        }
        let _ = stdout.flush();
    }
}

fn handle(catalog: &Catalog, method: &str, msg: &Value) -> Value {
    match method {
        "initialize" => json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "catalog-mcp-server", "version": "0.1.0" }
        }),
        "tools/list" => json!({ "tools": tool_specs() }),
        "tools/call" => call(catalog, msg),
        _ => json!({ "content": [{ "type": "text", "text": "" }] }),
    }
}

fn text_result(value: &Value) -> Value {
    json!({ "content": [{ "type": "text", "text": value.to_string() }] })
}

fn call(catalog: &Catalog, msg: &Value) -> Value {
    let params = msg.get("params");
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let args = params
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));

    match name {
        "list_tables" => {
            let tables: Vec<Value> = catalog
                .tables
                .iter()
                .map(|t| json!({ "name": t.name, "description": t.description }))
                .collect();
            text_result(&json!(tables))
        }
        "describe_table" => {
            let table_name = args.get("table").and_then(Value::as_str).unwrap_or("");
            match catalog.table(table_name) {
                Some(t) => text_result(&json!(t)),
                None => json!({
                    "content": [{ "type": "text", "text": format!("no such table: {table_name}") }],
                    "isError": true
                }),
            }
        }
        "get_glossary" => text_result(&json!(catalog.glossary)),
        "catalog_dump" => text_result(&json!(catalog)),
        other => json!({
            "content": [{ "type": "text", "text": format!("unknown tool: {other}") }],
            "isError": true
        }),
    }
}

fn tool_specs() -> Vec<Value> {
    vec![
        json!({
            "name": "list_tables",
            "description": "List catalog tables with their descriptions.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "describe_table",
            "description": "Columns (name/type/description) and lineage for one table.",
            "inputSchema": {
                "type": "object",
                "properties": { "table": { "type": "string" } },
                "required": ["table"]
            }
        }),
        json!({
            "name": "get_glossary",
            "description": "Business glossary: terms and their definitions.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "catalog_dump",
            "description": "The entire catalog (tables, columns, lineage, glossary) as one JSON object.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
    ]
}
