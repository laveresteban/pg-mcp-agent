# Tutorial

A hands-on tour of pg-mcp-agent. **Every example here runs with zero external
setup** — bundled mock MCP servers stand in for Postgres and ClickHouse, so you
can follow along without a database (and without Ollama for the non-LLM commands).

- [0. Build](#0-build)
- [1. The deterministic commands (no LLM, no DB)](#1-the-deterministic-commands-no-llm-no-db)
  - [verify](#verify--run-metrics-against-the-right-backend)
  - [materialize](#materialize--metrics--materialized-view-ddl)
  - [cdc plan](#cdc-plan--postgres--clickhouse-setup-ddl)
  - [cdc inspect](#cdc-inspect--replication-health)
- [2. Writing a spec](#2-writing-a-spec)
- [3. The interactive agent (needs Ollama)](#3-the-interactive-agent-needs-ollama)
- [4. The write-guard in action](#4-the-write-guard-in-action)
- [5. The audit log](#5-the-audit-log)
- [6. Local analytics & DataFusion](#6-local-analytics--datafusion)
- [7. Pointing at real databases](#7-pointing-at-real-databases)

Throughout, `cargo` means the rustup toolchain (1.97+). On this machine that is
`~/.cargo/bin/cargo.exe`.

## 0. Build

```bash
cargo build
```

This builds the agent and the two mock servers (`mock_mcp_server`,
`mock_ch_server`) plus the catalog server. [`config.pgch.mock.json`](../config.pgch.mock.json)
wires the two mock databases together and points at the demo specs in
[`specs-demo/`](../specs-demo/demo.spec.md).

## 1. The deterministic commands (no LLM, no DB)

These four commands are pure functions of your specs/config — no model, no live
database — which is exactly why they drop cleanly into CI.

### `verify` — run metrics against the right backend

```bash
cargo run -- verify config.pgch.mock.json
```

```
Verifying 2 spec(s)

  PASS  sales by region (Postgres)        [execute_sql]
  PASS  daily revenue rollup (ClickHouse) [run_select_query]

2 passed, 0 failed
```

Each spec carries a `Backend:` tag. The Postgres metric routes to the `pg`
server's `execute_sql`; the ClickHouse metric routes to the `ch` server's
`run_select_query`. `verify` exits non-zero if any spec fails, so
`cargo run -- verify` is a CI gate that keeps your metric definitions honest as
the schema drifts.

### `materialize` — metrics → materialized-view DDL

```bash
cargo run -- materialize config.pgch.mock.json
```

```sql
-- sales by region (Postgres)  [postgres]
CREATE MATERIALIZED VIEW mv_sales_by_region_postgres AS
SELECT region, SUM(sales) AS sales
FROM sales
GROUP BY region
ORDER BY sales DESC
WITH DATA;
REFRESH MATERIALIZED VIEW mv_sales_by_region_postgres;

-- daily revenue rollup (ClickHouse)  [clickhouse]
CREATE MATERIALIZED VIEW mv_daily_revenue_rollup_clickhouse
ENGINE = SummingMergeTree()
ORDER BY (day)
POPULATE AS
SELECT toDate(created_at) AS day,
       sum(quantity * unit_price) AS revenue,
       count() AS orders
FROM order_items
GROUP BY day
ORDER BY day;
```

Same specs, two DDL shapes: Postgres gets a snapshot MV + `REFRESH`; ClickHouse
gets an *incremental* MV with an `ENGINE` and sorting key (it updates as rows
arrive — no manual refresh). `materialize` only prints SQL; it never applies it,
so it is always safe to run and easy to review in a PR.

### `cdc plan` — Postgres → ClickHouse setup DDL

```bash
cargo run -- cdc plan config.pgch.mock.json
```

```sql
-- CDC plan: Postgres `shop` → ClickHouse `analytics` via MaterializedPostgreSQL.
-- Review before applying. This is print-only; the agent never runs it.

-- 1) Postgres prerequisites (run on the SOURCE, as a superuser):
--    * postgresql.conf: wal_level = logical  (needs a restart)
--    * the replication user needs REPLICATION + SELECT on the tables.
--    * each replicated table needs a primary key or REPLICA IDENTITY FULL.
CREATE PUBLICATION analytics_pub FOR TABLE orders, order_items;

-- 2) ClickHouse: create the replicated database (run on the TARGET).
CREATE DATABASE analytics
ENGINE = MaterializedPostgreSQL('localhost:5432', 'shop', 'replicator', '{PASSWORD}')
SETTINGS materialized_postgresql_tables_list = 'orders,order_items';

-- 3) Build analytical rollups on top with `pg-mcp-agent materialize`.
```

The plan comes from the `cdc` section of the config. Note the `{PASSWORD}`
placeholder: the secret is read from `source.password_env` at generate time and
never stored in the config or the output. If you export the env var it is
substituted:

```bash
PGPASSWORD=s3cret cargo run -- cdc plan config.pgch.mock.json | grep MaterializedPostgreSQL
# ENGINE = MaterializedPostgreSQL('localhost:5432', 'shop', 'replicator', 's3cret')
```

### `cdc inspect` — replication health

```bash
cargo run -- cdc inspect config.pgch.mock.json
```

```
Replication health
  ✓ wal_level = logical
  ✓ slot clickhouse_analytics — active, lag 0.0 MB

  Healthy: CDC is running and caught up.
```

`inspect` runs two read-only queries against the Postgres server
(`current_setting('wal_level')` and `pg_replication_slots`), parses the results,
and flags the classic failure modes: WAL level not `logical`, an **inactive**
slot (a stalled or abandoned consumer), or a slot **lagging** past 64 MB behind
the WAL. It exits non-zero when unhealthy, so it too works as a CI/monitoring
check. (The mock returns a healthy slot; against a real DB with no CDC set up it
would report "no replication slots found".)

## 2. Writing a spec

A spec file is Gauge-style markdown. Here is the ClickHouse metric from
[`specs-demo/demo.spec.md`](../specs-demo/demo.spec.md):

```markdown
## Metric: daily revenue rollup (ClickHouse)
Question: daily revenue for the dashboard
Backend: clickhouse
Engine: SummingMergeTree()
Order by: day
Expect: contains revenue
```sql
SELECT toDate(created_at) AS day,
       sum(quantity * unit_price) AS revenue,
       count() AS orders
FROM order_items
GROUP BY day
ORDER BY day;
```​
```

- `Question:` lines (one or more) are natural-language phrasings injected into the
  prompt so the model reuses this known-good SQL.
- `Expect:` is the test assertion: `runs`, `non-empty`, or `contains <text>`.
- `Backend:` routes the metric (default `postgres`). `Engine:` / `Order by:` are
  ClickHouse-only knobs for `materialize`.

Drop a `*.spec.md` file in the `specs_dir` folder and it is picked up by
`verify`, `materialize`, and the prompt grounding automatically. To bootstrap
from an existing schema, run `cargo run -- init-specs` — it introspects
`information_schema` (or a connected catalog) and writes a starter spec.

## 3. The interactive agent (needs Ollama)

For the LLM-driven REPL you need Ollama running with a tool-capable model:

```bash
ollama pull qwen2.5-coder
ollama serve            # if not already running
cargo run -- config.pgch.mock.json
```

```
› total sales by region
  ✓ execute_sql ran
The east region sold 150 and the west region sold 200.
```

Or run one request and exit (implies `--yes`):

```bash
cargo run -- --prompt "total sales by region" config.pgch.mock.json
```

The model sees your semantic layer in its system prompt, calls the MCP tools, and
the guard gates each call. After it answers, the **answer verifier** re-derives
the aggregates from the returned rows and warns if a figure in the prose matches
none of them — catching hallucinated numbers.

## 4. The write-guard in action

The guard classifies the SQL the model emits and applies your policy. With the
default policy (`allow_writes: true`, `allow_ddl: false`):

```
› delete the test orders from yesterday
  ⚠ write statement requested via `execute_sql`:
    DELETE FROM orders WHERE tag = 'test' AND created_at::date = current_date - 1
  Run this? [y/N] y
  ✓ execute_sql ran

› drop the orders table
  ⛔ blocked `execute_sql`: DDL is disabled by policy (allow_ddl = false)
```

Reads run silently, writes ask, DDL is blocked, and anything ambiguous
(multi-statement, unknown keyword) asks — always failing toward the human. A
write cannot hide behind `SELECT 1; DROP …` (multi-statement → unknown → confirm)
or a comment (stripped before classifying). Use `--yes` to auto-approve in
non-interactive runs.

## 5. The audit log

Set `audit_log` in the config (the mock config uses `audit-mock.jsonl`) to append
one JSONL record per tool call:

```json
{"tool":"execute_sql","decision":"allow","status":"ok","args":{"sql":"SELECT ..."},"detail":""}
{"tool":"execute_sql","decision":"blocked","status":"skipped","args":{"sql":"DROP ..."},"detail":"DDL is disabled by policy"}
```

Every path is recorded — allow, confirmed, declined, blocked, suppressed — which
is the foundation for the MCP activity logging the security research calls for.

## 6. Local analytics & DataFusion

After any query, the model can call the built-in `analyze_last_result` tool to
crunch the returned rows **in memory** (no second DB round-trip): `op=describe`,
`op=group_by`, `op=top`. Enable the heavier DataFusion engine for arbitrary
analytical SQL (`op=sql`, window functions and joins over the last result set,
registered as table `t`):

```bash
cargo build --features datafusion
cargo test --features datafusion
```

## 7. Pointing at real databases

Swap the mock servers for real MCP servers in your config. Copy the
`_mcp_servers_pgch_example` block from [`config.example.json`](../config.example.json):

```json
"mcp_servers": [
  { "name": "pg", "command": "uvx", "args": ["postgres-mcp", "--access-mode", "unrestricted"],
    "env": { "DATABASE_URI": "postgresql://user:pw@localhost:5432/shop" }, "dialect": "postgres" },
  { "name": "ch", "command": "uvx", "args": ["mcp-clickhouse"],
    "env": { "CLICKHOUSE_HOST": "localhost", "CLICKHOUSE_USER": "readonly", "CLICKHOUSE_PASSWORD": "" },
    "dialect": "clickhouse" }
]
```

Everything above works identically — the mock servers speak the same MCP stdio
protocol the real ones do. Start read-only and least-privilege; the guard is a
safety net, not a substitute for a scoped database role.

See [architecture.md](architecture.md) for how the pieces fit together, and
[../README.md](../README.md) for the design rationale behind each guardrail.
```
