"""Manual probe for `spid3r lsp`: sends a scripted session and prints every
message the server sends back, verbatim. Usage: python lsp_probe.py"""
import json
import subprocess
import sys
import threading
import time

BIN = "target/debug/spid3r.exe"

SRC = "fn helper(a) {\n  return a;\n}\nfn main() {\n  helper(1);\n}\n"
URI = "file:///project/main.matrix"


def frame(obj):
    body = json.dumps(obj).encode()
    return b"Content-Length: %d\r\n\r\n" % len(body) + body


def main():
    p = subprocess.Popen([BIN, "lsp"], stdin=subprocess.PIPE,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    msgs = []

    def reader():
        while True:
            line = p.stdout.readline()
            if not line:
                break
            line = line.strip()
            if line.startswith(b"Content-Length:"):
                n = int(line.split(b":")[1])
                p.stdout.readline()  # blank
                body = p.stdout.read(n)
                msgs.append(body.decode("utf-8", "replace"))

    t = threading.Thread(target=reader, daemon=True)
    t.start()

    def send(obj):
        p.stdin.write(frame(obj))
        p.stdin.flush()

    send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})
    send({"jsonrpc": "2.0", "method": "initialized", "params": {}})
    time.sleep(0.3)
    send({"jsonrpc": "2.0", "method": "textDocument/didOpen",
          "params": {"textDocument": {"uri": URI, "languageId": "spid3r",
                                      "version": 1, "text": SRC}}})
    time.sleep(0.5)
    send({"jsonrpc": "2.0", "id": 2, "method": "textDocument/documentSymbol",
          "params": {"textDocument": {"uri": URI}}})
    time.sleep(0.5)
    send({"jsonrpc": "2.0", "id": 3, "method": "textDocument/hover",
          "params": {"textDocument": {"uri": URI},
                     "position": {"line": 4, "character": 2}}})
    time.sleep(0.5)
    send({"jsonrpc": "2.0", "id": 4, "method": "shutdown", "params": None})
    time.sleep(0.3)
    send({"jsonrpc": "2.0", "method": "exit", "params": None})
    try:
        p.wait(timeout=5)
    except subprocess.TimeoutExpired:
        p.kill()

    for m in msgs:
        print(m)
        print("-" * 60)
    if p.stderr:
        err = p.stderr.read().decode("utf-8", "replace")
        if err.strip():
            print("STDERR:", err)


if __name__ == "__main__":
    main()
