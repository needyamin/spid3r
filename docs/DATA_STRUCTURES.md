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
| `list(a, b, …)` | Ordered, indexable by `list[i]`. Methods: `get(i)`, `set(i, v)`, `push`, `pop`, `remove`, `remove_at(i)`, `contains`, `sort`, `reverse`, `sorted` (new list), `reversed` (new list), `slice`, `join(sep)`, `append(other)`, `index_of`, `len`, `is_empty`, `clear`. **Higher-order (callback) methods:** `map(f)`, `filter(f)`, `reduce(init, f)`, `for_each(f)`, `all(f)`, `any(f)` — `f` is any Spid3r function; the list is snapshotted so the callback cannot mutate it mid-iteration. |
| `vec()` | Resizable array. `push_back`, `push_front`, `pop_back`, `pop_front`, `get`, `set`, `contains`, `len`. |
| `deque()` | Double-ended queue. `push_front`/`push_back`, `pop_front`/`pop_back`, `peek_front`, `len`. |
| `tuple(a, b, …)` | Fixed, immutable-length grouping; indexable by `t[i]`. |
| `ring_buffer(n)` | Fixed-capacity circular queue; overwrites the oldest when full. |

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
| `format(args…)` | `"{}` placeholders filled left to right; `{{`/`}}` escape braces. |

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

## Numeric grid

| Type | Notes |
|---|---|
| `matrix(rows, cols, fill)` | 2-D grid, element addressable by `m[i][j]` (matrix supports index assignment). |

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
