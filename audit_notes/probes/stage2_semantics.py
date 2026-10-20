"""Stage-2 semantic probes (A-3 exit drain, A-4 gc gate, A-9 structured throws).

Usage: python stage2_semantics.py [--bin PATH]
"""

import os
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.environ.get("SPID3R_BIN") or os.path.join(ROOT, "target", "debug", "spid3r.exe")
TIMEOUT = 20.0

CASES = {
    "exit_drain_keeps_task_output": """
fn main() {
    spawn fn() {
        sleep_ms(200);
        println("task-output");
    };
    println("main-output");
    return 0;
}
""",
    "gc_collect_refuses_while_tasks_live": """
async fn wait(t) {
    return await t;
}
fn main() {
    let t = spawn fn() {
        sleep_ms(400);
        return 1;
    };
    try {
        gc_collect();
        println("no-error");
    } catch(e) {
        println("blocked");
    }
    let v = wait(t);
    println(gc_collect() >= 0);
    return 0;
}
""",
    "structured_throw_across_await": """
async fn joiner(t) {
    return await t;
}
fn main() {
    let t = spawn fn() {
        let o = map();
        o.set("code", 7);
        o.set("why", "bad");
        throw o;
    };
    try {
        let v = joiner(t);
        println("no-throw");
    } catch(e) {
        println(type_of(e));
        println(e["code"]);
        println(e["why"]);
    }
    return 0;
}
""",
    "structured_throw_across_task_join": """
fn main() {
    let t = spawn fn() {
        let o = map();
        o.set("code", 9);
        throw o;
    };
    sleep_ms(50);
    try {
        let v = t.join();
        println("no-throw");
    } catch(e) {
        println(type_of(e));
        println(e["code"]);
    }
    return 0;
}
""",
    "structured_throw_across_callback": """
fn main() {
    let l = [1, 2, 3];
    try {
        let m = l.map(fn(x) {
            let o = map();
            o.set("code", 11);
            throw o;
        });
        println("no-throw");
    } catch(e) {
        println(type_of(e));
        println(e["code"]);
    }
    return 0;
}
""",
    "structured_throw_across_nested_callback": """
fn main() {
    let outer = [1];
    try {
        let m = outer.map(fn(x) {
            let inner = [2];
            return inner.map(fn(y) {
                let o = map();
                o.set("code", 13);
                throw o;
            });
        });
        println("no-throw");
    } catch(e) {
        println(type_of(e));
        println(e["code"]);
    }
    return 0;
}
""",
    "structured_throw_comparator": """
fn main() {
    let l = [3, 1, 2];
    try {
        l.sort_by(fn(a, b) {
            let o = map();
            o.set("code", 17);
            throw o;
        });
        println("no-throw");
    } catch(e) {
        println(type_of(e));
        println(e["code"]);
    }
    return 0;
}
""",
    "string_throw_stays_string": """
async fn joiner(t) {
    return await t;
}
fn main() {
    let t = spawn fn() {
        throw "plain";
    };
    try {
        let v = joiner(t);
        println("no-throw");
    } catch(e) {
        println(type_of(e));
        println(e);
    }
    return 0;
}
""",
}


def run(src, engine):
    with tempfile.NamedTemporaryFile("w", suffix=".matrix", delete=False, encoding="utf-8") as f:
        f.write(src)
        path = f.name
    try:
        p = subprocess.Popen(
            [BIN, "run", "--engine", engine, path],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            out, err = p.communicate(timeout=TIMEOUT)
            return p.returncode, out.decode("utf-8", "replace").strip(), err.decode("utf-8", "replace").strip()
        except subprocess.TimeoutExpired:
            p.kill()
            p.communicate()
            return "HANG", "", ""
    finally:
        os.unlink(path)


def main():
    for name, src in CASES.items():
        for engine in ("tree", "vm"):
            code, out, err = run(src, engine)
            flat = " / ".join(out.splitlines())
            print(f"{name:42s} {engine:5s} exit={code} out={flat!r} err={err[:60]!r}", flush=True)


if __name__ == "__main__":
    main()
