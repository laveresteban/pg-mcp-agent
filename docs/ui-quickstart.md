# Web UI quickstart (NL → SQL against Dockerized Pagila)

A React UI over the agent for testing natural-language questions: type English,
see the SQL the guard allowed, and read the result rows. Backed by a small HTTP
API (`api_server`) that reuses the same guard, semantic layer, Ollama loop, and
MCP router as the REPL.

```
Browser (React, :5173) ──/api/ask──▶ api_server (:7878) ──▶ Ollama + Postgres MCP ──▶ Docker Postgres (Pagila)
```

## 1. Start Postgres and load the public Pagila dataset

```bash
docker compose up -d postgres pagila-loader
docker compose logs -f pagila-loader     # wait for "done. Row counts:" then Ctrl-C
```

The loader downloads Pagila (~13 MB) the first time and loads it into a named
volume; later runs are instant. Optional web DB browser: `docker compose up -d
adminer` → http://localhost:8080 (System *PostgreSQL*, Server `postgres`,
user/pass/db all `pagila`).

## 2. Make sure Ollama is running with a tool-capable model

```bash
ollama serve            # if not already running
ollama pull qwen2.5-coder
```

## 3. Start the API server

```bash
CARGO="$HOME/.cargo/bin/cargo.exe"
$CARGO run --features server --bin api_server -- config.pagila.json
# → pg-mcp-agent API listening on http://127.0.0.1:7878
```

`config.pagila.json` uses the read-only `npx @modelcontextprotocol/server-postgres`
MCP server (no `uv` needed) and a read-only guard policy, so the UI can only run
SELECTs.

## 4. Run the UI

Dev mode (hot reload, proxies `/api` to :7878):

```bash
cd ui
npm install
npm run dev          # → http://localhost:5173
```

Or build once and let the API server serve it at http://localhost:7878:

```bash
cd ui && npm install && npm run build      # emits ui/dist
# api_server serves ui/dist as a fallback route
```

## What you'll see

A **live pipeline** — `You → Model → Guard → Postgres` — animates as your
question flows through it: the active stage pulses, connectors show data moving,
and a color-coded **activity log** narrates each hop (model writes SQL → guard
classifies it read-only and approves → Postgres returns N rows → answer
verified). Each SQL step expands to show the exact statement, a guard badge
(`ok` / `blocked` / `declined`), and the result table. Figures in the answer the
verifier can't match to the data are flagged as possible hallucinations.

This is driven by a streaming endpoint: `POST /api/ask/stream` emits
Server-Sent Events (`received`, `thinking`, `tool_proposed`, `guard`,
`executing`, `rows`, `step`, `verifying`, `answer`, `done`). `POST /api/ask`
still returns the whole transcript in one shot for scripting.

Try: *revenue by month*, *top 10 films by revenue*, *revenue by category*,
*who are our top customers by spend*. These are grounded by
[specs/pagila/pagila.spec.md](../specs/pagila/pagila.spec.md); run `pg-mcp-agent verify
config.pagila.json` to check the specs against the live DB.
