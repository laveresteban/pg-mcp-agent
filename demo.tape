# vhs tape — regenerate a GIF of the 60-second offline demo.
# Install vhs (https://github.com/charmbracelet/vhs), then:  vhs demo.tape
# Produces docs/assets/demo.gif. The animated SVG in the README (docs/assets/demo.svg)
# is the zero-dependency default; this GIF is the higher-fidelity alternative.

Output docs/assets/demo.gif

Set Shell "bash"
Set FontSize 15
Set Width 1100
Set Height 640
Set Theme "Github Dark"
Set Padding 18
Set TypingSpeed 45ms

Hide
Type "export CARGO=$HOME/.cargo/bin/cargo.exe && $CARGO build -q" Enter
Wait
Show

Type "# 1. Verify metrics against mock Postgres AND mock ClickHouse — offline, no LLM"
Enter
Type "$CARGO run -q -- verify config.pgch.mock.json" Enter
Wait
Sleep 2s

Type "# 2. Prove the same metric computes to the SAME number on both engines"
Enter
Type "$CARGO run -q -- parity config.pgch.mock.json" Enter
Wait
Sleep 3s

Type "# one metric, proven equal on Postgres + ClickHouse — self-hosted, no LLM"
Enter
Sleep 2s
