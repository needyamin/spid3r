# Spid3r — Visual Studio Code extension

Editor support for Spid3r (`.matrix` files) powered by the language server that
ships with the compiler: `spid3r lsp`.

## What it gives you

| Feature | Notes |
| --- | --- |
| Diagnostics | Type errors, syntax errors and lints from the exact same checker `spid3r check` uses |
| Hover | Type of a binding, function/class signature, doc comments |
| Go to definition | Declaration of any binding |
| Outline | Document symbols — classes, interfaces, enums, functions, fields, parameters |
| Completion | Symbols from the current file plus the keyword list |
| Formatting | The canonical `spid3r fmt` layout, as a whole-document edit |

The extension is a thin client. All of the behaviour lives in the compiler, so
the editor and the command line can never disagree.

## Requirements

- `spid3r` on your `PATH` (or set `spid3r.serverPath` to its absolute path).
- VS Code 1.70 or newer.

## Install

Either run `spid3r lsp` yourself and use any LSP-capable editor, or build this
extension into a `.vsix`:

```bash
cd packaging/vscode
npm install -g @vscode/vsce      # once
vsce package                     # produces spid3r-0.1.0.vsix
code --install-extension spid3r-0.1.0.vsix
```

`vsce package` bundles `extension.js`, `language-configuration.json` and
`syntaxes/`. There are no runtime dependencies, so no `npm install` is needed in
this directory.

## Settings

| Setting | Default | Meaning |
| --- | --- | --- |
| `spid3r.serverPath` | `"spid3r"` | How to launch the language server. Set this to an absolute path if `spid3r` is not on your `PATH`. |
| `spid3r.trace.server` | `"off"` | Set to `"messages"` or `"verbose"` to log the JSON-RPC traffic to the Spid3r output channel. |

## Developing against it

1. Open `packaging/vscode` in VS Code and press `F5` — this launches an Extension
   Development Host with the client loaded.
2. Open any `.matrix` file; the output channel *Spid3r Language Server* shows
   server stderr, which is where any startup failure is reported.

## Files

| File | Purpose |
| --- | --- |
| `package.json` | Extension manifest — activation event, `.matrix` language contribution, settings. |
| `extension.js` | The client. Spawns `spid3r lsp`, does the `initialize`/`initialized` handshake, forwards document events, renders diagnostics. |
| `language-configuration.json` | Comments, brackets, auto-closing pairs, folding markers, indentation rules. |
| `syntaxes/spid3r.tmLanguage.json` | TextMate grammar: comments, strings (including `{ident}` interpolation), numbers, keywords, types, declarations, operators. |

See `docs/LSP.md` for the protocol surface, and `docs/BUILDING.md` for building
the compiler itself.
