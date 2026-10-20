//! Data-structure tests (Phases 4-12): every collection constructor and its
//! documented operations, including the error cases the spec demands
//! (duplicate BST keys, float map keys, missing keys, empty pops).

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::parser;
use spid3r_lang::semantic;
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

// ---- vector & matrix linear algebra ---------------------------------------

#[test]
fn vector_constructor_and_indexing() {
    expect_ok(
        r#"
        fn main() {
            let v1 = vector(4, 2.5);
            if v1.len() != 4 { return 1; }
            if len(v1) != 4 { return 1; }
            if v1[0] != 2.5 { return 1; }
            if v1[3] != 2.5 { return 1; }
            if v1[-1] != 2.5 { return 1; }
            if v1[-4] != 2.5 { return 1; }

            v1[1] = 7.0;
            if v1[1] != 7.0 { return 1; }
            v1[-1] = 9.0;
            if v1[3] != 9.0 { return 1; }
            if v1[-1] != 9.0 { return 1; }

            let v2 = vector([1, 2, 3]);
            if v2.len() != 3 { return 1; }
            if v2[0] != 1.0 { return 1; }
            if v2[1] != 2.0 { return 1; }
            if v2[2] != 3.0 { return 1; }

            let total = 0.0;
            for x in v2 {
                total = total + x;
            }
            if total != 6.0 { return 1; }

            let sl = v2.slice(1, 3);
            if sl.len() != 2 { return 1; }
            if sl[0] != 2.0 or sl[1] != 3.0 { return 1; }

            let lst = v2.to_list();
            if lst.len() != 3 { return 1; }
            if lst[0] != 1.0 or lst[2] != 3.0 { return 1; }

            let c = v2.copy();
            c.set(0, 42.0);
            if v2[0] != 1.0 { return 1; }
            if c[0] != 42.0 { return 1; }

            v2.push(4.0);
            if v2.len() != 4 { return 1; }
            if v2[3] != 4.0 { return 1; }

            return 0;
        }
    "#,
    );
}

#[test]
fn vector_elementwise_and_reductions() {
    expect_ok(
        r#"
        fn main() {
            let a = vector([1.0, 2.0, 3.0]);
            let b = vector([4.0, 5.0, 6.0]);

            let c_add = a.add(b);
            if c_add[0] != 5.0 or c_add[1] != 7.0 or c_add[2] != 9.0 { return 1; }
            let s_add = a.add(10.0);
            if s_add[0] != 11.0 or s_add[1] != 12.0 or s_add[2] != 13.0 { return 1; }

            let c_sub = b.sub(a);
            if c_sub[0] != 3.0 or c_sub[1] != 3.0 or c_sub[2] != 3.0 { return 1; }
            let s_sub = b.sub(2.0);
            if s_sub[0] != 2.0 or s_sub[1] != 3.0 or s_sub[2] != 4.0 { return 1; }

            let c_mul = a.mul(b);
            if c_mul[0] != 4.0 or c_mul[1] != 10.0 or c_mul[2] != 18.0 { return 1; }
            let s_mul = a.mul(2.0);
            if s_mul[0] != 2.0 or s_mul[1] != 4.0 or s_mul[2] != 6.0 { return 1; }

            let c_div = b.div(a);
            if c_div[0] != 4.0 or c_div[1] != 2.5 or c_div[2] != 2.0 { return 1; }
            let s_div = b.div(2.0);
            if s_div[0] != 2.0 or s_div[1] != 2.5 or s_div[2] != 3.0 { return 1; }

            let v = vector([3.0, 1.0, 4.0, 1.0, 5.0]);
            if v.sum() != 14.0 { return 1; }
            if abs(v.mean() - 2.8) > 1e-9 { return 1; }
            if abs(v.variance() - 2.56) > 1e-9 { return 1; }
            if abs(v.std() - 1.6) > 1e-9 { return 1; }
            if v.min() != 1.0 { return 1; }
            if v.max() != 5.0 { return 1; }
            if v.argmin() != 1 { return 1; }
            if v.argmax() != 4 { return 1; }

            let d = a.dot(b);
            if d != 32.0 { return 1; }

            let n_vec = vector([3.0, 4.0]);
            if abs(n_vec.norm() - 5.0) > 1e-9 { return 1; }

            let cs = a.cumsum();
            if cs[0] != 1.0 or cs[1] != 3.0 or cs[2] != 6.0 { return 1; }

            let df = vector([1.0, 4.0, 9.0]).diff();
            if df.len() != 2 or df[0] != 3.0 or df[1] != 5.0 { return 1; }

            let v_unsorted = vector([5.0, 2.0, 8.0, 1.0]);
            let ord = v_unsorted.argsort();
            if ord[0] != 3 or ord[1] != 1 or ord[2] != 0 or ord[3] != 2 { return 1; }

            v_unsorted.sort();
            if v_unsorted[0] != 1.0 or v_unsorted[1] != 2.0 or v_unsorted[2] != 5.0 or v_unsorted[3] != 8.0 { return 1; }

            return 0;
        }
    "#,
    );
}

#[test]
fn vector_errors_handled_cleanly() {
    expect_err(
        r#"
        fn main() {
            let v = vector(2);
            let x = v[5];
            return 0;
        }
    "#,
        "out of bounds",
    );
    expect_err(
        r#"
        fn main() {
            let v = vector(2);
            let x = v[-3];
            return 0;
        }
    "#,
        "out of bounds",
    );
    expect_err(
        r#"
        fn main() {
            let v = vector(2);
            let s = v.div(0);
            return 0;
        }
    "#,
        "division by zero",
    );
}

#[test]
fn matrix_expanded_methods_and_solvers() {
    expect_ok(
        r#"
        fn main() {
            let m1 = matrix_from_list([[1.0, 2.0], [3.0, 4.0]]);
            let m2 = matrix([[1.0, 2.0], [3.0, 4.0]]);
            if m1.rows() != 2 or m1.cols() != 2 { return 1; }
            if m1.get(0, 1) != 2.0 or m2.get(1, 0) != 3.0 { return 1; }

            let lst = m1.to_list();
            if lst.len() != 2 or lst[0][1] != 2.0 { return 1; }

            let t = m1.transpose();
            if t.get(0, 1) != 3.0 or t.get(1, 0) != 2.0 { return 1; }

            let sc = m1.scale(3.0);
            if sc.get(0, 0) != 3.0 or sc.get(1, 1) != 12.0 { return 1; }

            let sb = sc.sub(m1);
            if sb.get(0, 0) != 2.0 or sb.get(1, 1) != 8.0 { return 1; }

            let hd = m1.hadamard(m1);
            if hd.get(0, 1) != 4.0 or hd.get(1, 0) != 9.0 { return 1; }

            if m1.trace() != 5.0 { return 1; }

            let total_sum = m1.sum(null);
            if total_sum != 10.0 { return 1; }
            let sum_cols = m1.sum(0);
            if sum_cols.len() != 2 or sum_cols[0] != 4.0 or sum_cols[1] != 6.0 { return 1; }
            let sum_rows = m1.sum(1);
            if sum_rows.len() != 2 or sum_rows[0] != 3.0 or sum_rows[1] != 7.0 { return 1; }

            let total_mean = m1.mean(null);
            if total_mean != 2.5 { return 1; }
            let mean_cols = m1.mean(0);
            if mean_cols.len() != 2 or mean_cols[0] != 2.0 or mean_cols[1] != 3.0 { return 1; }
            let mean_rows = m1.mean(1);
            if mean_rows.len() != 2 or mean_rows[0] != 1.5 or mean_rows[1] != 3.5 { return 1; }

            // Det: det([[1, 2], [3, 4]]) = 4 - 6 = -2.0
            if abs(m1.det() - (-2.0)) > 1e-9 { return 1; }

            // Inverse:
            let inv = m1.inverse();
            let ident = m1.mul(inv);
            if abs(ident.get(0, 0) - 1.0) > 1e-9 { return 1; }
            if abs(ident.get(0, 1) - 0.0) > 1e-9 { return 1; }
            if abs(ident.get(1, 0) - 0.0) > 1e-9 { return 1; }
            if abs(ident.get(1, 1) - 1.0) > 1e-9 { return 1; }

            // Solve:
            // [1, 2] * [x1] = [5]
            // [3, 4] * [x2]   [11]
            // x1 = 1, x2 = 2
            let b_vec = vector([5.0, 11.0]);
            let x_vec = m1.solve(b_vec);
            if abs(x_vec[0] - 1.0) > 1e-9 or abs(x_vec[1] - 2.0) > 1e-9 { return 1; }

            // Symmetric Eigenvalues:
            let sym = matrix([[2.0, 1.0], [1.0, 2.0]]);
            let eig = sym.eigen_sym();
            let vals = eig["values"];
            let vecs = eig["vectors"];
            // eigenvalues should be 1.0 and 3.0
            if abs(vals[0] - 1.0) > 1e-9 or abs(vals[1] - 3.0) > 1e-9 { return 1; }
            // Verify orthogonality: V^T * V = I
            let vt = vecs.transpose();
            let v_ident = vt.mul(vecs);
            if abs(v_ident.get(0, 0) - 1.0) > 1e-9 or abs(v_ident.get(1, 1) - 1.0) > 1e-9 { return 1; }
            if abs(v_ident.get(0, 1)) > 1e-9 or abs(v_ident.get(1, 0)) > 1e-9 { return 1; }

            return 0;
        }
    "#,
    );
}

#[test]
fn matrix_singular_returns_catchable_error() {
    expect_err(
        r#"
        fn main() {
            let m = matrix([[1.0, 2.0], [2.0, 4.0]]);
            let inv = m.inverse();
            return 0;
        }
    "#,
        "matrix is singular",
    );
    expect_err(
        r#"
        fn main() {
            let m = matrix([[1.0, 2.0], [2.0, 4.0]]);
            let x = m.solve(vector([1.0, 2.0]));
            return 0;
        }
    "#,
        "matrix is singular",
    );
}

#[test]
fn bytes_constructor_methods_and_indexing() {
    expect_ok(
        r#"
        fn main() {
            let b = bytes("hello");
            if b.len() != 5 { return 1; }
            if len(b) != 5 { return 2; }
            if b[0] != 104 { return 3; }
            if b[-1] != 111 { return 4; }
            b[0] = 72; // 'H'
            if b[0] != 72 { return 5; }

            let s = 0;
            for byte in b {
                s = s + byte;
            }
            if s <= 0 { return 6; }

            let sl = b.slice(1, 4);
            if sl.len() != 3 { return 7; }
            if sl.to_string().unwrap() != "ell" { return 8; }

            let b2 = bytes([65, 66, 67]);
            if b2.to_hex() != "414243" { return 9; }
            if b2.to_base64() != "QUJD" { return 10; }

            let b3 = bytes().from_hex("414243");
            if b3.to_string().unwrap() != "ABC" { return 11; }

            let b4 = bytes().from_base64("QUJD");
            if b4.to_string().unwrap() != "ABC" { return 12; }

            let b_filled = bytes(4, 255);
            if b_filled[0] != 255 or b_filled[3] != 255 { return 13; }

            return 0;
        }
    "#,
    );
}

#[test]
fn bytes_errors_handled_cleanly() {
    expect_err(
        r#"
        fn main() {
            let b = bytes("abc");
            let x = b[5];
            return 0;
        }
    "#,
        "out of bounds",
    );
    expect_err(
        r#"
        fn main() {
            let b = bytes("abc");
            b[0] = 300;
            return 0;
        }
    "#,
        "between 0 and 255",
    );
    expect_err(
        r#"
        fn main() {
            let b = bytes().from_hex("GG");
            return 0;
        }
    "#,
        "invalid hex",
    );
}

#[test]
fn npy_and_binary_io_tests() {
    expect_ok(
        r#"
        fn main() {
            let b = bytes("spid3r binary test");
            let p = "target/test_tmp.bin";
            write_bytes(p, b).unwrap();
            let b_read = read_bytes(p).unwrap();
            if b_read.to_string().unwrap() != "spid3r binary test" { return 1; }

            let v = vector([1.5, 2.5, 3.5]);
            let p_vec = "target/test_vec.npy";
            v.save_npy(p_vec).unwrap();
            let v_loaded = load_npy(p_vec).unwrap();
            if v_loaded.len() != 3 { return 2; }
            if abs(v_loaded[0] - 1.5) > 1e-9 { return 3; }
            if abs(v_loaded[2] - 3.5) > 1e-9 { return 4; }

            let m = matrix([[1.0, 2.0], [3.0, 4.0]]);
            let p_mat = "target/test_mat.npy";
            m.save_npy(p_mat).unwrap();
            let m_loaded = load_npy(p_mat).unwrap();
            if m_loaded.rows() != 2 or m_loaded.cols() != 2 { return 5; }
            if abs(m_loaded.get(0, 1) - 2.0) > 1e-9 { return 6; }
            if abs(m_loaded.get(1, 0) - 3.0) > 1e-9 { return 7; }

            return 0;
        }
    "#,
    );
}

#[test]
fn csv_parser_and_writer_tests() {
    expect_ok(
        r#"
        fn main() {
            let raw = "id,name,val,active\n1,\"Doe, John\",42.5,true\n2,Jane,null,false\n";
            let parsed = parse_csv(raw, null).unwrap();
            let headers = parsed["headers"];
            let rows = parsed["rows"];
            if headers.len() != 4 { return 1; }
            if headers[0] != "id" or headers[1] != "name" { return 2; }
            if rows.len() != 2 { return 3; }
            let r0 = rows[0];
            if r0[0] != 1 { return 4; }
            if r0[1] != "Doe, John" { return 5; }
            if abs(r0[2] - 42.5) > 1e-9 { return 6; }
            if r0[3] != true { return 7; }

            let r1 = rows[1];
            if r1[0] != 2 { return 8; }
            if r1[2] != null { return 9; }
            if r1[3] != false { return 10; }

            let csv_path = "target/test_out.csv";
            write_csv(csv_path, headers, rows).unwrap();
            let read_back = read_csv(csv_path, null).unwrap();
            if read_back["rows"].len() != 2 { return 11; }
            return 0;
        }
    "#,
    );
}

#[test]
fn regex_builtin_tests() {
    expect_ok(
        r#"
        fn main() {
            if regex_match("\\d+", "abc123xyz") != true { return 1; }
            if regex_match("^\\d+$", "abc123xyz") != false { return 2; }
            if regex_match("^\\d+$", "12345") != true { return 3; }

            let f = regex_find("[a-z]+", "123 apple 456");
            if f.unwrap() != "apple" { return 4; }

            let all = regex_find_all("\\d+", "one 1 two 22 three 333");
            if all.len() != 3 { return 5; }
            if all[0] != "1" or all[1] != "22" or all[2] != "333" { return 6; }

            let rep = regex_replace("[0-9]+", "id123_val456", "X");
            if rep != "idX_valX" { return 7; }

            return 0;
        }
    "#,
    );
}

#[test]
fn datetime_builtin_tests() {
    expect_ok(
        r#"
        fn main() {
            let now_sec = unix_time();
            if now_sec < 1700000000 { return 1; }
            let now_millis = unix_ms();
            if now_millis < 1700000000000 { return 2; }

            let p = datetime_parts(0);
            if p["year"] != 1970 { return 3; }
            if p["month"] != 1 { return 4; }
            if p["day"] != 1 { return 5; }
            if p["hour"] != 0 or p["minute"] != 0 or p["second"] != 0 { return 6; }
            if p["weekday"] != 4 { return 7; } // Thursday

            let ts = datetime_from_parts(2026, 10, 20, 15, 30, 45);
            let p2 = datetime_parts(ts);
            if p2["year"] != 2026 or p2["month"] != 10 or p2["day"] != 20 { return 8; }
            if p2["hour"] != 15 or p2["minute"] != 30 or p2["second"] != 45 { return 9; }

            let formatted = format_datetime(0, "%Y-%m-%d %H:%M:%S");
            if formatted != "1970-01-01 00:00:00" { return 10; }

            return 0;
        }
    "#,
    );
}


