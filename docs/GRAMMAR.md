# Spid3r Grammar

EBNF for the parser. One `program` is one `.matrix` file. This grammar was frozen
before the parser was written (Phase 0); a reviewer can implement the parser from
this document without guessing.

```
program        = { import } { declaration } ;

import         = "import" string [ "as" ident ] ";" ;

declaration    = fnDecl | classDecl | interfaceDecl | statement ;

fnDecl         = [ visibility ] [ "async" ] "fn" ident "(" [ params ] ")"
                 [ "->" type ] block ;

classDecl      = "class" ident [ "extends" ident ]
                 [ "implements" ident { "," ident } ] "{" { member } "}" ;

member         = fieldDecl | methodDecl ;

fieldDecl      = [ visibility ] "let" ident [ ":" type ] ";" ;

methodDecl     = [ visibility ] [ "async" ] "fn" ident "(" [ params ] ")"
                 [ "->" type ] block ;

interfaceDecl  = "interface" ident "{" { ifaceMethod } "}" ;

ifaceMethod    = "fn" ident "(" [ params ] ")" [ "->" type ] ";" ;

params         = param { "," param } ;
param          = "self" | ident [ ":" type ] ;

type           = ident [ "<" type ">" ] ;

statement      = letStmt | constStmt | ifStmt | whileStmt | forStmt
               | returnStmt | breakStmt | continueStmt | block | spawnStmt
               | tryStmt | throwStmt | exprStmt ;

letStmt        = "let" ident [ ":" type ] [ "=" expr ] ";" ;
constStmt      = "const" ident [ ":" type ] "=" expr ";" ;

ifStmt         = "if" expr block { "elif" expr block } [ "else" block ] ;
whileStmt      = "while" expr block ;

forStmt        = "for" ident "in" expr block
               | "for" "let" ident "=" expr ";" expr ";" expr block ;

returnStmt     = "return" [ expr ] ";" ;
breakStmt      = "break" ";" ;
continueStmt   = "continue" ";" ;

spawnStmt      = "spawn" fnLit ";" ;            (* the operand must be a fn literal;
                                                  a `spawn` expression is also
                                                  accepted in unary position *)

tryStmt        = "try" block [ "catch" "(" ident ")" block ] [ "finally" block ] ;
                                                (* at least one of catch/finally
                                                  is required *)
throwStmt      = "throw" expr ";" ;

block          = "{" { statement } "}" ;
exprStmt       = expr ";" ;
```

## Expressions

Pratt parser. Precedence, tightest first:

1. calls, indexing, field access, method calls (postfix)
2. unary `not`, unary `-`
3. `*` `/` `%`
4. `+` `-`
5. comparisons `==` `!=` `<` `<=` `>` `>=`
6. `and`
7. `or`
8. assignment (right associative, lowest)

```
expr           = assignment ;

assignment     = target assignOp expr
               | "await" expr
               | coalesceExpr ;

target         = ident
               | postfix "[" expr "]"            (* index assignment *)
               | postfix "." ident ;             (* field assignment, e.g. self.id *)

assignOp       = "=" | "+=" | "-=" | "*=" | "/=" | "%=" ;

coalesceExpr   = orExpr { "??" orExpr } ;
orExpr         = andExpr { "or" andExpr } ;
andExpr        = bitorExpr { "and" bitorExpr } ;
bitorExpr      = bitxorExpr { "|" bitxorExpr } ;
bitxorExpr     = bitandExpr { "^" bitandExpr } ;
bitandExpr     = cmpExpr { "&" cmpExpr } ;
cmpExpr        = shiftExpr { cmpOp shiftExpr } ;
cmpOp          = "==" | "!=" | "<" | "<=" | ">" | ">=" ;
shiftExpr      = addExpr { ("<<" | ">>") addExpr } ;
addExpr        = mulExpr { ("+" | "-") mulExpr } ;
mulExpr        = unary { ("*" | "/" | "%") unary } ;

unary          = "not" unary | "-" unary | postfix ;

postfix        = primary { call | index | field | method | optField | "?" } ;
call           = "(" [ args ] ")" ;
index          = "[" expr "]" ;
field          = "." ident ;
method         = "." ident "(" [ args ] ")" ;
optField       = "?" "." ident ;          (* null-safe field read *)
propagate      = expr "?" ;               (* return the failure, else unwrap *)
args           = expr { "," expr } ;

primary        = intLit | floatLit | stringLit | "true" | "false" | "null"
               | ident | listLit | fnLit | "new" ident "(" [ args ] ")"
               | "super" | ifExpr | matchExpr | "(" expr ")" ;

(* `if` in value position; a branch's last expression may omit its `;` *)
ifExpr         = "if" "(" expr ")" valueBlock [ "else" valueBlock ] ;

(* `match` is an expression; in statement position its trailing `;` is optional *)
matchExpr      = "match" "(" expr ")" "{" arm { [ "," ] arm } "}" ;
arm            = pattern [ "if" expr ] "=>" ( valueBlock | expr ) ;
pattern        = "_" | ident | literal
               | ident "(" [ pattern { "," pattern } ] ")" ;

listLit        = "[" [ args ] "]" ;
fnLit          = [ "async" ] "fn" "(" [ params ] ")" block ;
```

`await` binds at unary strength: `await t` parses, `await t.field` parses as
`await (t.field)`. A statement that is only an `await` still needs its `;`.

## Notes that the parser enforces

- `else` always belongs to the nearest unfinished `if` (no dangling-else
  ambiguity: `else` may only directly follow an if-block or an elif-block).
  `else if (c) { … }` is accepted as a synonym for `elif (c) { … }` and the two
  may be mixed in one chain.
- The `[]` operator accepts a negative index on a list, tuple or string: `-1` is
  the last element and `-len` the first. Method arguments (`remove`, `insert`,
  `slice`) take non-negative indices only.
- A `let` inside a C-style `for` header consumes no trailing `;` beyond the one
  written in the header.
- The parenthesized `for` form accepts one or two variables:
  `for (k, v) in map` and `for (x) in list` are both `for-in`; the bare
  `for x in list` is unchanged.
- Method names may be any identifier, including the shared verbs `len`, `push`,
  and `get`; those are not keywords, so `item.get(0)` and `map.get("k")` both
  parse.
- Index assignment targets `list`, `vec`, `matrix`, `hash_table`, and `map`
  (`m[k] = v` is an alias for `m.set(k, v)`). The semantic pass rejects it on
  every other type.
- A float literal may carry an exponent: `1.5e3`, `1e-5` and `2E+4` all lex
  as one float token. The `e`/`E` must be followed (after an optional `+` or
  `-`) by at least one digit, so `1e` is the int `1` followed by the
  identifier `e`, and `1elif` is `1` then `elif`. A float literal too large
  for an `f64` is a lexer error, never an infinity or a panic.
- Integer literals may carry a radix prefix: `0x`/`0X` (hex), `0o`/`0O`
  (octal), `0b`/`0B` (binary), each followed by at least one digit of that
  radix. Underscores may separate digits in any digit run — decimal, radix,
  fraction, or exponent (`1_000`, `0xDEAD_BEEF`, `1_0.5_0`) — and must sit
  between two digits. A prefix without a digit is not a radix literal:
  `0x` lexes as the decimal `0` followed by the identifier `x`, matching the
  `1elif` rule above.
- String literal escapes are `\n`, `\r`, `\t`, `\\`, and `\"`; identifiers are ASCII
  (`[A-Za-z_][A-Za-z0-9_]*`).
- A string literal may contain interpolation holes: `{name}` or `{obj.field}`
  with **no whitespace** inside the braces. `"hi {name}"` desugars at parse time
  to `"hi " + string(name)`, so a hole is an ordinary expression to the checker
  (an unknown name is reported as `unknown name 'x'`). Anything else starting
  with `{` is literal text — `"{ x }"`, `"{{ user }}"`, `"{}"` and `"{0}"` all
  stay as written, which keeps the stdlib template syntax working. There is no
  `{{` escape: a hole is recognised only when `{` is followed immediately by an
  identifier.
- `??` binds looser than `or` and tighter than assignment; `?.` is postfix.
  `??` is **null**-coalescing (PHP semantics): the right side is evaluated
  only when the left side is `null`. `none` is an option value, not null, so
  `none ?? fallback` yields `none`; use `.unwrap_or(fallback)` for options.
  Both, and the value-position `if`, are rewritten by `crate::lower` into a
  temporary plus statements before any backend sees them, so they cost no engine
  support. A `while` condition that needs such a temporary keeps its
  once-per-iteration semantics (it becomes `while (true) { <prelude>; if (not c)
  { break } … }`).
- A branch block of a value-position `if` may omit the `;` after its final
  expression, because that expression is the branch's value. Everywhere else an
  expression statement still needs its `;`.
- `let (a, b) = expr;` destructures a list/tuple; it lowers to one `let` per
  name sharing a temporary, so the value is evaluated once. `for (k, v) in m`
  walks a map's entries and lowers to a key loop with a lookup in the body.
- `spawn` takes any expression that evaluates to a function (`spawn work`,
  `spawn closure_var`), not just a `fn` literal.
- `enum Name { A, B(x), C(x, y) }` declares a sum type. Each variant becomes a
  `public` constructor function (`A()`, `B(1)`) returning a map with a `__tag`
  key, so a variant works across `import` like any other function. A `match`
  compiles to a chain of guarded `if`s: arms are tried in order and a failed
  guard falls through to the next arm. A match that names variants of one enum
  and has no `_`/binding arm must cover every variant (a semantic error
  otherwise); otherwise an unmatched value throws at run time.
- Expression/statement nesting is capped at 64 levels; deeper input is a parser
  diagnostic, never a stack overflow.
- Bitwise `& | ^ << >>` follow C precedence and are desugared to the natives
  `bit_and`, `bit_or`, `bit_xor`, `shl`, `shr`, so they are not separate opcodes.
  They require `int` operands; a shift amount outside `0..=63` is an error.
- A `public`/`private` prefix is accepted before any top-level declaration
  (`fn`, `class`, `interface`, `let`, `const`); class bodies additionally accept
  a constructor shorthand `new(params) { ... }` and a C-style `for let` header
  may omit the initializer or carry a type annotation.
- Inside a class body, a member without an explicit `public`/`private` prefix
  is **private** — including a method that overrides a public parent method.
  Write `public` on every member meant to be called from outside.
- `super` is valid only inside a class method or constructor:
  `super(args)` runs the parent class's constructor on the current instance,
  `super.name(args)` calls the parent class's implementation of `name` with
  the current instance as the receiver. The lookup starts at the parent of
  the class where the method is written, so a subclass that does not override
  a method still reaches the grandparent implementation. `super` used anywhere
  else is a semantic error, as is calling a method the parent does not have
  or passing the wrong number of arguments. The AOT backend does not support
  `super` yet (`--engine vm` runs it).
- `self` may appear only as a parameter name and inside method bodies.
