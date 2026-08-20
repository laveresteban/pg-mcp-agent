# Architecture

How pg-mcp-agent is put together, end to end. It is a **control-plane agent**: a
local LLM authors and validates SQL, a safety guard gates every call, a semantic
layer keeps the numbers honest, and one or more MCP servers run the actual data
plane (Postgres, ClickHouse, a data catalog).

## System overview

```mermaid
flowchart TB
    user([User / CI])

    subgraph agent["pg-mcp-agent (control plane)"]
        direction TB
        cli["main.rs — CLI<br/>repl · verify · init-specs<br/>materialize · cdc"]
        loop["agent.rs — the loop"]
        guard["guard.rs — SQL write-guard<br/>classify + policy, per-server dialect"]
        semantics["semantics.rs — semantic layer<br/>glossary + verified specs"]
        verifym["verify.rs — answer verifier<br/>re-derive figures"]
        analytics["analytics.rs — in-memory analytics<br/>(+ DataFusion op=sql)"]
        pipeline["pipeline.rs — materialize<br/>spec to MV DDL"]
        cdc["cdc.rs — CDC control plane<br/>plan + inspect"]
        audit["audit.rs — JSONL audit log"]
        router["router.rs — multi-server router<br/>merge tools, namespace, route by dialect"]
    end

    subgraph llm["Local model"]
        ollama["Ollama /api/chat<br/>(qwen2.5-coder)"]
    end

    subgraph servers["MCP servers (data plane)"]
        pg["Postgres MCP<br/>execute_sql"]
        ch["ClickHouse MCP<br/>run_select_query"]
        cat["Catalog MCP<br/>catalog_dump, glossary"]
    end

    pgdb[("Postgres<br/>OLTP, source of truth")]
    chdb[("ClickHouse<br/>OLAP, rollups")]

    user --> cli --> loop
    loop <-->|messages + tool defs| ollama
    loop --> guard
    loop --> analytics
    loop --> verifym
    semantics -.grounding.-> loop
    audit -.records.-> loop
    guard -->|allowed calls| router
    router --> pg & ch & cat
    pg --> pgdb
    ch --> chdb
    pipeline -.reads.-> semantics
    cdc -.inspects.-> router
    pgdb -->|WAL / logical replication| chdb

    classDef db fill:#1f2937,stroke:#60a5fa,color:#e5e7eb;
    class pgdb,chdb db;
```

The dashed **WAL** arrow is the CDC capture path: it runs *in the database layer*
(ClickHouse's `MaterializedPostgreSQL` engine consuming the Postgres WAL), not in
the agent. The agent only inspects and proposes it — see [CDC](#cdc-control-plane).

## The request loop

What happens on one user turn (`agent.rs::handle_user`):

```mermaid
sequenceDiagram
    autonumber
    actor U as User
    participant A as Agent loop
    participant O as Ollama
    participant G as Guard
    participant R as Router
    participant M as MCP server
    participant V as Answer verifier

    U->>A: "revenue by month"
    Note over A: system prompt = base +<br/>analytics cheatsheet + semantic layer
    loop up to max_steps
        A->>O: chat(messages, tool defs)
        O-->>A: tool_call execute_sql(SELECT …)
        A->>G: evaluate(args) in the tool's server dialect
        alt read
            G-->>A: Allow
        else write / DDL / unknown
            G-->>A: NeedsConfirmation / Blocked
            A->>U: confirm? (skipped under --yes)
        end
        A->>R: call_tool(execute_sql, args)
        R->>M: tools/call
        M-->>R: rows (JSON)
        R-->>A: text
        Note over A: cache tabular result for analyze_last_result
        A->>O: tool result
    end
    O-->>A: final prose answer
    A->>V: check figures against the data's aggregates
    V-->>A: flag hallucinated numbers (advisory)
    A-->>U: answer (+ any verifier warning)
```

Key invariant: **the guard classifies the SQL, not the tool name.** Postgres/
ClickHouse MCP servers funnel everything through one `execute_sql`/
`run_select_query` tool, so "allow the read tool only" cannot work.

## The write-guard state machine

```mermaid
flowchart TD
    start["tool call with SQL arg"] --> strip["strip comments,<br/>reject multi-statement"]
    strip --> classify{"leading keyword<br/>(+ dialect rules)"}
    classify -->|SELECT / EXPLAIN / read WITH| read["Read"]
    classify -->|INSERT/UPDATE/DELETE/COPY<br/>CH: ALTER…DELETE| write["Write"]
    classify -->|CREATE/ALTER/DROP/TRUNCATE<br/>CH: OPTIMIZE| ddl["DDL"]
    classify -->|multi-stmt / unknown| unknown["Unknown"]

    read --> rpol{confirm_reads?}
    rpol -->|no| allow["Allow — run"]
    rpol -->|yes| confirm["Confirm"]
    write --> wpol{allow_writes?}
    wpol -->|yes| confirm
    wpol -->|no| block["Blocked"]
    ddl --> dpol{allow_ddl?}
    dpol -->|yes| confirm
    dpol -->|no| block
    unknown --> confirm

    classDef ok fill:#064e3b,stroke:#34d399,color:#d1fae5;
    classDef bad fill:#7f1d1d,stroke:#f87171,color:#fee2e2;
    class allow ok;
    class block bad;
```

Dialect is **per server**: in a mixed pg+ch setup the router tells the guard which
backend owns each call (`router.dialect_for_tool`), so a ClickHouse `ALTER TABLE …
DELETE` is gated as a *write* (a row mutation), not blocked as Postgres DDL.

## Semantic layer & verification

Specs (`specs/*.spec.md`) are both **grounding** and **tests**:

```mermaid
flowchart LR
    spec["*.spec.md<br/>glossary + verified queries<br/>Backend / Engine / Order by"]
    spec -->|to_prompt| prompt["system prompt grounding"]
    spec -->|verify| ver["run each Expect:<br/>against the matching server"]
    spec -->|materialize| mv["backend-aware MV DDL"]
    catalog["Catalog MCP"] -->|init-specs| spec

    mv --> pgmv["Postgres:<br/>CREATE MATERIALIZED VIEW … WITH DATA<br/>+ REFRESH"]
    mv --> chmv["ClickHouse:<br/>CREATE MATERIALIZED VIEW …<br/>ENGINE = … ORDER BY … POPULATE"]
```

Each spec carries a `Backend:` tag; `verify` routes it to a SQL tool on the
matching server (`router.sql_tool_for_dialect`) and `materialize` emits the DDL
shape for that engine.

## CDC control plane

`cdc.rs` proposes and validates the Postgres → ClickHouse capture path — it never
runs it. Two commands:

```mermaid
flowchart LR
    subgraph plan["cdc plan (print-only)"]
        cfg["config.cdc<br/>target_db, source, tables"] --> ddl["MaterializedPostgreSQL<br/>setup DDL + prerequisites"]
    end
    subgraph inspect["cdc inspect (read-only)"]
        q1["SELECT current_setting('wal_level')"] --> chk["check_wal_level"]
        q2["SELECT … FROM pg_replication_slots"] --> an["analyze_slots<br/>(active? lag?)"]
        chk & an --> report["ReplicationReport<br/>healthy / issues"]
    end
```

The capture itself (ClickHouse consuming the WAL) runs in the database layer;
the agent authors the DDL and watches slot health. Analytical rollups on the
replicated tables are then generated with `materialize` (specs tagged
`Backend: clickhouse`).

## Module responsibilities

| Module | Responsibility |
|--------|----------------|
| `main.rs` | CLI: `repl`, `verify`, `init-specs`, `materialize`, `cdc plan/inspect` |
| `agent.rs` | the loop: model ↔ tools ↔ guard ↔ execute; result caching |
| `guard.rs` | classify SQL (read/write/DDL/unknown) + policy; per-dialect rules |
| `router.rs` | connect N MCP servers, merge/namespace tools, route by dialect |
| `semantics.rs` | parse specs; grounding prompt; `verify` against the DB |
| `pipeline.rs` | `materialize`: verified spec → backend-aware MV DDL |
| `cdc.rs` | Postgres→ClickHouse replication plan + health inspection |
| `verify.rs` | answer verifier: re-derive figures, flag hallucinations |
| `analytics.rs` | in-memory result-set analytics (+ DataFusion `op=sql`) |
| `catalog.rs` | data-catalog MCP server + client (glossary/lineage grounding) |
| `audit.rs` | JSONL audit log of every tool call + guard decision |
| `mcp.rs` / `ollama.rs` | MCP stdio JSON-RPC client / Ollama `/api/chat` client |
| `knowledge.rs` | always-on analytical-Postgres cheatsheet in the prompt |

See [tutorial.md](tutorial.md) for hands-on, zero-setup examples of each command.
