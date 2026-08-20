//! Routes tool calls across one or more MCP servers.
//!
//! The agent is an MCP client, so it can hold several server connections at
//! once (a Postgres server, a DuckDB server, a dbt server, …). This router
//! connects them all, merges their tool lists, and routes each call to the
//! right server. Tool names that are unique across servers are exposed as-is;
//! names that collide are prefixed with the server name (`<server>__<tool>`) so
//! the model can still address each one unambiguously.

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

use crate::config::McpServerConfig;
use crate::guard::Dialect;
use crate::mcp::{McpClient, Tool};
use crate::semantics::is_sql_tool;

pub struct McpRouter {
    clients: Vec<McpClient>,
    server_names: Vec<String>,
    /// SQL dialect of each server (parallel to `clients`/`server_names`).
    server_dialects: Vec<Dialect>,
    /// Tools as exposed to the model (names possibly prefixed on collision).
    exposed: Vec<Tool>,
    /// exposed tool name -> (client index, real tool name on that server)
    routes: HashMap<String, (usize, String)>,
}

impl McpRouter {
    /// Connect every configured server and build the routing table.
    pub async fn connect(servers: &[McpServerConfig]) -> Result<Self> {
        // Unique display name per server (suffix the index on duplicates).
        let mut seen = HashSet::new();
        let mut server_names = Vec::with_capacity(servers.len());
        for (i, s) in servers.iter().enumerate() {
            let mut name = s.display_name(i);
            while !seen.insert(name.clone()) {
                name = format!("{name}-{i}");
            }
            server_names.push(name);
        }

        let mut clients = Vec::with_capacity(servers.len());
        let mut tool_lists: Vec<Vec<Tool>> = Vec::with_capacity(servers.len());
        for (i, s) in servers.iter().enumerate() {
            let mut client = McpClient::connect(&s.command, &s.args, &s.env_pairs())
                .await
                .map_err(|e| anyhow!("connecting MCP server `{}`: {e}", server_names[i]))?;
            let tools = client.list_tools().await?;
            tool_lists.push(tools);
            clients.push(client);
        }

        let server_dialects = servers.iter().map(|s| s.dialect()).collect();
        let (exposed, routes) = build_routes(&server_names, &tool_lists);
        Ok(Self {
            clients,
            server_names,
            server_dialects,
            exposed,
            routes,
        })
    }

    pub fn tools(&self) -> &[Tool] {
        &self.exposed
    }

    /// The SQL dialect of the server that owns an exposed tool (Postgres if the
    /// name is unknown). Lets the guard classify each call in the right dialect.
    pub fn dialect_for_tool(&self, exposed_name: &str) -> Dialect {
        dialect_of(&self.routes, &self.server_dialects, exposed_name)
    }

    /// Find the exposed SQL tool for a target dialect: prefer a server whose
    /// dialect matches, else fall back to any SQL tool (untagged single-server
    /// setups). Returns the exposed tool name and its input schema.
    pub fn sql_tool_for_dialect(&self, want: Dialect) -> Option<(String, Value)> {
        select_sql_tool(&self.exposed, &self.routes, &self.server_dialects, want)
    }

    pub fn server_names(&self) -> &[String] {
        &self.server_names
    }

    pub fn server_count(&self) -> usize {
        self.clients.len()
    }

    /// Call a tool by its exposed name, routing to the owning server.
    pub async fn call_tool(&mut self, exposed_name: &str, args: Value) -> Result<String> {
        let (idx, real) = self
            .routes
            .get(exposed_name)
            .cloned()
            .ok_or_else(|| anyhow!("no MCP server exposes a tool named `{exposed_name}`"))?;
        self.clients[idx].call_tool(&real, args).await
    }

    pub async fn shutdown(self) {
        for c in self.clients {
            c.shutdown().await;
        }
    }
}

/// Build the exposed tool list and routing table from per-server tools.
/// Pure and unit-testable (no IO).
fn build_routes(
    server_names: &[String],
    tool_lists: &[Vec<Tool>],
) -> (Vec<Tool>, HashMap<String, (usize, String)>) {
    // Count how many servers expose each tool name.
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for tools in tool_lists {
        for t in tools {
            *counts.entry(t.name.as_str()).or_insert(0) += 1;
        }
    }

    let mut exposed = Vec::new();
    let mut routes = HashMap::new();
    for (idx, tools) in tool_lists.iter().enumerate() {
        for t in tools {
            let mut exposed_name = if counts[t.name.as_str()] > 1 {
                format!("{}__{}", server_names[idx], t.name)
            } else {
                t.name.clone()
            };
            // Guard against an unlikely post-prefix collision.
            while routes.contains_key(&exposed_name) {
                exposed_name.push('_');
            }
            routes.insert(exposed_name.clone(), (idx, t.name.clone()));
            exposed.push(Tool {
                name: exposed_name,
                description: t.description.clone(),
                input_schema: t.input_schema.clone(),
            });
        }
    }
    (exposed, routes)
}

/// The dialect of the server that owns `exposed_name`, defaulting to Postgres.
fn dialect_of(
    routes: &HashMap<String, (usize, String)>,
    dialects: &[Dialect],
    exposed_name: &str,
) -> Dialect {
    routes
        .get(exposed_name)
        .and_then(|(i, _)| dialects.get(*i).copied())
        .unwrap_or_default()
}

/// Pick an exposed SQL tool for `want`: a dialect-matching server first, then
/// any SQL tool as a fallback. Pure and unit-testable.
fn select_sql_tool(
    exposed: &[Tool],
    routes: &HashMap<String, (usize, String)>,
    dialects: &[Dialect],
    want: Dialect,
) -> Option<(String, Value)> {
    let matches = |t: &&Tool| is_sql_tool(&t.name) && dialect_of(routes, dialects, &t.name) == want;
    exposed
        .iter()
        .find(matches)
        .or_else(|| exposed.iter().find(|t| is_sql_tool(&t.name)))
        .map(|t| (t.name.clone(), t.input_schema.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str) -> Tool {
        Tool {
            name: name.to_string(),
            description: String::new(),
            input_schema: json!({}),
        }
    }

    #[test]
    fn unique_names_are_exposed_bare() {
        let names = vec!["pg".to_string(), "duck".to_string()];
        let lists = vec![vec![tool("query")], vec![tool("scan")]];
        let (exposed, routes) = build_routes(&names, &lists);
        let exposed_names: HashSet<&str> = exposed.iter().map(|t| t.name.as_str()).collect();
        assert!(exposed_names.contains("query"));
        assert!(exposed_names.contains("scan"));
        assert_eq!(routes["query"], (0, "query".to_string()));
        assert_eq!(routes["scan"], (1, "scan".to_string()));
    }

    #[test]
    fn colliding_names_are_prefixed_and_routed() {
        let names = vec!["pg".to_string(), "duck".to_string()];
        let lists = vec![vec![tool("execute_sql")], vec![tool("execute_sql")]];
        let (exposed, routes) = build_routes(&names, &lists);
        let exposed_names: HashSet<&str> = exposed.iter().map(|t| t.name.as_str()).collect();
        assert!(exposed_names.contains("pg__execute_sql"));
        assert!(exposed_names.contains("duck__execute_sql"));
        assert_eq!(routes["pg__execute_sql"], (0, "execute_sql".to_string()));
        assert_eq!(routes["duck__execute_sql"], (1, "execute_sql".to_string()));
    }

    type Routes = HashMap<String, (usize, String)>;

    /// A pg+ch setup: each server's SQL tool is dialect-tagged and routable.
    fn pgch_fixture() -> (Vec<Tool>, Routes, Vec<Dialect>) {
        let names = vec!["pg".to_string(), "ch".to_string()];
        let lists = vec![vec![tool("execute_sql")], vec![tool("run_select_query")]];
        let (exposed, routes) = build_routes(&names, &lists);
        let dialects = vec![Dialect::Postgres, Dialect::ClickHouse];
        (exposed, routes, dialects)
    }

    #[test]
    fn dialect_of_maps_tool_to_its_server() {
        let (_, routes, dialects) = pgch_fixture();
        assert_eq!(
            dialect_of(&routes, &dialects, "execute_sql"),
            Dialect::Postgres
        );
        assert_eq!(
            dialect_of(&routes, &dialects, "run_select_query"),
            Dialect::ClickHouse
        );
        // Unknown tool → default Postgres.
        assert_eq!(dialect_of(&routes, &dialects, "nope"), Dialect::Postgres);
    }

    #[test]
    fn select_sql_tool_prefers_matching_dialect() {
        let (exposed, routes, dialects) = pgch_fixture();
        let (pg, _) = select_sql_tool(&exposed, &routes, &dialects, Dialect::Postgres).unwrap();
        assert_eq!(pg, "execute_sql");
        let (ch, _) = select_sql_tool(&exposed, &routes, &dialects, Dialect::ClickHouse).unwrap();
        assert_eq!(ch, "run_select_query");
    }

    #[test]
    fn select_sql_tool_falls_back_when_no_dialect_match() {
        // Single Postgres server, but a spec asks for ClickHouse: fall back to
        // the only SQL tool rather than failing.
        let names = vec!["pg".to_string()];
        let lists = vec![vec![tool("execute_sql")]];
        let (exposed, routes) = build_routes(&names, &lists);
        let dialects = vec![Dialect::Postgres];
        let (name, _) = select_sql_tool(&exposed, &routes, &dialects, Dialect::ClickHouse).unwrap();
        assert_eq!(name, "execute_sql");
    }
}
