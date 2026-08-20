//! CDC control plane: propose and validate Postgres → ClickHouse replication.
//!
//! Design principle from the roadmap: **one well-operated capture path, many
//! materialization paths.** The agent is the *control plane* — it inspects,
//! proposes, and validates — while the capture itself runs in the database
//! layer. The lowest-effort near-real-time path from Postgres to ClickHouse is
//! ClickHouse's `MaterializedPostgreSQL` engine, which consumes the Postgres WAL
//! over logical replication and keeps selected tables in sync.
//!
//! This module is deterministic and print-only, mirroring `pipeline.rs`:
//!   * [`materialized_postgresql_ddl`] turns a [`CdcConfig`] into the ClickHouse
//!     `CREATE DATABASE … ENGINE = MaterializedPostgreSQL(…)` setup DDL plus the
//!     Postgres-side prerequisites, as reviewable SQL. It never applies anything.
//!   * [`analyze_slots`] / [`check_wal_level`] parse the result of the health
//!     queries below into a [`ReplicationReport`] that flags the usual failure
//!     modes (WAL level, dead slots, replication lag).

use crate::analytics::Table;
use serde::Deserialize;
use serde_json::{json, Value};

/// How to reach the source Postgres from ClickHouse. The password is never
/// stored here; it is read from `password_env` at generation time (and even then
/// only a placeholder is emitted unless the env var is set).
#[derive(Debug, Clone, Deserialize)]
pub struct CdcSource {
    pub host: String,
    #[serde(default = "default_pg_port")]
    pub port: u16,
    pub database: String,
    pub user: String,
    /// Name of the environment variable holding the password (never the secret).
    #[serde(default)]
    pub password_env: Option<String>,
}

/// Extra settings for the Debezium → Kafka → ClickHouse **fan-out** path, where
/// Kafka is the durable log between capture and one-or-more consumers. Optional:
/// present it only when you want `cdc plan --via kafka`.
#[derive(Debug, Clone, Deserialize)]
pub struct CdcFanout {
    /// Debezium connector name registered with Kafka Connect.
    pub connector_name: String,
    /// Debezium `topic.prefix`; topics are `<prefix>.<schema>.<table>`.
    pub topic_prefix: String,
    /// Kafka bootstrap servers, e.g. "kafka:9092".
    pub bootstrap_servers: String,
    /// The ClickHouse Kafka-engine consumer group.
    pub consumer_group: String,
    /// Postgres schema the tables live in (Debezium topic component).
    #[serde(default = "default_schema")]
    pub schema: String,
}

/// A CDC replication plan: replicate `tables` from a source Postgres into a
/// ClickHouse database. The direct path uses ClickHouse's `MaterializedPostgreSQL`
/// engine; add a [`CdcFanout`] to instead route through Debezium + Kafka.
#[derive(Debug, Clone, Deserialize)]
pub struct CdcConfig {
    /// ClickHouse database to create for the replicated tables.
    pub target_database: String,
    pub source: CdcSource,
    /// Tables to replicate. Empty means "all tables in the source database".
    #[serde(default)]
    pub tables: Vec<String>,
    /// Optional Debezium/Kafka fan-out settings (for `cdc plan --via kafka`).
    #[serde(default)]
    pub fanout: Option<CdcFanout>,
}

fn default_pg_port() -> u16 {
    5432
}

fn default_schema() -> String {
    "public".to_string()
}

/// Resolve the source password: the value of `password_env` if that env var is
/// set, otherwise a `{PASSWORD}` placeholder so secrets never leak into output.
fn resolve_password(src: &CdcSource) -> String {
    src.password_env
        .as_ref()
        .and_then(|k| std::env::var(k).ok())
        .unwrap_or_else(|| "{PASSWORD}".to_string())
}

/// SQL to read the current WAL level (must be `logical` for CDC to work).
pub const WAL_LEVEL_SQL: &str = "SELECT current_setting('wal_level') AS wal_level;";

/// SQL to inspect every logical replication slot and its lag in bytes.
pub const REPLICATION_SLOTS_SQL: &str = "\
SELECT slot_name,
       plugin,
       slot_type,
       active,
       pg_wal_lsn_diff(pg_current_wal_lsn(), confirmed_flush_lsn) AS lag_bytes
FROM pg_replication_slots
ORDER BY slot_name;";

/// Generate the reviewable DDL to stand up the replication. Two parts:
///   1. Postgres prerequisites (as comments + one publication statement).
///   2. The ClickHouse `CREATE DATABASE … ENGINE = MaterializedPostgreSQL(…)`.
///
/// The password is emitted as a `{PASSWORD}` placeholder unless `password_env`
/// names a set environment variable, so secrets never land in generated files by
/// accident.
pub fn materialized_postgresql_ddl(cfg: &CdcConfig) -> String {
    let src = &cfg.source;
    let password = resolve_password(src);

    let tables_list = if cfg.tables.is_empty() {
        String::new()
    } else {
        format!(
            "\nSETTINGS materialized_postgresql_tables_list = '{}'",
            cfg.tables.join(",")
        )
    };

    let publication_tables = if cfg.tables.is_empty() {
        "ALL TABLES".to_string()
    } else {
        format!("TABLE {}", cfg.tables.join(", "))
    };
    let pub_name = format!("{}_pub", cfg.target_database);

    format!(
        "\
-- CDC plan: Postgres `{db}` → ClickHouse `{target}` via MaterializedPostgreSQL.
-- Review before applying. This is print-only; the agent never runs it.

-- 1) Postgres prerequisites (run on the SOURCE, as a superuser):
--    * postgresql.conf: wal_level = logical  (needs a restart)
--    * the replication user needs REPLICATION + SELECT on the tables.
--    * each replicated table needs a primary key or REPLICA IDENTITY FULL.
CREATE PUBLICATION {pub_name} FOR {publication_tables};

-- 2) ClickHouse: create the replicated database (run on the TARGET).
--    ClickHouse consumes the Postgres WAL and keeps these tables in sync.
CREATE DATABASE {target}
ENGINE = MaterializedPostgreSQL('{host}:{port}', '{db}', '{user}', '{password}'){tables_list};

-- 3) Build analytical rollups on top with `pg-mcp-agent materialize`
--    (tag those specs `Backend: clickhouse`).",
        db = src.database,
        target = cfg.target_database,
        pub_name = pub_name,
        publication_tables = publication_tables,
        host = src.host,
        port = src.port,
        user = src.user,
        password = password,
        tables_list = tables_list,
    )
}

// --- Fan-out path: Debezium → Kafka → ClickHouse -------------------------------

/// The Debezium topic for one table: `<topic_prefix>.<schema>.<table>`.
pub fn topic_for(fan: &CdcFanout, table: &str) -> String {
    format!("{}.{}.{}", fan.topic_prefix, fan.schema, table)
}

/// The Debezium Postgres source-connector config, as the JSON you POST to Kafka
/// Connect's `/connectors` endpoint. The password follows the same env-var /
/// `{PASSWORD}`-placeholder rule as the direct path.
pub fn debezium_connector_config(cfg: &CdcConfig, fan: &CdcFanout) -> String {
    let src = &cfg.source;
    let mut config = serde_json::Map::new();
    config.insert(
        "connector.class".into(),
        json!("io.debezium.connector.postgresql.PostgresConnector"),
    );
    config.insert("plugin.name".into(), json!("pgoutput"));
    config.insert("database.hostname".into(), json!(src.host));
    config.insert("database.port".into(), json!(src.port.to_string()));
    config.insert("database.user".into(), json!(src.user));
    config.insert("database.password".into(), json!(resolve_password(src)));
    config.insert("database.dbname".into(), json!(src.database));
    config.insert("topic.prefix".into(), json!(fan.topic_prefix));
    config.insert(
        "slot.name".into(),
        json!(format!("{}_debezium", cfg.target_database)),
    );
    config.insert(
        "publication.name".into(),
        json!(format!("{}_pub", cfg.target_database)),
    );
    if !cfg.tables.is_empty() {
        let list = cfg
            .tables
            .iter()
            .map(|t| format!("{}.{}", fan.schema, t))
            .collect::<Vec<_>>()
            .join(",");
        config.insert("table.include.list".into(), json!(list));
    }
    let doc = json!({ "name": fan.connector_name, "config": Value::Object(config) });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
}

/// The ClickHouse side of the fan-out: for each table a Kafka-engine "queue"
/// table reading the Debezium topic, a MergeTree target, and a materialized view
/// that unpacks the Debezium envelope into the target. Column extraction is left
/// as a clearly-marked TODO because it depends on the source schema.
pub fn clickhouse_kafka_ddl(cfg: &CdcConfig, fan: &CdcFanout) -> String {
    let tables = if cfg.tables.is_empty() {
        // With no explicit tables, show a single representative block.
        vec!["<table>".to_string()]
    } else {
        cfg.tables.clone()
    };
    let mut out = String::new();
    for table in &tables {
        let topic = topic_for(fan, table);
        out.push_str(&format!(
            "\
-- Table: {table}  (topic: {topic})
CREATE TABLE {db}.{table}_queue
(
    raw String
)
ENGINE = Kafka
SETTINGS kafka_broker_list = '{brokers}',
         kafka_topic_list = '{topic}',
         kafka_group_name = '{group}',
         kafka_format = 'JSONAsString';

CREATE TABLE {db}.{table}
(
    -- TODO: define columns to match Postgres {schema}.{table}, then extract them
    -- from the Debezium `after` payload in the materialized view below.
    payload String,
    _op LowCardinality(String),
    _ingested_at DateTime DEFAULT now()
)
ENGINE = MergeTree
ORDER BY tuple();

CREATE MATERIALIZED VIEW {db}.{table}_mv TO {db}.{table} AS
SELECT JSONExtractString(raw, 'after') AS payload,
       JSONExtractString(raw, 'op')    AS _op
FROM {db}.{table}_queue;

",
            db = cfg.target_database,
            table = table,
            topic = topic,
            brokers = fan.bootstrap_servers,
            group = fan.consumer_group,
            schema = fan.schema,
        ));
    }
    out.trim_end().to_string()
}

/// The full reviewable fan-out plan: Postgres prereqs → Debezium connector JSON
/// → ClickHouse Kafka-ingest DDL. Print-only, like the direct path.
pub fn fanout_plan(cfg: &CdcConfig, fan: &CdcFanout) -> String {
    let publication_tables = if cfg.tables.is_empty() {
        "ALL TABLES".to_string()
    } else {
        format!("TABLE {}", cfg.tables.join(", "))
    };
    let pub_name = format!("{}_pub", cfg.target_database);
    format!(
        "\
-- CDC fan-out plan: Postgres `{db}` → Debezium → Kafka `{brokers}` → ClickHouse `{target}`.
-- Review before applying. Print-only; the agent never runs it. Use Kafka (vs the
-- direct MaterializedPostgreSQL path) when several consumers read one capture.

-- 1) Postgres prerequisites (same as the direct path):
--    wal_level = logical; a REPLICATION user; a primary key / REPLICA IDENTITY.
CREATE PUBLICATION {pub_name} FOR {publication_tables};

-- 2) Register the Debezium source connector with Kafka Connect:
--    curl -s -X POST http://connect:8083/connectors \\
--         -H 'Content-Type: application/json' -d @- <<'JSON'
{connector}
-- JSON

-- 3) ClickHouse: consume the Kafka topics into MergeTree tables.
{ch}

-- 4) Build analytical rollups on the target tables with `pg-mcp-agent materialize`
--    (tag those specs `Backend: clickhouse`).",
        db = cfg.source.database,
        target = cfg.target_database,
        brokers = fan.bootstrap_servers,
        pub_name = pub_name,
        publication_tables = publication_tables,
        connector = debezium_connector_config(cfg, fan),
        ch = clickhouse_kafka_ddl(cfg, fan),
    )
}

/// SQL to read ClickHouse's Kafka-engine consumer state — the fan-out analogue of
/// `pg_replication_slots`. Surfaces per-consumer activity and any exceptions.
pub const KAFKA_CONSUMERS_SQL: &str = "\
SELECT table,
       consumer_id,
       is_currently_used,
       arrayStringConcat(exceptions.text, '; ') AS last_exception,
       num_messages_read
FROM system.kafka_consumers
ORDER BY table, consumer_id;";

/// Health of a single ClickHouse Kafka consumer.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsumerStatus {
    pub name: String,
    pub active: bool,
    /// Consumer lag in messages, when the source provides it.
    pub lag: Option<f64>,
    /// The last exception text, if any.
    pub exception: Option<String>,
}

/// A parsed, flagged view of Kafka-consumer health.
#[derive(Debug, Default)]
pub struct ConsumerReport {
    pub consumers: Vec<ConsumerStatus>,
    pub issues: Vec<String>,
}

impl ConsumerReport {
    pub fn is_healthy(&self) -> bool {
        self.issues.is_empty()
    }
}

/// Parse a Kafka-consumer status result into a flagged report. Flexible about
/// the source: it uses whichever columns are present, so it works both for the
/// ClickHouse `system.kafka_consumers` shape (active/exception) and for a Kafka
/// MCP that also reports `lag`. Flags: no consumers, an inactive consumer, an
/// exception, or lag past `lag_warn_messages`.
pub fn analyze_consumers(table: &Table, lag_warn_messages: f64) -> ConsumerReport {
    let mut report = ConsumerReport::default();
    let name_i = col_index(table, "consumer_id").or_else(|| col_index(table, "table"));
    let active_i = col_index(table, "is_currently_used").or_else(|| col_index(table, "active"));
    let exc_i = col_index(table, "last_exception").or_else(|| col_index(table, "exception"));
    let lag_i = col_index(table, "lag");

    for row in &table.rows {
        let name = name_i
            .and_then(|i| row.get(i))
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
            .to_string();
        // A missing activity column is treated as active (assume running).
        let active = active_i
            .and_then(|i| row.get(i))
            .and_then(as_bool)
            .unwrap_or(true);
        let exception = exc_i
            .and_then(|i| row.get(i))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let lag = lag_i.and_then(|i| row.get(i)).and_then(as_f64);

        report.consumers.push(ConsumerStatus {
            name: name.clone(),
            active,
            lag,
            exception: exception.clone(),
        });

        if !active {
            report.issues.push(format!(
                "consumer `{name}` is not currently used — ingestion is stalled"
            ));
        }
        if let Some(e) = &exception {
            report
                .issues
                .push(format!("consumer `{name}` reported an exception: {e}"));
        }
        if let Some(l) = lag {
            if l > lag_warn_messages {
                report.issues.push(format!(
                    "consumer `{name}` is lagging {l:.0} messages behind"
                ));
            }
        }
    }

    if report.consumers.is_empty() {
        report.issues.push(
            "no Kafka consumers found — the ClickHouse Kafka tables are not set up (or not polling)"
                .to_string(),
        );
    }
    report
}

/// Render a Kafka-consumer report as human-readable lines for the CLI.
pub fn render_consumer_report(report: &ConsumerReport) -> String {
    let mut out = String::from("Kafka consumer health\n");
    if report.consumers.is_empty() {
        out.push_str("  (no Kafka consumers)\n");
    } else {
        for c in &report.consumers {
            let mark = if c.active && c.exception.is_none() {
                "✓"
            } else {
                "⚠"
            };
            let lag = c
                .lag
                .map(|l| format!(", lag {l:.0} msgs"))
                .unwrap_or_default();
            out.push_str(&format!(
                "  {mark} {} — {}{lag}\n",
                c.name,
                if c.active { "active" } else { "INACTIVE" }
            ));
        }
    }
    if report.issues.is_empty() {
        out.push_str("\n  Healthy: all consumers active and caught up.\n");
    } else {
        out.push_str(&format!("\n  {} issue(s):\n", report.issues.len()));
        for i in &report.issues {
            out.push_str(&format!("    - {i}\n"));
        }
    }
    out
}

/// Health of a single replication slot.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotStatus {
    pub name: String,
    pub active: bool,
    pub lag_bytes: Option<f64>,
}

/// A parsed, flagged view of replication health.
#[derive(Debug, Default)]
pub struct ReplicationReport {
    pub slots: Vec<SlotStatus>,
    /// Human-readable problems found (empty = healthy).
    pub issues: Vec<String>,
}

impl ReplicationReport {
    pub fn is_healthy(&self) -> bool {
        self.issues.is_empty()
    }
}

fn col_index(table: &Table, name: &str) -> Option<usize> {
    table.columns.iter().position(|c| c == name)
}

fn as_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim().to_lowercase().as_str() {
            "t" | "true" | "1" | "yes" => Some(true),
            "f" | "false" | "0" | "no" => Some(false),
            _ => None,
        },
        Value::Number(n) => n.as_i64().map(|i| i != 0),
        _ => None,
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Parse the result of [`REPLICATION_SLOTS_SQL`] into a flagged report.
/// Flags: no slots at all, any inactive slot, any slot lagging past
/// `lag_warn_bytes`.
pub fn analyze_slots(table: &Table, lag_warn_bytes: f64) -> ReplicationReport {
    let mut report = ReplicationReport::default();
    let name_i = col_index(table, "slot_name");
    let active_i = col_index(table, "active");
    let lag_i = col_index(table, "lag_bytes");

    for row in &table.rows {
        let name = name_i
            .and_then(|i| row.get(i))
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
            .to_string();
        let active = active_i
            .and_then(|i| row.get(i))
            .and_then(as_bool)
            .unwrap_or(false);
        let lag_bytes = lag_i.and_then(|i| row.get(i)).and_then(as_f64);
        report.slots.push(SlotStatus {
            name: name.clone(),
            active,
            lag_bytes,
        });

        if !active {
            report.issues.push(format!(
                "slot `{name}` is INACTIVE — replication is stalled or the consumer is gone"
            ));
        }
        if let Some(lag) = lag_bytes {
            if lag > lag_warn_bytes {
                report.issues.push(format!(
                    "slot `{name}` is lagging {:.1} MB behind the WAL",
                    lag / 1_048_576.0
                ));
            }
        }
    }

    if report.slots.is_empty() {
        report.issues.push(
            "no replication slots found — MaterializedPostgreSQL/CDC is not set up on this database".to_string(),
        );
    }
    report
}

/// Parse the result of [`WAL_LEVEL_SQL`]; returns an issue string if the WAL
/// level is anything other than `logical` (which CDC requires).
pub fn check_wal_level(table: &Table) -> Option<String> {
    let i = col_index(table, "wal_level")?;
    let level = table.rows.first()?.get(i)?.as_str()?;
    if level.eq_ignore_ascii_case("logical") {
        None
    } else {
        Some(format!(
            "wal_level is `{level}`, but CDC needs `logical` — set it in postgresql.conf and restart"
        ))
    }
}

/// Render a report as human-readable lines for the CLI.
pub fn render_report(wal_issue: &Option<String>, report: &ReplicationReport) -> String {
    let mut out = String::from("Replication health\n");
    if let Some(w) = wal_issue {
        out.push_str(&format!("  ⚠ {w}\n"));
    } else {
        out.push_str("  ✓ wal_level = logical\n");
    }
    if report.slots.is_empty() {
        out.push_str("  (no replication slots)\n");
    } else {
        for s in &report.slots {
            let lag = s
                .lag_bytes
                .map(|l| format!("{:.1} MB", l / 1_048_576.0))
                .unwrap_or_else(|| "?".into());
            let mark = if s.active { "✓" } else { "⚠" };
            out.push_str(&format!(
                "  {mark} slot {} — {}, lag {lag}\n",
                s.name,
                if s.active { "active" } else { "INACTIVE" }
            ));
        }
    }
    let total_issues = report.issues.len() + wal_issue.iter().count();
    if total_issues == 0 {
        out.push_str("\n  Healthy: CDC is running and caught up.\n");
    } else {
        out.push_str(&format!("\n  {total_issues} issue(s):\n"));
        for w in wal_issue.iter().chain(report.issues.iter()) {
            out.push_str(&format!("    - {w}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg() -> CdcConfig {
        CdcConfig {
            target_database: "analytics".into(),
            source: CdcSource {
                host: "pg.internal".into(),
                port: 5432,
                database: "shop".into(),
                user: "repl".into(),
                password_env: None,
            },
            tables: vec!["orders".into(), "order_items".into()],
            fanout: None,
        }
    }

    fn fanout() -> CdcFanout {
        CdcFanout {
            connector_name: "shop-connector".into(),
            topic_prefix: "pgch".into(),
            bootstrap_servers: "kafka:9092".into(),
            consumer_group: "clickhouse_analytics".into(),
            schema: "public".into(),
        }
    }

    #[test]
    fn ddl_targets_clickhouse_engine_and_lists_tables() {
        let ddl = materialized_postgresql_ddl(&cfg());
        assert!(ddl.contains("ENGINE = MaterializedPostgreSQL('pg.internal:5432', 'shop', 'repl'"));
        assert!(ddl.contains("materialized_postgresql_tables_list = 'orders,order_items'"));
        assert!(ddl.contains("CREATE DATABASE analytics"));
        assert!(ddl.contains("CREATE PUBLICATION analytics_pub FOR TABLE orders, order_items"));
    }

    #[test]
    fn ddl_uses_password_placeholder_when_env_unset() {
        let ddl = materialized_postgresql_ddl(&cfg());
        assert!(
            ddl.contains("{PASSWORD}"),
            "secret must not be required to render"
        );
    }

    #[test]
    fn ddl_reads_password_from_env_when_present() {
        std::env::set_var("CDC_TEST_PW", "s3cret");
        let mut c = cfg();
        c.source.password_env = Some("CDC_TEST_PW".into());
        let ddl = materialized_postgresql_ddl(&c);
        assert!(ddl.contains("'s3cret'"));
        std::env::remove_var("CDC_TEST_PW");
    }

    #[test]
    fn empty_tables_means_all_tables() {
        let mut c = cfg();
        c.tables.clear();
        let ddl = materialized_postgresql_ddl(&c);
        assert!(ddl.contains("FOR ALL TABLES"));
        assert!(!ddl.contains("materialized_postgresql_tables_list"));
    }

    // --- fan-out (Debezium → Kafka → ClickHouse) ---

    #[test]
    fn topic_name_follows_debezium_convention() {
        assert_eq!(topic_for(&fanout(), "orders"), "pgch.public.orders");
    }

    #[test]
    fn debezium_config_is_valid_json_with_the_right_fields() {
        let json_str = debezium_connector_config(&cfg(), &fanout());
        let v: Value = serde_json::from_str(&json_str).expect("valid JSON");
        assert_eq!(v["name"], "shop-connector");
        assert_eq!(
            v["config"]["connector.class"],
            "io.debezium.connector.postgresql.PostgresConnector"
        );
        assert_eq!(v["config"]["topic.prefix"], "pgch");
        assert_eq!(v["config"]["database.dbname"], "shop");
        // table.include.list is schema-qualified.
        assert_eq!(
            v["config"]["table.include.list"],
            "public.orders,public.order_items"
        );
        // The slot/publication names match the direct path's publication.
        assert_eq!(v["config"]["publication.name"], "analytics_pub");
    }

    #[test]
    fn debezium_config_hides_password_by_default() {
        let json_str = debezium_connector_config(&cfg(), &fanout());
        assert!(json_str.contains("{PASSWORD}"));
    }

    #[test]
    fn clickhouse_kafka_ddl_wires_queue_target_and_mv_per_table() {
        let ddl = clickhouse_kafka_ddl(&cfg(), &fanout());
        // Kafka-engine queue table pointed at the Debezium topic.
        assert!(ddl.contains("ENGINE = Kafka"));
        assert!(ddl.contains("kafka_topic_list = 'pgch.public.orders'"));
        assert!(ddl.contains("kafka_group_name = 'clickhouse_analytics'"));
        // Target MergeTree + materialized view per table, in the target database.
        assert!(ddl.contains("CREATE TABLE analytics.orders_queue"));
        assert!(ddl.contains("CREATE MATERIALIZED VIEW analytics.orders_mv TO analytics.orders"));
        assert!(ddl.contains("analytics.order_items_queue"));
    }

    #[test]
    fn fanout_plan_covers_prereqs_connector_and_clickhouse() {
        let plan = fanout_plan(&cfg(), &fanout());
        assert!(plan.contains("CREATE PUBLICATION analytics_pub FOR TABLE orders, order_items"));
        assert!(plan.contains("io.debezium.connector.postgresql.PostgresConnector"));
        assert!(plan.contains("ENGINE = Kafka"));
        assert!(plan.contains("kafka:9092"));
    }

    #[test]
    fn healthy_consumer_has_no_issues() {
        let t = slots_table(json!([
            {"table": "orders", "consumer_id": "c1", "is_currently_used": true, "last_exception": ""}
        ]));
        let report = analyze_consumers(&t, 100_000.0);
        assert!(report.is_healthy(), "issues: {:?}", report.issues);
        assert_eq!(report.consumers[0].name, "c1");
    }

    #[test]
    fn inactive_consumer_is_flagged() {
        let t = slots_table(json!([
            {"consumer_id": "c1", "is_currently_used": false, "last_exception": ""}
        ]));
        let report = analyze_consumers(&t, 100_000.0);
        assert!(!report.is_healthy());
        assert!(report.issues[0].contains("stalled"));
    }

    #[test]
    fn consumer_exception_is_flagged() {
        let t = slots_table(json!([
            {"consumer_id": "c1", "is_currently_used": true, "last_exception": "Local: Broker transport failure"}
        ]));
        let report = analyze_consumers(&t, 100_000.0);
        assert!(report.issues.iter().any(|i| i.contains("exception")));
    }

    #[test]
    fn consumer_lag_is_flagged_when_source_reports_it() {
        let t = slots_table(json!([
            {"consumer_id": "c1", "active": true, "lag": 500_000}
        ]));
        let report = analyze_consumers(&t, 100_000.0);
        assert!(report.issues.iter().any(|i| i.contains("lagging")));
    }

    #[test]
    fn no_consumers_is_flagged() {
        let t = slots_table(json!([]));
        let report = analyze_consumers(&t, 100_000.0);
        assert!(report.issues.iter().any(|i| i.contains("not set up")));
    }

    fn slots_table(rows: Value) -> Table {
        Table::from_tool_output(&rows.to_string()).unwrap()
    }

    #[test]
    fn healthy_slot_has_no_issues() {
        let t = slots_table(json!([
            {"slot_name": "ch_slot", "active": true, "lag_bytes": 1024}
        ]));
        let report = analyze_slots(&t, 64.0 * 1_048_576.0);
        assert!(report.is_healthy(), "issues: {:?}", report.issues);
        assert_eq!(report.slots[0].name, "ch_slot");
    }

    #[test]
    fn inactive_slot_is_flagged() {
        let t = slots_table(json!([
            {"slot_name": "dead", "active": false, "lag_bytes": 0}
        ]));
        let report = analyze_slots(&t, 64.0 * 1_048_576.0);
        assert!(!report.is_healthy());
        assert!(report.issues[0].contains("INACTIVE"));
    }

    #[test]
    fn lagging_slot_is_flagged() {
        let t = slots_table(json!([
            {"slot_name": "slow", "active": true, "lag_bytes": 200_000_000}
        ]));
        let report = analyze_slots(&t, 64.0 * 1_048_576.0);
        assert!(!report.is_healthy());
        assert!(report.issues.iter().any(|i| i.contains("lagging")));
    }

    #[test]
    fn no_slots_is_flagged() {
        let t = slots_table(json!([]));
        let report = analyze_slots(&t, 64.0 * 1_048_576.0);
        assert!(report.issues.iter().any(|i| i.contains("not set up")));
    }

    #[test]
    fn accepts_postgres_boolean_text() {
        // Some MCP servers stringify booleans as "t"/"f".
        let t = slots_table(json!([{"slot_name": "s", "active": "f", "lag_bytes": "0"}]));
        let report = analyze_slots(&t, 1.0);
        assert!(report.issues.iter().any(|i| i.contains("INACTIVE")));
    }

    #[test]
    fn wal_level_logical_is_ok() {
        let t = slots_table(json!([{"wal_level": "logical"}]));
        assert!(check_wal_level(&t).is_none());
    }

    #[test]
    fn wal_level_replica_is_flagged() {
        let t = slots_table(json!([{"wal_level": "replica"}]));
        assert!(check_wal_level(&t).unwrap().contains("logical"));
    }
}
