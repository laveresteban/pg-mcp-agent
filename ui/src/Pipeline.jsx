import React from 'react'

// The four stages a natural-language question flows through.
const NODES = [
  { key: 'user', icon: '💬', title: 'You', sub: 'NL question' },
  { key: 'model', icon: '🧠', title: 'Model', sub: 'Ollama · writes SQL' },
  { key: 'guard', icon: '🛡️', title: 'Guard', sub: 'classifies + gates SQL' },
  { key: 'db', icon: '🐘', title: 'Postgres', sub: 'via MCP server' },
]
const INDEX = Object.fromEntries(NODES.map((n, i) => [n.key, i]))

// Fold the event stream into a per-node status + a current-activity label.
function derive(turn) {
  const st = { user: 'idle', model: 'idle', guard: 'idle', db: 'idle' }
  const detail = { user: '', model: '', guard: '', db: '' }
  let active = 'user'
  let activity = 'Waiting…'

  for (const e of turn.events) {
    switch (e.type) {
      case 'received':
        st.user = 'done'
        active = 'user'
        activity = 'Question received'
        break
      case 'thinking':
        st.model = 'active'
        active = 'model'
        activity = e.label
        detail.model = `round ${e.round}`
        break
      case 'tool_proposed':
        st.model = 'done'
        st.guard = 'active'
        active = 'guard'
        activity = 'SQL written — handing it to the guard'
        break
      case 'guard':
        if (e.status === 'allow') {
          st.guard = 'done'
          active = 'guard'
          activity = 'Guard approved — read-only'
          detail.guard = 'read · approved'
        } else {
          st.guard = 'blocked'
          active = 'guard'
          activity = `Guard ${e.status}${e.reason ? ': ' + e.reason : ''}`
          detail.guard = e.status
        }
        break
      case 'executing':
        st.db = 'active'
        active = 'db'
        activity = 'Running the query on Postgres'
        break
      case 'rows':
        st.db = 'done'
        active = 'db'
        activity = `Postgres returned ${e.row_count} row(s)`
        detail.db = `${e.row_count} rows`
        break
      case 'tool_error':
        st.db = 'blocked'
        active = 'db'
        activity = 'Query error'
        break
      case 'verifying':
        st.model = 'active'
        active = 'model'
        activity = e.label
        break
      case 'answer':
        st.model = 'done'
        st.user = 'done'
        active = 'user'
        activity = 'Answer delivered'
        break
      case 'error':
        active = 'model'
        activity = e.message
        break
      default:
        break
    }
  }

  const busy = !turn.done
  if (turn.done && turn.error) activity = `Failed: ${turn.error}`
  return { st, detail, active, activity, busy }
}

export default function Pipeline({ turn }) {
  const { st, detail, active, activity, busy } = derive(turn)
  const activeIdx = INDEX[active]

  return (
    <div className="pipeline">
      <div className="pipe-track">
        {NODES.map((n, i) => (
          <React.Fragment key={n.key}>
            {i > 0 && <Edge left={st[NODES[i - 1].key]} right={st[n.key]} busy={busy} />}
            <Node
              node={n}
              status={st[n.key]}
              detail={detail[n.key]}
              pulsing={busy && i === activeIdx}
            />
          </React.Fragment>
        ))}
      </div>

      <div className={`pipe-status ${busy ? 'busy' : ''}`}>
        {busy ? <span className="spinner" /> : <span className="tick">✓</span>}
        <span className="pipe-activity">{activity}</span>
      </div>
    </div>
  )
}

function Node({ node, status, detail, pulsing }) {
  return (
    <div className={`node ${status} ${pulsing ? 'pulsing' : ''}`}>
      <div className="node-ring">
        <span className="node-icon">{node.icon}</span>
      </div>
      <div className="node-title">{node.title}</div>
      <div className="node-sub">{detail || node.sub}</div>
    </div>
  )
}

// A connector between two nodes. Flows (animated dashes) while an adjacent node
// is active; turns solid green once the downstream node is done.
function Edge({ left, right, busy }) {
  const flowing = busy && (left === 'active' || right === 'active')
  const cls = right === 'done' ? 'done' : right === 'blocked' ? 'blocked' : flowing ? 'flowing' : ''
  return (
    <div className={`edge ${cls}`}>
      <div className="edge-line" />
      {flowing && <div className="edge-dot" />}
    </div>
  )
}
