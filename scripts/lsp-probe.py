#!/usr/bin/env python3
"""Drive the laravel-lsp binary against a real project and print the
diagnostics it publishes for one file — a verification harness for changes
that affect diagnostics, without needing Zed in the loop.

Usage:
    scripts/lsp-probe.py [PROJECT_ROOT] [RELATIVE_FILE] [DEADLINE_SECS]

Defaults probe the in-repo fixture:
    scripts/lsp-probe.py test-project resources/views/namespaced-component-test.blade.php

The client performs the handshake the server expects from Zed: it answers
server->client requests (with empty results), sends
workspace/didChangeConfiguration after initialized (the server defers its
background vendor scan until then), and re-sends the document every 20s so
validation re-runs as background scans land. The final publish before the
deadline is printed.
"""
import json
import os
import subprocess
import sys
import threading
import time

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BINARY = os.path.join(REPO_ROOT, "laravel-lsp/target/release/laravel-lsp")
ROOT = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 else os.path.join(REPO_ROOT, "test-project")
FILE = os.path.join(ROOT, sys.argv[2] if len(sys.argv) > 2
                    else "resources/views/namespaced-component-test.blade.php")
DEADLINE_SECS = int(sys.argv[3]) if len(sys.argv) > 3 else 300

if not os.path.exists(BINARY):
    sys.exit(f"binary not found: {BINARY} — run `cargo build --release` first")

proc = subprocess.Popen([BINARY], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                        stderr=open("/tmp/lsp-probe.log", "wb"))
killer = threading.Timer(DEADLINE_SECS, proc.kill)
killer.start()

write_lock = threading.Lock()


def send(msg):
    data = json.dumps(msg).encode()
    with write_lock:
        proc.stdin.write(b"Content-Length: %d\r\n\r\n%s" % (len(data), data))
        proc.stdin.flush()


def read_msg():
    headers = {}
    while True:
        line = proc.stdout.readline()
        if not line:
            return None
        if line == b"\r\n":
            break
        if b":" in line:
            key, value = line.decode().split(":", 1)
            headers[key.strip().lower()] = value.strip()
    length = int(headers.get("content-length", 0))
    if length == 0:
        return None
    return json.loads(proc.stdout.read(length))


send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
    "processId": os.getpid(),
    "rootUri": "file://" + ROOT,
    "capabilities": {"workspace": {"configuration": True}},
    "workspaceFolders": [{"uri": "file://" + ROOT, "name": os.path.basename(ROOT)}],
}})

content = open(FILE).read()
publishes = []
version = 1
stop_nudges = threading.Event()


def nudge():
    """Periodically re-send the document text to re-trigger validation."""
    global version
    while not stop_nudges.wait(20):
        version += 1
        try:
            send({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
                "textDocument": {"uri": "file://" + FILE, "version": version},
                "contentChanges": [{"text": content}]}})
        except Exception:
            return


while True:
    msg = read_msg()
    if msg is None:
        break
    if msg.get("id") == 1 and "result" in msg:
        send({"jsonrpc": "2.0", "method": "initialized", "params": {}})
        send({"jsonrpc": "2.0", "method": "workspace/didChangeConfiguration",
              "params": {"settings": {}}})
        send({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": {"uri": "file://" + FILE, "languageId": "blade",
                              "version": 1, "text": content}}})
        threading.Thread(target=nudge, daemon=True).start()
    elif "method" in msg and "id" in msg:
        # Server->client request: answer with an empty/null result so the
        # server never blocks awaiting a response.
        method = msg["method"]
        result = [None] * len(msg.get("params", {}).get("items", [])) \
            if method == "workspace/configuration" else None
        send({"jsonrpc": "2.0", "id": msg["id"], "result": result})
    elif msg.get("method") == "textDocument/publishDiagnostics":
        params = msg["params"]
        if params["uri"] == "file://" + FILE:
            publishes.append(params["diagnostics"])
            ts = time.strftime("%H:%M:%S")
            print(f"[{ts}] publish #{len(publishes)}: {len(params['diagnostics'])} diagnostics",
                  flush=True)

if publishes:
    diags = publishes[-1]
    print(f"--- final publish: {len(diags)} diagnostics ---")
    for d in diags:
        lines = d["message"].splitlines()
        line_no = d["range"]["start"]["line"] + 1
        detail = lines[1] if len(lines) > 1 else ""
        print(f"  line {line_no}: {lines[0]} | {detail}")
else:
    print("no publishDiagnostics received for target file before deadline "
          "(server log: /tmp/lsp-probe.log)")

stop_nudges.set()
killer.cancel()
proc.kill()
