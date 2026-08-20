//! Configuration, loaded from a JSON file (default `config.json`).

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub ollama: OllamaConfig,
    /// A single MCP server (backward-compatible form).
    #[serde(default)]
    pub mcp_server: Option<McpServerConfig>,
    /// Multiple MCP servers. Takes precedence over `mcp_server` when set.
    #[serde(default)]
    pub mcp_servers: Option<Vec<McpServerConfig>>,
    #[serde(default)]
    pub guard: GuardConfig,
    /// Optional override for the agent's system prompt.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Cap on tool-call rounds per user turn, to stop runaway loops.
    #[serde(default = "default_max_steps")]
    pub max_steps: usize,
    /// Directory of `*.spec.md` semantic-layer / verified-query files.
    #[serde(default = "default_specs_dir")]
    pub specs_dir: String,
    /// Optional path to a JSONL audit log of tool activity. Disabled if unset.
    #[serde(default)]
    pub audit_log: Option<String>,
    /// Warn when figures in the model's answer aren't found in the data's
    /// computed aggregates (catches hallucinated numbers). On by default.
    #[serde(default = "default_true")]
    pub verify_answers: bool,
    /// Optional CDC replication plan (Postgres → ClickHouse). Used by the `cdc`
    /// subcommand to generate setup DDL. Disabled if unset.
    #[serde(default)]
    pub cdc: Option<crate::cdc::CdcConfig>,
}

#[derive(Debug, Deserialize)]
pub struct OllamaConfig {
    #[serde(default = "default_ollama_url")]
    pub base_url: String,
    #[serde(default = "default_model")]
    pub model: String,
    /// Optional Ollama generation options (temperature, num_ctx, …). Passed
    /// through to `/api/chat`. Defaults to `{"temperature": 0}` when omitted.
    #[serde(default)]
    pub options: Option<serde_json::Value>,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: default_ollama_url(),
            model: default_model(),
            options: None,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct McpServerConfig {
    /// Optional short name used to namespace this server's tools on collision.
    #[serde(default)]
    pub name: Option<String>,
    /// Executable to launch (e.g. "npx", "uvx", or an absolute path).
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment for the server process (e.g. connection string).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// SQL dialect this server speaks: "postgres" (default) or "clickhouse".
    /// Lets the guard classify each server's SQL correctly in a mixed setup and
    /// lets `verify`/`materialize` route a backend-tagged spec to the right one.
    #[serde(default)]
    pub dialect: Option<String>,
}

impl McpServerConfig {
    pub fn env_pairs(&self) -> Vec<(String, String)> {
        self.env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// The server's SQL dialect. Explicit `dialect` wins; otherwise infer from
    /// the name/command (anything mentioning "clickhouse"/"ch" → ClickHouse) so
    /// a typical two-server config needs no extra annotation.
    pub fn dialect(&self) -> crate::guard::Dialect {
        if let Some(d) = &self.dialect {
            return crate::guard::Dialect::parse(d);
        }
        let hay = format!(
            "{} {} {}",
            self.name.clone().unwrap_or_default(),
            self.command,
            self.args.join(" ")
        )
        .to_lowercase();
        if hay.contains("clickhouse") || hay.split_whitespace().any(|w| w == "ch") {
            crate::guard::Dialect::ClickHouse
        } else {
            crate::guard::Dialect::Postgres
        }
    }

    /// A display name: the configured `name`, else derived from the command.
    pub fn display_name(&self, index: usize) -> String {
        if let Some(n) = &self.name {
            return n.clone();
        }
        // Derive from the executable's file stem, falling back to an index.
        std::path::Path::new(&self.command)
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("server{index}"))
    }
}

#[derive(Debug, Deserialize)]
pub struct GuardConfig {
    #[serde(default = "default_true")]
    pub allow_writes: bool,
    #[serde(default)]
    pub allow_ddl: bool,
    #[serde(default)]
    pub confirm_reads: bool,
    /// SQL dialect for classification: "postgres" (default) or "clickhouse".
    #[serde(default = "default_dialect")]
    pub dialect: String,
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            allow_writes: true,
            allow_ddl: false,
            confirm_reads: false,
            dialect: default_dialect(),
        }
    }
}

impl GuardConfig {
    pub fn to_policy(&self) -> crate::guard::GuardPolicy {
        crate::guard::GuardPolicy {
            allow_writes: self.allow_writes,
            allow_ddl: self.allow_ddl,
            confirm_reads: self.confirm_reads,
            dialect: crate::guard::Dialect::parse(&self.dialect),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let cfg: Config = serde_json::from_str(&text)
            .with_context(|| format!("parsing config file {}", path.display()))?;
        Ok(cfg)
    }

    /// Resolve the list of MCP servers to connect: `mcp_servers` if present,
    /// otherwise the single `mcp_server`. Errors if neither is configured.
    pub fn servers(&self) -> Result<Vec<McpServerConfig>> {
        if let Some(list) = &self.mcp_servers {
            if list.is_empty() {
                anyhow::bail!("`mcp_servers` is empty; configure at least one server");
            }
            return Ok(list.clone());
        }
        if let Some(one) = &self.mcp_server {
            return Ok(vec![one.clone()]);
        }
        anyhow::bail!("no MCP server configured: set `mcp_server` or `mcp_servers`")
    }
}

fn default_ollama_url() -> String {
    "http://localhost:11434".into()
}
fn default_model() -> String {
    "qwen2.5-coder".into()
}
fn default_max_steps() -> usize {
    12
}
fn default_specs_dir() -> String {
    "specs".into()
}
fn default_dialect() -> String {
    "postgres".into()
}
fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::Dialect;

    fn server(
        name: Option<&str>,
        command: &str,
        args: &[&str],
        dialect: Option<&str>,
    ) -> McpServerConfig {
        McpServerConfig {
            name: name.map(String::from),
            command: command.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: BTreeMap::new(),
            dialect: dialect.map(String::from),
        }
    }

    #[test]
    fn explicit_dialect_wins() {
        let s = server(Some("warehouse"), "uvx", &["something"], Some("clickhouse"));
        assert_eq!(s.dialect(), Dialect::ClickHouse);
    }

    #[test]
    fn dialect_inferred_from_name_or_command() {
        assert_eq!(
            server(Some("ch"), "uvx", &["mcp-clickhouse"], None).dialect(),
            Dialect::ClickHouse
        );
        assert_eq!(
            server(None, "uvx", &["mcp-clickhouse"], None).dialect(),
            Dialect::ClickHouse
        );
        assert_eq!(
            server(Some("pg"), "uvx", &["postgres-mcp"], None).dialect(),
            Dialect::Postgres
        );
    }

    #[test]
    fn dialect_defaults_to_postgres() {
        assert_eq!(
            server(None, "npx", &["server-postgres"], None).dialect(),
            Dialect::Postgres
        );
    }

    #[test]
    fn parses_two_server_config_with_dialects() {
        let json = r#"{
            "mcp_servers": [
                { "name": "pg", "command": "uvx", "args": ["postgres-mcp"] },
                { "name": "ch", "command": "uvx", "args": ["mcp-clickhouse"], "dialect": "clickhouse" }
            ]
        }"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        let servers = cfg.servers().unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].dialect(), Dialect::Postgres);
        assert_eq!(servers[1].dialect(), Dialect::ClickHouse);
    }
}
