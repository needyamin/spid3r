//! Data-structure tests (Phases 4-12): every collection constructor and its
//! documented operations, including the error cases the spec demands
//! (duplicate BST keys, float map keys, missing keys, empty pops).

use spid3r::diagnostics::Diagnostic;
use spid3r::interpreter::Interp;
use spid3r::parser;
use spid3r::semantic;
use std::sync::Arc;

fn run(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("t.matrix"))?;
    semantic::check_file(&program, true, "t.matrix")?;
    let interp = Interp::new(Arc::from("t.matrix"), vec![]);
    interp.run_program(&program)
}

fn expect_ok(src: &str) {
    run(src).unwrap_or_else(|d| panic!("expected success, got: {}", d));
}

fn expect_err(src: &str, needle: &str) {
    match run(src) {
        Err(d) => assert!(
            d.message.contains(needle),
            "error {:?} does not mention {}",
            d.message,
            needle
        ),
        Ok(code) => panic!("expected failure mentioning {}, exited with {}", needle, code),
    }
}

// ---- list ----------------------------------------------------------------

#[test]
fn list_indexing_and_mutation() {
    expect_ok(
        r#"
        fn main() {
            let xs = list(1, 2, 3);
            if xs.len() != 3 { return 1; }
            if xs.get(2) != 3 { return 1; }
            xs.set(0, 10);
            if xs.get(0) != 10 { return 1; }
            xs.push(4);
            if xs.len() != 4 { return 1; }
            if xs.pop() != 4 { return 1; }
            xs.insert(1, 99);
            if xs.get(1) != 99 { return 1; }
            xs.remove_at(1);
            if xs.get(1) != 2 { return 1; }
            if xs.index_of(3) != 2 { return 1; }
            if xs.contains(2) != true { return 1; }
            xs.reverse();
            if xs.get(0) != 3 { return 1; }
            xs.sort();
            if xs.get(0) != 2 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn list_index_out_of_range_throws() {
    expect_err(
        r#"
        fn main() {
            let xs = list(1);
            let x = xs.get(5);
            return 0;
        }
    "#,
        "out of bounds",
    );
}

// ---- stack / queue / deque ----------------------------------------------

#[test]
fn stack_is_lifo() {
    expect_ok(
        r#"
        fn main() {
            let s = stack();
            s.push(1);
            s.push(2);
            s.push(3);
            if s.pop() != 3 { return 1; }
            if s.pop() != 2 { return 1; }
            if s.peek() != 1 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn stack_pop_on_empty_throws() {
    expect_err(
        r#"
        fn main() {
            let s = stack();
            return s.pop();
        }
    "#,
        "empty",
    );
}

#[test]
fn queue_is_fifo_and_deque_has_two_ends() {
    expect_ok(
        r#"
        fn main() {
            let q = queue();
            q.push("a");
            q.push("b");
            if q.pop() != "a" { return 1; }
            if q.pop() != "b" { return 1; }
            let dq = deque();
            dq.push_back(2);
            dq.push_front(1);
            dq.push_back(3);
            if dq.pop_front() != 1 { return 1; }
            if dq.pop_back() != 3 { return 1; }
            if dq.peek_front() != 2 { return 1; }
            return 0;
        }
    "#,
    );
}

// ---- map / set -----------------------------------------------------------

#[test]
fn map_is_insertion_ordered_with_typed_keys() {
    expect_ok(
        r#"
        fn main() {
            let m = map("b", 2, "a", 1);
            if m.len() != 2 { return 1; }
            if m.get("a") != 1 { return 1; }
            m.set("c", 3);
            if m.get("c") != 3 { return 1; }
            m.remove("b");
            if m.contains("b") != false { return 1; }
            if m.contains("a") != true { return 1; }
            let ports = map(80, "http", 443, "https");
            if ports.get(443) != "https" { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn map_missing_key_is_an_error_and_floats_are_rejected() {
    expect_err(
        r#"
        fn main() {
            let m = map();
            return m.get("nope");
        }
    "#,
        "missing key",
    );
    expect_err(
        r#"
        fn main() {
            let m = map(1.5, "x");
            return 0;
        }
    "#,
        "float",
    );
}

#[test]
fn set_does_union_intersection_difference() {
    expect_ok(
        r#"
        fn main() {
            let a = set(1, 2, 3);
            let b = set(3, 4);
            if a.union(b).len() != 4 { return 1; }
            if a.intersection(b).len() != 1 { return 1; }
            if a.difference(b).len() != 2 { return 1; }
            if a.contains(2) != true { return 1; }
            a.add(9);
            if a.len() != 4 { return 1; }
            return 0;
        }
    "#,
    );
}

// ---- linked lists ---------------------------------------------------------

#[test]
fn linked_lists_have_the_promised_end_costs() {
    expect_ok(
        r#"
        fn main() {
            let ll = linked_list();
            ll.push_back(2);
            ll.push_front(1);
            ll.push_back(3);
            if ll.get(1) != 2 { return 1; }
            if ll.pop_back() != 3 { return 1; }
            if ll.pop_front() != 1 { return 1; }
            let sl = singly_linked_list();
            sl.push_back(1);
            sl.push_front(0);
            if sl.get(0) != 0 { return 1; }
            let dl = doubly_linked_list();
            dl.push_back(1);
            dl.push_front(0);
            if dl.pop_back() != 1 { return 1; }
            if dl.pop_front() != 0 { return 1; }
            return 0;
        }
    "#,
    );
}

// ---- BST ------------------------------------------------------------------

#[test]
fn bst_orders_and_deletes_correctly() {
    expect_ok(
        r#"
        fn main() {
            let t = bst();
            t.insert(50);
            t.insert(30);
            t.insert(70);
            t.insert(20);
            t.insert(40);
            if t.contains(40) != true { return 1; }
            if t.contains(99) != false { return 1; }
            t.remove(20);  // leaf
            t.remove(30);  // one child
            t.remove(50);  // two children
            if t.inorder().len() != 2 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn bst_rejects_duplicates() {
    expect_err(
        r#"
        fn main() {
            let t = bst();
            t.insert(1);
            t.insert(1);
            return 0;
        }
    "#,
        "duplicate",
    );
}

// ---- heap / priority queue ------------------------------------------------

#[test]
fn heap_pops_in_priority_order() {
    expect_ok(
        r#"
        fn main() {
            let h = heap("min");
            h.push(5);
            h.push(1);
            h.push(3);
            if h.peek() != 1 { return 1; }
            if h.pop() != 1 { return 1; }
            if h.pop() != 3 { return 1; }
            if h.pop() != 5 { return 1; }
            let mx = heap("max");
            mx.push(1);
            mx.push(9);
            if mx.pop() != 9 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn priority_queue_breaks_ties_by_arrival() {
    expect_ok(
        r#"
        fn main() {
            let pq = priority_queue();
            pq.push("first", 1);
            pq.push("second", 1);
            pq.push("urgent", 0);
            if pq.pop() != "urgent" { return 1; }
            if pq.pop() != "first" { return 1; }
            if pq.pop() != "second" { return 1; }
            return 0;
        }
    "#,
    );
}

// ---- graph ----------------------------------------------------------------

#[test]
fn graph_connects_traversals_and_paths() {
    expect_ok(
        r#"
        fn main() {
            let g = graph("undirected");
            g.add_node("boss");
            g.add_node("manager");
            g.add_node("intern");
            g.add_edge("boss", "manager");
            g.add_edge("manager", "intern", 2.5);
            if g.path_exists("boss", "intern") != true { return 1; }
            let p = g.find_path("boss", "intern").unwrap();
            if p.len() != 3 { return 1; }
            if g.bfs("boss").len() != 3 { return 1; }
            if g.dfs("boss").len() != 3 { return 1; }
            if g.neighbors("manager").len() != 2 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn graph_rejects_edges_between_unknown_vertices() {
    expect_err(
        r#"
        fn main() {
            let g = graph("directed");
            g.add_node("a");
            g.add_edge("a", "ghost");
            return 0;
        }
    "#,
        "not a node",
    );
}

// ---- hash table -----------------------------------------------------------

#[test]
fn hash_table_rehashes_and_keeps_buckets_visible() {
    expect_ok(
        r#"
        fn main() {
            let h = hash_table();
            for let i = 0; i < 50; i += 1 {
                h.set(i, i * 2);
            }
            if h.len() != 50 { return 1; }
            if h.get(7) != 14 { return 1; }
            h.remove(7);
            if h.contains(7) != false { return 1; }
            // 50 entries over the 0.75 load factor must have grown past 8 buckets
            if h.bucket_count() <= 8 { return 1; }
            return 0;
        }
    "#,
    );
}

// ---- trie / matrix / ring / bag -------------------------------------------

#[test]
fn trie_does_prefix_queries() {
    expect_ok(
        r#"
        fn main() {
            let t = trie();
            t.insert("spider");
            t.insert("spidey");
            t.insert("web");
            if t.contains("spider") != true { return 1; }
            if t.contains("spi") != false { return 1; }
            if t.starts_with("spi") != true { return 1; }
            if t.with_prefix("spid").len() != 2 { return 1; }
            if t.all_words().len() != 3 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn matrix_adds_and_multiplies() {
    expect_ok(
        r#"
        fn main() {
            let a = matrix(2, 2, 1.0);
            let b = matrix(2, 2, 2.0);
            let sum = a.add(b);
            if sum.get(0, 0) != 3.0 { return 1; }
            let prod = a.mul(b);
            if prod.get(1, 1) != 4.0 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn ring_buffer_overwrites_the_oldest() {
    expect_ok(
        r#"
        fn main() {
            let r = ring_buffer(3);
            r.push(1);
            r.push(2);
            r.push(3);
            r.push(4);
            if r.len() != 3 { return 1; }
            if r.peek() != 2 { return 1; }
            if r.pop() != 2 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn bag_counts_multiplicity() {
    expect_ok(
        r#"
        fn main() {
            let b = bag();
            b.add("x");
            b.add("x");
            b.add("y");
            if b.count_of("x") != 2 { return 1; }
            b.remove("x");
            if b.count_of("x") != 1 { return 1; }
            return 0;
        }
    "#,
    );
}

// ---- option / result / ref / tuple ----------------------------------------

#[test]
fn option_and_result_carry_values() {
    expect_ok(
        r#"
        fn main() {
            let s = some(7);
            if s.is_some() != true { return 1; }
            if s.unwrap() != 7 { return 1; }
            let n = none();
            if n.is_none() != true { return 1; }
            if n.unwrap_or(0) != 0 { return 1; }
            let okv = ok(1);
            if okv.is_ok() != true { return 1; }
            let e = err("bad");
            if e.is_err() != true { return 1; }
            if e.unwrap_err() != "bad" { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn ref_box_shares_one_cell() {
    expect_ok(
        r#"
        fn main() {
            let box1 = ref(1);
            let box2 = box1;
            box2.set(2);
            if box1.get() != 2 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn clone_is_a_deep_copy() {
    expect_ok(
        r#"
        fn main() {
            let a = list(list(1, 2), list(3, 4));
            let b = a.clone();
            b.get(0).push(99);
            if a.get(0).len() != 2 { return 1; }
            if b.get(0).len() != 3 { return 1; }
            return 0;
        }
    "#,
    );
}
