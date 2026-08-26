#!/usr/bin/env python3
"""Generate docs/assets/demo.svg — a self-contained, looping animated SVG of the
offline pg+ClickHouse demo. No external tools: renders on GitHub via <img>, using
SMIL so it animates without JS or a player.

The frames mirror the REAL output of:
    cargo run -- verify     config.pgch.mock.json
    cargo run -- materialize config.pgch.mock.json
Regenerate with:  python scripts/gen_demo_svg.py
"""
from pathlib import Path
from html import escape

# ---- terminal geometry ----
FS = 15            # font size (px)
LH = 21            # line height (px)
CW = 8.4           # monospace char width (px)
PAD_X = 18
PAD_TOP = 44       # room for the title bar
PAD_BOT = 16
HEADER_H = 30

# ---- palette (GitHub dark) ----
BG      = "#0d1117"
BAR     = "#161b22"
FG      = "#c9d1d9"
PROMPT  = "#58a6ff"   # $ and typed command
GREEN   = "#3fb950"   # PASS / passed
DIM     = "#8b949e"   # -- comments / meta
CYAN    = "#39c5cf"   # SQL keywords
YELLOW  = "#d29922"   # section headers

DOTS = [("#ff5f56", 0), ("#ffbd2e", 1), ("#27c93f", 2)]

# A line = (text, color). None color => FG.
# A frame reveals the lines added since the previous frame; earlier lines stay.
Frame = list

def L(text, color=None):
    return (text, color)

# Build the session as a growing list; frames mark reveal points.
FRAMES = [
    # frame 0 — verify command
    [
        L("$ pg-mcp-agent verify config.pgch.mock.json", PROMPT),
    ],
    # frame 1 — verify output: every metric checked against its own engine
    [
        L("Verifying 4 spec(s)", DIM),
        L("  PASS  sales by region (Postgres)", GREEN),
        L("  PASS  daily revenue rollup (ClickHouse)", GREEN),
        L("  PASS  total revenue (Postgres source)", GREEN),
        L("  PASS  total revenue (ClickHouse rollup)", GREEN),
        L("4 passed, 0 failed", GREEN),
    ],
    # frame 2 — parity command (the differentiator)
    [
        L(""),
        L("$ pg-mcp-agent parity config.pgch.mock.json", PROMPT),
    ],
    # frame 3 — the payoff: same metric, same number, both engines
    [
        L("Checking 1 parity group(s)", DIM),
        L("  MATCH  total revenue  (postgres 4580 == clickhouse 4580)", GREEN),
        L("1 matched, 0 differed", GREEN),
    ],
]

CAPTION = "one metric, proven equal on Postgres + ClickHouse  ·  offline, no LLM needed"

# ---- layout ----
all_lines = [ln for f in FRAMES for ln in f]
n_lines = len(all_lines) + 2  # +caption spacing
max_cols = max(len(t) for t, _ in all_lines + [(CAPTION, None)])
WIDTH = int(PAD_X * 2 + max_cols * CW)
HEIGHT = int(PAD_TOP + n_lines * LH + PAD_BOT)

# ---- animation timeline ----
TOTAL = 16.0       # seconds per loop
FADE = 0.35        # reveal fade (s)
HOLD_AT_END = 3.0  # linger on the full screen before looping
usable = TOTAL - HOLD_AT_END
# start time (fraction of TOTAL) for each frame
n_frames = len(FRAMES)
starts = [i * (usable / n_frames) for i in range(n_frames)]

def anim(begin_s):
    """SMIL opacity: hidden until begin, fade in, stay, reset at loop."""
    f0 = begin_s / TOTAL
    f1 = (begin_s + FADE) / TOTAL
    return (f'<animate attributeName="opacity" values="0;0;1;1" '
            f'keyTimes="0;{f0:.4f};{f1:.4f};1" dur="{TOTAL}s" '
            f'repeatCount="indefinite"/>')

# ---- emit ----
parts = []
parts.append(
    f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" '
    f'viewBox="0 0 {WIDTH} {HEIGHT}" font-family="ui-monospace,SFMono-Regular,Consolas,monospace">'
)
# window
parts.append(f'<rect x="0" y="0" width="{WIDTH}" height="{HEIGHT}" rx="8" fill="{BG}"/>')
parts.append(f'<rect x="0" y="0" width="{WIDTH}" height="{HEADER_H}" rx="8" fill="{BAR}"/>')
parts.append(f'<rect x="0" y="{HEADER_H-8}" width="{WIDTH}" height="8" fill="{BAR}"/>')
for color, i in DOTS:
    parts.append(f'<circle cx="{18+i*20}" cy="{HEADER_H/2}" r="6" fill="{color}"/>')
parts.append(
    f'<text x="{WIDTH/2}" y="{HEADER_H/2+4}" fill="{DIM}" font-size="12" '
    f'text-anchor="middle">pg-mcp-agent — 60s demo</text>'
)

# lines, grouped by frame for staggered reveal
y = PAD_TOP + LH
for fi, frame in enumerate(FRAMES):
    parts.append(f'<g opacity="0">{anim(starts[fi])}')
    for text, color in frame:
        if text:
            fill = color or FG
            parts.append(
                f'<text x="{PAD_X}" y="{y:.0f}" fill="{fill}" font-size="{FS}" '
                f'xml:space="preserve">{escape(text)}</text>'
            )
        y += LH
    parts.append('</g>')

# caption (always visible, at the bottom)
y += LH * 0.4
parts.append(
    f'<text x="{PAD_X}" y="{y:.0f}" fill="{YELLOW}" font-size="12" '
    f'xml:space="preserve">{escape("→ " + CAPTION)}</text>'
)

parts.append('</svg>')

out = Path(__file__).resolve().parent.parent / "docs" / "assets" / "demo.svg"
out.parent.mkdir(parents=True, exist_ok=True)
out.write_text("\n".join(parts), encoding="utf-8")
print(f"wrote {out}  ({WIDTH}x{HEIGHT}, {n_frames} frames, {TOTAL}s loop)")
