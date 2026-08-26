# Cross-engine parity — the keystone of the defensible square

The market research ([market-research-2026.md](market-research-2026.md)) located
one square that no competitor occupies at once:

> **self-hosted + local-LLM + SQL-aware guard + verified semantic layer, across
> both Postgres AND ClickHouse.**

Cross-engine **parity** is the feature that only that square can ship. It turns
"we span both engines" from a plumbing fact into a correctness guarantee.

## The problem it solves

Teams run Postgres as the system of record and ClickHouse for analytics (the
GitLab/Cloudflare/Instacart/LangChain pattern). The recurring pain, in the words
of the 2026 research: **numbers disagree across tools, and metrics drift between
dashboards.** When revenue is computed on the OLTP source *and* on a ClickHouse
rollup, the two can silently diverge — a bad rollup definition, a late/partial
CDC load, a units mismatch.

Everyone else stops short of catching this:

- **ClickPipes / PeerDB / Debezium** move rows into ClickHouse. They guarantee
  delivery, not that `SUM(revenue)` on the rollup equals `SUM(revenue)` on the
  source.
- **dbt tests** run within one warehouse; they don't reach back to compare a
  Postgres source against a ClickHouse target.
- **Cloud NL-to-SQL** answers a question on one database at a time.

Parity is the missing assertion: *the rollup still equals the source.*

## How it works

A metric becomes a parity metric by tagging two specs with the same `Parity:`
key — one per backend:

````markdown
## Metric: total revenue (Postgres source)
Backend: postgres
Parity: total revenue
```sql
SELECT SUM(quantity * unit_price) AS revenue_total FROM order_items;
```

## Metric: total revenue (ClickHouse rollup)
Backend: clickhouse
Parity: total revenue
```sql
SELECT sum(quantity * unit_price) AS revenue_total FROM order_items;
```
````

`pg-mcp-agent parity <config>`:

1. Groups specs by their `Parity:` key ([src/semantics.rs](../src/semantics.rs),
   `verify_parity`).
2. Runs each member on the SQL tool of the server whose **dialect** matches its
   backend (`router.sql_tool_for_dialect`) — the same routing `verify` uses. The
   Postgres query hits the Postgres server; the ClickHouse query hits ClickHouse.
3. Extracts a scalar from each result ([src/parity.rs](../src/parity.rs),
   `extract_number` — tolerant of JSON/CSV/bare-number output, and it skips
   numbers embedded in dates or identifiers).
4. Compares every member pairwise by **relative** difference against a tolerance
   (`DEFAULT_TOLERANCE = 1e-6`), so float round-trips don't cause false alarms
   but a real 1% divergence does.
5. Prints a `MATCH`/`DIFF` report and **exits non-zero on any mismatch**, so it
   gates in CI beside `verify`.

A group with fewer than two parseable values fails loudly — a parity check you
can't actually evaluate is never reported as a pass.

## Why it lives only in this project's square

- **Spans both engines** — needs the multi-server MCP router
  ([src/router.rs](../src/router.rs)) that already merges tool lists and tracks
  each server's dialect. A single-database agent structurally can't do this.
- **One verified semantic layer** — parity is an assertion *on top of* the spec
  layer, reusing the same execution path as `verify`. Without a shared semantic
  layer there's nothing to hang the assertion on.
- **Self-hosted / local** — it reads from both production engines directly; a SaaS
  would need both databases shipped to it. Here nothing leaves the network.

## Design notes & extensions

- **Scalar-first, by design.** v1 compares one number per side (the common case:
  a total, a count, a ratio). It's deterministic and demo-able offline against the
  bundled mocks (`config.pgch.mock.json` + `specs-demo/parity.spec.md`, where the
  ClickHouse rollup 1200+1550+1830 = 4580 equals the Postgres source total).
- **Natural extensions** (not yet built), in leverage order:
  1. **Keyed row-set parity** — compare a small grouped result (e.g. revenue *by
     day*) key-by-key, not just one grand total. Catches a divergence in one
     partition that a total would mask.
  2. **Tolerance per group** — a `Tolerance:` spec line for metrics where a tiny
     approximation is acceptable (e.g. HLL-based distinct counts in ClickHouse).
  3. **Freshness-aware parity** — allow the rollup to lag the source by a bounded
     window before flagging (ties into `cdc inspect` lag data).
  4. **`verify --parity`** — fold parity into the main verify gate so one CI step
     covers both per-engine correctness and cross-engine agreement.
- **Guard interaction:** parity queries are read-shaped `SELECT`s and run freely
  under the default policy; nothing here writes.

## Tests

Pure logic in `src/parity.rs` is unit-tested (number extraction incl. the
date/identifier edge cases, relative-diff matching, unparseable-member failure,
report counts/rendering). The end-to-end offline path is exercised by
`pg-mcp-agent parity config.pgch.mock.json` returning a green MATCH.
