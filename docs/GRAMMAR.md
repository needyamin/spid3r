# Spid3r Grammar

EBNF for the parser. One `program` is one `.matrix` file. This grammar was frozen
before the parser was written (Phase 0); a reviewer can implement the parser from
this document without guessing.

```
program        = { import } { declaration } ;

import         = "import" string ";" ;

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
               | orExpr ;

target         = ident
               | postfix "[" expr "]"            (* index assignment *)
               | postfix "." ident ;             (* field assignment, e.g. self.id *)

assignOp       = "=" | "+=" | "-=" | "*=" | "/=" | "%=" ;

orExpr         = andExpr { "or" andExpr } ;
andExpr        = cmpExpr { "and" cmpExpr } ;
cmpExpr        = addExpr { cmpOp addExpr } ;
cmpOp          = "==" | "!=" | "<" | "<=" | ">" | ">=" ;
addExpr        = mulExpr { ("+" | "-") mulExpr } ;
mulExpr        = unary { ("*" | "/" | "%") unary } ;

unary          = "not" unary | "-" unary | postfix ;

postfix        = primary { call | index | field | method } ;
call           = "(" [ args ] ")" ;
index          = "[" expr "]" ;
field          = "." ident ;
method         = "." ident "(" [ args ] ")" ;
args           = expr { "," expr } ;

primary        = intLit | floatLit | stringLit | "true" | "false" | "null"
               | ident | listLit | fnLit | "new" ident "(" [ args ] ")"
               | "(" expr ")" ;

listLit        = "[" [ args ] "]" ;
fnLit          = [ "async" ] "fn" "(" [ params ] ")" block ;
```

`await` binds at unary strength: `await t` parses, `await t.field` parses as
`await (t.field)`. A statement that is only an `await` still needs its `;`.

## Notes that the parser enforces

- `else` always belongs to the nearest unfinished `if` (no dangling-else
  ambiguity: `else` may only directly follow an if-block or an elif-block).
- A `let` inside a C-style `for` header consumes no trailing `;` beyond the one
  written in the header.
- Method names may be any identifier, including the shared verbs `len`, `push`,
  and `get`; those are not keywords, so `item.get(0)` and `map.get("k")` both
  parse.
- Index assignment targets `list`, `vec`, `matrix`, `hash_table`, and `map`
  (`m[k] = v` is an alias for `m.set(k, v)`). The semantic pass rejects it on
  every other type.
- String literal escapes are `\n`, `\t`, `\\`, and `\"`; identifiers are ASCII
  (`[A-Za-z_][A-Za-z0-9_]*`).
- A `public`/`private` prefix is accepted before any top-level declaration
  (`fn`, `class`, `interface`, `let`, `const`); class bodies additionally accept
  a constructor shorthand `new(params) { ... }` and a C-style `for let` header
  may omit the initializer or carry a type annotation.
- `self` may appear only as a parameter name and inside method bodies.
