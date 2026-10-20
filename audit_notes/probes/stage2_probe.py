"""Stage-2 probe: run small Spid3r programs under a watchdog on both engines.

Usage: python stage2_probe.py <cases.py> ...
Reports OK / HANG / ERR per (case, engine). Not part of the test suite; the
permanent regressions live in tests/stage2_concurrency.rs.
"""

import os
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.environ.get("SPID3R_BIN") or os.path.join(ROOT, "target", "debug", "spid3r.exe")
TIMEOUT = 6.0

CASES = {
    "list_contains_self": """
fn main() {
    let l = list();
    l.push(l);
    println(l.contains(l));
    return 0;
}
""",
    "list_index_of_self": """
fn main() {
    let l = list();
    l.push(l);
    println(l.index_of(l));
    return 0;
}
""",
    "list_slice_self": """
fn main() {
    let l = list();
    l.push(l);
    let s = l.slice(0, 1);
    println(s.len());
    return 0;
}
""",
    "list_remove_self": """
fn main() {
    let l = list();
    l.push(l);
    l.remove(l);
    println(l.len());
    return 0;
}
""",
    "stack_to_list_self": """
fn main() {
    let s = stack();
    s.push(s);
    println(s.to_list().len());
    return 0;
}
""",
    "stack_contains_self": """
fn main() {
    let s = stack();
    s.push(s);
    println(s.contains(s));
    return 0;
}
""",
    "queue_to_list_self": """
fn main() {
    let q = queue();
    q.push(q);
    println(q.to_list().len());
    return 0;
}
""",
    "deque_to_list_self": """
fn main() {
    let d = deque();
    d.push(d);
    println(d.to_list().len());
    return 0;
}
""",
    "ring_contains_self": """
fn main() {
    let r = ring_buffer(4);
    r.push(r);
    println(r.contains(r));
    return 0;
}
""",
    "set_to_list_self": """
fn main() {
    let s = set();
    s.add(s);
    println(s.to_list().len());
    return 0;
}
""",
    "set_contains_self": """
fn main() {
    let s = set();
    s.add(s);
    println(s.contains(s));
    return 0;
}
""",
    "bag_count_self": """
fn main() {
    let b = bag();
    b.add(b);
    println(b.count_of(b));
    return 0;
}
""",
    "bag_to_list_self": """
fn main() {
    let b = bag();
    b.add(b);
    println(b.len());
    return 0;
}
""",
    "graph_nodes_self": """
fn main() {
    let g = graph("directed");
    g.add_node(g);
    println(g.nodes().len());
    return 0;
}
""",
    "graph_has_node_self": """
fn main() {
    let g = graph("directed");
    g.add_node(g);
    println(g.has_node(g));
    return 0;
}
""",
    "graph_bfs_self": """
fn main() {
    let g = graph("directed");
    g.add_node(g);
    println(g.bfs(g).len());
    return 0;
}
""",
    "bintree_inorder_self": """
fn main() {
    let t = binary_tree();
    t.insert(t);
    println(t.inorder().len());
    return 0;
}
""",
    "linked_list_to_list_self": """
fn main() {
    let l = linked_list();
    l.push_back(l);
    println(l.to_list().len());
    return 0;
}
""",
    "linked_list_contains_self": """
fn main() {
    let l = linked_list();
    l.push_back(l);
    println(l.contains(l));
    return 0;
}
""",
    "map_values_self": """
fn main() {
    let m = map();
    m.set("k", m);
    println(m.values().len());
    return 0;
}
""",
    "list_sorted_self": """
fn main() {
    let l = list();
    l.push(l);
    println(l.len());
    return 0;
}
""",
}


def run_case(name, src, engine):
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
            return p.returncode, out.decode("utf-8", "replace"), err.decode("utf-8", "replace")
        except subprocess.TimeoutExpired:
            p.kill()
            p.communicate()
            return "HANG", "", ""
    finally:
        os.unlink(path)


CASES.update({
    "graph_neighbors_self": """
fn main() {
    let g = graph("directed");
    g.add_node(g);
    println(g.neighbors(g).len());
    return 0;
}
""",
    "graph_dfs_self": """
fn main() {
    let g = graph("directed");
    g.add_node(g);
    println(g.dfs(g).len());
    return 0;
}
""",
    "graph_path_exists_self": """
fn main() {
    let g = graph("directed");
    g.add_node(g);
    println(g.path_exists(g, g));
    return 0;
}
""",
    "graph_find_path_self": """
fn main() {
    let g = graph("directed");
    g.add_node(g);
    let p = g.find_path(g, g);
    println(p.is_some());
    return 0;
}
""",
    "graph_add_edge_self": """
fn main() {
    let g = graph("directed");
    g.add_node(g);
    g.add_edge(g, g, 2.0);
    println(g.edge_count());
    return 0;
}
""",
    "graph_clone_self": """
fn main() {
    let g = graph("directed");
    g.add_node(g);
    let h = g.clone();
    println(h.len());
    return 0;
}
""",
    "graph_clear_self": """
fn main() {
    let g = graph("directed");
    g.add_node(g);
    g.clear();
    println(g.len());
    return 0;
}
""",
    "linked_list_remove_self": """
fn main() {
    let l = linked_list();
    l.push_back(l);
    l.remove(l);
    println(l.len());
    return 0;
}
""",
    "tuple_contains_self": """
fn main() {
    let l = list();
    let t = tuple(l);
    l.push(t);
    println(t.contains(t));
    return 0;
}
""",
    "prio_queue_self": """
fn main() {
    let q = priority_queue("min");
    q.push(q, 1.0);
    println(q.len());
    return 0;
}
""",
    "heap_self": """
fn main() {
    let h = heap("min");
    h.push(h);
    println(h.len());
    return 0;
}
""",
    "bst_container": """
fn main() {
    let t = bst();
    t.insert(list());
    println(t.len());
    return 0;
}
""",
    "hash_table_self": """
fn main() {
    let h = hash_table();
    h.set("k", h);
    println(h.get("k").len());
    return 0;
}
""",
    "map_items_self": """
fn main() {
    let m = map();
    m.set("k", m);
    println(m.items().len());
    return 0;
}
""",
    "set_union_self": """
fn main() {
    let s = set();
    s.add(s);
    println(s.union(s).len());
    return 0;
}
""",
    "list_append_self": """
fn main() {
    let l = list();
    l.push(1);
    l.append(l);
    println(l.len());
    return 0;
}
""",
})

def main():
    only = sys.argv[1:] or None
    for name, src in CASES.items():
        if only and name not in only:
            continue
        row = [name]
        for engine in ("tree", "vm"):
            code, out, err = run_case(name, src, engine)
            first = (out.strip().splitlines() or [""])[0][:40]
            msg = (err.strip().splitlines() or [""])[-1][:60] if err.strip() else ""
            row.append(f"{engine}={code} {first!r} {msg}")
        print(" | ".join(row), flush=True)


if __name__ == "__main__":
    main()
