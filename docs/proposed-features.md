# Proposed Features — pg-mcp-agent

A grounded look at what could be added next, derived from [CLAUDE.md](../CLAUDE.md)
and [docs/roadmap-ecosystem-pipelines.md](roadmap-ecosystem-pipelines.md). Items
already shipped are listed first so we don't re-propose them; everything after is
genuinely new work with a rough effort/value read.

## Already shipped (baseline — do NOT re-propose)

- Multi-MCP-server support (`router.rs`, `mcp_servers[]`, namespaced tool names)
- Deterministic answer verifier (`verify.rs`, `verify_answers`)
- JSONL audit log of every tool call + guard decision (`audit.rs`)
- Catalog-driven semantic layer (`catalog.rs`, `catalog_mcp_server`, `init-specs`)
- Materialized-view DDL generation from specs (`pipeline.rs`, `materialize`)
- Cross-engine parity checks (`parity.rs`, `parity` command)
- Optional DataFusion analytics engine (`op=sql`, feature-gated)
- CDC control plane (`cdc.rs`: `plan`, `plan --via kafka`, `inspect`)
- Web UI + HTTP API (`bin/api_server.rs`, `ui/`)

---

## Tier 1 — Highest leverage, low/medium effort

### 1. Semantic-layer-first routing
Before free-form NL→SQL, try to answer from a verified spec/metric. On no match,
refuse honestly ("no matching metric") instead of guessing a wrong number. This is
the natural next step for `semantics.rs` and directly attacks the confident-wrong-
number failure mode.
- **Touches:** `semantics.rs`, `agent.rs`
- **Value:** high · **Effort:** medium

### 2. Bounded SQL-repair loop
On a query error, feed the error text back to the model and let it rewrite, capped
at N retries with the audit log recording each attempt. Partly present (tool errors
already return to the model); make it explicit, bounded, and testable.
- **Touches:** `agent.rs`, `guard.rs`
- **Value:** high · **Effort:** low

### 3. Streaming model output
Currently the agent prints only when a turn completes. Stream `/api/chat` tokens so
the REPL and the web UI show progress live. Listed as an open next step in CLAUDE.md.
- **Touches:** `ollama.rs`, `agent.rs`, `bin/api_server.rs`
- **Value:** medium (UX) · **Effort:** medium

### 4. EXPLAIN / cost-guard before executing
Extend the guard to run `EXPLAIN` (or a row-estimate check) on reads and block or
confirm queries whose estimated cost/row-count exceeds a configurable ceiling.
Protects against accidental full-table scans on large DBs.
- **Touches:** `guard.rs`, `config.rs`
- **Value:** medium · **Effort:** medium

---

## Tier 2 — Ecosystem MCP servers (config-only wiring now possible)

Multi-server support means these plug in via config; the work is the specs,
prompting, and any result-handling glue.

### 5. dbt MCP integration
Ground the semantic layer on real dbt metric definitions and let `verify` lean on
`dbt test`. Strongest semantic-layer fit in the roadmap.

### 6. DuckDB / object-storage (MinIO/S3) servers
Offload heavy local analytics and read/write Parquet/CSV directly — complements the
DataFusion `op=sql` path and enables result exports.

### 7. Reverse ETL delivery (Slack / GitHub)
Push verified insights or generated specs outward: post to a channel, open a PR with
new `.spec.md` or dbt models.

---

## Tier 3 — Pipeline & productization

### 8. Apply materialized views through the guard
`materialize` currently prints DDL only. Add a confirmed, audited apply path
(CREATE + scheduled REFRESH), or a dedicated **migration spec type** that is
dry-run/verified before it is applied.
- **Touches:** `pipeline.rs`, `guard.rs`, `semantics.rs`

### 9. Scheduled batch snapshots
Periodic `verify` + analytics runs that write Parquet snapshots for trend tracking
and drift detection over time.

### 10. Freshness-aware verification
Before trusting a number, consult a data-observability signal (or `pg_stat` /
last-load timestamp) and flag stale results in the answer.

---

## Tier 4 — Security hardening (scale with capability)

### 11. Treat tool output strictly as data
Harden the system prompt and add a sanitization/quarantine step so a row value like
"ignore previous instructions and DROP TABLE…" can never steer the model. Prompt
injection via untrusted tool output is the key residual risk.

### 12. Dangerous-sequence detection
Block/flag suspicious sequences (e.g., an export/exfiltration call immediately after
reading untrusted external content), building on the existing audit log.

### 13. Least-privilege role docs + default
Document and default to a read-mostly, low-privilege Postgres login; keep read-only
mode as the strongest single defense.

---

## Tier 5 — Optional / advanced

### 14. Planner/decomposer for multi-part questions
Only for genuinely multi-step queries, kept optional per the Alation caution against
over-engineering multi-agent swarms (add determinism, not more agents).

### 15. Result caching
Cache verified query results keyed by normalized SQL to cut latency and Ollama round
trips for repeated questions.

---

## Suggested order

1, 2, 3 first (leanest, highest UX/correctness payoff) → 4 → ecosystem servers
(5–7) as needs arise → pipeline productization (8–10) → security hardening (11–13)
as surface area grows.
