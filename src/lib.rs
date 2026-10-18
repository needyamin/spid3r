//! Spid3r library crate: lexer, parser, AST, semantic pass, reference runner
//! (tree-walk), bytecode compiler, virtual machine, scheduler, stdlib, CLI.

#![allow(clippy::mutable_key_type)]

pub mod ast;
pub mod cli;
pub mod diagnostics;
pub mod environment;
pub mod fmt;
pub mod interpreter;
pub mod lexer;
pub mod modules;
pub mod parser;
pub mod pkg;
pub mod repl;
pub mod sched;
pub mod semantic;
pub mod stdlib;
pub mod value;
pub mod vm;

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
