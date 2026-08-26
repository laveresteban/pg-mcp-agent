# dbt MCP integration — design note

**Why this is now the top ecosystem feature:** the 2026 dbt Semantic Layer
benchmark is the measured winner for NL→SQL correctness (near-100% on covered
queries vs 84–90% raw — see [market-research-2026.md](market-research-2026.md)).
dbt Labs ships an **official MCP server** (`dbt-mcp`, PyPI/GitHub `dbt-labs/dbt-mcp`),
and pg-mcp-agent is already a multi-server MCP client. So grounding on the layer
the market already trusts is mostly **config + grounding wiring**, not new protocol
work. It complements our `*.spec.md` layer rather than replacing it — specs stay the
git-native option for teams without dbt.

## The dbt MCP tool surface (Semantic Layer)

| Tool | Returns | Use in our loop |
|------|---------|-----------------|
| `list_metrics` | all defined metrics | inject into the system prompt as grounding (like our glossary) |
| `get_dimensions` (per metric) | valid group-bys | constrain what the model can slice by |
| `get_dimension_values` | distinct values of a dimension | validate filters / enum grounding |
| `get_entities` | entities for metrics | join/scope awareness |
| `get_metrics_compiled_sql` | compiled SQL, **not executed** | feed our `verify` and `materialize` (view DDL) |
| `query_metrics` | filtered/grouped metric result | **preferred answer path** — governed, no free-form NL2SQL |

## Integration plan (leanest first)

1. **Connect it as another MCP server** (zero code): add a `dbt` entry to the
   `mcp_servers` array in config. `router.rs` already merges tool lists and
   namespaces collisions (`dbt__query_metrics`). The dbt tools appear alongside the
   Postgres/ClickHouse ones immediately.
2. **Semantic-layer grounding from `list_metrics`** (small): at startup, if a dbt
   server is connected, call `list_metrics` + `get_dimensions` and fold the result
   into the semantic-layer prompt injection (`semantics::to_prompt`), the same way
   `catalog_dump` grounds `init-specs`. This is the "auto-populate the semantic
   layer from real metadata" item from the ecosystem roadmap — dbt is a stronger
   source than `init-specs` guesses.
3. **Semantic-layer-first routing** (the roadmap's item B2): for a metric-shaped
   question, prefer `query_metrics` over free-form NL2SQL. A miss becomes an honest
   "no matching metric" refusal instead of a confident-wrong number — exactly the
   failure-mode swap the benchmark rewards.
4. **`verify` / `materialize` on compiled SQL** (medium): `get_metrics_compiled_sql`
   gives governed SQL we can (a) run through the answer verifier and (b) turn into
   backend-aware MATERIALIZED VIEW DDL via `pipeline.rs`. This closes the loop:
   dbt defines the metric → we validate it against the live DB → we emit the
   Postgres/ClickHouse rollup DDL. Fits the control-plane framing perfectly.

## Guard interaction

`query_metrics` and `get_metrics_compiled_sql` are read-shaped; the guard's
`extract_sql` won't find a raw SQL arg on most of them, so they fall through to the
"metadata/introspection → treat as read" branch (`guard.rs`). Confirm this is the
desired behavior when wiring — a governed metric query should run freely; only the
`materialize`-generated DDL goes through the write/DDL gate as today.

## Open questions for the implementing session

- dbt MCP needs dbt Cloud creds or a local dbt project + MetricFlow; decide which
  the demo targets. A **mock dbt server** (mirroring `mock_ch_server.rs`) would keep
  the zero-setup demo working offline and is probably worth it.
- Where the dbt metric list lives relative to `*.spec.md` — merge into one semantic
  layer, or keep two sources with dbt taking precedence? Recommend: one merged
  layer, dbt metrics win on name collision (they're the governed source of truth).

## Sources

- [How the dbt MCP Server connects AI to trusted data — dbt Labs](https://www.getdbt.com/blog/mcp)
- [Available tools — dbt Developer Hub](https://docs.getdbt.com/docs/dbt-ai/mcp-available-tools)
- [dbt-labs/dbt-mcp — GitHub](https://github.com/dbt-labs/dbt-mcp)
- [Semantic Layer vs. Text-to-SQL: 2026 Benchmark Update — dbt](https://docs.getdbt.com/blog/semantic-layer-vs-text-to-sql-2026)
