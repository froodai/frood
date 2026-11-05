#!/usr/bin/env python3
"""`openclaw mcp serve` for tests: MCP over stdio, the same tools and shapes as OpenClaw's
bridge (2026.10). Events come from a file a test appends to; messages go to another file."""

import json
import sys
import threading
import time

events_path, sent_path = sys.argv[1], sys.argv[2]
write_lock = threading.Lock()


def answer(request_id, result):
    with write_lock:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}) + "\n")
        sys.stdout.flush()


def events_wait(arguments):
    after = arguments.get("after_cursor", 0)
    deadline = time.time() + min(arguments.get("timeout_ms", 30000) / 1000, 30)
    while time.time() < deadline:
        with open(events_path) as file:
            for line in file:
                event = json.loads(line)
                if event["cursor"] > after:
                    return {"event": event}
        time.sleep(0.02)
    return {}


def messages_send(arguments):
    if arguments["session_key"] == "no-route":
        raise LookupError("Conversation not found for session no-route")
    with write_lock, open(sent_path, "a") as file:
        file.write(json.dumps(arguments, ensure_ascii=False) + "\n")
    return {"result": {"ok": True}}


TOOLS = {"events_wait": events_wait, "messages_send": messages_send}


def call(request_id, params):
    try:
        structured = TOOLS[params["name"]](params.get("arguments") or {})
        answer(request_id, {"content": [{"type": "text", "text": "ok"}], "structuredContent": structured})
    except Exception as err:  # the MCP SDK turns a tool's exception into an error result
        answer(request_id, {"content": [{"type": "text", "text": str(err)}], "isError": True})


for raw in sys.stdin:
    message = json.loads(raw)
    method = message.get("method")
    if method == "initialize":
        answer(message["id"], {
            "protocolVersion": message["params"]["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fake-openclaw", "version": "0"},
        })
    elif method == "tools/call":
        threading.Thread(target=call, args=(message["id"], message["params"]), daemon=True).start()
    elif method == "ping":
        answer(message["id"], {})
