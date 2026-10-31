//! Spid3r library crate: lexer, parser, AST, semantic pass, reference runner
//! (tree-walk), bytecode compiler, virtual machine, scheduler, stdlib, CLI.

#![allow(clippy::mutable_key_type)]

pub mod ast;
pub mod budget;
#[cfg(not(target_arch = "wasm32"))]
pub mod bundle;
pub mod cli;
pub mod diagnostics;
pub mod embed;
pub mod environment;
pub mod fmt;
pub mod interpreter;
pub mod lexer;
pub mod logging;
pub mod lower;
pub mod lsp;
pub mod modules;
pub mod parser;
pub mod pkg;
pub mod plugin;
pub mod repl;
pub mod sched;
pub mod semantic;
pub mod stdlib;
/// Stdlib sources baked into the binary. Used by the wasm build (no
/// filesystem) and as the last-resort fallback on native targets, so a
/// single-file app that ships `.matrix` source can still `import "semver"`
/// on a machine with no `stdlib/` directory (see [`modules`]).
pub mod stdlib_embed;
pub mod value;
pub mod vm;
pub mod wasm;

// Native backend. `rt` (the extern "C" runtime trampolines the AOT binaries
// link against) has zero dependencies and is always compiled; `ir`/`link`
// need LLVM (inkwell) and are feature-gated behind `native`.
pub mod native;

// GUI layer: run programs in a desktop window with an embedded webview
// (tao + wry). Feature-gated behind `gui`.
#[cfg(feature = "gui")]
pub mod gui;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const LANGUAGE_VERSION: &str = "1.0";

// Embedding ergonomics: hosts work with `spid3r_lang::Value` (see spid3r_lang::embed).
pub use value::Value;
