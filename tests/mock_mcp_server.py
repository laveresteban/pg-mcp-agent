#!/usr/bin/env python3
import sys
import json

# Simple MCP mock server speaking newline-delimited JSON-RPC 2.0 on stdio.
# Handles: initialize, notifications/initialized, tools/list, tools/call.

def write_msg(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except Exception:
        # ignore non-JSON lines
        continue
    # notifications have no id
    msg_id = msg.get("id")
    method = msg.get("method")
    params = msg.get("params", {})

    if method == "initialize":
        if msg_id is not None:
            write_msg({"jsonrpc": "2.0", "id": msg_id, "result": {}})
        continue
    if method == "notifications/initialized":
        # no response for notifications
        continue
    if method == "tools/list":
        if msg_id is not None:
            res = {
                "tools": [
                    {"name": "echo", "description": "Echo tool", "inputSchema": {"type": "object"}},
                    {"name": "error", "description": "Erroring tool"}
                ]
            }
            write_msg({"jsonrpc": "2.0", "id": msg_id, "result": res})
        continue
    if method == "tools/call":
        if msg_id is None:
            continue
        name = params.get("name")
        arguments = params.get("arguments", {})
        if name == "echo":
            content = [{"type":"text","text": f"echoed: {arguments.get('msg','')}"}]
            write_msg({"jsonrpc":"2.0","id": msg_id, "result": {"isError": False, "content": content}})
            continue
        if name == "error":
            content = [{"type":"text","text": "oops"}]
            write_msg({"jsonrpc":"2.0","id": msg_id, "result": {"isError": True, "content": content}})
            continue
        # default: return a generic text
        content = [{"type":"text","text": "ok"}]
        write_msg({"jsonrpc":"2.0","id": msg_id, "result": {"isError": False, "content": content}})

# EOF
