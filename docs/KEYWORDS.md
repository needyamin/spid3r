# Spid3r Keyword Registry

Frozen before the parser was written. Every token below is a **reserved keyword**:
lowercase English, one token, case-sensitive. A keyword inside a string literal is
text, not a keyword (`"class"` is the six characters `class`, nothing more).

## 1. Core keywords

| Concept | Token |
|---|---|
| Function | `fn` |
| Variable | `let` |
| Constant | `const` |
| If | `if` |
| Else if | `elif` |
| Else | `else` |
| While | `while` |
| For | `for` |
| In | `in` |
| Break | `break` |
| Continue | `continue` |
| Return | `return` |
| True | `true` |
| False | `false` |
| Null | `null` |
| And | `and` |
| Or | `or` |
| Not | `not` |
| Import | `import` |
| Try | `try` |
| Catch | `catch` |
| Finally | `finally` |
| Throw | `throw` |
| Class | `class` |
| Abstract | `abstract` |
| Yield | `yield` |
| Interface | `interface` |
| Construct | `new` |
| Self | `self` |
| Public | `public` |
| Private | `private` |
| Spawn task | `spawn` |
| Async | `async` |
| Await | `await` |
| Extends | `extends` |
| Implements | `implements` |
| Match | `match` |
| Enum | `enum` |
| Super | `super` |
| Import alias | `as` |

Rules that come with the registry:

- `async` is allowed on functions that `await`. A non-async function that uses
  `await` is a **semantic error**.
- `self` is the explicit receiver parameter of methods. Methods take `self`
  explicitly; there is no implicit receiver.
- `new` is the constructor call: `new User(7, "hidden")`. Inside a class the
  constructor is the method written `fn new(...)`.
- `spawn` takes a function expression: `spawn fn() { ... };`. It returns a `task`.
- `await` waits on a `task` (joins it) or a `channel` (receives one message).

## 2. Type names

These are **not** keywords. They are ordinary identifiers bound to builtin
constructors in the global scope, so `list`, `map`, and `heap` can be shadowed by
user declarations (the semantic pass warns nothing; the builtin is then simply not
reachable under that name).

| Kind | Names |
|---|---|
| Primitives | `int`, `float`, `bool`, `string` |
| Sequences | `list`, `vec`, `deque`, `tuple`, `ring_buffer`, `bytes` |
| Stacks & queues | `stack`, `queue`, `priority_queue` |
| Associative | `map`, `set`, `bag`, `hash_table` |
| Linked nodes | `linked_list`, `singly_linked_list`, `doubly_linked_list` |
| Trees | `binary_tree`, `bst`, `trie` |
| Numeric grid & vector | `matrix`, `vector` |
| Graph | `graph` |
| Shared state | `ref`, `mutex`, `channel` |
| Sum types | `option`, `result` |
| Concurrency | `task` |

Constructors are called like functions: `list(10, 20, 30)`, `stack()`,
`graph("undirected")`, `heap("min")`, `matrix(2, 3, 0.0)`, `vector(list(1.0, 2.0))`, `bytes("abc")`,
`ring_buffer(4)`, `ref(1)`, `some(5)`, `none`, `ok(1)`, `err("bad")`, `channel()`, `mutex(0)`.

## 3. Operators and delimiters

| Group | Tokens |
|---|---|
| Arithmetic | `+` `-` `*` `/` `%` |
| Bitwise | `&` `|` `^` `<<` `>>` |
| Comparison | `==` `!=` `<` `<=` `>` `>=` |
| Logical | `and` `or` `not` |
| Null safety & coalescing | `?.` `??` `?` |
| Assignment | `=` `+=` `-=` `*=` `/=` `%=` |
| Pattern Matching & Functions | `=>` `->` |
| Decorator | `@` |
| Grouping | `(` `)` `[` `]` `{` `}` |
| Punctuation | `,` `:` `;` `.` |

Statements end with `;`. Blocks use `{ }`. `->` is used in function return
type annotations (`fn add(a, b) -> int { ... }`), `=>` separates pattern arms in `match`,
and `@` annotates functions and classes.

## 4. Identifiers

An identifier starts with an ASCII letter or `_`, then continues with letters,
digits, and `_`. Identifiers are case-sensitive. Keywords are not identifiers.

## 5. Comments

`//` runs to the end of the line. `/* ... */` is a block comment and does not
nest.

## 6. Standard library names

Global functions (see `STANDARD_LIBRARY.md`): `print`, `println`, `eprint`,
`read_line`, `read_file`, `write_file`, `file_exists`, `sleep_ms`, `now`, `env`,
`args`, `spawn_process`, `yield`, `task_count`, `len`, `abs`, `sqrt`, `pow`,
`min`, `max`, `int`, `float`, `string`, `bool`, `random_int`, `json_parse`,
`json_write`.

Method verbs shared by collections (same name, same meaning everywhere):
`len`, `is_empty`, `clear`, `push`, `pop`, `peek`, `get`, `remove`, `contains`.
Per-structure methods are listed in `DATA_STRUCTURES.md`.
