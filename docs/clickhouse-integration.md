# Design: ClickHouse + ClickHouse MCP integration

How to add ClickHouse as an analytical backend alongside Postgres. This is a
design, tied to what the project already has (the multi-server `router`, the SQL
`guard`, the `semantics` layer, `materialize`, and the `verify` command).

## Why ClickHouse here

Postgres is the system of record (OLTP): correct, transactional, row-oriented.
ClickHouse is columnar OLAP: it chews through large aggregations and time-series
scans far faster. A natural split:

- **Postgres** — source of truth, writes, small/point queries, the schema the
  catalog describes.
- **ClickHouse** — big GROUP BYs, funnels, retention, dashboards over lots of
  history. The agent's `analyze`/reporting workload.

The agent is already a multi-server MCP client, so adding ClickHouse is mostly
configuration plus a few dialect-aware touches.

## Connecting it (config only, today)

ClickHouse ships an official MCP server. Add it as a second entry in
`mcp_servers`:

```json
"mcp_servers": [
  { "name": "pg",  "command": "uvx", "args": ["postgres-mcp", "--access-mode", "unrestricted"],
    "env": { "DATABASE_URI": "postgresql://…" } },
  { "name": "ch",  "command": "uvx", "args": ["mcp-clickhouse"],
    "env": { "CLICKHOUSE_HOST": "…", "CLICKHOUSE_USER": "readonly", "CLICKHOUSE_PASSWORD": "…" } }
]
```

Both servers expose a SQL-execution tool. The `router` already namespaces the
collision as `pg__<tool>` / `ch__<tool>` and routes each call to the right
engine. Nothing else is required to *reach* ClickHouse.

## What needs a code touch

1. **Guard: ClickHouse dialect.** The guard classifies statements by leading
   keyword, which mostly holds for ClickHouse, but a few cases differ and should
   be added to [`src/guard.rs`](../src/guard.rs):
   - `ALTER TABLE … DELETE/UPDATE` in ClickHouse is a **mutation** (data change),
     not schema DDL — today it classifies as DDL and gets blocked. Add a check:
     an `ALTER … DELETE/UPDATE` is a *write*, plain `ALTER` is DDL.
   - `OPTIMIZE`, `TRUNCATE`, `INSERT … SELECT` — OPTIMIZE is maintenance (treat
     as DDL/blocked by default), INSERT is a write.
   - `ATTACH`/`DETACH`/`RENAME` partitions — DDL.
   A small `dialect: Postgres | ClickHouse` on the policy, inferred per server,
   keeps classification correct. (The guard runs on the SQL, so per-server
   dialect means the router tells the guard which backend a call targets.)

2. **Routing which backend.** Three options, best first:
   - **Spec target tags.** Extend the spec format with `Backend: clickhouse` on
     an example; `verify`/`materialize` then target that server. Deterministic
     and reviewable — fits the semantic-layer philosophy.
   - **Tool-description hints.** The ClickHouse tool description says "prefer for
     large aggregations and time-series." The model routes by picking the tool.
     Zero new code, but non-deterministic.
   - **A planner** that sends heavy analytical SQL to ClickHouse. More power,
     more latency/complexity — defer (per the Alation caution in the roadmap).

3. **Materialized views differ.** `materialize` currently emits Postgres
   `CREATE MATERIALIZED VIEW … WITH DATA` + `REFRESH`. ClickHouse MVs are
   **incremental** (they update as data is inserted) and need an `ENGINE` and
   usually a target table: `CREATE MATERIALIZED VIEW mv TO target ENGINE = … AS
   SELECT …`. Add a backend parameter to [`src/pipeline.rs`](../src/pipeline.rs)
   so the generated DDL matches the target engine.

## Getting data into ClickHouse (the pipeline)

This is where the "one capture path, many materialization paths" idea pays off,
and it lines up with near-real-time ETL experience:

- **ClickHouse `MaterializedPostgreSQL` engine** — ClickHouse replicates
  Postgres natively over logical replication (CDC). Point it at the Postgres WAL
  and selected tables land in ClickHouse and stay in sync. Lowest-effort
  near-real-time path; the agent's role is to inspect replication health and
  generate the analytical views on top.
- **Debezium → Kafka → ClickHouse** — the heavier, more general CDC fan-out when
  ClickHouse is one of several consumers. A Kafka MCP server lets the agent
  inspect topic lag.
- **Periodic ELT** — scheduled `INSERT INTO ch SELECT … FROM postgres_fdw` or a
  dump/load. Simplest, not real-time.

The agent stays the **control plane**: it proposes and validates the replication
setup and writes the ClickHouse analytical views, but the capture path runs in
the database layer, not in the agent.

## DataFusion vs ClickHouse (both already relevant)

- **DataFusion** (`op=sql`, optional feature) — in-process analytics over a
  *result set the agent already fetched*. Great for a quick second-pass
  transform without another round-trip. Small data.
- **ClickHouse** — server-side OLAP over the *full* dataset. Large data,
  persistent, shared. The heavyweight.

They don't compete: DataFusion is the local scratchpad, ClickHouse is the
warehouse.

## Security notes specific to ClickHouse

- Use a **read-only ClickHouse profile/user** with row/complexity quotas
  (`max_rows_to_read`, `max_execution_time`). ClickHouse's settings profiles make
  this clean and enforce cost limits the agent can't exceed.
- Keep the guard's writes/DDL gating on; add the dialect fixes above so a
  ClickHouse mutation isn't mis-classified.

## Minimal implementation checklist

- [x] Add `Dialect` to `GuardPolicy` (config `guard.dialect: "clickhouse"`).
- [x] Fix `ALTER … DELETE/UPDATE` (write) vs `ALTER`/`OPTIMIZE` (DDL) in
      `guard.rs`, unit-tested (`classify_with`).
- [x] Per-server dialect: `McpServerConfig.dialect` (inferred from name/command
      when unset). The `router` tracks each server's `Dialect` and the agent
      classifies every call in the owning server's dialect
      (`router.dialect_for_tool`), so a mixed pg+ch setup gates each correctly.
- [x] `Backend: <server>` tag in the spec format; `materialize` honors it
      (`Backend:`/`Engine:`/`Order by:` spec lines → `semantics::Backend`).
      (`verify` still routes to the first SQL tool; per-server routing is above.)
- [x] Backend-aware DDL in `pipeline.rs`: ClickHouse specs emit an incremental
      `CREATE MATERIALIZED VIEW … ENGINE = … ORDER BY … POPULATE AS`; Postgres
      specs keep the `WITH DATA` + `REFRESH` pair. Unit-tested.
- [x] `verify` routes each spec to a SQL tool on the server matching its
      `Backend` (`router.sql_tool_for_dialect`), with a fallback to any SQL tool
      for untagged single-server setups. Output labels the tool used.
- [x] `config.example.json`: a two-server (pg + ch) block
      (`_mcp_servers_pgch_example`), plus `config.pgch.mock.json` for the
      zero-setup demo.
- [x] A mock ClickHouse server (`src/bin/mock_ch_server.rs`) exposing
      `run_select_query` with canned rollup rows, so the full Postgres +
      ClickHouse demo runs offline. Covered by `mcp_e2e.rs`.
