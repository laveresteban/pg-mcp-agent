# Developer guide

Everything you need to be productive in this repo. The guiding idea: **you can do
almost all development offline** — bundled mock MCP servers stand in for Postgres
and ClickHouse, so you rarely need a real database or even a running model.

- [Quick start](#quick-start)
- [Toolchain (read this once)](#toolchain-read-this-once)
- [Command reference](#command-reference)
- [The mock-first workflow](#the-mock-first-workflow)
- [Project layout](#project-layout)
- [Common tasks](#common-tasks)
- [Testing](#testing)
- [CI gates & reproducing them locally](#ci-gates--reproducing-them-locally)
- [The spec-triage agent](#the-spec-triage-agent)
- [Conventions](#conventions)
- [Troubleshooting](#troubleshooting)

## Quick start

```bash
git clone <repo> && cd pg-mcp-agent
cargo build          # builds the agent + mock servers + catalog server
cargo demo           # verify demo metrics against mock Postgres + ClickHouse
```

`cargo demo` should print two green `PASS` lines. That is the whole loop —
semantic layer, dialect routing, two backends — running with zero setup.

## Toolchain (read this once)

This project needs **rustup stable (1.97+)**. [`rust-toolchain.toml`](../rust-toolchain.toml)
pins the channel, so the rustup `cargo` selects the right version automatically.

> ⚠️ **Windows PATH gotcha.** rustup was installed with `--no-modify-path`, so a
> bare `cargo` in a fresh shell may resolve to an **older standalone install**
> (`C:\Program Files\Rust stable MSVC 1.85`). Always use the rustup cargo:
> `C:\Users\<you>\.cargo\bin\cargo.exe` (bash: `"$HOME/.cargo/bin/cargo.exe"`).
> To fix it permanently, put `%USERPROFILE%\.cargo\bin` at the front of PATH.

Nothing else is required to build. Ollama and a database are only needed for the
live REPL (see [the mock-first workflow](#the-mock-first-workflow)).

## Command reference

Cargo aliases ([`.cargo/config.toml`](../.cargo/config.toml)) wrap the common
tasks — they need nothing beyond cargo:

| Alias | Runs | Purpose |
|-------|------|---------|
| `cargo demo` | `run -- verify config.pgch.mock.json` | offline pg+ch metric verification |
| `cargo triage-json` | `run -- verify --json …` | structured output for the triage agent |
| `cargo mat` | `run -- materialize config.pgch.mock.json` | print backend-aware MV DDL |
| `cargo cdc-plan` | `run -- cdc plan config.pgch.mock.json` | print Postgres→ClickHouse CDC DDL |
| `cargo lint` | `clippy --all-targets -- -D warnings` | lint exactly as CI does |

The underlying CLI (see `cargo run -- --help`):

```
pg-mcp-agent [config.json]                 interactive REPL
pg-mcp-agent verify [--json] [config]      run specs against the DB(s)
pg-mcp-agent init-specs [config]           generate a starter spec from the schema
pg-mcp-agent materialize [config]          print MATERIALIZED VIEW DDL from specs
pg-mcp-agent cdc plan|inspect [config]     CDC setup DDL / replication health
  -y/--yes   -p/--prompt <text>   --audit-log <path>   --no-verify   --json
```

## The mock-first workflow

Two bundled binaries speak the real MCP stdio protocol so the whole agent runs
without external services:

- [`mock_mcp_server`](../src/bin/mock_mcp_server.rs) — a *Postgres* server
  (`execute_sql`) returning canned sales rows; it also answers the CDC health
  queries (`wal_level`, `pg_replication_slots`) so `cdc inspect` demos offline.
- [`mock_ch_server`](../src/bin/mock_ch_server.rs) — a *ClickHouse* server
  (`run_select_query`) returning a canned daily rollup.

[`config.pgch.mock.json`](../config.pgch.mock.json) wires both (tagged with their
`dialect`) and points at [`specs-demo/`](../specs-demo/demo.spec.md). This is what
`cargo demo` and CI use. Only the interactive REPL needs Ollama:

```bash
ollama pull qwen2.5-coder && ollama serve
cargo run -- config.pgch.mock.json          # REPL
cargo run -- --prompt "sales by region" config.pgch.mock.json   # one-shot
```

## Project layout

See [architecture.md](architecture.md) for the diagrams and full module table.
The short version:

- **The loop** — `agent.rs` (model ↔ tools ↔ guard ↔ execute).
- **Safety** — `guard.rs` (classify SQL + policy), `audit.rs` (log), `verify.rs`
  (answer verifier).
- **Correctness** — `semantics.rs` (specs → grounding + `verify`),
  `knowledge.rs` (cheatsheet).
- **Multi-backend** — `router.rs` (connect/route N servers by dialect),
  `pipeline.rs` (`materialize`), `cdc.rs` (replication control plane).
- **Plumbing** — `mcp.rs`, `ollama.rs`, `config.rs`, `catalog.rs`.
- **Offline** — `src/bin/mock_*_server.rs`, `tests/mcp_e2e.rs`.

## Common tasks

### Add a metric (spec)
Create or edit a `*.spec.md` in the `specs_dir`. Give it a `## Metric: <name>`
heading, `Question:` phrasings, an `Expect:` assertion, an optional `Backend:`
(+ `Engine:`/`Order by:` for ClickHouse), and a fenced ```sql block. Then:

```bash
cargo run -- verify <config>        # check it against the DB(s)
```

See [tutorial.md](tutorial.md#2-writing-a-spec). To bootstrap from a live schema,
`cargo run -- init-specs <config>`.

### Add an MCP server
Add an entry to `mcp_servers` in the config with `command`, `args`, optional
`env`, `name`, and `dialect` (`postgres` | `clickhouse`). The router merges tool
lists and namespaces collisions as `<name>__<tool>`. No code change needed —
that is the point of the multi-server design.

### Add a CLI flag
Flags live in `Cli::parse` in [`main.rs`](../src/main.rs); add the field, parse
it, thread it into the relevant `run_*`, and add a parser unit test.

## Testing

```bash
cargo test                    # unit + integration (default features)
cargo test --features datafusion   # includes the DataFusion op=sql engine
cargo test <name>             # a single test by substring
```

- **Unit tests** live in each module behind `#[cfg(test)]`. Pure logic
  (classification, routing, DDL/CDC generation, report rendering) is tested here.
- **Integration tests** ([`tests/mcp_e2e.rs`](../tests/mcp_e2e.rs)) drive the real
  stdio transport against the mock servers — the part unit tests can't cover.
- **TDD is the norm here.** New behavior lands with a failing test first; see the
  `cdc.rs`, `pipeline.rs`, and `semantics.rs` test modules for the style.

## CI gates & reproducing them locally

[`.github/workflows/ci.yml`](../.github/workflows/ci.yml) runs, and you can run
the same four locally before pushing:

```bash
cargo fmt --all --check                  # formatting
cargo lint                               # clippy -D warnings (alias)
cargo test && cargo test --features datafusion
cargo demo                               # verify metrics (exits non-zero on a bad spec)
```

A green local run of those four means CI will pass.

## The spec-triage agent

When `verify` fails, the **spec-triage agent** diagnoses each failing metric and
proposes a fix. It is defined in [`.claude/agents/spec-triage.md`](../.claude/agents/spec-triage.md)
and reads the structured output of `cargo run -- verify --json`.

- **Locally (Claude Code):** ask for it by name — e.g. "use the spec-triage agent
  to triage config.pgch.mock.json". It runs the JSON verifier, reads the failing
  specs, and prints a per-failure diagnosis + proposed edit. It is read-only.
- **In CI:** [`.github/workflows/spec-triage.yml`](../.github/workflows/spec-triage.yml)
  runs it on PRs when metrics fail and posts the diagnosis as a PR comment. It is
  opt-in — set an `ANTHROPIC_API_KEY` repo secret to enable it; otherwise it
  no-ops. The red `verify` gate in `ci.yml` stays authoritative.

To see it work, introduce a bad `Expect:` in a demo spec, run `cargo triage-json`,
and note the failing entry carries the SQL, the assertion, and the actual output —
everything the agent needs. Other proposed CI/CD agents are in
[cicd-agents.md](cicd-agents.md).

## Conventions

- **Match the surrounding code** — comment density, naming, module docs. Each
  module starts with a `//!` doc explaining its job and the design reason.
- **Keep the default build light.** Heavy analytics stays behind the `datafusion`
  feature. Don't add dependencies to the default build without a strong reason.
- **Gate on the SQL, not the tool name** — the core safety invariant. If you
  touch `guard.rs`, keep the classifier + policy tests green and add cases.
- **Generators are print-only.** `materialize` and `cdc plan` emit SQL for review;
  they never apply it. Preserve that.
- **`serde_json` `preserve_order` is on** so result columns keep query order.

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `cargo` builds an old Rust (1.85) | Use the rustup cargo; see [Toolchain](#toolchain-read-this-once). |
| `no MCP server configured` | Pass a config with `mcp_server`/`mcp_servers`, e.g. `config.pgch.mock.json`. |
| REPL never calls tools | The Ollama model must support tool calls (e.g. `qwen2.5-coder`). |
| `op=sql needs the datafusion feature` | Rebuild with `--features datafusion`. |
| `verify` fails on the mock | Expected if a spec's `Expect:` doesn't match the canned rows; the mock is illustrative, not a real DB. |
| CDC `inspect` says "no replication slots" | The real DB has no CDC set up; run `cdc plan` to generate the setup DDL. |

More context: [architecture.md](architecture.md) · [tutorial.md](tutorial.md) ·
[../CLAUDE.md](../CLAUDE.md) (deep project notes).
