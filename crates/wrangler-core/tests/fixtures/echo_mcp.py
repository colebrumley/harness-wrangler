#!/usr/bin/env python3
"""A minimal MCP server over stdio for tests: one tool, `echo`, that returns its text reversed."""
import json, sys

def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method, rid = req.get("method"), req.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": rid, "result": {"protocolVersion": req["params"].get("protocolVersion", "2024-11-05"), "capabilities": {"tools": {}}, "serverInfo": {"name": "echo", "version": "1.0.0"}}})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": rid, "result": {"tools": [{"name": "reverse", "description": "Reverse a string. Returns the characters of `text` in reverse order.", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}}]}})
    elif method == "tools/call":
        text = req["params"].get("arguments", {}).get("text", "")
        send({"jsonrpc": "2.0", "id": rid, "result": {"content": [{"type": "text", "text": text[::-1]}]}})
    elif rid is not None:
        send({"jsonrpc": "2.0", "id": rid, "result": {}})
