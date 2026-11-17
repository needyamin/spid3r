#!/usr/bin/env python3
"""R2.4 http_echo benchmark driver (Spid3r vs Node.js, keep-alive & close arms).

Measures client-side completed requests/sec + P99 latency across two
consecutive passes and checks two-pass agreement <= 3%.
"""

import argparse
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent.parent
EXPECTED_BODY = b"x" * 64


def find_spid3r() -> Path:
    for cfg in ("release", "debug"):
        for name in ("spid3r.exe", "spid3r"):
            p = ROOT / "target" / cfg / name
            if p.is_file():
                return p
    raise FileNotFoundError("spid3r binary not found")


def run_pass(cmd: list[str], conns: int, warmup_s: float, duration_s: float, mode: str) -> dict:
    proc = subprocess.Popen(
        cmd,
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        ready = proc.stdout.readline().strip()
        if not ready.startswith("READY "):
            raise RuntimeError(f"server failed to start: {ready!r} stderr={proc.stderr.read()!r}")
        port = int(ready.split()[1].rsplit(":", 1)[-1])

        req_bytes = (
            f"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: {'close' if mode == 'close' else 'keep-alive'}\r\n\r\n"
        ).encode("ascii")

        stop_evt = threading.Event()
        record_evt = threading.Event()
        lats: list[float] = []
        counts = [0] * conns
        bad = [0] * conns
        lock = threading.Lock()

        def read_http_response(sock: socket.socket) -> bool:
            buf = b""
            while b"\r\n\r\n" not in buf:
                chunk = sock.recv(4096)
                if not chunk:
                    return False
                buf += chunk
            hdr, rest = buf.split(b"\r\n\r\n", 1)
            if b"200 OK" not in hdr.split(b"\r\n", 1)[0]:
                return False
            while len(rest) < 64:
                chunk = sock.recv(64 - len(rest))
                if not chunk:
                    return False
                rest += chunk
            return rest[:64] == EXPECTED_BODY

        def worker(idx: int) -> None:
            local_lats: list[float] = []
            c = 0
            err = 0
            s = None
            try:
                while not stop_evt.is_set():
                    if s is None:
                        s = socket.create_connection(("127.0.0.1", port), timeout=5.0)
                        s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
                    t0 = time.perf_counter_ns()
                    try:
                        s.sendall(req_bytes)
                        ok = read_http_response(s)
                    except OSError:
                        ok = False
                    dt_ms = (time.perf_counter_ns() - t0) / 1e6
                    if not ok:
                        err += 1
                        s.close()
                        s = None
                        continue
                    if record_evt.is_set():
                        c += 1
                        local_lats.append(dt_ms)
                    if mode == "close":
                        s.close()
                        s = None
            finally:
                if s is not None:
                    s.close()
            counts[idx] = c
            bad[idx] = err
            with lock:
                lats.extend(local_lats)

        threads = [threading.Thread(target=worker, args=(i,)) for i in range(conns)]
        for t in threads:
            t.start()
        time.sleep(warmup_s)
        t_start = time.perf_counter()
        record_evt.set()
        time.sleep(duration_s)
        stop_evt.set()
        for t in threads:
            t.join(timeout=5.0)
        elapsed = time.perf_counter() - t_start
        total = sum(counts)
        total_bad = sum(bad)
        if total_bad > 0 or total == 0:
            raise RuntimeError(f"checksum mismatch: completed={total} bad={total_bad}")
        lats.sort()
        p50 = lats[len(lats) // 2]
        p99 = lats[min(len(lats) - 1, int(0.99 * len(lats)))]
        return {
            "completed": total,
            "bytes_verified": total * 64,
            "rps": total / elapsed,
            "p50_ms": p50,
            "p99_ms": p99,
        }
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5.0)
        except subprocess.TimeoutExpired:
            proc.kill()


def measure_target(name: str, cmd: list[str], conns: int, warmup: float, duration: float, mode: str) -> dict:
    p1 = run_pass(cmd, conns, warmup, duration, mode)
    p2 = run_pass(cmd, conns, warmup, duration, mode)
    agree = abs(p1["rps"] - p2["rps"]) / max(p1["rps"], p2["rps"])
    avg_rps = 0.5 * (p1["rps"] + p2["rps"])
    avg_p99 = 0.5 * (p1["p99_ms"] + p2["p99_ms"])
    status = "PASS" if agree <= 0.03 else "WARN_AGREE"
    print(
        f"{name:<12} mode={mode:<9} conns={conns:2d}  "
        f"rps={avg_rps:8.1f} (p1={p1['rps']:.1f}, p2={p2['rps']:.1f})  "
        f"p99={avg_p99:6.2f} ms  agree={agree*100:4.2f}%  "
        f"CHECKSUM {p1['completed']+p2['completed']} {(p1['bytes_verified']+p2['bytes_verified'])}  {status}"
    )
    return {"p1": p1, "p2": p2, "avg_rps": avg_rps, "avg_p99_ms": avg_p99, "agree": agree, "status": status}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--conns", type=int, default=16)
    ap.add_argument("--warmup", type=float, default=0.5)
    ap.add_argument("--duration", type=float, default=2.0)
    ap.add_argument("--mode", choices=["keepalive", "close", "both"], default="keepalive")
    ap.add_argument("--target", choices=["spid3r", "node", "both"], default="both")
    args = ap.parse_args()

    spid3r_bin = find_spid3r()
    modes = ["keepalive", "close"] if args.mode == "both" else [args.mode]
    for m in modes:
        if args.target in ("spid3r", "both"):
            measure_target(
                "spid3r-vm",
                [str(spid3r_bin), "run", "--engine", "vm", str(HERE / "server.matrix")],
                args.conns,
                args.warmup,
                args.duration,
                m,
            )
        if args.target in ("node", "both"):
            measure_target(
                "node",
                ["node", str(HERE / "server.js")],
                args.conns,
                args.warmup,
                args.duration,
                m,
            )
    return 0


if __name__ == "__main__":
    sys.exit(main())
