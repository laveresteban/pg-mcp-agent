# Market research & positioning — August 2026

Fresh research on the market pg-mcp-agent sits in, what's been commoditized since
the wedge was chosen, and the sharpest positioning given today's landscape. Read
this alongside [product-strategy.md](product-strategy.md) — it updates that plan
where the ground has shifted. Sources at the bottom.

## TL;DR — what changed and what to do

1. **Both original theses are now strongly validated by 2026 data.** Postgres +
   ClickHouse is a mainstream, fast-growing stack; semantic-layer grounding is the
   measured fix for NL→SQL correctness.
2. **But the raw CDC plumbing has been commoditized by ClickHouse itself.**
   ClickPipes (built on the acquired PeerDB) is GA, "the best managed Postgres CDC
   in the market," 5× cheaper than external ETL, and Postgres→ClickHouse CDC has
   grown ~100× since the acquisition. **Do not position pg-mcp-agent as a CDC-setup
   generator** — that race is over and we'd lose it.
3. **The defensible ground is correctness + safety + self-hosting**, not pipes:
   - **Correctness** — the semantic layer / verifier (measured differentiator).
   - **Safety/governance** — MCP adoption is exploding while MCP security has *not*
     caught up (Gartner + independent scans). Our guard/audit/least-priv story is
     suddenly a headline feature, not a footnote.
   - **Self-hosted / local-LLM** — ClickPipes correctness+governance live in
     ClickHouse *Cloud*. Teams that can't ship data to a SaaS have no managed
     equivalent of what we do.
4. **Recommended repositioning:** lead with *"the correctness & safety layer for
   data agents on Postgres + ClickHouse"*; treat CDC as "works with your ClickPipes
   / your own replication — we validate and roll up what it delivers," not as a
   thing we set up for you.

## Thesis 1 — Postgres + ClickHouse is a real, growing stack ✅

- ClickHouse acquired **PeerDB** (2024) specifically because "countless teams pair
  Postgres with ClickHouse." PeerDB became the foundation of **ClickPipes**.
- The **Postgres CDC connector for ClickPipes is GA** (since May 2026) and CDC
  volume into ClickHouse has grown **~100×** since the acquisition.
- Named users of the exact pattern (Postgres = system of record, ClickHouse =
  real-time analytics): **GitLab, Cloudflare, Instacart**, and — explicitly "in the
  AI era" — **LangChain, LangFuse, Vapi**. The AI-observability crowd is adopting
  this stack, which overlaps with our ideal users.
- Takeaway: the stack bet is correct and if anything *safer* than when we chose it.
  The market is being actively grown by a well-funded incumbent — good for
  awareness, and it means our job is to be the piece **they don't sell to
  self-hosters**.

## Thesis 2 — Semantic layer is the measured fix for NL→SQL ✅

- **dbt Semantic Layer 2026 benchmark:** grounding lifts Claude Sonnet 4.6 from
  90.0% → **98.2%** and GPT-5.3-Codex from 84.1% → **100%** on covered queries;
  "near-100% for covered queries."
- Raw text-to-SQL on bare enterprise schemas sits around **40%** (toy datasets look
  fine, real schemas don't); grounding jumps it to **85–95%**.
- The failure-mode framing matches our pitch almost word-for-word: **"semantic-layer
  failures are typically refusals; text-to-SQL failures are confident wrong
  numbers."** That single sentence is our whole differentiator, now third-party
  validated. Use it verbatim (paraphrased) in the README hero and the Show HN.
- **Implication for the roadmap:** the dbt Semantic Layer is the benchmark *winner*
  and the reference metric format. A **dbt MCP integration** (already the top item
  on the ecosystem roadmap) is now the single highest-leverage feature — it lets us
  ground on the layer the market already trusts instead of only our `*.spec.md`.
  Position `*.spec.md` as the lightweight, git-native option for teams without dbt.

## Thesis 3 (NEW) — MCP security is an unmet, urgent need ✅

This wasn't a headline when the wedge was chosen; it is now.

- MCP is going enterprise fast: **~97M SDK downloads/month**, **10,000+ public
  servers**, ~41% of technical leaders report limited-to-broad production use.
- **Security has not kept up:** independent scans find a *majority* of public MCP
  servers carry exploitable risk; MCP "was built without any real access control
  model." Gartner (Apr 2026): by 2028, 25% of enterprise GenAI apps will hit 5+
  security incidents/year, up from 9% in 2025 — **tied explicitly to MCP.**
- pg-mcp-agent already does the things the market is now asking for: **gate on the
  SQL statement not the tool name**, reject multi-statement/comment-hidden writes,
  **JSONL audit log** of every call + guard decision, least-privilege DB role,
  "tool output is data not instructions." This is a *governance control plane in
  front of data MCP servers* — a second, credible wedge.
- **Action:** elevate the safety story to co-headline. Add a one-page
  `docs/security-model.md` (threat model + the OWASP-LLM / MCP-cheatsheet mapping)
  and a README section "Safe by construction." This is also the most portfolio-
  legible angle for a security-conscious hiring audience.

## Competitive map (who does what)

| Player | Does | Where we differ |
|--------|------|-----------------|
| **ClickPipes / PeerDB (ClickHouse Cloud)** | Managed pg→CH CDC, GA, cheap | Cloud-only; not a correctness/semantic layer; not self-hostable; no NL authoring |
| **dbt Semantic Layer** | The winning grounding layer; metrics-as-code | Not an agent; no guard; needs dbt Cloud/adapter; we can *consume* it via MCP |
| **Cloud NL-to-SQL (Atlan, etc.)** | Text-to-SQL + context layer | SaaS, sends schema/data out; our bet is local-LLM + self-hosted |
| **Generic MCP gateways/proxies** | Auth/routing for MCP | Not data-aware; don't classify SQL or verify numbers |

Our unoccupied square: **self-hosted + local-LLM + SQL-aware guard + verified
semantic layer, spanning Postgres and ClickHouse.** Nobody else is standing on all
four at once.

## Positioning language to adopt

- Headline candidate: *"The correctness & safety layer for AI on your Postgres +
  ClickHouse — self-hosted, local LLM, no confident-wrong numbers."*
- Keep the control-plane framing ("we author & validate; the databases run the data
  plane") — it now doubles as the answer to "aren't you just reinventing
  ClickPipes?" (No — ClickPipes is a data-plane pipe; we're the control plane that
  decides *what* to build and proves it's right.)
- Retire any copy that implies we *move* the data or *replace* CDC tooling.

## Go-to-market / launch tactics (OSS, portfolio-first)

- **Show HN** format: `Show HN: pg-mcp-agent – a self-hosted copilot that verifies
  its own SQL on Postgres + ClickHouse`. Post **8–9am ET**; a strong Show HN drives
  5k–15k repo views and hundreds of stars in a day.
- What's trending in Rust-CLI land in 2026: **single binary, zero-dependency,
  LLM-adjacent** tools (the token-saving proxy at 73k stars, lazygit-style TUIs).
  Lean into "one Rust binary, runs offline, drives your local Ollama."
- **The 60-second zero-setup demo is still the #1 missing asset** (asciinema/GIF of
  `cargo demo`). It's the highest-ROI unshipped thing for launch — a Show HN
  without a GIF underperforms badly.
- Pick a **LICENSE** before any public launch (README already says "open-source";
  default is all-rights-reserved without one). MIT or `MIT OR Apache-2.0` per the
  portfolio-first strategy.
- Secondary channels: `r/dataengineering`, the ClickHouse and dbt communities (ride
  the incumbent's awareness), a short write-up of the correctness design (the
  benchmark numbers above make a compelling blog spine).

## What to build next (re-ranked by this research)

1. **60-sec demo GIF + LICENSE** — launch blockers, tiny effort. (Was already the
   remaining hero item.)
2. **`docs/security-model.md` + README "Safe by construction" section** — converts
   existing guard/audit code into a headline; rides the MCP-security wave; strong
   portfolio signal. Low effort, high leverage.
3. **dbt MCP integration** — ground on the market's trusted semantic layer; now the
   top *feature* on the ecosystem roadmap given the benchmark result.
4. **Positioning refresh in README + product-strategy.md** — reframe CDC as
   "validate what ClickPipes/your replication delivers," add the competitive map.
5. Everything else on the ecosystem roadmap (DuckDB, reverse-ETL) stays lower.

## Sources

- [Postgres CDC in ClickHouse, a year in review — ClickHouse](https://clickhouse.com/blog/postgres-cdc-year-in-review-2025)
- [ClickHouse acquires PeerDB — ClickHouse](https://clickhouse.com/blog/clickhouse-acquires-peerdb-to-boost-real-time-analytics-with-postgres-cdc-integration)
- [Postgres CDC connector for ClickPipes is GA — ClickHouse](https://clickhouse.com/blog/postgres-cdc-connector-clickpipes-ga)
- [PostgreSQL + ClickHouse as the OSS unified data stack — ClickHouse](https://clickhouse.com/blog/postgres-clickhouse-oss)
- [Semantic Layer vs. Text-to-SQL: 2026 Benchmark Update — dbt](https://docs.getdbt.com/blog/semantic-layer-vs-text-to-sql-2026)
- [Text-to-SQL for Enterprise: Metric Drift and Context Layer — Atlan](https://atlan.com/know/ai-agent/data-for-ai/text-to-sql-for-enterprise/)
- [Why Semantic Layers Make Enterprise Text-to-SQL Safer — Datalakehouse Hub](https://datalakehousehub.com/blog/2026-05-semantic-layers-text-to-sql/)
- [Semantic Layers for Reliable LLM-Powered Data Analytics (paired benchmark) — arXiv](https://arxiv.org/pdf/2604.25149)
- [2026: The Year for Enterprise-Ready MCP Adoption — CData](https://www.cdata.com/blog/2026-year-enterprise-ready-mcp-adoption)
- [Why MCP is suddenly on every executive agenda — CIO](https://www.cio.com/article/4136548/why-model-context-protocol-is-suddenly-on-every-executive-agenda.html)
- [The Evolution of MCP — security control surface 2026 — Innovate Cybersecurity](https://innovatecybersecurity.com/news/mcp-security-control-surface/)
- [Open Source as a Growth Engine (2026) — DEV](https://dev.to/alexcloudstar/open-source-as-a-growth-engine-how-developers-are-using-github-to-build-profitable-businesses-in-2k82)
- [11 Rust CLI Tools Every Developer Should Know in 2026 — Repotoire](https://www.repotoire.com/blog/rust-cli-tools-2026)
