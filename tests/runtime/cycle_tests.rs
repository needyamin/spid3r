//! Cycle-collector tests (`docs/GC.md`).
//!
//! The collector reclaims reference cycles that pure ref-counting leaks. It is
//! safe by construction (external-reference mark-sweep), and these tests pin
//! down the two things that matter: it reclaims cycles, and it never frees live
//! data. Auto-collection is opt-in via `SPID3R_GC`; `gc_collect()` is explicit.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn write_case(name: &str, src: &str) -> String {
    let path =
        std::env::temp_dir().join(format!("spid3r_gc_{}_{}.matrix", name, std::process::id()));
    std::fs::write(&path, src).expect("write case");
    path.to_string_lossy().to_string()
}

/// Run a program, optionally with `SPID3R_GC` set (or removed).
fn run(path: &str, gc: Option<&str>) -> (i32, String, String) {
    let mut cmd = Command::new(bin());
    cmd.args(["run", path]);
    match gc {
        Some(v) => cmd.env("SPID3R_GC", v),
        None => cmd.env_remove("SPID3R_GC"),
    };
    let out = cmd.output().expect("spawn");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

const SELF_CYCLE_LOOP: &str = r#"
    fn main() {
        let i = 0;
        while i < 2000 {
            let a = list();
            a.push(a);          // a self-referential list: leaks under pure RC
            i = i + 1;
        }
        let before = gc_live_cells();
        gc_collect();
        let after = gc_live_cells();
        println(string(before) + "/" + string(after));
        return 0;
    }
"#;

#[test]
fn explicit_gc_collect_reclaims_self_referential_lists() {
    let path = write_case("explicit", SELF_CYCLE_LOOP);
    // Auto-collection is on by default; disable it so the cycles leak and we can
    // prove the explicit gc_collect() reclaims them.
    let (code, out, err) = run(&path, Some("0"));
    assert_eq!(code, 0, "run failed: {}{}", out, err);
    let parts: Vec<i64> = out.trim().split('/').map(|s| s.parse().unwrap()).collect();
    let (before, after) = (parts[0], parts[1]);
    assert!(
        before > 1000,
        "the self-cycles should have leaked before collecting (got {})",
        before
    );
    assert!(
        after < 100,
        "gc_collect() must reclaim the cycles (before {}, after {})",
        before,
        after
    );
}

#[test]
fn gc_reclaims_instance_and_map_self_cycles() {
    let src = r#"
        class Node { public let next; fn new() { self.next = null; } }
        fn main() {
            let i = 0;
            while i < 2000 { let n = new Node(); n.next = n; i = i + 1; }
            let j = 0;
            while j < 2000 { let mm = map(); mm.set("self", mm); j = j + 1; }
            let keep = new Node();
            keep.next = keep;
            gc_collect();
            println(string(gc_live_cells()));
            println(keep.next == keep);
            return 0;
        }
    "#;
    let path = write_case("instmap", src);
    let (code, out, err) = run(&path, Some("0"));
    assert_eq!(code, 0, "run failed: {}{}", out, err);
    let lines: Vec<&str> = out.trim().lines().collect();
    let live: i64 = lines[0].parse().expect("number");
    // A-13 (2026-10-21): environments, closures, and classes are tracked
    // cells too, so a handful of legitimately-live roots remain (the global
    // environment, the running frame's environment, `main`'s closure, the
    // class of the live cycle). The 4000 garbage cycles must still be gone.
    assert!(
        live <= 8,
        "only the live cycle and the constant roots should survive the 4000 \
         garbage cycles, got {}",
        live
    );
    assert_eq!(
        lines[1], "true",
        "the live self-cycle must be intact: {}",
        out
    );
}

#[test]
fn auto_gc_keeps_the_live_set_bounded() {
    // With SPID3R_GC set, the statement-boundary trigger collects during the
    // loop, so the live set stays far below the number of cycles created.
    let src = r#"
        fn main() {
            let i = 0;
            while i < 5000 {
                let a = list();
                a.push(a);
                i = i + 1;
            }
            println(string(gc_live_cells()));
            return 0;
        }
    "#;
    let path = write_case("auto", src);
    let (code, out, err) = run(&path, Some("1"));
    assert_eq!(code, 0, "run failed: {}{}", out, err);
    let live: i64 = out.trim().parse().expect("number");
    assert!(
        live < 2000,
        "auto GC should keep live cells bounded below the 5000 created, got {}",
        live
    );
}

#[test]
fn gc_never_frees_live_data() {
    // `keep` is a live, reachable list while many cycles churn around it; with
    // GC on it must survive intact (external-reference mark-sweep keeps roots).
    let src = r#"
        fn main() {
            let keep = list();
            keep.push(1);
            keep.push(2);
            keep.push(3);
            let i = 0;
            while i < 2000 {
                let a = list();
                a.push(a);
                i = i + 1;
            }
            println(string(keep.get(0)) + string(keep.get(1)) + string(keep.get(2)));
            println(len(keep));
            return 0;
        }
    "#;
    let path = write_case("live", src);
    let (code, out, err) = run(&path, Some("1"));
    assert_eq!(code, 0, "run failed: {}{}", out, err);
    assert_eq!(
        out, "123\n3\n",
        "live data must survive collection: {}",
        out
    );
}

#[test]
fn gc_reclaims_cycles_across_all_container_types() {
    let src = r#"
        fn main() {
            let i = 0;
            while i < 200 {
                let bt = binary_tree();
                bt.insert(bt);

                let g = graph("directed");
                let r = ref(g);
                g.add_node(r);

                let ll = linked_list();
                ll.push_back(ll);

                let dll = doubly_linked_list();
                dll.push_back(dll);

                let st = stack();
                st.push(st);

                let q = queue();
                q.push(q);

                let dq = deque();
                dq.push_back(dq);

                let rb = ring_buffer(2);
                rb.push(rb);

                let bg = bag();
                bg.add(bg);

                let pq = priority_queue("min");
                pq.push(pq, 1);

                let ht = hash_table();
                ht.set("self", ht);

                let box_a = ref(null);
                box_a.set(some(tuple(ok(box_a))));

                i = i + 1;
            }
            let before = gc_live_cells();
            gc_collect();
            let after = gc_live_cells();
            println(string(before) + "/" + string(after));
            return 0;
        }
    "#;
    let path = write_case("all_containers", src);
    let (code, out, err) = run(&path, Some("0"));
    assert_eq!(code, 0, "run failed: {}{}", out, err);
    let parts: Vec<i64> = out.trim().split('/').map(|s| s.parse().unwrap()).collect();
    let (before, after) = (parts[0], parts[1]);
    assert!(
        before >= 2000,
        "expected >= 2000 leaked container cells before gc_collect(), got {}",
        before
    );
    // A-13 (2026-10-21): the baseline after a full reclaim is no longer 0 —
    // the global environment, the running frame's environment, and `main`'s
    // closure are tracked cells that are genuinely live. Every *cycle* cell
    // must still be gone, so the remainder is capped at the constant root set.
    assert!(
        after <= 8,
        "gc_collect() must reclaim all container cycles (before {}, after {})",
        before,
        after
    );
}
