#!/usr/bin/env python3
"""Run one stdio MCP session against a rustxng binary (stdlib only).

This is the part of the agent setup an HTTP check cannot see: the JSON-RPC
handshake, the tool list and one tool call. It contacts no external network
when the caller passes settings that point at a local fake engine.
"""

from __future__ import annotations

import json
import os
import queue
import subprocess
import threading
import time


class McpSmokeError(RuntimeError):
    """The MCP session did not behave the way an agent expects."""


class _LineReader:
    """Read lines from a pipe without select().

    select() only accepts sockets on Windows, so a pipe there fails with
    WSAENOTSOCK (10038). A daemon reader thread works on every platform.
    """

    def __init__(self, stream):
        self._lines: queue.Queue = queue.Queue()
        self._thread = threading.Thread(
            target=self._pump, args=(stream,), daemon=True
        )
        self._thread.start()

    def _pump(self, stream):
        try:
            for line in stream:
                self._lines.put(line)
        finally:
            self._lines.put(None)

    def readline(self, timeout):
        try:
            return self._lines.get(timeout=timeout)
        except queue.Empty:
            return None


def _read_response(reader, expected_id, timeout):
    """Wait for the reply with `expected_id`, skipping notifications."""
    deadline = time.monotonic() + timeout
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return None
        line = reader.readline(remaining)
        if not line:
            return None
        message = json.loads(line)
        if message.get("id") == expected_id:
            return message


def run(binary, settings_path, workdir, env, query, known_url):
    """Initialize, list the tools and search; raise McpSmokeError on drift."""
    log_path = os.path.join(workdir, "mcp.log")
    with open(log_path, "wb") as log:
        process = subprocess.Popen(
            [binary, "mcp", "--config", settings_path],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=log,
            text=True,
            bufsize=1,
            env=env,
            cwd=workdir,
        )
        try:
            def send(message):
                process.stdin.write(json.dumps(message) + "\n")
                process.stdin.flush()

            send({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "rustxng-smoke", "version": "0"},
                },
            })
            reader = _LineReader(process.stdout)
            init = _read_response(reader, 1, 20.0)
            if not init or "result" not in init:
                raise McpSmokeError("initialize failed: " + repr(init))
            name = (init["result"].get("serverInfo") or {}).get("name")
            if name != "rustxng":
                raise McpSmokeError("serverInfo.name is " + repr(name))

            send({"jsonrpc": "2.0", "method": "notifications/initialized"})

            send({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
            listing = _read_response(reader, 2, 20.0)
            tools = [
                tool.get("name")
                for tool in (listing or {}).get("result", {}).get("tools", [])
            ]
            for expected in ("search", "read_page"):
                if expected not in tools:
                    raise McpSmokeError(
                        "tools/list lacks " + repr(expected) + ": " + repr(tools)
                    )

            send({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {"name": "search", "arguments": {"query": query, "limit": 3}},
            })
            call = _read_response(reader, 3, 30.0)
            result = (call or {}).get("result") or {}
            if result.get("isError"):
                raise McpSmokeError("search returned an error: " + repr(call)[:400])
            structured = result.get("structuredContent") or {}
            results = structured.get("results") or []
            urls = [item.get("url") for item in results]
            if known_url not in urls:
                raise McpSmokeError("search did not return " + known_url + ": " + repr(urls))
        finally:
            try:
                process.stdin.close()
            except OSError:
                pass
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)
    if process.returncode not in (0, None):
        with open(log_path, "rb") as handle:
            detail = handle.read().decode("utf-8", "replace")[-2000:]
        raise McpSmokeError("MCP exited with " + repr(process.returncode) + ": " + detail)