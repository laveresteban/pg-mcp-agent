//! pg-mcp-agent: a local-LLM agent that drives a Postgres MCP server with
//! guarded writes and a markdown semantic layer.
//!
//! Usage:
//!   pg-mcp-agent [config.json]           interactive REPL (default config.json)
//!   pg-mcp-agent verify [config.json]    run the spec files against the database

use anyhow::Result;
use pg_mcp_agent::agent::{Agent, Console};
use pg_mcp_agent::config::Config;
use pg_mcp_agent::ollama::OllamaClient;
use pg_mcp_agent::router::McpRouter;
use pg_mcp_agent::semantics::SemanticLayer;
use std::path::{Path, PathBuf};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = match Cli::parse(std::env::args().skip(1)) {
        Ok(cli) => cli,
        Err(msg) => {
            eprintln!("{msg}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if cli.help {
        println!("{USAGE}");
        return Ok(());
    }

    let mut cfg = Config::load(&cli.config_path)?;
    // CLI overrides win over the config file.
    if cli.audit_log.is_some() {
        cfg.audit_log = cli.audit_log.clone();
    }
    if cli.no_verify {
        cfg.verify_answers = false;
    }
    let specs = SemanticLayer::load_dir(Path::new(&cfg.specs_dir))?;

    match cli.mode {
        Mode::Verify => run_verify(cfg, specs, cli.json).await,
        Mode::Parity => run_parity(cfg, specs).await,
        Mode::InitSpecs => run_init_specs(cfg).await,
        Mode::Materialize => run_materialize(specs),
        Mode::Cdc(CdcAction::Plan(via)) => run_cdc_plan(cfg, via),
        Mode::Cdc(CdcAction::Inspect) => run_cdc_inspect(cfg).await,
        Mode::Repl => run_repl(cfg, specs, cli.auto_yes, cli.prompt).await,
    }
}

/// Print CREATE/REFRESH MATERIALIZED VIEW DDL generated from the specs. This
/// only prints SQL (never applies it), so it is always safe.
fn run_materialize(specs: SemanticLayer) -> Result<()> {
    if specs.specs.is_empty() {
        println!("No specs found. Nothing to materialize.");
        return Ok(());
    }
    print!("{}", pg_mcp_agent::pipeline::layer_to_ddl(&specs));
    Ok(())
}

/// Generate the Postgres → ClickHouse CDC setup DDL from the config's `cdc`
/// section. Print-only (never applies), like `materialize`. `via` selects the
/// capture path: the direct MaterializedPostgreSQL engine, or the Debezium +
/// Kafka fan-out.
fn run_cdc_plan(cfg: Config, via: CdcVia) -> Result<()> {
    let Some(plan) = &cfg.cdc else {
        println!(
            "No `cdc` section in the config. Add one to generate replication DDL, e.g.:\n\
             \n  \"cdc\": {{\n    \"target_database\": \"analytics\",\n    \"source\": {{ \"host\": \"localhost\", \"database\": \"shop\", \"user\": \"repl\", \"password_env\": \"PGPASSWORD\" }},\n    \"tables\": [\"orders\", \"order_items\"]\n  }}"
        );
        return Ok(());
    };
    match via {
        CdcVia::Materialized => {
            print!("{}", pg_mcp_agent::cdc::materialized_postgresql_ddl(plan));
            println!();
        }
        CdcVia::Kafka => {
            let Some(fan) = &plan.fanout else {
                println!(
                    "`cdc plan --via kafka` needs a `cdc.fanout` block, e.g.:\n\
                     \n  \"fanout\": {{\n    \"connector_name\": \"shop-connector\",\n    \"topic_prefix\": \"pgch\",\n    \"bootstrap_servers\": \"kafka:9092\",\n    \"consumer_group\": \"clickhouse_analytics\"\n  }}"
                );
                return Ok(());
            };
            print!("{}", pg_mcp_agent::cdc::fanout_plan(plan, fan));
            println!();
        }
    }
    Ok(())
}

/// Inspect Postgres replication health through the connected MCP server: check
/// `wal_level` and enumerate replication slots, flagging the usual problems.
async fn run_cdc_inspect(cfg: Config) -> Result<()> {
    use pg_mcp_agent::analytics::Table;
    use pg_mcp_agent::cdc::{
        analyze_slots, check_wal_level, render_report, REPLICATION_SLOTS_SQL, WAL_LEVEL_SQL,
    };
    use pg_mcp_agent::guard::Dialect;
    use pg_mcp_agent::semantics::fill_sql_arg;

    let mut router = McpRouter::connect(&cfg.servers()?).await?;
    // Prefer the Postgres server: replication slots live on the source DB.
    let (tool, schema) = router
        .sql_tool_for_dialect(Dialect::Postgres)
        .ok_or_else(|| anyhow::anyhow!("no SQL tool available to inspect replication"))?;

    let wal_out = router
        .call_tool(&tool, fill_sql_arg(&schema, WAL_LEVEL_SQL))
        .await?;
    let wal_issue = Table::from_tool_output(&wal_out)
        .as_ref()
        .and_then(check_wal_level);

    let slots_out = router
        .call_tool(&tool, fill_sql_arg(&schema, REPLICATION_SLOTS_SQL))
        .await?;
    let report = match Table::from_tool_output(&slots_out) {
        Some(t) => analyze_slots(&t, 64.0 * 1_048_576.0),
        None => {
            router.shutdown().await;
            anyhow::bail!("could not parse replication-slot output: {slots_out}");
        }
    };

    println!("{}", render_report(&wal_issue, &report));
    let mut unhealthy = !report.is_healthy() || wal_issue.is_some();

    // When the fan-out path is configured, also check ClickHouse Kafka consumers.
    if cfg.cdc.as_ref().and_then(|c| c.fanout.as_ref()).is_some() {
        use pg_mcp_agent::cdc::{analyze_consumers, render_consumer_report, KAFKA_CONSUMERS_SQL};
        match router.sql_tool_for_dialect(Dialect::ClickHouse) {
            Some((ch_tool, ch_schema)) => {
                let out = router
                    .call_tool(&ch_tool, fill_sql_arg(&ch_schema, KAFKA_CONSUMERS_SQL))
                    .await?;
                if let Some(t) = Table::from_tool_output(&out) {
                    let creport = analyze_consumers(&t, 100_000.0);
                    println!("\n{}", render_consumer_report(&creport));
                    unhealthy = unhealthy || !creport.is_healthy();
                } else {
                    println!("\n(could not parse Kafka-consumer output: {out})");
                    unhealthy = true;
                }
            }
            None => println!("\n(no ClickHouse server connected; skipping Kafka-consumer check)"),
        }
    }

    router.shutdown().await;
    if unhealthy {
        std::process::exit(1);
    }
    Ok(())
}

const USAGE: &str = "\
Usage:
  pg-mcp-agent [options] [config.json]      interactive REPL (default config.json)
  pg-mcp-agent verify [config.json]         run specs against the database
  pg-mcp-agent parity [config.json]         cross-check `Parity:` metrics agree across engines
  pg-mcp-agent init-specs [config.json]     generate a starter spec from the schema
  pg-mcp-agent materialize [config.json]    print CREATE MATERIALIZED VIEW DDL from specs
  pg-mcp-agent cdc plan [--via materialized|kafka] [config.json]
                                            print Postgres→ClickHouse CDC setup DDL
                                            (direct MaterializedPostgreSQL, or Debezium/Kafka fan-out)
  pg-mcp-agent cdc inspect [config.json]    check replication health (slots, lag, Kafka consumers)

Options:
  -y, --yes             auto-approve guarded writes (non-interactive)
  -p, --prompt <text>   run a single request and exit (implies non-interactive)
      --audit-log <path> append a JSONL audit log of tool calls
      --no-verify       disable the answer verifier
      --json            emit machine-readable JSON (verify; for the triage agent)
  -h, --help            show this help";

#[derive(Debug, PartialEq)]
enum Mode {
    Repl,
    Verify,
    Parity,
    InitSpecs,
    Materialize,
    Cdc(CdcAction),
}

#[derive(Debug, PartialEq)]
enum CdcAction {
    /// Generate the replication setup DDL (print-only), via a chosen path.
    Plan(CdcVia),
    /// Inspect live replication health.
    Inspect,
}

/// Which capture path `cdc plan` generates.
#[derive(Debug, PartialEq, Clone, Copy)]
enum CdcVia {
    /// ClickHouse `MaterializedPostgreSQL` engine (direct). The default.
    Materialized,
    /// Debezium → Kafka → ClickHouse fan-out.
    Kafka,
}

struct Cli {
    mode: Mode,
    config_path: PathBuf,
    auto_yes: bool,
    prompt: Option<String>,
    /// CLI override for `audit_log`.
    audit_log: Option<String>,
    /// CLI override: disable the answer verifier.
    no_verify: bool,
    /// Emit machine-readable JSON (currently for `verify`, consumed by the
    /// spec-triage agent).
    json: bool,
    /// Set when `--help` was requested (handled by the caller).
    help: bool,
}

impl Cli {
    fn parse(args: impl Iterator<Item = String>) -> std::result::Result<Cli, String> {
        let mut mode = Mode::Repl;
        let mut config: Option<String> = None;
        let mut auto_yes = false;
        let mut prompt = None;
        let mut audit_log = None;
        let mut no_verify = false;
        let mut json = false;

        let mut it = args.peekable();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "verify" => mode = Mode::Verify,
                "parity" => mode = Mode::Parity,
                "init-specs" => mode = Mode::InitSpecs,
                "materialize" => mode = Mode::Materialize,
                "cdc" => {
                    // Optional sub-action: `cdc plan [--via materialized|kafka]` /
                    // `cdc inspect` (default inspect).
                    let action = match it.peek().map(String::as_str) {
                        Some("plan") => {
                            it.next();
                            let mut via = CdcVia::Materialized;
                            if it.peek().map(String::as_str) == Some("--via") {
                                it.next();
                                via = match it.next().as_deref() {
                                    Some("kafka") => CdcVia::Kafka,
                                    Some("materialized") | Some("materializedpostgresql") => {
                                        CdcVia::Materialized
                                    }
                                    other => {
                                        return Err(format!(
                                            "unknown --via value `{}` (expected materialized | kafka)",
                                            other.unwrap_or("")
                                        ));
                                    }
                                };
                            }
                            CdcAction::Plan(via)
                        }
                        Some("inspect") => {
                            it.next();
                            CdcAction::Inspect
                        }
                        _ => CdcAction::Inspect,
                    };
                    mode = Mode::Cdc(action);
                }
                "-y" | "--yes" => auto_yes = true,
                "--no-verify" => no_verify = true,
                "--json" => json = true,
                "-h" | "--help" => {
                    return Ok(Cli {
                        mode: Mode::Repl,
                        config_path: "config.json".into(),
                        auto_yes: false,
                        prompt: None,
                        audit_log: None,
                        no_verify: false,
                        json: false,
                        help: true,
                    });
                }
                "-p" | "--prompt" => {
                    let text = it.next().ok_or("--prompt needs a value")?;
                    prompt = Some(text);
                }
                "--audit-log" => {
                    let path = it.next().ok_or("--audit-log needs a path")?;
                    audit_log = Some(path);
                }
                other if other.starts_with('-') => {
                    return Err(format!("unknown option `{other}`"));
                }
                other => {
                    if config.replace(other.to_string()).is_some() {
                        return Err("more than one config path given".into());
                    }
                }
            }
        }

        // A one-shot prompt is inherently non-interactive.
        if prompt.is_some() {
            auto_yes = true;
        }
        Ok(Cli {
            mode,
            config_path: config.unwrap_or_else(|| "config.json".to_string()).into(),
            auto_yes,
            prompt,
            audit_log,
            no_verify,
            json,
            help: false,
        })
    }
}

async fn run_verify(cfg: Config, specs: SemanticLayer, json: bool) -> Result<()> {
    if specs.specs.is_empty() {
        if json {
            println!("{{\"summary\":{{\"total\":0,\"passed\":0,\"failed\":0}},\"results\":[]}}");
        } else {
            println!("No specs found in `{}`. Nothing to verify.", cfg.specs_dir);
        }
        return Ok(());
    }
    let mut router = McpRouter::connect(&cfg.servers()?).await?;
    let report = specs.verify(&mut router).await?;
    router.shutdown().await;

    // JSON goes to stdout (for the spec-triage agent); human output otherwise.
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_human());
    }
    if report.failures() > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Cross-engine parity: run every `Parity:`-tagged spec on its backend and
/// assert the members of each group compute to the same number. Exits non-zero
/// on any mismatch so it gates in CI, like `verify`.
async fn run_parity(cfg: Config, specs: SemanticLayer) -> Result<()> {
    use pg_mcp_agent::parity::DEFAULT_TOLERANCE;

    let n = specs
        .specs
        .iter()
        .filter(|s| s.parity_key.is_some())
        .count();
    if n == 0 {
        println!(
            "No parity groups in `{}`. Add a `Parity: <key>` line to two specs on \
             different backends (e.g. one Postgres, one ClickHouse) to cross-check them.",
            cfg.specs_dir
        );
        return Ok(());
    }
    let mut router = McpRouter::connect(&cfg.servers()?).await?;
    let report = specs.verify_parity(&mut router, DEFAULT_TOLERANCE).await?;
    router.shutdown().await;

    print!("{}", report.to_human());
    if report.failures() > 0 {
        std::process::exit(1);
    }
    Ok(())
}

async fn run_init_specs(cfg: Config) -> Result<()> {
    use pg_mcp_agent::analytics::Table;
    use pg_mcp_agent::catalog::Catalog;
    use pg_mcp_agent::semantics::{fill_sql_arg, pick_sql_tool};
    use pg_mcp_agent::specgen::{generate_spec, generate_spec_from_catalog, INTROSPECTION_SQL};
    use serde_json::json;

    let mut router = McpRouter::connect(&cfg.servers()?).await?;
    let tools = router.tools().to_vec();

    // Prefer a data catalog when one is connected: it carries real business
    // metadata, so the generated spec is grounded rather than guessed.
    let has_catalog = tools.iter().any(|t| t.name == "catalog_dump");

    let spec = if has_catalog {
        println!("Using the connected data catalog for grounding.");
        let output = router.call_tool("catalog_dump", json!({})).await?;
        let catalog: Catalog = serde_json::from_str(&output)
            .map_err(|e| anyhow::anyhow!("parsing catalog_dump output: {e}"))?;
        generate_spec_from_catalog(&catalog)
    } else {
        let (tool_name, schema) = pick_sql_tool(&tools)
            .map(|t| (t.name.clone(), t.input_schema.clone()))
            .ok_or_else(|| anyhow::anyhow!("no SQL tool and no catalog server available"))?;
        let output = router
            .call_tool(&tool_name, fill_sql_arg(&schema, INTROSPECTION_SQL))
            .await?;
        let table = Table::from_tool_output(&output)
            .ok_or_else(|| anyhow::anyhow!("could not parse introspection result as rows"))?;
        generate_spec(&table)
    };
    router.shutdown().await;

    std::fs::create_dir_all(&cfg.specs_dir)?;
    let path = Path::new(&cfg.specs_dir).join("generated.spec.md");
    std::fs::write(&path, spec)?;
    println!("Wrote {}", path.display());
    println!("Review the glossary and add verified queries, then run `pg-mcp-agent verify`.");
    Ok(())
}

async fn run_repl(
    cfg: Config,
    specs: SemanticLayer,
    auto_yes: bool,
    one_shot: Option<String>,
) -> Result<()> {
    let servers = cfg.servers()?;
    println!("pg-mcp-agent");
    println!("  model:  {} @ {}", cfg.ollama.model, cfg.ollama.base_url);
    for (i, s) in servers.iter().enumerate() {
        println!(
            "  server: {} ({} {})",
            s.display_name(i),
            s.command,
            s.args.join(" ")
        );
    }
    println!(
        "  policy: writes={}, ddl={}, confirm_reads={}{}",
        cfg.guard.allow_writes,
        cfg.guard.allow_ddl,
        cfg.guard.confirm_reads,
        if auto_yes {
            " [--yes: writes auto-approved]"
        } else {
            ""
        }
    );

    let router = McpRouter::connect(&servers).await?;
    let ollama = OllamaClient::new(
        cfg.ollama.base_url.clone(),
        cfg.ollama.model.clone(),
        cfg.ollama.options.clone(),
    );

    let audit = pg_mcp_agent::audit::AuditLogger::new(cfg.audit_log.clone());
    if audit.is_enabled() {
        println!("  audit: {}", cfg.audit_log.as_deref().unwrap_or(""));
    }

    let options = pg_mcp_agent::agent::AgentOptions {
        max_steps: cfg.max_steps,
        auto_yes,
        audit,
        verify_answers: cfg.verify_answers,
    };
    let mut agent = Agent::new(
        ollama,
        router,
        cfg.guard.to_policy(),
        cfg.system_prompt.clone(),
        &specs,
        options,
    )
    .await?;

    let mut console = Console::new();

    // One-shot: run a single request and exit (for scripting / CI).
    if let Some(input) = one_shot {
        let result = agent.handle_user(input, &mut console).await;
        agent.shutdown().await;
        return result;
    }

    println!("\nType a request, or `exit` / Ctrl-C to quit.\n");
    loop {
        let Some(line) = console.read_line("› ").await? else {
            break; // EOF
        };
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        if matches!(input, "exit" | "quit") {
            break;
        }

        if let Err(e) = agent.handle_user(input.to_string(), &mut console).await {
            eprintln!("error: {e:#}");
        }
    }

    agent.shutdown().await;
    println!("bye");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::parse(args.iter().map(|s| s.to_string())).unwrap()
    }

    #[test]
    fn defaults_to_repl_with_default_config() {
        let cli = parse(&[]);
        assert_eq!(cli.mode, Mode::Repl);
        assert_eq!(cli.config_path, PathBuf::from("config.json"));
        assert!(!cli.auto_yes);
        assert!(cli.prompt.is_none());
    }

    #[test]
    fn verify_subcommand_with_config_path() {
        let cli = parse(&["verify", "my.json"]);
        assert_eq!(cli.mode, Mode::Verify);
        assert_eq!(cli.config_path, PathBuf::from("my.json"));
    }

    #[test]
    fn init_specs_subcommand() {
        assert_eq!(parse(&["init-specs"]).mode, Mode::InitSpecs);
    }

    #[test]
    fn materialize_subcommand() {
        assert_eq!(parse(&["materialize"]).mode, Mode::Materialize);
    }

    #[test]
    fn cdc_defaults_to_inspect() {
        assert_eq!(parse(&["cdc"]).mode, Mode::Cdc(CdcAction::Inspect));
    }

    #[test]
    fn cdc_plan_defaults_to_materialized() {
        assert_eq!(
            parse(&["cdc", "plan"]).mode,
            Mode::Cdc(CdcAction::Plan(CdcVia::Materialized))
        );
        assert_eq!(
            parse(&["cdc", "inspect"]).mode,
            Mode::Cdc(CdcAction::Inspect)
        );
    }

    #[test]
    fn cdc_plan_via_kafka() {
        assert_eq!(
            parse(&["cdc", "plan", "--via", "kafka"]).mode,
            Mode::Cdc(CdcAction::Plan(CdcVia::Kafka))
        );
    }

    #[test]
    fn cdc_plan_via_unknown_errors() {
        assert!(Cli::parse(
            ["cdc", "plan", "--via", "sqs"]
                .iter()
                .map(|s| s.to_string())
        )
        .is_err());
    }

    #[test]
    fn cdc_plan_with_config_path() {
        let cli = parse(&["cdc", "plan", "my.json"]);
        assert_eq!(cli.mode, Mode::Cdc(CdcAction::Plan(CdcVia::Materialized)));
        assert_eq!(cli.config_path, PathBuf::from("my.json"));
    }

    #[test]
    fn cdc_plan_via_kafka_with_config_path() {
        let cli = parse(&["cdc", "plan", "--via", "kafka", "my.json"]);
        assert_eq!(cli.mode, Mode::Cdc(CdcAction::Plan(CdcVia::Kafka)));
        assert_eq!(cli.config_path, PathBuf::from("my.json"));
    }

    #[test]
    fn prompt_implies_auto_yes() {
        let cli = parse(&["--prompt", "hello"]);
        assert_eq!(cli.prompt.as_deref(), Some("hello"));
        assert!(cli.auto_yes, "one-shot prompt should be non-interactive");
    }

    #[test]
    fn yes_and_no_verify_and_audit_flags() {
        let cli = parse(&["--yes", "--no-verify", "--audit-log", "a.jsonl"]);
        assert!(cli.auto_yes);
        assert!(cli.no_verify);
        assert_eq!(cli.audit_log.as_deref(), Some("a.jsonl"));
    }

    #[test]
    fn verify_json_flag() {
        let cli = parse(&["verify", "--json", "config.pgch.mock.json"]);
        assert_eq!(cli.mode, Mode::Verify);
        assert!(cli.json);
        assert_eq!(cli.config_path, PathBuf::from("config.pgch.mock.json"));
    }

    #[test]
    fn help_sets_help_flag() {
        assert!(parse(&["--help"]).help);
    }

    #[test]
    fn unknown_option_errors() {
        assert!(Cli::parse(["--bogus".to_string()].into_iter()).is_err());
    }

    #[test]
    fn missing_prompt_value_errors() {
        assert!(Cli::parse(["--prompt".to_string()].into_iter()).is_err());
    }

    #[test]
    fn two_config_paths_error() {
        assert!(Cli::parse(["a".to_string(), "b".to_string()].into_iter()).is_err());
    }
}
