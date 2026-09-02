import React, { useState, useRef, useEffect } from 'react'
import Pipeline from './Pipeline.jsx'

const SAMPLES = [
  'Revenue by month',
  'Top 10 films by revenue',
  'Revenue by category',
  'Top 10 customers by spend',
  'Revenue by store',
  'Most rented films',
]

let TURN_ID = 0

export default function App() {
  const [prompt, setPrompt] = useState('')
  const [turns, setTurns] = useState([])
  const [busy, setBusy] = useState(false)
  const bottomRef = useRef(null)

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth', block: 'end' })
  }, [turns])

  function updateTurn(id, patch) {
    setTurns((ts) =>
      ts.map((t) => (t.id === id ? (typeof patch === 'function' ? patch(t) : { ...t, ...patch }) : t)),
    )
  }

  async function ask(text) {
    const q = (text ?? prompt).trim()
    if (!q || busy) return
    setPrompt('')
    setBusy(true)
    const id = ++TURN_ID
    setTurns((ts) => [
      ...ts,
      { id, prompt: q, events: [], steps: [], answer: null, flags: [], model: null, done: false, error: null },
    ])

    try {
      const res = await fetch('/api/ask/stream', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ prompt: q }),
      })
      if (!res.ok || !res.body) throw new Error(`HTTP ${res.status}`)

      const reader = res.body.getReader()
      const decoder = new TextDecoder()
      let buf = ''
      for (;;) {
        const { done, value } = await reader.read()
        if (done) break
        buf += decoder.decode(value, { stream: true })
        let sep
        while ((sep = buf.indexOf('\n\n')) >= 0) {
          const chunk = buf.slice(0, sep)
          buf = buf.slice(sep + 2)
          const data = chunk
            .split('\n')
            .filter((l) => l.startsWith('data:'))
            .map((l) => l.slice(5).trim())
            .join('')
          if (!data) continue
          let ev
          try {
            ev = JSON.parse(data)
          } catch {
            continue
          }
          applyEvent(id, ev)
        }
      }
    } catch (e) {
      updateTurn(id, { error: String(e), done: true })
    } finally {
      setBusy(false)
    }
  }

  function applyEvent(id, ev) {
    updateTurn(id, (t) => {
      const next = { ...t, events: [...t.events, ev] }
      switch (ev.type) {
        case 'step':
          if (ev.step) next.steps = [...t.steps, ev.step]
          break
        case 'answer':
          next.answer = ev.answer
          next.flags = ev.flags || []
          next.model = ev.model
          break
        case 'error':
          next.error = ev.message
          break
        case 'done':
          next.done = true
          break
        default:
          break
      }
      return next
    })
  }

  return (
    <div className="app">
      <header>
        <h1>pg-mcp-agent</h1>
        <p className="tag">
          Watch your question flow through the model, the SQL guard, and Postgres — live.
        </p>
      </header>

      {turns.length === 0 && (
        <div className="samples">
          <span className="samples-label">Try:</span>
          {SAMPLES.map((s) => (
            <button key={s} className="chip" onClick={() => ask(s)}>
              {s}
            </button>
          ))}
        </div>
      )}

      <div className="thread">
        {turns.map((turn) => (
          <Turn key={turn.id} turn={turn} />
        ))}
        <div ref={bottomRef} />
      </div>

      <form
        className="composer"
        onSubmit={(e) => {
          e.preventDefault()
          ask()
        }}
      >
        <input
          autoFocus
          value={prompt}
          placeholder="Ask about films, rentals, revenue, customers…"
          onChange={(e) => setPrompt(e.target.value)}
          disabled={busy}
        />
        <button type="submit" disabled={busy || !prompt.trim()}>
          {busy ? '…' : 'Ask'}
        </button>
      </form>
    </div>
  )
}

function Turn({ turn }) {
  return (
    <div className="turn">
      <div className="bubble user">{turn.prompt}</div>
      <div className="bubble agent">
        <Pipeline turn={turn} />

        {turn.error && <div className="turn-error">Request failed: {turn.error}</div>}

        {turn.answer != null && (
          <div className="answer-block">
            <div className="answer-label">Answer</div>
            <div className="answer-text">{turn.answer || '(no answer)'}</div>
            {turn.flags?.length > 0 && (
              <div className="flags">
                ⚠ Figures the verifier couldn't match to the data: {turn.flags.join(', ')}. Check
                them against the rows below.
              </div>
            )}
            {turn.model && <div className="meta">model: {turn.model}</div>}
          </div>
        )}

        {turn.steps.map((step, i) => (
          <StepCard key={i} step={step} n={i + 1} />
        ))}

        <ActivityFeed events={turn.events} done={turn.done} />
      </div>
    </div>
  )
}

const FEED = {
  received: (e) => ['📥', `Received your question`, 'user'],
  thinking: (e) => ['🧠', `Model · round ${e.round} — ${e.label}`, 'model'],
  tool_proposed: (e) => ['📝', `Model proposed SQL (tool: ${e.tool})`, 'model'],
  guard: (e) =>
    e.status === 'allow'
      ? ['🛡️', `Guard: classified read-only — approved`, 'guard']
      : ['🚫', `Guard: ${e.status}${e.reason ? ' — ' + e.reason : ''}`, 'guard'],
  executing: (e) => ['🗄️', `Executing on Postgres via MCP…`, 'db'],
  rows: (e) => ['✅', `Postgres returned ${e.row_count} row(s) · [${(e.columns || []).join(', ')}]`, 'db'],
  tool_error: (e) => ['❌', `Query error: ${e.error}`, 'db'],
  verifying: (e) => ['🔍', e.label, 'model'],
  answer: (e) => ['💬', `Answer ready`, 'user'],
  error: (e) => ['❌', e.message, 'model'],
  done: () => null,
}

function ActivityFeed({ events, done }) {
  const [open, setOpen] = useState(true)
  const lines = events.map((e, i) => ({ i, r: FEED[e.type]?.(e) })).filter((x) => x.r)

  return (
    <div className="feed">
      <button className="feed-head" onClick={() => setOpen((o) => !o)}>
        <span className="chevron">{open ? '▾' : '▸'}</span>
        Activity log <span className="feed-count">({lines.length})</span>
        {!done && <span className="live-dot" />}
      </button>
      {open && (
        <ul className="feed-list">
          {lines.map(({ i, r }) => (
            <li key={i} className={`feed-item ${r[2]}`}>
              <span className="feed-icon">{r[0]}</span>
              <span className="feed-text">{r[1]}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

function StepCard({ step, n }) {
  const [open, setOpen] = useState(step.status === 'ok')
  const label = { ok: 'ok', error: 'error', blocked: 'blocked', declined: 'declined' }[step.status] || step.status

  return (
    <div className={`step ${step.status}`}>
      <button className="step-head" onClick={() => setOpen((o) => !o)}>
        <span className={`badge ${step.status}`}>{label}</span>
        <span className="step-title">
          Step {n} · {step.tool}
          {step.row_count > 0 && <span className="rowcount"> · {step.row_count} rows</span>}
        </span>
        <span className="chevron">{open ? '▾' : '▸'}</span>
      </button>
      {open && (
        <div className="step-body">
          {step.sql && (
            <pre className="sql">
              <code>{step.sql}</code>
            </pre>
          )}
          {step.reason && <div className="reason">Guard: {step.reason}</div>}
          {step.error && <div className="reason">Error: {step.error}</div>}
          {step.columns?.length > 0 && (
            <ResultTable columns={step.columns} rows={step.rows} total={step.row_count} />
          )}
        </div>
      )}
    </div>
  )
}

function ResultTable({ columns, rows, total }) {
  const shown = rows.slice(0, 100)
  return (
    <div className="table-wrap">
      <table>
        <thead>
          <tr>
            {columns.map((c) => (
              <th key={c}>{c}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {shown.map((row, i) => (
            <tr key={i}>
              {row.map((cell, j) => (
                <td key={j}>{formatCell(cell)}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      {total > shown.length && (
        <div className="truncated">
          showing first {shown.length} of {total} rows
        </div>
      )}
    </div>
  )
}

function formatCell(v) {
  if (v === null || v === undefined) return '∅'
  if (typeof v === 'object') return JSON.stringify(v)
  return String(v)
}
