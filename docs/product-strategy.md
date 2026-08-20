# Product strategy: a Postgres→ClickHouse pipeline copilot

Direction chosen: **open-source, portfolio-first**, wedge = **Postgres→ClickHouse
pipeline copilot**, first users = **small data teams already on Postgres +
ClickHouse**.

## One-liner

> An open-source, self-hosted copilot that turns plain-English metrics into
> verified ClickHouse analytics on top of your Postgres data — and keeps them
> fresh.

## The job to be done

Small teams on Postgres + ClickHouse hand-write and babysit the SQL that moves
and rolls up data: the CDC/replication setup, the ClickHouse materialized views,
the metric definitions that drift between dashboards. It's tedious, it breaks
silently, and the numbers disagree across tools. The copilot owns that loop:

1. You describe a metric in plain English (or point it at a catalog).
2. It writes the ClickHouse view/rollup SQL, grounded in the semantic layer.
3. It **verifies** the result against the data (no confident-wrong numbers).
4. It emits the DDL/pipeline you review and apply, and can keep it fresh.

The agent is the **control plane** — it authors and validates; the databases run
the data plane.

## Why this project, specifically

- **Private / self-hosted.** Runs against a local model; data never leaves the
  team's network. No "send your warehouse to our cloud."
- **Correctness is the moat.** Verified specs + answer verifier + catalog
  grounding. It refuses instead of hallucinating — the exact gap the research
  says cloud NL-to-SQL tools miss.
- **Semantic layer as the shared source of truth.** `*.spec.md` files are
  human-readable, version-controlled, and testable (`verify`). Metrics stop
  drifting between dashboards.
- **Deep stack fit.** Multi-server routing already connects Postgres +
  ClickHouse + a catalog. `materialize` already turns specs into view DDL.

## Ideal first users

Startups/scaleups who already run Postgres for OLTP and ClickHouse for analytics
(or are adopting ClickHouse), have 1–5 data/backend engineers, no dedicated
analytics-engineering team, and care about not shipping data to a SaaS. They feel
the "maintaining pipeline SQL by hand" pain acutely.

## Open-source strategy (portfolio-first)

- **MIT-licensed core.** The whole agent. Adoption and credibility over revenue.
- **Zero-setup trial.** The bundled `mock_mcp_server` and `catalog_mcp_server`
  let anyone run the full loop with no database — huge for a README demo and for
  "try it in 60 seconds."
- **Portfolio value.** This is a staff-level artifact: Rust, MCP protocol work,
  a real multi-server agent, a safety guard, a semantic-layer/verifier system,
  and thorough tests (74+). It demonstrates systems design, not just a toy. Link
  it prominently on the resume/LinkedIn; write it up (README + a short Show HN /
  blog post walking through the correctness design).
- **Monetize later, if ever.** Open-core team layer (shared catalog/specs, RBAC,
  audit dashboard, managed connectors) is the option, not the obligation.

## Concrete roadmap toward the wedge (from today)

Highest-leverage, in order:

1. ~~**ClickHouse dialect in the guard**~~ — DONE. `ALTER … DELETE/UPDATE` = write
   (not DDL), `OPTIMIZE` = maintenance (`guard::Dialect`). Now **per-server**: the
   router tags each server's dialect and the agent gates each call in the owning
   server's dialect (`router.dialect_for_tool`).
2. ~~**A mock ClickHouse server**~~ — DONE. `src/bin/mock_ch_server.rs`
   (`run_select_query`, canned daily rollup). `config.pgch.mock.json` wires mock
   Postgres + mock ClickHouse; the whole demo runs with zero external setup.
3. ~~**Backend-tagged specs + backend-aware `materialize`**~~ — DONE. `Backend:`
   (+ ClickHouse `Engine:`/`Order by:`) spec lines; `materialize` emits Postgres
   `REFRESH` MVs or ClickHouse incremental `ENGINE … POPULATE` MVs. `verify`
   routes each spec to the matching server (`router.sql_tool_for_dialect`).
4. **The end-to-end demo** — PARTLY DONE. `verify config.pgch.mock.json` already
   routes pg vs ch specs across two mock servers offline; `materialize` prints the
   dual-backend DDL. Remaining: a passing demo spec set + a README hero GIF.
5. **CDC control-plane helpers** — inspect replication (ClickHouse
   `MaterializedPostgreSQL`), generate the analytical views on top. (Next.)

## What NOT to build (protect the portfolio goal)

- No cloud SaaS control plane yet. No billing, no multi-tenant, no auth server.
- No broad connector zoo — stay Postgres + ClickHouse + catalog.
- No agent swarm. Keep the single lean loop + deterministic verifier.

## Names to consider

`pgch-copilot`, `rollup` (the copilot that builds your rollups), `warehouse-mate`,
`spec2view`, `columnar-copilot`. (The repo is `pg-mcp-agent`; a rename can wait
until the wedge is proven.)

## Success metrics (portfolio/OSS)

- A clean README with a 60-second zero-setup demo (GIF/asciinema).
- A written walkthrough of the correctness design (the differentiator).
- GitHub stars / a Show HN / a few design-partner conversations.
- Not revenue — reach and credibility.
