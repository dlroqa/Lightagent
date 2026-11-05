"""A stdio MCP server for the lifecycle checks that does not leave on its own.

  python stubborn_mcp.py <marker>

It answers `initialize`, `tools/list` and `ping`, and keeps running after its
standard input ends: only being stopped ends it. A check that still finds it
after its runtime has gone has found a process the runtime left behind, not
one that was merely slow to notice. The marker argument is not used except to
make the process findable by its command line.
"""

import json
import sys
import time


def send(message):
    sys.stdout.buffer.write(json.dumps(message).encode("utf-8") + b"\n")
    sys.stdout.buffer.flush()


def answer(message):
    method = message.get("method")
    if method == "initialize":
        version = (message.get("params") or {}).get("protocolVersion") or "2025-06-18"
        return {"protocolVersion": version, "capabilities": {"tools": {}},
                "serverInfo": {"name": "stubborn", "version": "1.0.0"}}
    if method == "tools/list":
        return {"tools": [{"name": "wait", "description": "Does nothing.",
                           "inputSchema": {"type": "object", "properties": {}}}]}
    if method == "ping":
        return {}
    return None


def main():
    for line in sys.stdin.buffer:
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if not isinstance(message, dict) or "id" not in message or "method" not in message:
            continue
        result = answer(message)
        if result is None:
            send({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32601, "message": "not supported"}})
        else:
            send({"jsonrpc": "2.0", "id": message["id"], "result": result})
    # The end of input: stay, on purpose.
    while True:
        time.sleep(60)


if __name__ == "__main__":
    main()
