"""Manual probe for `spid3r lsp`: scripted session, prints every reply verbatim.

Usage: python lsp_probe2.py
"""
import json
import subprocess
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
                p.stdout.readline()  # blank line
                msgs.append(p.stdout.read(n).decode("utf-8", "replace"))

    threading.Thread(target=reader, daemon=True).start()

    def send(obj):
        p.stdin.write(frame(obj))
        p.stdin.flush()

    send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})
    send({"jsonrpc": "2.0", "method": "initialized", "params": {}})
    time.sleep(0.3)
    send({"jsonrpc": "2.0", "method": "textDocument/didOpen",
          "params": {"textDocument": {"uri": URI, "languageId": "spid3r",
                                      "version": 1, "text": SRC}}})
    time.sleep(0.4)
    for rid, line, ch in [(2, 1, 9), (3, 1, 10), (5, 4, 2)]:
        send({"jsonrpc": "2.0", "id": rid, "method": "textDocument/hover",
              "params": {"textDocument": {"uri": URI},
                         "position": {"line": line, "character": ch}}})
        time.sleep(0.3)
    send({"jsonrpc": "2.0", "id": 4, "method": "shutdown", "params": None})
    time.sleep(0.2)
    send({"jsonrpc": "2.0", "method": "exit", "params": None})
    try:
        p.wait(timeout=5)
    except subprocess.TimeoutExpired:
        p.kill()

    for m in msgs:
        print(m)
        print("-" * 60)
    err = p.stderr.read().decode("utf-8", "replace")
    if err.strip():
        print("STDERR:", err)


if __name__ == "__main__":
    main()
