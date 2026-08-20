# CLAUDE.md — pg-mcp-agent

Context for future sessions working on this project. Read before making changes.

## What this is

A Rust REPL agent where a **local Ollama model** answers questions and makes
changes against Postgres by driving an **existing Postgres MCP server**. Every
tool call is gated: the agent inspects the SQL and either runs it, asks for
confirmation, or blocks it. On top of that sits a semantic layer (markdown specs)
and a local analytics engine for NL→SQL insight work.

```
Ollama (local LLM)  <-- /api/chat + tool calls -->  pg-mcp-agent (+ guard, semantic layer, analytics)
                                                          |  MCP (JSON-RPC over stdio)
                                                     Postgres MCP server  <-- SQL -->  Postgres
```

This is a standalone project, unrelated to the resumes-cover-letters repo it
happens to sit near. It is NOT a git repository yet.

## Toolchain (important)

- This project uses **rustup** stable (currently **1.97.1**), installed at the
  user level under `%USERPROFILE%\.cargo` and `%USERPROFILE%\.rustup`.
- rustup was installed with `--no-modify-path`, so the **system PATH still points
  at an older standalone install** (`C:\Program Files\Rust stable MSVC 1.85`).
  That means a bare `cargo` in a fresh shell may still be 1.85.
- **Always build this project with the rustup cargo:**
  `C:\Users\Esteban\.cargo\bin\cargo.exe` (bash: `"$HOME/.cargo/bin/cargo.exe"`).
- [`rust-toolchain.toml`](rust-toolchain.toml) pins the channel to `stable`, so
  the rustup cargo automatically selects 1.97+ here.
- To make the new toolchain the machine-wide default later: add
  `%USERPROFILE%\.cargo\bin` to the front of PATH (or uninstall the Program Files
  1.85 install, which needs admin). Not required for building this project.

## Build / test / run

```bash
CARGO="$HOME/.cargo/bin/cargo.exe"

$CARGO test                          # default build, unit tests
$CARGO test --features datafusion    # includes the DataFusion engine + its tests
$CARGO build --release

# Run the agent
$CARGO run -- [config.json]                 # interactive REPL
$CARGO run -- verify [config.json]          # run specs against the DB (CI-friendly, exit-nonzero on failure)
$CARGO run -- init-specs [config.json]      # generate specs/generated.spec.md from information_schema
$CARGO run -- --yes                         # auto-approve guarded writes (non-interactive)
$CARGO run -- --prompt "revenue by month"   # one-shot: run a single request and exit (implies --yes)
$CARGO run --features datafusion -- ...      # enable op=sql analytical SQL
```

CLI flags live in `Cli::parse` in [src/main.rs](src/main.rs): `-y/--yes`,
`-p/--prompt <text>`, `verify` subcommand, optional config path, `-h/--help`.

## Module map

| File | Responsibility |
|------|----------------|
| [src/mcp.rs](src/mcp.rs) | MCP stdio client (JSON-RPC: initialize, tools/list, tools/call). `parse_tools_from_value`, `extract_text` are unit-tested. |
| [src/router.rs](src/router.rs) | Connects MULTIPLE MCP servers, merges tool lists, namespaces colliding names (`<server>__<tool>`), routes calls. `build_routes` unit-tested; routing integration-tested with two mock servers. |
| [src/ollama.rs](src/ollama.rs) | Ollama `/api/chat` client with tool-calling. Args come back as JSON objects, not strings. |
| [src/guard.rs](src/guard.rs) | Classifies a SQL statement (read/write/DDL/unknown) and applies the policy. Gates on the SQL, not the tool name. Unit-tested. |
| [src/audit.rs](src/audit.rs) | JSONL audit log of every tool call (tool, args, guard decision, status). Config `audit_log`. Unit-tested. |
| [src/verify.rs](src/verify.rs) | Answer verifier: recomputes aggregates and flags hallucinated figures in the model's prose. Config `verify_answers` (default on). Unit-tested. |
| [src/knowledge.rs](src/knowledge.rs) | Always-on analytical-Postgres cheatsheet injected into the system prompt. |
| [src/semantics.rs](src/semantics.rs) | Parses `specs/*.spec.md` into a semantic layer (glossary + verified examples). Injects grounding; `verify` runs specs against the DB and returns a `VerifyReport` (`--json` feeds the spec-triage agent). |
| [src/analytics.rs](src/analytics.rs) | Built-in dependency-free result-set analytics (describe/group_by/top) + the `analyze_last_result` tool schema. |
| [src/analytics_datafusion.rs](src/analytics_datafusion.rs) | Optional DataFusion engine (`op=sql`): loads the last result set into an Arrow table `t` and runs analytical SQL. Feature-gated. |
| [src/specgen.rs](src/specgen.rs) | `init-specs`: turns an information_schema introspection result into a starter `.spec.md`. Pure `generate_spec` is unit-tested. |
| [src/pipeline.rs](src/pipeline.rs) | `materialize`: turns verified specs into backend-aware MATERIALIZED VIEW DDL — Postgres CREATE/REFRESH or ClickHouse ENGINE/POPULATE (prints only, never applies). Unit-tested. |
| [src/cdc.rs](src/cdc.rs) | CDC control plane: `cdc plan` prints the direct `MaterializedPostgreSQL` setup DDL; `cdc plan --via kafka` prints the Debezium→Kafka→ClickHouse fan-out (connector JSON + CH Kafka-engine DDL); `cdc inspect` parses `pg_replication_slots` + (fan-out) `system.kafka_consumers` and flags health. Pure fns unit-tested. See [docs/cdc-fanout.md](docs/cdc-fanout.md). |
| [src/agent.rs](src/agent.rs) | The loop: MCP tools + local analyze tool → Ollama → guard → execute. Caches the last tabular result. `Console` for stdin prompts. |
| [src/config.rs](src/config.rs) | JSON config. |
| [src/lib.rs](src/lib.rs) | Library surface exposing all modules (so tests + the mock server reuse them). |
| [src/main.rs](src/main.rs) | Thin CLI over the lib: parsing, REPL, one-shot `--prompt`, `--yes`, verify. |
| [src/bin/mock_mcp_server.rs](src/bin/mock_mcp_server.rs) | In-memory *Postgres* MCP server (canned rows, `execute_sql`) for offline runs + integration tests. |
| [src/bin/mock_ch_server.rs](src/bin/mock_ch_server.rs) | In-memory *ClickHouse* MCP server (canned daily rollup, `run_select_query`, dialect `clickhouse`) so the full pg+ch demo runs offline. `config.pgch.mock.json` wires both mocks. |
| [src/catalog.rs](src/catalog.rs) + [src/bin/catalog_mcp_server.rs](src/bin/catalog_mcp_server.rs) | A data-catalog MCP server: serves tables/columns/lineage/glossary from a JSON file. `init-specs` grounds specs on it when connected (via the `catalog_dump` tool). |
| [tests/mcp_e2e.rs](tests/mcp_e2e.rs) | End-to-end stdio-transport tests against the mock server. |

## Key design decisions (don't regress these)

- **Gate on the SQL statement, not the tool name.** Postgres MCP servers usually
  expose one `execute_sql`/`query` tool, so the guard extracts the SQL argument
  and classifies it. It strips comments and rejects multi-statement input so a
  write can't hide behind `SELECT 1; DROP …` or a comment. Policy: reads run
  silently, writes confirm, DDL blocked by default, unknown/multi → confirm.
- **Semantic layer over raw NL→SQL.** Research is clear that the failure mode is
  confident WRONG numbers, not syntax, and a semantic layer is the fix. Specs in
  `specs/*.spec.md` are both grounding (glossary + verified queries injected into
  the prompt) and tests (`verify` checks each `Expect:` against the live DB).
  See [specs/orders.spec.md](specs/orders.spec.md) for the format.
- **Two analytics engines behind one tool.** `analyze_last_result` runs over the
  rows the last query returned, in memory. Built-in engine (default) does
  describe/group_by/top. DataFusion engine (feature) adds `op=sql` for arbitrary
  analytical SQL (window functions, joins) over table `t`.
- **serde_json `preserve_order` is on** so result columns keep query order.

## Dependency notes / history

- `idna_adapter` is pinned to 1.1.0 in Cargo.lock (a leftover from the 1.85 days,
  when newer transitive deps needed rustc 1.88). On 1.97 the pin is harmless and
  can be dropped with `cargo update` if desired.
- DataFusion was previously pinned to `=45.0.0` + `comfy-table =7.1.1` to build on
  rustc 1.85. After upgrading to 1.97 those pins were dropped; the feature now
  uses current DataFusion (50.x). If you downgrade the toolchain again, you'll
  need those pins back.

## Ollama setup (DONE this session)

- Installed: Ollama 0.32.13 at `%LOCALAPPDATA%\Programs\Ollama\ollama.exe`;
  server runs on `http://localhost:11434` (already up).
- Model pulled: **`qwen2.5-coder`** (4.7 GB, tool-capable). Set as `ollama.model`
  in the config.
- The agent talks to `/api/chat` with `tools`; a model without tool support will
  never call the MCP tools.

## Offline testing without Postgres

- `src/bin/mock_mcp_server.rs` is a self-contained MCP server that returns canned
  sales rows over the real stdio protocol. `config.mock.json` points at it.
- End-to-end run (no DB): `cargo run --bin pg-mcp-agent -- --prompt "total sales by region" config.mock.json`
- `tests/mcp_e2e.rs` drives the real `McpClient` against this mock (spawned via
  `CARGO_BIN_EXE_mock_mcp_server`), covering the stdio transport the in-module
  unit tests can't.
- Two bin targets exist now (`pg-mcp-agent`, `mock_mcp_server`), so `Cargo.toml`
  sets `default-run = "pg-mcp-agent"` — a bare `cargo run` still starts the agent.

## Postgres MCP server + config

- Copy `config.example.json` to `config.json` and edit.
- Server options:
  - Writes: `crystaldba/postgres-mcp` via `uvx` (needs `uv`); exposes
    `execute_sql`.
  - Read-only, no `uv`: `npx -y @modelcontextprotocol/server-postgres <conn>`.
- `mcp_server.env.DATABASE_URI` (or the conn-string arg) points at Postgres.
- `guard.allow_writes` / `allow_ddl` / `confirm_reads` set the policy.
- `specs_dir` (default `specs`) is where `*.spec.md` files live.

## Working norms for this repo

- A **second AI agent has been adding tests** here in parallel. It refactored
  `mcp.rs` (added `parse_tools_from_value`) and may add files. Use targeted edits,
  re-read before editing, and don't overwrite files you didn't write.
- Keep the default build dependency-light; heavy analytics stays behind the
  `datafusion` feature.
- Persistent project memory (state, research, decisions) lives in the user's
  auto-memory: `pg-mcp-agent-project` and `nl-to-sql-tips`.

## Roadmap / research

[docs/roadmap-ecosystem-pipelines.md](docs/roadmap-ecosystem-pipelines.md) is the
research-backed plan for growing this: complementary MCP servers to connect
(DuckDB, dbt, data catalogs, Kafka, object storage), agent-architecture moves
(a deterministic verifier pass, semantic-layer-first routing), data-pipeline
patterns (CDC, ELT, materialized views, reverse ETL), and security hardening
(audit log, least-privilege role, prompt-injection via tool output). The gating
change for most of it is multi-MCP-server support in config.

## Open next steps

- DONE: `init-specs`, answer verifier (`verify.rs`), audit log (`audit.rs`), and
  **multi-MCP-server support** (`router.rs`; config `mcp_servers` array or single
  `mcp_server`). The ecosystem servers (DuckDB, dbt, catalogs) can now be added
  by config alone.
- Next on the roadmap: catalog-driven specs (pull glossary/lineage from a catalog
  MCP to supersede `init-specs` guesses); materialized-view spec type.
- Stream model output (currently prints when a turn completes).
- Consider making rustup's toolchain the machine default (PATH change / remove
  the 1.85 standalone install).
