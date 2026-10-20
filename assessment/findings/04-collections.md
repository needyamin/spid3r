# 04 — Static Audit: Built-in Collections (`src/value/collections/*`)

Date: 2026-10-20. Scope owned: `mod.rs`, `seq.rs`, `nodes.rs`, `misc.rs` (~2,570 + ~1,000 + ~880 + ~1,060 lines).
Sibling files (`src/value/mod.rs`, `src/value/gc.rs`, `interpreter/`) were read/grepped **only** to confirm API contracts
(`Ref<T>`, `new_ref`, `values_equal`, `deep_clone`, `ref_id`, `MapKey`); their own defects are out of scope.
Method: full read of all four owned files; line numbers verified by targeted grep. No build/run (forbidden).

## 1. Conclusion summary

Collections are **not** Rc/RefCell-based as previously claimed — they are `Arc<Mutex<T>>` (`src/value/mod.rs:16`),
which makes lock discipline the module's central hazard. The highest-risk findings are (1) a deterministic host
panic in `bytes.from_hex` on multi-byte UTF-8 input, (2) a family of **permanent deadlocks** on self-referential
containers in methods that were *not* covered by the earlier F3 snapshot-first fixes, and (3) a potential
`sort_by` total-order panic on NaN / >2^53 int-float mixes in `list.sort()`. Pop/index/cast paths and the
equality/hashing cycle story are in good shape. Overall: several crash/hang-class defects remain, all with
concrete, user-reachable triggers.

Legend: tier = [C] primary code in owned files, [S] sibling code read for contract only, [D] doc/probe claim.

## 2. Findings (ranked: severity × confidence × user impact)

### F1 — HIGH severity / HIGH confidence: `bytes.from_hex` panics the host on UTF-8 input
- **Fact.** `bytes_from_hex` slices the input by byte offsets before validating hex: `for i in (0..s.len()).step_by(2) { let byte = u8::from_str_radix(&s[i..i + 2], 16)` — the `&s[i..i+2]` slice is evaluated **before** `from_str_radix` can reject the content. If `i+2` lands inside a multi-byte UTF-8 char, Rust panics with `byte index 2 is not a char boundary`.
- **Evidence.** `src/value/collections/misc.rs:2287-2293` (loop + slice at 2290-2291); even-length check at 2286 (`if !s.len().is_multiple_of(2)`).
- **Repro (by inspection).** `bytes.from_hex("a⚡")` — `⚡` = bytes E2 9A A1, total length 4 (even) → i=0 slice is fine ("a" + 0xE2? no: i=2 → `&s[2..4]` starts mid-char `⚡` (spans bytes 1..4)) → panic. Deterministic.
- **Impact.** Panic escapes the `Result<String,_>` boundary — crashes the whole interpreter/embedding host, from a user-supplied string.
- **Fix direction.** Iterate `s.as_bytes()` instead of string slices (validate hex on bytes).
- Tier [C]. Confidence: high (standard Rust slice semantics; no char-boundary guard present).

### F2 — HIGH severity / HIGH confidence: self-referential membership & clone-adjacent methods deadlock
- **Contract.** Method dispatch holds the receiver's `Mutex` during the whole method call (`mod.rs:453-456`, e.g. `Value::List(d) … d.lock().unwrap().method(name, &args)`). `std::sync::Mutex` is non-reentrant. Both `values_equal` and `deep_clone` **lock container mutexes** when they walk values (confirmed: `src/value/mod.rs:642-645` `x.lock().unwrap()` in the List/List arm; `mod.rs:1162-1165` in `deep_clone_inner`).
- **Cycle safety ≠ re-entrancy safety.** `values_equal` terminates on cycles via an identity fast-path + seen-set (`src/value/mod.rs:623-630`), but the fast-path only fires when **both sides are the same Arc**. A different Arc forces the (List,List) arm which locks the *receiver's own already-held* mutex → permanent hang.
- **Trigger (by inspection).** `l = list(); l.push(l); l2 = list(1); l.contains(l2)` → `contains` compares item `l` with `l2`: ref_ids differ → locks `l` again → deadlock. Same shape with `l.remove(l2)`, `l.index_of(l2)`.
- **Live paths with this pattern (all values_equal or deep_clone under the dispatch-held lock):**
  - list: `remove` seq.rs:144, `index_of` seq.rs:152, `contains` seq.rs:156, `slice` (deep_clone) seq.rs:198
  - stack: `contains` seq.rs:312, `to_list` seq.rs:318 · queue: 382 / 388 · deque: 459 / 465
  - set: `add`→`find` seq.rs:749-755, `contains` seq.rs:784, `to_list` seq.rs:803
  - map: `contains` seq.rs:647 (value scan; keys are safe scalar `MapKey`)
  - bag: `push|add` misc.rs:2110, `remove` misc.rs:2122, `contains` misc.rs:2150
  - ring_buffer: `contains` misc.rs:2068
  - graph: `index_of` misc.rs:309-311 (reached via `add_node`/`add_edge`/`has_node`); `nodes` arm deep_clones misc.rs:526
  - binary_tree: `inorder|preorder|postorder|level_order` deep_clone under lock nodes.rs:865 — `bt.insert(bt)` is legal because BinTree insert accepts any value (nodes.rs:724)
  - linked lists: `contains` nodes.rs:261, `remove_value` nodes.rs:274 — the module's own comment admits "a node value can be the list itself" (nodes.rs:298-300)
- **Contrast (already-fixed siblings of this exact bug).** Iteration snapshots first (`mod.rs:504-513`, comment cites audit F3/p01: "holding a lock across the recursion deadlocked the loop forever"), `map.values` (mod.rs:486-492, p03), linked-list `to_list` (mod.rs:471-477, p06). The arms above were missed by those fixes.
- **Impact.** No panic, no error — the task freezes forever. For a REPL/GUI host that is a full hang.
- **Fix direction.** Apply the same snapshot-under-lock pattern (`items.clone()` is an Arc-cheap shallow clone) before any `values_equal`/`deep_clone`, or make the receiver's own Arc never be re-locked (identity check per item before deep compare).
- Tier [C] + [S]. Confidence: high (logic follows directly from confirmed mutex semantics; the codebase's own comments document identical deadlocks in the sibling paths).

### F3 — HIGH severity / MEDIUM-HIGH confidence: `list.sort()` / `sorted()` can panic (total-order violation)
- **Fact.** `list.sort` pre-filters to "all numbers (Int|Float) or all strings" (seq.rs:170-176) — NaN floats and >2^53 ints pass the filter — then calls `vals.sort_by(|a, b| value_cmp(a, b).unwrap_or(std::cmp::Ordering::Equal))` (seq.rs:185). `value_cmp` returns `None` for NaN (partial_cmp, seq.rs:938-943) and folds Int→f64 (seq.rs:941: `(*x as f64).partial_cmp(y)`).
- **Panic mechanism.** Since Rust 1.81 the std sort (driftsort/ipnsort) **panics** ("user-provided comparison function does not correctly implement a total order") when the comparator is inconsistent. Two violations are constructible:
  1. NaN: `NaN ~ 1.0`, `NaN ~ 2.0`, but `1.0 < 2.0` — intransitive equivalence.
  2. Precision: `a=9007199254740993 (int) ~ b=9007199254740992.0 (float) ~ c=9007199254740992 (int)`, but `a > c` — both `a~b` and `b~c` compare Equal via the f64 cast while `a` vs `c` compares Greater.
- **Toolchain.** `Cargo.toml` pins no `rust-version` (edition 2021); a 2026 build is ≥1.81 unless deliberately downgraded. [D] The module's **own comment** states the hazard: "slice::sort_by is allowed to panic when the comparator does not implement a total order" (seq.rs:991-998) — and hand-writes `merge_sort_indices` for `sort_by`/`argsort` — yet `sort`/`sorted` (seq.rs:185, and `sorted`'s copy at seq.rs:258) still call std `sort_by`.
- **Impact.** Panic fires **while holding the container's mutex** (dispatch) → mutex poisoning → every later operation on that list panics too (cascading `lock().unwrap()`).
- **Fix direction.** Route `sort`/`sorted` through the existing `merge_sort_indices` (already total-order-tolerant: NaN lands stable, no panic), or sanitize (total_cmp / reject NaN) before std sort.
- Tier [C] + [D comment]. Confidence: medium-high (logic certain on Rust ≥1.81; on older toolchains it degrades to silently wrong order, still a bug).

### F4 — MEDIUM severity / HIGH confidence: deep-recursion crash class only partially mitigated (BST; Stack/Queue/Deque chains)
- **Fact.** The F4 fix made list/map/set **teardown** iterative with the explicit comment "iterative teardown … the original recursive Drop overflowed the stack around 10^5 levels" (`seq.rs:17-50` VecData::Drop, `seq.rs:500-543` MapData::Drop, `seq.rs:713-748` SetData::Drop). No equivalent exists for:
  - **BST traversal/removal recursion**: `remove_min` nodes.rs:523 (recursive), `walk/go` nodes.rs:579-580 (recursive), `remove` path nodes.rs:656+ — a degenerate BST built from ~10^5 sorted inserts makes every traversal/remove recursive at full depth → stack overflow (host crash).
  - **Stack/Queue/Deque/Tuple/Heap/Ring deep chains**: these types have **no custom Drop**; a chain built by `s = stack(s); s = stack(s); …` recurses in the default drop. (Deep *lists* are safe; the same nesting via stack/queue/deque is not.)
  - BinTree is safe by construction: level-order fill keeps it a complete tree (depth log2 n), though `open.remove(0)` makes building O(n²) (nodes.rs:724-756) — perf wart only.
- **Impact.** Host abort from user data — the exact class the team already fixed for lists ("attack was fixed at 10^5", seq.rs:16 comment).
- **Fix direction.** Iterative traversal with explicit stack in `walk`/`remove_min`; iterative drop or parent-pointer teardown for the remaining container types.
- Tier [C]. Confidence: high (recursion is plainly visible; the 10^5 threshold for lists is the code's own measured figure).

### F5 — MEDIUM severity / HIGH confidence: dead `clone`/`to_list` method arms diverge from live behavior (latent deadlock trap)
- **Fact.** `call_method_host` intercepts `"clone"` for **every** value before dispatch (mod.rs:433-436: `if name == "clone" { return Ok(deep_clone(obj)); }`), plus linked-list `"to_list"` (mod.rs:471-477) and map `"values"` (mod.rs:486-492). A repo-wide grep for `method("clone"` / `method("to_list")` finds **no other call sites** — so the following arms are unreachable dead code *today*:
  - `HeapData::clone` misc.rs:136, `PrioQueueData::clone` misc.rs:270-278, `GraphData::clone` misc.rs:534, `HashTableData::clone` misc.rs:684, `RingData::clone` misc.rs:2074, `BagData::clone` misc.rs:2158, `ListData::to_list` nodes.rs:367/371, Bst/BinTree clone arms nodes.rs:684-685/879-880, tuple clone misc.rs:2230, vector/matrix/bytes copy-arms misc.rs:1003/1998/2461.
  - Two of them are actively dangerous if ever called directly: `PrioQueueData::clone` does a **shallow** `it.value.clone()` (misc.rs:270-278) — different semantics from the live universal deep clone; and `GraphData::clone`/`RingData::clone` deep_clone **while the dispatch lock would be held** — instant F2-class deadlock on self-referential content.
- **Impact.** No user impact today; a maintenance trap (resurrecting an arm silently changes semantics or reintroduces the fixed deadlock class).
- **Fix direction.** Delete the dead arms or make each delegate to the universal path; add `#[cfg(test)]` asserts that dispatch interception covers them.
- Tier [C]. Confidence: high (grep-verified unreachability via the only public dispatcher).

### F6 — LOW-MEDIUM severity / HIGH confidence: unchecked size arithmetic in npy loader; latent `MatrixData::new` mismatch
- **Fact.** `load_npy_data` multiplies file-controlled dimensions without checking: `let total = rows * cols;` (misc.rs:2561) and `payload.len() != n * 8` (misc.rs:2543-2546). Debug builds panic on usize overflow; release builds wrap and produce only a misleading "payload size mismatch" error. In-memory container sizes are all bounded (MAX_VECTOR_ELEMENTS = 16,777,216, bytes ≤ 16 MiB, matrix cells via `check_matrix_dims` misc.rs:1216-1229) — the file loader is the one unbounded surface.
- **Latent.** `MatrixData::new` does `check_matrix_dims(rows, cols).unwrap_or(0)` (misc.rs:1324-1327) — on failure it keeps `rows`/`cols` but allocates 0 cells; any later `get/set` computes `self.data[r * self.cols + c]` (misc.rs:1343-1352) → out-of-bounds panic. Currently unreachable (all call sites pre-validate: mod.rs:~330 construct, matrix_from_list_val misc.rs:1230+), but a loaded footgun for the next caller.
- Tier [C]. Confidence: high.

### F7 — LOW severity / CONDITIONAL: `append` lock-order inversion across concurrent tasks
- **Fact.** `VecData::append` locks the *other* list while the receiver's lock is held by dispatch (`seq.rs:220-242`; the dispatch-level `ptr_eq` guard at mod.rs:447-451 only covers `other == self`). Two concurrent tasks running `l1.append(l2)` and `l2.append(l1)` acquire the two mutexes in opposite order → AB/BA deadlock.
- **Conditional.** Whether tasks run on parallel threads is decided by `src/sched` (sibling scope, not audited). If the scheduler is cooperative/single-threaded, no exposure.
- Tier [C] + [S]. Confidence: logic high; exposure conditional on scheduler.

## 3. Established-facts / prior probes re-verified

1. **"Containers are Rc/RefCell-based" — FALSE.** `pub type Ref<T> = Arc<Mutex<T>>` (`src/value/mod.rs:16`, `new_ref` :18). The thread-safe claim is supported (Mutex + Send bounds), but every `lock().unwrap()` (≈200 sites in the four files) panics on a poisoned mutex — which F3/F6 panics would cause, amplifying them.
2. **README "20+ containers … all covered by automatic cycle garbage collection" — overstated for this module.** Cycle *termination* is solid (values_equal seen-set mod.rs:623-630; deep_clone memo mod.rs:1154-1160; display seen mod.rs:821-822; iterative Drop for list/map/set), but cycle *reachability under a held lock* still deadlocks in the F2 paths. (gc.rs itself: sibling-owned, not audited.)
3. **p05_map_self_values — VERIFIED FIXED**: `map.values` intercepted pre-dispatch with snapshot-first (mod.rs:486-492).
4. **p07_deep_nesting_clone — PARTIALLY FIXED**: `obj.clone()` on self-referential values terminates (universal intercept mod.rs:434 + memoized deep_clone mod.rs:1154-1160, no lock held); but `slice`/`to_list` on stack/queue/deque/set and binary_tree traversals still deep_clone under the dispatch lock (→ F2), and deep *drop* chains remain possible via Stack/Queue/Deque/BST (→ F4).

## 4. What is SOUND (explicitly verified)

- **Popping/peeking empty containers never panics** — every pop/peek returns `Err`: list seq.rs:88-91, stack/queue/deque pops (seq.rs:277-499), heap misc.rs:87-96, priority queue misc.rs:228+, ring_buffer misc.rs:2049-2061, bst `remove` nodes.rs:656+, tuple `get` misc.rs:2210-2221.
- **Indexing is bounds-checked everywhere**: `want_index` rejects negatives; `get/set/insert/remove_at/slice` use `.get()`/explicit range checks (seq.rs:97-142, 184-199); `index_get`/`index_set` (mod.rs:1153-1313) use `.get()`/`get_mut()` with error messages; vector & bytes negative-index resolvers are exact (misc.rs:853-900, 2256-2273).
- **Integer casts**: ring cap requires `cap >= 1` before `as usize` (mod.rs construct); matrix dims funnel through `check_matrix_dims` (misc.rs:1216-1229); bytes values range-checked 0..=255 before `as u8` (mod.rs:1255-1262, misc.rs:2410-2425). No silent truncation found (outside F6's file path).
- **Equality & hashing of self-containing containers cannot loop**: `MapKey` is `Null|Bool|Int|Str` only (`src/value/mod.rs:65-70`) → hash_table (misc.rs:562-716) hashes scalar keys exclusively → no hash-cycle is reachable; `values_equal` terminates on cycles (seen-set) including `xs == xs` (identity fast-path).
- **`obj.clone()` on self-referential structures terminates and preserves shape** (F5 note: universal intercept + memoized deep_clone, called with no lock held).
- **Comparator paths that are clean**: `comparator_ordering` rejects NaN with a proper error (seq.rs:962-989); heap/`priority_queue.push` reject non-comparable values before mutation (misc.rs:31-44, 76-86, 215-227); `vector.sort` uses `f64::total_cmp` (total order incl. NaN); `eigen_sym` ordering uses `total_cmp` (misc.rs:1838); `argsort`/`sort_by` use the hand-written `merge_sort_indices` (seq.rs:991+) with provably in-range indices.
- **Matrix numerics** (lu_decompose/solve/inverse/det/eigen_sym, misc.rs:1577-1893): dimension and pivot checks all return `Err`; no unwraps on data; NaN propagates as a value, never panics.
- **`iter_values` / `len_of`** (mod.rs:1315-1450): snapshot-first discipline throughout, per the F3/p01 fix — no lock held across deep_clone.

## 5. 待证项 / suggested next steps

- **sched concurrency model** (F7 exposure): confirm whether tasks run on real threads → then `append`/`union`-style cross-container locks need a global lock order.
- **Toolchain floor**: pin `rust-version = 1.8x` in Cargo.toml or route F3 through merge_sort_indices; a quick fuzz (`[1.0, nan, 2.0].sort()`, `[2^53+1, 2^53.0, 2^53].sort()`) would confirm the panic empirically (build/run forbidden in this audit).
- **gc.rs cycle collection** (sibling scope): whether Arc-cycle garbage (e.g. `l.push(l)` then drop all roots) is actually reclaimed — the README claim's strongest half remains unverified here.
- Sweep remaining `lock().unwrap()` sites for poison-cascade hardening (`unwrap_or_else(|e| e.into_inner())`, already used in VecData::Drop seq.rs:31) if panic-resilience is desired.
