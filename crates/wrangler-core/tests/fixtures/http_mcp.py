#!/usr/bin/env python3
"""A minimal MCP server over HTTP for tests: one tool, `shout`, that upper-cases its text.

Usage: http_mcp.py <port>. Answers JSON-RPC POSTs with plain JSON (no SSE stream).
"""
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def _send(self, code, body=None, headers=None):
        data = json.dumps(body).encode() if body is not None else b""
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        # No server-initiated stream.
        self._send(405)

    def do_DELETE(self):
        self._send(200)

    def do_POST(self):
        req = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))) or b"{}")
        method, rid = req.get("method"), req.get("id")
        if rid is None:
            return self._send(202)
        if method == "initialize":
            result = {"protocolVersion": req["params"].get("protocolVersion", "2025-03-26"), "capabilities": {"tools": {}}, "serverInfo": {"name": "web", "version": "1.0.0"}}
            return self._send(200, {"jsonrpc": "2.0", "id": rid, "result": result}, {"Mcp-Session-Id": "test-session"})
        if method == "tools/list":
            tool = {"name": "shout", "description": "Upper-case a string. Returns `text` in capital letters.", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}}
            return self._send(200, {"jsonrpc": "2.0", "id": rid, "result": {"tools": [tool]}})
        if method == "tools/call":
            text = req["params"].get("arguments", {}).get("text", "")
            return self._send(200, {"jsonrpc": "2.0", "id": rid, "result": {"content": [{"type": "text", "text": text.upper()}]}})
        self._send(200, {"jsonrpc": "2.0", "id": rid, "result": {}})


HTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
