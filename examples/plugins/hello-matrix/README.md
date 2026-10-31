# hello-matrix — a plugin written in Spid3r

The plugin kind that needs no other language. Two files:

| File | What it is |
| --- | --- |
| `plugin.toml` | the manifest — name, ABI, and one `[[function]]` per public function |
| `hello.matrix` | the plugin itself: ordinary `public fn`s |
| `demo.matrix` | a program that calls them |

```console
$ SPID3R_PLUGINS=examples/plugins spid3r run examples/plugins/hello-matrix/demo.matrix
Hello, Ada! (from Spid3r)
55
```

Identical on `--engine tree` and `--engine vm`.

## Why this exists

A plugin system for a language that only accepts plugins written in *another*
language is a plugin system for a language nobody writes plugins in. `kind =
"matrix"` closes that: the plugin is a `.matrix` file, and its `public fn`s
become callable from any program that loads it.

There is no build step, no ABI to implement, and no second toolchain. The file
goes through the same parser, the same semantic checker (lints included) and the
same interpreter as everything else.

## How it loads

1. The `.matrix` file is parsed and checked — a plugin that does not check never
   loads.
2. It is run as a **module**, not a program: every top-level statement, no
   `fn main` required.
3. For each `[[function]]`, the loader looks the name up in the plugin's globals,
   checks it is a function with the declared `arity`, and wraps it as a native
   closure.
4. The closures join the same registry as every other plugin kind, so both
   script engines see them with no per-engine code.

## Trust

A Spid3r plugin is **script code**. It has exactly the authority a script has —
the same builtins, the same file access — because it *is* a script. It is tier
T1, the same trust as your project's own source.

That is the honest consequence of "write it in your own language": the code is
not sandboxed by virtue of being a plugin. Use `kind = "wasm"` for code you did
not write.

## What it cannot do

Each plugin gets its own interpreter and its own globals, so it cannot read or
clobber the calling program's variables — but it can call the whole standard
library. A `throw` inside the plugin reaches the caller's `catch` with its
message intact.

A native executable (`spidr build`) still refuses a program that calls a plugin
function: a standalone binary cannot carry one.
