# pg-mcp-agent

An open-source, self-hosted **Postgres → ClickHouse pipeline copilot**: describe a
metric in plain English and it grounds the query in a versioned semantic layer,
**verifies** it against the data (no confident-wrong numbers), and emits the
Postgres or ClickHouse materialized-view DDL you review and apply. It runs on a
**local Ollama model** and drives your **existing MCP servers** — data never
leaves your network — with **writes gated in the agent**, not left to the model's
judgment.

> **Try it in 30 seconds, no database:** `cargo build && cargo run -- verify config.pgch.mock.json`
> spins up a mock Postgres **and** a mock ClickHouse server and verifies a
> metric against each. See [the zero-setup demo](#postgres--clickhouse-copilot-zero-setup).

The agent is the **control plane** — it authors and validates SQL; the databases
run the data plane. Postgres is the row-oriented source of truth; ClickHouse is
the columnar engine for big aggregations and time-series rollups.

📐 **[Architecture](docs/architecture.md)** (diagrams) · 📖 **[Tutorial](docs/tutorial.md)**
(zero-setup, worked examples) · 🛠 **[Developer guide](docs/development.md)** ·
🧭 **[Product strategy](docs/product-strategy.md)** ·
🔌 **[ClickHouse integration](docs/clickhouse-integration.md)** ·
🤖 **[CI/CD agents](docs/cicd-agents.md)**

**New here?** `cargo build && cargo demo` runs the whole Postgres + ClickHouse
loop offline in ~30 seconds. Then read the [tutorial](docs/tutorial.md).

```
┌────────────┐   /api/chat + tools    ┌─────────────┐   MCP (JSON-RPC/stdio)   ┌──────────────┐   SQL   ┌──────────┐
│  Ollama    │◄──────────────────────►│ pg-mcp-agent│◄────────────────────────►│ Postgres MCP │◄───────►│ Postgres │
│ (local LLM)│    tool calls back      │  + guard    │   tools/list, tools/call │    server     │         │          │
└────────────┘                         └─────────────┘                          └──────────────┘         └──────────┘
```

The model never touches the database directly. Every tool call is inspected: the
guard finds the SQL argument, classifies the statement (read / write / DDL), and
either runs it, asks you to confirm, or blocks it.

## Why gate on the SQL, not the tool name

Most Postgres MCP servers funnel everything through one `execute_sql`/`query`
tool, so "only allow the read tool" doesn't work. Instead the guard
([`src/guard.rs`](src/guard.rs)) parses the SQL:

| Statement                              | Class | Default policy                 |
|----------------------------------------|-------|--------------------------------|
| `SELECT`, `EXPLAIN`, read-only `WITH`  | read  | run silently                   |
| `INSERT` / `UPDATE` / `DELETE` / `COPY`| write | **confirm** (allow_writes=true)|
| `CREATE` / `ALTER` / `DROP` / `TRUNCATE`| DDL  | **blocked** (allow_ddl=false)  |
| multiple statements, unknown keyword   | ?     | confirm (fail toward the human)|

It strips comments before classifying and treats multi-statement input as
unknown, so a write can't ride in behind `SELECT 1; DROP …` or a `/* */` comment.

Classification is **dialect-aware**: in a mixed Postgres + ClickHouse setup each
call is classified in its own server's dialect, so a ClickHouse `ALTER TABLE …
DELETE` is gated as a *write* (a row mutation), not blocked as Postgres DDL, while
`OPTIMIZE` is treated as maintenance. The router tags each server's dialect from
config (`dialect: "clickhouse"`, or inferred from the name/command).

## Prerequisites

1. **Rust** (rustup stable, 1.97+). Build with `cargo build --release`.
2. **Ollama** with a tool-capable model:
   ```
   # install from https://ollama.com, then:
   ollama pull qwen2.5-coder      # or llama3.1 — both support tool calls
   ollama serve                   # if it isn't already running
   ```
3. **A Postgres MCP server.** Two easy options:
   - Writes (recommended): [crystaldba/postgres-mcp](https://github.com/crystaldba/postgres-mcp)
     via `uvx` — needs [uv](https://docs.astral.sh/uv). It exposes `execute_sql`.
   - Read-only demo (no uv): the reference `@modelcontextprotocol/server-postgres`
     via `npx` (you have Node). No write tool, so only the read path exercises.

## Run

```
cp config.example.json config.json      # then edit DATABASE_URI + model
cargo run --release                     # or: cargo run --release -- path/to/config.json
```

Non-interactive / scripting:

```
cargo run -- --yes                          # auto-approve guarded writes
cargo run -- --prompt "revenue by month"    # run one request and exit (implies --yes)
cargo run -- verify                         # run specs against the DB (exit-nonzero on failure)
```

### Try it without Postgres

A bundled **mock MCP server** ([`src/bin/mock_mcp_server.rs`](src/bin/mock_mcp_server.rs))
speaks the same stdio protocol and returns canned sales rows, so you can exercise
the whole loop (LLM → tool call → guard → analytics) with no database:

```
cargo build
cargo run -- --prompt "total sales by region" config.mock.json
```

[`config.mock.json`](config.mock.json) points at the mock server. This is also
what the end-to-end tests drive.

### Postgres + ClickHouse copilot (zero setup)

The product wedge — a metric verified against Postgres *and* a ClickHouse rollup —
runs entirely offline. Two bundled mock servers stand in for the real databases:
[`mock_mcp_server`](src/bin/mock_mcp_server.rs) (Postgres, `execute_sql`) and
[`mock_ch_server`](src/bin/mock_ch_server.rs) (ClickHouse, `run_select_query`).
[`config.pgch.mock.json`](config.pgch.mock.json) wires both, each tagged with its
`dialect`, and points at the [`specs-demo/`](specs-demo/demo.spec.md) metrics.

```
cargo build
cargo run -- verify config.pgch.mock.json
```

```
Verifying 2 spec(s)

  PASS  sales by region (Postgres)        [execute_sql]
  PASS  daily revenue rollup (ClickHouse) [run_select_query]

2 passed, 0 failed
```

Each spec routes to the server matching its `Backend:` tag. Then turn the verified
metrics into materialized-view DDL — Postgres and ClickHouse get different shapes:

```
cargo run -- materialize config.pgch.mock.json
```

```sql
-- sales by region (Postgres)  [postgres]
CREATE MATERIALIZED VIEW mv_sales_by_region_postgres AS
SELECT region, SUM(sales) AS sales FROM sales GROUP BY region ORDER BY sales DESC
WITH DATA;
REFRESH MATERIALIZED VIEW mv_sales_by_region_postgres;

-- daily revenue rollup (ClickHouse)  [clickhouse]
CREATE MATERIALIZED VIEW mv_daily_revenue_rollup_clickhouse
ENGINE = SummingMergeTree()
ORDER BY (day)
POPULATE AS
SELECT toDate(created_at) AS day, sum(quantity * unit_price) AS revenue, count() AS orders
FROM order_items GROUP BY day ORDER BY day;
```

Postgres emits a snapshot MV + `REFRESH`; ClickHouse emits an *incremental* MV
with an `ENGINE` and sorting key (it updates as rows arrive — no manual refresh).
To point at real databases, copy the `_mcp_servers_pgch_example` block from
[`config.example.json`](config.example.json) (a `uvx postgres-mcp` +
`uvx mcp-clickhouse` pair).

You also get a REPL:

```
› how many orders were placed last week?
  ✓ execute_sql ran
There were 1,204 orders between …

› delete the test orders from yesterday
  ⚠ write statement requested via `execute_sql`:
    DELETE FROM orders WHERE tag = 'test' AND created_at::date = current_date - 1
  Run this? [y/N] y
  ✓ execute_sql ran
Removed 7 rows.
```

## Config

See [`config.example.json`](config.example.json). Key knobs:

- `ollama.model` — any Ollama model that supports tools.
- `mcp_server.{command,args,env}` — how to launch the MCP server. `env` is where
  the connection string goes (`DATABASE_URI` for crystaldba).
- `mcp_servers` — an **array** for connecting several servers at once (e.g.
  Postgres + ClickHouse). Each entry may set a `name` (used to namespace tools
  when names collide, exposed as `<name>__<tool>`) and a `dialect`
  (`postgres` | `clickhouse`) so the guard classifies that server's SQL correctly
  and backend-tagged specs route to it. Takes precedence over `mcp_server`.
- `guard.allow_writes` / `allow_ddl` / `confirm_reads` — the policy. Set
  `allow_ddl` to `true` only if you really want CREATE/ALTER/DROP (still confirmed).
- `max_steps` — cap on tool-call rounds per turn.

## Correctness & safety guardrails

- **Answer verifier.** After the model answers, the agent recomputes the
  plausible aggregates from the last result set and warns if a figure in the
  answer matches none of them — catching hallucinated numbers (a model once said
  "$300" for a $150 sum). Soft advisory, never blocks. Toggle with
  `verify_answers` (default on). See [`src/verify.rs`](src/verify.rs).
- **Audit log.** Set `audit_log` to a path to append one JSONL record per tool
  call: tool, arguments, guard decision (allow/blocked/confirmed/declined/
  suppressed), and status. Security research on MCP stresses end-to-end activity
  logging, and this is the foundation for it. See [`src/audit.rs`](src/audit.rs).
- **SQL write-guard** (below) gates writes/DDL regardless of what the model asks.

## Local-model robustness

Local models don't all use Ollama's structured tool-calling reliably. Two
safeguards keep the loop working (both verified end-to-end with `qwen2.5-coder`):

- **Text tool-call recovery.** Some models emit a tool call as JSON in the
  message content instead of the `tool_calls` field. The agent detects that
  (fenced blocks, `function`/`tool_call` wrappers, `arguments` vs `parameters`,
  arrays) and only accepts calls naming a known tool, so prose is never mistaken
  for a call. Recovered calls are rewritten into the transcript as a proper
  `assistant(tool_calls=…) → tool(result)` pair, or the model loops re-calling.
- **Repeat-call guard.** If a model calls the same tool with identical arguments
  more than twice in one turn, further identical calls are suppressed and the
  model is nudged to answer with what it already has.

## Plain English → correct SQL: the semantic layer

Research on NL→SQL is blunt: models write syntactically valid SQL, but on real
schemas they invent business meaning and return *confident wrong numbers* (one
study: 91% on academic benchmarks → 21% on real enterprise schemas). The fix is
a **semantic layer** — business terms and verified query fragments the model
reuses instead of guessing. That lifts accuracy toward ~98–100% and turns
failures into honest "I can't answer that" instead of wrong numbers.

Here the semantic layer is a folder of **Gauge-style markdown specs**
(`specs/*.spec.md`, see [`specs/orders.spec.md`](specs/orders.spec.md)). Each file
carries a glossary and verified example queries:

```markdown
## Glossary
- **active customer**: a customer with an order in the last 90 days

## Example: monthly revenue
Question: revenue by month
Expect: contains revenue
```sql
SELECT date_trunc('month', o.created_at) AS month, SUM(oi.quantity*oi.unit_price) AS revenue
FROM orders o JOIN order_items oi ON oi.order_id = o.id GROUP BY 1 ORDER BY 1;
```
```

Two payoffs:

- **Grounding** — the glossary and examples are injected into the system prompt,
  so the model reuses known-good SQL and honors your definitions.
- **Test design (TDD)** — `pg-mcp-agent verify` runs every example against the
  real database and checks its `Expect:` (runs / non-empty / contains …). Specs
  stay honest as the schema drifts. Exit code is non-zero on any failure, so it
  drops straight into CI. Add `--json` for machine-readable output: it feeds the
  **spec-triage agent** ([`.claude/agents/spec-triage.md`](.claude/agents/spec-triage.md)),
  which diagnoses each failing metric and proposes a fix on the PR — see
  [docs/cicd-agents.md](docs/cicd-agents.md).

```
cargo run -- verify           # PASS/FAIL per spec against the live DB
cargo run -- init-specs       # generate specs/generated.spec.md from the schema
cargo run -- materialize      # print backend-aware MATERIALIZED VIEW DDL from the specs
cargo run -- cdc plan             # print Postgres→ClickHouse CDC setup DDL (direct)
cargo run -- cdc plan --via kafka # print the Debezium→Kafka→ClickHouse fan-out plan
cargo run -- cdc inspect          # check replication + Kafka-consumer health
```

`init-specs` introspects `information_schema` through the MCP server and writes a
starter spec (a glossary placeholder plus one "rows from <table>" example per
table) to fill in — a fast way to seed the semantic layer.

`materialize` turns each verified spec into materialized-view DDL you can drop
into a migration — a verified query becomes a maintained rollup. It is
**backend-aware**: a spec tagged `Backend: clickhouse` (with optional `Engine:`
and `Order by:`) emits a ClickHouse incremental MV; otherwise a Postgres snapshot
MV + `REFRESH`. It only prints SQL, never applies it.

An always-on cheatsheet of analytical Postgres idioms (time bucketing, window
functions, ROLLUP, percentiles, cohort/retention, funnels) lives in
[`src/knowledge.rs`](src/knowledge.rs) and is injected too.

## Local analytics on a result set

After any query, the model can call the built-in `analyze_last_result` tool to
crunch the returned rows **in memory**, without another DB round-trip:

- `op=describe` — per-column stats (count/min/max/mean/sum/stddev, or
  distinct-count for text)
- `op=group_by` with `by` / `agg` / `column` — aggregate a metric by dimensions
- `op=top` with `by` / `n` — top-N ranking

The engine ([`src/analytics.rs`](src/analytics.rs)) is dependency-free. A heavier
**DataFusion**-backed engine adds an `op=sql` mode that runs arbitrary analytical
SQL — window functions, joins, the lot — over the last result set (registered as
table `t`), behind an optional cargo feature
([`src/analytics_datafusion.rs`](src/analytics_datafusion.rs)):

```
cargo build --features datafusion
```

```
› pull last month's orders, then show a 7-day moving average of daily revenue
  ✓ execute_sql ran
  ✓ analyze_last_result (sql) ran     # DataFusion computed the window locally
```

> Toolchain note: build this project with the **rustup** stable toolchain
> (1.97+); [`rust-toolchain.toml`](rust-toolchain.toml) pins the channel. The
> DataFusion feature tracks current DataFusion (50.x) — the old rustc-1.85 pins
> (`datafusion = 45`, `comfy-table = 7.1.1`) have been dropped. The built-in
> engine needs none of this.

## CDC control plane: Postgres → ClickHouse

Getting data into ClickHouse is the other half of the wedge. The lowest-effort
near-real-time path is ClickHouse's `MaterializedPostgreSQL` engine, which
consumes the Postgres WAL over logical replication and keeps selected tables in
sync. Following the principle **one well-operated capture path, many
materialization paths**, the agent is the *control plane* — it proposes and
validates the setup and watches its health — while the capture itself runs in the
database layer. Two commands ([`src/cdc.rs`](src/cdc.rs), both safe):

- `cdc plan` reads the config's `cdc` section and prints the setup DDL: the
  Postgres publication + prerequisites and the ClickHouse
  `CREATE DATABASE … ENGINE = MaterializedPostgreSQL(…)`. It is **print-only** and
  the password is a `{PASSWORD}` placeholder unless `source.password_env` names a
  set env var — secrets never land in generated files.
- `cdc plan --via kafka` prints the **fan-out** path instead — a Debezium source
  connector (the JSON to POST to Kafka Connect) plus the ClickHouse Kafka-engine
  ingest DDL — for when several consumers need the same change stream. See
  **[docs/cdc-fanout.md](docs/cdc-fanout.md)** for the full guide and a worked
  near-real-time-revenue example.
- `cdc inspect` runs read-only queries and flags the usual failure modes — WAL
  level not `logical`, an **inactive** or **lagging** replication slot, and (when
  the fan-out is configured) a stalled or exception-throwing ClickHouse Kafka
  consumer — exiting non-zero when unhealthy so it works as a CI / monitoring check.

Build the analytical rollups on top with `materialize` (specs tagged
`Backend: clickhouse`). See the [tutorial](docs/tutorial.md#cdc-plan--postgres--clickhouse-setup-ddl)
for worked output and [docs/clickhouse-integration.md](docs/clickhouse-integration.md)
for the design.

## Layout

| File | Responsibility |
|------|----------------|
| [`src/mcp.rs`](src/mcp.rs) | MCP stdio client: initialize, tools/list, tools/call |
| [`src/ollama.rs`](src/ollama.rs) | `/api/chat` client with tool-calling |
| [`src/guard.rs`](src/guard.rs) | SQL classification + policy (unit-tested) |
| [`src/audit.rs`](src/audit.rs) | JSONL audit log of tool calls + guard decisions |
| [`src/verify.rs`](src/verify.rs) | answer verifier: flag figures not in the data's aggregates |
| [`src/knowledge.rs`](src/knowledge.rs) | always-on analytical Postgres cheatsheet |
| [`src/semantics.rs`](src/semantics.rs) | markdown semantic layer + spec verification |
| [`src/analytics.rs`](src/analytics.rs) | built-in local result-set analytics + `analyze` tool |
| [`src/analytics_datafusion.rs`](src/analytics_datafusion.rs) | optional DataFusion engine (feature) |
| [`src/agent.rs`](src/agent.rs) | the loop: MCP tools → Ollama → guard → execute |
| [`src/router.rs`](src/router.rs) | multi-server routing: merge tool lists, namespace collisions, per-server dialect |
| [`src/pipeline.rs`](src/pipeline.rs) | verified spec → backend-aware materialized-view DDL (`materialize`) |
| [`src/cdc.rs`](src/cdc.rs) | CDC control plane: Postgres→ClickHouse replication plan + health inspection |
| [`src/config.rs`](src/config.rs) | JSON config |
| [`src/lib.rs`](src/lib.rs) | library surface (so tests + the mock server share the code) |
| [`src/main.rs`](src/main.rs) | CLI: REPL, one-shot `--prompt`, `--yes`, `verify`, `materialize` |
| [`src/bin/mock_mcp_server.rs`](src/bin/mock_mcp_server.rs) | in-memory *Postgres* MCP server for offline runs + tests |
| [`src/bin/mock_ch_server.rs`](src/bin/mock_ch_server.rs) | in-memory *ClickHouse* MCP server so the full pg+ch demo runs offline |
| [`tests/mcp_e2e.rs`](tests/mcp_e2e.rs) | end-to-end tests of the stdio client against the mock servers |

Run the tests with `cargo test` (add `--features datafusion` to include that engine).

## Limits / next steps

- Confirmation is interactive (stdin); `--yes` auto-approves for non-interactive
  / scripting use. A pre-approved statement allowlist is a possible next step.
- The MCP client is synchronous request/response (one tool call at a time),
  which is all the agent needs here.
- No streaming of model output yet; responses print when complete.
- `verify` routes each spec by its `Backend:` tag, but per-spec routing picks the
  first SQL tool matching that dialect; explicit per-spec server targeting is a
  possible refinement.
- `cdc plan` / `cdc inspect` cover the `MaterializedPostgreSQL` capture path and
  slot health; the Debezium→Kafka→ClickHouse fan-out and scheduled ELT paths
  (see [`docs/product-strategy.md`](docs/product-strategy.md)) are not built yet.
- CI/CD automation (spec-verifier gate, schema-drift watch, guard-policy review)
  is sketched in [`docs/cicd-agents.md`](docs/cicd-agents.md).
