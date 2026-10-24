# Spid3r Language Server (`spid3r lsp`)

`spid3r lsp` speaks the [Language Server Protocol](https://microsoft.github.io/language-server-protocol/)
over stdio: JSON-RPC 2.0 messages framed with `Content-Length` headers, the
transport every editor client (VS Code, Neovim, Emacs, Zed, …) speaks natively.

```bash
spid3r lsp
```

The server reads requests from stdin and writes responses and notifications to
stdout. It never touches the filesystem for the text it works on — everything
comes from the client (`didOpen`/`didChange`), so unsaved buffers are analysed
exactly as they are typed.

## Why it exists

Editor support was the top adoption barrier for the language. It is also the
cheapest one to close: the language already owns a lexer, a parser, a semantic
checker with a lint pass, and a formatter. An LSP is a thin JSON-RPC shell
around code that already exists and is already tested — so the editor and the
CLI can never disagree about whether a file is correct.

## Supported methods

| Method | Direction | Notes |
|---|---|---|
| `initialize` | → client | Capability advertisement (below) |
| `initialized` | ← client | Accepted, no reply |
| `shutdown` / `exit` | ← client | `exit` returns 0 only when `shutdown` was called first |
| `textDocument/didOpen` | ← client | Publishes diagnostics |
| `textDocument/didChange` | ← client | Full-document sync; republishes diagnostics |
| `textDocument/didClose` | ← client | Clears diagnostics for that file |
| `textDocument/hover` | → client | Kind, signature, and doc comment of the declaration under the cursor |
| `textDocument/definition` | → client | `Location[]` of the declaration |
| `textDocument/references` | → client | `Location[]` of all references to the symbol under the cursor |
| `textDocument/rename` | → client | `WorkspaceEdit` renaming symbol across document references |
| `textDocument/documentSymbol` | → client | Outline tree: functions, classes (with methods and fields), interfaces, enums (with variants), `let`/`const` |
| `textDocument/completion` | → client | Every symbol in the file plus the keyword list |
| `textDocument/formatting` | → client | One whole-document `TextEdit`, from the same formatter as `spid3r fmt` |
| `textDocument/semanticTokens/full` | → client | Delta-encoded tokens over the whole file (legend below) |
| `textDocument/codeAction` | → client | Quick fixes for the diagnostics under the cursor |
| anything else | → client | `-32601 method not supported` |

`textDocumentSync` is advertised as `1` (**Full**): the client sends the entire
document text on every change. That is deliberate — it removes incremental
parsing from the server and makes every edit idempotent.

## Diagnostics

`didOpen`/`didChange` run the exact pipeline `spid3r check` runs:

1. lex (`Lexer::tokenize_with_comments`),
2. parse (`parser::parse`),
3. semantic check **with warnings** (`semantic::check_file_with_warnings`).

The first error, or the full warning list, becomes a `publishDiagnostics`
notification. Severity maps to LSP as `1` (error) and `2` (warning), and the
range comes from the diagnostic's span — which is already 1-based
line/column with a character length, so the conversion to a 0-based LSP
`Range` is arithmetic, not a second offset model. A diagnostic's `= help:`
line is appended to the message, because most clients show the message on its
own.

## Positions

Spid3r diagnostics use **character** columns, and so does this server: an LSP
`Position` is interpreted as `(line, character)` where `character` is a Unicode
scalar index within the line, 0-based. Clients that send UTF-16 code units
(notably older VS Code on files containing astral characters) can disagree by
one position on lines containing such characters. Everything else matches.

## Hover, definition, and completion

On `didOpen` the server builds a flat **declaration index** from the AST:

- top-level declarations (`fn`, `class`, `interface`, `enum`, `let`, `const`,
  destructuring `let`) resolve over the whole file;
- each function body's parameters and locals resolve over that body.

A name under the cursor resolves to the declaration whose region is the
*smallest* one containing the cursor, which is what makes a local shadow a
global without any scope algebra. Resolution is deliberately lexical and
single-file: no cross-file or cross-module navigation in this version.

Hover shows the kind, the signature, and — when there is one — the doc comment
immediately above the declaration, recovered from the lexer's comment
side-channel. `fn`, `let x: int = 7;` and `let x = 7;` hover differently
because the annotation is what the index records.

## Editor configuration

### VS Code

The repository ships an extension in `packaging/vscode/` (see
`docs/BUILDING.md` for building it). The client side is a handful of lines: it
starts `spid3r lsp` as a child process and forwards `textDocument/didOpen`,
`didChange`, and `didSave`. No language server framework, no dependencies.

For another editor, the same three facts are enough: command `spid3r lsp`,
language id `spid3r`, file extension `.matrix`.

## Testing

`tests/lsp_tests.rs` launches the real `spid3r` binary (via
`CARGO_BIN_EXE_spid3r`) and drives it over a pipe with hand-written
`Content-Length` framing, asserting on the parsed replies: capabilities,
diagnostic ranges and severities, hover text, definition targets, the outline
tree, completion contents, formatting edits, the `-32601` error path, and both
`exit` code paths. A hand-written JSON reader keeps the tests independent of
the implementation's own parser.

`src/lsp/mod.rs` also carries unit tests for the index itself (parameter
resolution, shadowing, doc-comment attachment, error capture), which is where
the subtle bugs live — a wire test can only prove the plumbing.

`_dustbin/audit_notes/probes/lsp_probe.py` is a scripted session that prints every
reply verbatim; useful when debugging a client handshake by hand.

## Not implemented

- Incremental (`textDocumentSync` kinds 2/3) sync, and range-based semantic
  tokens (`semanticTokens/range`) — tokens are computed for the whole file.
- Cross-file navigation, workspace symbols, `signatureHelp`, `inlayHint`, the
  `debug` adapter (DAP).
- `window/logMessage` / telemetry: the server is silent unless it is answering
  a request or publishing diagnostics.

## Semantic tokens

`textDocument/semanticTokens/full` returns the LSP's delta encoding: five
integers per token (`deltaLine, deltaStartChar, length, tokenType,
modifiers`), sorted by position. The legend is fixed and its **order is the
protocol** — index 0 is `type`, 1 is `class`, and so on — so clients map themes
by position, not by name:

| # | type | # | type |
|---|---|---|---|
| 0 | `type` | 8 | `function` |
| 1 | `class` | 9 | `method` |
| 2 | `enum` | 10 | `keyword` |
| 3 | `interface` | 11 | `comment` |
| 4 | `parameter` | 12 | `string` |
| 5 | `variable` | 13 | `number` |
| 6 | `property` | 14 | `operator` |
| 7 | `enumMember` | 15 | `decorator` |

Modifiers: bit 0 = `declaration`, bit 1 = `readonly` (a `const`).

Classification is not a second analysis. Keywords and literals come from the
token kind; identifiers are resolved through the same symbol index `hover`
uses; comments come from the lexer's side-channel (they are not tokens). Two
cases needed care:

- a parameter's index extent is its function's body, so the token in the
  signature is outside its own extent — the server falls back to matching the
  declared name on the declaration line;
- `fn new()` lexes `new` as a keyword, so a keyword spelling that names a
  declaration is coloured as that declaration.

Punctuation is deliberately skipped (brackets carry no meaning to a reader).

## Code actions

`textDocument/codeAction` returns quick fixes for the diagnostics whose line
falls inside the requested range. Only mechanical edits are offered:

| Trigger | Fix |
|---|---|
| `unused variable 'x'` / `unused parameter 'x'` | rename to `_x` (the documented "intentionally unused" spelling) |
| `unused import 'x'` | delete the line |
| `unknown name 'x'` where `x` is a stdlib module | insert `import "x";` after the last import |

Anything that needs judgement — moving a declaration, changing a signature — is
not offered: a quick fix that guesses is worse than no quick fix.

## References & rename (file-scoped)

`textDocument/references` and `textDocument/rename` are implemented
(2026-10-22) and honored by the same declaration index as hover/definition —
occurrences resolve through the smallest containing extent, so shadowing is
respected. Semantics:

- **File-scoped.** Only the open document is searched; cross-file references
  need the import graph the index deliberately does not model. Renaming a
  top-level function therefore does not touch importers — check other files
  (the editor shows exactly which spans change before you accept).
- The declaration's own name is renamed with the rest; for references,
  `context.includeDeclaration` filters it.
- `rename` validates the replacement lexes as one identifier and answers with
  `failureReason` otherwise; the result is a standard `WorkspaceEdit`.
- Local extents are narrowed per nested block (if/while/for/try), so a
  same-named binding inside a block shadows correctly.
