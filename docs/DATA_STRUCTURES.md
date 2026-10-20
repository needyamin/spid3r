# Spid3r Built-in Data Structures

Every container is a value created by calling a constructor function
(`list(...)`, `map()`, `graph("undirected")`, …). Constructors are ordinary
globals, so they can be shadowed. All containers share a common vocabulary:
`len`, `is_empty`, `clear`, plus `push`/`pop`/`peek`/`get`/`contains` where they
make sense. Methods are called with dot syntax (`s.push(x)`); passing the wrong
argument type throws a descriptive runtime error.

Constructors accept initial elements where it is natural:

```
let xs   = list(10, 20, 30);
let m    = map("a", 1, "b", 2);
let q    = queue();
let g    = graph("undirected");
let h    = heap("min");
let mx   = matrix(2, 3, 0.0);
let rb   = ring_buffer(4);
let cell = mutex(0);
let ch   = channel();
let box  = ref(1);             // a shared, interior-mutable cell
let yes  = some(5);            // option
let no   = none;
let okv  = ok(1);  let errv = err("bad");   // result
```

## Sequences

| Type | Notes |
|---|---|
| `list(a, b, …)` | Ordered, indexable by `list[i]`. Methods: `get(i)`, `set(i, v)`, `push`, `pop`, `remove`, `remove_at(i)`, `contains`, `sort`, `reverse`, `sorted` (new list), `reversed` (new list), `slice`, `join(sep)`, `append(other)`, `index_of`, `len`, `is_empty`, `clear`. **Higher-order (callback) methods:** `map(f)`, `filter(f)`, `reduce(init, f)`, `for_each(f)`, `all(f)`, `any(f)`, `sort_by(cmp)`, `sorted_by(cmp)`, `argsort()` — see [Sorting with a comparator](#sorting-with-a-comparator) and [Formatting floats](#formatting-floats). |
| `vec()` | Resizable array. `push_back`, `push_front`, `pop_back`, `pop_front`, `get`, `set`, `contains`, `len`. |
| `deque()` | Double-ended queue. `push_front`/`push_back`, `pop_front`/`pop_back`, `peek_front`, `len`. |
| `tuple(a, b, …)` | Fixed, immutable-length grouping; indexable by `t[i]`. |
| `ring_buffer(n)` | Fixed-capacity circular queue; overwrites the oldest when full. |

## Sorting with a comparator

`list` adds three ways to sort beyond the natural `sort`/`sorted`:

| Method | Meaning |
|---|---|
| `xs.sort_by(cmp)` | Sort **in place** using `cmp`; returns `null`. |
| `xs.sorted_by(cmp)` | Return a **new** sorted list; `xs` is untouched. |
| `xs.argsort()` | Return a list of the original indices in ascending order. |

`argsort()[i]` is the position in the *unsorted* list of the element that
belongs at position `i`, so `xs.argsort()` tells you how to permute `xs`
into sorted order:

```
let xs = list(30, 10, 20);
let order = xs.argsort();        // [1, 2, 0]
let by_order = list();
for i in order { by_order.push(xs[i]); }   // [10, 20, 30]
```

`argsort` uses the same natural order as `sort` — all numbers (ints and floats
may mix) or all strings, and it throws a catchable error otherwise.

### Writing a comparator

`cmp(a, b)` is called with two elements and answers how they order. It may
return **either**:

- a `bool`: `true` means "`a` comes before `b`";
- an `int` or `float`: negative means "before", `0` means "equal", positive
  means "after".

```
xs.sort_by(fn(a, b) { return a < b; });          // bool form
xs.sort_by(fn(a, b) { return b - a; });          // descending, numeric form
people.sort_by(fn(a, b) { return a["age"] < b["age"]; });
```

Sorts are **stable**: equal elements keep the order they had in the input.
A comparator returning anything other than a bool/int/float — or one that
returns `NaN` — throws a catchable error rather than guessing.

Because a boolean `false` cannot distinguish "after" from "equal", the
implementation asks the mirrored question (`cmp(b, a)`) whenever `cmp(a, b)`
answers `false`. That is what preserves stability; it also means a comparator
may be called more than once per pair, so keep it side-effect free.

### Errors and re-entrancy

The list is snapshotted before any comparison, and no lock is held while a
comparator runs. A comparator that throws, mutates the receiver, or touches a
self-referential list therefore gets a clean error or a deterministic result —
never a deadlock and never a host panic. `sort_by` writes the sorted snapshot
back into the receiver *after* the last comparator call, so a comparator that
mutates the list cannot change the outcome. A throw propagates with the thrown
value, and the list is left as it was.

## Stacks & queues

| Type | Notes |
|---|---|
| `stack()` | LIFO. `push`, `pop`, `peek`, `len`, `is_empty`. `pop` on an empty stack throws. |
| `queue()` | FIFO. `push` (enqueue), `pop` (dequeue), `peek_front`, `len`, `is_empty`. |
| `priority_queue("min"\|"max")` | Heap-backed; `push`, `pop`, `peek`, `len`. |

## Strings

Strings are immutable values with a method suite (dot syntax):

| Method | Notes |
|---|---|
| `len`, `is_empty` | Length in characters. |
| `slice(a, [b])` | Character-indexed slice; bounds-checked. |
| `upper`, `lower`, `trim`, `ltrim`, `rtrim` | Case and whitespace. |
| `split(sep)`, `join` (on lists) | Split returns a `list` of strings. |
| `contains`, `starts_with`, `ends_with`, `index_of(sub)` | Search; `index_of` is `-1` when absent. |
| `replace(from, to)`, `repeat(n)` | Build new strings. |
| `reverse`, `chars` | Reverse / `list` of 1-char strings. |
| `pad_left(w, pad)`, `pad_right(w, pad)` | Pad to width `w` with `pad` (cycled). |
| `format(args…)` | `"{}` placeholders filled left to right; `{{`/`}}` escape braces. A placeholder may also carry a precision: `"{:.2}"` renders its number with exactly 2 fractional digits — see [Formatting floats](#formatting-floats). |

## Formatting floats

A `format` placeholder may carry a precision, written `{:.N}` where `N` is
`0` through `15`:

```
"{:.2}".format(88.6875)   // "88.69"
"{:.0}".format(2.5)       // "2"
"[{:.2}]".format(1.0)     // "[1.00]"
"{:.0}".format(5)         // "5"  (an int is widened, so it gets a fixed point)
```

`N` is the number of digits printed *after* the decimal point. Rounding uses
the same tie-to-even rule as `{:.N}` in Rust, so `2.5` and `3.5` at precision
`0` print `2` and `4` — note this differs from `round(x)`, which rounds halves
away from zero.

Anything else keeps its existing meaning: `{}` (and a bare name inside braces)
fills with the element's display form, and `{{`/`}}` are literal braces. A
precision larger than 15, a placeholder that is not a number, or a malformed
spec such as `{:.}` or `{:x}` is a catchable error rather than a silently
ignored format.

## Associative

| Type | Notes |
|---|---|
| `map(k, v, …)` | Insertion-order map. `get`, `set`/index-assign `m[k] = v`, `remove`/`delete`, `has`/`contains`, `keys`, `values`, `items` (list of `(k, v)` tuples), `len`, `is_empty`, `clear`. |
| `set(a, b, …)` | Unique members. `add`, `remove`, `contains`, `union`, `intersection`/`intersect`, `difference`, `is_subset`, `is_superset`, `len`, `is_empty`, `clear`. |
| `bag()` | Multiset (counts duplicates). `add`, `count`, `remove`, `len`. |
| `hash_table(n)` | Open addressing; `get`, `set`, `remove`, `contains`, `exists`, `len`, `clear`. |

## Linked nodes

| Type | Notes |
|---|---|
| `linked_list()` | Singly linked list. `push_back`, `push_front`, `pop_front`, `pop_back`, `insert_after`, `set_at`, `remove_at`, `contains`, `peek`, `len`, `reverse`. |
| `singly_linked_list()` | Singly linked list (same surface as `linked_list`). |
| `doubly_linked_list()` | Doubly linked list; fast `pop_back`, `push_front`/`push_back`. |

## Trees & heaps

| Type | Notes |
|---|---|
| `binary_tree(a, b, …)` | Level-order built. `insert`, `remove`, `find`, `min`, `max`, `traverse`, `depth`, `n_nodes`, `n_leaves`, `height`. |
| `bst()` | Binary search tree. `insert`, `remove`, `get`, `min`, `max`, `find_closest`, `get_ith`, `traverse` (inorder / preorder / postorder), `depth`, `n_nodes`, `n_leaves`, `height`. |
| `trie()` | Prefix tree of strings. `add(word)`, `exists(word)`, `remove(word)`, `collect(prefix)`, `n_words`, `is_empty`. |
| `heap("min"\|"max")` | Min/max heap. `push`, `pop`, `peek`, `change_priority`, `merge`, `len`, `is_empty`. |

## Numeric Vector & Matrix (Linear Algebra)

Spid3r provides high-performance, contiguous 64-bit float storage for 1-D vectors and 2-D matrices with zero external dependencies. Both types are capped at 16,777,216 elements to protect system memory.

### Vector (`vector`)

A contiguous 1-D buffer of `f64` values.

| Constructor | Meaning |
|---|---|
| `vector(n, fill_val)` | Creates an `n`-element vector filled with `fill_val` (defaults to `0.0`). |
| `vector(list_of_numbers)` | Converts a `list` or `tuple` of `int`/`float` into a `vector`. |

#### Indexing & Iteration
- **Indexing**: `v[i]` reads or writes the `float` at index `i`. Supports negative indexing (e.g. `v[-1]` is the last element). Throws a catchable error on out-of-bounds access.
- **Iteration**: `for x in v { ... }` yields each element as a `float`.
- **Length**: `len(v)`.

#### Native Methods
- **Basics**:
  - `v.len()`: Number of elements (`int`).
  - `v.is_empty()`: `true` if empty.
  - `v.get(i)`: Element at index `i` (supports negative indices).
  - `v.set(i, val)`: Set element at index `i` (supports negative indices).
  - `v.push(val)`: Append a number to the vector.
  - `v.slice(start, end)`: Returns a new sub-vector (supports negative indices and clamping).
  - `v.to_list()`: Converts vector to a `list` of floats.
  - `v.copy()` / `v.clone()`: Returns an independent clone.
- **Elementwise Arithmetic**:
  - `v.add(other)`, `v.sub(other)`, `v.mul(other)`, `v.div(other)`: Accepts either another `vector` of identical length or a scalar (`int` or `float`). Returns a new `vector`. Division by zero throws a catchable runtime error.
- **Fast Native Reductions & Stats**:
  - `v.sum()`: Sum of elements.
  - `v.mean()`: Arithmetic mean.
  - `v.variance()`: Population variance ($N$).
  - `v.std()`: Population standard deviation.
  - `v.min()`, `v.max()`: Minimum / maximum value.
  - `v.argmin()`, `v.argmax()`: Index of minimum / maximum value.
  - `v.dot(other_vec)`: Dot product with another vector of identical length.
  - `v.norm()`: Euclidean $L_2$ norm ($\sqrt{\sum v_i^2}$).
  - `v.cumsum()`: Cumulative sum as a new `vector`.
  - `v.diff()`: Forward differences ($v_{i+1} - v_i$) as an $(N-1)$-element `vector`.
  - `v.sort()`: In-place stable sort (using IEEE 754 total ordering).
  - `v.argsort()`: Returns a `list` of integer indices representing sorted order.

### Matrix (`matrix`)

A contiguous row-major 2-D grid of `f64` floats.

| Constructor | Meaning |
|---|---|
| `matrix(rows, cols, fill)` | 2-D grid of floats (`rows` x `cols`). `fill` defaults to `0.0`. |
| `matrix(rows_list)` / `matrix_from_list(rows_list)` | Construct from a list/tuple of row sequences. |

### Matrix indexing: `m[i]` is a *flat* index, not a row

A `matrix` stores its cells in one contiguous row-major buffer, and `m[i]`
addresses that buffer directly — `i` counts cells from the top-left, *not*
rows. So on a 2x3 matrix:

```
let mx = matrix(2, 3, 0.0);

mx[0] = 1.0;          // flat cell 0  == (row 0, col 0)
mx[1] = 2.0;          // flat cell 1  == (row 0, col 1)
mx[2] = 3.0;          // flat cell 2  == (row 0, col 2)
mx[3] = 4.0;          // flat cell 3  == (row 1, col 0)

println(mx[3]);       // 4  — a float, read from the flat buffer
mx[4] = 9.0;          // flat cell 4  == (row 1, col 1): index assignment works
mx[7] = 0.0;          // throws: 7 is out of bounds for 6 cells
len(mx);              // 6  == rows * cols
```

`m[i]` returns a `float` and never a list, so `m[i][j]` is a type error —
there is no second dimension behind a flat index. For 2-D addressing use
`get`/`set`, which take `(row, col)` and bounds-check both:

| Method | Meaning |
|---|---|
| `m.get(r, c)` | the `float` at row `r`, column `c` |
| `m.set(r, c, v)` | write `v` into row `r`, column `c` |
| `m.row(r)` | row `r` as a new `list` of floats |
| `m.col(c)` | column `c` as a new `list` of floats |
| `m.rows()` / `m.cols()` | dimensions as `int` |
| `m.to_list()` | nested `list` of row float lists |
| `m.transpose()` | returns transposed matrix ($A^T$) |
| `m.scale(s)` | scales all elements by scalar `s` |
| `m.add(other)` | matrix addition ($A + B$) |
| `m.sub(other)` | matrix subtraction ($A - B$) |
| `m.hadamard(other)` | elementwise (Schur / Hadamard) multiplication |
| `m.mul(other)` | standard matrix multiplication ($A \cdot B$) |
| `m.trace()` | sum of diagonal elements (square matrices only) |
| `m.sum(axis)` | sum: `axis=null` overall float, `axis=0` 1xCols matrix, `axis=1` Rowsx1 matrix |
| `m.mean(axis)` | mean: `axis=null` overall float, `axis=0` 1xCols matrix, `axis=1` Rowsx1 matrix |
| `m.solve(b)` | solves $A X = B$ via LU decomposition with partial pivoting (returns `vector` or `matrix`) |
| `m.inverse()` | matrix inverse $A^{-1}$ via LU decomposition |
| `m.det()` | determinant $\det(A)$ (returns 0.0 if singular) |
| `m.eigen_sym()` | Jacobi eigensolver for symmetric matrices -> `map("values", vector, "vectors", matrix)` |
| `m.is_empty()` | `true` when `rows == 0` or `cols == 0` |
| `m.clone()` / `m.copy()` | an independent copy |

The cell count is `len(m)` (`rows * cols`); the `matrix` type has no `push`
or `pop`, and `m[i] = v` writes a cell rather than appending one.

`m.get`/`m.set` and the flat index agree by construction: for a matrix with
`cols` columns, `mx[r * mx.cols + c]` and `mx.get(r, c)` are the same cell.
A single matrix may hold at most 16,777,216 cells; larger dimensions throw a
catchable error instead of attempting the allocation. Singular matrices in `solve`
and `inverse` throw catchable runtime errors (`"matrix is singular"`).

## Graphs

| Type | Notes |
|---|---|
| `graph("directed"\|"undirected")` | Adjacency map (may be cyclic). `add_node`, `remove_node`, `add_edge`, `remove_edge`, `neighbors`, `n_nodes`, `n_edges`, `bfs`, `dfs`, `path_exists`, `find_path`, `is_connected`, `cycle`, `replace`, `clear`. |

## Shared state & concurrency

| Type | Notes |
|---|---|
| `ref(v)` | Interior-mutable cell: `get()`, `set(v)`. Safe to share across tasks. |
| `mutex(v)` | Guarded cell: `m.lock()` returns the value and holds a RAII guard, auto-released on the calling scope's exit. |
| `channel()` | Unbuffered message channel. `send(v)`, `recv()`, `try_recv()`, `state()`, `len()`. |
| `task` | Returned by `spawn`. Joined with `await`; `status()` and `join()`. |

## Sum types

| Type | Notes |
|---|---|
| `option` | `some(v)` / `none`. `is_some`, `is_none`, `unwrap`, `unwrap_or(d)`. |
| `result` | `ok(v)` / `err(e)`. `is_ok`, `is_err`, `unwrap`, `unwrap_or(d)`, `unwrap_err`. |

## Errors

Methods validate their arguments and throw on violation rather than silently
misbehaving — for example `pop` on an empty `stack`, an out-of-range `get(i)`,
or calling a method a collection does not have. These are ordinary thrown
values, so they are catchable with `try`/`catch`. See
[`examples/`](../examples) and the differential suite for concrete behavior.
