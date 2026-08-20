//! End-to-end test of the MCP stdio client against the bundled mock server.
//!
//! This exercises the real transport: spawning a child process, the initialize
//! handshake, `tools/list`, and `tools/call` with argument passing and result
//! extraction — none of which the in-module unit tests can cover.

use pg_mcp_agent::analytics::{Analysis, AnalyticsEngine, BuiltinEngine, Table};
use pg_mcp_agent::config::McpServerConfig;
use pg_mcp_agent::mcp::McpClient;
use pg_mcp_agent::router::McpRouter;
use serde_json::json;

/// Path to the compiled mock server binary (provided by Cargo to integration tests).
const MOCK_SERVER: &str = env!("CARGO_BIN_EXE_mock_mcp_server");
const MOCK_CH_SERVER: &str = env!("CARGO_BIN_EXE_mock_ch_server");
const CATALOG_SERVER: &str = env!("CARGO_BIN_EXE_catalog_mcp_server");
const CATALOG_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/catalog.example.json");

fn server(name: &str) -> McpServerConfig {
    McpServerConfig {
        name: Some(name.to_string()),
        command: MOCK_SERVER.to_string(),
        args: vec![],
        env: Default::default(),
        dialect: None,
    }
}

#[tokio::test]
async fn connects_lists_and_calls_tool() {
    let mut client = McpClient::connect(MOCK_SERVER, &[], &[])
        .await
        .expect("connect to mock server");

    let tools = client.list_tools().await.expect("list tools");
    assert!(
        tools.iter().any(|t| t.name == "execute_sql"),
        "expected execute_sql tool, got: {:?}",
        tools.iter().map(|t| &t.name).collect::<Vec<_>>()
    );

    let out = client
        .call_tool("execute_sql", json!({ "sql": "SELECT * FROM sales" }))
        .await
        .expect("call execute_sql");

    // The mock returns canned regional sales rows.
    assert!(out.contains("east"), "output: {out}");
    assert!(out.contains("west"), "output: {out}");

    client.shutdown().await;
}

#[tokio::test]
async fn tool_output_feeds_the_analytics_engine() {
    let mut client = McpClient::connect(MOCK_SERVER, &[], &[])
        .await
        .expect("connect");
    let out = client
        .call_tool("execute_sql", json!({ "sql": "SELECT * FROM sales" }))
        .await
        .expect("call");
    client.shutdown().await;

    // Parse the tool output the way the agent does, then run local analytics.
    let table = Table::from_tool_output(&out).expect("parse rows into a table");
    let analysis = Analysis::GroupBy {
        by: vec!["region".into()],
        agg: pg_mcp_agent::analytics::Agg::Sum,
        column: Some("sales".into()),
    };
    let summary = BuiltinEngine.run(&table, &analysis).expect("group_by");
    assert!(summary.contains("east :: 150"), "summary: {summary}");
    assert!(summary.contains("west :: 200"), "summary: {summary}");
}

#[tokio::test]
async fn unknown_tool_reports_error() {
    let mut client = McpClient::connect(MOCK_SERVER, &[], &[])
        .await
        .expect("connect");
    let err = client.call_tool("no_such_tool", json!({})).await;
    assert!(
        err.is_err(),
        "expected error for unknown tool, got: {err:?}"
    );
    client.shutdown().await;
}

#[tokio::test]
async fn router_single_server_exposes_bare_names() {
    let mut router = McpRouter::connect(&[server("pg")]).await.expect("connect");
    assert_eq!(router.server_count(), 1);
    let names: Vec<&str> = router.tools().iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"execute_sql"), "got {names:?}");
    let out = router
        .call_tool("execute_sql", json!({ "sql": "SELECT 1" }))
        .await
        .expect("call");
    assert!(out.contains("east"));
    router.shutdown().await;
}

#[tokio::test]
async fn router_namespaces_colliding_tools_and_routes_each() {
    // Two servers exposing the same `execute_sql` tool must both be reachable.
    let mut router = McpRouter::connect(&[server("pg"), server("duck")])
        .await
        .expect("connect two");
    assert_eq!(router.server_count(), 2);
    let names: Vec<&str> = router.tools().iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"pg__execute_sql"), "got {names:?}");
    assert!(names.contains(&"duck__execute_sql"), "got {names:?}");

    // Both routes resolve and return the canned rows.
    let a = router
        .call_tool("pg__execute_sql", json!({"sql": "SELECT 1"}))
        .await
        .expect("pg");
    let b = router
        .call_tool("duck__execute_sql", json!({"sql": "SELECT 1"}))
        .await
        .expect("duck");
    assert!(a.contains("west"));
    assert!(b.contains("west"));
    router.shutdown().await;
}

#[tokio::test]
async fn pg_and_clickhouse_servers_route_by_dialect() {
    // The Postgres + ClickHouse copilot demo, offline: two mock servers, one
    // tagged clickhouse. Their SQL tools have different names (execute_sql vs
    // run_select_query), so both are exposed bare and dialect-routed.
    let pg = McpServerConfig {
        name: Some("pg".to_string()),
        command: MOCK_SERVER.to_string(),
        args: vec![],
        env: Default::default(),
        dialect: Some("postgres".to_string()),
    };
    let ch = McpServerConfig {
        name: Some("ch".to_string()),
        command: MOCK_CH_SERVER.to_string(),
        args: vec![],
        env: Default::default(),
        dialect: Some("clickhouse".to_string()),
    };
    let mut router = McpRouter::connect(&[pg, ch]).await.expect("connect pg+ch");
    assert_eq!(router.server_count(), 2);

    // Dialect-aware SQL-tool selection lands on the right server's tool.
    use pg_mcp_agent::guard::Dialect;
    let (pg_tool, _) = router
        .sql_tool_for_dialect(Dialect::Postgres)
        .expect("pg tool");
    let (ch_tool, _) = router
        .sql_tool_for_dialect(Dialect::ClickHouse)
        .expect("ch tool");
    assert_eq!(pg_tool, "execute_sql");
    assert_eq!(ch_tool, "run_select_query");
    assert_eq!(
        router.dialect_for_tool("run_select_query"),
        Dialect::ClickHouse
    );
    assert_eq!(router.dialect_for_tool("execute_sql"), Dialect::Postgres);

    // The ClickHouse server returns its own rollup rows (revenue by day).
    let ch_out = router
        .call_tool(
            "run_select_query",
            json!({ "query": "SELECT * FROM sales_daily" }),
        )
        .await
        .expect("ch query");
    assert!(ch_out.contains("revenue"), "ch output: {ch_out}");

    router.shutdown().await;
}

#[tokio::test]
async fn catalog_server_serves_tables_glossary_and_dump() {
    let cfg = McpServerConfig {
        name: Some("catalog".to_string()),
        command: CATALOG_SERVER.to_string(),
        args: vec![CATALOG_FILE.to_string()],
        env: Default::default(),
        dialect: None,
    };
    let mut router = McpRouter::connect(&[cfg]).await.expect("connect catalog");
    let names: Vec<&str> = router.tools().iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"catalog_dump"), "got {names:?}");
    assert!(names.contains(&"get_glossary"));

    // catalog_dump round-trips into a Catalog with the sample glossary.
    let dump = router
        .call_tool("catalog_dump", json!({}))
        .await
        .expect("dump");
    let catalog: pg_mcp_agent::catalog::Catalog =
        serde_json::from_str(&dump).expect("parse catalog");
    assert!(catalog.glossary.iter().any(|g| g.term == "revenue"));
    assert!(catalog.table("orders").is_some());

    // A catalog-grounded spec is generated from it and parses back.
    let spec = pg_mcp_agent::specgen::generate_spec_from_catalog(&catalog);
    let layer = pg_mcp_agent::semantics::parse_str(&spec);
    assert!(!layer.glossary.is_empty());
    assert!(layer.specs.iter().any(|s| s.name == "rows from orders"));

    router.shutdown().await;
}

#[tokio::test]
async fn spec_generation_from_tool_output() {
    // The mock returns rows without table_name/column_name, so specgen should
    // fall back to a generic stub that still round-trips through the parser.
    let mut client = McpClient::connect(MOCK_SERVER, &[], &[])
        .await
        .expect("connect");
    let out = client
        .call_tool(
            "execute_sql",
            json!({ "sql": pg_mcp_agent::specgen::INTROSPECTION_SQL }),
        )
        .await
        .expect("call");
    client.shutdown().await;

    let table = Table::from_tool_output(&out).expect("parse rows");
    let spec = pg_mcp_agent::specgen::generate_spec(&table);
    let layer = pg_mcp_agent::semantics::parse_str(&spec);
    assert!(
        !layer.specs.is_empty(),
        "generated spec should contain an example"
    );
}
